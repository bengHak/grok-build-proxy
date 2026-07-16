package proxy

import (
	"bytes"
	"context"
	"crypto/rand"
	"crypto/subtle"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/url"
	"strings"
	"time"

	"github.com/bengHak/grok-build-proxy/internal/auth"
	"github.com/bengHak/grok-build-proxy/internal/catalog"
)

const defaultMaxBodyBytes int64 = 64 << 20

type CredentialProvider interface {
	Get(ctx context.Context, forceRefresh bool) (auth.Credentials, error)
}

type Config struct {
	UpstreamURL  string
	Credentials  CredentialProvider
	Catalog      catalog.Catalog
	HTTPClient   *http.Client
	Logger       *slog.Logger
	ClientToken  string
	Version      string
	MaxBodyBytes int64
}

type Handler struct {
	upstream     *url.URL
	credentials  CredentialProvider
	catalog      catalog.Catalog
	httpClient   *http.Client
	logger       *slog.Logger
	clientToken  string
	version      string
	maxBodyBytes int64
}

func New(cfg Config) (*Handler, error) {
	if cfg.Credentials == nil {
		return nil, errors.New("credential provider is required")
	}
	upstream, err := url.Parse(cfg.UpstreamURL)
	if err != nil || upstream.Scheme == "" || upstream.Host == "" {
		return nil, fmt.Errorf("invalid upstream URL %q", cfg.UpstreamURL)
	}
	if upstream.Scheme != "https" && upstream.Scheme != "http" {
		return nil, fmt.Errorf("unsupported upstream URL scheme %q", upstream.Scheme)
	}
	if cfg.HTTPClient == nil {
		cfg.HTTPClient = &http.Client{Transport: &http.Transport{
			Proxy:                 http.ProxyFromEnvironment,
			MaxIdleConns:          100,
			MaxIdleConnsPerHost:   20,
			IdleConnTimeout:       90 * time.Second,
			ResponseHeaderTimeout: 90 * time.Second,
			ForceAttemptHTTP2:     true,
		}}
	}
	if cfg.Logger == nil {
		cfg.Logger = slog.Default()
	}
	if cfg.MaxBodyBytes <= 0 {
		cfg.MaxBodyBytes = defaultMaxBodyBytes
	}
	return &Handler{
		upstream:     upstream,
		credentials:  cfg.Credentials,
		catalog:      cfg.Catalog,
		httpClient:   cfg.HTTPClient,
		logger:       cfg.Logger,
		clientToken:  strings.TrimSpace(cfg.ClientToken),
		version:      cfg.Version,
		maxBodyBytes: cfg.MaxBodyBytes,
	}, nil
}

func (h *Handler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	switch r.URL.Path {
	case "/", "/healthz":
		h.handleHealth(w, r)
	case "/readyz":
		if !h.authorize(w, r) {
			return
		}
		h.handleReady(w, r)
	case "/v1/models", "/models":
		if !h.authorize(w, r) {
			return
		}
		h.handleModels(w, r)
	case "/v1/responses", "/responses":
		if !h.authorize(w, r) {
			return
		}
		h.handleResponses(w, r)
	default:
		writeError(w, http.StatusNotFound, "not_found_error", "endpoint not found")
	}
}

func (h *Handler) authorize(w http.ResponseWriter, r *http.Request) bool {
	if h.clientToken == "" {
		return true
	}
	value := strings.TrimSpace(r.Header.Get("Authorization"))
	want := "Bearer " + h.clientToken
	if len(value) != len(want) || subtle.ConstantTimeCompare([]byte(value), []byte(want)) != 1 {
		w.Header().Set("WWW-Authenticate", `Bearer realm="grok-build-proxy"`)
		writeError(w, http.StatusUnauthorized, "authentication_error", "invalid proxy bearer token")
		return false
	}
	return true
}

func (h *Handler) handleHealth(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "invalid_request_error", "method not allowed")
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"ok":      true,
		"service": "grok-build-proxy",
		"version": h.version,
	})
}

func (h *Handler) handleReady(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "invalid_request_error", "method not allowed")
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 15*time.Second)
	defer cancel()
	if _, err := h.credentials.Get(ctx, false); err != nil {
		writeError(w, http.StatusServiceUnavailable, "authentication_error", err.Error())
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"ok": true, "auth": "ready"})
}

