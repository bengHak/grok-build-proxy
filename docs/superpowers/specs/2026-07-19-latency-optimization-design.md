# GPT-5.6 Latency Optimization Design

## Objective

Reduce the end-to-end latency gap between Grok Build through
`grok-build-proxy` and native Codex CLI for GPT-5.6, without weakening
Responses Lite compatibility or changing model behavior on unverified
assumptions.

The work is split into independently verifiable stages. Each stage must show
which part of the latency budget it changes before the next optimization is
selected.

## Current evidence

- `gpt-5.6-sol`, `terra`, and `luna` use Responses Lite in both this proxy and
  native Codex CLI.
- Both paths send `parallel_tool_calls: false` for Responses Lite. Native Codex
  obtains practical parallelism through Code Mode, so changing this flag in the
  proxy is not a supported optimization.
- This proxy uses pooled HTTP requests and SSE. It does not implement Responses
  WebSocket continuation or `previous_response_id` input deltas.
- The proxy advertises a 372,000-token context window, matching Codex CLI
  0.144.5 model metadata. The value must not be changed to 272,000.
- Prompt-cache keys and cache-usage counters already exist, but the current
  request lifecycle does not expose enough timing data to attribute latency.
- The Responses Lite normalizer streams chunks as they arrive. It is a latency
  cause only when malformed terminal output triggers a client retry.
- Every Codex request briefly serializes on the credential store and reads the
  auth file. Token refresh holds the same lock across its network request.

## Approaches considered

### 1. Implement WebSocket continuation immediately

This targets a confirmed architectural difference, but it cannot show whether
the observed delay is primarily repeated model/tool turns, prompt-cache misses,
hidden retries, or transport. It also introduces session state with
cross-conversation isolation requirements. This is deferred until measurements
show that long-turn transport and repeated input are material.

### 2. Evidence-first staged optimization — selected

Add a small latency breakdown to the existing request events, reproduce the
three latency shapes, then fix the largest measured source. This reuses the
current monitor and event store and avoids a new metrics subsystem.

### 3. Configuration-only tuning

Matching reasoning effort and service tier is required for valid comparisons,
and Fast mode is useful for immediate speed. It does not address repeated
model/tool round trips or missing continuation, so it is a benchmark control,
not the complete solution.

## Stage 1: request latency attribution

### Data model

Extend the existing `RequestEvent` and stored turn record with the minimum
fields needed to divide one proxy request into observable phases:

- `request_body_bytes`: incoming request body size.
- `input_item_count`: item count after request transformation.
- `proxy_prepare_ms`: request receipt through transformation and identity
  derivation.
- `credential_ms`: credential load, lock wait, and refresh time combined.
- `upstream_headers_ms`: upstream send start through response headers.
- `first_chunk_ms`: upstream send start through the first body chunk, or zero
  when no chunk arrives.
- `request_fingerprint`: a short, process-local hash used only to identify
  likely duplicate requests without logging prompt content.

`duration_ms`, usage tokens, response ID, output count, failure kind, model,
Lite/Fast flags, and retry attempt already exist and remain authoritative.

The first implementation deliberately combines credential lock, file read, and
refresh into `credential_ms`. Splitting these phases is unnecessary unless that
combined value is material.

### Fingerprint privacy and semantics

Build the fingerprint from the transformed request after removing known
per-request metadata such as request IDs and tracing metadata. Use the standard
library hasher; stability across process restarts is not required. Never log or
persist the source body. Compute the fingerprint and item count from the JSON
value already decoded by request transformation; instrumentation must not add
another parse or full-body copy to the measured path.

Two matching fingerprints in the same conversation within 30 seconds are
diagnostic evidence of a likely retry, not proof. Existing terminal status,
failure type, response ID, and output count determine whether it is classified
as a retry candidate.

### Capture points

The handler owns the end-to-end request start and body size. Request
transformation records `proxy_prepare_ms`. The Codex upstream function records
credential and header timings. The streamed response wrapper records the first
chunk exactly once. The completed event carries the assembled timing values to
the existing observer and store.

No prompt or response text is added to logs or metrics.

### Presentation

Add the new values to plain structured logs and the existing turn-detail view.
Do not add a new dashboard panel or persistent metrics database. Zero denotes
“not observed” only for `first_chunk_ms`; other values are valid elapsed times.

