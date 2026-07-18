use grok_build_proxy::{
    catalog::Catalog,
    provider::{
        Provider,
        kimi::{
            auth::Store, request::translate_request, stream::Translator, validate_upstream_url,
        },
    },
};
use serde_json::{Value, json};

fn translate_stream(input: &[u8], chunk_size: usize) -> Vec<u8> {
    let mut translator = Translator::new("resp_kimi", "kimi-for-coding");
    let mut output = Vec::new();
    for chunk in input.chunks(chunk_size) {
        output.extend(translator.push(chunk));
    }
    output.extend(translator.finish());
    output
}

fn events(stream: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(stream)
        .split("\n\n")
        .filter_map(|frame| {
            frame
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .filter(|data| *data != "[DONE]")
                .map(|data| serde_json::from_str(data).unwrap())
        })
        .collect()
}

#[test]
fn kimi_catalog_aliases_resolve_to_the_canonical_wire_model() {
    let catalog = Catalog::default();

    for alias in ["kimi-for-coding", "kimi-k2.6", "k2.6"] {
        let (model, known) = catalog.lookup(alias);
        assert!(known);
        assert_eq!(model.provider, Provider::Kimi);
        assert_eq!(model.upstream_id, "kimi-for-coding");
    }
}

#[test]
fn kimi_request_translates_responses_history_tools_and_reasoning() {
    let request = json!({
        "model": "kimi-k2.6",
        "instructions": "system context",
        "input": [
            {"type":"message","role":"user","content":[
                {"type":"input_text","text":"hello"},
                {"type":"input_image","image_url":"data:image/png;base64,AA=="}
            ]},
            {"type":"reasoning","summary":[{"type":"summary_text","text":"prior thought"}]},
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"checking"}]},
            {"type":"function_call","call_id":"call_1","name":"search","arguments":"{\"q\":\"rust\"}"},
            {"type":"function_call","call_id":"call_2","name":"search","arguments":"{\"q\":\"axum\"}"},
            {"type":"function_call_output","call_id":"call_1","output":"result 1"},
            {"type":"function_call_output","call_id":"call_2","output":"result 2"}
        ],
        "tools": [{
            "type": "function",
            "name": "search",
            "description": "Search",
            "parameters": {"type":"object","properties":{"q":{"type":"string"}}}
        }],
        "tool_choice": {"type":"function","name":"search"},
        "reasoning": {"effort":"xhigh"},
        "max_output_tokens": 50000,
        "store": false
    });

    let translated =
        translate_request(&serde_json::to_vec(&request).unwrap(), "session-1").unwrap();

    assert_eq!(translated["model"], "kimi-for-coding");
    assert_eq!(translated["stream"], true);
    assert_eq!(translated["stream_options"]["include_usage"], true);
    assert_eq!(translated["max_tokens"], 32000);
    assert_eq!(translated["reasoning_effort"], "high");
    assert_eq!(translated["thinking"]["type"], "enabled");
    assert_eq!(translated["prompt_cache_key"], "session-1");
    assert_eq!(translated["messages"][0]["role"], "system");
    assert_eq!(translated["messages"][1]["role"], "user");
    assert_eq!(
        translated["messages"][1]["content"][1]["image_url"]["url"],
        "data:image/png;base64,AA=="
    );
    assert_eq!(
        translated["messages"][2]["reasoning_content"],
        "prior thought"
    );
    assert_eq!(translated["messages"][2]["content"], "checking");
    assert_eq!(translated["messages"][2]["tool_calls"][0]["id"], "call_1");
    assert_eq!(translated["messages"][2]["tool_calls"][1]["id"], "call_2");
    assert_eq!(translated["messages"][3]["tool_call_id"], "call_1");
    assert_eq!(translated["messages"][4]["tool_call_id"], "call_2");
    assert_eq!(translated["tools"][0]["function"]["name"], "search");
    assert_eq!(translated["tool_choice"]["function"]["name"], "search");
    assert!(translated.get("input").is_none());
    assert!(translated.get("store").is_none());
}

