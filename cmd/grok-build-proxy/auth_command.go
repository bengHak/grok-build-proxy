package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"path/filepath"
	"strings"
	"time"

	"github.com/bengHak/grok-build-proxy/internal/auth"
	"github.com/bengHak/grok-build-proxy/internal/codexcli"
)

func runAuth(ctx context.Context, args []string, streams commandIO, defaults commandDefaults) error {
	if err := requireMacOS(); err != nil {
		return err
	}
	if len(args) == 0 || args[0] == "help" || args[0] == "--help" || args[0] == "-h" {
		writeAuthUsage(streams.stdout)
		if len(args) == 0 {
			return errors.New("auth action is required")
		}
		return nil
	}

	action := args[0]
	flags := flag.NewFlagSet("auth "+action, flag.ContinueOnError)
	flags.SetOutput(streams.stderr)
	codexHome := flags.String("codex-home", defaults.codexHome, "dedicated CODEX_HOME used by the proxy")
	codexBinary := flags.String("codex-binary", envOr("GROK_BUILD_PROXY_CODEX_BINARY", "codex"), "path or command name for the official Codex CLI")
	flags.Usage = func() {
		fmt.Fprintf(streams.stderr, "Usage: grok-build-proxy auth %s [flags]\n", action)
		flags.PrintDefaults()
	}
	if err := flags.Parse(args[1:]); err != nil {
		if errors.Is(err, flag.ErrHelp) {
			return nil
		}
		return err
	}
	if flags.NArg() != 0 {
		return fmt.Errorf("unexpected auth argument %q", flags.Arg(0))
	}

	client, err := codexcli.New(codexcli.Config{
		Binary: *codexBinary,
		Home:   *codexHome,
		Stdin:  streams.stdin,
		Stdout: streams.stdout,
		Stderr: streams.stderr,
	})
	if err != nil {
		return err
	}

	switch action {
	case "login":
		if err := client.Login(ctx, false); err != nil {
			return err
		}
		return printAuthSummary(streams.stdout, filepath.Join(client.Home(), "auth.json"))
	case "device":
		if err := client.Login(ctx, true); err != nil {
			return err
		}
		return printAuthSummary(streams.stdout, filepath.Join(client.Home(), "auth.json"))
	case "status":
		if err := client.Status(ctx); err != nil {
			return err
		}
		return printAuthSummary(streams.stdout, filepath.Join(client.Home(), "auth.json"))
	case "logout":
		if err := client.Logout(ctx); err != nil {
			return err
		}
		fmt.Fprintf(streams.stdout, "Codex credentials cleared from %s\n", client.Home())
		return nil
	default:
		writeAuthUsage(streams.stderr)
		return fmt.Errorf("unknown auth action %q", action)
	}
}

func printAuthSummary(w io.Writer, authFile string) error {
	store, err := auth.NewStore(auth.Config{Path: authFile})
	if err != nil {
		return err
	}
	status, err := store.Inspect()
	if err != nil {
		return fmt.Errorf("Codex command completed, but proxy credentials are not usable: %w", err)
	}
	fmt.Fprintf(w, "\nProxy credential file: %s\n", status.Path)
	fmt.Fprintf(w, "Authentication mode: %s\n", status.AuthMode)
	if status.AccountID != "" {
		fmt.Fprintf(w, "ChatGPT account: %s\n", maskAccount(status.AccountID))
	}
	if !status.ExpiresAt.IsZero() {
		fmt.Fprintf(w, "Access token expires: %s\n", status.ExpiresAt.Local().Format(time.RFC3339))
	}
	if status.HasRefreshToken {
		fmt.Fprintln(w, "Refresh token: present")
	} else {
		fmt.Fprintln(w, "Refresh token: missing")
	}
	fmt.Fprintln(w, "Run `grok-build-proxy doctor` to validate the complete setup.")
	return nil
}

func maskAccount(value string) string {
	value = strings.TrimSpace(value)
	if len(value) <= 8 {
		return "…" + value
	}
	return value[:4] + "…" + value[len(value)-4:]
}

func writeAuthUsage(w io.Writer) {
	fmt.Fprintln(w, "Usage:\n"+
		"  grok-build-proxy auth login [flags]\n"+
		"  grok-build-proxy auth device [flags]\n"+
		"  grok-build-proxy auth status [flags]\n"+
		"  grok-build-proxy auth logout [flags]\n\n"+
		"Actions:\n"+
		"  login    Run the official browser-based `codex login` flow\n"+
		"  device   Run `codex login --device-auth` for a headless Mac\n"+
		"  status   Run `codex login status` and inspect the proxy credential file\n"+
		"  logout   Run `codex logout` in the proxy's dedicated CODEX_HOME")
}
