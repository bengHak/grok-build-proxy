package codexcli

import (
	"bytes"
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestEnsureAuthConfigCreatesPrivateRootSettings(t *testing.T) {
	home := filepath.Join(t.TempDir(), "codex-home")
	if err := EnsureAuthConfig(home); err != nil {
		t.Fatal(err)
	}
	status, err := InspectAuthConfig(home)
	if err != nil {
		t.Fatal(err)
	}
	if status.CredentialStore != "file" {
		t.Fatalf("CredentialStore = %q", status.CredentialStore)
	}
	if status.ForcedLogin != "chatgpt" {
		t.Fatalf("ForcedLogin = %q", status.ForcedLogin)
	}
	info, err := os.Stat(status.Path)
	if err != nil {
		t.Fatal(err)
	}
	if info.Mode().Perm()&0o077 != 0 {
		t.Fatalf("config mode = %o", info.Mode().Perm())
	}
}

func TestEnsureAuthConfigPreservesOtherSettingsAndTables(t *testing.T) {
	home := t.TempDir()
	path := filepath.Join(home, "config.toml")
	original := "model = \"gpt-test\"\ncli_auth_credentials_store = \"keyring\"\n\n[features]\ntelemetry = false\n"
	if err := os.WriteFile(path, []byte(original), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := EnsureAuthConfig(home); err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	text := string(data)
	for _, expected := range []string{
		"model = \"gpt-test\"",
		"cli_auth_credentials_store = \"file\"",
		"forced_login_method = \"chatgpt\"",
		"[features]",
		"telemetry = false",
	} {
		if !strings.Contains(text, expected) {
			t.Fatalf("config missing %q:\n%s", expected, text)
		}
	}
	if strings.Count(text, "cli_auth_credentials_store") != 1 {
		t.Fatalf("credential store duplicated:\n%s", text)
	}
}

func TestClientRunsOfficialCLIWithDedicatedHome(t *testing.T) {
	dir := t.TempDir()
	logPath := filepath.Join(dir, "args.log")
	binary := filepath.Join(dir, "codex")
	script := "#!/bin/sh\nprintf '%s\\n' \"$CODEX_HOME|$*\" >> \"$TEST_LOG\"\n"
	if err := os.WriteFile(binary, []byte(script), 0o755); err != nil {
		t.Fatal(err)
	}
	home := filepath.Join(dir, "home")
	var output bytes.Buffer
	client, err := New(Config{
		Binary: binary,
		Home:   home,
		Stdout: &output,
		Stderr: &output,
		Env:    []string{"TEST_LOG=" + logPath},
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := client.Login(context.Background(), true); err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(logPath)
	if err != nil {
		t.Fatal(err)
	}
	got := strings.TrimSpace(string(data))
	want := home + "|login --device-auth"
	if got != want {
		t.Fatalf("command = %q, want %q", got, want)
	}
}
