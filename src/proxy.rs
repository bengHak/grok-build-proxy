use crate::{
    auth::CredentialProvider,
    catalog::{Catalog, ReasoningCapability},
    events::{classify_stream_end, parse_capture_diagnostics, sanitize},
    modelmap::ModelMap,
};
use anyhow::{Context, Result, anyhow, bail};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::any,
};
use futures_util::StreamExt;
use serde_json::{Map, Value, json};
use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    sync::Arc,
};
use tracing::info;

pub use crate::events::{FailureKind, Observer, RequestEvent, RequestEventKind};

pub const DEFAULT_CODEX_COMPATIBILITY_VERSION: &str = "0.144.0";
pub const DEFAULT_MAX_BODY_BYTES: usize = 64 << 20;

#[derive(Clone)]
pub struct ProxyConfig {
    pub upstream_url: String,
    pub credentials: Arc<dyn CredentialProvider>,
    pub catalog: Catalog,
    pub model_map: ModelMap,
    pub client: reqwest::Client,
    pub client_token: String,
    pub version: String,
    pub compatibility_version: String,
    pub responses_compat: CompatMode,
    pub observer: Option<Arc<dyn Observer>>,
    pub max_body_bytes: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompatMode {
    Full,
    Text,
    Off,
}
impl CompatMode {
    pub fn from_env() -> Result<Self> {
        match std::env::var("GROK_BUILD_PROXY_RESPONSES_COMPAT")
            .unwrap_or_else(|_| "full".into())
            .to_lowercase()
            .as_str()
        {
            "full" => Ok(Self::Full),
            "text" | "legacy" => Ok(Self::Text),
            "off" | "false" | "0" => Ok(Self::Off),
            _ => Ok(Self::Full),
        }
    }
}
#[derive(Clone)]
struct AppState(Arc<ProxyConfig>);

#[derive(Clone, Debug, PartialEq, Eq)]
struct UpstreamIdentity {
    /// Current Grok Build conversation/thread identity.
    thread_id: String,
    /// Root session shared by related threads.
    session_id: String,
    /// Stable, Responses-compatible prompt-cache routing key.
    cache_key: String,
}

impl UpstreamIdentity {
    fn from_request(headers: &HeaderMap, body: &[u8], fallback: &str) -> Self {
        let thread_id = first_valid_header(
            headers,
            &[
                "x-grok-session-id",
                "x-grok-conv-id",
                "x-grok-req-id",
                "x-request-id",
            ],
        )
        .unwrap_or(fallback)
        .to_owned();
        let session_id = first_valid_header(headers, &["x-grok-cache-lineage-id"])
            .unwrap_or(&thread_id)
            .to_owned();

        // An explicit Responses API key controls only prompt-cache routing. Forks may
        // otherwise share their root session's cache namespace without collapsing threads.
        let explicit_cache_key = serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|value| {
                value
                    .get("prompt_cache_key")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .filter(|value| valid_cache_key(value));
        let cache_key = explicit_cache_key.unwrap_or_else(|| bounded_cache_key(&session_id));

        Self {
            thread_id,
            session_id,
            cache_key,
        }
    }
}

fn first_valid_header<'a>(headers: &'a HeaderMap, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .filter_map(|key| headers.get(*key)?.to_str().ok())
        .find(|value| valid_header(value))
}

fn valid_cache_key(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64 && !value.chars().any(char::is_control)
}

fn bounded_cache_key(value: &str) -> String {
    if valid_cache_key(value) {
        value.to_owned()
    } else {
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, value.as_bytes()).to_string()
    }
}

pub fn router(config: ProxyConfig) -> Result<Router> {
    url::Url::parse(&config.upstream_url).context("invalid upstream URL")?;
    let limit = config.max_body_bytes;
    let state = AppState(Arc::new(config));
    Ok(Router::new()
        .route("/", any(health))
        .route("/healthz", any(health))
        .route("/readyz", any(ready))
        .route("/v1/models", any(models))
        .route("/models", any(models))
        .route("/v1/responses", any(responses))
        .route("/responses", any(responses))
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(limit))
        .with_state(state))
}
fn error(status: StatusCode, kind: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(json!({"error":{"message":message.into(),"type":kind}})),
    )
        .into_response()
}
fn authorized(cfg: &ProxyConfig, headers: &HeaderMap) -> bool {
    if cfg.client_token.trim().is_empty() {
        return true;
    }
    let want = format!("Bearer {}", cfg.client_token.trim());
    headers
        .get(header::AUTHORIZATION)
        .and_then(|x| x.to_str().ok())
        .is_some_and(|x| constant_time_eq(x.trim().as_bytes(), want.as_bytes()))
}
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |x, (a, b)| x | (a ^ b)) == 0
}
async fn health(State(s): State<AppState>, method: Method) -> Response {
    if method != Method::GET {
        return error(
            StatusCode::METHOD_NOT_ALLOWED,
            "invalid_request_error",
            "method not allowed",
        );
    };
    Json(json!({"ok":true,"service":"grok-build-proxy","version":s.0.version,"model_substitutions":s.0.model_map.len()})).into_response()
}
async fn ready(State(s): State<AppState>, method: Method, headers: HeaderMap) -> Response {
    if !authorized(&s.0, &headers) {
        return unauthorized();
    }
    if method != Method::GET {
        return error(
            StatusCode::METHOD_NOT_ALLOWED,
            "invalid_request_error",
            "method not allowed",
        );
    };
    match s.0.credentials.get(false).await {
        Ok(_) => Json(json!({"ok":true,"auth":"ready"})).into_response(),
        Err(e) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "authentication_error",
            e.to_string(),
        ),
    }
}
fn unauthorized() -> Response {
    let mut r = error(
        StatusCode::UNAUTHORIZED,
        "authentication_error",
        "invalid proxy bearer token",
    );
    r.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer realm=\"grok-build-proxy\""),
    );
    r
}
async fn not_found() -> Response {
    error(
        StatusCode::NOT_FOUND,
        "not_found_error",
        "endpoint not found",
    )
}

async fn models(State(s): State<AppState>, method: Method, headers: HeaderMap) -> Response {
    if !authorized(&s.0, &headers) {
        return unauthorized();
    }
    if method != Method::GET {
        return error(
            StatusCode::METHOD_NOT_ALLOWED,
            "invalid_request_error",
            "method not allowed",
        );
    }
    #[derive(Clone)]
    struct Route {
        id: String,
        target: String,
        name: String,
        description: String,
        context: u64,
        fast: bool,
        reasoning: Option<ReasoningCapability>,
    }
    let mut routes = Vec::new();
    let mut seen = HashSet::new();
    {
        let mut push = |r: Route| {
            if !r.id.is_empty() && seen.insert(r.id.clone()) {
                routes.push(r)
            }
        };
        for entry in s.0.model_map.entries() {
            let resolved = s.0.model_map.resolve(&entry.source);
            let (m, _) = s.0.catalog.lookup(&resolved.model);
            let effective = resolved.effective_model_id();
            push(Route {
                id: entry.source.clone(),
                target: resolved.model,
                name: format!(
                    "{} via {}{}",
                    entry.source,
                    m.display_name,
                    if resolved.fast { " (Fast)" } else { "" }
                ),
                description: format!(
                    "Maps {} to {} through ChatGPT Codex.",
                    entry.source, effective
                ),
                context: m.context_window,
                fast: resolved.fast,
                reasoning: m.reasoning,
            });
        }
        for m in s.0.catalog.models() {
            push(Route {
                id: m.id.clone(),
                target: m.id,
                name: m.display_name,
                description: m.description,
                context: m.context_window,
                fast: false,
                reasoning: m.reasoning,
            });
        }
    }
    let base = routes.clone();
    for r in base {
        if !r.fast && !r.id.ends_with("-fast") && crate::catalog::supports_fast(&r.target) {
            let route = Route {
                id: format!("{}-fast", r.id),
                target: r.target,
                name: format!("{} (Fast)", r.name),
                description: r.description,
                context: r.context,
                fast: true,
                reasoning: r.reasoning,
            };
            if seen.insert(route.id.clone()) {
                routes.push(route);
            }
        }
    }
    let data:Vec<Value>=routes.into_iter().map(|r|{
        let mut value=json!({"id":r.id,"object":"model","owned_by":"openai-codex","name":r.name,"description":r.description,"context_window":r.context,"api_backend":"responses"});
        let object=value.as_object_mut().unwrap();
        if r.id!=r.target||r.fast { object.insert("target_model".into(),format!("{}{}",r.target,if r.fast{"-fast"}else{""}).into()); }

        if r.fast { object.insert("service_tier".into(),"priority".into()); }

        if let Some(capability)=r.reasoning { object.insert("supports_reasoning_effort".into(),true.into());object.insert("reasoning_effort".into(),capability.default_effort.into());object.insert("reasoning_efforts".into(),serde_json::to_value(capability.efforts).unwrap()); }
        value
    }).collect();
    Json(json!({"object":"list","data":data})).into_response()
}

