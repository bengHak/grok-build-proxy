# grok-build-proxy

A lightweight, macOS-only local proxy that lets **Grok Build** use Codex models
available through your ChatGPT account. It accepts Grok Build's native OpenAI
Responses API requests, adds Codex authentication, applies the small request
shape differences required by Responses Lite models, and streams the response
back without an Anthropic/Claude translation layer.

> [!WARNING]
> This is an unofficial community project. It is not affiliated with or
> endorsed by OpenAI, ChatGPT, Codex, xAI, or Grok. Model access depends on your
> ChatGPT plan and workspace policy. The private ChatGPT Codex backend can change
> without notice and may require proxy updates.

## Table of contents

- [Requirements](#requirements)
- [Install with curl](#install-with-curl)
- [Authenticate Codex](#authenticate-codex)
- [Start the proxy](#start-the-proxy)
- [Configure Grok Build](#configure-grok-build)
- [Supported models](#supported-models)
- [How it works](#how-it-works)
- [Configuration](#configuration)
- [Security](#security)
- [Development](#development)
- [Release process](#release-process)
- [Limitations](#limitations)

## Requirements

- macOS on Apple Silicon (`arm64`) or Intel (`x86_64`)
- The official Codex CLI
- A ChatGPT account that is allowed to use the selected Codex model
- Grok Build

The installer intentionally rejects Linux and Windows. Release artifacts are
built only for macOS.

## Install with curl

Install the latest release into `$HOME/.local/bin`:

```sh
curl -fsSL https://raw.githubusercontent.com/bengHak/grok-build-proxy/main/install.sh | sh
```

Make sure that directory is on your `PATH`. The default macOS shell is zsh:

```sh
echo 'export PATH="$HOME/.local/bin:$PATH"' >> "$HOME/.zshrc"
exec zsh
```

Verify the installation:

```sh
grok-build-proxy --version
```

The installer downloads the architecture-specific release archive and verifies
its SHA-256 checksum. Before the first tagged release exists, it downloads the
source from `main` and builds it locally when Go 1.23 or newer is available.

Install a specific release or choose another directory:

```sh
curl -fsSL https://raw.githubusercontent.com/bengHak/grok-build-proxy/main/install.sh \
  | sh -s -- --version v0.1.0 --install-dir "$HOME/bin"
```

Equivalent environment variables are also supported:

```sh
curl -fsSL https://raw.githubusercontent.com/bengHak/grok-build-proxy/main/install.sh \
  | GROK_BUILD_PROXY_VERSION=v0.1.0 \
    GROK_BUILD_PROXY_INSTALL_DIR="$HOME/bin" \
    sh
```

Review [`install.sh`](install.sh) before piping it to a shell when required by
your security policy.

## Authenticate Codex

Use a dedicated `CODEX_HOME` so the proxy and another Codex process do not try
to rotate the same refresh token at the same time.

```sh
export CODEX_HOME="$HOME/.codex-grok-build-proxy"
mkdir -p "$CODEX_HOME"
cat > "$CODEX_HOME/config.toml" <<'EOF'
cli_auth_credentials_store = "file"
EOF
CODEX_HOME="$CODEX_HOME" codex login
```

For a headless Mac, use the official device-code flow:

```sh
CODEX_HOME="$HOME/.codex-grok-build-proxy" codex login --device-auth
```

After login, `$CODEX_HOME/auth.json` must exist. It contains access and refresh
tokens and must be protected like a password.

## Start the proxy

```sh
CODEX_HOME="$HOME/.codex-grok-build-proxy" grok-build-proxy
```

The default address is `http://127.0.0.1:18765`. Check readiness with:

```sh
curl --fail http://127.0.0.1:18765/readyz
```

The proxy exposes these endpoints:

| Endpoint | Purpose |
|---|---|
| `POST /v1/responses` | Proxies a Codex Responses request |
| `GET /v1/models` | Returns the model catalog for Grok Build |
| `GET /healthz` | Reports process health |
| `GET /readyz` | Verifies that Codex credentials can be loaded |

`/responses` and `/models` are compatibility aliases.

## Configure Grok Build

Copy the model blocks you need from
[`examples/grok-config.toml`](examples/grok-config.toml) into
`~/.grok/config.toml`. A minimal example is:

```toml
[model.codex-terra]
model = "gpt-5.6-terra"
name = "Codex GPT-5.6 Terra"
base_url = "http://127.0.0.1:18765/v1"
api_backend = "responses"
api_key = "unused"
context_window = 372000
```

`api_key = "unused"` prevents Grok Build from reusing an xAI session token for
this local endpoint. The proxy ignores the incoming Authorization value while
bound to loopback and loads the real Codex credentials from the Codex CLI auth
file.

Start Grok Build with the custom model:

```sh
grok -m codex-terra
```

You can also generate configuration blocks from the proxy's current catalog:

```sh
grok-build-proxy --print-grok-config
```

## Supported models

The built-in catalog currently exposes:

| Model | Context window | Upstream request shape |
|---|---:|---|
| `gpt-5.6-sol` | 372,000 | Responses Lite |
| `gpt-5.6-terra` | 372,000 | Responses Lite |
| `gpt-5.6-luna` | 372,000 | Responses Lite |
| `gpt-5.5` | 272,000 | Responses |
| `gpt-5.2` | 272,000 | Responses |

A catalog entry does not grant model access. Availability can differ by plan,
workspace, region, and server-side rollout.

Append `-fast` to a model ID to have the proxy remove the suffix and set
`service_tier = "priority"`:

```toml
[model.codex-sol-fast]
model = "gpt-5.6-sol-fast"
name = "Codex GPT-5.6 Sol (Fast)"
base_url = "http://127.0.0.1:18765/v1"
api_backend = "responses"
api_key = "unused"
context_window = 372000
```

Fast-tier availability and usage effects are account-dependent.

Override the advertised catalog with a comma-separated list:

```sh
GROK_BUILD_PROXY_MODELS="gpt-5.6-sol,gpt-5.6-terra" \
  grok-build-proxy
```

Unknown model IDs are passed through so newly enabled account-specific models
can be tested before the catalog is updated.

## How it works

```text
Grok Build
  POST /v1/responses
          |
          v
  grok-build-proxy
  - loads Codex CLI auth.json
  - refreshes OAuth tokens before expiry
  - adds ChatGPT-Account-ID and Codex headers
  - adapts GPT-5.6 requests to Responses Lite
  - forwards SSE bytes as they arrive
          |
          v
  ChatGPT Codex Responses backend
```

For Responses Lite models, the proxy performs these request transformations:

- moves top-level `tools` into an `additional_tools` developer input item;
- moves `instructions` into a developer message;
- sets `reasoning.context = "all_turns"`;
- sets `parallel_tool_calls = false`;
- adds the Responses Lite header and client metadata;
- streams returned Responses SSE events to Grok Build unchanged.

Normal Responses models retain their request structure and receive only the
required authentication and routing headers.

## Configuration

| Flag | Environment variable | Default |
|---|---|---|
| `--listen` | `GROK_BUILD_PROXY_LISTEN` | `127.0.0.1:18765` |
| `--auth-file` | `GROK_BUILD_PROXY_AUTH_FILE` | `$CODEX_HOME/auth.json` or `~/.codex/auth.json` |
| `--upstream` | `GROK_BUILD_PROXY_UPSTREAM` | ChatGPT Codex Responses endpoint |
| `--refresh-url` | `GROK_BUILD_PROXY_REFRESH_URL` | OpenAI OAuth token endpoint |
| `--models` | `GROK_BUILD_PROXY_MODELS` | Built-in catalog |
| `--client-token` | `GROK_BUILD_PROXY_TOKEN` | Empty |
| `--log-format` | `GROK_BUILD_PROXY_LOG_FORMAT` | `text` |

### Non-loopback binding

A bearer token is mandatory when binding to a LAN or all-interface address:

```sh
export GROK_BUILD_PROXY_TOKEN="replace-with-a-long-random-value"
grok-build-proxy --listen 0.0.0.0:18765
```

Set the same value as `api_key` in the Grok Build model configuration. Do not
expose this proxy directly to the public internet.

## Security

- Keep the default loopback binding whenever possible.
- Never commit `auth.json` or copy it into logs, issues, chat messages, or build
  artifacts.
- The proxy does not log request bodies, response bodies, or Authorization
  headers.
- Use a dedicated `CODEX_HOME` to reduce refresh-token races.
- Prefer an official OpenAI API key for unattended production automation where
  the ChatGPT subscription path is not appropriate.
- See [`SECURITY.md`](SECURITY.md) for vulnerability reporting guidance.

## Development

Development and CI target macOS only. Go 1.23 or newer is required.

```sh
git clone https://github.com/bengHak/grok-build-proxy.git
cd grok-build-proxy
make check
```

Individual commands:

```sh
test -z "$(gofmt -l .)"
go vet ./...
go test -race ./...
go build ./cmd/grok-build-proxy
sh -n install.sh
```

Build both supported macOS archives locally:

```sh
make dist
```

## Release process

Pushing a semantic-version tag such as `v0.1.0` runs the release workflow. It
builds and publishes these assets:

```text
grok-build-proxy_Darwin_arm64.tar.gz
grok-build-proxy_Darwin_amd64.tar.gz
checksums.txt
```

The curl installer selects the correct archive using `uname -m`.

## Limitations

- macOS is the only supported operating system.
- The proxy cannot read credentials stored only in the macOS Keychain. Configure
  the dedicated Codex home with `cli_auth_credentials_store = "file"`.
- The ChatGPT Codex backend is separate from the public OpenAI Platform API and
  can change server-side.
- The current transport uses HTTP Responses/SSE, not Codex WebSocket transport.
- The proxy targets function tools executed locally by Grok Build. Compatibility
  with Codex-hosted search tools is not guaranteed.

## References

- [OpenAI Codex authentication](https://developers.openai.com/codex/auth)
- [OpenAI Codex repository](https://github.com/openai/codex)
- [xAI Grok Build repository](https://github.com/xai-org/grok-build)
- [raine/claude-code-proxy](https://github.com/raine/claude-code-proxy)

## License

MIT. See [`LICENSE`](LICENSE).
