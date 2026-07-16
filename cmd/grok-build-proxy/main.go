package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"syscall"
	"time"

	"github.com/bengHak/grok-build-proxy/internal/auth"
	"github.com/bengHak/grok-build-proxy/internal/catalog"
	proxyhandler "github.com/bengHak/grok-build-proxy/internal/proxy"
)

var version = "dev"

const defaultUpstream = "https://chatgpt.com/backend-api/codex/responses"

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, "error:", err)
		os.Exit(1)
	}
}

func run() error {
	home, err := os.UserHomeDir()
	if err != nil {
		return fmt.Errorf("resolve home directory: %w", err)
	}
	defaultCodexHome := envOr("CODEX_HOME", filepath.Join(home, ".codex"))

	listen := flag.String("listen", envOr("GROK_BUILD_PROXY_LISTEN", "127.0.0.1:18765"), "address to listen on")
	authFile := flag.String("auth-file", envOr("GROK_BUILD_PROXY_AUTH_FILE", filepath.Join(defaultCodexHome, "auth.json")), "path to the Codex CLI auth.json file")
	upstream := flag.String("upstream", envOr("GROK_BUILD_PROXY_UPSTREAM", defaultUpstream), "ChatGPT Codex Responses endpoint")
	refreshURL := flag.String("refresh-url", envOr("GROK_BUILD_PROXY_REFRESH_URL", auth.DefaultRefreshURL), "OpenAI OAuth token refresh endpoint")
	modelsCSV := flag.String("models", os.Getenv("GROK_BUILD_PROXY_MODELS"), "comma-separated model IDs exposed by /v1/models")
	clientToken := flag.String("client-token", os.Getenv("GROK_BUILD_PROXY_TOKEN"), "optional bearer token required from local clients")
	logFormat := flag.String("log-format", envOr("GROK_BUILD_PROXY_LOG_FORMAT", "text"), "text or json")
	printConfig := flag.Bool("print-grok-config", false, "print example Grok Build model configuration and exit")
	showVersion := flag.Bool("version", false, "print version and exit")
	flag.Parse()

	if *showVersion {
		fmt.Println(version)
		return nil
	}
	models := catalog.New(*modelsCSV)
	if *printConfig {
		fmt.Print(renderGrokConfig(*listen, models))
		return nil
	}
	if !proxyhandler.IsLoopbackListen(*listen) && strings.TrimSpace(*clientToken) == "" {
		return errors.New("refusing to bind to a non-loopback address without --client-token or GROK_BUILD_PROXY_TOKEN")
	}

	logger := newLogger(*logFormat)
	store, err := auth.NewStore(auth.Config{Path: *authFile, RefreshURL: *refreshURL})
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

	shutdownCtx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	serveErr := make(chan error, 1)
	go func() {
		logger.Info("proxy listening", "address", *listen, "auth_file", *authFile, "models", strings.Join(models.IDs(), ","), "version", version)
		serveErr <- server.ListenAndServe()
	}()

	select {
	case err := <-serveErr:
		if errors.Is(err, http.ErrServerClosed) {
			return nil
		}
		return err
	case <-shutdownCtx.Done():
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		logger.Info("shutting down")
		return server.Shutdown(ctx)
	}
}

func newLogger(format string) *slog.Logger {
	options := &slog.HandlerOptions{Level: slog.LevelInfo}
	if strings.EqualFold(format, "json") {
		return slog.New(slog.NewJSONHandler(os.Stderr, options))
	}
	return slog.New(slog.NewTextHandler(os.Stderr, options))
}

func envOr(name, fallback string) string {
	if value := strings.TrimSpace(os.Getenv(name)); value != "" {
		return value
	}
	return fallback
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
