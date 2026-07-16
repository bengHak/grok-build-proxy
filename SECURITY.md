# Security

`grok-build-proxy` handles ChatGPT/Codex access and refresh tokens. Treat the
Codex auth file as a password.

- The server binds to `127.0.0.1` by default.
- Binding to a non-loopback address is rejected unless an inbound bearer token
  is configured with `GROK_BUILD_PROXY_TOKEN` or `--client-token`.
- Request and response bodies are never logged.
- Do not commit `auth.json`, copy it into an image, or expose the proxy directly
  to the public internet.
- Prefer a dedicated `CODEX_HOME` so another Codex process does not refresh the
  same token concurrently.

To report a vulnerability, open a private GitHub security advisory for this
repository rather than a public issue.
