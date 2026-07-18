# GPT-5.6 Latency Baseline

Status: awaiting matched live runs.

This worksheet compares native Codex CLI with Grok Build through
`grok-build-proxy`. Do not compare unmatched model, service tier, reasoning
effort, repository state, prompt, or cache warm-up conditions.

## Controls

- Record the exact Codex CLI and proxy versions.
- Use the same GPT-5.6 variant, reasoning effort, and service tier on both paths.
- Reset to the same repository commit before each run.
- Run each path at least five times per workload and compare medians.
- Keep cold and warm-cache runs in separate result sets.
- Record failures and retry candidates; do not discard slow failed runs.

## Workloads

1. `short-no-tools`: short response without tools; isolates inference tier and transport.
2. `eight-independent-files`: read and summarize eight independent files; isolates tool orchestration.
3. `ten-step-tool-loop`: ten dependent tool steps; isolates repeated model/tool round trips.
4. `ten-turn-growing-session`: ten append-only turns; isolates cache and continuation behavior.
5. `goal-or-subagents`: one Goal/subagent workload; isolates cache lineage and concurrent credential access.

## Run data

Add one row only after a real run completes. `first_chunk_ms` is measured from
the final upstream attempt. `requests` is the number of model requests for the
workload, not the number of local tool calls.

| path | model | tier | effort | workload | run | wall_ms | requests | body_bytes | prepare_ms | credential_ms | headers_ms | first_chunk_ms | fresh_input | cached_input | outputs | retry_candidates |
|---|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|

## Median comparison

Populate this table only after each compared path has at least five matched
runs. Use the median of each run-level value; do not average per-request
latencies across workloads with different request counts.

| workload | native wall_ms | proxy wall_ms | ratio | main measured delta | decision gate |
|---|---:|---:|---:|---|---|

## Decision gates

- Repeated fingerprint after an incomplete/empty terminal: fix the shared stream normalizer.
- More model requests only on tool-heavy work: improve tool batching in the owning harness.
- Growing body and first-chunk time despite healthy cache reads: design WebSocket continuation.
- Low Goal/subagent cache reads with a shared prefix: add explicit cache lineage.
- Material `credential_ms`: cache valid credentials and serialize refresh only.
- Material `proxy_prepare_ms` on large bodies: remove remaining duplicate JSON decoding.

No gate is selected until the matched run data supports it.
