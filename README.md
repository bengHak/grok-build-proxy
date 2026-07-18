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
   supports_reasoning_effort = true
   reasoning_efforts = ["low", "medium", "high", "xhigh"]
   ```

   When `[models]` already exists, add only the
   `default_reasoning_effort = "xhigh"` key to that table. TOML does not allow
   the same table to be declared twice. The shown `api_key = "unused"` assumes
   client-token authentication is disabled. If it is enabled, set `api_key` in
   each proxy-backed model entry to the configured local proxy token so Grok
   sends it as the bearer credential.

5. Validate the local setup:

   ```sh
   grok-build-proxy doctor
   ```

6. Start the proxy:

   ```sh
   grok-build-proxy serve
   ```

7. In a separate terminal, launch Grok Build with the proxy-backed GPT model,
   then use `/effort` to select the reasoning effort for the active session:

   ```sh
   grok -m codex-sol
   ```

   The model picker surfaces effort choices for models that advertise this
   capability.

### Serve monitor

When standard input and output are attached to a terminal, `grok-build-proxy
serve` (and the default `grok-build-proxy` command) opens an interactive monitor
instead of scrolling logs. It shows sessions, active and recent requests,
observed output-token throughput, and error events from real proxy traffic.

- `↑`/`k` and `↓`/`j`: move the selection
- `Enter`: open session or request details
- `?`: show or close help
- `Esc`/`Backspace`: return to the dashboard
- `q`/`Ctrl-C`: stop the proxy and restore the terminal

Use plain logs for scripts, background services, or troubleshooting:

```sh
grok-build-proxy serve --no-monitor
```

Non-interactive output automatically keeps the existing plain-log behavior.

## Reasoning effort selection

`[models].default_reasoning_effort` sets Grok Build's default; `/effort` changes
that selection for the current session. The proxy preserves each request's
`reasoning.effort` on both `POST /v1/responses` and `POST /responses`; it does
not force a global effort value.

Capable models advertise `low`, `medium`, `high`, and `xhigh`. Codex also has
`max` and `ultra` levels, but they are not exposed because the current Grok Build
wire contract cannot represent them as distinct values. The proxy does not
silently map them to another level.

Capability metadata appears on both `GET /v1/models` and `GET /models`.
Canonical catalog routes, configured model-map aliases, and eligible generated
`-fast` routes inherit their target's capability. Unknown or unsupported models
omit the capability fields.

## Responses Lite, Plan, and Goal compatibility

The required compatibility layer was introduced in v0.0.7 and remains enabled
in current releases. Grok Build displays streamed text immediately, but accepts a
turn from the final `response.completed.response.output`. Some private Responses
Lite streams omit
standard event-envelope fields such as `sequence_number`, `output_index`,
`content_index`, or `item_id`; others emit a complete delta sequence followed by
an empty or malformed `done` payload. A visible answer or complete tool call can
therefore be discarded at the terminal boundary and retried unless the proxy
normalizes both the live events and the final response.

Version `0.0.7` extends the canonical Responses Lite assembler to:

- fill missing event-envelope fields before forwarding events to Grok Build;
- associate output by explicit index, item ID, call ID, or one unambiguous open
  output;
- synthesize stable message and tool item IDs, then rebind them when an upstream
  item ID arrives later;
- preserve accumulated text and refusal deltas when the matching done value is
  empty;
- recover function arguments from a completed, valid delta sequence when the
  done payload is empty or invalid;
- recover custom-tool input from deltas while preserving intentionally empty
  input and rejecting a missing input;
- reconstruct `function_call` and `custom_tool_call` items from compact or
  standard stream shapes;
- keep multiple Plan and Goal calls distinct, including `ask_user_question`,
  `exit_plan_mode`, and `update_goal`;
- merge streamed text and tools into the terminal output without duplication;
- fail closed instead of executing an incomplete or ambiguous tool call;
- log proxy-generated normalization failures without logging model content;
- preserve `response.incomplete`, `response.failed`, and upstream error
  terminals instead of upgrading them to completed;
- preserve explicit `tool_choice` values and `function_call_output.call_id`
  across multi-turn Plan and Goal requests.

It also includes the request and text-stream compatibility introduced in earlier
releases, including `system` to `developer` role normalization and final text
backfilling. See [`COMPATIBILITY.md`](COMPATIBILITY.md) for the support contract
and validation matrix.

The compatibility version defaults to `0.144.0`. Override it only when testing a
newer official Codex contract:

```sh
GROK_BUILD_PROXY_CODEX_COMPAT_VERSION=0.144.0 grok-build-proxy
```

The full tool-call assembler is enabled by default. Temporary rollback modes are
available when diagnosing an upstream protocol change:

```sh
# Disable all Responses Lite response normalization.
GROK_BUILD_PROXY_RESPONSES_COMPAT=off grok-build-proxy

# Keep only the earlier text compatibility behavior. Missing tool terminals fail.
GROK_BUILD_PROXY_RESPONSES_COMPAT=text grok-build-proxy
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

Useful health checks (these examples assume no client token is configured):

```sh
# Always unauthenticated.
curl --fail http://127.0.0.1:18765/healthz

# Protected when client-token authentication is enabled.
curl --fail http://127.0.0.1:18765/readyz
curl -fsS http://127.0.0.1:18765/v1/models | python3 -c '
import json, sys
for model in json.load(sys.stdin)["data"]:
    fields = {key: model[key] for key in (
        "supports_reasoning_effort", "reasoning_effort", "reasoning_efforts"
    ) if key in model}
    if fields:
        print(model["id"], fields)
'
```

