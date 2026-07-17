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

/// Sanitize monitor-facing strings: strip control chars, cap length.
pub fn sanitize(value: &str) -> String {
    let count = value.chars().count();
    let mut out: String = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(256)
        .collect();
    out = out.trim().to_owned();
    if count > 256 {
        out.pop();
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
}

/// Extract failure diagnostics from a capture tail (SSE frames or JSON body).
pub fn parse_capture_diagnostics(bytes: &[u8]) -> CaptureDiagnostics {
    let text = String::from_utf8_lossy(bytes);
    let mut diag = CaptureDiagnostics::default();

    // response_id: last occurrence of "id":"resp_..." or "response_id":"..."
    for marker in ["\"response_id\"", "\"id\""] {
        for (index, _) in text.match_indices(marker) {
            if let Some(id) = extract_json_string_after(&text[index + marker.len()..]) {
                if id.starts_with("resp_") || marker == "\"response_id\"" {
                    diag.response_id = sanitize(&id);
                }
            }
        }
    }

    // Count output items roughly via "type":"function_call" / message / custom_tool_call in output
    // Prefer response.output array length if present; fallback to item type markers.
    if let Some(count) = count_output_items(&text) {
        diag.output_count = count;
    }

    // Walk SSE-ish frames and JSON fragments for terminal / error markers.
    for frame in text.split("\n\n") {
        let mut event_name = None;
        let mut data_lines = Vec::new();
        for line in frame.lines() {
            if let Some(v) = line.strip_prefix("event:") {
                event_name = Some(v.trim());
            } else if let Some(v) = line.strip_prefix("data:") {
                data_lines.push(v.strip_prefix(' ').unwrap_or(v));
            }
        }
        let data = if data_lines.is_empty() {
            frame.trim()
        } else {
            // join without allocating much for large frames — data is already in text
            // Use joined only when multi-line data.
            if data_lines.len() == 1 {
                data_lines[0]
            } else {
                // fall through via temporary
                ""
            }
        };
        let joined;
        let data = if data.is_empty() && !data_lines.is_empty() {
            joined = data_lines.join("\n");
            joined.as_str()
        } else if data.is_empty() {
            continue;
        } else {
            data
        };
        if data == "[DONE]" {
            continue;
        }

        let typ = event_name
            .map(str::to_owned)
            .or_else(|| extract_json_string_field(data, "type"))
            .unwrap_or_default();

        match typ.as_str() {
            "response.failed" | "response.incomplete" => {
                diag.has_stream_terminal_failure = true;
                diag.terminal_event = typ.clone();
                if let Some(msg) = extract_nested_error_message(data) {
                    if diag.error_message.is_empty() {
                        diag.error_message = sanitize(&msg);
                    }
                }
                if let Some(et) = extract_nested_error_type(data) {
                    diag.error_type = sanitize(&et);
                    if et.starts_with("proxy_") {
                        diag.has_proxy_error = true;
                    }
                }
            }
            "error" => {
                diag.has_stream_terminal_failure = true;
                diag.terminal_event = "error".into();
                if let Some(et) = extract_json_string_field(data, "type")
                    .filter(|t| t != "error")
                    .or_else(|| extract_nested_error_type(data))
                {
                    diag.error_type = sanitize(&et);
                    if et.starts_with("proxy_") {
                        diag.has_proxy_error = true;
                    }
                } else if let Some(et) = extract_nested_error_type(data) {
                    diag.error_type = sanitize(&et);
                    if et.starts_with("proxy_") {
                        diag.has_proxy_error = true;
                    }
                }
                // error event often: {"type":"error","error":{"type":"...","message":"..."}}
                if let Some(et) = extract_nested_error_type(data) {
                    diag.error_type = sanitize(&et);
                    if et.starts_with("proxy_") {
                        diag.has_proxy_error = true;
                    }
                }
                if let Some(msg) = extract_nested_error_message(data)
                    .or_else(|| extract_json_string_field(data, "message"))
                {
                    diag.error_message = sanitize(&msg);
                }
            }
            _ => {
                // Non-SSE JSON body or embedded error object
                if let Some(et) = extract_nested_error_type(data) {
                    if et.starts_with("proxy_") {
                        diag.has_proxy_error = true;
                        diag.error_type = sanitize(&et);
                        if let Some(msg) = extract_nested_error_message(data) {
                            diag.error_message = sanitize(&msg);
                        }
                    } else if diag.error_type.is_empty() {
                        diag.error_type = sanitize(&et);
                        if let Some(msg) = extract_nested_error_message(data) {
                            diag.error_message = sanitize(&msg);
                        }
                    }
                }
            }
        }
    }

    // Also scan whole text for proxy_ error types (synthetic frames may nest oddly)
    if diag.error_type.is_empty() || !diag.has_proxy_error {
        for marker in [
            "proxy_incomplete_output",
            "proxy_missing_terminal_output",
            "proxy_",
        ] {
            if let Some(pos) = text.find(marker) {
                // extract surrounding quoted type if possible
                let slice = &text[pos..];
                let end = slice
                    .find(|c: char| {
                        c == '"' || c == '\'' || c.is_whitespace() || c == ',' || c == '}'
                    })
                    .unwrap_or(slice.len().min(64));
                let et = &slice[..end];
                if et.starts_with("proxy_") {
                    diag.has_proxy_error = true;
                    if diag.error_type.is_empty() {
                        diag.error_type = sanitize(et);
                    }
                }
            }
        }
    }

    // Whole-body JSON error.type for non-SSE upstream error responses
    if diag.error_type.is_empty() {
        if let Some(et) = extract_nested_error_type(&text) {
            diag.error_type = sanitize(&et);
            if et.starts_with("proxy_") {
                diag.has_proxy_error = true;
            }
        }
    }
    if diag.error_message.is_empty() {
        if let Some(msg) = extract_nested_error_message(&text) {
            diag.error_message = sanitize(&msg);
        }
    }

    if diag.has_proxy_error {
        diag.has_stream_terminal_failure = true;
        if diag.terminal_event.is_empty() {
            diag.terminal_event = "error".into();
        }
    }

    diag
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
    // Look for "output":[ ... ] and count top-level objects roughly via "type":
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
        // HTTP failure path — finer auth/connect handled by callers
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
    if diag.has_proxy_error || diag.error_type.starts_with("proxy_") {
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
    fn classifies_stream_io() {
        let diag = CaptureDiagnostics::default();
        let (kind, fk, et, msg) = classify_stream_end(true, Some("connection reset"), &diag);
        assert_eq!(kind, RequestEventKind::Failed);
        assert_eq!(fk, Some(FailureKind::StreamIo));
        assert_eq!(et, "stream_io");
        assert!(msg.contains("connection reset"));
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
        let (kind, fk, _, _) = classify_stream_end(true, None, &diag);
        assert_eq!(kind, RequestEventKind::Completed);
        assert_eq!(fk, None);
        assert_eq!(diag.response_id, "resp_ok");
    }
}
