//! Structured request lifecycle events for the serve monitor.

use std::fmt;
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestEventKind {
    Started,
    Completed,
    Failed,
}

/// Classification of a failed turn for monitor/report surfaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FailureKind {
    /// Non-2xx status from Codex upstream.
    UpstreamHttp,
    /// Network / `send_upstream` error before a response status.
    UpstreamConnect,
    /// 401 after force-refresh re-auth path still failed.
    AuthRetryFailed,
    /// Chunk error mid-stream.
    StreamIo,
    /// Terminal SSE event: `response.failed` / `response.incomplete` / `error`.
    StreamTerminalFailed,
    /// Proxy assembly failure (`proxy_incomplete_output`, etc.).
    ProxyAssemble,
    /// Proxy itself rejected the client (400/401 before upstream).
    ClientRejected,
    Unknown,
}

impl FailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UpstreamHttp => "UpstreamHttp",
            Self::UpstreamConnect => "UpstreamConnect",
            Self::AuthRetryFailed => "AuthRetryFailed",
            Self::StreamIo => "StreamIo",
            Self::StreamTerminalFailed => "StreamTerminalFailed",
            Self::ProxyAssemble => "ProxyAssemble",
            Self::ClientRejected => "ClientRejected",
            Self::Unknown => "Unknown",
        }
    }
}

impl fmt::Display for FailureKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug)]
pub struct RequestEvent {
    pub kind: RequestEventKind,
    pub request_id: String,
    pub session_id: String,
    pub requested_model: String,
    pub model: String,
    pub status_code: u16,
    pub output_tokens: u64,
    pub error: String,
    pub started_at: Instant,
    pub duration_ms: u64,
    pub failure_kind: Option<FailureKind>,
    /// e.g. `proxy_incomplete_output`, `upstream_error`
    pub error_type: String,
    pub response_id: String,
    pub mapped: bool,
    pub lite: bool,
    pub fast: bool,
    pub auth_retried: bool,
    /// 1-based proxy-internal attempt (auth retry = 2).
    pub attempt: u32,
    pub output_count: u32,
    pub capture_bytes: u32,
}

pub trait Observer: Send + Sync {
    fn observe(&self, event: RequestEvent);
}

impl RequestEvent {
    pub fn started(
        request_id: impl Into<String>,
        session_id: impl Into<String>,
        requested_model: impl Into<String>,
        model: impl Into<String>,
        mapped: bool,
        lite: bool,
        fast: bool,
    ) -> Self {
        Self {
            kind: RequestEventKind::Started,
            request_id: request_id.into(),
            session_id: session_id.into(),
            requested_model: requested_model.into(),
            model: model.into(),
            status_code: 0,
            output_tokens: 0,
            error: String::new(),
            started_at: Instant::now(),
            duration_ms: 0,
            failure_kind: None,
            error_type: String::new(),
            response_id: String::new(),
            mapped,
            lite,
            fast,
            auth_retried: false,
            attempt: 1,
            output_count: 0,
            capture_bytes: 0,
        }
    }

    pub fn with_duration(mut self) -> Self {
        self.duration_ms = self.started_at.elapsed().as_millis() as u64;
        self
    }
}

/// Preserve an identifier losslessly while making control characters safe to display.
pub fn sanitize_id(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.extend(c.escape_default()),
            c => out.push(c),
        }
    }
    out
}

/// Sanitize monitor-facing strings: strip control chars, cap length.
pub fn sanitize(value: &str) -> String {
    let original_len = value.chars().count();
    let mut out: String = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(256)
        .collect();
    out = out.trim().to_owned();
    // Ellipsis only when the original exceeded the cap (post-trim may be shorter).
    if original_len > 256 && out.chars().count() >= 256 {
        while out.chars().count() >= 256 {
            out.pop();
        }
        out.push('…');
    } else if original_len > 256 && !out.is_empty() && !out.ends_with('…') {
        // Trim ate trailing space; still mark truncation when we took a full prefix.
        if out.chars().count() >= 255 {
            out.pop();
        }
        out.push('…');
    }
    out
}

