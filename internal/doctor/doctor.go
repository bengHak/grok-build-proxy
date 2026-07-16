package doctor

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
	"time"

	"github.com/bengHak/grok-build-proxy/internal/auth"
	"github.com/bengHak/grok-build-proxy/internal/codexcli"
)

type Level string

const (
	Pass Level = "PASS"
	Warn Level = "WARN"
	Fail Level = "FAIL"
)

// Check is a single doctor result. Detail must not contain credential values.
type Check struct {
	Level       Level  `json:"level"`
	Name        string `json:"name"`
	Detail      string `json:"detail"`
	Remediation string `json:"remediation,omitempty"`
}

// Report is the complete diagnostic result.
type Report struct {
	Checks []Check `json:"checks"`
}

func (r Report) HasFailures() bool {
	for _, check := range r.Checks {
		if check.Level == Fail {
			return true
		}
	}
	return false
}

func (r Report) Counts() (passed, warnings, failed int) {
	for _, check := range r.Checks {
		switch check.Level {
		case Pass:
			passed++
		case Warn:
			warnings++
		case Fail:
			failed++
		}
	}
	return
}

func (r Report) Write(w io.Writer) {
	for _, check := range r.Checks {
		fmt.Fprintf(w, "[%s] %s: %s\n", check.Level, check.Name, check.Detail)
		if check.Remediation != "" {
			fmt.Fprintf(w, "       Fix: %s\n", check.Remediation)
		}
	}
	passed, warnings, failed := r.Counts()
	fmt.Fprintf(w, "\nSummary: %d passed, %d warning(s), %d failed\n", passed, warnings, failed)
}

// Config identifies the local installation to inspect.
type Config struct {
	RuntimeOS    string
	RuntimeArch  string
	Version      string
	CodexHome    string
	AuthFile     string
	CodexBinary  string
	GrokBinary   string
	GrokConfig   string
	Listen       string
	ClientToken  string
	Timeout      time.Duration
	Now          func() time.Time
	HTTPClient   *http.Client
	CommandEnv   []string
	SkipCommands bool
}

func Run(ctx context.Context, cfg Config) Report {
	cfg = withDefaults(cfg)
	report := Report{}
	report.Checks = append(report.Checks, checkPlatform(cfg))
	report.Checks = append(report.Checks, checkCodexCLI(ctx, cfg)...)
	report.Checks = append(report.Checks, checkCodexConfig(cfg))
	report.Checks = append(report.Checks, checkAuthFile(cfg)...)
	report.Checks = append(report.Checks, checkGrokCLI(ctx, cfg))
	report.Checks = append(report.Checks, checkGrokConfig(cfg))
	report.Checks = append(report.Checks, checkProxy(ctx, cfg)...)
	return report
}

func withDefaults(cfg Config) Config {
	if cfg.RuntimeOS == "" {
		cfg.RuntimeOS = runtime.GOOS
	}
	if cfg.RuntimeArch == "" {
		cfg.RuntimeArch = runtime.GOARCH
	}
	if cfg.CodexBinary == "" {
		cfg.CodexBinary = "codex"
	}
	if cfg.GrokBinary == "" {
		cfg.GrokBinary = "grok"
	}
	if cfg.Timeout <= 0 {
		cfg.Timeout = 5 * time.Second
	}
	if cfg.Now == nil {
		cfg.Now = time.Now
	}
	if cfg.HTTPClient == nil {
		cfg.HTTPClient = &http.Client{Timeout: cfg.Timeout}
	}
	return cfg
}

func checkPlatform(cfg Config) Check {
	if cfg.RuntimeOS != "darwin" {
		return Check{
			Level:       Fail,
			Name:        "Platform",
			Detail:      fmt.Sprintf("unsupported %s/%s", cfg.RuntimeOS, cfg.RuntimeArch),
			Remediation: "run grok-build-proxy on macOS",
		}
	}
	if cfg.RuntimeArch != "arm64" && cfg.RuntimeArch != "amd64" {
		return Check{
			Level:       Fail,
			Name:        "Platform",
			Detail:      fmt.Sprintf("unsupported macOS architecture %s", cfg.RuntimeArch),
			Remediation: "use an Apple Silicon or Intel Mac",
		}
	}
	return Check{Level: Pass, Name: "Platform", Detail: fmt.Sprintf("macOS/%s, proxy %s", cfg.RuntimeArch, cfg.Version)}
}

