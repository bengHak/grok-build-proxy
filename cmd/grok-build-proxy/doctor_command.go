package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"path/filepath"
	"time"

	"github.com/bengHak/grok-build-proxy/internal/doctor"
)

func runDoctor(ctx context.Context, args []string, streams commandIO, defaults commandDefaults) error {
	flags := flag.NewFlagSet("doctor", flag.ContinueOnError)
	flags.SetOutput(streams.stderr)
	codexHome := flags.String("codex-home", defaults.codexHome, "dedicated CODEX_HOME used by the proxy")
	authFile := flags.String("auth-file", defaults.authFile, "path to the Codex CLI auth.json file")
	codexBinary := flags.String("codex-binary", envOr("GROK_BUILD_PROXY_CODEX_BINARY", "codex"), "path or command name for the official Codex CLI")
	grokBinary := flags.String("grok-binary", envOr("GROK_BUILD_PROXY_GROK_BINARY", "grok"), "path or command name for Grok Build")
	grokConfig := flags.String("grok-config", defaults.grokConfig, "path to Grok Build config.toml")
	listen := flags.String("listen", envOr("GROK_BUILD_PROXY_LISTEN", "127.0.0.1:18765"), "proxy listen address to inspect")
	modelMapSpec := flags.String("model-map", envOr("GROK_BUILD_PROXY_MODEL_MAP", ""), "comma-separated Grok-to-Codex substitutions (source=target)")
	clientToken := flags.String("client-token", envOr("GROK_BUILD_PROXY_TOKEN", ""), "bearer token for a protected local proxy")
	timeout := flags.Duration("timeout", 5*time.Second, "timeout for each command and HTTP check")
	flags.Usage = func() {
		fmt.Fprintln(streams.stderr, "Usage: grok-build-proxy doctor [flags]")
		flags.PrintDefaults()
	}
	if err := flags.Parse(args); err != nil {
		if errors.Is(err, flag.ErrHelp) {
			return nil
		}
		return err
	}
	if flags.NArg() != 0 {
		return fmt.Errorf("unexpected doctor argument %q", flags.Arg(0))
	}
	if filepath.Clean(*codexHome) != filepath.Clean(defaults.codexHome) && filepath.Clean(*authFile) == filepath.Clean(defaults.authFile) {
		*authFile = filepath.Join(*codexHome, "auth.json")
	}

	fmt.Fprintln(streams.stdout, "grok-build-proxy doctor")
	fmt.Fprintln(streams.stdout)
	report := doctor.Run(ctx, doctor.Config{
		Version:     version,
		CodexHome:   *codexHome,
		AuthFile:    *authFile,
		CodexBinary: *codexBinary,
		GrokBinary:  *grokBinary,
		GrokConfig:  *grokConfig,
		Listen:      *listen,
		ModelMap:    *modelMapSpec,
		ClientToken: *clientToken,
		Timeout:     *timeout,
	})
	report.Write(streams.stdout)
	if report.HasFailures() {
		return errors.New("doctor found one or more blocking setup problems")
	}
	return nil
}
