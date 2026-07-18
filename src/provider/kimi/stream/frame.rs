use serde_json::Value;

pub(super) fn frame_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|end| (end, 2));
    let crlf = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|end| (end, 4));
    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(boundary), None) | (None, Some(boundary)) => Some(boundary),
        (None, None) => None,
    }
}

pub(super) fn event_bytes(
    sequence: &mut u64,
    response_id: &str,
    kind: &str,
    fields: Value,
) -> Vec<u8> {
    let mut event = fields.as_object().cloned().unwrap_or_default();
    event.insert("type".into(), kind.into());
    event.insert("sequence_number".into(), (*sequence).into());
    event.insert("response_id".into(), response_id.into());
    *sequence += 1;
    format!("event: {kind}\ndata: {}\n\n", Value::Object(event)).into_bytes()
}