#[derive(Debug)]
pub struct TransformedRequest {
    pub body: Vec<u8>,
    pub requested_model: String,
    pub model: String,
    pub mapped: bool,
    pub lite: bool,
    pub fast: bool,
    pub stream: bool,
}
pub fn transform_request(
    raw: &[u8],
    catalog: &Catalog,
    mappings: &ModelMap,
) -> Result<TransformedRequest> {
    if raw.iter().all(u8::is_ascii_whitespace) {
        bail!("request body is empty")
    }
    let mut body: Value = serde_json::from_slice(raw).context("invalid JSON request")?;
    let object = body
        .as_object_mut()
        .ok_or_else(|| anyhow!("request body must be a JSON object"))?;
    let requested = object
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    if requested.is_empty() {
        bail!("model is required")
    };
    let resolution = mappings.resolve(&requested);
    let (model, _) = catalog.lookup(&resolution.model);
    object.insert("model".into(), resolution.model.clone().into());
    object.insert("store".into(), false.into());
    let stream = object
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if !object.get("stream").is_some_and(Value::is_boolean) {
        object.insert("stream".into(), true.into());
    }
    if resolution.fast {
        object.entry("service_tier").or_insert("priority".into());
    }
    if model.responses_lite {
        apply_responses_lite(object)
    } else {
        object.entry("parallel_tool_calls").or_insert(true.into());
    }
    Ok(TransformedRequest {
        body: serde_json::to_vec(&body)?,
        requested_model: requested,
        model: resolution.model,
        mapped: resolution.mapped,
        lite: model.responses_lite,
        fast: resolution.fast,
        stream,
    })
}
fn apply_responses_lite(body: &mut Map<String, Value>) {
    body.insert("parallel_tool_calls".into(), false.into());
    for (key, name, value) in [
        (
            "client_metadata",
            "ws_request_header_x_openai_internal_codex_responses_lite",
            "true",
        ),
        ("reasoning", "context", "all_turns"),
        ("text", "verbosity", "low"),
    ] {
        let o = body.entry(key).or_insert_with(|| json!({}));
        if !o.is_object() {
            *o = json!({})
        }
        if key == "text" {
            o.as_object_mut()
                .unwrap()
                .entry(name)
                .or_insert(value.into());
        } else {
            o.as_object_mut().unwrap().insert(name.into(), value.into());
        }
    }
    let input = body.remove("input");
    let mut normalized = match input {
        Some(Value::Array(a)) => a,
        Some(Value::String(s)) if !s.is_empty() => {
            vec![json!({"type":"message","role":"user","content":[{"type":"input_text","text":s}]})]
        }
        Some(Value::Null) | None => vec![],
        Some(v) => vec![v],
    };
    let mut prefix = Vec::new();
    if let Some(Value::Array(tools)) = body.remove("tools")
        && !tools.is_empty()
    {
        prefix.push(json!({"type":"additional_tools","role":"developer","tools":tools}));
    }
    if let Some(Value::String(i)) = body.remove("instructions")
        && !i.trim().is_empty()
    {
        prefix.push(
            json!({"type":"message","role":"developer","content":[{"type":"input_text","text":i}]}),
        );
    }
    prefix.append(&mut normalized);
    body.insert("input".into(), prefix.into());
}

