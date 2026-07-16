package codexcli

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
)

// Action identifies an official Codex CLI authentication operation.
type Action string

const (
	ActionLogin  Action = "login"
	ActionDevice Action = "device"
	ActionStatus Action = "status"
	ActionLogout Action = "logout"

	CredentialStoreKey = "cli_auth_credentials_store"
	ForcedLoginKey     = "forced_login_method"
)

var rootAssignmentPattern = regexp.MustCompile(`^\s*([A-Za-z0-9_-]+)\s*=\s*(.*?)\s*(?:#.*)?$`)

// Client delegates authentication to the official Codex CLI while pinning it to
// a dedicated CODEX_HOME managed by grok-build-proxy.
type Client struct {
	binary string
	home   string
	stdin  io.Reader
	stdout io.Writer
	stderr io.Writer
	env    []string
}

// Config controls how the official Codex CLI is invoked.
type Config struct {
	Binary string
	Home   string
	Stdin  io.Reader
	Stdout io.Writer
	Stderr io.Writer
	Env    []string
}

func New(cfg Config) (*Client, error) {
	if strings.TrimSpace(cfg.Home) == "" {
		return nil, errors.New("Codex home is required")
	}
	binary := strings.TrimSpace(cfg.Binary)
	if binary == "" {
		binary = "codex"
	}
	resolved, err := exec.LookPath(binary)
	if err != nil {
		return nil, fmt.Errorf("official Codex CLI not found (%s): %w", binary, err)
	}
	if cfg.Stdin == nil {
		cfg.Stdin = os.Stdin
	}
	if cfg.Stdout == nil {
		cfg.Stdout = os.Stdout
	}
	if cfg.Stderr == nil {
		cfg.Stderr = os.Stderr
	}
	return &Client{
		binary: resolved,
		home:   filepath.Clean(cfg.Home),
		stdin:  cfg.Stdin,
		stdout: cfg.Stdout,
		stderr: cfg.Stderr,
		env:    append([]string(nil), cfg.Env...),
	}, nil
}

func (c *Client) Binary() string { return c.binary }
func (c *Client) Home() string   { return c.home }

// Prepare creates a private Codex home and configures the official CLI to use a
// file-backed ChatGPT login. Existing unrelated TOML settings are preserved.
func (c *Client) Prepare() error {
	return EnsureAuthConfig(c.home)
}

func (c *Client) Login(ctx context.Context, device bool) error {
	if err := c.Prepare(); err != nil {
		return err
	}
	args := []string{"login"}
	if device {
		args = append(args, "--device-auth")
	}
	return c.run(ctx, args...)
}

func (c *Client) Status(ctx context.Context) error {
	if err := c.Prepare(); err != nil {
		return err
	}
	return c.run(ctx, "login", "status")
}

func (c *Client) Logout(ctx context.Context) error {
	if err := c.Prepare(); err != nil {
		return err
	}
	return c.run(ctx, "logout")
}

func (c *Client) run(ctx context.Context, args ...string) error {
	cmd := exec.CommandContext(ctx, c.binary, args...)
	cmd.Stdin = c.stdin
	cmd.Stdout = c.stdout
	cmd.Stderr = c.stderr
	cmd.Env = withEnvironment(c.env, "CODEX_HOME", c.home)
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("codex %s failed: %w", strings.Join(args, " "), err)
	}
	return nil
}

// EnsureAuthConfig configures a dedicated Codex home for plaintext file-backed
// ChatGPT credentials. The file is written atomically with user-only permissions.
func EnsureAuthConfig(home string) error {
	home = filepath.Clean(strings.TrimSpace(home))
	if home == "." || home == "" {
		return errors.New("Codex home is required")
	}
	if err := os.MkdirAll(home, 0o700); err != nil {
		return fmt.Errorf("create Codex home: %w", err)
	}
	if err := os.Chmod(home, 0o700); err != nil {
		return fmt.Errorf("protect Codex home: %w", err)
	}

	path := filepath.Join(home, "config.toml")
	content, err := os.ReadFile(path)
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("read Codex config: %w", err)
	}
	updated := string(content)
	updated = setRootString(updated, CredentialStoreKey, "file")
	updated = setRootString(updated, ForcedLoginKey, "chatgpt")
	if !strings.HasSuffix(updated, "\n") {
		updated += "\n"
	}
	if err := writeAtomic(path, []byte(updated), 0o600); err != nil {
		return fmt.Errorf("write Codex config: %w", err)
	}
	return nil
}