func checkCodexCLI(ctx context.Context, cfg Config) []Check {
	if cfg.SkipCommands {
		return []Check{{Level: Warn, Name: "Codex CLI", Detail: "command checks skipped"}}
	}
	binary, err := exec.LookPath(cfg.CodexBinary)
	if err != nil {
		return []Check{{
			Level:       Fail,
			Name:        "Codex CLI",
			Detail:      "official Codex CLI was not found in PATH",
			Remediation: "install Codex, then run `grok-build-proxy auth login`",
		}}
	}
	output, versionErr := commandOutput(ctx, cfg.Timeout, cfg.CommandEnv, cfg.CodexHome, binary, "--version")
	if versionErr != nil {
		output = filepath.Base(binary)
	}
	checks := []Check{{Level: Pass, Name: "Codex CLI", Detail: compactDetail(output, binary)}}

	statusOutput, statusErr := commandOutput(ctx, cfg.Timeout, cfg.CommandEnv, cfg.CodexHome, binary, "login", "status")
	if statusErr != nil {
		detail := compactDetail(statusOutput, statusErr.Error())
		checks = append(checks, Check{
			Level:       Fail,
			Name:        "Codex login status",
			Detail:      detail,
			Remediation: "run `grok-build-proxy auth login`",
		})
	} else {
		checks = append(checks, Check{Level: Pass, Name: "Codex login status", Detail: compactDetail(statusOutput, "authenticated")})
	}
	return checks
}

func checkCodexConfig(cfg Config) Check {
	status, err := codexcli.InspectAuthConfig(cfg.CodexHome)
	if err != nil {
		return Check{
			Level:       Fail,
			Name:        "Codex credential storage",
			Detail:      fmt.Sprintf("cannot read %s", status.Path),
			Remediation: "run `grok-build-proxy auth login` to create a file-backed Codex home",
		}
	}
	if status.CredentialStore != "file" {
		return Check{
			Level:       Fail,
			Name:        "Codex credential storage",
			Detail:      fmt.Sprintf("%s is %q, expected \"file\"", codexcli.CredentialStoreKey, status.CredentialStore),
			Remediation: "run `grok-build-proxy auth login`",
		}
	}
	if status.ForcedLogin != "chatgpt" {
		return Check{
			Level:       Warn,
			Name:        "Codex login policy",
			Detail:      fmt.Sprintf("%s is %q, expected \"chatgpt\"", codexcli.ForcedLoginKey, status.ForcedLogin),
			Remediation: "run `grok-build-proxy auth login`",
		}
	}
	return Check{Level: Pass, Name: "Codex credential storage", Detail: fmt.Sprintf("file-backed credentials in %s", cfg.CodexHome)}
}

func checkAuthFile(cfg Config) []Check {
	store, err := auth.NewStore(auth.Config{Path: cfg.AuthFile, Now: cfg.Now})
	if err != nil {
		return []Check{{Level: Fail, Name: "Codex authentication", Detail: err.Error()}}
	}
	status, err := store.Inspect()
	if err != nil {
		return []Check{{
			Level:       Fail,
			Name:        "Codex authentication",
			Detail:      err.Error(),
			Remediation: "run `grok-build-proxy auth login`",
		}}
	}

	checks := make([]Check, 0, 4)
	mode := strings.ToLower(strings.TrimSpace(status.AuthMode))
	if mode != "chatgpt" && mode != "chatgptauthtokens" {
		checks = append(checks, Check{
			Level:       Fail,
			Name:        "Codex authentication",
			Detail:      fmt.Sprintf("auth mode is %q, not a ChatGPT subscription session", status.AuthMode),
			Remediation: "run `grok-build-proxy auth login` and sign in with ChatGPT",
		})
	} else {
		detail := "ChatGPT session"
		if status.AccountID != "" {
			detail += " for account " + maskIdentifier(status.AccountID)
		}
		checks = append(checks, Check{Level: Pass, Name: "Codex authentication", Detail: detail})
	}

	if status.FileMode.Perm()&0o077 != 0 {
		checks = append(checks, Check{
			Level:       Fail,
			Name:        "Credential permissions",
			Detail:      fmt.Sprintf("%s has mode %04o", status.Path, status.FileMode.Perm()),
			Remediation: fmt.Sprintf("chmod 600 %q", status.Path),
		})
	} else {
		checks = append(checks, Check{Level: Pass, Name: "Credential permissions", Detail: fmt.Sprintf("%s is user-only (%04o)", status.Path, status.FileMode.Perm())})
	}

	if status.HasRefreshToken {
		checks = append(checks, Check{Level: Pass, Name: "Refresh token", Detail: "present"})
	} else {
		checks = append(checks, Check{
			Level:       Warn,
			Name:        "Refresh token",
			Detail:      "missing; the current session cannot be renewed by the proxy",
			Remediation: "run `grok-build-proxy auth login`",
		})
	}

	if status.ExpiresAt.IsZero() {
		checks = append(checks, Check{Level: Warn, Name: "Access token expiry", Detail: "could not be determined from the cached JWT"})
	} else if !status.ExpiresAt.After(cfg.Now()) {
		level := Warn
		if !status.HasRefreshToken {
			level = Fail
		}
		checks = append(checks, Check{
			Level:       level,
			Name:        "Access token expiry",
			Detail:      fmt.Sprintf("expired at %s", status.ExpiresAt.Local().Format(time.RFC3339)),
			Remediation: "start the proxy to refresh it, or run `grok-build-proxy auth login`",
		})
	} else {
		checks = append(checks, Check{Level: Pass, Name: "Access token expiry", Detail: status.ExpiresAt.Local().Format(time.RFC3339)})
	}
	return checks
}

