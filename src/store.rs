//! Structured monitor store: sessions, active/recent turns, failure ring, metrics samples.

use crate::events::{
    FailureKind, Observer, RequestDiagnostics, RequestEvent, RequestEventKind, RequestPhase,
    TokenUsage, sanitize, sanitize_id,
};
use chrono::{DateTime, Utc};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    env,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const DEFAULT_FAILURE_CAP: usize = 200;
const DEFAULT_RECENT_CAP: usize = 200;
const METRICS_CAP: usize = 120;
const RETRY_CANDIDATE_WINDOW: Duration = Duration::from_secs(30);
/// Per-session output token sparkline bucket width (seconds).
pub const SESSION_TOKEN_BUCKET_SECS: u64 = 10;
const SESSION_TOKEN_BUCKET_HISTORY: usize = 24;

#[derive(Clone, Debug)]
pub struct Request {
    pub id: String,
    pub session_id: String,
    pub requested_model: String,
    pub model: String,
    pub provider: String,
    pub status: u16,
    pub error: String,
    pub error_type: String,
    pub failure_kind: Option<FailureKind>,
    pub usage: Option<TokenUsage>,
    pub output_tokens: u64,
    pub started_at: Instant,
    pub ended_at: Option<Instant>,
    pub duration_ms: u64,
    pub response_id: String,
    pub mapped: bool,
    pub lite: bool,
    pub fast: bool,
    pub auth_retried: bool,
    pub attempt: u32,
    pub output_count: u32,
    pub capture_bytes: u32,
    pub diagnostics: RequestDiagnostics,
    pub retry_candidate: bool,
    pub phase: RequestPhase,
    pub streamed_bytes: u64,
    pub stream_chunks: u64,
    /// First content/usage/first_chunk instant for generation-window tok/s.
    pub generation_started_at: Option<Instant>,
    pub generation_initial_output_tokens: u64,
}

impl Request {
    pub fn duration(&self) -> Duration {
        self.ended_at
            .unwrap_or_else(Instant::now)
            .saturating_duration_since(self.started_at)
    }
    /// Wall-clock rate from request start (includes TTFT / auth).
    pub fn tokens_per_second(&self) -> f64 {
        let seconds = self.duration().as_secs_f64();
        if seconds > 0.0 {
            self.output_tokens as f64 / seconds
        } else {
            0.0
        }
    }

