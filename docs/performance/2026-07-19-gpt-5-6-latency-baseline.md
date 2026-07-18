# GPT-5.6 Latency Baseline

Status: first matched live comparison complete; tool-round-trip gate selected.

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

Environment: Codex CLI 0.144.5, Grok 0.2.105, proxy 0.0.17 on
`refacotr/optimization`, 2026-07-19 (Asia/Seoul). All rows use
`gpt-5.6-sol`, standard tier, low effort, and the same repository checkout and
user prompt. Native runs use `--ignore-user-config --ignore-rules`; Grok runs
use an isolated `GROK_HOME`, `--verbatim`, and disable memory, plan,
subagents, and web search. Cache state is live rather than forced cold, so the
fresh and cached token columns remain part of the comparison.

`requests` includes one failed `grok-4.5` request emitted by Grok before every
selected-model run. Thus proxy short runs contain one selected-model call plus
one failed request, unbatched file runs contain nine plus one, and batched file
runs contain three plus one. Dashes indicate data the native CLI did not expose
or proxy phase logs that were not retained for the initial unbatched series.

| path | model | tier | effort | workload | run | wall_ms | requests | body_bytes | input_items | prepare_ms | credential_ms | headers_ms | first_chunk_ms | fresh_input | cached_input | outputs | retry_candidates |
|---|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| native-clean | gpt-5.6-sol | standard | low | short-no-tools | 1 | 5700 | 1 | — | — | — | — | — | — | 6391 | 9984 | 1 | 0 |
| native-clean | gpt-5.6-sol | standard | low | short-no-tools | 2 | 4040 | 1 | — | — | — | — | — | — | 2178 | 9984 | 1 | 0 |
| native-clean | gpt-5.6-sol | standard | low | short-no-tools | 3 | 2430 | 1 | — | — | — | — | — | — | 2178 | 9984 | 1 | 0 |
| native-clean | gpt-5.6-sol | standard | low | short-no-tools | 4 | 2770 | 1 | — | — | — | — | — | — | 130 | 12032 | 1 | 0 |
| native-clean | gpt-5.6-sol | standard | low | short-no-tools | 5 | 2740 | 1 | — | — | — | — | — | — | 2178 | 9984 | 1 | 0 |
| proxy | gpt-5.6-sol | standard | low | short-no-tools | 1 | 3850 | 2 | 42673 | 7 | 0 | 0 | 1195 | 1200 | 9555 | 0 | 0 | 0 |
| proxy | gpt-5.6-sol | standard | low | short-no-tools | 2 | 2810 | 2 | 42673 | 7 | 0 | 0 | 1166 | 1168 | 9555 | 0 | 0 | 0 |
| proxy | gpt-5.6-sol | standard | low | short-no-tools | 3 | 2960 | 2 | 42673 | 7 | 0 | 0 | 1071 | 1072 | 9555 | 0 | 0 | 0 |
| proxy | gpt-5.6-sol | standard | low | short-no-tools | 4 | 4730 | 2 | 42673 | 7 | 0 | 0 | 1764 | 1765 | 9555 | 0 | 0 | 0 |
| proxy | gpt-5.6-sol | standard | low | short-no-tools | 5 | 2590 | 2 | 42673 | 7 | 0 | 0 | 883 | 882 | 9555 | 0 | 0 | 0 |
| native-clean | gpt-5.6-sol | standard | low | eight-independent-files | 1 | 23490 | — | — | — | — | — | — | — | 26862 | 51456 | 1 | 0 |
| native-clean | gpt-5.6-sol | standard | low | eight-independent-files | 2 | 17030 | — | — | — | — | — | — | — | 16892 | 41216 | 1 | 0 |
| native-clean | gpt-5.6-sol | standard | low | eight-independent-files | 3 | 16440 | — | — | — | — | — | — | — | 21185 | 41216 | 1 | 0 |
| native-clean | gpt-5.6-sol | standard | low | eight-independent-files | 4 | 23320 | — | — | — | — | — | — | — | 23713 | 78848 | 1 | 0 |
| native-clean | gpt-5.6-sol | standard | low | eight-independent-files | 5 | 19120 | — | — | — | — | — | — | — | 14334 | 43264 | 1 | 0 |
| proxy | gpt-5.6-sol | standard | low | eight-independent-files | 1 | 33100 | 10 | — | 119 | — | — | — | — | 50283 | 171008 | 8 | 0 |
| proxy | gpt-5.6-sol | standard | low | eight-independent-files | 2 | 27290 | 10 | — | 119 | — | — | — | — | 50283 | 171008 | 8 | 0 |
| proxy | gpt-5.6-sol | standard | low | eight-independent-files | 3 | 30590 | 10 | — | 119 | — | — | — | — | 59243 | 162048 | 8 | 0 |
| proxy | gpt-5.6-sol | standard | low | eight-independent-files | 4 | 28000 | 10 | — | 119 | — | — | — | — | 62315 | 158976 | 8 | 0 |
| proxy | gpt-5.6-sol | standard | low | eight-independent-files | 5 | 28800 | 10 | — | 119 | — | — | — | — | 50283 | 171008 | 8 | 0 |
| proxy+batching | gpt-5.6-sol | standard | low | eight-independent-files | 1 | 20240 | 4 | 176350 | 26 | 1 | 0 | 2449 | 2456 | 17493 | 23040 | 2 | 0 |
| proxy+batching | gpt-5.6-sol | standard | low | eight-independent-files | 2 | 21080 | 4 | 176630 | 26 | 2 | 0 | 2413 | 2461 | 19608 | 20992 | 2 | 0 |
| proxy+batching | gpt-5.6-sol | standard | low | eight-independent-files | 3 | 17650 | 4 | 175726 | 26 | 1 | 0 | 2289 | 2293 | 10405 | 29952 | 2 | 0 |
| proxy+batching | gpt-5.6-sol | standard | low | eight-independent-files | 4 | 21510 | 4 | 176264 | 26 | 1 | 0 | 2093 | 2094 | 10553 | 29952 | 2 | 0 |
| proxy+batching | gpt-5.6-sol | standard | low | eight-independent-files | 5 | 23300 | 4 | 176051 | 26 | 2 | 0 | 2132 | 2138 | 17408 | 23040 | 2 | 0 |

