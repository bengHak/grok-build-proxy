# Maximizing Codex prompt-cache hits

Codex subscription credits bill **cached input tokens at roughly 1/10th** the rate of
fresh input tokens. Higher hit rates therefore extend quota for the same workload.

## How this proxy routes cache keys

Priority order:

1. Body `prompt_cache_key` (authoritative)
2. Header `x-grok-conv-id`
3. Cache lineage: `x-grok-cache-lineage`, `x-grok-cache-lineage-id`, or `x-cache-lineage`
4. Header `x-grok-session-id`

Request IDs and proxy-generated UUIDs are **never** used as cache keys.

## Goal / subagent lineage

Child sessions often get a different `x-grok-session-id` even when they share the
same system prompt and tool definitions as the parent. Set:

```http
x-grok-cache-lineage: <parent-prompt-cache-key-or-conv-id>
```

so those turns reuse the parent cache namespace.

## Prefix stability

Responses Lite rebuilds tools into an `additional_tools` developer item at the
front of `input`. This proxy sorts tools by `name` (then `type`) before send so
identical tool sets produce the same prefix regardless of emission order.

Keep system/developer instructions and tool schemas stable across turns. Put
dynamic content after the shared prefix.

## Observability

Plain logs include:

- `input_tokens`
- `cached_input_tokens`
- `cache_write_tokens`
- `fresh_input_tokens`
- `cache_read_percent`

When `input_tokens >= 2048` and `cached_input_tokens == 0`, a warning is emitted
(without prompt content) so operators can investigate key/prefix stability.

## Practical checklist

1. Prefer long multi-turn sessions over many short sessions.
2. Keep system + tools fixed; avoid reordering or rewriting them each turn.
3. Send a stable `prompt_cache_key` or `x-grok-conv-id` for the whole conversation.
4. For Goal/subagent children, set `x-grok-cache-lineage` to the parent key.
5. Watch `cache_read_percent` in logs / the serve monitor.
