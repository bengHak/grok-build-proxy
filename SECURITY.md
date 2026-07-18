# Security policy

`grok-build-proxy` handles ChatGPT/Codex and Kimi access and refresh tokens on
macOS. Treat both authentication files as passwords.

## Safe defaults

- The server binds to `127.0.0.1` by default.
- Binding to a non-loopback address is rejected unless an inbound bearer token
  is configured with `GROK_BUILD_PROXY_TOKEN` or `--client-token`.
- Request bodies, successful response bodies, and authorization values are not
  logged. For upstream 4xx/5xx responses and semantic failure terminals carried
  by HTTP 200 responses, the proxy records only a bounded, single-line error
  summary with common credential shapes redacted; the original response body is
  forwarded to the local caller.
- The installer writes only the executable into the selected installation
  directory and does not read or copy Codex credentials.
- `grok-build-proxy auth` delegates login, device authorization, status, and
  logout to the official Codex CLI. It does not collect ChatGPT passwords or
  implement its own OAuth callback flow.
- `grok-build-proxy kimi auth` uses Kimi's device-code OAuth endpoint. It never
  collects a Kimi password and never prints access or refresh tokens.
- `grok-build-proxy doctor` reports only redacted account metadata and never
  prints access or refresh token values.
- Serve-monitor failure reports (`y`/`Y` clipboard, `w`/`W` under
  `~/.grok/proxy-reports/`) include selected `FailureRecord` metadata. They omit
  diagnostic error messages, prompt/request bodies, response bodies, and
  credentials.

## Credential handling

- Do not commit, upload, back up to a public location, or share `auth.json`.
- The default proxy credential directory is `~/.codex-grok-build-proxy`.
- Kimi credentials default to `~/.grok-build-proxy/kimi/auth.json`; the adjacent
  `device_id` is not a bearer credential but is still kept private.
- The auth wrapper configures `cli_auth_credentials_store = "file"` and
  `forced_login_method = "chatgpt"` in that dedicated Codex home.
- Keep the authentication file readable only by your macOS user. The doctor
  reports group or world-readable credentials as a blocking problem.
- Kimi auth and device ID files are atomically written with mode `0600` in a
  mode `0700` directory.
- Do not point multiple long-running proxy processes at the same credential
  file. A dedicated home reduces refresh-token races with normal Codex sessions.
- Log out with `grok-build-proxy auth logout` or `grok-build-proxy kimi auth
  logout` before deleting a dedicated credential directory.

## Network exposure

Keep the default loopback binding whenever possible. Do not expose this proxy
directly to the public internet. For access from another device, use a trusted
private network plus an inbound bearer token or place the proxy behind an
authenticating reverse proxy.

## Reporting a vulnerability

Use a private GitHub security advisory for this repository instead of opening a
public issue. Do not include live tokens, authentication files, or unredacted
request data in a report.
