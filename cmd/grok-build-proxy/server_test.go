package main

import (
	"bytes"
	"context"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/bengHak/grok-build-proxy/internal/catalog"
	"github.com/bengHak/grok-build-proxy/internal/modelmap"
)

func TestRunServeUsesPlainLogsForNonTTYAndNoMonitor(t *testing.T) {
	for _, extraArgs := range [][]string{nil, {"--no-monitor"}} {
		t.Run(strings.Join(extraArgs, "_"), func(t *testing.T) {
			ctx, cancel := context.WithCancel(context.Background())
			var stdout, stderr bytes.Buffer
			args := append([]string{"--listen", "127.0.0.1:0", "--auth-file", filepath.Join(t.TempDir(), "auth.json")}, extraArgs...)
			done := make(chan error, 1)
			go func() {
				done <- runServe(ctx, args, commandIO{stdin: strings.NewReader(""), stdout: &stdout, stderr: &stderr}, commandDefaults{})
			}()
			time.Sleep(50 * time.Millisecond)
			cancel()
			if err := <-done; err != nil {
				t.Fatal(err)
			}
			logs := stdout.String() + stderr.String()
			if !strings.Contains(logs, "proxy listening") || !strings.Contains(logs, "address=127.0.0.1:0") {
				t.Fatalf("plain listening log missing:\n%s", logs)
			}
			if strings.Contains(logs, "\x1b[") || strings.Contains(logs, "Sessions") {
				t.Fatalf("non-TTY path emitted monitor output: %q", logs)
			}
		})
	}
}

func TestServeHelpDocumentsNoMonitor(t *testing.T) {
	var stdout, stderr bytes.Buffer
	err := runServe(context.Background(), []string{"--help"}, commandIO{stdin: strings.NewReader(""), stdout: &stdout, stderr: &stderr}, commandDefaults{})
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(stderr.String(), "-no-monitor") {
		t.Fatalf("serve help missing --no-monitor:\n%s", stderr.String())
	}
}

func TestRenderGrokConfigIncludesModelMappings(t *testing.T) {
	mappings, err := modelmap.Parse("grok-build=gpt-5.6-terra,grok-4.5=gpt-5.6-sol-fast")
	if err != nil {
		t.Fatal(err)
	}
	output := renderGrokConfig("127.0.0.1:18765", catalog.New(""), mappings)
	for _, want := range []string{
		"# Proxy mapping: grok-build -> gpt-5.6-terra",
		"[model.grok-build]",
		`model = "grok-build"`,
		`[model."grok-4.5"]`,
		`model = "grok-4.5"`,
		"# Proxy mapping: grok-4.5 -> gpt-5.6-sol-fast",
		"Grok 4.5 via Codex GPT-5.6 Sol (Fast)",
		"context_window = 372000",
	} {
		if !strings.Contains(output, want) {
			t.Fatalf("output does not contain %q:\n%s", want, output)
		}
	}
}

func TestTOMLTableKey(t *testing.T) {
	cases := map[string]string{
		"grok-build":         "grok-build",
		"grok-4.5":           `"grok-4.5"`,
		"provider/model":     `"provider/model"`,
		`model-with-"quote"`: `"model-with-\"quote\""`,
	}
	for input, want := range cases {
		if got := tomlTableKey(input); got != want {
			t.Errorf("tomlTableKey(%q) = %q, want %q", input, got, want)
		}
	}
}
