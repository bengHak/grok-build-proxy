# Security policy

`grok-build-proxy` handles ChatGPT/Codex access and refresh tokens on macOS.
Treat the Codex authentication file as a password.

## Safe defaults

- The server binds to `127.0.0.1` by default.
- Binding to a non-loopback address is rejected unless an inbound bearer token
  is configured with `GROK_BUILD_PROXY_TOKEN` or `--client-token`.
- Request bodies, response bodies, and authorization values are not logged.
- The installer writes only the executable into the selected installation
  directory and does not read or copy Codex credentials.

## Credential handling

- Do not commit, upload, back up to a public location, or share `auth.json`.
- Use a dedicated `CODEX_HOME` configured with
  `cli_auth_credentials_store = "file"`.
- Keep the authentication file readable only by your macOS user.
- Do not run multiple processes that refresh the same Codex credential file.
- Log out with the Codex CLI before deleting a dedicated credential directory.

## Network exposure

Keep the default loopback binding whenever possible. Do not expose this proxy
directly to the public internet. For access from another device, use a trusted
private network plus an inbound bearer token or place the proxy behind an
authenticating reverse proxy.

## Reporting a vulnerability

Use a private GitHub security advisory for this repository instead of opening a
public issue. Do not include live tokens, authentication files, or unredacted
request data in a report.
