# Maximizing Codex prompt-cache hits

Codex subscription credits bill **cached input tokens at roughly 1/10th** the rate of
fresh input tokens. Higher hit rates therefore extend quota for the same workload.

## How this proxy routes cache keys

Priority order:

1. Body `prompt_cache_key` (authoritative **only** when non-empty and ≤64 characters)
2. Cache lineage: `x-grok-cache-lineage`, `x-grok-cache-lineage-id`, or `x-cache-lineage`
3. Header `x-grok-conv-id`
4. Header `x-grok-session-id`

Request IDs and proxy-generated UUIDs are **never** used as cache keys.

An empty string body `prompt_cache_key` is ignored and falls through to the header
fallbacks above. Overlong body keys (>64 characters) still fail request validation
with `400`.

## Goal / subagent lineage

Child sessions often get a different `x-grok-session-id` and/or `x-grok-conv-id`
even when they share the same system prompt and tool definitions as the parent.
Set lineage to the parent’s **resolved** cache namespace — the same priority the
proxy uses for every request: non-empty valid body `prompt_cache_key`, else
lineage, else `x-grok-conv-id`, else `x-grok-session-id`. Nested children should
reuse the string the parent already resolved to (which may already be a
grandparent lineage value), not merely the parent’s session or conv id:

```http
x-grok-cache-lineage: <parent-resolved-cache-namespace>
```

so those turns reuse the parent cache namespace without inheriting thread/session
identity.

Lineage is cache-routing only. It never becomes `session-id` / `thread-id`.

### When lineage is shadowed

- A non-empty valid body `prompt_cache_key` always wins over lineage.
- If the child must share the parent namespace, either set lineage to the parent
  resolved key (recommended) or avoid sending a distinct body key that would
  override it.

Lineage **does** override ambient `x-grok-conv-id` and `x-grok-session-id`, which
is the common Goal/subagent case (child conversation + parent lineage).

## Prefix stability

Responses Lite rebuilds tools into an `additional_tools` developer item at the
front of `input`. This proxy sorts tools by `name`, `type`, then canonical full
definition before send so identical tool sets, including multiple MCP tools
without a `name`, produce the same prefix regardless of emission order.

Keep system/developer instructions and tool schemas stable across turns. Put
dynamic content after the shared prefix.

## Observability

Plain logs include:

- `input_tokens`
- `cached_input_tokens`
- `cache_write_tokens`
- `fresh_input_tokens`
- `cache_read_percent`

When `input_tokens >= 2048`, `cached_input_tokens == 0`, **and**
`cache_write_tokens == 0`, a content-free warning is emitted so operators can
investigate key/prefix stability. Warnings are limited to GPT-5.6+ Codex
requests using implicit caching or explicit caching with a cache breakpoint.
Providers and older models without write-token reporting, explicit mode without
a breakpoint, and pure cold-start first writes do **not** warn. The warning
includes `cache_write_tokens` and `fresh_input_tokens` for diagnosis and never
logs prompt or response content.

## Practical checklist

1. Prefer long multi-turn sessions over many short sessions.
2. Keep system + tools fixed; avoid reordering or rewriting them each turn.
3. Send a stable non-empty `prompt_cache_key` or rely on conv/session headers for
   the whole conversation.
4. For Goal/subagent children, set `x-grok-cache-lineage` to the parent’s
   **resolved** cache key using the full order (body `prompt_cache_key` →
   lineage → conv-id → session-id). Reuse the parent’s already-resolved
   namespace string when the parent itself inherited lineage. Lineage overrides
   child conv/session; a child body key still overrides lineage.
5. Watch `cache_read_percent` in logs / the serve monitor; treat large-input
   zero-read/zero-write warnings as real misses, not first-write noise.
