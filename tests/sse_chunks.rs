use grok_build_proxy::{
    proxy::CompatMode,
    sse::{StreamNormalizer, normalize_sse},
};
use serde_json::Value;

#[test]
fn created_event_uses_in_progress_response_status() {
    // Given
    let input = b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_created\"}}\n\n";

    // When
    let output = String::from_utf8(normalize_sse(
        input,
        CompatMode::Full,
        "gpt-5.6-sol",
        "created",
    ))
    .unwrap();
    let data = output
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .unwrap();
    let event: Value = serde_json::from_str(data).unwrap();

    // Then
    assert_eq!(event["response"]["status"], "in_progress");
}

#[test]
fn normalization_is_invariant_to_network_chunk_boundaries() {
    // Supply a fixed timestamp so the one-shot and incrementally chunked normalizers remain
    // comparable even when this loop crosses a wall-clock second boundary.
    let input=b"event: response.created\r\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_chunks\",\"created_at\":123}}\r\n\r\nevent: keepalive\r\ndata: {\"type\":\"keepalive\"}\r\n\r\nevent: response.output_text.delta\r\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"chunk safe\"}\r\n\r\ndata: [DONE]\r\n\r\n";
    let expected = normalize_sse(input, CompatMode::Full, "gpt-5.6-sol", "chunks");
    assert!(!String::from_utf8_lossy(&expected).contains("keepalive"));
    let expected = stable_sse_bytes(&expected);
    for size in 1..=input.len() {
        let mut normalizer = StreamNormalizer::new(CompatMode::Full, "gpt-5.6-sol", "chunks");
        let mut actual = Vec::new();
        for chunk in input.chunks(size) {
            actual.extend(normalizer.push(chunk));
        }
        actual.extend(normalizer.finish());
        assert_eq!(stable_sse_bytes(&actual), expected, "chunk size {size}");
    }
}

/// Zero wall-clock `created_at` values so comparisons are not flaky across second
/// boundaries (normalize injects `Utc::now()` when the field is missing).
fn stable_sse_bytes(bytes: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_ref();
    let marker = "\"created_at\":";
    while let Some(index) = rest.find(marker) {
        out.push_str(&rest[..index]);
        out.push_str(marker);
        out.push('0');
        rest = &rest[index + marker.len()..];
        while rest.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out.into_bytes()
}