    /// Generation-window rate: tokens after generation start / generation elapsed.
    /// Falls back to wall-clock when generation has not started, or when the
    /// generation window saturates to zero length (e.g. terminal Instant::now mark).
    pub fn generation_tokens_per_second(&self) -> f64 {
        let Some(gen_start) = self.generation_started_at else {
            return self.tokens_per_second();
        };
        let end = self.ended_at.unwrap_or_else(Instant::now);
        let seconds = end.saturating_duration_since(gen_start).as_secs_f64();
        let tokens = self
            .output_tokens
            .saturating_sub(self.generation_initial_output_tokens);
        if seconds > 0.0 {
            tokens as f64 / seconds
        } else {
            // Zero-length window is worse than unmarked — use wall-clock.
            self.tokens_per_second()
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Session {
    pub id: String,
    pub requests: u64,
    pub active: u64,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_write_tokens: u64,
    pub fresh_input_tokens: u64,
    pub output_tokens: u64,
    pub usage_requests: u64,
    pub last_model: String,
    pub last_provider: String,
    /// Latest non-empty user prompt preview observed for this session.
    pub last_prompt: String,
    /// Latest workspace/current-working-directory path observed for this session.
    pub cwd: String,
    pub errors: u64,
    pub last_failure_kind: Option<FailureKind>,
    pub updated_at: Option<DateTime<Utc>>,
    /// Sum of completed-turn durations (seconds) used for lifetime tok/s.
    pub(crate) sample_seconds: f64,
    /// Sum of generation-window durations (seconds) for generation tok/s.
    pub(crate) generation_sample_seconds: f64,
    /// Generation-window output tokens (excludes pre-generation baseline).
    pub(crate) generation_output_tokens: u64,
    /// Rolling 10s output-token buckets (oldest → newest) for sparklines.
    pub output_token_buckets: Vec<u64>,
    /// Last bucket index (unix_secs / SESSION_TOKEN_BUCKET_SECS) written.
    pub(crate) last_output_bucket: Option<u64>,
}

impl Session {
    /// Lifetime / wall-clock fleet rate (sum output / sum complete durations).
    pub fn tokens_per_second(&self) -> f64 {
        if self.sample_seconds > 0.0 {
            self.output_tokens as f64 / self.sample_seconds
        } else {
            0.0
        }
    }

    /// Generation-window session rate (distinct from lifetime tok/s).
    pub fn generation_tokens_per_second(&self) -> f64 {
        if self.generation_sample_seconds > 0.0 {
            self.generation_output_tokens as f64 / self.generation_sample_seconds
        } else {
            0.0
        }
    }

    /// Weighted cache-read ratio for the session: total cached input / total input.
    pub fn cache_read_ratio(&self) -> Option<f64> {
        ratio(
            self.cached_input_tokens,
            self.input_tokens,
            self.usage_requests,
        )
    }
}

#[derive(Clone, Debug)]
pub struct FailureRecord {
    pub ts: DateTime<Utc>,
    pub request_id: String,
    pub session_id: String,
    pub requested_model: String,
    pub model: String,
    pub provider: String,
    pub status_code: u16,
    pub duration_ms: u64,
    pub kind: FailureKind,
    pub error_type: String,
    pub error_message: String,
    pub response_id: String,
    pub mapped: bool,
    pub lite: bool,
    pub fast: bool,
    pub auth_retried: bool,
    pub attempt: u32,
    pub output_count: u32,
    pub capture_bytes: u32,
    pub session_failure_index: u32,
    pub diagnostics: RequestDiagnostics,
    pub retry_candidate: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub sessions: Vec<Session>,
    pub active: Vec<Request>,
    pub recent: Vec<Request>,
    /// Legacy alias for failures (monitor UI).
    pub errors: Vec<Request>,
    pub failures: Vec<FailureRecord>,
    /// 1 Hz fleet-average session tok/s samples (monitor pushes; not per-request).
    pub metrics_tok_s: Vec<f64>,
    pub metrics_completed: Vec<f64>,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_write_tokens: u64,
    pub fresh_input_tokens: u64,
    pub usage_requests: u64,
}

impl Snapshot {
    /// Weighted cache-read ratio across all observed usage: total cached input / total input.
    pub fn cache_read_ratio(&self) -> Option<f64> {
        ratio(
            self.cached_input_tokens,
            self.input_tokens,
            self.usage_requests,
        )
    }
}

fn ratio(numerator: u64, denominator: u64, observations: u64) -> Option<f64> {
    if observations == 0 {
        None
    } else if denominator == 0 {
        Some(0.0)
    } else {
        Some(numerator as f64 / denominator as f64)
    }
}

fn terminal_is_suspicious(request: &Request) -> bool {
    request.failure_kind.is_some()
        || !(200..300).contains(&request.status)
        || !request.error_type.is_empty()
        || request.output_count == 0
}

struct State {
    sessions: HashMap<String, Session>,
    active: HashMap<String, Request>,
    recent: VecDeque<Request>,
    errors: VecDeque<Request>,
    failures: VecDeque<FailureRecord>,
    completed: HashSet<String>,
    session_failure_counts: HashMap<String, u32>,
    failure_cap: usize,
    recent_cap: usize,
    /// Rolling 1 Hz fleet-average tok/s (filled by [`Dashboard::push_tok_s_sample`]).
    metrics_tok_s: VecDeque<f64>,
    metrics_completed: VecDeque<f64>,
    input_tokens: u64,
    cached_input_tokens: u64,
    cache_write_tokens: u64,
    fresh_input_tokens: u64,
    usage_requests: u64,
}

impl Default for State {
    fn default() -> Self {
        Self {
            sessions: HashMap::new(),
            active: HashMap::new(),
            recent: VecDeque::new(),
            errors: VecDeque::new(),
            failures: VecDeque::new(),
            completed: HashSet::new(),
            session_failure_counts: HashMap::new(),
            failure_cap: failure_cap_from_env(),
            recent_cap: recent_cap_from_env(),
            metrics_tok_s: VecDeque::new(),
            metrics_completed: VecDeque::new(),
            input_tokens: 0,
            cached_input_tokens: 0,
            cache_write_tokens: 0,
            fresh_input_tokens: 0,
            usage_requests: 0,
        }
    }
}

fn failure_cap_from_env() -> usize {
    env::var("GROK_BUILD_PROXY_FAILURE_CAP")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n: &usize| *n > 0)
        .unwrap_or(DEFAULT_FAILURE_CAP)
}

/// Completed recent / legacy-errors ring. Distinct from [`failure_cap_from_env`].
pub fn recent_cap_from_env() -> usize {
    env::var("GROK_BUILD_PROXY_RECENT_CAP")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n: &usize| *n > 0)
        .unwrap_or(DEFAULT_RECENT_CAP)
}

fn request_from_event(event: &RequestEvent, request_id: String, session_id: String) -> Request {
    Request {
        id: request_id,
        session_id,
        requested_model: sanitize(&event.requested_model),
        model: sanitize(&event.model),
        provider: sanitize(&event.provider),
        status: 0,
        error: String::new(),
        error_type: String::new(),
        failure_kind: None,
        usage: None,
        output_tokens: 0,
        started_at: event.started_at,
        ended_at: None,
        duration_ms: 0,
        response_id: String::new(),
        mapped: event.mapped,
        lite: event.lite,
        fast: event.fast,
        auth_retried: event.auth_retried,
        attempt: event.attempt.max(1),
        output_count: 0,
        capture_bytes: 0,
        diagnostics: event.diagnostics.clone(),
        retry_candidate: false,
        phase: event.phase,
        streamed_bytes: event.streamed_bytes,
        stream_chunks: event.stream_chunks,
        generation_started_at: None,
        generation_initial_output_tokens: 0,
    }
}

fn maybe_mark_generation(request: &mut Request, event: &RequestEvent) {
    if request.generation_started_at.is_some() {
        return;
    }
    if event.mark_generation_start
        || event.phase == RequestPhase::Streaming
        || event.diagnostics.first_chunk_ms > 0
        || event.output_tokens > 0
        || event.usage.is_some()
    {
        // Prefer reconstructing generation start from first_chunk_ms so terminal-only
        // mark paths still yield a positive generation window (not Instant::now() at end).
        let first_chunk_ms = event
            .diagnostics
            .first_chunk_ms
            .max(request.diagnostics.first_chunk_ms);
        let gen_start = if first_chunk_ms > 0 {
            request
                .started_at
                .checked_add(Duration::from_millis(first_chunk_ms))
                .unwrap_or_else(Instant::now)
        } else {
            Instant::now()
        };
        request.generation_started_at = Some(gen_start);
        // Baseline is pre-generation output on the request. Terminal apply must call
        // this *before* overwriting output_tokens with the absolute final, so baseline
        // is not the final count (which would collapse gen_tokens to 0).
        request.generation_initial_output_tokens = request.output_tokens;
    }
}

fn record_session_output_bucket(session: &mut Session, tokens: u64) {
    if tokens == 0 {
        return;
    }
    let bucket = (Utc::now().timestamp().max(0) as u64) / SESSION_TOKEN_BUCKET_SECS;
    match session.last_output_bucket {
        Some(last) if last == bucket => {
            if let Some(slot) = session.output_token_buckets.last_mut() {
                *slot = slot.saturating_add(tokens);
            }
        }
        Some(last) if bucket > last => {
            let gap = (bucket - last).min(SESSION_TOKEN_BUCKET_HISTORY as u64) as usize;
            for _ in 1..gap {
                session.output_token_buckets.push(0);
            }
            session.output_token_buckets.push(tokens);
            while session.output_token_buckets.len() > SESSION_TOKEN_BUCKET_HISTORY {
                session.output_token_buckets.remove(0);
            }
            session.last_output_bucket = Some(bucket);
        }
        _ => {
            session.output_token_buckets.push(tokens);
            while session.output_token_buckets.len() > SESSION_TOKEN_BUCKET_HISTORY {
                session.output_token_buckets.remove(0);
            }
            session.last_output_bucket = Some(bucket);
        }
    }
}

fn push_rolling(samples: &mut VecDeque<f64>, value: f64) {
    if samples.len() == METRICS_CAP {
        samples.pop_front();
    }
    samples.push_back(value);
}

#[derive(Clone, Default)]
pub struct Dashboard {
    inner: Arc<Mutex<State>>,
}

fn lock_state(inner: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    // Recover from poison so a prior panic during apply cannot permanently kill the monitor.
    inner.lock().unwrap_or_else(|e| e.into_inner())
}

impl Dashboard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_failure_cap(cap: usize) -> Self {
        let d = Self::new();
        lock_state(&d.inner).failure_cap = cap.max(1);
        d
    }

    pub fn with_recent_cap(cap: usize) -> Self {
        let d = Self::new();
        lock_state(&d.inner).recent_cap = cap.max(1);
        d
    }

