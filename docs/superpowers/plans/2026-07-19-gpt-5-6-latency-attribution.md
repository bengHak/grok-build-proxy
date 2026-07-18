# GPT-5.6 Latency Attribution Implementation Plan

> **Required implementation skill:** Use `superpowers:executing-plans` for inline execution. Use `superpowers:subagent-driven-development` only when the user explicitly authorizes delegation.

**Goal:** Add privacy-safe per-request latency attribution and likely-retry evidence so the next GPT-5.6 optimization is selected from measured data rather than assumptions.

**Architecture:** Reuse the existing `RequestEvent -> Dashboard store -> monitor/report` path. Prepare the final Codex-compatible body once before sending, compute a process-local fingerprint from that already-decoded JSON value, return upstream timing beside the response, and record the first upstream body chunk in the existing stream guard. Keep all diagnostics in memory except the metadata already included in explicit failure-report exports.

**Tech stack:** Rust 2024, Axum, Reqwest, Tokio, Serde JSON, tracing, Ratatui, existing in-memory dashboard store.

**Design source:** `docs/superpowers/specs/2026-07-19-latency-optimization-design.md`

---

## Task 1: Define request diagnostics and a privacy-safe fingerprint

**Files:**

- Modify: `src/events.rs:77-149`
- Modify: `src/proxy.rs:20-27, 601-719, 1416-1554`
- Test: `src/proxy.rs` module tests near the existing request-transformation tests

### Step 1: Write failing fingerprint tests

Add tests that prepare two semantically identical Codex requests whose JSON key order, input item IDs, and `client_metadata` values differ. Require the same non-empty fingerprint and the same final input-item count.

```rust
#[test]
fn request_fingerprint_ignores_dynamic_metadata_and_key_order() {
    let a = br#"{
        \"model\":\"gpt-5.6-sol\",
        \"input\":[{\"id\":\"msg-a\",\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"inspect\"}]}],
        \"client_metadata\":{\"turn_id\":\"turn-a\"}
    }"#;
    let b = br#"{
        \"client_metadata\":{\"turn_id\":\"turn-b\"},
        \"input\":[{\"content\":[{\"text\":\"inspect\",\"type\":\"input_text\"}],\"role\":\"user\",\"type\":\"message\",\"id\":\"msg-b\"}],
        \"model\":\"gpt-5.6-sol\"
    }"#;
    let identity = UpstreamIdentity {
        thread_id: \"session\".into(),
        request_id: \"request\".into(),
        cache_key: Some(\"conversation\".into()),
    };

    let a = prepare_codex_request(a, &identity, true).unwrap();
    let b = prepare_codex_request(b, &identity, true).unwrap();

    assert!(!a.request_fingerprint.is_empty());
    assert_eq!(a.request_fingerprint, b.request_fingerprint);
    assert_eq!(a.input_item_count, b.input_item_count);
}

#[test]
fn request_fingerprint_changes_with_model_visible_input() {
    let identity = UpstreamIdentity {
        thread_id: \"session\".into(),
        request_id: \"request\".into(),
        cache_key: Some(\"conversation\".into()),
    };
    let a = prepare_codex_request(
        br#"{\"model\":\"gpt-5.6-sol\",\"input\":\"inspect A\"}"#,
        &identity,
        true,
    )
    .unwrap();
    let b = prepare_codex_request(
        br#"{\"model\":\"gpt-5.6-sol\",\"input\":\"inspect B\"}"#,
        &identity,
        true,
    )
    .unwrap();

    assert_ne!(a.request_fingerprint, b.request_fingerprint);
}
```

Run:

```bash
cargo test request_fingerprint -- --nocapture
```

Expected: compilation fails because `prepare_codex_request` and its diagnostic fields do not exist.

### Step 2: Add one diagnostics value object

Add this type to `src/events.rs`:

```rust
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestDiagnostics {
    pub request_body_bytes: u64,
    pub input_item_count: u32,
    pub proxy_prepare_ms: u64,
    pub credential_ms: u64,
    pub upstream_headers_ms: u64,
    pub first_chunk_ms: u64,
    pub request_fingerprint: String,
}
```

