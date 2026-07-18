use serde_json::{Value, json};
use std::collections::BTreeMap;

mod delta;
mod finish;
mod frame;
mod tool;

use frame::frame_boundary;

const MAX_FRAME_BYTES: usize = 8 << 20;
const MAX_STREAM_BYTES: usize = 64 << 20;

#[derive(Clone, Debug)]
enum Output {
    Reasoning {
        id: String,
        text: String,
    },
    Message {
        id: String,
        text: String,
    },
    Tool {
        id: String,
        call_id: String,
        name: String,
        arguments: String,
        started: bool,
    },
}

#[derive(Debug)]
pub struct Translator {
    response_id: String,
    model: String,
    created_at: u64,
    buffer: Vec<u8>,
    received_bytes: usize,
    outputs: BTreeMap<usize, Output>,
    reasoning_index: Option<usize>,
    message_index: Option<usize>,
    tool_indexes: BTreeMap<usize, usize>,
    next_index: usize,
    sequence: u64,
    usage: Value,
    finish_reason: Option<String>,
    started: bool,
    terminal: bool,
    terminal_response: Option<Value>,
}

impl Translator {
    pub fn new(response_id: &str, model: &str) -> Self {
        Self {
            response_id: response_id.into(),
            model: model.into(),
            created_at: chrono::Utc::now().timestamp().max(0) as u64,
            buffer: Vec::new(),
            received_bytes: 0,
            outputs: BTreeMap::new(),
            reasoning_index: None,
            message_index: None,
            tool_indexes: BTreeMap::new(),
            next_index: 0,
            sequence: 0,
            usage: json!({
                "input_tokens":0,
                "input_tokens_details":{"cached_tokens":0},
                "output_tokens":0,
                "output_tokens_details":{"reasoning_tokens":0},
                "total_tokens":0
            }),
            finish_reason: None,
            started: false,
            terminal: false,
            terminal_response: None,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        if self.terminal {
            return Vec::new();
        }
        self.received_bytes = self.received_bytes.saturating_add(chunk.len());
        if self.received_bytes > MAX_STREAM_BYTES {
            return self.fail(&json!({
                "type":"upstream_error",
                "message":"Kimi stream exceeded the 64 MiB safety limit"
            }));
        }
        self.buffer.extend_from_slice(chunk);
        let mut output = Vec::new();
        while let Some((end, delimiter)) = frame_boundary(&self.buffer) {
            let frame = self.buffer.drain(..end).collect::<Vec<_>>();
            self.buffer.drain(..delimiter);
            let data = String::from_utf8_lossy(&frame)
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim_start)
                .collect::<Vec<_>>()
                .join("\n");
            output.extend(self.consume(&data));
        }
        if self.buffer.len() > MAX_FRAME_BYTES {
            output.extend(self.fail(&json!({
                "type":"upstream_error",
                "message":"Kimi SSE frame exceeded the 8 MiB safety limit"
            })));
            self.buffer.clear();
        }
        output
    }

    pub fn finish(&mut self) -> Vec<u8> {
        if self.terminal {
            Vec::new()
        } else if self.buffer.iter().any(|byte| !byte.is_ascii_whitespace()) {
            self.buffer.clear();
            self.fail(&json!({
                "type":"upstream_error",
                "message":"Kimi stream ended with an incomplete SSE frame"
            }))
        } else {
            self.terminal()
        }
    }

    pub fn terminal_response(&self) -> Option<&Value> {
        self.terminal_response.as_ref()
    }

    fn consume(&mut self, data: &str) -> Vec<u8> {
        if self.terminal {
            return Vec::new();
        }
        if data.is_empty() {
            return Vec::new();
        }
        if data == "[DONE]" {
            return self.terminal();
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            return self.fail(&json!({
                "type":"upstream_error",
                "message":"Kimi returned malformed SSE JSON"
            }));
        };
        if let Some(error) = chunk.get("error") {
            return self.fail(error);
        }
        let mut output = self.start();
        if let Some(usage) = chunk.get("usage") {
            self.usage = json!({
                "input_tokens":usage.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
                "input_tokens_details":{
                    "cached_tokens":usage.pointer("/prompt_tokens_details/cached_tokens").and_then(Value::as_u64).unwrap_or(0)
                },
                "output_tokens":usage.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0),
                "output_tokens_details":{
                    "reasoning_tokens":usage.pointer("/completion_tokens_details/reasoning_tokens").and_then(Value::as_u64).unwrap_or(0)
                },
                "total_tokens":usage.get("total_tokens").and_then(Value::as_u64).unwrap_or_else(|| {
                    usage.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0)
                        + usage.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0)
                }),
            });
        }
        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|v| v.first())
        else {
            return output;
        };
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish_reason = Some(reason.into());
        }
        let Some(delta) = choice.get("delta") else {
            return output;
        };
        if let Some(text) = delta
            .get("reasoning_content")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            output.extend(self.reasoning_delta(text));
        }
        if let Some(text) = delta
            .get("content")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            output.extend(self.text_delta(text));
        }
        if let Some(tools) = delta.get("tool_calls").and_then(Value::as_array) {
            for tool in tools {
                output.extend(self.tool_delta(tool));
            }
        }
        output
    }
}
