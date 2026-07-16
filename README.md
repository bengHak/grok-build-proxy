# grok-build-proxy

[![CI](https://github.com/bengHak/grok-build-proxy/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/bengHak/grok-build-proxy/actions/workflows/ci.yml)

A lightweight, macOS-only local proxy that lets **Grok Build** use Codex models
available through your ChatGPT account.

Grok Build remains the agent harness and continues to own prompts, tools,
permissions, Plan mode, Goal mode, subagents, context, and session state. The
proxy handles Codex authentication, optional model-ID substitution, Responses
API adaptation, and streaming.

> [!WARNING]
> This is an unofficial community project. It is not affiliated with or endorsed
> by OpenAI, ChatGPT, Codex, xAI, or Grok. It uses the ChatGPT Codex backend,
> which is separate from the public OpenAI Platform API and may change without
> notice. Model availability depends on your ChatGPT plan and workspace policy.

## Table of contents

- [Quick start](#quick-start)
- [Requirements](#requirements)
- [Install](#install)
- [Authenticate with the official Codex CLI](#authenticate-with-the-official-codex-cli)
- [Configure Grok Build](#configure-grok-build)
- [Run and verify](#run-and-verify)
- [Model substitutions](#model-substitutions)
- [Supported Codex models](#supported-codex-models)
- [How it works](#how-it-works)
- [Commands](#commands)
- [Configuration reference](#configuration-reference)
- [Troubleshooting](#troubleshooting)
- [Security](#security)
- [Update and uninstall](#update-and-uninstall)
- [Development and releases](#development-and-releases)
- [Limitations](#limitations)

## Quick start

Start with a separate Codex model name inside Grok Build. This is the simplest
way to verify authentication and transport before adding model substitutions.

1. Install the proxy:

   ```sh
   curl -fsSL https://raw.githubusercontent.com/bengHak/grok-build-proxy/main/install.sh | sh
   ```

2. Add the default install directory to `PATH` when necessary:

   ```sh
   echo 'export PATH="$HOME/.local/bin:$PATH"' >> "$HOME/.zshrc"
   exec zsh
   ```

3. Sign in through the official Codex CLI:

   ```sh
   grok-build-proxy auth login
   ```

4. Add this block to `~/.grok/config.toml`:

   ```toml
   [model.codex-terra]
   model = "gpt-5.6-terra"
   name = "Codex GPT-5.6 Terra"
   base_url = "http://127.0.0.1:18765/v1"
   api_backend = "responses"
   api_key = "unused"
   context_window = 372000
   ```

5. Check the setup:

   ```sh
   grok-build-proxy doctor
   ```

6. Start the proxy in one terminal and Grok Build in another:

   ```sh
   # Terminal 1
   grok-build-proxy
   ```

   ```sh
   # Terminal 2
   grok -m codex-terra
   ```

A stopped proxy is a doctor warning when the port is available. Fix every
`FAIL` result before starting Grok Build.

## Requirements

- macOS on Apple Silicon (`arm64`) or Intel (`x86_64`)
- The official Codex CLI
- A ChatGPT account allowed to use the selected Codex model
- Grok Build

The installer intentionally rejects Linux and Windows. Release artifacts are
built only for macOS.

## Install

The installer prefers a matching binary from the latest GitHub release. If no
release or matching asset is available, it builds the repository source and
requires Go 1.23 or newer.

### Latest release when available

```sh
curl -fsSL https://raw.githubusercontent.com/bengHak/grok-build-proxy/main/install.sh | sh
```

The default destination is `$HOME/.local/bin/grok-build-proxy`.

### Current `main` source

Use this when a required feature has not reached a tagged release:

```sh
curl -fsSL https://raw.githubusercontent.com/bengHak/grok-build-proxy/main/install.sh \
  | sh -s -- --from-source
```

### Specific release or source ref

```sh
curl -fsSL https://raw.githubusercontent.com/bengHak/grok-build-proxy/main/install.sh \
  | sh -s -- --version v0.1.0 --install-dir "$HOME/bin"
```

If the release asset is unavailable, `--version` is used as the source tag or
branch fallback. Equivalent environment variables are supported:

```sh
curl -fsSL https://raw.githubusercontent.com/bengHak/grok-build-proxy/main/install.sh \
  | GROK_BUILD_PROXY_VERSION=v0.1.0 \
    GROK_BUILD_PROXY_INSTALL_DIR="$HOME/bin" \
    sh
```

Release archives are verified against the published SHA-256 manifest. To review
the installer before running it:

```sh
curl -fsSL https://raw.githubusercontent.com/bengHak/grok-build-proxy/main/install.sh \
  -o /tmp/grok-build-proxy-install.sh
less /tmp/grok-build-proxy-install.sh
sh /tmp/grok-build-proxy-install.sh
```

Verify the result:

```sh
grok-build-proxy --version
```

## Authenticate with the official Codex CLI

The proxy does **not** implement or imitate OpenAI's OAuth login UI. Its auth
commands prepare a dedicated, file-backed `CODEX_HOME` and execute the official
Codex CLI.

```sh
# Browser login
grok-build-proxy auth login

# Device-code login for a headless Mac
grok-build-proxy auth device

# Inspect or clear the login
grok-build-proxy auth status
grok-build-proxy auth logout
```

Device-code authentication is currently marked beta by OpenAI and may need to
be enabled in personal security settings or workspace permissions.

The default credential directory is:

```text
~/.codex-grok-build-proxy
```

The wrapper preserves unrelated Codex settings while ensuring:

```toml
cli_auth_credentials_store = "file"
forced_login_method = "chatgpt"
```

The resulting `auth.json` contains access and refresh tokens. Treat it like a
password. Credential-directory precedence is:

1. `GROK_BUILD_PROXY_CODEX_HOME`
2. `CODEX_HOME`
3. `~/.codex-grok-build-proxy`

Use a custom dedicated directory with:

```sh
grok-build-proxy auth login --codex-home "$HOME/.my-codex-proxy"
```

A dedicated directory reduces refresh-token races with ordinary Codex CLI
sessions.

## Configure Grok Build

There are two routing modes. They can coexist, but test the direct mode first.

### Direct Codex model names

Create a new Grok model entry whose `model` value is already a Codex model ID:

```toml
[model.codex-sol]
model = "gpt-5.6-sol"
name = "Codex GPT-5.6 Sol"
base_url = "http://127.0.0.1:18765/v1"
api_backend = "responses"
api_key = "unused"
context_window = 372000
```

Run it with:

```sh
grok -m codex-sol
```

No model map is needed in this mode. Additional examples are available in
[`examples/grok-config.toml`](examples/grok-config.toml).

### Preserve built-in Grok model IDs

Model substitution preserves source IDs such as `grok-build`,
`grok-composer-2.5`, or `grok-4.5` while sending a selected Codex target
upstream.

This mode requires both:

1. a model map when the proxy starts; and
2. Grok model blocks that point the source IDs to the local proxy.

Generate matching blocks with the same map used by the proxy:

```sh
GROK_BUILD_PROXY_MODEL_MAP='grok-build=gpt-5.6-terra,grok-4.5=gpt-5.6-sol' \
  grok-build-proxy --print-grok-config \
  > /tmp/grok-build-proxy-models.toml

cat /tmp/grok-build-proxy-models.toml
```

Review and merge the relevant blocks into `~/.grok/config.toml`. Do not blindly
append the output if the file already defines the same table names.

A generated override looks like this:

```toml
# Proxy mapping: grok-4.5 -> gpt-5.6-sol
[model."grok-4.5"]
model = "grok-4.5"
name = "Grok 4.5 via Codex GPT-5.6 Sol"
description = "Routes grok-4.5 to gpt-5.6-sol through grok-build-proxy"
base_url = "http://127.0.0.1:18765/v1"
api_backend = "responses"
api_key = "unused"
context_window = 372000
```

> [!IMPORTANT]
> Keep the source ID in the `model` field. For example, a `grok-4.5` mapping
> requires `model = "grok-4.5"`. Setting it directly to `gpt-5.6-sol` switches
> to direct-model routing and bypasses that source mapping.

The generated config does not persist the proxy's model map. Start the proxy
with the same `GROK_BUILD_PROXY_MODEL_MAP` or `--model-map` value every time.

`api_key = "unused"` prevents Grok Build from attaching an xAI session token to
the loopback endpoint. When using a protected non-loopback endpoint, replace it
with the configured proxy bearer token.

## Run and verify

Start the proxy in the foreground while configuring it:

```sh
grok-build-proxy
```

The explicit form is equivalent:

```sh
grok-build-proxy serve
```

The default address is `http://127.0.0.1:18765`.

### Health, readiness, and routes

```sh
curl -fsS http://127.0.0.1:18765/healthz | python3 -m json.tool
curl -fsS http://127.0.0.1:18765/readyz | python3 -m json.tool
curl -fsS http://127.0.0.1:18765/v1/models | python3 -m json.tool
```

- `healthz` confirms the process is running and reports the map count.
- `readyz` verifies that Codex credentials can be loaded or refreshed.
- `v1/models` lists canonical and mapped routes. Mapped entries include
  `target_model`; fast routes include `service_tier: "priority"`.

Run `grok-build-proxy doctor` before startup to validate files and port
availability, or from another terminal after startup to validate the live
endpoints too. It checks the platform, CLIs, authentication, file permissions,
Grok config, model-map syntax, port state, health, and readiness without printing
token values.

### HTTP endpoints

| Endpoint | Authentication | Purpose |
|---|---|---|
| `POST /v1/responses` | Proxy token when configured | Proxy a Codex Responses request |
| `GET /v1/models` | Proxy token when configured | Return canonical and mapped routes |
| `GET /healthz` | None | Report process health and map count |
| `GET /readyz` | Proxy token when configured | Verify Codex credentials |

`/responses` and `/models` are compatibility aliases.

## Model substitutions

Substitution is disabled by default. Configure comma-separated `source=target`
pairs:

```sh
export GROK_BUILD_PROXY_MODEL_MAP='grok-build=gpt-5.6-terra,grok-composer-2.5=gpt-5.6-terra,grok-4.5=gpt-5.6-sol'
grok-build-proxy
```

The equivalent flag is:

```sh
grok-build-proxy \
  --model-map 'grok-build=gpt-5.6-terra,grok-4.5=gpt-5.6-sol'
```

The source is the exact `model` value Grok Build sends, not merely the display
name. Use `grok models` and inspect `~/.grok/config.toml` to confirm IDs.

### Plan mode, Goal mode, and subagents

The map applies to every `POST /v1/responses` request that reaches this proxy:

- a mapped parent session uses the selected Codex target;
- `/plan` continues to use that mapped parent model;
- ordinary subagents using a mapped source use its target;
- Goal planner, verifier, strategist, and summarizer requests are mapped when
  their resolved source is present in the map.

Grok Build still controls the agent definition, system prompt, tools,
permissions, capability mode, context, and subagent lifecycle. A Goal role
explicitly routed to an unmapped source is unchanged, and any model entry still
pointing to an xAI endpoint bypasses this proxy completely.

### Resolution rules

Mappings may chain:

```sh
GROK_BUILD_PROXY_MODEL_MAP='composer=grok-build,grok-build=gpt-5.6-terra' \
  grok-build-proxy
```

A `-fast` suffix on the requested source or any target selects the final base
model with `service_tier = "priority"`:

```sh
GROK_BUILD_PROXY_MODEL_MAP='grok-4.5=gpt-5.6-sol-fast' \
  grok-build-proxy
```

The parser accepts comma-, semicolon-, or newline-separated pairs. Duplicate
sources, empty IDs, whitespace inside IDs, self-maps, and cycles are rejected
before the server starts.

`GET /v1/models` advertises mapped source IDs and their resolved targets.
`grok-build-proxy doctor` validates the map supplied through the environment or
`doctor --model-map`.

## Supported Codex models

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

Append `-fast` to a supported model ID to remove the suffix upstream and set
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

Override the advertised canonical catalog with:

```sh
GROK_BUILD_PROXY_MODELS='gpt-5.6-sol,gpt-5.6-terra' \
  grok-build-proxy
```

Unknown non-empty IDs pass through so account-specific models can be tested
before a proxy release adds them. The proxy infers Responses Lite for unknown
`gpt-5.6-*` targets and uses regular Responses for other unknown targets.

## How it works

```text
Grok Build
  POST /v1/responses
          |
          v
  grok-build-proxy
  - reads the official Codex CLI auth.json
  - refreshes OAuth tokens before expiry
  - resolves an optional Grok-to-Codex model map
  - adds ChatGPT-Account-ID and Codex headers
  - adapts Responses Lite requests when required
  - forwards SSE bytes as they arrive
          |
          v
  ChatGPT Codex Responses backend
```

For Responses Lite models, the proxy:

- moves top-level `tools` into an `additional_tools` developer input item;
- moves `instructions` into a developer message;
- sets `reasoning.context = "all_turns"`;
- sets `parallel_tool_calls = false`;
- adds the Responses Lite header and client metadata;
- streams returned Responses SSE events unchanged.

Regular Responses models retain their request structure and receive only the
required authentication and routing metadata. On an upstream `401`, the proxy
forces one token refresh and retries once.

## Commands

| Command | Purpose |
|---|---|
| `grok-build-proxy` | Start the proxy |
| `grok-build-proxy serve` | Start the proxy explicitly |
| `grok-build-proxy auth login` | Run official browser-based Codex login |
| `grok-build-proxy auth device` | Run official Codex device-code login |
| `grok-build-proxy auth status` | Show login status and a redacted credential summary |
| `grok-build-proxy auth logout` | Run official Codex logout |
| `grok-build-proxy doctor` | Diagnose the local setup |
| `grok-build-proxy --print-grok-config` | Print Grok model blocks |
| `grok-build-proxy version` | Print the installed version |
| `grok-build-proxy --help` | Show command help |

Command-specific help:

```sh
grok-build-proxy serve --help
grok-build-proxy auth login --help
grok-build-proxy doctor --help
```

## Configuration reference

### Proxy server

| Flag | Environment variable | Default |
|---|---|---|
| `--listen` | `GROK_BUILD_PROXY_LISTEN` | `127.0.0.1:18765` |
| `--auth-file` | `GROK_BUILD_PROXY_AUTH_FILE` | Resolved Codex home plus `/auth.json` |
| `--upstream` | `GROK_BUILD_PROXY_UPSTREAM` | ChatGPT Codex Responses endpoint |
| `--refresh-url` | `GROK_BUILD_PROXY_REFRESH_URL` | OpenAI OAuth token endpoint |
| `--models` | `GROK_BUILD_PROXY_MODELS` | Built-in catalog |
| `--model-map` | `GROK_BUILD_PROXY_MODEL_MAP` | Empty; IDs pass through |
| `--client-token` | `GROK_BUILD_PROXY_TOKEN` | Empty |
| `--log-format` | `GROK_BUILD_PROXY_LOG_FORMAT` | `text` |

### Auth and doctor

| Flag | Environment variable | Default |
|---|---|---|
| `auth <action> --codex-home` | `GROK_BUILD_PROXY_CODEX_HOME`, then `CODEX_HOME` | `~/.codex-grok-build-proxy` |
| `auth <action> --codex-binary` | `GROK_BUILD_PROXY_CODEX_BINARY` | `codex` |
| `doctor --auth-file` | `GROK_BUILD_PROXY_AUTH_FILE` | Resolved Codex home plus `/auth.json` |
| `doctor --codex-binary` | `GROK_BUILD_PROXY_CODEX_BINARY` | `codex` |
| `doctor --grok-binary` | `GROK_BUILD_PROXY_GROK_BINARY` | `grok` |
| `doctor --grok-config` | `GROK_BUILD_PROXY_GROK_CONFIG` | `~/.grok/config.toml` |
| `doctor --model-map` | `GROK_BUILD_PROXY_MODEL_MAP` | Empty |
| `doctor --client-token` | `GROK_BUILD_PROXY_TOKEN` | Empty |
| `doctor --timeout` | None | `5s` |

### Non-loopback binding

A bearer token is mandatory when binding to a LAN or all-interface address:

```sh
export GROK_BUILD_PROXY_TOKEN='replace-with-a-long-random-value'
grok-build-proxy --listen 0.0.0.0:18765
```

Set the same value as `api_key` in each Grok model block. Do not expose this
proxy directly to the public internet.

## Troubleshooting

| Symptom | What to check |
|---|---|
| Installer asks for Go | No matching release asset was found. Install Go 1.23+, select a tagged release, or use `--from-source` intentionally. |
| `codex` is not found | Install the official Codex CLI and confirm `codex --version` works. |
| `auth.json` is missing | Run `grok-build-proxy auth login`; the wrapper configures file-backed storage in its dedicated Codex home. |
| Device-code login is unavailable | Enable it in ChatGPT security/workspace settings or use `grok-build-proxy auth login`. |
| `readyz` or upstream returns `401` | Run `grok-build-proxy auth status`, then log in again if the session was revoked. |
| Upstream rejects a model | The target is not enabled for the current account or workspace. Select another target. |
| A mapping has no effect | Confirm the proxy started with the map, the Grok model points to this proxy, and its `model` value exactly matches the source. |
| Plan or Goal uses another model | Check Goal-role or subagent overrides; only mapped source IDs routed through this endpoint are changed. |
| Port `18765` is occupied | Run `lsof -nP -iTCP:18765 -sTCP:LISTEN`, stop the process, or change both `--listen` and Grok `base_url`. |
| Grok still contacts xAI | The selected model entry is not overridden to use the local proxy, or a different model entry is active. |

Inspect live routing with:

```sh
curl -fsS http://127.0.0.1:18765/v1/models | python3 -m json.tool
```

Proxy logs include `requested_model`, final `model`, and `mapped` fields without
logging prompts or credentials.

## Security

- Keep the default loopback binding whenever possible.
- Never commit or share `auth.json`.
- The proxy does not log request bodies, response bodies, or Authorization
  headers.
- Auth commands execute the official Codex CLI instead of collecting passwords
  or browser credentials.
- Use the dedicated Codex home to reduce refresh-token races.
- Treat model maps as routing configuration, not as a security boundary.
- Prefer an official OpenAI API key for unattended production automation when
  the ChatGPT subscription path is not appropriate.
- See [`SECURITY.md`](SECURITY.md) for vulnerability reporting guidance.

## Update and uninstall

Update to the latest release by rerunning the installer, or install current
`main` with `--from-source`:

```sh
curl -fsSL https://raw.githubusercontent.com/bengHak/grok-build-proxy/main/install.sh | sh

curl -fsSL https://raw.githubusercontent.com/bengHak/grok-build-proxy/main/install.sh \
  | sh -s -- --from-source
```

Uninstall the default setup:

```sh
grok-build-proxy auth logout
rm -f "$HOME/.local/bin/grok-build-proxy"
rm -rf "$HOME/.codex-grok-build-proxy"
```

Adjust the binary path for a custom install directory. Remove related entries
from `~/.grok/config.toml` and any model-map export from your shell config.

## Development and releases

Development and CI target macOS only. Go 1.23 or newer is required.

```sh
git clone https://github.com/bengHak/grok-build-proxy.git
cd grok-build-proxy
make check
```

Individual checks:

```sh
test -z "$(gofmt -l .)"
go vet ./...
go test -race ./...
go build ./cmd/grok-build-proxy
sh -n install.sh
```

Build both macOS archives:

```sh
make dist
```

Pushing a semantic-version tag such as `v0.1.0` runs the release workflow and
publishes:

```text
grok-build-proxy_Darwin_arm64.tar.gz
grok-build-proxy_Darwin_amd64.tar.gz
checksums.txt
```

## Limitations

- macOS is the only supported operating system.
- The official Codex CLI is required for login, status, device-code login, and
  logout.
- The proxy cannot read credentials stored only in the macOS Keychain. Its auth
  wrapper configures a dedicated file-backed Codex home.
- The ChatGPT Codex backend can change server-side.
- Model substitution affects only traffic routed through this proxy.
- The current transport uses HTTP Responses/SSE, not Codex WebSocket transport.
- Compatibility with Codex-hosted search tools is not guaranteed.
- A local catalog entry does not prove account entitlement to that model.

## References

- [OpenAI Codex authentication](https://learn.chatgpt.com/docs/auth)
- [OpenAI Codex repository](https://github.com/openai/codex)
- [xAI Grok Build repository](https://github.com/xai-org/grok-build)
- [raine/claude-code-proxy](https://github.com/raine/claude-code-proxy)

## License

MIT. See [`LICENSE`](LICENSE).
