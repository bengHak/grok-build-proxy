# Compatibility

## v0.0.7 support contract

`grok-build-proxy` adapts Grok Build's public Responses API requests to the
private ChatGPT Codex Responses Lite contract. Grok Build remains responsible
for Plan state, Goal state, permissions, subagents, tool execution, and session
persistence.

The v0.0.7 response adapter supports:

- assistant `output_text` and refusal content;
- `function_call` items assembled from output-item and argument events;
- `custom_tool_call` items with completed input;
- standard Responses events and compact Responses Lite events that omit
  `sequence_number`, `output_index`, `content_index`, or `item_id`;
- output association by explicit index, item ID, call ID, or one unambiguous
  open output;
- stable synthesized item IDs that are rebound when a later event supplies the
  upstream item ID;
- preservation of non-empty streamed deltas when a terminal `done` event omits,
  empties, or corrupts its repeated text, function arguments, or custom input;
- validation of recovered function arguments before a call is exposed;
- distinction between an explicitly empty custom-tool input and a missing input;
- multiple interleaved output indexes and distinct Plan or Goal calls;
- terminal output merging without duplicate items;
- a single completed, incomplete, failed, or error terminal;
- fail-closed handling for incomplete or ambiguous executable calls;
- bounded, content-free proxy logs for normalization failures;
- CRLF, multiline `data:` fields, and arbitrary HTTP read boundaries.

The request adapter preserves:

- explicit `tool_choice` values (`auto`, `none`, `required`, or an object);
- `function_call_output.call_id` and output payloads;
- the order of multi-turn input items;
- developer instructions and tool definitions in Responses Lite input;
- request-level `reasoning.effort` on both `POST /v1/responses` and
  `POST /responses`, without replacing it with a proxy-wide value.

## Kimi compatibility

Requests for `kimi-for-coding`, `kimi-k2.6`, or `k2.6` are translated from the
Responses API to Kimi's coding Chat Completions endpoint. The adapter preserves
developer/system instructions, user and assistant messages, function calls and
outputs, function definitions and tool choice, prompt-cache session affinity,
and `low`, `medium`, or `high` reasoning effort. Output is translated back to
incremental Responses events for reasoning summaries, text, function arguments,
terminal output items, and token usage. Both streaming and non-streaming client
requests are supported.

Kimi currently accepts function tools through this adapter. Responses custom
tools are rejected instead of being silently changed into a different tool
contract.

## Reasoning effort and model discovery

For capable models, both `GET /v1/models` and `GET /models` advertise
`supports_reasoning_effort`, the default `reasoning_effort`, and the available
`reasoning_efforts`. Codex advertises `low`, `medium`, `high`, and `xhigh`; Kimi
advertises `low`, `medium`, and `high`. `grok-build-proxy --print-grok-config`
emits the corresponding `supports_reasoning_effort = true` and
`reasoning_efforts` fields for Grok Build model entries.

Capability metadata is inherited by canonical catalog routes, configured
model-map aliases, and eligible generated `-fast` routes. Unknown or unsupported
models omit it. `[models].default_reasoning_effort` is Grok Build's default,
while `/effort` changes the active session; the resulting request value is what
the proxy forwards.

Codex's `max` and `ultra` values are intentionally not exposed. Current Grok
Build cannot represent those as distinct canonical wire values, and the proxy
does not silently map them to another effort.

When client-token authentication is enabled, `/readyz`, `/v1/models`, `/models`,
`/v1/responses`, and `/responses` require
`Authorization: Bearer $GROK_BUILD_PROXY_TOKEN`; `/healthz` and its `/` health
alias are unauthenticated. Each proxy-backed Grok model must use that configured
local proxy token as its API key instead of `unused`. This credential is separate
from Codex or ChatGPT access tokens and `auth.json`; never use or expose those
upstream secrets as local client credentials.

## Plan mode

Protocol support covers the common Plan sequence:

1. repository reads and searches;
2. `plan.md` creation or update;
3. optional `ask_user_question`;
4. `exit_plan_mode`;
5. the next implementation turn after approval.

The Plan approval UI and write restrictions are implemented by Grok Build.

## Goal mode

Protocol support covers tool and text turns used by the parent Goal session and
inherited planner, verifier, strategist, and summarizer subagents. Explicit
subagent model overrides must point to this proxy separately when they should use
Codex.

Always verify the final repository diff and test results independently. Goal
completion policy is owned by Grok Build and may include fail-open paths that are
outside the proxy's protocol responsibilities.

## Validation

The repository CI runs `cargo fmt --check`, strict Clippy, unit and integration
tests, installer validation, and macOS arm64/amd64 release builds. Stream tests
cover indexed and indexless text, empty text/refusal done payloads, mixed
text/tool output, `exit_plan_mode`, `ask_user_question`, `update_goal`, valid
argument-delta recovery from empty or invalid done payloads, explicit empty and
missing custom-tool input, missing completion boundaries, multiple distinct
function calls, call-ID correlation, synthesized-to-upstream ID rebinding,
invalid arguments, missing terminals, incomplete and failed responses, and
fuzzed network chunk boundaries.

The protocol comparison for this release was based on Grok Build's Responses
stream implementation on the `main` branch at commit
`c68e39f60462f28d9be5e683d9cbe2c57b1a5027`. The private Codex backend can
change independently; use `GROK_BUILD_PROXY_RESPONSES_COMPAT=text` or `off` as a
temporary diagnostic rollback when that happens.
