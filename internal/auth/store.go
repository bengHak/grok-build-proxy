package auth

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"
)

const (
	DefaultRefreshURL  = "https://auth.openai.com/oauth/token"
	CodexOAuthClientID = "app_EMoamEEZ73f0CkXaXp7hrann"
	defaultMaxError    = 4096
)

// Credentials are the request-scoped values required by the ChatGPT Codex
// backend. Tokens are intentionally never exposed in logs by this package.
type Credentials struct {
	AccessToken string
	AccountID   string
	ExpiresAt   time.Time
}

// Status is a read-only summary of a Codex authentication file. It never
// contains access or refresh token values.
type Status struct {
	Path            string
	AuthMode        string
	AccountID       string
	ExpiresAt       time.Time
	HasRefreshToken bool
	LastRefresh     time.Time
	FileMode        os.FileMode
	FileSize        int64
}

// Config controls how Store loads and refreshes the official Codex CLI cache.
type Config struct {
	Path          string
	RefreshURL    string
	HTTPClient    *http.Client
	RefreshMargin time.Duration
	Now           func() time.Time
}

// Store loads the official Codex CLI auth.json file and refreshes ChatGPT OAuth
// tokens when needed. It preserves unknown JSON fields when persisting updates.
type Store struct {
	path          string
	refreshURL    string
	httpClient    *http.Client
	refreshMargin time.Duration
	now           func() time.Time
	mu            sync.Mutex
}

func NewStore(cfg Config) (*Store, error) {
	if strings.TrimSpace(cfg.Path) == "" {
		return nil, errors.New("auth file path is required")
	}
	if cfg.RefreshURL == "" {
		cfg.RefreshURL = DefaultRefreshURL
	}
	if cfg.HTTPClient == nil {
		cfg.HTTPClient = &http.Client{Timeout: 30 * time.Second}
	}
	if cfg.RefreshMargin <= 0 {
		cfg.RefreshMargin = 5 * time.Minute
	}
	if cfg.Now == nil {
		cfg.Now = time.Now
	}
	return &Store{
		path:          cfg.Path,
		refreshURL:    cfg.RefreshURL,
		httpClient:    cfg.HTTPClient,
		refreshMargin: cfg.RefreshMargin,
		now:           cfg.Now,
	}, nil
}

func (s *Store) Path() string { return s.path }

// Inspect returns a read-only authentication summary without refreshing or
// modifying credentials.
func (s *Store) Inspect() (Status, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	doc, tokens, err := s.load()
	if err != nil {
		return Status{Path: s.path}, err
	}
	creds, refreshToken, err := credentialsFromTokens(tokens)
	if err != nil {
		return Status{Path: s.path}, err
	}
	info, err := os.Stat(s.path)
	if err != nil {
		return Status{Path: s.path}, fmt.Errorf("stat Codex auth file: %w", err)
	}

	status := Status{
		Path:            s.path,
		AuthMode:        resolvedAuthMode(doc),
		AccountID:       creds.AccountID,
		ExpiresAt:       creds.ExpiresAt,
		HasRefreshToken: refreshToken != "",
		FileMode:        info.Mode(),
		FileSize:        info.Size(),
	}
	if value := strings.TrimSpace(stringValue(doc["last_refresh"])); value != "" {
		if parsed, parseErr := time.Parse(time.RFC3339Nano, value); parseErr == nil {
			status.LastRefresh = parsed
		}
	}
	return status, nil
}

