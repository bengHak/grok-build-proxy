package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"net/http"
	"os"
	"strings"
	"time"

	"github.com/bengHak/grok-build-proxy/internal/auth"
	"github.com/bengHak/grok-build-proxy/internal/catalog"
	proxyhandler "github.com/bengHak/grok-build-proxy/internal/proxy"
)

func runServe(ctx context.Context, args []string, streams commandIO, defaults commandDefaults) error {
	if err := requireMacOS(); err != nil {
		return err
	}
	flags := flag.NewFlagSet("serve", flag.ContinueOnError)
	flags.SetOutput(streams.stderr)
	listen := flags.String("listen", envOr("GROK_BUILD_PROXY_LISTEN", "127.0.0.1:18765"), "address to listen on")
	authFile := flags.String("auth-file", defaults.authFile, "path to the Codex CLI auth.json file")
	upstream := flags.String("upstream", envOr("GROK_BUILD_PROXY_UPSTREAM", defaultUpstream), "ChatGPT Codex Responses endpoint")
	refreshURL := flags.String("refresh-url", envOr("GROK_BUILD_PROXY_REFRESH_URL", auth.DefaultRefreshURL), "OpenAI OAuth token refresh endpoint")
	modelsCSV := flags.String("models", strings.TrimSpace(os.Getenv("GROK_BUILD_PROXY_MODELS")), "comma-separated model IDs exposed by /v1/models")
	clientToken := flags.String("client-token", strings.TrimSpace(os.Getenv("GROK_BUILD_PROXY_TOKEN")), "optional bearer token required from local clients")
	logFormat := flags.String("log-format", envOr("GROK_BUILD_PROXY_LOG_FORMAT", "text"), "text or json")
	printConfig := flags.Bool("print-grok-config", false, "print example Grok Build model configuration and exit")
	showVersion := flags.Bool("version", false, "print version and exit")
	flags.Usage = func() {
		fmt.Fprintln(streams.stderr, "Usage: grok-build-proxy [serve] [flags]")
		flags.PrintDefaults()
	}
	if err := flags.Parse(args); err != nil {
		if errors.Is(err, flag.ErrHelp) {
			return nil
		}
		return err
	}
	if flags.NArg() != 0 {
		return fmt.Errorf("unexpected serve argument %q", flags.Arg(0))
	}

	if *showVersion {
		fmt.Fprintln(streams.stdout, version)
		return nil
	}
	models := catalog.New(*modelsCSV)
	if *printConfig {
		fmt.Fprint(streams.stdout, renderGrokConfig(*listen, models))
		return nil
	}
	if !proxyhandler.IsLoopbackListen(*listen) && strings.TrimSpace(*clientToken) == "" {
		return errors.New("refusing to bind to a non-loopback address without --client-token or GROK_BUILD_PROXY_TOKEN")
	}

	logger := newLogger(*logFormat, streams.stderr)
	store, err := auth.NewStore(auth.Config{
		Path:       *authFile,
		RefreshURL: *refreshURL,
	})
	if err != nil {
		return err
	}
	handler, err := proxyhandler.New(proxyhandler.Config{
		UpstreamURL: *upstream,
		Credentials: store,
		Catalog:     models,
		Logger:      logger,
		ClientToken: *clientToken,
		Version:     version,
	})
	if err != nil {
		return err
	}

	server := &http.Server{
		Addr:              *listen,
		Handler:           handler,
		ReadHeaderTimeout: 15 * time.Second,
		IdleTimeout:       120 * time.Second,
		MaxHeaderBytes:    1 << 20,
	}

	serveErr := make(chan error, 1)
	go func() {
		logger.Info("proxy listening",
			"address", *listen,
			"auth_file", *authFile,
			"models", strings.Join(models.IDs(), ","),
			"version", version,
		)
		serveErr <- server.ListenAndServe()
	}()

	select {
	case err := <-serveErr:
		if errors.Is(err, http.ErrServerClosed) {
			return nil
		}
		return err
	case <-ctx.Done():
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		logger.Info("shutting down")
		return server.Shutdown(shutdownCtx)
	}
}

func renderGrokConfig(listen string, models catalog.Catalog) string {
	var builder strings.Builder
	builder.WriteString("# Add selected blocks to ~/.grok/config.toml\n\n")
	for _, model := range models.Models() {
		name := strings.NewReplacer(".", "-", "_", "-", "/", "-").Replace(model.ID)
		fmt.Fprintf(&builder, "[model.codex-%s]\n", name)
		fmt.Fprintf(&builder, "model = %q\n", model.ID)
		fmt.Fprintf(&builder, "name = %q\n", "Codex "+model.DisplayName)
		fmt.Fprintf(&builder, "base_url = %q\n", "http://"+listen+"/v1")
		builder.WriteString("api_backend = \"responses\"\n")
		builder.WriteString("api_key = \"unused\"\n")
		fmt.Fprintf(&builder, "context_window = %d\n\n", model.ContextWindow)
	}
	return builder.String()
}
