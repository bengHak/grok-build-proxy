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
    let input=b"event: response.created\r\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_chunks\"}}\r\n\r\nevent: response.output_text.delta\r\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"chunk safe\"}\r\n\r\ndata: [DONE]\r\n\r\n";
    let expected = normalize_sse(input, CompatMode::Full, "gpt-5.6-sol", "chunks");
    for size in 1..=input.len() {
        let mut normalizer = StreamNormalizer::new(CompatMode::Full, "gpt-5.6-sol", "chunks");
        let mut actual = Vec::new();
        for chunk in input.chunks(size) {
            actual.extend(normalizer.push(chunk));
        }
        actual.extend(normalizer.finish());
        assert_eq!(actual, expected, "chunk size {size}");
    }
}