// Get returns valid credentials. When forceRefresh is true, the refresh token
// flow is attempted even if the access token has not reached its refresh margin.
func (s *Store) Get(ctx context.Context, forceRefresh bool) (Credentials, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	doc, tokens, err := s.load()
	if err != nil {
		return Credentials{}, err
	}

	creds, refreshToken, err := credentialsFromTokens(tokens)
	if err != nil {
		return Credentials{}, err
	}

	shouldRefresh := forceRefresh
	if !creds.ExpiresAt.IsZero() && !creds.ExpiresAt.After(s.now().Add(s.refreshMargin)) {
		shouldRefresh = true
	}
	if !shouldRefresh {
		return creds, nil
	}
	if refreshToken == "" {
		if forceRefresh {
			return Credentials{}, errors.New("Codex credentials cannot be refreshed: refresh_token is missing; run `codex login` again")
		}
		return creds, nil
	}

	updated, err := s.refresh(ctx, refreshToken)
	if err != nil {
		return Credentials{}, err
	}
	if updated.AccessToken != "" {
		tokens["access_token"] = updated.AccessToken
	}
	if updated.RefreshToken != "" {
		tokens["refresh_token"] = updated.RefreshToken
	}
	if updated.IDToken != "" {
		tokens["id_token"] = updated.IDToken
	}
	if stringValue(tokens["account_id"]) == "" {
		if accountID := accountIDFromJWT(stringValue(tokens["id_token"])); accountID != "" {
			tokens["account_id"] = accountID
		}
	}
	doc["last_refresh"] = s.now().UTC().Format(time.RFC3339Nano)
	if err := s.save(doc); err != nil {
		return Credentials{}, fmt.Errorf("persist refreshed Codex credentials: %w", err)
	}

	creds, _, err = credentialsFromTokens(tokens)
	if err != nil {
		return Credentials{}, err
	}
	return creds, nil
}

func (s *Store) load() (map[string]any, map[string]any, error) {
	data, err := os.ReadFile(s.path)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil, nil, fmt.Errorf("Codex auth file not found at %s; use a file-backed CODEX_HOME and run `codex login`", s.path)
		}
		return nil, nil, fmt.Errorf("read Codex auth file: %w", err)
	}

	var doc map[string]any
	dec := json.NewDecoder(bytes.NewReader(data))
	dec.UseNumber()
	if err := dec.Decode(&doc); err != nil {
		return nil, nil, fmt.Errorf("parse Codex auth file: %w", err)
	}
	tokens, ok := doc["tokens"].(map[string]any)
	if !ok || tokens == nil {
		if stringValue(doc["OPENAI_API_KEY"]) != "" {
			return nil, nil, errors.New("Codex auth file contains an API key, not a ChatGPT session; run `codex login` and choose ChatGPT sign-in")
		}
		return nil, nil, errors.New("Codex auth file does not contain ChatGPT token data; run `codex login` again")
	}
	return doc, tokens, nil
}

type refreshResponse struct {
	IDToken      string `json:"id_token"`
	AccessToken  string `json:"access_token"`
	RefreshToken string `json:"refresh_token"`
}

