package auth

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestStoreLoadsUnexpiredCredentials(t *testing.T) {
	now := time.Unix(1_800_000_000, 0).UTC()
	authPath := filepath.Join(t.TempDir(), "auth.json")
	writeAuthFile(t, authPath, map[string]any{
		"tokens": map[string]any{
			"id_token":      testJWT(map[string]any{"https://api.openai.com/auth": map[string]any{"chatgpt_account_id": "acct-from-jwt"}}),
			"access_token":  testJWT(map[string]any{"exp": now.Add(time.Hour).Unix()}),
			"refresh_token": "refresh-secret",
		},
	})

	store, err := NewStore(Config{Path: authPath, Now: func() time.Time { return now }})
	if err != nil {
		t.Fatal(err)
	}
	creds, err := store.Get(context.Background(), false)
	if err != nil {
		t.Fatal(err)
	}
	if creds.AccountID != "acct-from-jwt" {
		t.Fatalf("AccountID = %q", creds.AccountID)
	}
	if creds.AccessToken == "" {
		t.Fatal("missing access token")
	}
	if !creds.ExpiresAt.Equal(now.Add(time.Hour)) {
		t.Fatalf("ExpiresAt = %v", creds.ExpiresAt)
	}
}

func TestStoreRefreshesAndPersistsRotatedTokens(t *testing.T) {
	now := time.Unix(1_800_000_000, 0).UTC()
	authPath := filepath.Join(t.TempDir(), "auth.json")
	writeAuthFile(t, authPath, map[string]any{
		"auth_mode": "chatgpt",
		"unknown":   map[string]any{"preserved": true},
		"tokens": map[string]any{
			"id_token":      testJWT(map[string]any{"https://api.openai.com/auth": map[string]any{"chatgpt_account_id": "old-account"}}),
			"access_token":  testJWT(map[string]any{"exp": now.Add(time.Minute).Unix()}),
			"refresh_token": "old-refresh",
		},
	})

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			t.Fatalf("method = %s", r.Method)
		}
		var request map[string]string
		if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
			t.Fatal(err)
		}
		if request["client_id"] != CodexOAuthClientID || request["refresh_token"] != "old-refresh" {
			t.Fatalf("unexpected refresh request: %#v", request)
		}
		_ = json.NewEncoder(w).Encode(map[string]string{
			"id_token":      testJWT(map[string]any{"https://api.openai.com/auth": map[string]any{"chatgpt_account_id": "new-account"}}),
			"access_token":  testJWT(map[string]any{"exp": now.Add(2 * time.Hour).Unix()}),
			"refresh_token": "new-refresh",
		})
	}))
	defer server.Close()

	store, err := NewStore(Config{Path: authPath, RefreshURL: server.URL, Now: func() time.Time { return now }})
	if err != nil {
		t.Fatal(err)
	}
	creds, err := store.Get(context.Background(), false)
	if err != nil {
		t.Fatal(err)
	}
	if creds.AccountID != "new-account" {
		t.Fatalf("AccountID = %q", creds.AccountID)
	}
	if !creds.ExpiresAt.Equal(now.Add(2 * time.Hour)) {
		t.Fatalf("ExpiresAt = %v", creds.ExpiresAt)
	}

	data, err := os.ReadFile(authPath)
	if err != nil {
		t.Fatal(err)
	}
	var persisted map[string]any
	if err := json.Unmarshal(data, &persisted); err != nil {
		t.Fatal(err)
	}
	if persisted["unknown"].(map[string]any)["preserved"] != true {
		t.Fatal("unknown fields were not preserved")
	}
	tokens := persisted["tokens"].(map[string]any)
	if tokens["refresh_token"] != "new-refresh" {
		t.Fatalf("refresh_token = %#v", tokens["refresh_token"])
	}
	if tokens["account_id"] != "new-account" {
		t.Fatalf("account_id = %#v", tokens["account_id"])
	}
	info, err := os.Stat(authPath)
	if err != nil {
		t.Fatal(err)
	}
	if info.Mode().Perm()&0o077 != 0 {
		t.Fatalf("auth mode is too permissive: %o", info.Mode().Perm())
	}
}

func TestStoreRejectsAPIKeyOnlyAuth(t *testing.T) {
	authPath := filepath.Join(t.TempDir(), "auth.json")
	writeAuthFile(t, authPath, map[string]any{"OPENAI_API_KEY": "secret"})
	store, err := NewStore(Config{Path: authPath})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := store.Get(context.Background(), false); err == nil {
		t.Fatal("expected API-key-only auth to be rejected")
	}
}

func writeAuthFile(t *testing.T, path string, value any) {
	t.Helper()
	data, err := json.Marshal(value)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatal(err)
	}
}

func testJWT(claims map[string]any) string {
	header, _ := json.Marshal(map[string]any{"alg": "none", "typ": "JWT"})
	payload, _ := json.Marshal(claims)
	return base64.RawURLEncoding.EncodeToString(header) + "." + base64.RawURLEncoding.EncodeToString(payload) + ".sig"
}