Do not add it to `RequestEvent` until Task 2, when the handler has values to record. Keeping the values grouped avoids seven-field initialization across the codebase.

### Step 3: Compute the fingerprint without another parse or body clone

In `src/proxy.rs`, use a process-local randomized standard-library hasher:

```rust
use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hash, Hasher},
    sync::{Arc, OnceLock},
    time::Instant,
};

static REQUEST_FINGERPRINT_STATE: OnceLock<RandomState> = OnceLock::new();

fn request_fingerprint(value: &Value) -> String {
    let state = REQUEST_FINGERPRINT_STATE.get_or_init(RandomState::new);
    let mut hasher = state.build_hasher();
    hash_request_value(value, &mut hasher, true);
    format!("{:016x}", hasher.finish())
}

fn hash_request_value<H: Hasher>(value: &Value, hasher: &mut H, root: bool) {
    match value {
        Value::Null => 0u8.hash(hasher),
        Value::Bool(value) => {
            1u8.hash(hasher);
            value.hash(hasher);
        }
        Value::Number(value) => {
            2u8.hash(hasher);
            value.to_string().hash(hasher);
        }
        Value::String(value) => {
            3u8.hash(hasher);
            value.hash(hasher);
        }
        Value::Array(values) => {
            4u8.hash(hasher);
            values.len().hash(hasher);
            for value in values {
                hash_request_value(value, hasher, false);
            }
        }
        Value::Object(values) => {
            5u8.hash(hasher);
            let mut entries: Vec<_> = values
                .iter()
                .filter(|(key, _)| !(root && key.as_str() == "client_metadata"))
                .collect();
            entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
            entries.len().hash(hasher);
            for (key, value) in entries {
                key.hash(hasher);
                hash_request_value(value, hasher, false);
            }
        }
    }
}
```

Implement `hash_request_value` recursively with a distinct type tag for null, bool, number, string, array, and object. Sort object keys by name before hashing so JSON insertion order does not matter. Skip only the top-level `client_metadata`; the final Codex normalization has already removed input-item IDs and the allowed-field filter has removed tracing fields. Do not generically skip keys named `id`, because an `id` property inside a tool schema can be model-visible. Keep semantic IDs such as `call_id` and cache/model/tier settings in the hash.

The helper must hash the existing `serde_json::Value` by reference. It must not serialize a second copy, retain the prompt, or add a hashing dependency.

### Step 4: Return the final body and shape from one preparation pass

Replace the private `codex_compat_body_for_identity` result with:

```rust
#[derive(Debug)]
struct PreparedCodexRequest {
    body: Bytes,
    input_item_count: u32,
    request_fingerprint: String,
}
```

Rename the existing private compatibility function to `prepare_codex_request` with signature `fn prepare_codex_request(raw: &[u8], identity: &UpstreamIdentity, lite: bool) -> Result<PreparedCodexRequest>`. Retain all mutations currently in `codex_compat_body_for_identity`, and replace only its final serialization return with the diagnostic return below.

After all existing compatibility mutations:

```rust
let input_item_count = value
    .get("input")
    .and_then(Value::as_array)
    .map_or(0, |items| items.len().min(u32::MAX as usize) as u32);
let request_fingerprint = request_fingerprint(&value);
let body = Bytes::from(serde_json::to_vec(&value)?);
Ok(PreparedCodexRequest {
    body,
    input_item_count,
    request_fingerprint,
})
```

Keep the public compatibility helper source-compatible:

```rust
pub fn codex_compat_body(raw: &[u8], session: &str, lite: bool) -> Result<Vec<u8>> {
    let identity = UpstreamIdentity {
        thread_id: session.to_owned(),
        request_id: session.to_owned(),
        cache_key: (!session.is_empty() && valid_cache_key(session)).then(|| session.to_owned()),
    };
    Ok(prepare_codex_request(raw, &identity, lite)?.body.to_vec())
}
```