func (h *Handler) handleModels(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "invalid_request_error", "method not allowed")
		return
	}
	type modelResponse struct {
		ID            string `json:"id"`
		Object        string `json:"object"`
		OwnedBy       string `json:"owned_by"`
		DisplayName   string `json:"name,omitempty"`
		Description   string `json:"description,omitempty"`
		ContextWindow int    `json:"context_window,omitempty"`
		APIBackend    string `json:"api_backend,omitempty"`
	}
	models := h.catalog.Models()
	data := make([]modelResponse, 0, len(models)*2)
	for _, model := range models {
		data = append(data, modelResponse{
			ID:            model.ID,
			Object:        "model",
			OwnedBy:       "openai-codex",
			DisplayName:   model.DisplayName,
			Description:   model.Description,
			ContextWindow: model.ContextWindow,
			APIBackend:    "responses",
		})
		if strings.HasPrefix(model.ID, "gpt-5.6-") || model.ID == "gpt-5.5" {
			data = append(data, modelResponse{
				ID:            model.ID + "-fast",
				Object:        "model",
				OwnedBy:       "openai-codex",
				DisplayName:   model.DisplayName + " (Fast)",
				Description:   model.Description,
				ContextWindow: model.ContextWindow,
				APIBackend:    "responses",
			})
		}
	}
	writeJSON(w, http.StatusOK, map[string]any{"object": "list", "data": data})
}

func (h *Handler) handleResponses(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		writeError(w, http.StatusMethodNotAllowed, "invalid_request_error", "method not allowed")
		return
	}
	started := time.Now()
	requestID := newRequestID()
	w.Header().Set("X-Request-ID", requestID)

	limited := http.MaxBytesReader(w, r.Body, h.maxBodyBytes)
	body, err := io.ReadAll(limited)
	if err != nil {
		var maxBytesErr *http.MaxBytesError
		if errors.As(err, &maxBytesErr) {
			writeError(w, http.StatusRequestEntityTooLarge, "invalid_request_error", "request body exceeds proxy limit")
		} else {
			writeError(w, http.StatusBadRequest, "invalid_request_error", "failed to read request body")
		}
		return
	}
	transformed, err := transformRequest(body, h.catalog)
	if err != nil {
		writeError(w, http.StatusBadRequest, "invalid_request_error", err.Error())
		return
	}

	resp, err := h.sendUpstream(r.Context(), r, transformed, requestID, false)
	if err != nil {
		h.logger.Error("upstream request failed", "request_id", requestID, "model", transformed.Model, "error", err)
		writeError(w, http.StatusBadGateway, "upstream_error", err.Error())
		return
	}
	if resp.StatusCode == http.StatusUnauthorized {
		io.Copy(io.Discard, io.LimitReader(resp.Body, 1<<20))
		resp.Body.Close()
		resp, err = h.sendUpstream(r.Context(), r, transformed, requestID, true)
		if err != nil {
			h.logger.Error("upstream retry failed", "request_id", requestID, "model", transformed.Model, "error", err)
			writeError(w, http.StatusBadGateway, "upstream_error", err.Error())
			return
		}
	}
	defer resp.Body.Close()

	copyResponseHeaders(w.Header(), resp.Header)
	w.Header().Set("X-Grok-Build-Proxy-Version", h.version)
	w.WriteHeader(resp.StatusCode)
	copyErr := copyResponseBody(w, resp.Body, strings.Contains(strings.ToLower(resp.Header.Get("Content-Type")), "text/event-stream"))
	if copyErr != nil && !errors.Is(copyErr, context.Canceled) {
		h.logger.Warn("response stream ended with error", "request_id", requestID, "model", transformed.Model, "error", copyErr)
	}
	h.logger.Info("request complete",
		"request_id", requestID,
		"model", transformed.Model,
		"responses_lite", transformed.Lite,
		"fast", transformed.Fast,
		"status", resp.StatusCode,
		"duration_ms", time.Since(started).Milliseconds(),
	)
}

