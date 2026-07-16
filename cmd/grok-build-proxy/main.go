package main

import (
	"context"
	"fmt"
	"io"
	"log/slog"
	"os"
	"os/signal"
	"path/filepath"
	"runtime"
	"strings"
	"syscall"
)

var version = "dev"

const defaultUpstream = "https://chatgpt.com/backend-api/codex/responses"

type commandIO struct {
	stdin  io.Reader
	stdout io.Writer
	stderr io.Writer
}

type commandDefaults struct {
	home       string
	codexHome  string
	authFile   string
	grokConfig string
}

func main() {
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	if err := execute(ctx, os.Args[1:], commandIO{stdin: os.Stdin, stdout: os.Stdout, stderr: os.Stderr}); err != nil {
		fmt.Fprintln(os.Stderr, "error:", err)
		os.Exit(1)
	}
}

func execute(ctx context.Context, args []string, streams commandIO) error {
	defaults, err := resolveDefaults()
	if err != nil {
		return err
	}
	if len(args) == 0 {
		return runServe(ctx, nil, streams, defaults)
	}

	switch args[0] {
	case "serve":
		return runServe(ctx, args[1:], streams, defaults)
	case "auth":
		return runAuth(ctx, args[1:], streams, defaults)
	case "doctor":
		return runDoctor(ctx, args[1:], streams, defaults)
	case "version", "--version":
		fmt.Fprintln(streams.stdout, version)
		return nil
	case "help", "--help", "-h":
		writeRootUsage(streams.stdout)
		return nil
	default:
		if strings.HasPrefix(args[0], "-") {
			return runServe(ctx, args, streams, defaults)
		}
		writeRootUsage(streams.stderr)
		return fmt.Errorf("unknown command %q", args[0])
	}
}

func resolveDefaults() (commandDefaults, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return commandDefaults{}, fmt.Errorf("resolve home directory: %w", err)
	}
	codexHome := firstNonEmpty(
		os.Getenv("GROK_BUILD_PROXY_CODEX_HOME"),
		os.Getenv("CODEX_HOME"),
		filepath.Join(home, ".codex-grok-build-proxy"),
	)
	return commandDefaults{
		home:       home,
		codexHome:  filepath.Clean(codexHome),
		authFile:   filepath.Clean(envOr("GROK_BUILD_PROXY_AUTH_FILE", filepath.Join(codexHome, "auth.json"))),
		grokConfig: filepath.Clean(envOr("GROK_BUILD_PROXY_GROK_CONFIG", filepath.Join(home, ".grok", "config.toml"))),
	}, nil
}

func requireMacOS() error {
	if runtime.GOOS != "darwin" {
		return fmt.Errorf("grok-build-proxy supports macOS only (detected %s)", runtime.GOOS)
	}
	if runtime.GOARCH != "arm64" && runtime.GOARCH != "amd64" {
		return fmt.Errorf("grok-build-proxy does not support macOS/%s", runtime.GOARCH)
	}
	return nil
}

func writeRootUsage(w io.Writer) {
	fmt.Fprintln(w, `Usage:
  grok-build-proxy [serve flags]
  grok-build-proxy serve [flags]
  grok-build-proxy auth <login|device|status|logout> [flags]
  grok-build-proxy doctor [flags]
  grok-build-proxy --version

Commands:
  serve    Start the local Grok Build to Codex proxy (default command)
  auth     Delegate ChatGPT authentication to the official Codex CLI
  doctor   Check Codex auth, Grok Build configuration, and proxy readiness

Run "grok-build-proxy <command> --help" for command-specific options.`)
}

func newLogger(format string, w io.Writer) *slog.Logger {
	options := &slog.HandlerOptions{Level: slog.LevelInfo}
	if strings.EqualFold(format, "json") {
		return slog.New(slog.NewJSONHandler(w, options))
	}
	return slog.New(slog.NewTextHandler(w, options))
}

func envOr(name, fallback string) string {
	if value := strings.TrimSpace(os.Getenv(name)); value != "" {
		return value
	}
	return fallback
}

func firstNonEmpty(values ...string) string {
	for _, value := range values {
		if strings.TrimSpace(value) != "" {
			return strings.TrimSpace(value)
		}
	}
	return ""
}