async fn responses(State(s): State<AppState>, request: Request) -> Response {
    let request_id = uuid::Uuid::new_v4().to_string();
    if !authorized(&s.0, request.headers()) {
        return with_request_id(unauthorized(), &request_id);
    }
    if request.method() != Method::POST {
        return with_request_id(
            error(
                StatusCode::METHOD_NOT_ALLOWED,
                "invalid_request_error",
                "method not allowed",
            ),
            &request_id,
        );
    }
    let incoming_headers = request.headers().clone();
    let body = match axum::body::to_bytes(request.into_body(), s.0.max_body_bytes).await {
        Ok(b) => b,
        Err(_) => {
            return with_request_id(
                error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "invalid_request_error",
                    "request body exceeds proxy limit",
                ),
                &request_id,
            );
        }
    };
    let transformed = match transform_request(&body, &s.0.catalog, &s.0.model_map) {
        Ok(v) => v,
        Err(e) => {
            return with_request_id(
                error(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    e.to_string(),
                ),
                &request_id,
            );
        }
    };
    let identity =
        UpstreamIdentity::from_request(&incoming_headers, &transformed.body, &request_id);
    let session_id = identity.thread_id.clone();
    let started = std::time::Instant::now();
    let mut base_event = RequestEvent {
        kind: RequestEventKind::Started,
        request_id: request_id.clone(),
        session_id: session_id.clone(),
        requested_model: transformed.requested_model.clone(),
        model: transformed.model.clone(),
        status_code: 0,
        output_tokens: 0,
        error: String::new(),
        started_at: started,
        duration_ms: 0,
        failure_kind: None,
        error_type: String::new(),
        response_id: String::new(),
        mapped: transformed.mapped,
        lite: transformed.lite,
        fast: transformed.fast,
        auth_retried: false,
        attempt: 1,
        output_count: 0,
        capture_bytes: 0,
    };
    if let Some(observer) = &s.0.observer {
        observer.observe(base_event.clone());
    }
    let mut upstream =
        match send_upstream(&s.0, &transformed, &incoming_headers, &identity, false).await {
            Ok(r) => r,
            Err(e) => {
                observe_failure(
                    &s.0,
                    &base_event,
                    FailureKind::UpstreamConnect,
                    "upstream_connect",
                    e.to_string(),
                    StatusCode::BAD_GATEWAY,
                );
                return with_request_id(
                    error(StatusCode::BAD_GATEWAY, "upstream_error", e.to_string()),
                    &request_id,
                );
            }
        };
    if upstream.status() == reqwest::StatusCode::UNAUTHORIZED {
        let _ = upstream.bytes().await;
        base_event.auth_retried = true;
        base_event.attempt = 2;
        // Refresh active turn in the monitor so TUI shows attempt=2 while force-refresh runs.
        if let Some(observer) = &s.0.observer {
            observer.observe(base_event.clone());
        }
        upstream = match send_upstream(&s.0, &transformed, &incoming_headers, &identity, true).await
        {
            Ok(r) => r,
            Err(e) => {
                observe_failure(
                    &s.0,
                    &base_event,
                    FailureKind::AuthRetryFailed,
                    "auth_retry_failed",
                    e.to_string(),
                    StatusCode::BAD_GATEWAY,
                );
                return with_request_id(
                    error(StatusCode::BAD_GATEWAY, "upstream_error", e.to_string()),
                    &request_id,
                );
            }
        };
        // Still-401 classification is handled in observe_stream_end (auth_retried + 401).
    }
    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let headers = upstream.headers().clone();
    let is_sse = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().contains("text/event-stream"));
    let normalize_stream = transformed.lite
        && s.0.responses_compat != CompatMode::Off
        && status.is_success()
        && (is_sse || transformed.stream);
    let observer = s.0.observer.clone();
    let body = if normalize_stream {
        let mut source = upstream.bytes_stream();
        let mut normalizer = crate::sse::StreamNormalizer::new(
            s.0.responses_compat,
            &transformed.model,
            &request_id,
        );
        let observer = observer.clone();
        let event = base_event.clone();
        Body::from_stream(async_stream::stream! {
            let mut guard = StreamObserveGuard::new(observer, event, status);
            while let Some(chunk) = source.next().await {
                match chunk {
                    Ok(chunk) => {
                        let output = normalizer.push(&chunk);
                        guard.capture(&output);
                        if !output.is_empty() {
                            yield Ok::<Bytes, std::io::Error>(Bytes::from(output));
                        }
                    }
                    Err(error) => {
                        let message = error.to_string();
                        guard.finish(Some(message.clone()));
                        yield Err(std::io::Error::other(message));
                        return;
                    }
                }
            }
            // Observe before final yield so client drop at the last chunk cannot force StreamIo.
            let output = normalizer.finish();
            guard.capture(&output);
            guard.finish(None);
            if !output.is_empty() {
                yield Ok::<Bytes, std::io::Error>(Bytes::from(output));
            }
        })
    } else {
        let mut source = upstream.bytes_stream();
        let event = base_event.clone();
        Body::from_stream(async_stream::stream! {
            let mut guard = StreamObserveGuard::new(observer, event, status);
            while let Some(chunk) = source.next().await {
                match chunk {
                    Ok(chunk) => {
                        guard.capture(&chunk);
                        yield Ok::<Bytes, std::io::Error>(chunk);
                    }
                    Err(error) => {
                        let message = error.to_string();
                        guard.finish(Some(message.clone()));
                        yield Err(std::io::Error::other(message));
                        return;
                    }
                }
            }
            // Upstream EOF: observe immediately (no trailing yield after this).
            guard.finish(None);
        })
    };
    let mut response = Response::builder().status(status).body(body).unwrap();
    copy_headers(response.headers_mut(), &headers);
    if normalize_stream {
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream; charset=utf-8"),
        );
        response.headers_mut().remove(header::CONTENT_ENCODING);
    }
    response.headers_mut().insert(
        "x-grok-build-proxy-version",
        HeaderValue::from_str(&s.0.version).unwrap_or(HeaderValue::from_static("dev")),
    );
    let response = with_request_id(response, &request_id);
    info!(
        request_id,
        requested_model = transformed.requested_model,
        model = transformed.model,
        mapped = transformed.mapped,
        responses_lite = transformed.lite,
        fast = transformed.fast,
        status = status.as_u16(),
        duration_ms = started.elapsed().as_millis(),
        "request complete"
    );
    response
}
fn capture_tail(buffer: &mut Vec<u8>, chunk: &[u8]) {
    const LIMIT: usize = 256 << 10;
    if chunk.len() >= LIMIT {
        buffer.clear();
        buffer.extend_from_slice(&chunk[chunk.len() - LIMIT..]);
    } else {
        let excess = buffer
            .len()
            .saturating_add(chunk.len())
            .saturating_sub(LIMIT);
        if excess > 0 {
            buffer.drain(..excess);
        }
        buffer.extend_from_slice(chunk);
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ObservedUsage {
    input_tokens: u64,
    cached_input_tokens: u64,
    cache_write_tokens: u64,
    output_tokens: u64,
}

#[derive(Default)]
struct UsageAccumulator {
    pending: Vec<u8>,
    observed: ObservedUsage,
}

impl UsageAccumulator {
    fn push(&mut self, chunk: &[u8]) {
        self.pending.extend_from_slice(chunk);
        while let Some((position, separator_len)) = crate::sse::frame_boundary(&self.pending) {
            let frame: Vec<u8> = self.pending.drain(..position + separator_len).collect();
            if let Some(usage) = usage_from_frame(&frame) {
                self.observed = usage;
            }
        }
    }

    fn finish(&mut self) -> ObservedUsage {
        if let Some(usage) = usage_from_frame(&self.pending) {
            self.observed = usage;
        }
        self.pending.clear();
        self.observed
    }
}

#[cfg(test)]
fn observed_usage(bytes: &[u8]) -> ObservedUsage {
    let mut accumulator = UsageAccumulator::default();
    accumulator.push(bytes);
    accumulator.finish()
}

fn usage_from_frame(frame: &[u8]) -> Option<ObservedUsage> {
    let text = String::from_utf8_lossy(frame).replace("\r\n", "\n");
    let data_lines: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|value| value.strip_prefix(' ').unwrap_or(value))
        .collect();
    if data_lines.is_empty() {
        return serde_json::from_str::<Value>(text.trim())
            .ok()
            .as_ref()
            .and_then(usage_from_value);
    }
    let data = data_lines.join("\n");
    if data == "[DONE]" {
        return None;
    }
    serde_json::from_str::<Value>(&data)
        .ok()
        .as_ref()
        .and_then(usage_from_value)
}

fn usage_from_value(value: &Value) -> Option<ObservedUsage> {
    let usage = value
        .pointer("/response/usage")
        .or_else(|| value.get("usage"))?;
    let details = usage.get("input_tokens_details");
    Some(ObservedUsage {
        input_tokens: usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cached_input_tokens: details
            .and_then(|value| value.get("cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_write_tokens: details
            .and_then(|value| {
                value
                    .get("cache_write_tokens")
                    .or_else(|| value.get("cache_creation_tokens"))
            })
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}

/// Ensures observe_stream_end runs even when the client disconnects and the body stream is dropped.
struct StreamObserveGuard {
    observer: Option<Arc<dyn Observer>>,
    event: RequestEvent,
    status: StatusCode,
    capture: Vec<u8>,
    usage: UsageAccumulator,
    finished: bool,
}

impl StreamObserveGuard {
    fn new(observer: Option<Arc<dyn Observer>>, event: RequestEvent, status: StatusCode) -> Self {
        Self {
            observer,
            event,
            status,
            capture: Vec::new(),
            usage: UsageAccumulator::default(),
            finished: false,
        }
    }

    fn capture(&mut self, chunk: &[u8]) {
        self.usage.push(chunk);
        capture_tail(&mut self.capture, chunk);
    }

    fn finish(&mut self, stream_io_error: Option<String>) {
        if self.finished {
            return;
        }
        self.finished = true;
        let usage = self.usage.finish();
        observe_stream_end(
            &self.observer,
            self.event.clone(),
            self.status,
            &self.capture,
            usage,
            stream_io_error,
        );
    }
}

impl Drop for StreamObserveGuard {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        // If capture already has a terminal frame, classify from diagnostics — client often
        // disconnects after the last SSE chunk without polling the generator to completion.
        let diag = parse_capture_diagnostics(&self.capture);
        let stream_io = if diag.has_terminal_end() {
            None
        } else {
            Some("client disconnected".into())
        };
        let usage = self.usage.finish();
        observe_stream_end(
            &self.observer,
            self.event.clone(),
            self.status,
            &self.capture,
            usage,
            stream_io,
        );
    }
}

fn observe_stream_end(
    observer: &Option<Arc<dyn Observer>>,
    mut event: RequestEvent,
    status: StatusCode,
    capture: &[u8],
    usage: ObservedUsage,
    stream_io_error: Option<String>,
) {
    event.status_code = status.as_u16();
    event.duration_ms = event.started_at.elapsed().as_millis() as u64;
    event.capture_bytes = capture.len() as u32;
    event.output_tokens = usage.output_tokens;
    if usage.input_tokens > 0
        || usage.cached_input_tokens > 0
        || usage.cache_write_tokens > 0
        || usage.output_tokens > 0
    {
        let uncached_input_tokens = usage.input_tokens.saturating_sub(usage.cached_input_tokens);
        let cache_read_percent = usage
            .cached_input_tokens
            .saturating_mul(100)
            .checked_div(usage.input_tokens)
            .unwrap_or(0);
        info!(
            request_id = event.request_id,
            session_id = event.session_id,
            input_tokens = usage.input_tokens,
            cached_input_tokens = usage.cached_input_tokens,
            cache_write_tokens = usage.cache_write_tokens,
            uncached_input_tokens,
            output_tokens = usage.output_tokens,
            cache_read_percent,
            "prompt cache usage"
        );
    }

    let diag = parse_capture_diagnostics(capture);
    if !diag.response_id.is_empty() {
        event.response_id = sanitize(&diag.response_id);
    }
    if diag.output_count > 0 {
        event.output_count = diag.output_count;
    }

    // 401 after force-refresh re-auth is AuthRetryFailed.
    if event.auth_retried
        && stream_io_error.is_none()
        && status.as_u16() == StatusCode::UNAUTHORIZED.as_u16()
    {
        event.kind = RequestEventKind::Failed;
        event.failure_kind = Some(FailureKind::AuthRetryFailed);
        event.error_type = sanitize(if !diag.error_type.is_empty() {
            diag.error_type.as_str()
        } else if !event.error_type.is_empty() {
            event.error_type.as_str()
        } else {
            "auth_retry_failed"
        });
        event.error = if !diag.error_message.is_empty() {
            sanitize(&diag.error_message)
        } else {
            sanitize(&format!("upstream HTTP {}", status.as_u16()))
        };
        if let Some(observer) = observer {
            observer.observe(event);
        }
        return;
    }

    let (kind, failure_kind, error_type, error_message) =
        classify_stream_end(status.is_success(), stream_io_error.as_deref(), &diag);

    event.kind = kind;
    event.failure_kind = failure_kind.or(event.failure_kind.take());
    event.error_type = sanitize(if !error_type.is_empty() {
        error_type.as_str()
    } else if kind == RequestEventKind::Failed {
        event.failure_kind.unwrap_or(FailureKind::Unknown).as_str()
    } else {
        ""
    });
    event.error = if !error_message.is_empty() {
        sanitize(&error_message)
    } else if kind == RequestEventKind::Failed && !status.is_success() {
        sanitize(&format!("upstream HTTP {}", status.as_u16()))
    } else {
        String::new()
    };

    if let Some(observer) = observer {
        observer.observe(event);
    }
}

fn observe_failure(
    config: &ProxyConfig,
    base: &RequestEvent,
    kind: FailureKind,
    error_type: &str,
    message: String,
    status: StatusCode,
) {
    if let Some(observer) = &config.observer {
        let mut event = base.clone();
        event.kind = RequestEventKind::Failed;
        event.status_code = status.as_u16();
        event.duration_ms = event.started_at.elapsed().as_millis() as u64;
        event.failure_kind = Some(kind);
        event.error_type = sanitize(error_type);
        event.error = sanitize(&message);
        observer.observe(event);
    }
}

fn with_request_id(mut r: Response, id: &str) -> Response {
    if let Ok(v) = HeaderValue::from_str(id) {
        r.headers_mut().insert("x-request-id", v);
    }
    r
}
async fn send_upstream(
    cfg: &ProxyConfig,
    t: &TransformedRequest,
    incoming: &HeaderMap,
    identity: &UpstreamIdentity,
    force: bool,
) -> Result<reqwest::Response> {
    let creds = cfg
        .credentials
        .get(force)
        .await
        .context("load Codex credentials")?;
    let compatible_body = codex_compat_body_for_identity(&t.body, identity, t.lite)?;
    let mut req = cfg
        .client
        .post(&cfg.upstream_url)
        .bearer_auth(creds.access_token)
        .header(header::CONTENT_TYPE, "application/json")
        .header(
            header::ACCEPT,
            if t.stream {
                "text/event-stream"
            } else {
                "application/json"
            },
        )
        .header(
            header::USER_AGENT,
            format!("grok-build-proxy/{}", cfg.version),
        )
        .header(
            "originator",
            if t.lite {
                "codex_cli_rs"
            } else {
                "grok-build-proxy"
            },
        )
        .header("session-id", &identity.session_id)
        .header("thread-id", &identity.thread_id)
        .header("x-session-affinity", &identity.session_id)
        .header("x-client-request-id", &identity.thread_id)
        .header("x-codex-window-id", format!("{}:0", identity.thread_id))
        .header("version", &cfg.compatibility_version);
    if t.lite {
        req = req
            .header("x-openai-internal-codex-responses-lite", "true")
            .header(header::ACCEPT_ENCODING, "identity");
    }
    if !creds.account_id.is_empty() {
        req = req.header("chatgpt-account-id", creds.account_id);
    }
    for key in ["traceparent", "tracestate"] {
        if let Some(v) = incoming
            .get(key)
            .and_then(|x| x.to_str().ok())
            .filter(|x| valid_header(x))
        {
            req = req.header(key, v);
        }
    }
    Ok(req.body(compatible_body).send().await?)
}

pub fn codex_compat_body(raw: &[u8], session: &str, lite: bool) -> Result<Vec<u8>> {
    let identity = UpstreamIdentity {
        thread_id: session.to_owned(),
        session_id: session.to_owned(),
        cache_key: bounded_cache_key(session),
    };
    codex_compat_body_for_identity(raw, &identity, lite)
}

fn codex_compat_body_for_identity(
    raw: &[u8],
    identity: &UpstreamIdentity,
    lite: bool,
) -> Result<Vec<u8>> {
    let mut value: Value = serde_json::from_slice(raw).context("decode Codex request body")?;
    let body = value
        .as_object_mut()
        .ok_or_else(|| anyhow!("request body must be a JSON object"))?;
    const ALLOWED: &[&str] = &[
        "client_metadata",
        "include",
        "input",
        "instructions",
        "model",
        "parallel_tool_calls",
        "prompt_cache_key",
        "prompt_cache_options",
        "prompt_cache_retention",
        "reasoning",
        "service_tier",
        "store",
        "stream",
        "stream_options",
        "text",
        "tool_choice",
        "tools",
    ];
    body.retain(|key, value| ALLOWED.contains(&key.as_str()) && !value.is_null());
    body.insert("store".into(), false.into());
    let prompt_cache_key = body
        .get("prompt_cache_key")
        .and_then(Value::as_str)
        .filter(|value| valid_cache_key(value))
        .map(str::to_owned)
        .unwrap_or_else(|| bounded_cache_key(&identity.cache_key));
    body.insert("prompt_cache_key".into(), prompt_cache_key.into());
    match body.get("tool_choice") {
        None | Some(Value::Null) => {
            body.insert("tool_choice".into(), "auto".into());
        }
        Some(Value::String(choice))
            if matches!(
                choice.trim().to_ascii_lowercase().as_str(),
                "auto" | "none" | "required"
            ) => {}
        Some(Value::Object(choice))
            if choice
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.trim().is_empty()) => {}
        _ => bail!("invalid tool_choice"),
    }
    let include = body.entry("include").or_insert_with(|| json!([]));
    let values = include
        .as_array_mut()
        .ok_or_else(|| anyhow!("include must be an array"))?;
    if !values.iter().any(|v| v == "reasoning.encrypted_content") {
        values.push("reasoning.encrypted_content".into());
    }
    let normalized_input = match body.remove("input") {
        Some(Value::Array(items)) => items,
        Some(Value::String(text)) if !text.is_empty() => vec![
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":text}]}),
        ],
        Some(Value::Null) | None => vec![],
        Some(value) => vec![value],
    };
    body.insert("input".into(), normalized_input.into());
    let items = body.get_mut("input").and_then(Value::as_array_mut).unwrap();
    for item in items.iter_mut() {
        normalize_input_item(item);
    }
    if let Some(metadata) = body.get_mut("client_metadata") {
        let object = metadata
            .as_object_mut()
            .ok_or_else(|| anyhow!("client_metadata must be an object"))?;
        object.retain(|_, v| v.is_string());
        object.remove("ws_request_header_x_openai_internal_codex_responses_lite");
        object.insert("session_id".into(), identity.session_id.clone().into());
        object.insert("thread_id".into(), identity.thread_id.clone().into());
        object.insert(
            "window_id".into(),
            format!("{}:0", identity.thread_id).into(),
        );
    }
    if lite {
        body.insert("parallel_tool_calls".into(), false.into());
        body.entry("reasoning")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| anyhow!("reasoning must be an object"))?
            .insert("context".into(), "all_turns".into());
        body.entry("text")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| anyhow!("text must be an object"))?
            .entry("verbosity")
            .or_insert("low".into());
        let top_tools = match body.remove("tools") {
            Some(Value::Array(v)) => v,
            Some(_) => bail!("tools must be an array"),
            None => vec![],
        };
        let items = body.get_mut("input").and_then(Value::as_array_mut).unwrap();
        let mut adopted = top_tools;
        items.retain(|item| {
            if item.get("type").and_then(Value::as_str) == Some("additional_tools") {
                if adopted.is_empty() {
                    adopted = item
                        .get("tools")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                }
                false
            } else {
                true
            }
        });
        items.insert(
            0,
            json!({"type":"additional_tools","role":"developer","tools":adopted}),
        );
        body.entry("instructions").or_insert("".into());
    }
    Ok(serde_json::to_vec(&value)?)
}

fn normalize_input_item(value: &mut Value) {
    if let Some(object) = value.as_object_mut() {
        object.remove("id");
        if object.get("role").and_then(Value::as_str) == Some("system") {
            object.insert("role".into(), "developer".into());
        }
        for child in object.values_mut() {
            if child.is_array() {
                for nested in child.as_array_mut().unwrap() {
                    normalize_input_item(nested)
                }
            }
        }
        if object.get("type").and_then(Value::as_str) == Some("input_image") {
            object.remove("detail");
        }
    }
}

fn valid_header(v: &str) -> bool {
    !v.is_empty() && v.len() <= 512 && !v.chars().any(char::is_control)
}
fn copy_headers(dst: &mut HeaderMap, src: &reqwest::header::HeaderMap) {
    let skip = [
        "connection",
        "proxy-connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "content-length",
        "set-cookie",
    ];
    for (k, v) in src {
        if !skip.contains(&k.as_str())
            && let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(k.as_str().as_bytes()),
                HeaderValue::from_bytes(v.as_bytes()),
            )
        {
            dst.append(name, value);
        }
    }
}
pub fn is_loopback_listen(address: &str) -> bool {
    let Some((host, _)) = address.rsplit_once(':') else {
        return false;
    };
    let host = host.trim_matches(['[', ']']);
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

pub fn normalize_sse(input: &[u8], mode: CompatMode, model: &str, request_id: &str) -> Vec<u8> {
    crate::sse::normalize_sse(input, mode, model, request_id)
}

#[allow(dead_code, clippy::collapsible_match)]
fn normalize_sse_old(input: &[u8], mode: CompatMode, model: &str, request_id: &str) -> Vec<u8> {
    let text = String::from_utf8_lossy(input);
    let mut out = String::new();
    let mut seq = 0u64;
    let mut response_id = String::new();
    let mut visible = String::new();
    let mut outputs: HashMap<usize, Value> = HashMap::new();
    let mut args: HashMap<usize, String> = HashMap::new();
    let mut terminal = false;
    for frame in text.split_inclusive("\n\n") {
        let mut data = None;
        let mut event_name = None;
        for line in frame.lines() {
            if let Some(v) = line.strip_prefix("event:") {
                event_name = Some(v.trim())
            }
            if let Some(v) = line.strip_prefix("data:") {
                data = Some(v.trim())
            }
        }
        let Some(raw) = data else {
            out.push_str(frame);
            continue;
        };
        if raw == "[DONE]" {
            out.push_str(frame);
            continue;
        }
        let Ok(mut event) = serde_json::from_str::<Value>(raw) else {
            out.push_str(frame);
            continue;
        };
        let typ = event
            .get("type")
            .and_then(Value::as_str)
            .or(event_name)
            .unwrap_or("")
            .to_owned();
        if typ == "response.metadata" {
            continue;
        }
        seq += 1;
        let discovered_id = event
            .pointer("/response/id")
            .and_then(Value::as_str)
            .or_else(|| event.get("response_id").and_then(Value::as_str))
            .map(str::to_owned);
        if let Some(id) = discovered_id {
            response_id = id;
        }
        if response_id.is_empty() {
            response_id = format!("resp_{request_id}")
        }
        let obj = event.as_object_mut().unwrap();
        obj.entry("type").or_insert(typ.clone().into());
        obj.entry("sequence_number").or_insert(seq.into());
        obj.entry("response_id")
            .or_insert(response_id.clone().into());
        let idx = event
            .get("output_index")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        match typ.as_str() {
            "response.output_text.delta" => {
                if let Some(d) = event.get("delta").and_then(Value::as_str) {
                    visible.push_str(d)
                }
            }
            "response.output_item.added" | "response.output_item.done" => {
                if let Some(item) = event.get("item") {
                    outputs.insert(idx, item.clone());
                }
            }
            "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
                if mode == CompatMode::Full
                    && let Some(d) = event.get("delta").and_then(Value::as_str)
                {
                    args.entry(idx).or_default().push_str(d)
                }
            }
            "response.completed" => {
                terminal = true;
                repair_terminal(
                    &mut event,
                    &response_id,
                    model,
                    &visible,
                    &outputs,
                    &args,
                    mode,
                )
            }
            "response.incomplete" | "response.failed" | "error" => terminal = true,
            _ => {}
        }
        out.push_str(&format!(
            "event: {typ}\ndata: {}\n\n",
            serde_json::to_string(&event).unwrap()
        ));
    }
    if !terminal && !visible.is_empty() {
        let response = json!({"id":response_id,"object":"response","status":"completed","model":model,"output":[{"id":format!("msg_{}",request_id),"type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":visible,"annotations":[]}]}]});
        out.push_str(&format!("event: response.completed\ndata: {}\n\n",json!({"type":"response.completed","sequence_number":seq+1,"response_id":response_id,"response":response})));
    }
    out.into_bytes()
}
fn repair_terminal(
    event: &mut Value,
    response_id: &str,
    model: &str,
    text: &str,
    outputs: &HashMap<usize, Value>,
    args: &HashMap<usize, String>,
    mode: CompatMode,
) {
    let Some(response) = event.get_mut("response").and_then(Value::as_object_mut) else {
        return;
    };
    response.entry("id").or_insert(response_id.into());
    response.entry("object").or_insert("response".into());
    response.entry("status").or_insert("completed".into());
    response.entry("model").or_insert(model.into());
    let empty = response
        .get("output")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty);
    if empty {
        let mut built = Vec::new();
        if !text.is_empty() {
            built.push(json!({"id":format!("msg_{}",response_id),"type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":text,"annotations":[]}]}));
        }
        if mode == CompatMode::Full {
            let mut keys: Vec<_> = outputs.keys().copied().collect();
            keys.sort();
            for k in keys {
                let mut item = outputs[&k].clone();
                if let Some(a) = args.get(&k) {
                    if item.get("type").and_then(Value::as_str) == Some("function_call") {
                        item.as_object_mut()
                            .unwrap()
                            .insert("arguments".into(), a.clone().into());
                    } else if item.get("type").and_then(Value::as_str) == Some("custom_tool_call") {
                        item.as_object_mut()
                            .unwrap()
                            .insert("input".into(), a.clone().into());
                    }
                }
                built.push(item);
            }
        }
        response.insert("output".into(), built.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transforms_lite() {
        let c = Catalog::default();
        let t=transform_request(br#"{"model":"gpt-5.6-sol","input":"hi","instructions":"dev","tools":[{"type":"function"}]}"#,&c,&ModelMap::default()).unwrap();
        let v: Value = serde_json::from_slice(&t.body).unwrap();
        assert_eq!(v["parallel_tool_calls"], false);
        assert_eq!(v["input"][0]["type"], "additional_tools");
        assert!(v["input"][0].get("id").is_none());
        let wire: Value =
            serde_json::from_slice(&codex_compat_body(&t.body, "session", true).unwrap()).unwrap();
        assert!(wire["input"][0].get("id").is_none());
        assert_eq!(wire["input"][0]["type"], "additional_tools");
        assert_eq!(v["input"][1]["role"], "developer");
        assert_eq!(v["input"][2]["role"], "user");
    }
    #[test]
    fn normalizes_terminal() {
        let input=b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\nevent: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n\n";
        let out = normalize_sse(input, CompatMode::Full, "gpt-5.6-sol", "r");
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("hello"));
        assert!(s.contains("sequence_number"));
        assert!(s.contains("output_text"));
    }
    #[test]
    fn loopback_policy() {
        assert!(is_loopback_listen("127.0.0.1:123"));
        assert!(is_loopback_listen("[::1]:123"));
        assert!(!is_loopback_listen("0.0.0.0:123"));
    }
    #[test]
    fn non_lite_string_input_reaches_codex_shape() {
        let transformed = transform_request(
            br#"{"model":"gpt-5.5","input":"hello"}"#,
            &Catalog::default(),
            &ModelMap::default(),
        )
        .unwrap();
        let value: Value = serde_json::from_slice(
            &codex_compat_body(&transformed.body, "session", false).unwrap(),
        )
        .unwrap();
        assert_eq!(value["input"][0]["role"], "user");
        assert_eq!(value["input"][0]["content"][0]["text"], "hello");
    }
    #[test]
    fn normalizes_codex_contract() {
        let raw=br#"{"model":"gpt-5.6-sol","input":[{"type":"message","role":"system","id":"drop","content":[{"type":"input_image","detail":"high"}]}],"temperature":1,"tool_choice":"required"}"#;
        let value: Value =
            serde_json::from_slice(&codex_compat_body(raw, "session", false).unwrap()).unwrap();
        assert!(value.get("temperature").is_none());
        assert_eq!(value["input"][0]["role"], "developer");
        assert!(value["input"][0].get("id").is_none());
        assert!(value["input"][0]["content"][0].get("detail").is_none());
        assert_eq!(value["prompt_cache_key"], "session");
        assert_eq!(value["tool_choice"], "required");
    }

    #[test]
    fn cache_identity_keeps_session_and_explicit_key_separate() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-grok-session-id",
            HeaderValue::from_static("child-thread"),
        );
        headers.insert(
            "x-grok-cache-lineage-id",
            HeaderValue::from_static("root-session"),
        );
        let identity = UpstreamIdentity::from_request(
            &headers,
            br#"{"prompt_cache_key":"shared-cache"}"#,
            "fallback",
        );
        assert_eq!(identity.thread_id, "child-thread");
        assert_eq!(identity.session_id, "root-session");
        assert_eq!(identity.cache_key, "shared-cache");
    }

    #[test]
    fn cache_keys_enforce_utf8_byte_limit_and_bound_fallbacks() {
        assert!(valid_cache_key(&"a".repeat(64)));
        assert!(!valid_cache_key(&"a".repeat(65)));
        assert!(valid_cache_key(&"é".repeat(32)));
        assert!(!valid_cache_key(&"é".repeat(33)));

        let lineage = "root".repeat(20);
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-grok-cache-lineage-id",
            HeaderValue::from_str(&lineage).unwrap(),
        );
        let first = UpstreamIdentity::from_request(&headers, b"{}", "fallback");
        let second = UpstreamIdentity::from_request(&headers, b"{}", "fallback");
        assert_eq!(first.session_id, lineage);
        assert_eq!(first.cache_key, second.cache_key);
        assert!(valid_cache_key(&first.cache_key));
        assert_ne!(first.cache_key, first.session_id);
    }

    #[test]
    fn preserves_explicit_prompt_cache_fields() {
        let identity = UpstreamIdentity {
            thread_id: "child-thread".into(),
            session_id: "lineage-default".into(),
            cache_key: "lineage-default".into(),
        };
        let raw = br#"{"model":"gpt-5.6-sol","input":"hello","prompt_cache_key":"root-cache","prompt_cache_options":{"mode":"auto"},"prompt_cache_retention":"24h","client_metadata":{}}"#;
        let value: Value =
            serde_json::from_slice(&codex_compat_body_for_identity(raw, &identity, false).unwrap())
                .unwrap();
        assert_eq!(value["prompt_cache_key"], "root-cache");
        assert_eq!(value["prompt_cache_options"]["mode"], "auto");
        assert_eq!(value["prompt_cache_retention"], "24h");
        assert_eq!(value["client_metadata"]["session_id"], "lineage-default");
        assert_eq!(value["client_metadata"]["thread_id"], "child-thread");
        assert_eq!(value["client_metadata"]["window_id"], "child-thread:0");
    }

    #[test]
    fn invalid_prompt_cache_key_falls_back_to_lineage() {
        let identity = UpstreamIdentity {
            thread_id: "child-thread".into(),
            session_id: "root-cache".into(),
            cache_key: "root-cache".into(),
        };
        for invalid in ["bad\nkey".into(), "a".repeat(65), "é".repeat(33)] {
            let raw = serde_json::to_vec(&json!({
                "model": "gpt-5.6-sol",
                "input": "hello",
                "prompt_cache_key": invalid,
            }))
            .unwrap();
            let value: Value = serde_json::from_slice(
                &codex_compat_body_for_identity(&raw, &identity, false).unwrap(),
            )
            .unwrap();
            assert_eq!(value["prompt_cache_key"], "root-cache");
        }
    }

    #[test]
    fn parses_prompt_cache_usage_from_terminal_sse() {
        let capture = br#"event: response.completed
data: {"type":"response.completed","response":{"usage":{"input_tokens":12000,"input_tokens_details":{"cached_tokens":9000,"cache_write_tokens":1500},"output_tokens":321}}}

"#;
        assert_eq!(
            observed_usage(capture),
            ObservedUsage {
                input_tokens: 12_000,
                cached_input_tokens: 9_000,
                cache_write_tokens: 1_500,
                output_tokens: 321,
            }
        );
    }

    #[test]
    fn usage_accumulator_handles_split_large_crlf_terminal_frame() {
        let padding = "x".repeat((256 << 10) + 1);
        let frame = format!(
            "event: response.completed\r\ndata: {{\"type\":\"response.completed\",\"padding\":\"{padding}\",\"response\":{{\"usage\":{{\"input_tokens\":12000,\"input_tokens_details\":{{\"cached_tokens\":9000,\"cache_write_tokens\":1500}},\"output_tokens\":321}}}}}}\r\n\r\n"
        );
        let split = frame.len() / 2;
        let mut accumulator = UsageAccumulator::default();
        let mut diagnostic_tail = Vec::new();
        accumulator.push(&frame.as_bytes()[..split]);
        capture_tail(&mut diagnostic_tail, &frame.as_bytes()[..split]);
        assert_eq!(accumulator.observed, ObservedUsage::default());
        accumulator.push(&frame.as_bytes()[split..]);
        capture_tail(&mut diagnostic_tail, &frame.as_bytes()[split..]);
        assert_eq!(diagnostic_tail.len(), 256 << 10);
        assert!(!diagnostic_tail.starts_with(b"event:"));
        assert_eq!(
            accumulator.finish(),
            ObservedUsage {
                input_tokens: 12_000,
                cached_input_tokens: 9_000,
                cache_write_tokens: 1_500,
                output_tokens: 321,
            }
        );
    }
    #[tokio::test]
    async fn bearer_protects_models_but_not_health() {
        use crate::auth::Credentials;
        use tower::ServiceExt;
        struct Creds;
        #[async_trait::async_trait]
        impl CredentialProvider for Creds {
            async fn get(&self, _: bool) -> Result<Credentials> {
                Ok(Credentials {
                    access_token: "x".into(),
                    account_id: String::new(),
                    expires_at: None,
                })
            }
        }
        let app = router(ProxyConfig {
            upstream_url: "http://127.0.0.1:9/responses".into(),
            credentials: Arc::new(Creds),
            catalog: Catalog::default(),
            model_map: ModelMap::default(),
            client: reqwest::Client::new(),
            client_token: "secret".into(),
            version: "test".into(),
            compatibility_version: DEFAULT_CODEX_COMPATIBILITY_VERSION.into(),
            responses_compat: CompatMode::Full,
            observer: None,
            max_body_bytes: 1024,
        })
        .unwrap();
        let health = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);
        let denied = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let allowed = app
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .header(header::AUTHORIZATION, "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(allowed.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn handler_streams_and_normalizes_fake_upstream() {
        use crate::auth::Credentials;
        use axum::response::IntoResponse;
        use tower::ServiceExt;
        struct Creds;
        #[async_trait::async_trait]
        impl CredentialProvider for Creds {
            async fn get(&self, _: bool) -> Result<Credentials> {
                Ok(Credentials {
                    access_token: "upstream-secret".into(),
                    account_id: "account".into(),
                    expires_at: None,
                })
            }
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream=Router::new().route("/responses",axum::routing::post(|headers:HeaderMap|async move {assert_eq!(headers[header::AUTHORIZATION],"Bearer upstream-secret");([(header::CONTENT_TYPE,"text/event-stream")],"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_live\"}}\n\nevent: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"live\"}\n\ndata: [DONE]\n\n").into_response()}));
        tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let app = router(ProxyConfig {
            upstream_url: format!("http://{address}/responses"),
            credentials: Arc::new(Creds),
            catalog: Catalog::default(),
            model_map: ModelMap::default(),
            client: reqwest::Client::new(),
            client_token: String::new(),
            version: "test".into(),
            compatibility_version: DEFAULT_CODEX_COMPATIBILITY_VERSION.into(),
            responses_compat: CompatMode::Full,
            observer: None,
            max_body_bytes: 4096,
        })
        .unwrap();
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/responses")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"model":"gpt-5.6-sol","input":"hi"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        let stream = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(stream.contains("live"));
        assert!(stream.contains("response.completed"));
    }

    #[tokio::test]
    async fn cache_lineage_separates_upstream_session_and_thread() {
        use crate::auth::Credentials;
        use axum::response::IntoResponse;
        use tower::ServiceExt;

        struct Creds;
        #[async_trait::async_trait]
        impl CredentialProvider for Creds {
            async fn get(&self, _: bool) -> Result<Credentials> {
                Ok(Credentials {
                    access_token: "upstream-secret".into(),
                    account_id: String::new(),
                    expires_at: None,
                })
            }
        }

        let captured = Arc::new(std::sync::Mutex::new(None::<(HeaderMap, Value)>));
        let captured_upstream = captured.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream = Router::new().route(
            "/responses",
            axum::routing::post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let captured = captured_upstream.clone();
                async move {
                    *captured.lock().unwrap() = Some((headers, body));
                    (
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_cache\",\"output\":[]}}\n\n",
                    )
                        .into_response()
                }
            }),
        );
        tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });

        let app = router(ProxyConfig {
            upstream_url: format!("http://{address}/responses"),
            credentials: Arc::new(Creds),
            catalog: Catalog::default(),
            model_map: ModelMap::default(),
            client: reqwest::Client::new(),
            client_token: String::new(),
            version: "test".into(),
            compatibility_version: DEFAULT_CODEX_COMPATIBILITY_VERSION.into(),
            responses_compat: CompatMode::Full,
            observer: None,
            max_body_bytes: 4096,
        })
        .unwrap();
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/responses")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-grok-session-id", "child-thread")
                    .header("x-grok-cache-lineage-id", "root-cache")
                    .body(Body::from(
                        r#"{"model":"gpt-5.6-sol","input":"hi","prompt_cache_key":"shared-cache","client_metadata":{}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let _ = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();

        let guard = captured.lock().unwrap();
        let (headers, body) = guard.as_ref().expect("upstream request captured");
        assert_eq!(headers["session-id"], "root-cache");
        assert_eq!(headers["x-session-affinity"], "root-cache");
        assert_eq!(headers["thread-id"], "child-thread");
        assert_eq!(headers["x-client-request-id"], "child-thread");
        assert_eq!(headers["x-codex-window-id"], "child-thread:0");
        assert_eq!(body["prompt_cache_key"], "shared-cache");
        assert_eq!(body["client_metadata"]["session_id"], "root-cache");
        assert_eq!(body["client_metadata"]["thread_id"], "child-thread");
    }

    struct RecordingObserver {
        events: std::sync::Mutex<Vec<RequestEvent>>,
    }
    impl Observer for RecordingObserver {
        fn observe(&self, event: RequestEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    fn sample_event() -> RequestEvent {
        RequestEvent {
            kind: RequestEventKind::Started,
            request_id: "req-1".into(),
            session_id: "sess".into(),
            requested_model: "alias".into(),
            model: "gpt-5.6-sol".into(),
            status_code: 0,
            output_tokens: 0,
            error: String::new(),
            started_at: std::time::Instant::now() - std::time::Duration::from_millis(50),
            duration_ms: 0,
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
    fn observe_stream_end_marks_2xx_proxy_incomplete_as_failed() {
        let obs = Arc::new(RecordingObserver {
            events: std::sync::Mutex::new(Vec::new()),
        });
        let observer: Option<Arc<dyn Observer>> = Some(obs.clone());
        let capture = br#"event: error
data: {"type":"error","sequence_number":3,"response_id":"resp_x","error":{"type":"proxy_incomplete_output","message":"The proxy could not safely assemble a complete Responses stream."}}

"#;
        observe_stream_end(
            &observer,
            sample_event(),
            StatusCode::OK,
            capture,
            observed_usage(capture),
            None,
        );
        let events = obs.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.kind, RequestEventKind::Failed);
        assert_eq!(e.failure_kind, Some(FailureKind::ProxyAssemble));
        assert_eq!(e.error_type, "proxy_incomplete_output");
        assert_eq!(e.status_code, 200);
        assert_eq!(e.response_id, "resp_x");
        assert!(e.duration_ms >= 50);
        assert!(e.capture_bytes > 0);
    }

    #[test]
    fn observe_stream_end_auth_retry_attempt() {
        let obs = Arc::new(RecordingObserver {
            events: std::sync::Mutex::new(Vec::new()),
        });
        let observer: Option<Arc<dyn Observer>> = Some(obs.clone());
        let mut event = sample_event();
        event.auth_retried = true;
        event.attempt = 2;
        observe_stream_end(
            &observer,
            event,
            StatusCode::UNAUTHORIZED,
            br#"{"error":{"type":"invalid_request_error","message":"unauthorized"}}"#,
            ObservedUsage::default(),
            None,
        );
        let events = obs.events.lock().unwrap();
        assert_eq!(events[0].kind, RequestEventKind::Failed);
        assert_eq!(events[0].failure_kind, Some(FailureKind::AuthRetryFailed));
        assert_eq!(events[0].attempt, 2);
        assert!(events[0].auth_retried);
    }

    #[test]
    fn observe_stream_end_stream_io() {
        let obs = Arc::new(RecordingObserver {
            events: std::sync::Mutex::new(Vec::new()),
        });
        let observer: Option<Arc<dyn Observer>> = Some(obs.clone());
        observe_stream_end(
            &observer,
            sample_event(),
            StatusCode::OK,
            b"",
            ObservedUsage::default(),
            Some("connection reset".into()),
        );
        let events = obs.events.lock().unwrap();
        assert_eq!(events[0].failure_kind, Some(FailureKind::StreamIo));
        assert_eq!(events[0].kind, RequestEventKind::Failed);
    }

    #[test]
    fn observe_stream_end_proxy_mode_in_content_stays_completed() {
        let obs = Arc::new(RecordingObserver {
            events: std::sync::Mutex::new(Vec::new()),
        });
        let observer: Option<Arc<dyn Observer>> = Some(obs.clone());
        let capture = br#"event: response.completed
data: {"type":"response.completed","response":{"id":"resp_ok","output":[{"type":"message","content":[{"type":"output_text","text":"configure proxy_mode now"}]}]}}

"#;
        observe_stream_end(
            &observer,
            sample_event(),
            StatusCode::OK,
            capture,
            observed_usage(capture),
            None,
        );
        let events = obs.events.lock().unwrap();
        assert_eq!(events[0].kind, RequestEventKind::Completed);
        assert_eq!(events[0].failure_kind, None);
    }

    #[test]
    fn observe_failure_sanitizes_and_auth_retry_kind() {
        use crate::auth::Credentials;
        struct Creds;
        #[async_trait::async_trait]
        impl CredentialProvider for Creds {
            async fn get(&self, _: bool) -> Result<Credentials> {
                unreachable!()
            }
        }
        let obs = Arc::new(RecordingObserver {
            events: std::sync::Mutex::new(Vec::new()),
        });
        let config = ProxyConfig {
            upstream_url: "http://127.0.0.1:9/responses".into(),
            credentials: Arc::new(Creds),
            catalog: Catalog::default(),
            model_map: ModelMap::default(),
            client: reqwest::Client::new(),
            client_token: String::new(),
            version: "test".into(),
            compatibility_version: DEFAULT_CODEX_COMPATIBILITY_VERSION.into(),
            responses_compat: CompatMode::Full,
            observer: Some(obs.clone()),
            max_body_bytes: 1024,
        };
        let mut base = sample_event();
        base.auth_retried = true;
        base.attempt = 2;
        observe_failure(
            &config,
            &base,
            FailureKind::AuthRetryFailed,
            "auth_retry_failed",
            format!("connect error\n{}", "x".repeat(300)),
            StatusCode::BAD_GATEWAY,
        );
        let events = obs.events.lock().unwrap();
        assert_eq!(events[0].failure_kind, Some(FailureKind::AuthRetryFailed));
        assert_eq!(events[0].attempt, 2);
        assert!(!events[0].error.contains('\n'));
        assert!(events[0].error.chars().count() <= 256);
        assert_eq!(events[0].error_type, "auth_retry_failed");
    }

    #[test]
    fn stream_observe_guard_emits_on_drop() {
        let obs = Arc::new(RecordingObserver {
            events: std::sync::Mutex::new(Vec::new()),
        });
        {
            let mut guard =
                StreamObserveGuard::new(Some(obs.clone()), sample_event(), StatusCode::OK);
            capture_tail(&mut guard.capture, b"partial");
            // drop without finish and without terminal → client disconnect path
        }
        let events = obs.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, RequestEventKind::Failed);
        assert_eq!(events[0].failure_kind, Some(FailureKind::StreamIo));
        assert!(events[0].error.contains("client disconnected"));
    }

    #[test]
    fn stream_observe_guard_drop_with_completed_capture_stays_completed() {
        let obs = Arc::new(RecordingObserver {
            events: std::sync::Mutex::new(Vec::new()),
        });
        let completed = br#"event: response.completed
data: {"type":"response.completed","response":{"id":"resp_ok","output":[{"type":"message"}]}}

"#;
        {
            let mut guard =
                StreamObserveGuard::new(Some(obs.clone()), sample_event(), StatusCode::OK);
            capture_tail(&mut guard.capture, completed);
            // Client drop after last chunk without finish() — must not force StreamIo.
        }
        let events = obs.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, RequestEventKind::Completed);
        assert_eq!(events[0].failure_kind, None);
        assert_eq!(events[0].response_id, "resp_ok");
    }

    #[test]
    fn stream_observe_guard_drop_with_proxy_error_keeps_assemble() {
        let obs = Arc::new(RecordingObserver {
            events: std::sync::Mutex::new(Vec::new()),
        });
        let err = br#"event: error
data: {"type":"error","error":{"type":"proxy_incomplete_output","message":"incomplete"}}

"#;
        {
            let mut guard =
                StreamObserveGuard::new(Some(obs.clone()), sample_event(), StatusCode::OK);
            capture_tail(&mut guard.capture, err);
        }
        let events = obs.events.lock().unwrap();
        assert_eq!(events[0].kind, RequestEventKind::Failed);
        assert_eq!(events[0].failure_kind, Some(FailureKind::ProxyAssemble));
        assert_eq!(events[0].error_type, "proxy_incomplete_output");
    }
}