func (h *Handler) sendUpstream(ctx context.Context, incoming *http.Request, transformed transformedRequest, requestID string, forceRefresh bool) (*http.Response, error) {
	creds, err := h.credentials.Get(ctx, forceRefresh)
	if err != nil {
		return nil, fmt.Errorf("load Codex credentials: %w", err)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, h.upstream.String(), bytes.NewReader(transformed.Body))
	if err != nil {
		return nil, fmt.Errorf("create upstream request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+creds.AccessToken)
	req.Header.Set("Content-Type", "application/json")
	if transformed.Stream {
		req.Header.Set("Accept", "text/event-stream")
	} else {
		req.Header.Set("Accept", "application/json")
	}
	req.Header.Set("OpenAI-Beta", "responses=experimental")
	req.Header.Set("User-Agent", "grok-build-proxy/"+h.version)
	if transformed.Lite {
		req.Header.Set("Originator", "codex_cli_rs")
		req.Header.Set("X-OpenAI-Internal-Codex-Responses-Lite", "true")
	} else {
		req.Header.Set("Originator", "grok-build-proxy")
	}
	if creds.AccountID != "" {
		req.Header.Set("ChatGPT-Account-ID", creds.AccountID)
	}
	if value := incoming.Header.Get("traceparent"); validHeaderValue(value) {
		req.Header.Set("traceparent", value)
	}
	if value := incoming.Header.Get("tracestate"); validHeaderValue(value) {
		req.Header.Set("tracestate", value)
	}
	sessionID := firstValidHeader(
		incoming.Header.Get("x-grok-session-id"),
		incoming.Header.Get("x-grok-conv-id"),
		incoming.Header.Get("x-request-id"),
		requestID,
	)
	if sessionID != "" {
		req.Header.Set("session_id", sessionID)
		req.Header.Set("x-client-request-id", sessionID)
		req.Header.Set("x-codex-window-id", sessionID+":0")
	}
	return h.httpClient.Do(req)
}

func writeJSON(w http.ResponseWriter, status int, value any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(value)
}

func writeError(w http.ResponseWriter, status int, kind, message string) {
	writeJSON(w, status, map[string]any{
		"error": map[string]any{
			"message": message,
			"type":    kind,
		},
	})
}

var hopByHopHeaders = map[string]struct{}{
	"connection":          {},
	"proxy-connection":    {},
	"keep-alive":          {},
	"proxy-authenticate":  {},
	"proxy-authorization": {},
	"te":                  {},
	"trailer":             {},
	"transfer-encoding":   {},
	"upgrade":             {},
	"content-length":      {},
	"set-cookie":          {},
}

func copyResponseHeaders(dst, src http.Header) {
	for key, values := range src {
		if _, skip := hopByHopHeaders[strings.ToLower(key)]; skip {
			continue
		}
		for _, value := range values {
			dst.Add(key, value)
		}
	}
}

func copyResponseBody(dst http.ResponseWriter, src io.Reader, flush bool) error {
	if !flush {
		_, err := io.Copy(dst, src)
		return err
	}
	flusher, canFlush := dst.(http.Flusher)
	buf := make([]byte, 16*1024)
	for {
		n, err := src.Read(buf)
		if n > 0 {
			if _, writeErr := dst.Write(buf[:n]); writeErr != nil {
				return writeErr
			}
			if canFlush {
				flusher.Flush()
			}
		}
		if err != nil {
			if errors.Is(err, io.EOF) {
				return nil
			}
			return err
		}
	}
}

func newRequestID() string {
	var value [16]byte
	if _, err := rand.Read(value[:]); err != nil {
		return fmt.Sprintf("00000000-0000-4000-8000-%012x", time.Now().UnixNano()&0xffffffffffff)
	}
	value[6] = (value[6] & 0x0f) | 0x40
	value[8] = (value[8] & 0x3f) | 0x80
	hexValue := hex.EncodeToString(value[:])
	return hexValue[0:8] + "-" + hexValue[8:12] + "-" + hexValue[12:16] + "-" + hexValue[16:20] + "-" + hexValue[20:32]
}

func firstValidHeader(values ...string) string {
	for _, value := range values {
		value = strings.TrimSpace(value)
		if value != "" && validHeaderValue(value) {
			return value
		}
	}
	return ""
}

func validHeaderValue(value string) bool {
	if value == "" || len(value) > 512 {
		return false
	}
	for _, r := range value {
		if r < 0x20 || r == 0x7f {
			return false
		}
	}
	return true
}

// IsLoopbackListen reports whether a listen address is safely restricted to
// the local machine. Hostnames other than localhost are treated as non-loopback.
func IsLoopbackListen(address string) bool {
	host, _, err := net.SplitHostPort(address)
	if err != nil {
		return false
	}
	if strings.EqualFold(host, "localhost") {
		return true
	}
	ip := net.ParseIP(host)
	return ip != nil && ip.IsLoopback()
}
