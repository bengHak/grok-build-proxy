package main

import (
	"strings"
	"testing"

	"github.com/bengHak/grok-build-proxy/internal/catalog"
	"github.com/bengHak/grok-build-proxy/internal/modelmap"
)

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