Change `TransformedRequest.body` from `Vec<u8>` to `Bytes`, and add `input_item_count` and `request_fingerprint` fields computed from its already-decoded `body` value as a fallback for Kimi. `Bytes::clone()` is reference-counted, so a 401 retry can reuse the final request body without a full-body copy. For Codex, overwrite the shape fields with the final prepared values in the handler so the diagnostics describe the actual Codex wire body.

### Step 5: Prepare Codex once in the handler

In `responses`, make `transformed` mutable. After deriving `UpstreamIdentity` and before creating `base_event`, run `prepare_codex_request` for Codex, replace `transformed.body`, and replace its shape diagnostics. Remove the second compatibility conversion from `send_codex_upstream`; it must send `t.body` directly. This preserves the existing wire body while avoiding another parse/serialization on a 401 retry.

Run:

```bash
cargo test request_fingerprint -- --nocapture
cargo test codex_compat -- --nocapture
```

Expected: both fingerprint tests and all existing compatibility tests pass.

### Step 6: Commit the isolated request-shape change

```bash
git add src/events.rs src/proxy.rs
git commit -m "feat: add privacy-safe request diagnostics"
```

---

## Task 2: Capture preparation, credential, header, and first-chunk timing

**Files:**

- Modify: `src/events.rs:77-149`
- Modify: `src/proxy.rs:721-1077, 1127-1200, 1317-1414`
- Modify: RequestEvent fixtures in `src/store.rs:537-560` and `src/monitor/mod.rs:612-636`
- Test: `src/proxy.rs` module tests near the existing router streaming test

### Step 1: Write a failing streaming integration test

Add a mock credential provider that waits before returning and an Axum upstream that waits before returning headers, then waits again before its first SSE body chunk. Attach `RecordingObserver`, consume the proxy response body, and assert the completed event contains all request-shape and timing fields.

Core assertions:

```rust
let events = observer.events.lock().unwrap();
let completed = events
    .iter()
    .find(|event| event.kind == RequestEventKind::Completed)
    .expect("completed event");

assert_eq!(completed.diagnostics.request_body_bytes, request_body.len() as u64);
assert_eq!(completed.diagnostics.input_item_count, 2); // Lite additional_tools + user message
assert!(!completed.diagnostics.request_fingerprint.is_empty());
assert!(completed.diagnostics.credential_ms >= 10);
assert!(completed.diagnostics.upstream_headers_ms >= 10);
assert!(completed.diagnostics.first_chunk_ms >= 25);
```

Use 15 ms for credential delay, 15 ms before headers, and 20 ms before the first body chunk. The lower bounds above leave scheduler tolerance while still proving each capture point.

Run:

```bash
cargo test handler_records_latency_breakdown_and_first_chunk -- --nocapture
```

Expected: the test fails because upstream timing and first-chunk capture are not implemented.

### Step 2: Start the end-to-end timer before reading the body

Add `pub diagnostics: RequestDiagnostics` to `RequestEvent`, initialize it in `RequestEvent::started`, and add `RequestDiagnostics::default()` to existing RequestEvent fixtures. Move `started = Instant::now()` to the beginning of the valid POST handler, before `axum::body::to_bytes`. When `base_event` is created, set:

```rust
diagnostics: RequestDiagnostics {
    request_body_bytes: body.len().min(u64::MAX as usize) as u64,
    input_item_count: transformed.input_item_count,
    proxy_prepare_ms: started.elapsed().as_millis() as u64,
    request_fingerprint: transformed.request_fingerprint.clone(),
    ..Default::default()
},
```

This makes `proxy_prepare_ms` include body collection, both request transformations, identity derivation, and Kimi validation, but not credential loading or the upstream request.

### Step 3: Return upstream timing with the response

Add a private wrapper:

```rust
struct TimedUpstreamResponse {
    response: reqwest::Response,
    credential_ms: u64,
    upstream_headers_ms: u64,
    upstream_started_at: Instant,
}
```

Change `send_upstream` and `send_codex_upstream` to return this wrapper. In the Codex path:

