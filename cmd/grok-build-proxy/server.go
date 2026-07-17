package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"net/http"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/bengHak/grok-build-proxy/internal/auth"
	"github.com/bengHak/grok-build-proxy/internal/catalog"
	"github.com/bengHak/grok-build-proxy/internal/modelmap"
	"github.com/bengHak/grok-build-proxy/internal/monitor"
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
	modelMapSpec := flags.String("model-map", strings.TrimSpace(os.Getenv("GROK_BUILD_PROXY_MODEL_MAP")), "comma-separated Grok-to-Codex substitutions (source=target)")
	codexCompatVersion := flags.String("codex-compat-version", envOr("GROK_BUILD_PROXY_CODEX_COMPAT_VERSION", proxyhandler.DefaultCodexCompatibilityVersion), "Codex backend compatibility version header")
	clientToken := flags.String("client-token", strings.TrimSpace(os.Getenv("GROK_BUILD_PROXY_TOKEN")), "optional bearer token required from local clients")
	logFormat := flags.String("log-format", envOr("GROK_BUILD_PROXY_LOG_FORMAT", "text"), "text or json")
	noMonitor := flags.Bool("no-monitor", false, "use plain logs instead of the interactive monitor")
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
	mappings, err := modelmap.Parse(*modelMapSpec)
	if err != nil {
		return fmt.Errorf("parse model substitutions: %w", err)
	}
	models := catalog.New(*modelsCSV)
	if *printConfig {
		fmt.Fprint(streams.stdout, renderGrokConfig(*listen, models, mappings))
		return nil
	}
	if !proxyhandler.IsLoopbackListen(*listen) && strings.TrimSpace(*clientToken) == "" {
		return errors.New("refusing to bind to a non-loopback address without --client-token or GROK_BUILD_PROXY_TOKEN")
	}

	inputFile, inputFileOK := streams.stdin.(*os.File)
	outputFile, outputFileOK := streams.stdout.(*os.File)
	monitorEnabled := !*noMonitor && inputFileOK && outputFileOK && monitor.IsInteractive(streams.stdin, streams.stdout)
	logOutput := streams.stderr
	if monitorEnabled {
		logOutput = io.Discard
	}
	logger := newLogger(*logFormat, logOutput)
	var dashboard *monitor.Dashboard
	if monitorEnabled {
		dashboard = monitor.NewDashboard()
	}
	store, err := auth.NewStore(auth.Config{
		Path:       *authFile,
		RefreshURL: *refreshURL,
	})
	if err != nil {
		return err
	}
	var observer proxyhandler.Observer
	if dashboard != nil {
		observer = dashboard
	}
	handler, err := proxyhandler.New(proxyhandler.Config{
		UpstreamURL: *upstream,
		Credentials: store,
		Catalog:     models,
		ModelMap:    mappings,
		HTTPClient:  proxyhandler.NewCodexHTTPClient(logger, *codexCompatVersion),
		Logger:      logger,
		Observer:    observer,
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
			"model_map", mappings.String(),
			"codex_compat_version", *codexCompatVersion,
			"version", version,
		)
		serveErr <- server.ListenAndServe()
	}()

	shutdown := func() error {
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		logger.Info("shutting down")
		return server.Shutdown(shutdownCtx)
	}

	if dashboard != nil {
		monitorCtx, cancelMonitor := context.WithCancel(ctx)
		defer cancelMonitor()
		programErr := make(chan error, 1)
		go func() {
			programErr <- (&monitor.Program{
				Dashboard: dashboard,
				Input:     streams.stdin,
				Output:    streams.stdout,
				Terminal:  monitor.NewTerminal(inputFile, outputFile),
				Address:   *listen,
				Version:   version,
			}).Run(monitorCtx)
		}()
		select {
		case err := <-serveErr:
			cancelMonitor()
			<-programErr
			if errors.Is(err, http.ErrServerClosed) {
				return nil
			}
			return err
		case err := <-programErr:
			cancelMonitor()
			shutdownErr := shutdown()
			if err != nil {
				return err
			}
			return shutdownErr
		case <-ctx.Done():
			cancelMonitor()
			<-programErr
			return shutdown()
		}
	}

	select {
	case err := <-serveErr:
		if errors.Is(err, http.ErrServerClosed) {
			return nil
		}
		return err
	case <-ctx.Done():
		return shutdown()
	}
}

func renderGrokConfig(listen string, models catalog.Catalog, mappings modelmap.Map) string {
	var builder strings.Builder
	builder.WriteString("# Add selected blocks to ~/.grok/config.toml\n\n")
	builder.WriteString("# Optional global default used by the Quick Start:\n")
	builder.WriteString("# [models]\n")
	builder.WriteString("# default_reasoning_effort = \"xhigh\"\n\n")
	mappedSources := make(map[string]struct{}, mappings.Len())
	for _, entry := range mappings.Entries() {
		mappedSources[entry.Source] = struct{}{}
		resolved := mappings.Resolve(entry.Source)
		target, _ := models.Lookup(resolved.Model)
		targetName := target.DisplayName
		if resolved.Fast {
			targetName += " (Fast)"
		}
		fmt.Fprintf(&builder, "# Proxy mapping: %s -> %s\n", entry.Source, resolved.EffectiveModelID())
		fmt.Fprintf(&builder, "[model.%s]\n", tomlTableKey(entry.Source))
		fmt.Fprintf(&builder, "model = %q\n", entry.Source)
		fmt.Fprintf(&builder, "name = %q\n", displayModelID(entry.Source)+" via Codex "+targetName)
		fmt.Fprintf(&builder, "description = %q\n", fmt.Sprintf("Routes %s to %s through grok-build-proxy", entry.Source, resolved.EffectiveModelID()))
		fmt.Fprintf(&builder, "base_url = %q\n", "http://"+listen+"/v1")
		builder.WriteString("api_backend = \"responses\"\n")
		builder.WriteString("api_key = \"unused\"\n")
		fmt.Fprintf(&builder, "context_window = %d\n\n", target.ContextWindow)
	}
	for _, model := range models.Models() {
		if _, mapped := mappedSources[model.ID]; mapped {
			continue
		}
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

func displayModelID(value string) string {
	parts := strings.FieldsFunc(value, func(r rune) bool {
		return r == '-' || r == '_' || r == '/'
	})
	for i, part := range parts {
		switch strings.ToLower(part) {
		case "grok":
			parts[i] = "Grok"
		case "gpt":
			parts[i] = "GPT"
		case "codex":
			parts[i] = "Codex"
		default:
			if part != "" {
				runes := []rune(part)
				runes[0] = []rune(strings.ToUpper(string(runes[0])))[0]
				parts[i] = string(runes)
			}
		}
	}
	if len(parts) == 0 {
		return value
	}
	return strings.Join(parts, " ")
}

func tomlTableKey(value string) string {
	if value != "" {
		bare := true
		for _, r := range value {
			if !((r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z') || (r >= '0' && r <= '9') || r == '_' || r == '-') {
				bare = false
				break
			}
		}
		if bare {
			return value
		}
	}
	return strconv.Quote(value)
}