func checkGrokCLI(ctx context.Context, cfg Config) Check {
	if cfg.SkipCommands {
		return Check{Level: Warn, Name: "Grok Build CLI", Detail: "command checks skipped"}
	}
	binary, err := exec.LookPath(cfg.GrokBinary)
	if err != nil {
		return Check{
			Level:       Fail,
			Name:        "Grok Build CLI",
			Detail:      "grok was not found in PATH",
			Remediation: "install Grok Build before using the proxy",
		}
	}
	output, versionErr := commandOutput(ctx, cfg.Timeout, cfg.CommandEnv, "", binary, "--version")
	if versionErr != nil {
		return Check{Level: Pass, Name: "Grok Build CLI", Detail: binary}
	}
	return Check{Level: Pass, Name: "Grok Build CLI", Detail: compactDetail(output, binary)}
}

func checkGrokConfig(cfg Config) Check {
	data, err := os.ReadFile(cfg.GrokConfig)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return Check{
				Level:       Fail,
				Name:        "Grok Build config",
				Detail:      fmt.Sprintf("%s does not exist", cfg.GrokConfig),
				Remediation: "run `grok-build-proxy --print-grok-config` and add a model block",
			}
		}
		return Check{Level: Fail, Name: "Grok Build config", Detail: err.Error()}
	}
	text := string(data)
	if !containsTOMLString(text, "api_backend", "responses") {
		return Check{
			Level:       Fail,
			Name:        "Grok Build config",
			Detail:      "no model using api_backend = \"responses\" was found",
			Remediation: "add a model block printed by `grok-build-proxy --print-grok-config`",
		}
	}
	urls := tomlStringValues(text, "base_url")
	for _, raw := range urls {
		if matchesListen(raw, cfg.Listen) {
			return Check{Level: Pass, Name: "Grok Build config", Detail: fmt.Sprintf("Responses model points to %s", raw)}
		}
	}
	return Check{
		Level:       Fail,
		Name:        "Grok Build config",
		Detail:      fmt.Sprintf("no model base_url points to %s", cfg.Listen),
		Remediation: "update the model base_url or run `grok-build-proxy --print-grok-config`",
	}
}

func checkProxy(ctx context.Context, cfg Config) []Check {
	baseURL := "http://" + cfg.Listen
	health, healthErr := getEndpoint(ctx, cfg, baseURL+"/healthz", false)
	if healthErr == nil && health.StatusCode == http.StatusOK {
		service := stringValue(health.JSON["service"])
		if service != "grok-build-proxy" {
			return []Check{{
				Level:       Fail,
				Name:        "Proxy endpoint",
				Detail:      fmt.Sprintf("%s is occupied by %q", cfg.Listen, service),
				Remediation: "choose another --listen address",
			}}
		}
		checks := []Check{{Level: Pass, Name: "Proxy endpoint", Detail: fmt.Sprintf("running at %s", baseURL)}}
		ready, readyErr := getEndpoint(ctx, cfg, baseURL+"/readyz", true)
		if readyErr != nil {
			checks = append(checks, Check{Level: Fail, Name: "Proxy readiness", Detail: readyErr.Error(), Remediation: "inspect the proxy logs and run `grok-build-proxy auth status`"})
		} else if ready.StatusCode != http.StatusOK {
			checks = append(checks, Check{Level: Fail, Name: "Proxy readiness", Detail: fmt.Sprintf("HTTP %d: %s", ready.StatusCode, compactDetail(ready.Body, "not ready")), Remediation: "run `grok-build-proxy auth login`"})
		} else {
			checks = append(checks, Check{Level: Pass, Name: "Proxy readiness", Detail: "credentials are ready"})
		}
		return checks
	}

	listener, err := net.Listen("tcp", cfg.Listen)
	if err != nil {
		detail := err.Error()
		if healthErr != nil {
			detail = fmt.Sprintf("port is occupied and health check failed: %v", healthErr)
		}
		return []Check{{
			Level:       Fail,
			Name:        "Proxy endpoint",
			Detail:      detail,
			Remediation: "stop the process using the port or select another --listen address",
		}}
	}
	_ = listener.Close()
	return []Check{{
		Level:       Warn,
		Name:        "Proxy endpoint",
		Detail:      fmt.Sprintf("not running; %s is available", cfg.Listen),
		Remediation: "start `grok-build-proxy` after the other checks pass",
	}}
}

