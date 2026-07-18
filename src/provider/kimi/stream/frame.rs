use serde_json::Value;

pub(super) fn frame_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|end| (end, 2))
        .or_else(|| {
            buffer
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|end| (end, 4))
        })
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