```rust
let credential_started = Instant::now();
let creds = cfg.credentials.get(force).await.context("load Codex credentials")?;
let credential_ms = credential_started.elapsed().as_millis() as u64;

let upstream_started_at = Instant::now();
let response = req.body(t.body.clone()).send().await?;
let upstream_headers_ms = upstream_started_at.elapsed().as_millis() as u64;
```

The request body clone here is a cheap `Bytes` reference-count increment, not a full-body copy. For Kimi, set `credential_ms` to zero because Kimi credential timing is not independently exposed, and measure the whole Kimi send through response headers as `upstream_headers_ms`.

After the first send, copy the timings into `base_event.diagnostics`. On a 401 retry, saturating-add the second attempt's credential and header time and replace `upstream_started_at` with the final attempt's start. `auth_retried` and `attempt` remain the authoritative indication that the values span two attempts.

### Step 4: Record the first upstream body chunk exactly once

Add `first_chunk_recorded: bool` to `StreamObserveGuard` and this method:

```rust
fn record_first_chunk(&mut self, upstream_started_at: Instant) {
    if self.first_chunk_recorded {
        return;
    }
    self.first_chunk_recorded = true;
    self.event.diagnostics.first_chunk_ms =
        upstream_started_at.elapsed().as_millis() as u64;
}
```

Call it on the first successful upstream `chunk`, before Kimi translation, Lite normalization, or passthrough capture. Do not call it for normalizer-generated EOF output. A real sub-millisecond first chunk may validly record zero; the boolean, not the numeric value, prevents a later chunk from overwriting it.

Add a focused guard test proving a second call does not replace the first measurement.

### Step 5: Run the timing tests

```bash
cargo test handler_records_latency_breakdown_and_first_chunk -- --nocapture
cargo test stream_observe_guard -- --nocapture
```

Expected: the integration test observes one completed event with non-empty fingerprint and the requested timing lower bounds; all existing guard drop/terminal tests still pass.

### Step 6: Commit timing capture

```bash
git add src/events.rs src/proxy.rs src/store.rs src/monitor/mod.rs
git commit -m "feat: capture request latency phases"
```

---

## Task 3: Carry diagnostics through the store and classify likely retries

**Files:**

- Modify: `src/store.rs:18-122, 315-461, 531-725`
- Modify: test fixtures in `src/monitor/mod.rs`, `src/monitor/widgets/session_detail.rs`, `src/monitor/widgets/failures.rs`, and `src/report.rs`
- Test: `src/store.rs` module tests

### Step 1: Write a failing store propagation test

Set distinctive diagnostic values on a started event, complete it, and assert the same object reaches active, recent, and failure records.

```rust
#[test]
fn request_diagnostics_reach_active_recent_and_failure_records() {
    let dashboard = Dashboard::new();
    let diagnostics = RequestDiagnostics {
        request_body_bytes: 1234,
        input_item_count: 9,
        proxy_prepare_ms: 3,
        credential_ms: 4,
        upstream_headers_ms: 5,
        first_chunk_ms: 8,
        request_fingerprint: "fp-safe".into(),
    };

    let mut started = base_event(RequestEventKind::Started);
    started.diagnostics = diagnostics.clone();
    dashboard.observe(started);
    assert_eq!(dashboard.snapshot().active[0].diagnostics, diagnostics);

    let mut failed = base_event(RequestEventKind::Failed);
    failed.failure_kind = Some(FailureKind::ProxyAssemble);
    failed.diagnostics = diagnostics.clone();
    dashboard.observe(failed);

    let snapshot = dashboard.snapshot();
    assert_eq!(snapshot.recent[0].diagnostics, diagnostics);
    assert_eq!(snapshot.failures[0].diagnostics, diagnostics);
}
```

Run:

```bash
cargo test request_diagnostics_reach_active_recent_and_failure_records -- --nocapture
```

Expected: compilation fails because stored `Request` and `FailureRecord` do not carry diagnostics.

### Step 2: Store the diagnostics object without flattening it