/// Parse diagnostic fields from a captured SSE / JSON body tail without storing model content.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CaptureDiagnostics {
    pub error_type: String,
    pub error_message: String,
    pub response_id: String,
    pub terminal_event: String,
    pub output_count: u32,
    pub has_proxy_error: bool,
    pub has_stream_terminal_failure: bool,
    /// True when a `response.completed` frame was seen (successful terminal).
    pub has_completed: bool,
}

impl CaptureDiagnostics {
    /// Capture already has a terminal signal (success or failure) — Drop must not force StreamIo.
    pub fn has_terminal_end(&self) -> bool {
        self.has_completed || self.has_stream_terminal_failure || self.has_proxy_error
    }
}

const KNOWN_PROXY_ERROR_TYPES: &[&str] =
    &["proxy_incomplete_output", "proxy_missing_terminal_output"];

fn is_known_proxy_error_type(et: &str) -> bool {
    KNOWN_PROXY_ERROR_TYPES.contains(&et) || {
        // Accept only exact known tokens or future proxy_* kinds that look like error type ids
        // (snake_case, no spaces) — still only when extracted from structured error.type fields.
        et.starts_with("proxy_")
            && et.len() > "proxy_".len()
            && et
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    }
}

/// Extract failure diagnostics from a capture tail (SSE frames or JSON body).
///
/// Failure signals are taken only from:
/// - terminal SSE events (`error`, `response.failed`, `response.incomplete`)
/// - plain non-SSE JSON error bodies (no `event:` frames)
///
/// Model content / completed-frame payloads are never scanned for `proxy_` substrings.
pub fn parse_capture_diagnostics(bytes: &[u8]) -> CaptureDiagnostics {
    let text = String::from_utf8_lossy(bytes);
    let mut diag = CaptureDiagnostics::default();

    // response_id: last occurrence of "response_id":"..." or "id":"resp_..."
    for marker in ["\"response_id\"", "\"id\""] {
        for (index, _) in text.match_indices(marker) {
            if let Some(id) = extract_json_string_after(&text[index + marker.len()..]) {
                if id.starts_with("resp_") || marker == "\"response_id\"" {
                    diag.response_id = sanitize(&id);
                }
            }
        }
    }

    if let Some(count) = count_output_items(&text) {
        diag.output_count = count;
    }

    let mut saw_sse_event = false;

    for frame in text.split("\n\n") {
        let mut event_name = None;
        let mut data_lines = Vec::new();
        for line in frame.lines() {
            if let Some(v) = line.strip_prefix("event:") {
                event_name = Some(v.trim());
                saw_sse_event = true;
            } else if let Some(v) = line.strip_prefix("data:") {
                data_lines.push(v.strip_prefix(' ').unwrap_or(v));
            }
        }
        let joined;
        let data = if data_lines.is_empty() {
            let trimmed = frame.trim();
            if trimmed.is_empty() {
                continue;
            }
            trimmed
        } else if data_lines.len() == 1 {
            data_lines[0]
        } else {
            joined = data_lines.join("\n");
            joined.as_str()
        };
        if data == "[DONE]" {
            continue;
        }

        let typ = event_name
            .map(str::to_owned)
            .or_else(|| extract_json_string_field(data, "type"))
            .unwrap_or_default();

        match typ.as_str() {
            "response.completed" => {
                diag.has_completed = true;
            }
            "response.failed" | "response.incomplete" => {
                apply_terminal_error(&mut diag, &typ, data);
            }
            "error" => {
                apply_terminal_error(&mut diag, "error", data);
            }
            // Non-terminal frames (deltas, output items, completed payloads): ignore nested
            // "error" objects so model-visible JSON cannot trip ProxyAssemble.
            _ => {}
        }
    }

    // Plain JSON error body (non-SSE upstream HTTP responses). Never mark proxy errors from
    // successful completed streams.
    if !saw_sse_event && !diag.has_completed && !diag.has_stream_terminal_failure {
        if let Some(et) = extract_nested_error_type(&text) {
            diag.error_type = sanitize(&et);
            if is_known_proxy_error_type(&et) {
                diag.has_proxy_error = true;
                diag.has_stream_terminal_failure = true;
                diag.terminal_event = "error".into();
            }
        }
        if diag.error_message.is_empty() {
            if let Some(msg) = extract_nested_error_message(&text) {
                diag.error_message = sanitize(&msg);
            }
        }
    }

    diag
}