#[test]
fn kimi_stream_translates_reasoning_text_tools_and_usage() {
    let upstream = concat!(
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"think\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"KIMI_OK\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"search\",\"arguments\":\"{\\\"q\\\":\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"rust\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":4,\"total_tokens\":14}}\n\n",
        "data: [DONE]\n\n"
    );

    let translated = translate_stream(upstream.as_bytes(), upstream.len());
    let parsed = events(&translated);
    let kinds: Vec<_> = parsed
        .iter()
        .filter_map(|event| event["type"].as_str())
        .collect();

    assert!(kinds.contains(&"response.reasoning_summary_text.delta"));
    assert!(kinds.contains(&"response.output_text.delta"));
    assert!(kinds.contains(&"response.function_call_arguments.delta"));
    assert_eq!(kinds.last(), Some(&"response.completed"));

    for kind in [
        "response.reasoning_summary_text.delta",
        "response.output_text.delta",
        "response.function_call_arguments.delta",
    ] {
        let event = parsed.iter().find(|event| event["type"] == kind).unwrap();
        assert!(event["item_id"].as_str().is_some_and(|id| !id.is_empty()));
    }

    let completed = parsed.last().unwrap();
    assert_eq!(completed["response"]["usage"]["input_tokens"], 10);
    assert_eq!(completed["response"]["usage"]["output_tokens"], 4);
    assert_eq!(completed["response"]["usage"]["total_tokens"], 14);
    let output = completed["response"]["output"].as_array().unwrap();
    assert!(output.iter().any(|item| item["type"] == "reasoning"));
    assert!(
        output
            .iter()
            .any(|item| { item["type"] == "message" && item["content"][0]["text"] == "KIMI_OK" })
    );
    assert!(output.iter().any(|item| {
        item["type"] == "function_call"
            && item["call_id"] == "call_1"
            && item["name"] == "search"
            && item["arguments"] == "{\"q\":\"rust\"}"
    }));
}

#[test]
fn kimi_stream_translation_is_invariant_to_network_chunk_boundaries() {
    let upstream = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
        "data: {\"choices\":[{\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1}}\n\n",
        "data: [DONE]\n\n"
    )
    .as_bytes();
    let expected = translate_stream(upstream, upstream.len());

    for chunk_size in 1..=upstream.len() {
        assert_eq!(translate_stream(upstream, chunk_size), expected);
    }
}

#[test]
fn kimi_stream_parses_mixed_crlf_and_lf_frames_in_wire_order() {
    let upstream = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"mixed\"}}]}\r\n\r\n",
        "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let parsed = events(&translate_stream(upstream.as_bytes(), upstream.len()));
    assert!(parsed.iter().any(|event| {
        event["type"] == "response.output_text.delta" && event["delta"] == "mixed"
    }));
    assert_eq!(parsed.last().unwrap()["type"], "response.completed");
}

#[test]
fn kimi_stream_fails_closed_on_truncated_or_invalid_tool_calls() {
    for upstream in [
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"shell\",\"arguments\":\"{\\\"cmd\\\":\"}}]}}]}\n\n",
        concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"shell\",\"arguments\":\"not-json\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n"
        ),
    ] {
        let parsed = events(&translate_stream(upstream.as_bytes(), upstream.len()));
        let kinds: Vec<_> = parsed
            .iter()
            .filter_map(|event| event["type"].as_str())
            .collect();
        assert!(kinds.contains(&"response.failed"));
        assert!(!kinds.contains(&"response.completed"));
        assert!(!kinds.contains(&"response.output_item.done"));
    }
}

#[test]
fn kimi_stream_rejects_malformed_frames_and_reports_length_as_incomplete() {
    for (upstream, terminal) in [
        ("data: not-json\n\n", "response.failed"),
        (
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
                "data: {\"choices\":[{\"finish_reason\":\"length\"}]}\n\n",
                "data: [DONE]\n\n"
            ),
            "response.incomplete",
        ),
    ] {
        let parsed = events(&translate_stream(upstream.as_bytes(), upstream.len()));
        assert_eq!(parsed.last().unwrap()["type"], terminal);
        assert!(
            !parsed
                .iter()
                .any(|event| event["type"] == "response.completed")
        );
        if terminal == "response.incomplete" {
            assert_eq!(
                parsed.last().unwrap()["response"]["output"][0]["content"][0]["text"],
                "partial"
            );
        }
    }
}

#[test]
fn kimi_tool_item_is_added_once_when_the_first_argument_fragment_is_empty() {
    let upstream = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"search\",\"arguments\":\"\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let parsed = events(&translate_stream(upstream.as_bytes(), upstream.len()));
    let additions = parsed
        .iter()
        .filter(|event| {
            event["type"] == "response.output_item.added"
                && event["item"]["type"] == "function_call"
        })
        .count();
    assert_eq!(additions, 1);
    assert_eq!(parsed.last().unwrap()["type"], "response.completed");
}

#[test]
fn kimi_credential_endpoints_reject_untrusted_origins() {
    assert!(Store::new("auth.json", "https://auth.kimi.com").is_ok());
    assert!(Store::new("auth.json", "http://127.0.0.1:3000").is_ok());
    assert!(Store::new("auth.json", "http://auth.kimi.com").is_err());
    assert!(Store::new("auth.json", "https://example.com").is_err());

    assert!(validate_upstream_url("https://api.kimi.com/coding/v1/chat/completions").is_ok());
    assert!(validate_upstream_url("http://127.0.0.1:3000/chat/completions").is_ok());
    assert!(validate_upstream_url("http://api.kimi.com/coding/v1/chat/completions").is_err());
    assert!(validate_upstream_url("https://example.com/chat/completions").is_err());
}