## Stage 2: reproducible comparison

Run native Codex CLI and the proxy path with the same GPT-5.6 variant,
reasoning effort, service tier, repository state, and prompt. Compare medians
over at least five runs for each workload:

1. A short response with no tools isolates inference tier and transport.
2. Reading eight independent files isolates tool orchestration.
3. A ten-step tool loop isolates repeated model/tool round trips.
4. A ten-turn growing conversation isolates cache and continuation behavior.
5. A Goal or subagent workload isolates cache lineage and credential
   serialization under concurrency.

Record total wall time, model request count, request bytes, timing phases,
fresh/cached input tokens, output count, and retry candidates. Standard must be
compared with Standard and Fast with Fast.

## Stage 3: decision gates

Apply one root-cause change at a time.

### Hidden retry gate

If a repeated fingerprint follows `proxy_incomplete_output`, empty terminal
output, or another assembler failure, add the captured upstream event shape as
a regression fixture and fix the shared `StreamNormalizer`. Do not disable the
normalizer.

### Tool-round-trip gate

If tool-heavy workloads use materially more model requests than native Codex
while individual upstream timings are comparable, the root cause is the agent
tool surface. Start with a Grok instruction to batch independent shell work.
If that is insufficient, the owning harness needs one batch/programmatic tool
that executes independent child tools concurrently. The proxy must not execute
local tools or assume Grok permissions.

### Continuation gate

If request bodies and first-chunk latency grow across a tool loop or long
conversation despite healthy prompt-cache reads, implement Responses WebSocket
continuation as a separate design and plan. That design must include:

- one bounded connection state per verified conversation/agent lineage;
- strict append-only input comparison;
- equality checks for model, instructions, tools, tool choice, reasoning,
  service tier, cache key, include, and text settings;
- last completed response ID and server-added response items;
- state reset on errors or mismatches;
- full HTTP request fallback;
- TTL and maximum-session bounds;
- tests proving cross-conversation isolation.

Allowing `previous_response_id` through the field filter alone is insufficient
because Grok Build does not maintain this state.

### Cache-lineage gate

If Goal/subagent requests have low cache reads while sharing a large exact
prefix, add an explicit Grok-provided cache-lineage identifier. Use it only for
`prompt_cache_key` and `x-session-affinity`; keep thread, session, and
continuation identities child-specific. Common instructions and tool schemas
must stay at the start of the prompt, with role-specific content appended.

### Local-overhead gate

If `credential_ms` is material under normal traffic, cache valid credentials in
memory and serialize refresh only. Reload on file change or a 401. If
`proxy_prepare_ms` is material only for large bodies, pass one decoded JSON
value through both transformation stages to remove the second parse/serialize
cycle.

User-Agent changes remain an isolated development-only A/B test. They are not
shipped unless repeated measurements show a benefit with every other request
property held constant.

## Non-goals

- Do not force `parallel_tool_calls: true` for Responses Lite.
- Do not change the GPT-5.6 context metadata to 272,000.
- Do not disable the Responses Lite normalizer.
- Do not make the proxy execute Grok-owned local tools or subagents.
- Do not add a metrics service, tracing dependency, or persistent request-body
  capture.
- Do not optimize User-Agent strings, JSON copies, or auth locking before their
  measured phase is material.

## Testing and verification

Stage 1 is complete when:

- unit tests demonstrate each new timing/size field reaches the store;
- a streaming integration test demonstrates first-chunk timing is captured;
- a test demonstrates dynamic request metadata does not change the duplicate
  fingerprint;
- logs and reports contain no prompt, response, token, or account secrets;
- all current tests pass.

An optimization stage is complete only when the matched benchmark that selected
it improves without increasing failures, retry candidates, or incorrect output.
WebSocket continuation additionally requires full-request fallback and
cross-conversation isolation tests before it can be enabled by default.

## Delivery order

1. Latency attribution and duplicate fingerprint.
2. Matched benchmark capture.
3. Hidden retry fix, if observed.
4. Tool batching or WebSocket continuation, selected by evidence.
5. Cache lineage, if Goal/subagent cache evidence supports it.
6. Credential or JSON optimization, only if local phase timing supports it.
