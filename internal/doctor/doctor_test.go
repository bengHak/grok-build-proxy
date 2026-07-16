package doctor

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/bengHak/grok-build-proxy/internal/codexcli"
)

func TestRunReportsReadySetup(t *testing.T) {
	now := time.Unix(1_800_000_000, 0).UTC()
	root := t.TempDir()
	codexHome := filepath.Join(root, "codex-home")
	if err := codexcli.EnsureAuthConfig(codexHome); err != nil {
		t.Fatal(err)
	}
	writeJSON(t, filepath.Join(codexHome, "auth.json"), map[string]any{
		"auth_mode": "chatgpt",
		"tokens": map[string]any{
			"access_token":  jwt(map[string]any{"exp": now.Add(time.Hour).Unix()}),
			"id_token":      jwt(map[string]any{"https://api.openai.com/auth": map[string]any{"chatgpt_account_id": "account-12345678"}}),
			"refresh_token": "refresh",
		},
	})
	grokConfig := filepath.Join(root, "grok.toml")
	if err := os.WriteFile(grokConfig, []byte("[model.codex]\napi_backend = \"responses\"\nbase_url = \"http://127.0.0.1:0/v1\"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	codexBinary := fakeCommand(t, root, "codex", "codex-cli 1.0")
	grokBinary := fakeCommand(t, root, "grok", "grok 1.0")

	report := Run(context.Background(), Config{
		RuntimeOS:   "darwin",
		RuntimeArch: "arm64",
		Version:     "test",
		CodexHome:   codexHome,
		AuthFile:    filepath.Join(codexHome, "auth.json"),
		CodexBinary: codexBinary,
		GrokBinary:  grokBinary,
		GrokConfig:  grokConfig,
		Listen:      "127.0.0.1:0",
		Now:         func() time.Time { return now },
		HTTPClient:  &http.Client{Timeout: 20 * time.Millisecond},
	})
	if report.HasFailures() {
		t.Fatalf("unexpected failures: %#v", report.Checks)
	}
	_, warnings, _ := report.Counts()
	if warnings != 1 {
		t.Fatalf("warnings = %d, checks=%#v", warnings, report.Checks)
	}
}

func TestRunFailsWithoutCodexAuthentication(t *testing.T) {
	root := t.TempDir()
	codexHome := filepath.Join(root, "codex-home")
	if err := codexcli.EnsureAuthConfig(codexHome); err != nil {
		t.Fatal(err)
	}
	grokConfig := filepath.Join(root, "grok.toml")
	if err := os.WriteFile(grokConfig, []byte("[model.codex]\napi_backend = \"responses\"\nbase_url = \"http://127.0.0.1:0/v1\"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	report := Run(context.Background(), Config{
		RuntimeOS:    "darwin",
		RuntimeArch:  "arm64",
		Version:      "test",
		CodexHome:    codexHome,
		AuthFile:     filepath.Join(codexHome, "auth.json"),
		GrokConfig:   grokConfig,
		Listen:       "127.0.0.1:0",
		SkipCommands: true,
	})
	if !report.HasFailures() {
		t.Fatalf("expected authentication failure: %#v", report.Checks)
	}
}

func fakeCommand(t *testing.T, dir, name, output string) string {
	t.Helper()
	path := filepath.Join(dir, name)
	script := "#!/bin/sh\nprintf '%s\\n' " + quoteShell(output) + "\n"
	if err := os.WriteFile(path, []byte(script), 0o755); err != nil {
		t.Fatal(err)
	}
	return path
}

func quoteShell(value string) string {
	data, _ := json.Marshal(value)
	return string(data)
}

func writeJSON(t *testing.T, path string, value any) {
	t.Helper()
	data, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatal(err)
	}
}

func jwt(claims map[string]any) string {
	header, _ := json.Marshal(map[string]any{"alg": "none", "typ": "JWT"})
	payload, _ := json.Marshal(claims)
	return base64.RawURLEncoding.EncodeToString(header) + "." + base64.RawURLEncoding.EncodeToString(payload) + ".sig"
}

func TestRunRejectsInvalidModelMapping(t *testing.T) {
	report := Run(context.Background(), Config{
		RuntimeOS:    "darwin",
		RuntimeArch:  "arm64",
		Version:      "test",
		ModelMap:     "grok-4.5",
		SkipCommands: true,
	})
	if !report.HasFailures() {
		t.Fatalf("expected invalid model map failure: %#v", report.Checks)
	}
	found := false
	for _, check := range report.Checks {
		if check.Name == "Model substitutions" {
			found = true
			if check.Level != Fail {
				t.Fatalf("model substitutions level = %s", check.Level)
			}
		}
	}
	if !found {
		t.Fatalf("model substitutions check missing: %#v", report.Checks)
	}
}

func TestModelSubstitutionCheckReportsResolvedChain(t *testing.T) {
	check := checkModelMap(Config{ModelMap: "composer=grok-build,grok-build=gpt-5.6-terra-fast"})
	if check.Level != Pass {
		t.Fatalf("level = %s detail=%s", check.Level, check.Detail)
	}
	if want := "composer -> gpt-5.6-terra-fast"; !strings.Contains(check.Detail, want) {
		t.Fatalf("detail = %q, missing %q", check.Detail, want)
	}
}