    /// Seed a deterministic offline demo fixture into the store (no bind/creds).
    pub fn seed_demo_fixture(&self) {
        use crate::events::FailureKind;
        let t0 = Instant::now() - Duration::from_secs(12);

        let mut start = RequestEvent::started(
            "demo-req-1",
            "demo-sess-a",
            "codex-sol",
            "gpt-5.6-sol",
            true,
            true,
            false,
        )
        .with_provider("codex")
        .with_phase(RequestPhase::Preparing);
        start.started_at = t0;
        start.diagnostics.request_body_bytes = 4096;
        start.diagnostics.input_item_count = 12;
        start.diagnostics.request_fingerprint = "demo-fp-a".into();
        self.observe(start);
        self.observe_session_context(
            "demo-sess-a",
            "demo: inspect this session",
            "/tmp/demo-project",
        );

        let mut phase = RequestEvent::started(
            "demo-req-1",
            "demo-sess-a",
            "codex-sol",
            "gpt-5.6-sol",
            true,
            true,
            false,
        )
        .with_provider("codex")
        .with_phase(RequestPhase::Upstream)
        .as_updated();
        phase.started_at = t0;
        phase.diagnostics.credential_ms = 40;
        phase.diagnostics.upstream_headers_ms = 120;
        self.observe(phase);

        let mut progress = RequestEvent::started(
            "demo-req-1",
            "demo-sess-a",
            "codex-sol",
            "gpt-5.6-sol",
            true,
            true,
            false,
        )
        .with_provider("codex")
        .with_phase(RequestPhase::Streaming)
        .as_updated();
        progress.started_at = t0;
        progress.output_tokens = 80;
        progress.streamed_bytes = 2400;
        progress.stream_chunks = 12;
        progress.mark_generation_start = true;
        progress.diagnostics.first_chunk_ms = 350;
        self.observe(progress);

        let mut done = RequestEvent::started(
            "demo-req-1",
            "demo-sess-a",
            "codex-sol",
            "gpt-5.6-sol",
            true,
            true,
            false,
        )
        .with_provider("codex")
        .with_phase(RequestPhase::Streaming);
        done.kind = RequestEventKind::Completed;
        done.started_at = t0;
        done.status_code = 200;
        done.duration_ms = 4000;
        done.output_tokens = 120;
        done.output_count = 2;
        done.streamed_bytes = 3600;
        done.stream_chunks = 18;
        done.usage = Some(TokenUsage {
            input_tokens: 2000,
            cached_input_tokens: 1500,
            cache_write_tokens: 100,
            output_tokens: 120,
        });
        done.diagnostics = RequestDiagnostics {
            request_body_bytes: 4096,
            input_item_count: 12,
            proxy_prepare_ms: 5,
            credential_ms: 40,
            upstream_headers_ms: 120,
            first_chunk_ms: 350,
            request_fingerprint: "demo-fp-a".into(),
        };
        self.observe(done);

        // Second session with a live streaming turn + a failed turn.
        let t1 = Instant::now() - Duration::from_secs(3);
        let mut live = RequestEvent::started(
            "demo-req-live",
            "demo-sess-b",
            "kimi-k3",
            "k3",
            true,
            false,
            false,
        )
        .with_provider("kimi")
        .with_phase(RequestPhase::Streaming);
        live.started_at = t1;
        live.streamed_bytes = 900;
        live.stream_chunks = 4;
        live.output_tokens = 30;
        live.mark_generation_start = true;
        self.observe(live);
        self.observe_session_context("demo-sess-b", "demo: kimi live stream", "/tmp/kimi");

        let mut fail_start = RequestEvent::started(
            "demo-req-fail",
            "demo-sess-b",
            "kimi-k3",
            "k3",
            true,
            false,
            false,
        )
        .with_provider("kimi");
        fail_start.started_at = Instant::now() - Duration::from_secs(8);
        self.observe(fail_start);
        let mut fail = RequestEvent::started(
            "demo-req-fail",
            "demo-sess-b",
            "kimi-k3",
            "k3",
            true,
            false,
            false,
        )
        .with_provider("kimi");
        fail.kind = RequestEventKind::Failed;
        fail.started_at = Instant::now() - Duration::from_secs(8);
        fail.status_code = 502;
        fail.duration_ms = 500;
        fail.failure_kind = Some(FailureKind::UpstreamHttp);
        fail.error_type = "upstream_http".into();
        fail.error = "demo upstream failure".into();
        self.observe(fail);
    }

    pub fn snapshot(&self) -> Snapshot {
        let state = lock_state(&self.inner);
        let mut sessions: Vec<_> = state.sessions.values().cloned().collect();
        sessions.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        let mut active: Vec<_> = state.active.values().cloned().collect();
        active.sort_by_key(|r| r.started_at);
        Snapshot {
            sessions,
            active,
            recent: state.recent.iter().cloned().collect(),
            errors: state.errors.iter().cloned().collect(),
            failures: state.failures.iter().cloned().collect(),
            metrics_tok_s: state.metrics_tok_s.iter().copied().collect(),
            metrics_completed: state.metrics_completed.iter().copied().collect(),
            input_tokens: state.input_tokens,
            cached_input_tokens: state.cached_input_tokens,
            cache_write_tokens: state.cache_write_tokens,
            fresh_input_tokens: state.fresh_input_tokens,
            usage_requests: state.usage_requests,
        }
    }

    /// Append one fleet-average tok/s sample (call at most ~1 Hz from the monitor).
    pub fn push_tok_s_sample(&self, tok_s: f64) {
        let mut state = lock_state(&self.inner);
        let v = if tok_s.is_finite() && tok_s >= 0.0 {
            tok_s
        } else {
            0.0
        };
        push_rolling(&mut state.metrics_tok_s, v);
    }

    /// Failures for later report export (newest first). Optional kind filter.
    pub fn failures_for_report(&self, kind: Option<FailureKind>) -> Vec<FailureRecord> {
        let state = lock_state(&self.inner);
        state
            .failures
            .iter()
            .filter(|f| kind.is_none_or(|k| f.kind == k))
            .cloned()
            .collect()
    }

    fn apply_session_context(&self, session_key: &str, last_prompt: &str, cwd: &str) {
        if last_prompt.trim().is_empty() && cwd.trim().is_empty() {
            return;
        }
        let mut state = lock_state(&self.inner);
        let session = state
            .sessions
            .entry(session_key.to_owned())
            .or_insert_with(|| Session {
                id: sanitize_id(session_key),
                ..Default::default()
            });
        if !last_prompt.trim().is_empty() {
            session.last_prompt = sanitize(last_prompt);
        }
        if !cwd.trim().is_empty() {
            session.cwd = sanitize(cwd);
        }
        session.updated_at = Some(Utc::now());
    }

