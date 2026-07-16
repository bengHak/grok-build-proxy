# grok-build-proxy

[![CI](https://github.com/bengHak/grok-build-proxy/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/bengHak/grok-build-proxy/actions/workflows/ci.yml)

A macOS-only local proxy that lets Grok Build use Codex models available through
your ChatGPT account. Grok Build remains the agent harness and owns prompts,
tools, permissions, Plan mode, Goal mode, subagents, context, and session state.

> [!WARNING]
> This is an unofficial community project and is not affiliated with OpenAI,
> ChatGPT, Codex, xAI, or Grok. The private ChatGPT Codex backend can change
> without notice. Model access depends on your plan and workspace policy.

## Quick start

Requirements: macOS, the official `codex` CLI, Grok Build, and a ChatGPT account
that can use the selected Codex model.

1. Install the latest release:

   ```sh
   curl -fsSL https://raw.githubusercontent.com/bengHak/grok-build-proxy/main/install.sh | sh
   ```

2. Add the default install directory to `PATH` when necessary:

   ```sh
   echo 'export PATH="$HOME/.local/bin:$PATH"' >> "$HOME/.zshrc"
   exec zsh
   ```

3. Sign in through the official Codex CLI wrapper:

   ```sh
   grok-build-proxy auth login
   ```

4. Add this configuration to `~/.grok/config.toml`:

   ```toml
   [models]
   default_reasoning_effort = "xhigh"

   [model.codex-sol]
   model = "gpt-5.6-sol"
   name = "Codex GPT-5.6 Sol"
   base_url = "http://127.0.0.1:18765/v1"
   api_backend = "responses"
   api_key = "unused"
   context_window = 372000
   ```

   When `[models]` already exists, add only the
   `default_reasoning_effort = "xhigh"` key to that table. TOML does not allow
   the same table to be declared twice.

5. Validate the local setup:

   ```sh
   grok-build-proxy doctor
   ```

6. Start the proxy and Grok Build in separate terminals:

   ```sh
   grok-build-proxy
   ```

   ```sh
   grok -m codex-sol
   ```

## Why v0.0.2 is required for GPT-5.6

Version `0.0.1` used an outdated Responses Lite HTTP shape and could receive a
400 response after a prompt was submitted. Version `0.0.2` aligns requests with
the current official Codex HTTP contract:

- sends `session-id`, `thread-id`, `x-session-affinity`, and the Codex
  compatibility `version` header;
- removes the obsolete fixed `OpenAI-Beta: responses=experimental` header;
- removes WebSocket-only metadata from HTTP requests;
- creates the canonical `additional_tools` input item, even when the tool list
  is empty;
- sets `tool_choice = "auto"`, `parallel_tool_calls = false`,
  `reasoning.context = "all_turns"`, and a stable `prompt_cache_key`;
- requests `reasoning.encrypted_content`, removes unsupported provider fields,
  image detail, and nonpersistent input IDs;
- logs a bounded, redacted upstream error summary while returning the original
  error body to Grok Build.

The compatibility version defaults to `0.144.0`. Override it only when testing a
newer official Codex contract:

```sh
GROK_BUILD_PROXY_CODEX_COMPAT_VERSION=0.144.0 grok-build-proxy
```

## Authentication and diagnostics

```sh
grok-build-proxy auth login
grok-build-proxy auth device
grok-build-proxy auth status
grok-build-proxy auth logout
grok-build-proxy doctor
```

The default dedicated Codex home is `~/.codex-grok-build-proxy`. The wrapper
uses the official Codex CLI and configures file-backed credentials; it does not
implement its own OAuth login UI.

Useful health checks:

```sh
curl --fail http://127.0.0.1:18765/healthz
curl --fail http://127.0.0.1:18765/readyz
curl -fsS http://127.0.0.1:18765/v1/models | python3 -m json.tool
```

## Model substitutions

The proxy can preserve Grok-facing IDs while selecting Codex targets. The map
applies to every `/v1/responses` request that reaches the proxy, including parent
sessions, `/plan`, inherited subagents, and Goal planner/verifier/strategist/
summarizer requests whose resolved source ID is mapped.

```sh
export GROK_BUILD_PROXY_MODEL_MAP='grok-build=gpt-5.6-terra,grok-4.5=gpt-5.6-sol'
grok-build-proxy --print-grok-config > /tmp/grok-build-proxy-models.toml
```

Review and merge the generated blocks into `~/.grok/config.toml`, then start the
proxy with the same environment variable. The source must be the exact model ID
shown by `grok models`, not only its display name.

Mappings can chain. A `-fast` suffix on a source or target selects the final base
model with `service_tier = "priority"`. Duplicate sources, self-maps, and cycles
are rejected before the server starts.

## Configuration

| Flag | Environment variable | Default |
|---|---|---|
| `--listen` | `GROK_BUILD_PROXY_LISTEN` | `127.0.0.1:18765` |
| `--auth-file` | `GROK_BUILD_PROXY_AUTH_FILE` | dedicated Codex home `auth.json` |
| `--upstream` | `GROK_BUILD_PROXY_UPSTREAM` | ChatGPT Codex Responses endpoint |
| `--models` | `GROK_BUILD_PROXY_MODELS` | built-in catalog |
| `--model-map` | `GROK_BUILD_PROXY_MODEL_MAP` | empty |
| `--codex-compat-version` | `GROK_BUILD_PROXY_CODEX_COMPAT_VERSION` | `0.144.0` |
| `--client-token` | `GROK_BUILD_PROXY_TOKEN` | empty |
| `--log-format` | `GROK_BUILD_PROXY_LOG_FORMAT` | `text` |

A bearer token is mandatory when binding to a non-loopback address. Keep the
default loopback binding whenever possible.

## Troubleshooting

- `command not found`: ensure `$HOME/.local/bin` is in `PATH`.
- `auth.json` missing: run `grok-build-proxy auth login`.
- 400 with a GPT-5.6 model: confirm `grok-build-proxy --version` reports
  `0.0.2` or newer and inspect the new `upstream_error` log field.
- 401: run `grok-build-proxy auth status`, then log in again if required.
- Mapping has no effect: confirm the selected Grok entry points to this local
  endpoint and its `model` value exactly matches the map source.
- Port 18765 occupied: run `lsof -nP -iTCP:18765 -sTCP:LISTEN` or change both
  `--listen` and the Grok `base_url`.

## Development and release

```sh
git clone https://github.com/bengHak/grok-build-proxy.git
cd grok-build-proxy
gofmt -w $(find . -name '*.go' -type f)
go vet ./...
go test -race ./...
make dist VERSION=0.0.2
```

Release assets are built for macOS arm64 and amd64 and published with a SHA-256
manifest. See [`SECURITY.md`](SECURITY.md) for credential and vulnerability
reporting guidance. Licensed under MIT.