Import `RequestDiagnostics`, add `pub diagnostics: RequestDiagnostics` to `Request` and `FailureRecord`, and clone the event value through all started/completed/failure construction and update paths. When a repeated Started event refreshes an active request, update its diagnostics as well as attempt flags.

Use `RequestDiagnostics::default()` in existing fixtures. Do not duplicate the seven fields in `Request`, `FailureRecord`, or `Snapshot`.

### Step 3: Write failing likely-retry tests

Add tests for both sides of the heuristic:

```rust
#[test]
fn marks_matching_suspicious_fingerprints_as_retry_candidates() {
    let dashboard = Dashboard::new();

    let mut first_started = base_event(RequestEventKind::Started);
    first_started.request_id = "first".into();
    first_started.diagnostics.request_fingerprint = "same-fingerprint".into();
    dashboard.observe(first_started);
    let mut first_failed = base_event(RequestEventKind::Failed);
    first_failed.request_id = "first".into();
    first_failed.failure_kind = Some(FailureKind::ProxyAssemble);
    first_failed.error_type = "proxy_incomplete_output".into();
    first_failed.output_count = 0;
    first_failed.diagnostics.request_fingerprint = "same-fingerprint".into();
    dashboard.observe(first_failed);

    let mut second_started = base_event(RequestEventKind::Started);
    second_started.request_id = "second".into();
    second_started.diagnostics.request_fingerprint = "same-fingerprint".into();
    dashboard.observe(second_started);
    let mut second_completed = base_event(RequestEventKind::Completed);
    second_completed.request_id = "second".into();
    second_completed.output_count = 1;
    second_completed.diagnostics.request_fingerprint = "same-fingerprint".into();
    dashboard.observe(second_completed);

    let snapshot = dashboard.snapshot();
    assert!(snapshot.recent.iter().all(|request| request.retry_candidate));
    assert!(snapshot.failures[0].retry_candidate);
}

#[test]
fn successful_repeats_and_different_fingerprints_are_not_retry_candidates() {
    let dashboard = Dashboard::new();
    for request_id in ["success-a", "success-b"] {
        let mut started = base_event(RequestEventKind::Started);
        started.request_id = request_id.into();
        started.diagnostics.request_fingerprint = "successful-repeat".into();
        dashboard.observe(started);
        let mut completed = base_event(RequestEventKind::Completed);
        completed.request_id = request_id.into();
        completed.output_count = 1;
        completed.diagnostics.request_fingerprint = "successful-repeat".into();
        dashboard.observe(completed);
    }

    let mut started = base_event(RequestEventKind::Started);
    started.request_id = "different".into();
    started.diagnostics.request_fingerprint = "different-fingerprint".into();
    dashboard.observe(started);
    let mut failed = base_event(RequestEventKind::Failed);
    failed.request_id = "different".into();
    failed.failure_kind = Some(FailureKind::ProxyAssemble);
    failed.output_count = 0;
    failed.diagnostics.request_fingerprint = "different-fingerprint".into();
    dashboard.observe(failed);

    assert!(dashboard
        .snapshot()
        .recent
        .iter()
        .all(|request| !request.retry_candidate));
}
```

Run:

```bash
cargo test retry_candidates -- --nocapture
```

Expected: compilation fails because `retry_candidate` does not exist.

### Step 4: Implement the bounded heuristic in the store

Add `pub retry_candidate: bool` to `Request` and `FailureRecord`. Just before pushing a completed request into `state.recent`, search the bounded recent ring for a record with:

- the same non-empty `request_fingerprint`;
- the same `session_id`;
- a different request ID;
- a start-time gap no greater than 30 seconds; and
- at least one suspicious terminal: failure kind, non-2xx status, non-empty error type, or zero `output_count`.

When found, mark both records. If the prior request has a `FailureRecord`, update that record by request ID. The current failure record inherits the current request's flag when it is created.

Keep the result explicitly named `retry_candidate`; do not call it a confirmed retry and do not alter request outcomes or retry behavior.

### Step 5: Run store and dependent tests