    fn apply(&self, event: RequestEvent) {
        let mut state = lock_state(&self.inner);
        let request_id = sanitize_id(&event.request_id);
        let session_id = sanitize_id(&event.session_id);
        match event.kind {
            RequestEventKind::Started => {
                if state.completed.contains(&event.request_id) {
                    return;
                }
                // Re-observe Started after auth retry: refresh in-flight attempt flags only.
                // Do NOT regress phase (base_event still carries Preparing) — only advance.
                if let Some(active) = state.active.get_mut(&event.request_id) {
                    active.auth_retried = event.auth_retried;
                    active.attempt = event.attempt.max(1);
                    active.mapped = event.mapped;
                    active.lite = event.lite;
                    active.fast = event.fast;
                    if !event.provider.is_empty() {
                        active.provider = sanitize(&event.provider);
                    }
                    active.diagnostics = event.diagnostics.clone();
                    if phase_rank(event.phase) >= phase_rank(active.phase) {
                        active.phase = event.phase;
                    }
                    if let Some(session) = state.sessions.get_mut(&event.session_id) {
                        session.updated_at = Some(Utc::now());
                        if !event.provider.is_empty() {
                            session.last_provider = sanitize(&event.provider);
                        }
                    }
                    return;
                }
                state.active.insert(
                    event.request_id.clone(),
                    request_from_event(&event, request_id, session_id.clone()),
                );
                let session = state
                    .sessions
                    .entry(event.session_id.clone())
                    .or_insert_with(|| Session {
                        id: session_id,
                        ..Default::default()
                    });
                session.requests += 1;
                session.active += 1;
                session.last_model = sanitize(&event.model);
                if !event.provider.is_empty() {
                    session.last_provider = sanitize(&event.provider);
                }
                session.updated_at = Some(Utc::now());
            }
            RequestEventKind::Updated => {
                // Contract: non-terminal updates mutate active only — never recent/failures.
                if state.completed.contains(&event.request_id) {
                    return;
                }
                let Some(active) = state.active.get_mut(&event.request_id) else {
                    return;
                };
                // Phase only advances (preparing < auth < upstream < streaming).
                if phase_rank(event.phase) >= phase_rank(active.phase) {
                    active.phase = event.phase;
                }
                if !event.provider.is_empty() {
                    active.provider = sanitize(&event.provider);
                }
                active.auth_retried = event.auth_retried;
                active.attempt = event.attempt.max(1);
                active.mapped = event.mapped;
                active.lite = event.lite;
                active.fast = event.fast;
                // Progress counters are absolute from the publisher; never double-add on terminal.
                if event.streamed_bytes > active.streamed_bytes {
                    active.streamed_bytes = event.streamed_bytes;
                }
                if event.stream_chunks > active.stream_chunks {
                    active.stream_chunks = event.stream_chunks;
                }
                if event.output_tokens > active.output_tokens {
                    active.output_tokens = event.output_tokens;
                }
                if event.capture_bytes > active.capture_bytes {
                    active.capture_bytes = event.capture_bytes;
                }
                if event.usage.is_some() {
                    active.usage = event.usage;
                }
                // Merge diagnostics: keep higher phase timings / non-empty fingerprint.
                let d = &event.diagnostics;
                if d.proxy_prepare_ms > 0 {
                    active.diagnostics.proxy_prepare_ms = d.proxy_prepare_ms;
                }
                if d.credential_ms > 0 {
                    active.diagnostics.credential_ms = d.credential_ms;
                }
                if d.upstream_headers_ms > 0 {
                    active.diagnostics.upstream_headers_ms = d.upstream_headers_ms;
                }
                if d.first_chunk_ms > 0 {
                    active.diagnostics.first_chunk_ms = d.first_chunk_ms;
                }
                if d.request_body_bytes > 0 {
                    active.diagnostics.request_body_bytes = d.request_body_bytes;
                }
                if d.input_item_count > 0 {
                    active.diagnostics.input_item_count = d.input_item_count;
                }
                if !d.request_fingerprint.is_empty() {
                    active.diagnostics.request_fingerprint = d.request_fingerprint.clone();
                }
                maybe_mark_generation(active, &event);
                if let Some(session) = state.sessions.get_mut(&event.session_id) {
                    session.updated_at = Some(Utc::now());
                    if !event.provider.is_empty() {
                        session.last_provider = sanitize(&event.provider);
                    }
                }
            }
            RequestEventKind::Completed | RequestEventKind::Failed => {
                if !state.completed.insert(event.request_id.clone()) {
                    return;
                }
                let mut request = state
                    .active
                    .remove(&event.request_id)
                    .unwrap_or_else(|| request_from_event(&event, request_id, session_id.clone()));
                // Terminal overwrite of counters (absolute finals) — no double-count with Updated.
                request.status = event.status_code;
                request.error = sanitize(&event.error);
                request.error_type = sanitize(&event.error_type);
                request.failure_kind = event.failure_kind;
                request.response_id = sanitize(&event.response_id);
                request.mapped = event.mapped;
                request.lite = event.lite;
                request.fast = event.fast;
                request.auth_retried = event.auth_retried;
                request.attempt = event.attempt.max(1);
                request.output_count = event.output_count;
                request.capture_bytes = event.capture_bytes;
                // Diagnostics (incl. first_chunk_ms) before generation mark so terminal-only
                // paths can reconstruct generation_started_at.
                request.diagnostics = event.diagnostics.clone();
                if !event.provider.is_empty() {
                    request.provider = sanitize(&event.provider);
                }
                if event.streamed_bytes > request.streamed_bytes {
                    request.streamed_bytes = event.streamed_bytes;
                }
                if event.stream_chunks > request.stream_chunks {
                    request.stream_chunks = event.stream_chunks;
                }
                // Phase only advances — never regress Auth/Upstream/Streaming to Preparing
                // when terminal publishers still send base_event.phase=Preparing.
                if phase_rank(event.phase) >= phase_rank(request.phase) {
                    request.phase = event.phase;
                }
                // Mark generation *before* final output_tokens / ended_at so baseline is the
                // pre-terminal active count (usually 0) and first_chunk_ms can backdate start.
                maybe_mark_generation(&mut request, &event);
                request.usage = event.usage;
                request.output_tokens = event.output_tokens;
                request.ended_at = Some(Instant::now());
                request.duration_ms = if event.duration_ms > 0 {
                    event.duration_ms
                } else {
                    request.duration().as_millis() as u64
                };

                let duration_secs = request.duration().as_secs_f64();
                let gen_secs = request
                    .generation_started_at
                    .map(|g| {
                        request
                            .ended_at
                            .unwrap_or_else(Instant::now)
                            .saturating_duration_since(g)
                            .as_secs_f64()
                    })
                    .unwrap_or(0.0);
                let gen_tokens = request
                    .output_tokens
                    .saturating_sub(request.generation_initial_output_tokens);
                let failed = event.kind == RequestEventKind::Failed;

                let prior_retry_id = if request.diagnostics.request_fingerprint.is_empty() {
                    None
                } else {
                    state
                        .recent
                        .iter_mut()
                        .find(|prior| {
                            prior.session_id == request.session_id
                                && prior.id != request.id
                                && prior.diagnostics.request_fingerprint
                                    == request.diagnostics.request_fingerprint
                                && request
                                    .started_at
                                    .checked_duration_since(prior.started_at)
                                    .or_else(|| {
                                        prior.started_at.checked_duration_since(request.started_at)
                                    })
                                    .is_some_and(|gap| gap <= RETRY_CANDIDATE_WINDOW)
                                && (terminal_is_suspicious(prior)
                                    || terminal_is_suspicious(&request))
                        })
                        .map(|prior| {
                            prior.retry_candidate = true;
                            request.retry_candidate = true;
                            prior.id.clone()
                        })
                };
                if let Some(prior_id) = prior_retry_id
                    && let Some(prior) = state
                        .failures
                        .iter_mut()
                        .find(|failure| failure.request_id == prior_id)
                {
                    prior.retry_candidate = true;
                }

                let recent_cap = state.recent_cap;
                state.recent.push_front(request.clone());
                state.recent.truncate(recent_cap);

                if failed {
                    state.errors.push_front(request.clone());
                    state.errors.truncate(recent_cap);

                    let session_key = event.session_id.clone();
                    let idx = {
                        let c = state.session_failure_counts.entry(session_key).or_insert(0);
                        *c = c.saturating_add(1);
                        *c
                    };
                    let kind = event.failure_kind.unwrap_or(FailureKind::Unknown);
                    let record = FailureRecord {
                        ts: Utc::now(),
                        request_id: request.id.clone(),
                        session_id: request.session_id.clone(),
                        requested_model: request.requested_model.clone(),
                        model: request.model.clone(),
                        provider: request.provider.clone(),
                        status_code: request.status,
                        duration_ms: request.duration_ms,
                        kind,
                        error_type: if request.error_type.is_empty() {
                            kind.as_str().to_owned()
                        } else {
                            request.error_type.clone()
                        },
                        error_message: request.error.clone(),
                        response_id: request.response_id.clone(),
                        mapped: request.mapped,
                        lite: request.lite,
                        fast: request.fast,
                        auth_retried: request.auth_retried,
                        attempt: request.attempt,
                        output_count: request.output_count,
                        capture_bytes: request.capture_bytes,
                        session_failure_index: idx,
                        diagnostics: request.diagnostics.clone(),
                        retry_candidate: request.retry_candidate,
                    };
                    state.failures.push_front(record);
                    let cap = state.failure_cap;
                    state.failures.truncate(cap);
                }

                // Rolling outcome samples for fail%/done sparklines (tok/s is 1 Hz lifetime avg).
                push_rolling(&mut state.metrics_completed, if failed { 0.0 } else { 1.0 });

                if let Some(usage) = event.usage {
                    state.input_tokens = state.input_tokens.saturating_add(usage.input_tokens);
                    state.cached_input_tokens = state
                        .cached_input_tokens
                        .saturating_add(usage.cached_input_tokens);
                    state.cache_write_tokens = state
                        .cache_write_tokens
                        .saturating_add(usage.cache_write_tokens);
                    state.fresh_input_tokens = state
                        .fresh_input_tokens
                        .saturating_add(usage.fresh_input_tokens());
                    state.usage_requests = state.usage_requests.saturating_add(1);
                }

                let session_key = event.session_id;
                let session = state
                    .sessions
                    .entry(session_key.clone())
                    .or_insert_with(|| Session {
                        id: session_id,
                        ..Default::default()
                    });
                session.active = session.active.saturating_sub(1);
                session.output_tokens = session.output_tokens.saturating_add(event.output_tokens);
                if let Some(usage) = event.usage {
                    session.input_tokens = session.input_tokens.saturating_add(usage.input_tokens);
                    session.cached_input_tokens = session
                        .cached_input_tokens
                        .saturating_add(usage.cached_input_tokens);
                    session.cache_write_tokens = session
                        .cache_write_tokens
                        .saturating_add(usage.cache_write_tokens);
                    session.fresh_input_tokens = session
                        .fresh_input_tokens
                        .saturating_add(usage.fresh_input_tokens());
                    session.usage_requests = session.usage_requests.saturating_add(1);
                }
                if event.output_tokens > 0 {
                    session.sample_seconds += duration_secs;
                }
                if gen_tokens > 0 && gen_secs > 0.0 {
                    session.generation_output_tokens =
                        session.generation_output_tokens.saturating_add(gen_tokens);
                    session.generation_sample_seconds += gen_secs;
                }
                record_session_output_bucket(session, event.output_tokens);
                if !request.provider.is_empty() {
                    session.last_provider = request.provider.clone();
                }
                if failed {
                    session.errors += 1;
                    session.last_failure_kind = event.failure_kind.or(Some(FailureKind::Unknown));
                }
                session.updated_at = Some(Utc::now());
                let completed_cap = state.recent_cap.saturating_mul(4).max(200);
                if state.completed.len() > completed_cap {
                    state.completed.clear();
                }
            }
        }
    }
}

