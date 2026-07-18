//! Structured monitor store: sessions, active/recent turns, failure ring, metrics samples.

use crate::events::{
    FailureKind, Observer, RequestEvent, RequestEventKind, TokenUsage, sanitize, sanitize_id,
};
use chrono::{DateTime, Utc};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    env,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const DEFAULT_FAILURE_CAP: usize = 200;
const RECENT_CAP: usize = 50;
const METRICS_CAP: usize = 120;

#[derive(Clone, Debug)]
pub struct Request {
    pub id: String,
    pub session_id: String,
    pub requested_model: String,
    pub model: String,
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
}

impl Request {
    pub fn duration(&self) -> Duration {
        self.ended_at
            .unwrap_or_else(Instant::now)
            .saturating_duration_since(self.started_at)
    }
    pub fn tokens_per_second(&self) -> f64 {
        let seconds = self.duration().as_secs_f64();
        if seconds > 0.0 {
            self.output_tokens as f64 / seconds
        } else {
            0.0
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
    /// Latest non-empty user prompt preview observed for this session.
    pub last_prompt: String,
    /// Latest workspace/current-working-directory path observed for this session.
    pub cwd: String,
    pub errors: u64,
    pub last_failure_kind: Option<FailureKind>,
    pub updated_at: Option<DateTime<Utc>>,
    /// Sum of completed-turn durations (seconds) used for lifetime tok/s.
    pub(crate) sample_seconds: f64,
}

impl Session {
    pub fn tokens_per_second(&self) -> f64 {
        if self.sample_seconds > 0.0 {
            self.output_tokens as f64 / self.sample_seconds
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

struct State {
    sessions: HashMap<String, Session>,
    active: HashMap<String, Request>,
    recent: VecDeque<Request>,
    errors: VecDeque<Request>,
    failures: VecDeque<FailureRecord>,
    completed: HashSet<String>,
    session_failure_counts: HashMap<String, u32>,
    failure_cap: usize,
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
                if let Some(active) = state.active.get_mut(&event.request_id) {
                    active.auth_retried = event.auth_retried;
                    active.attempt = event.attempt.max(1);
                    active.mapped = event.mapped;
                    active.lite = event.lite;
                    active.fast = event.fast;
                    if let Some(session) = state.sessions.get_mut(&event.session_id) {
                        session.updated_at = Some(Utc::now());
                    }
                    return;
                }
                state.active.insert(
                    event.request_id.clone(),
                    Request {
                        id: request_id,
                        session_id: session_id.clone(),
                        requested_model: sanitize(&event.requested_model),
                        model: sanitize(&event.model),
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
                    },
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
                session.updated_at = Some(Utc::now());
            }
            RequestEventKind::Completed | RequestEventKind::Failed => {
                if !state.completed.insert(event.request_id.clone()) {
                    return;
                }
                let mut request = state.active.remove(&event.request_id).unwrap_or(Request {
                    id: request_id,
                    session_id: session_id.clone(),
                    requested_model: sanitize(&event.requested_model),
                    model: sanitize(&event.model),
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
                });
                request.status = event.status_code;
                request.error = sanitize(&event.error);
                request.error_type = sanitize(&event.error_type);
                request.failure_kind = event.failure_kind;
                request.usage = event.usage;
                request.output_tokens = event.output_tokens;
                request.ended_at = Some(Instant::now());
                request.duration_ms = if event.duration_ms > 0 {
                    event.duration_ms
                } else {
                    request.duration().as_millis() as u64
                };
                request.response_id = sanitize(&event.response_id);
                request.mapped = event.mapped;
                request.lite = event.lite;
                request.fast = event.fast;
                request.auth_retried = event.auth_retried;
                request.attempt = event.attempt.max(1);
                request.output_count = event.output_count;
                request.capture_bytes = event.capture_bytes;

                let duration_secs = request.duration().as_secs_f64();
                let failed = event.kind == RequestEventKind::Failed;

                state.recent.push_front(request.clone());
                state.recent.truncate(RECENT_CAP);

                if failed {
                    state.errors.push_front(request.clone());
                    state.errors.truncate(RECENT_CAP);

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
                    };
                    state.failures.push_front(record);
                    let cap = state.failure_cap;
                    state.failures.truncate(cap);
                }

                // Rolling outcome samples for fail%/done sparklines (tok/s is 1 Hz fleet avg).
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
                if failed {
                    session.errors += 1;
                    session.last_failure_kind = event.failure_kind.or(Some(FailureKind::Unknown));
                }
                session.updated_at = Some(Utc::now());
                if state.completed.len() > 200 {
                    state.completed.clear();
                }
            }
        }
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
        }
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