```bash
cargo test store::tests -- --nocapture
cargo test monitor::widgets::failures::tests -- --nocapture
```

Expected: propagation and classification tests pass, including the fallback behavior of existing failure grouping.

### Step 6: Commit store propagation

```bash
git add src/store.rs src/monitor/mod.rs src/monitor/widgets/session_detail.rs src/monitor/widgets/failures.rs src/report.rs
git commit -m "feat: track likely duplicate requests"
```

---

## Task 4: Expose diagnostics in logs, detail views, and safe reports

**Files:**

- Modify: `src/proxy.rs:1065-1075, 1202-1309`
- Modify: `src/monitor/mod.rs:463-558, 1073-1139`
- Modify: `src/report.rs:89-181, 388-455`
- Modify: `README.md:179-209, 265-293`
- Modify: `SECURITY.md:7-31`
- Test: `src/proxy.rs`, `src/monitor/mod.rs`, and `src/report.rs` module tests

### Step 1: Write failing presentation and privacy tests

Extend the turn-detail test to require two diagnostic lines and a retry marker:

```rust
assert!(text.contains("request: 1234 B · 9 items · fp fp-safe"));
assert!(text.contains("latency_ms: prepare 3 · credential 4 · headers 5 · first_chunk 8"));
assert!(text.contains("retry_candidate: true"));
```

Extend the report tests to require all seven fields and `retry_candidate`, while continuing to reject the diagnostic error message, prompt/body strings, bearer tokens, and account IDs.

Add this in-memory writer and structured-log unit test. It deliberately places secret-shaped text in the event error field and verifies that the diagnostic logger does not emit it:

```rust
#[derive(Clone, Default)]
struct LogBuffer(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::writer::MakeWriter<'a> for LogBuffer {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[test]
fn request_diagnostics_log_omits_content_and_credentials() {
    let output = LogBuffer::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_writer(output.clone())
        .finish();
    let mut event = sample_event();
    event.kind = RequestEventKind::Completed;
    event.status_code = 200;
    event.error =
        "secret prompt response secret access_token=bearer account_id=acct".into();
    event.diagnostics = RequestDiagnostics {
        request_body_bytes: 1234,
        input_item_count: 9,
        proxy_prepare_ms: 3,
        credential_ms: 4,
        upstream_headers_ms: 5,
        first_chunk_ms: 8,
        request_fingerprint: "fp-safe".into(),
    };

    tracing::subscriber::with_default(subscriber, || log_request_diagnostics(&event));
    let text = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();

    assert!(text.contains("request_body_bytes=1234"));
    assert!(text.contains("request_fingerprint=\"fp-safe\""));
    assert!(!text.contains("secret prompt"));
    assert!(!text.contains("response secret"));
    assert!(!text.contains("access_token"));
    assert!(!text.contains("account_id"));
}
```

Run:

```bash
cargo test detail_text_includes_request_diagnostics -- --nocapture
cargo test report -- --nocapture
cargo test request_diagnostics_log_omits_content_and_credentials -- --nocapture
```

Expected: the new assertions fail because diagnostics are not rendered or logged.

### Step 2: Add one terminal structured log

Replace the premature `"request complete"` log emitted when response headers are assembled with a private helper called from `observe_stream_end` after classification:

```rust
fn log_request_diagnostics(event: &RequestEvent) {
    info!(
        request_id = event.request_id,
        requested_model = event.requested_model,
        model = event.model,
        status = event.status_code,
        duration_ms = event.duration_ms,
        request_body_bytes = event.diagnostics.request_body_bytes,
        input_item_count = event.diagnostics.input_item_count,
        proxy_prepare_ms = event.diagnostics.proxy_prepare_ms,
        credential_ms = event.diagnostics.credential_ms,
        upstream_headers_ms = event.diagnostics.upstream_headers_ms,
        first_chunk_ms = event.diagnostics.first_chunk_ms,
        request_fingerprint = event.diagnostics.request_fingerprint,
        attempt = event.attempt,
        output_count = event.output_count,
        "request complete"
    );
}
```

