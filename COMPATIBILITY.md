# Compatibility

## v0.0.5 support contract

`grok-build-proxy` adapts Grok Build's public Responses API requests to the
private ChatGPT Codex Responses Lite contract. Grok Build remains responsible
for Plan state, Goal state, permissions, subagents, tool execution, and session
persistence.

The v0.0.5 response adapter supports:

- assistant `output_text` and refusal content;
- `function_call` items assembled from output-item and argument events;
- `custom_tool_call` items with completed input;
- multiple interleaved output indexes;
- terminal output merging without duplicate items;
- a single completed, incomplete, failed, or error terminal;
- fail-closed handling for incomplete executable calls;
- CRLF, multiline `data:` fields, and arbitrary HTTP read boundaries.

The request adapter preserves:

- explicit `tool_choice` values (`auto`, `none`, `required`, or an object);
- `function_call_output.call_id` and output payloads;
- the order of multi-turn input items;
- developer instructions and tool definitions in Responses Lite input.

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

The repository CI runs formatting, `go vet`, race-enabled unit and integration
tests, installer validation, and macOS arm64/amd64 release builds. Stream tests
cover text, mixed text/tool output, `exit_plan_mode`, `update_goal`, multiple
interleaved function calls, invalid arguments, missing terminals, incomplete and
failed responses, custom tool calls, and fuzzed network chunk boundaries.

The protocol comparison for this release was based on Grok Build's Responses
stream implementation on the `main` branch at commit
`c68e39f60462f28d9be5e683d9cbe2c57b1a5027`. The private Codex backend can
change independently; use `GROK_BUILD_PROXY_RESPONSES_COMPAT=text` or `off` as a
temporary diagnostic rollback when that happens.