type endpointResult struct {
	StatusCode int
	Body       string
	JSON       map[string]any
}

func getEndpoint(ctx context.Context, cfg Config, endpoint string, authorized bool) (endpointResult, error) {
	requestCtx, cancel := context.WithTimeout(ctx, cfg.Timeout)
	defer cancel()
	req, err := http.NewRequestWithContext(requestCtx, http.MethodGet, endpoint, nil)
	if err != nil {
		return endpointResult{}, err
	}
	if authorized && strings.TrimSpace(cfg.ClientToken) != "" {
		req.Header.Set("Authorization", "Bearer "+strings.TrimSpace(cfg.ClientToken))
	}
	resp, err := cfg.HTTPClient.Do(req)
	if err != nil {
		return endpointResult{}, err
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(io.LimitReader(resp.Body, 64<<10))
	if err != nil {
		return endpointResult{}, err
	}
	result := endpointResult{StatusCode: resp.StatusCode, Body: strings.TrimSpace(string(body))}
	_ = json.Unmarshal(body, &result.JSON)
	return result, nil
}

func commandOutput(ctx context.Context, timeout time.Duration, extraEnv []string, codexHome, binary string, args ...string) (string, error) {
	commandCtx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	cmd := exec.CommandContext(commandCtx, binary, args...)
	cmd.Env = append([]string(nil), os.Environ()...)
	cmd.Env = append(cmd.Env, extraEnv...)
	if codexHome != "" {
		cmd.Env = replaceEnv(cmd.Env, "CODEX_HOME", codexHome)
	}
	output, err := cmd.CombinedOutput()
	text := strings.TrimSpace(string(output))
	if errors.Is(commandCtx.Err(), context.DeadlineExceeded) {
		return text, fmt.Errorf("command timed out after %s", timeout)
	}
	return text, err
}

func replaceEnv(env []string, key, value string) []string {
	prefix := key + "="
	result := make([]string, 0, len(env)+1)
	for _, entry := range env {
		if !strings.HasPrefix(entry, prefix) {
			result = append(result, entry)
		}
	}
	return append(result, prefix+value)
}

func compactDetail(value, fallback string) string {
	value = strings.TrimSpace(value)
	if value == "" {
		return fallback
	}
	fields := strings.Fields(value)
	if len(fields) > 24 {
		fields = fields[:24]
		return strings.Join(fields, " ") + "…"
	}
	return strings.Join(fields, " ")
}

func maskIdentifier(value string) string {
	value = strings.TrimSpace(value)
	if len(value) <= 8 {
		return "…" + value
	}
	return value[:4] + "…" + value[len(value)-4:]
}

func stringValue(value any) string {
	text, _ := value.(string)
	return text
}

func containsTOMLString(content, key, want string) bool {
	for _, value := range tomlStringValues(content, key) {
		if value == want {
			return true
		}
	}
	return false
}

func tomlStringValues(content, key string) []string {
	values := make([]string, 0)
	for _, rawLine := range strings.Split(content, "\n") {
		line := strings.TrimSpace(rawLine)
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		parts := strings.SplitN(line, "=", 2)
		if len(parts) != 2 || strings.TrimSpace(parts[0]) != key {
			continue
		}
		value := strings.TrimSpace(strings.SplitN(parts[1], "#", 2)[0])
		value = strings.Trim(value, `"'`)
		if value != "" {
			values = append(values, value)
		}
	}
	sort.Strings(values)
	return values
}

func matchesListen(rawURL, listen string) bool {
	parsed, err := url.Parse(rawURL)
	if err != nil || parsed.Scheme != "http" {
		return false
	}
	if parsed.Path != "/v1" && !strings.HasPrefix(parsed.Path, "/v1/") {
		return false
	}
	_, listenPort, err := net.SplitHostPort(listen)
	if err != nil {
		return false
	}
	if parsed.Port() != listenPort {
		return false
	}
	host := parsed.Hostname()
	if strings.EqualFold(host, "localhost") {
		return true
	}
	ip := net.ParseIP(host)
	return ip != nil && ip.IsLoopback()
}