Call the same helper from `observe_failure` so pre-stream upstream failures still produce a terminal record. Ensure every early return in `observe_stream_end`, including the auth-retry failure branch, logs once. Do not log the body, prompt, response text, account ID, cache key, or credential.

### Step 3: Extend existing detail views only

Add `request`, `latency_ms`, and `retry_candidate` lines to `turn_detail_text`. Add the same compact metadata to `failure_detail_text`. Use `-` for an empty fingerprint. Do not add a panel, chart, persistent database, or prompt preview beyond the monitor's existing bounded session context.

### Step 4: Extend explicit failure exports

Render the diagnostic fields and `retry_candidate` in Markdown and JSON failure reports. Keep `error_message` excluded as it is today. The fingerprint is a process-randomized short hash, not request content; document that it is only comparable within one proxy process.

### Step 5: Document operation and privacy

In `README.md`, document the plain-log timing fields, the definition of `first_chunk_ms`, and the likely-retry rule. In `SECURITY.md`, state that explicit failure exports may contain request size/count, timing, and a process-local randomized fingerprint, but never retain source request bodies or a stable cross-process digest.

### Step 6: Run presentation tests and commit

```bash
cargo test detail_text -- --nocapture
cargo test report -- --nocapture
cargo test request_diagnostics_log_omits_content_and_credentials -- --nocapture
git add src/proxy.rs src/monitor/mod.rs src/report.rs README.md SECURITY.md
git commit -m "feat: surface latency diagnostics safely"
```

Expected: all presentation/privacy tests pass and the report contains no injected secret strings.

---

## Task 5: Verify Stage 1 and prepare the measurement handoff

**Files:**

- Modify if needed: `docs/superpowers/specs/2026-07-19-latency-optimization-design.md` only to record implementation deviations discovered during verification
- Create: `docs/performance/2026-07-19-gpt-5-6-latency-baseline.md`

### Step 1: Format and run static checks

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

Expected: both commands exit zero with no diff and no warning.

### Step 2: Run the full test suite

```bash
cargo test --all-targets
```

Expected: the baseline 190 tests plus the new diagnostics, retry-candidate, timing, log, monitor, and report tests all pass.

### Step 3: Confirm prohibited changes did not enter the diff

```bash
git diff 986f646 -- src/catalog.rs src/proxy.rs Cargo.toml
rg -n 'parallel_tool_calls.*true|272_000|previous_response_id|tungstenite|request_body[^_]' src Cargo.toml
```

Review the output manually. Expected:

- no change to the 372,000 GPT-5.6 context metadata;
- no forced `parallel_tool_calls: true` for Responses Lite;
- no WebSocket or new tracing dependency;
- no prompt/body logging or persistence;
- no `previous_response_id` pass-through without continuation state.

### Step 4: Create the baseline worksheet

Create `docs/performance/2026-07-19-gpt-5-6-latency-baseline.md` with one row per run and these columns:

```markdown
| path | model | tier | effort | workload | run | wall_ms | requests | body_bytes | prepare_ms | credential_ms | headers_ms | first_chunk_ms | fresh_input | cached_input | outputs | retry_candidates |
|---|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
```

Include the five workloads from the approved design and a results section that compares medians only after at least five matched runs. Leave measurement rows empty until real native-Codex and proxy runs are captured; do not invent numbers.

### Step 5: Inspect the final diff and commit verification artifacts

```bash
git status --short
git diff --check
git diff 986f646 --stat
git add docs/performance/2026-07-19-gpt-5-6-latency-baseline.md docs/superpowers/specs/2026-07-19-latency-optimization-design.md
git commit -m "docs: add GPT-5.6 latency baseline worksheet"
```

### Step 6: Execution handoff

Report:

- exact tests and static checks run;
- the new test count;
- any implementation deviation from this plan;
- whether real matched benchmark data was captured;
- the first measured decision gate to evaluate next.

Do not claim the native Codex performance gap is fixed at Stage 1. Stage 1 only makes the gap attributable and removes the redundant final-body conversion on auth retry.