fn apply_terminal_error(diag: &mut CaptureDiagnostics, terminal: &str, data: &str) {
    diag.has_stream_terminal_failure = true;
    diag.terminal_event = terminal.to_owned();

    // Prefer nested error.type; fall back to top-level type when it is not the event name.
    let et = extract_nested_error_type(data).or_else(|| {
        extract_json_string_field(data, "type").filter(|t| t != "error" && t != terminal)
    });
    if let Some(et) = et {
        diag.error_type = sanitize(&et);
        if is_known_proxy_error_type(&et) {
            diag.has_proxy_error = true;
        }
    }

    if let Some(msg) =
        extract_nested_error_message(data).or_else(|| extract_json_string_field(data, "message"))
    {
        diag.error_message = sanitize(&msg);
    }
}

fn extract_json_string_after(rest: &str) -> Option<String> {
    let rest = rest.trim_start().trim_start_matches(':').trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else if c == '"' {
            break;
        } else {
            out.push(c);
        }
        if out.len() > 256 {
            break;
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn extract_json_string_field(json: &str, field: &str) -> Option<String> {
    let marker = format!("\"{field}\"");
    let index = json.find(&marker)?;
    extract_json_string_after(&json[index + marker.len()..])
}

fn extract_nested_error_type(json: &str) -> Option<String> {
    // Prefer "error":{"type":"..."} over top-level "type"
    if let Some(err_idx) = json.find("\"error\"") {
        let rest = &json[err_idx..];
        if let Some(type_rel) = rest.find("\"type\"") {
            if let Some(et) = extract_json_string_after(&rest[type_rel + "\"type\"".len()..]) {
                return Some(et);
            }
        }
    }
    None
}

fn extract_nested_error_message(json: &str) -> Option<String> {
    if let Some(err_idx) = json.find("\"error\"") {
        let rest = &json[err_idx..];
        if let Some(msg_rel) = rest.find("\"message\"") {
            return extract_json_string_after(&rest[msg_rel + "\"message\"".len()..]);
        }
    }
    extract_json_string_field(json, "message")
}

fn count_output_items(text: &str) -> Option<u32> {
    let marker = "\"output\"";
    let idx = text.rfind(marker)?;
    let rest = text[idx + marker.len()..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('[')?;
    let mut depth = 1i32;
    let mut items = 0u32;
    let mut in_string = false;
    let mut escape = false;
    let mut saw_object = false;
    for c in rest.chars() {
        if in_string {
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => {
                if depth == 1 {
                    saw_object = true;
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 1 && saw_object {
                    items += 1;
                    saw_object = false;
                }
            }
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(items);
                }
            }
            _ => {}
        }
        if depth < 0 {
            break;
        }
    }
    if items > 0 { Some(items) } else { None }
}

/// Classify a stream-end / non-stream response for the observer.
pub fn classify_stream_end(
    status_success: bool,
    stream_io_error: Option<&str>,
    diag: &CaptureDiagnostics,
) -> (RequestEventKind, Option<FailureKind>, String, String) {
    if let Some(msg) = stream_io_error {
        return (
            RequestEventKind::Failed,
            Some(FailureKind::StreamIo),
            "stream_io".into(),
            sanitize(msg),
        );
    }

    if !status_success {
        let error_type = if !diag.error_type.is_empty() {
            diag.error_type.clone()
        } else {
            "upstream_http".into()
        };
        let message = if !diag.error_message.is_empty() {
            diag.error_message.clone()
        } else {
            String::new()
        };
        return (
            RequestEventKind::Failed,
            Some(FailureKind::UpstreamHttp),
            error_type,
            message,
        );
    }

    // 2xx but stream terminal / proxy assembly failure
    if diag.has_proxy_error
        || (diag.has_stream_terminal_failure && is_known_proxy_error_type(&diag.error_type))
    {
        let et = if diag.error_type.is_empty() {
            "proxy_assemble".into()
        } else {
            diag.error_type.clone()
        };
        let msg = if diag.error_message.is_empty() {
            et.clone()
        } else {
            diag.error_message.clone()
        };
        return (
            RequestEventKind::Failed,
            Some(FailureKind::ProxyAssemble),
            et,
            msg,
        );
    }

    if diag.has_stream_terminal_failure {
        let et = if !diag.error_type.is_empty() {
            diag.error_type.clone()
        } else if !diag.terminal_event.is_empty() {
            diag.terminal_event.clone()
        } else {
            "stream_terminal_failed".into()
        };
        let msg = if diag.error_message.is_empty() {
            et.clone()
        } else {
            diag.error_message.clone()
        };
        return (
            RequestEventKind::Failed,
            Some(FailureKind::StreamTerminalFailed),
            et,
            msg,
        );
    }

    (
        RequestEventKind::Completed,
        None,
        String::new(),
        String::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_proxy_incomplete_on_2xx() {
        let sse = r#"event: error
data: {"type":"error","error":{"type":"proxy_incomplete_output","message":"The proxy could not safely assemble a terminal response."}}

"#;
        let diag = parse_capture_diagnostics(sse.as_bytes());
        assert!(diag.has_proxy_error);
        assert_eq!(diag.error_type, "proxy_incomplete_output");
        let (kind, fk, et, _) = classify_stream_end(true, None, &diag);
        assert_eq!(kind, RequestEventKind::Failed);
        assert_eq!(fk, Some(FailureKind::ProxyAssemble));
        assert_eq!(et, "proxy_incomplete_output");
    }

    #[test]
    fn classifies_response_failed() {
        let sse = r#"event: response.failed
data: {"type":"response.failed","response":{"id":"resp_abc","error":{"type":"server_error","message":"boom"}}}

"#;
        let diag = parse_capture_diagnostics(sse.as_bytes());
        assert!(diag.has_stream_terminal_failure);
        assert_eq!(diag.response_id, "resp_abc");
        let (kind, fk, et, msg) = classify_stream_end(true, None, &diag);
        assert_eq!(kind, RequestEventKind::Failed);
        assert_eq!(fk, Some(FailureKind::StreamTerminalFailed));
        assert_eq!(et, "server_error");
        assert!(msg.contains("boom"));
    }

    #[test]
    fn classifies_response_incomplete() {
        let sse = r#"event: response.incomplete
data: {"type":"response.incomplete","response":{"id":"resp_inc","status":"incomplete"}}

"#;
        let diag = parse_capture_diagnostics(sse.as_bytes());
        assert!(diag.has_stream_terminal_failure);
        assert!(!diag.has_proxy_error);
        let (kind, fk, et, _) = classify_stream_end(true, None, &diag);
        assert_eq!(kind, RequestEventKind::Failed);
        assert_eq!(fk, Some(FailureKind::StreamTerminalFailed));
        assert_eq!(et, "response.incomplete");
        assert_eq!(diag.response_id, "resp_inc");
    }

    #[test]
    fn classifies_response_incomplete_with_nested_error_type() {
        let sse = r#"event: response.incomplete
data: {"type":"response.incomplete","response":{"id":"resp_inc","error":{"type":"max_output_tokens","message":"hit limit"}}}

"#;
        let diag = parse_capture_diagnostics(sse.as_bytes());
        let (kind, fk, et, msg) = classify_stream_end(true, None, &diag);
        assert_eq!(kind, RequestEventKind::Failed);
        assert_eq!(fk, Some(FailureKind::StreamTerminalFailed));
        assert_eq!(et, "max_output_tokens");
        assert!(msg.contains("hit limit"));
    }

    #[test]
    fn classifies_stream_io() {
        let diag = CaptureDiagnostics::default();
        let (kind, fk, et, msg) = classify_stream_end(true, Some("connection reset"), &diag);
        assert_eq!(kind, RequestEventKind::Failed);
        assert_eq!(fk, Some(FailureKind::StreamIo));
        assert_eq!(et, "stream_io");
        assert!(msg.contains("connection reset"));
    }

    #[test]
    fn sanitize_id_is_lossless_and_control_safe() {
        let newline = sanitize_id("session\n");
        let literal = sanitize_id("session\\n");
        assert_eq!(newline, "session\\n");
        assert_eq!(literal, "session\\\\n");
        assert_ne!(newline, literal);
        assert!(!newline.contains('\n'));
    }

    #[test]
    fn sanitize_strips_control_and_caps() {
        let s = sanitize(&format!("a\nb{}", "x".repeat(300)));
        assert!(!s.contains('\n'));
        assert!(s.ends_with('…'));
        assert!(s.chars().count() <= 256);
    }

    #[test]
    fn success_with_clean_capture() {
        let sse = r#"event: response.completed
data: {"type":"response.completed","response":{"id":"resp_ok","output":[{"type":"message"}]}}

"#;
        let diag = parse_capture_diagnostics(sse.as_bytes());
        assert!(diag.has_completed);
        assert!(diag.has_terminal_end());
        let (kind, fk, _, _) = classify_stream_end(true, None, &diag);
        assert_eq!(kind, RequestEventKind::Completed);
        assert_eq!(fk, None);
        assert_eq!(diag.response_id, "resp_ok");
    }

    #[test]
    fn content_mentioning_proxy_mode_does_not_fail() {
        let sse = r#"event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"configure proxy_mode now"}

event: response.completed
data: {"type":"response.completed","response":{"id":"resp_ok","output":[{"type":"message","content":[{"type":"output_text","text":"configure proxy_mode now"}]}]}}

"#;
        let diag = parse_capture_diagnostics(sse.as_bytes());
        assert!(!diag.has_proxy_error);
        assert!(!diag.has_stream_terminal_failure);
        let (kind, fk, _, _) = classify_stream_end(true, None, &diag);
        assert_eq!(kind, RequestEventKind::Completed);
        assert_eq!(fk, None);
    }

    #[test]
    fn embedded_error_json_in_completed_output_does_not_fail() {
        let sse = r#"event: response.completed
data: {"type":"response.completed","response":{"id":"resp_ok","output":[{"type":"message","content":[{"type":"output_text","text":"{\"error\":{\"type\":\"proxy_incomplete_output\",\"message\":\"nope\"}}"}]}]}}

"#;
        let diag = parse_capture_diagnostics(sse.as_bytes());
        assert!(!diag.has_proxy_error);
        let (kind, fk, _, _) = classify_stream_end(true, None, &diag);
        assert_eq!(kind, RequestEventKind::Completed);
        assert_eq!(fk, None);
    }

    #[test]
    fn plain_json_upstream_error_body() {
        let body = r#"{"error":{"type":"invalid_request_error","message":"bad model"}}"#;
        let diag = parse_capture_diagnostics(body.as_bytes());
        assert_eq!(diag.error_type, "invalid_request_error");
        assert!(!diag.has_proxy_error);
        let (kind, fk, et, msg) = classify_stream_end(false, None, &diag);
        assert_eq!(kind, RequestEventKind::Failed);
        assert_eq!(fk, Some(FailureKind::UpstreamHttp));
        assert_eq!(et, "invalid_request_error");
        assert!(msg.contains("bad model"));
    }
}