// AuthConfigStatus reports the effective top-level settings required by the
// proxy without changing the file.
type AuthConfigStatus struct {
	Path            string
	CredentialStore string
	ForcedLogin     string
}

func InspectAuthConfig(home string) (AuthConfigStatus, error) {
	path := filepath.Join(filepath.Clean(home), "config.toml")
	content, err := os.ReadFile(path)
	if err != nil {
		return AuthConfigStatus{Path: path}, err
	}
	settings := rootSettings(string(content))
	return AuthConfigStatus{
		Path:            path,
		CredentialStore: settings[CredentialStoreKey],
		ForcedLogin:     settings[ForcedLoginKey],
	}, nil
}

func setRootString(content, key, value string) string {
	lines := splitLines(content)
	assignment := fmt.Sprintf("%s = %q\n", key, value)
	inRoot := true
	for index, line := range lines {
		trimmed := strings.TrimSpace(line)
		if strings.HasPrefix(trimmed, "[") && strings.HasSuffix(strings.TrimSuffix(trimmed, "\r"), "]") {
			inRoot = false
		}
		if !inRoot || trimmed == "" || strings.HasPrefix(trimmed, "#") {
			continue
		}
		match := rootAssignmentPattern.FindStringSubmatch(strings.TrimSuffix(line, "\n"))
		if len(match) == 3 && match[1] == key {
			lines[index] = assignment
			return strings.Join(lines, "")
		}
	}

	insertAt := 0
	for insertAt < len(lines) {
		trimmed := strings.TrimSpace(lines[insertAt])
		if strings.HasPrefix(trimmed, "[") {
			break
		}
		insertAt++
	}
	prefix := append([]string(nil), lines[:insertAt]...)
	if insertAt > 0 && strings.TrimSpace(prefix[len(prefix)-1]) != "" {
		prefix = append(prefix, "\n")
	}
	prefix = append(prefix, assignment)
	if insertAt < len(lines) && strings.TrimSpace(lines[insertAt]) != "" {
		prefix = append(prefix, "\n")
	}
	prefix = append(prefix, lines[insertAt:]...)
	return strings.Join(prefix, "")
}

func rootSettings(content string) map[string]string {
	result := make(map[string]string)
	scanner := bufio.NewScanner(strings.NewReader(content))
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if strings.HasPrefix(line, "[") {
			break
		}
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		match := rootAssignmentPattern.FindStringSubmatch(line)
		if len(match) != 3 {
			continue
		}
		value := strings.TrimSpace(match[2])
		value = strings.Trim(value, `"'`)
		result[match[1]] = value
	}
	return result
}

func splitLines(content string) []string {
	if content == "" {
		return nil
	}
	parts := strings.SplitAfter(content, "\n")
	if parts[len(parts)-1] == "" {
		parts = parts[:len(parts)-1]
	}
	return parts
}

func withEnvironment(extra []string, key, value string) []string {
	env := append([]string(nil), os.Environ()...)
	env = append(env, extra...)
	prefix := key + "="
	filtered := env[:0]
	for _, entry := range env {
		if strings.HasPrefix(entry, prefix) {
			continue
		}
		filtered = append(filtered, entry)
	}
	return append(filtered, prefix+value)
}

func writeAtomic(path string, data []byte, mode os.FileMode) error {
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return err
	}
	file, err := os.CreateTemp(dir, ".config.toml.*")
	if err != nil {
		return err
	}
	name := file.Name()
	defer os.Remove(name)
	if err := file.Chmod(mode); err != nil {
		file.Close()
		return err
	}
	if _, err := file.Write(data); err != nil {
		file.Close()
		return err
	}
	if err := file.Sync(); err != nil {
		file.Close()
		return err
	}
	if err := file.Close(); err != nil {
		return err
	}
	if err := os.Rename(name, path); err != nil {
		return err
	}
	return os.Chmod(path, mode)
}