fn phase_rank(phase: RequestPhase) -> u8 {
    match phase {
        RequestPhase::Preparing => 0,
        RequestPhase::Auth => 1,
        RequestPhase::Upstream => 2,
        RequestPhase::Streaming => 3,
    }
}

impl Observer for Dashboard {
    fn observe(&self, event: RequestEvent) {
        self.apply(event)
    }

    fn observe_session_context(&self, session_id: &str, last_prompt: &str, cwd: &str) {
        self.apply_session_context(session_id, last_prompt, cwd)
    }
}

// Re-export Observer trait usage from events via proxy for main compatibility is handled in proxy/monitor.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::FailureKind;
    use std::time::Duration;

    fn base_event(kind: RequestEventKind) -> RequestEvent {
        RequestEvent {
            kind,
            request_id: "req\n1".into(),
            session_id: "session".into(),
            requested_model: "alias".into(),
            model: "gpt".into(),
            provider: "codex".into(),
            status_code: 200,
            usage: None,
            output_tokens: 20,
            error: String::new(),
            started_at: Instant::now() - Duration::from_secs(2),
            duration_ms: 2000,
            failure_kind: None,
            error_type: String::new(),
            response_id: String::new(),
            mapped: true,
            lite: true,
            fast: false,
            auth_retried: false,
            attempt: 1,
            output_count: 0,
            capture_bytes: 0,
            warn_on_cache_miss: false,
            diagnostics: Default::default(),
            phase: RequestPhase::Preparing,
            streamed_bytes: 0,
            stream_chunks: 0,
            mark_generation_start: false,
        }
    }

    #[test]
    fn provider_threads_to_session_and_failure() {
        let d = Dashboard::new();
        let mut start = base_event(RequestEventKind::Started);
        start.provider = "kimi".into();
        d.observe(start);
        assert_eq!(d.snapshot().active[0].provider, "kimi");
        let mut fail = base_event(RequestEventKind::Failed);
        fail.provider = "kimi".into();
        fail.failure_kind = Some(FailureKind::UpstreamHttp);
        d.observe(fail);
        let s = d.snapshot();
        assert_eq!(s.sessions[0].last_provider, "kimi");
        assert_eq!(s.failures[0].provider, "kimi");
    }

    #[test]
    fn updated_mutates_active_only_never_failures() {
        let d = Dashboard::new();
        d.observe(base_event(RequestEventKind::Started));
        let mut upd = base_event(RequestEventKind::Updated);
        upd.phase = RequestPhase::Streaming;
        upd.output_tokens = 50;
        upd.streamed_bytes = 1000;
        upd.stream_chunks = 5;
        upd.mark_generation_start = true;
        d.observe(upd);
        let s = d.snapshot();
        assert_eq!(s.active.len(), 1);
        assert_eq!(s.active[0].phase, RequestPhase::Streaming);
        assert_eq!(s.active[0].output_tokens, 50);
        assert_eq!(s.active[0].streamed_bytes, 1000);
        assert!(s.active[0].generation_started_at.is_some());
        assert!(s.recent.is_empty());
        assert!(s.failures.is_empty());
        assert!(s.errors.is_empty());
    }

    #[test]
    fn progress_then_completed_does_not_double_count_tokens() {
        let d = Dashboard::new();
        d.observe(base_event(RequestEventKind::Started));
        let mut upd = base_event(RequestEventKind::Updated);
        upd.phase = RequestPhase::Streaming;
        upd.output_tokens = 40;
        upd.streamed_bytes = 800;
        d.observe(upd);
        let mut done = base_event(RequestEventKind::Completed);
        done.output_tokens = 40; // same absolute final, not 40+40
        done.output_count = 1;
        d.observe(done);
        let s = d.snapshot();
        assert!(s.active.is_empty());
        assert_eq!(s.recent[0].output_tokens, 40);
        assert_eq!(s.sessions[0].output_tokens, 40);
    }

    #[test]
    fn generation_window_rate_exceeds_wall_clock_on_ttft_heavy_turn() {
        let d = Dashboard::new();
        let start_at = Instant::now() - Duration::from_secs(10);
        let mut start = base_event(RequestEventKind::Started);
        start.started_at = start_at;
        d.observe(start);
        // Generation starts late (TTFT ~8s).
        let mut upd = base_event(RequestEventKind::Updated);
        upd.started_at = start_at;
        upd.phase = RequestPhase::Streaming;
        upd.mark_generation_start = true;
        upd.output_tokens = 0;
        d.observe(upd);
        // Simulate generation window of ~2s with 100 tokens by setting generation_started_at back.
        {
            let mut state = lock_state(&d.inner);
            let req = state.active.get_mut("req\n1").expect("active");
            req.generation_started_at = Some(Instant::now() - Duration::from_secs(2));
            req.generation_initial_output_tokens = 0;
        }
        let mut done = base_event(RequestEventKind::Completed);
        done.started_at = start_at;
        done.duration_ms = 10_000;
        done.output_tokens = 100;
        done.output_count = 1;
        d.observe(done);
        let req = &d.snapshot().recent[0];
        let wall = req.tokens_per_second();
        let gen_rate = req.generation_tokens_per_second();
        assert!(
            gen_rate > wall,
            "generation rate {gen_rate} should exceed wall-clock {wall} on TTFT-heavy turn"
        );
        assert!(wall > 0.0 && wall < 15.0, "wall ~10 tok/s, got {wall}");
        assert!(gen_rate > 30.0, "gen ~50 tok/s, got {gen_rate}");
    }

    /// Terminal-only generation mark (no mid-flight Updated): baseline must not be final
    /// tokens, and gen rate must be > 0 when output tokens exist (Kimi non-stream path).
    #[test]
    fn terminal_only_generation_mark_yields_nonzero_gen_window() {
        let d = Dashboard::new();
        let start_at = Instant::now() - Duration::from_secs(5);
        let mut start = base_event(RequestEventKind::Started);
        start.started_at = start_at;
        d.observe(start);

        let mut done = base_event(RequestEventKind::Completed);
        done.started_at = start_at;
        done.duration_ms = 5_000;
        done.output_tokens = 80;
        done.output_count = 1;
        done.diagnostics.first_chunk_ms = 2_000; // generation after ~2s TTFT
        done.mark_generation_start = true;
        done.phase = RequestPhase::Preparing; // publisher still stuck at Preparing
        d.observe(done);

        let req = &d.snapshot().recent[0];
        assert!(
            req.generation_started_at.is_some(),
            "terminal-only path must mark generation"
        );
        assert_eq!(
            req.generation_initial_output_tokens, 0,
            "baseline must be pre-final (0), not final output_tokens"
        );
        let gen_tokens = req
            .output_tokens
            .saturating_sub(req.generation_initial_output_tokens);
        assert_eq!(gen_tokens, 80, "gen tokens must equal final output");
        let gen_rate = req.generation_tokens_per_second();
        assert!(
            gen_rate > 0.0,
            "gen rate must be non-zero with tokens; got {gen_rate}"
        );
        // ~80 tokens over ~3s generation window → well above 0
        assert!(
            gen_rate > 10.0,
            "expected meaningful gen rate from first_chunk_ms reconstruction, got {gen_rate}"
        );
    }

    /// Terminal must not regress an already-advanced phase to Preparing.
    #[test]
    fn terminal_does_not_regress_phase_to_preparing() {
        let d = Dashboard::new();
        d.observe(base_event(RequestEventKind::Started));
        let mut upd = base_event(RequestEventKind::Updated);
        upd.phase = RequestPhase::Streaming;
        d.observe(upd);
        assert_eq!(d.snapshot().active[0].phase, RequestPhase::Streaming);

        let mut done = base_event(RequestEventKind::Completed);
        done.phase = RequestPhase::Preparing; // base_event default from many publishers
        d.observe(done);

        let recent = &d.snapshot().recent[0];
        assert_eq!(
            recent.phase,
            RequestPhase::Streaming,
            "terminal Preparing must not overwrite advanced Streaming phase"
        );
    }

    /// Zero-length generation window falls back to wall-clock (not 0.0).
    #[test]
    fn zero_length_generation_window_falls_back_to_wall_clock() {
        let d = Dashboard::new();
        let start_at = Instant::now() - Duration::from_secs(4);
        let mut start = base_event(RequestEventKind::Started);
        start.started_at = start_at;
        d.observe(start);
        // Force a zero-length generation window: mark at ended_at with baseline == final.
        {
            let mut state = lock_state(&d.inner);
            let req = state.active.get_mut("req\n1").expect("active");
            req.generation_started_at = Some(Instant::now());
            req.generation_initial_output_tokens = 0;
        }
        let mut done = base_event(RequestEventKind::Completed);
        done.started_at = start_at;
        done.duration_ms = 4_000;
        done.output_tokens = 40;
        done.output_count = 1;
        // Clear mark signals so maybe_mark_generation does not rewrite our zero window;
        // but terminal apply will set ended_at ≈ generation_started_at.
        done.mark_generation_start = false;
        done.diagnostics.first_chunk_ms = 0;
        done.output_tokens = 40;
        d.observe(done);

        // After complete, force generation_started_at == ended_at for a pure zero window.
        {
            let mut state = lock_state(&d.inner);
            let req = state.recent.front_mut().expect("recent");
            let ended = req.ended_at.expect("ended");
            req.generation_started_at = Some(ended);
            req.generation_initial_output_tokens = 0;
            req.output_tokens = 40;
        }
        let req = &d.snapshot().recent[0];
        let wall = req.tokens_per_second();
        let gen_rate = req.generation_tokens_per_second();
        assert!(wall > 0.0, "wall-clock rate should be positive, got {wall}");
        assert!(
            (gen_rate - wall).abs() < 1e-9,
            "zero-length gen window must fall back to wall-clock: gen={gen_rate} wall={wall}"
        );
    }

    #[test]
    fn recent_cap_default_is_at_least_200() {
        assert!(recent_cap_from_env() >= 200);
        let d = Dashboard::with_recent_cap(3);
        for i in 0..5 {
            let mut start = base_event(RequestEventKind::Started);
            start.request_id = format!("r{i}");
            d.observe(start);
            let mut done = base_event(RequestEventKind::Completed);
            done.request_id = format!("r{i}");
            d.observe(done);
        }
        assert_eq!(d.snapshot().recent.len(), 3);
    }

    #[test]
    fn session_output_buckets_recorded_on_complete() {
        let d = Dashboard::new();
        d.observe(base_event(RequestEventKind::Started));
        d.observe(base_event(RequestEventKind::Completed));
        let s = d.snapshot();
        assert!(
            !s.sessions[0].output_token_buckets.is_empty(),
            "expected per-session token bucket after complete"
        );
        assert_eq!(s.sessions[0].output_token_buckets.last().copied(), Some(20));
    }

    #[test]
    fn lifecycle_updates_bounded_state() {
        let d = Dashboard::new();
        d.observe(base_event(RequestEventKind::Started));
        assert_eq!(d.snapshot().active.len(), 1);
        d.observe(base_event(RequestEventKind::Completed));
        let s = d.snapshot();
        assert!(s.active.is_empty());
        assert_eq!(s.recent.len(), 1);
        assert_eq!(s.sessions[0].active, 0);
        assert_eq!(s.sessions[0].output_tokens, 20);
        assert!(!s.recent[0].id.contains('\n'));
        assert!(s.recent[0].mapped);
        assert!(s.recent[0].lite);
    }

    #[test]
    fn request_diagnostics_reach_active_recent_and_failure_records() {
        let dashboard = Dashboard::new();
        let diagnostics = crate::events::RequestDiagnostics {
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

    fn observe_turn(
        dashboard: &Dashboard,
        request_id: &str,
        fingerprint: &str,
        kind: RequestEventKind,
        output_count: u32,
    ) {
        let mut started = base_event(RequestEventKind::Started);
        started.request_id = request_id.into();
        started.diagnostics.request_fingerprint = fingerprint.into();
        dashboard.observe(started);

        let mut terminal = base_event(kind);
        terminal.request_id = request_id.into();
        terminal.output_count = output_count;
        terminal.diagnostics.request_fingerprint = fingerprint.into();
        if kind == RequestEventKind::Failed {
            terminal.failure_kind = Some(FailureKind::ProxyAssemble);
            terminal.error_type = "proxy_incomplete_output".into();
        }
        dashboard.observe(terminal);
    }

    #[test]
    fn matching_suspicious_fingerprints_are_retry_candidates() {
        let dashboard = Dashboard::new();
        observe_turn(
            &dashboard,
            "first",
            "same-fingerprint",
            RequestEventKind::Failed,
            0,
        );
        observe_turn(
            &dashboard,
            "second",
            "same-fingerprint",
            RequestEventKind::Completed,
            1,
        );

        let snapshot = dashboard.snapshot();
        assert!(
            snapshot
                .recent
                .iter()
                .all(|request| request.retry_candidate)
        );
        assert!(snapshot.failures[0].retry_candidate);
    }

    #[test]
    fn successful_repeats_and_different_fingerprints_are_not_retry_candidates() {
        let dashboard = Dashboard::new();
        for request_id in ["success-a", "success-b"] {
            observe_turn(
                &dashboard,
                request_id,
                "successful-repeat",
                RequestEventKind::Completed,
                1,
            );
        }
        observe_turn(
            &dashboard,
            "different",
            "different-fingerprint",
            RequestEventKind::Failed,
            0,
        );

        assert!(
            dashboard
                .snapshot()
                .recent
                .iter()
                .all(|request| !request.retry_candidate)
        );
    }

    #[test]
    fn matching_suspicious_fingerprints_after_window_are_not_retry_candidates() {
        let dashboard = Dashboard::new();
        let old_start = Instant::now() - Duration::from_secs(31);

        let mut first_started = base_event(RequestEventKind::Started);
        first_started.request_id = "old".into();
        first_started.started_at = old_start;
        first_started.diagnostics.request_fingerprint = "same-fingerprint".into();
        dashboard.observe(first_started);
        let mut first_failed = base_event(RequestEventKind::Failed);
        first_failed.request_id = "old".into();
        first_failed.failure_kind = Some(FailureKind::ProxyAssemble);
        first_failed.diagnostics.request_fingerprint = "same-fingerprint".into();
        dashboard.observe(first_failed);

        let mut second_started = base_event(RequestEventKind::Started);
        second_started.request_id = "new".into();
        second_started.started_at = Instant::now();
        second_started.diagnostics.request_fingerprint = "same-fingerprint".into();
        dashboard.observe(second_started);
        let mut second_failed = base_event(RequestEventKind::Failed);
        second_failed.request_id = "new".into();
        second_failed.failure_kind = Some(FailureKind::ProxyAssemble);
        second_failed.diagnostics.request_fingerprint = "same-fingerprint".into();
        dashboard.observe(second_failed);

        assert!(
            dashboard
                .snapshot()
                .recent
                .iter()
                .all(|request| !request.retry_candidate)
        );
    }

    #[test]
    fn aggregates_usage_with_weighted_cache_read_ratio() {
        let d = Dashboard::new();
        for (request_id, session_id, usage) in [
            (
                "large",
                "session-a",
                TokenUsage {
                    input_tokens: 1_000,
                    cached_input_tokens: 900,
                    cache_write_tokens: 50,
                    output_tokens: 20,
                },
            ),
            (
                "small",
                "session-a",
                TokenUsage {
                    input_tokens: 10,
                    cached_input_tokens: 0,
                    cache_write_tokens: 4,
                    output_tokens: 2,
                },
            ),
            ("zero", "session-b", TokenUsage::default()),
        ] {
            let mut start = base_event(RequestEventKind::Started);
            start.request_id = request_id.into();
            start.session_id = session_id.into();
            d.observe(start);
            let mut completed = base_event(RequestEventKind::Completed);
            completed.request_id = request_id.into();
            completed.session_id = session_id.into();
            completed.usage = Some(usage);
            completed.output_tokens = usage.output_tokens;
            d.observe(completed);
        }

        let snapshot = d.snapshot();
        assert_eq!(snapshot.usage_requests, 3);
        assert_eq!(snapshot.input_tokens, 1_010);
        assert_eq!(snapshot.cached_input_tokens, 900);
        assert_eq!(snapshot.cache_write_tokens, 54);
        assert_eq!(snapshot.fresh_input_tokens, 56);
        assert!((snapshot.cache_read_ratio().unwrap() - 900.0 / 1_010.0).abs() < 1e-12);
        assert_eq!(snapshot.recent[0].usage, Some(TokenUsage::default()));

        let session_a = snapshot
            .sessions
            .iter()
            .find(|session| session.id == "session-a")
            .unwrap();
        assert_eq!(session_a.usage_requests, 2);
        assert_eq!(session_a.input_tokens, 1_010);
        assert_eq!(session_a.cached_input_tokens, 900);
        assert_eq!(session_a.cache_write_tokens, 54);
        assert_eq!(session_a.fresh_input_tokens, 56);
        // Weighted aggregate is ~89%, not the 45% average of per-request percentages.
        assert!((session_a.cache_read_ratio().unwrap() - 900.0 / 1_010.0).abs() < 1e-12);

        let session_b = snapshot
            .sessions
            .iter()
            .find(|session| session.id == "session-b")
            .unwrap();
        assert_eq!(session_b.cache_read_ratio(), Some(0.0));
    }

    #[test]
    fn missing_usage_is_not_counted_as_zero_usage() {
        let d = Dashboard::new();
        d.observe(base_event(RequestEventKind::Started));
        d.observe(base_event(RequestEventKind::Completed));
        let snapshot = d.snapshot();
        assert_eq!(snapshot.usage_requests, 0);
        assert_eq!(snapshot.cache_read_ratio(), None);
        assert_eq!(snapshot.sessions[0].cache_read_ratio(), None);
        assert_eq!(snapshot.recent[0].usage, None);
    }

    #[test]
    fn readme_documents_default_failure_ring_configuration() {
        let readme = include_str!("../README.md");
        let d = Dashboard::new();
        assert_eq!(lock_state(&d.inner).failure_cap, DEFAULT_FAILURE_CAP);
        assert_eq!(lock_state(&d.inner).recent_cap, DEFAULT_RECENT_CAP);
        assert!(readme.contains("`GROK_BUILD_PROXY_FAILURE_CAP`"));
        assert!(readme.contains("`GROK_BUILD_PROXY_RECENT_CAP`"));
        assert!(readme.contains("| 200 | Failure-ring") || readme.contains("default | 200"));
        assert!(readme.contains("separate rings"));
    }

    #[test]
    fn failure_ring_respects_cap() {
        let d = Dashboard::with_failure_cap(5);
        for i in 0..12 {
            let mut start = base_event(RequestEventKind::Started);
            start.request_id = format!("req-{i}");
            d.observe(start);
            let mut fail = base_event(RequestEventKind::Failed);
            fail.request_id = format!("req-{i}");
            fail.failure_kind = Some(FailureKind::UpstreamHttp);
            fail.error_type = "upstream_http".into();
            fail.error = format!("err {i}");
            fail.status_code = 502;
            d.observe(fail);
        }
        let s = d.snapshot();
        assert_eq!(s.failures.len(), 5);
        assert_eq!(s.errors.len(), 12); // recent-errors ring uses RECENT_CAP (50)
        assert_eq!(s.failures[0].request_id, "req-11");
        assert_eq!(s.failures[4].request_id, "req-7");
    }

    #[test]
    fn classifies_proxy_assemble_failure_record() {
        let d = Dashboard::new();
        d.observe(base_event(RequestEventKind::Started));
        let mut fail = base_event(RequestEventKind::Failed);
        fail.failure_kind = Some(FailureKind::ProxyAssemble);
        fail.error_type = "proxy_incomplete_output".into();
        fail.error = "could not assemble".into();
        fail.status_code = 200;
        fail.response_id = "resp_x".into();
        fail.output_count = 1;
        fail.capture_bytes = 4096;
        d.observe(fail);
        let s = d.snapshot();
        assert_eq!(s.failures.len(), 1);
        let f = &s.failures[0];
        assert_eq!(f.kind, FailureKind::ProxyAssemble);
        assert_eq!(f.error_type, "proxy_incomplete_output");
        assert_eq!(f.status_code, 200);
        assert_eq!(f.session_failure_index, 1);
        assert_eq!(f.response_id, "resp_x");
        assert_eq!(
            s.sessions[0].last_failure_kind,
            Some(FailureKind::ProxyAssemble)
        );
        assert_eq!(s.sessions[0].errors, 1);
    }

    #[test]
    fn auth_retry_attempt_field_recorded() {
        let d = Dashboard::new();
        let mut start = base_event(RequestEventKind::Started);
        start.auth_retried = true;
        start.attempt = 2;
        d.observe(start);
        let mut fail = base_event(RequestEventKind::Failed);
        fail.auth_retried = true;
        fail.attempt = 2;
        fail.failure_kind = Some(FailureKind::AuthRetryFailed);
        fail.error_type = "auth_retry_failed".into();
        fail.status_code = 401;
        d.observe(fail);
        let s = d.snapshot();
        assert_eq!(s.failures[0].attempt, 2);
        assert!(s.failures[0].auth_retried);
        assert_eq!(s.failures[0].kind, FailureKind::AuthRetryFailed);
        assert_eq!(s.recent[0].attempt, 2);
    }

    #[test]
    fn started_reobserve_updates_active_attempt() {
        let d = Dashboard::new();
        d.observe(base_event(RequestEventKind::Started));
        assert_eq!(d.snapshot().active[0].attempt, 1);
        assert!(!d.snapshot().active[0].auth_retried);
        let mut retry = base_event(RequestEventKind::Started);
        retry.auth_retried = true;
        retry.attempt = 2;
        d.observe(retry);
        let s = d.snapshot();
        assert_eq!(s.active.len(), 1);
        assert_eq!(s.active[0].attempt, 2);
        assert!(s.active[0].auth_retried);
        assert_eq!(s.sessions[0].requests, 1); // not double-counted
        assert_eq!(s.sessions[0].active, 1);
    }

    #[test]
    fn started_reobserve_does_not_regress_phase_from_upstream() {
        // Auth-retry re-emits Started with phase=Preparing; hang diagnosis must keep Upstream.
        let d = Dashboard::new();
        d.observe(base_event(RequestEventKind::Started));
        let mut upstream = base_event(RequestEventKind::Updated);
        upstream.phase = RequestPhase::Upstream;
        d.observe(upstream);
        assert_eq!(d.snapshot().active[0].phase, RequestPhase::Upstream);

        let mut retry = base_event(RequestEventKind::Started);
        retry.auth_retried = true;
        retry.attempt = 2;
        retry.phase = RequestPhase::Preparing; // base_event default — must not overwrite
        d.observe(retry);

        let active = &d.snapshot().active[0];
        assert_eq!(active.phase, RequestPhase::Upstream);
        assert_eq!(active.attempt, 2);
        assert!(active.auth_retried);
    }

    #[test]
    fn push_tok_s_sample_rolls_forward_at_capacity() {
        let d = Dashboard::new();
        for i in 0..=METRICS_CAP {
            d.push_tok_s_sample(i as f64);
        }
        let samples = d.snapshot().metrics_tok_s;
        assert_eq!(samples.len(), METRICS_CAP);
        assert_eq!(samples.first(), Some(&1.0));
        assert_eq!(samples.last(), Some(&(METRICS_CAP as f64)));
    }

    #[test]
    fn completion_metrics_roll_forward_at_capacity() {
        let d = Dashboard::new();
        for i in 0..=METRICS_CAP {
            let mut start = base_event(RequestEventKind::Started);
            start.request_id = format!("metric-{i}");
            d.observe(start);
            let mut done = base_event(if i == METRICS_CAP {
                RequestEventKind::Failed
            } else {
                RequestEventKind::Completed
            });
            done.request_id = format!("metric-{i}");
            d.observe(done);
        }
        let samples = d.snapshot().metrics_completed;
        assert_eq!(samples.len(), METRICS_CAP);
        assert_eq!(samples.first(), Some(&1.0));
        assert_eq!(samples.last(), Some(&0.0));
    }

    #[test]
    fn full_session_keys_do_not_collide_after_display_truncation() {
        let d = Dashboard::new();
        let prefix = "x".repeat(256);
        for suffix in ["a", "b"] {
            let mut start = base_event(RequestEventKind::Started);
            start.request_id = format!("req-{suffix}");
            start.session_id = format!("{prefix}{suffix}");
            d.observe(start);
        }
        let snapshot = d.snapshot();
        assert_eq!(snapshot.sessions.len(), 2);
        assert_eq!(snapshot.active.len(), 2);
        assert_ne!(snapshot.sessions[0].id, snapshot.sessions[1].id);
        assert_ne!(snapshot.active[0].session_id, snapshot.active[1].session_id);
    }

    #[test]
    fn completion_does_not_push_tok_s_ring() {
        let d = Dashboard::new();
        d.observe(base_event(RequestEventKind::Started));
        d.observe(base_event(RequestEventKind::Completed));
        assert!(
            d.snapshot().metrics_tok_s.is_empty(),
            "tok/s ring is 1 Hz fleet avg, not per-request"
        );
        assert_eq!(d.snapshot().metrics_completed, vec![1.0]);
    }

    #[test]
    fn session_context_keeps_latest_non_empty_values() {
        let d = Dashboard::new();
        d.observe(base_event(RequestEventKind::Started));
        d.observe_session_context("session", "first prompt", "/tmp/first");
        d.observe_session_context("session", "", "/tmp/second");
        let snapshot = d.snapshot();
        assert_eq!(snapshot.sessions[0].last_prompt, "first prompt");
        assert_eq!(snapshot.sessions[0].cwd, "/tmp/second");
    }

    #[test]
    fn failures_for_report_filters_kind() {
        let d = Dashboard::new();
        for (i, kind) in [
            FailureKind::ProxyAssemble,
            FailureKind::UpstreamHttp,
            FailureKind::ProxyAssemble,
        ]
        .into_iter()
        .enumerate()
        {
            let mut start = base_event(RequestEventKind::Started);
            start.request_id = format!("r{i}");
            d.observe(start);
            let mut fail = base_event(RequestEventKind::Failed);
            fail.request_id = format!("r{i}");
            fail.failure_kind = Some(kind);
            d.observe(fail);
        }
        assert_eq!(
            d.failures_for_report(Some(FailureKind::ProxyAssemble))
                .len(),
            2
        );
        assert_eq!(d.failures_for_report(None).len(), 3);
    }
}