`GET /v1/models` and `GET /models` are equivalent route variants. When
`--client-token` or `GROK_BUILD_PROXY_TOKEN` enables client authentication,
`/readyz`, `/v1/models`, `/models`, `/v1/responses`, and `/responses` require
`Authorization: Bearer $GROK_BUILD_PROXY_TOKEN`; add that header to direct
requests. The proxy-backed Grok model must likewise use this configured local
proxy token as its API key instead of `unused`. It is not a Codex or ChatGPT
access token or the contents of `auth.json`; never use, send, paste, or expose
those upstream secrets as local client credentials. `/healthz` and its `/`
health alias remain unauthenticated.

## Plan and Goal

Run the parent session with a proxy-backed Responses model:

```sh
grok -m codex-sol
```

`/plan` and `/goal` are implemented by Grok Build, not by this proxy. The proxy
supplies protocol compatibility for their text and tool-call turns. Subagents
inherit the parent model unless Grok configuration assigns a different model to
that role. A subagent configured to use another provider will not pass through
this proxy.

For the first Goal test, use a small disposable Git repository and independently
verify the resulting diff and tests. Goal orchestration may run several planner,
verifier, strategist, and summarizer requests concurrently.

## Model substitutions

The proxy can preserve Grok-facing IDs while selecting Codex targets. The map
applies to every `POST /v1/responses` or `POST /responses` request that reaches
the proxy, including parent sessions, `/plan`, inherited subagents, and Goal
planner/verifier/strategist/summarizer requests whose resolved source ID is
mapped.

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

### Serve

| Flag | Environment variable | Default |
|---|---|---|
| `--listen` | `GROK_BUILD_PROXY_LISTEN` | `127.0.0.1:18765` |
| `--auth-file` | `GROK_BUILD_PROXY_AUTH_FILE` | dedicated Codex home `auth.json` |
| `--upstream` | `GROK_BUILD_PROXY_UPSTREAM` | ChatGPT Codex Responses endpoint |
| `--refresh-url` | `GROK_BUILD_PROXY_REFRESH_URL` | OpenAI OAuth token endpoint |
| `--models` | `GROK_BUILD_PROXY_MODELS` | built-in catalog |
| `--model-map` | `GROK_BUILD_PROXY_MODEL_MAP` | empty |
| `--codex-compat-version` | `GROK_BUILD_PROXY_CODEX_COMPAT_VERSION` | `0.144.0` |
| — | `GROK_BUILD_PROXY_RESPONSES_COMPAT` | `full` (`full`, `text`, or `off`) |
| `--client-token` | `GROK_BUILD_PROXY_TOKEN` | empty |
| `--log-format` | `GROK_BUILD_PROXY_LOG_FORMAT` | `text` |
| `--no-monitor` | — | auto-enable monitor on an interactive terminal |
| `--print-grok-config` | — | print model blocks and exit |

`GROK_BUILD_PROXY_RESPONSES_COMPAT` also accepts `legacy` for `text`, and
`false` or `0` for `off`. Unknown values currently fall back to `full`.

### Auth and doctor

| Purpose | Flag or environment variable | Default |
|---|---|---|
| Dedicated Codex home | `--codex-home`, `GROK_BUILD_PROXY_CODEX_HOME`, or `CODEX_HOME` | `~/.codex-grok-build-proxy` |
| Codex executable | `--codex-binary`, `GROK_BUILD_PROXY_CODEX_BINARY` | `codex` |
| Grok executable (doctor) | `--grok-binary`, `GROK_BUILD_PROXY_GROK_BINARY` | `grok` |
| Grok config (doctor) | `--grok-config`, `GROK_BUILD_PROXY_GROK_CONFIG` | `~/.grok/config.toml` |
| Doctor timeout | `--timeout` | 5 seconds |

`doctor` also accepts `--auth-file`, `--listen`, `--model-map`, and
`--client-token`, with the same environment variables shown in the Serve table.
Run `grok-build-proxy serve --help`, `auth <command> --help`, or `doctor --help`
for the complete command-specific options.

A bearer token is mandatory when binding to a non-loopback address. Keep the
default loopback binding whenever possible.

## Troubleshooting

- `command not found`: ensure `$HOME/.local/bin` is in `PATH`.
- `auth.json` missing: run `grok-build-proxy auth login`.
- The same text answer or a Plan/Goal tool call is repeated: upgrade to `0.0.7`
  or newer. Proxy-generated failures now log `error_type`, `response_id`, output
  state count, and buffered state byte count without logging model content.
- `proxy_incomplete_output`: the upstream stream ended before a safe executable
  tool call could be reconstructed; the proxy intentionally did not complete it.
- `proxy_missing_terminal_output`: no unambiguous text or tool output could be
  assembled; capture the Grok Build sampling log because the private stream shape
  may have changed.
- `System messages are not allowed`: upgrade to `0.0.3` or newer.
- Other upstream rejections with a GPT-5.6 model: correlate the sanitized
  `status`, `request_id`, and summarized `upstream_error` log fields, then
  confirm `grok-build-proxy --version` reports `0.0.7` or newer. Do not share
  credentials, `auth.json`, or unreviewed logs.
- 401: run `grok-build-proxy auth status`, then log in again if required.
- Mapping has no effect: confirm the selected Grok entry points to this local
  endpoint and its `model` value exactly matches the map source.
- Port 18765 occupied: run `lsof -nP -iTCP:18765 -sTCP:LISTEN` or change both
  `--listen` and the Grok `base_url`.

## Development and release

```sh
git clone https://github.com/bengHak/grok-build-proxy.git
cd grok-build-proxy
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
make dist
```

Release assets are built for macOS arm64 and amd64 and published with a SHA-256
manifest. See [`SECURITY.md`](SECURITY.md) for credential and vulnerability
reporting guidance. Licensed under MIT.