## Median comparison

Populate this table only after each compared path has at least five matched
runs. Use the median of each run-level value; do not average per-request
latencies across workloads with different request counts.

| workload | native wall_ms | proxy wall_ms | ratio | main measured delta | decision gate |
|---|---:|---:|---:|---|---|
| short-no-tools | 2770 | 2960 | 1.07x | No material transport-only gap | None |
| eight-independent-files | 19120 | 28800 | 1.51x | Grok selected-model calls: 9; native diagnostic run batched reads into 3 shell calls | Tool-round-trip |
| eight-independent-files + batching | 19120 | 21080 | 1.10x | Grok selected-model calls: 9 → 3; proxy wall: -26.8% | Gate passed |

## Decision gates

- Repeated fingerprint after an incomplete/empty terminal: fix the shared stream normalizer.
- More model requests only on tool-heavy work: improve tool batching in the owning harness.
- Growing body and first-chunk time despite healthy cache reads: design WebSocket continuation.
- Low Goal/subagent cache reads with a shared prefix: add explicit cache lineage.
- Material `credential_ms`: cache valid credentials and serialize refresh only.
- Material `proxy_prepare_ms` on large bodies: remove remaining duplicate JSON decoding.

The first selected gate is tool-round-trip batching. The opt-in instruction
reduced the matched proxy workload median from 28.80 seconds to 21.08 seconds,
with all five runs returning the requested eight summaries and no observed
retry candidates. The next unmeasured gates remain the dependent ten-step
loop, a ten-turn growing session, and Goal/subagent cache lineage.