func (s *Store) refresh(ctx context.Context, refreshToken string) (refreshResponse, error) {
	payload := map[string]string{
		"client_id":     CodexOAuthClientID,
		"grant_type":    "refresh_token",
		"refresh_token": refreshToken,
	}
	body, err := json.Marshal(payload)
	if err != nil {
		return refreshResponse{}, fmt.Errorf("encode token refresh request: %w", err)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, s.refreshURL, bytes.NewReader(body))
	if err != nil {
		return refreshResponse{}, fmt.Errorf("create token refresh request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json")
	req.Header.Set("User-Agent", "grok-build-proxy")

	resp, err := s.httpClient.Do(req)
	if err != nil {
		return refreshResponse{}, fmt.Errorf("refresh Codex access token: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		_, _ = io.Copy(io.Discard, io.LimitReader(resp.Body, defaultMaxError))
		return refreshResponse{}, fmt.Errorf("refresh Codex access token: HTTP %d; run `codex login` again if the session was revoked", resp.StatusCode)
	}
	var result refreshResponse
	if err := json.NewDecoder(io.LimitReader(resp.Body, 1<<20)).Decode(&result); err != nil {
		return refreshResponse{}, fmt.Errorf("decode token refresh response: %w", err)
	}
	if strings.TrimSpace(result.AccessToken) == "" {
		return refreshResponse{}, errors.New("token refresh response did not include access_token")
	}
	return result, nil
}

func (s *Store) save(doc map[string]any) error {
	data, err := json.MarshalIndent(doc, "", "  ")
	if err != nil {
		return err
	}
	data = append(data, '\n')
	dir := filepath.Dir(s.path)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return err
	}
	tmp, err := os.CreateTemp(dir, ".auth.json.*")
	if err != nil {
		return err
	}
	tmpName := tmp.Name()
	defer os.Remove(tmpName)
	if err := tmp.Chmod(0o600); err != nil {
		tmp.Close()
		return err
	}
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Sync(); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	if err := os.Rename(tmpName, s.path); err != nil {
		// Windows may not replace an existing file atomically. Fall back to a
		// mode-restricted direct write while retaining the temp file cleanup.
		if writeErr := os.WriteFile(s.path, data, 0o600); writeErr != nil {
			return fmt.Errorf("rename temp auth file: %v; fallback write: %w", err, writeErr)
		}
	}
	return os.Chmod(s.path, 0o600)
}

func credentialsFromTokens(tokens map[string]any) (Credentials, string, error) {
	accessToken := strings.TrimSpace(stringValue(tokens["access_token"]))
	if accessToken == "" {
		return Credentials{}, "", errors.New("Codex auth file is missing access_token; run `codex login` again")
	}
	idToken := strings.TrimSpace(stringValue(tokens["id_token"]))
	accountID := strings.TrimSpace(stringValue(tokens["account_id"]))
	if accountID == "" {
		accountID = accountIDFromJWT(idToken)
	}
	if accountID == "" {
		accountID = accountIDFromJWT(accessToken)
	}
	expiresAt := expirationFromJWT(accessToken)
	if expiresAt.IsZero() {
		expiresAt = expirationFromJWT(idToken)
	}
	return Credentials{
		AccessToken: accessToken,
		AccountID:   accountID,
		ExpiresAt:   expiresAt,
	}, strings.TrimSpace(stringValue(tokens["refresh_token"])), nil
}

func resolvedAuthMode(doc map[string]any) string {
	if mode := strings.TrimSpace(stringValue(doc["auth_mode"])); mode != "" {
		return mode
	}
	if strings.TrimSpace(stringValue(doc["OPENAI_API_KEY"])) != "" {
		return "api_key"
	}
	if _, ok := doc["tokens"].(map[string]any); ok {
		return "chatgpt"
	}
	return "unknown"
}

func stringValue(v any) string {
	s, _ := v.(string)
	return s
}

func jwtClaims(token string) map[string]any {
	parts := strings.Split(token, ".")
	if len(parts) != 3 || parts[1] == "" {
		return nil
	}
	payload, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		return nil
	}
	var claims map[string]any
	dec := json.NewDecoder(bytes.NewReader(payload))
	dec.UseNumber()
	if err := dec.Decode(&claims); err != nil {
		return nil
	}
	return claims
}

func expirationFromJWT(token string) time.Time {
	claims := jwtClaims(token)
	if claims == nil {
		return time.Time{}
	}
	var seconds int64
	switch v := claims["exp"].(type) {
	case json.Number:
		seconds, _ = v.Int64()
	case float64:
		seconds = int64(v)
	case string:
		seconds, _ = strconv.ParseInt(v, 10, 64)
	}
	if seconds <= 0 {
		return time.Time{}
	}
	return time.Unix(seconds, 0).UTC()
}

func accountIDFromJWT(token string) string {
	claims := jwtClaims(token)
	if claims == nil {
		return ""
	}
	authClaims, _ := claims["https://api.openai.com/auth"].(map[string]any)
	if authClaims == nil {
		return ""
	}
	return strings.TrimSpace(stringValue(authClaims["chatgpt_account_id"]))
}
