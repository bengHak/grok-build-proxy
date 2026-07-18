use serde_json::{Value, json};
use std::collections::BTreeMap;

mod finish;
mod frame;
mod tool;

use frame::{event_bytes, frame_boundary};

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
    },
}

#[derive(Debug)]
pub struct Translator {
    response_id: String,
    model: String,
    buffer: Vec<u8>,
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
            buffer: Vec::new(),
            outputs: BTreeMap::new(),
            reasoning_index: None,
            message_index: None,
            tool_indexes: BTreeMap::new(),
            next_index: 0,
            sequence: 0,
            usage: json!({"input_tokens":0,"output_tokens":0,"total_tokens":0}),
            finish_reason: None,
            started: false,
            terminal: false,
            terminal_response: None,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
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
        output
    }

    pub fn finish(&mut self) -> Vec<u8> {
        if self.terminal {
            Vec::new()
        } else {
            self.complete()
        }
    }

    pub fn terminal_response(&self) -> Option<&Value> {
        self.terminal_response.as_ref()
    }

    fn consume(&mut self, data: &str) -> Vec<u8> {
        if data.is_empty() {
            return Vec::new();
        }
        if data == "[DONE]" {
            return self.complete();
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            return Vec::new();
        };
        if let Some(error) = chunk.get("error") {
            return self.fail(error);
        }
        let mut output = self.start();
        if let Some(usage) = chunk.get("usage") {
            self.usage = json!({
                "input_tokens":usage.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
                "output_tokens":usage.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0),
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

    fn start(&mut self) -> Vec<u8> {
        if self.started {
            return Vec::new();
        }
        self.started = true;
        self.event(
            "response.created",
            json!({"response":self.response("in_progress",Vec::new())}),
        )
    }

    fn reasoning_delta(&mut self, delta: &str) -> Vec<u8> {
        let index = match self.reasoning_index {
            Some(index) => index,
            None => {
                let index = self.allocate(Output::Reasoning {
                    id: format!("rs_{}", self.response_id),
                    text: String::new(),
                });
                self.reasoning_index = Some(index);
                index
            }
        };
        let mut output = Vec::new();
        let item_id = match self.outputs.get_mut(&index) {
            Some(Output::Reasoning { id, text }) => {
                if text.is_empty() {
                    output.extend(event_bytes(&mut self.sequence, &self.response_id, "response.output_item.added", json!({"output_index":index,"item":{"id":id,"type":"reasoning","status":"in_progress","summary":[]}})));
                    output.extend(event_bytes(&mut self.sequence, &self.response_id, "response.reasoning_summary_part.added", json!({"item_id":id,"output_index":index,"summary_index":0,"part":{"type":"summary_text","text":""}})));
                }
                text.push_str(delta);
                id.clone()
            }
            _ => return output,
        };
        output.extend(self.event(
            "response.reasoning_summary_text.delta",
            json!({"item_id":item_id,"output_index":index,"summary_index":0,"delta":delta}),
        ));
        output
    }

    fn text_delta(&mut self, delta: &str) -> Vec<u8> {
        let index = match self.message_index {
            Some(index) => index,
            None => {
                let index = self.allocate(Output::Message {
                    id: format!("msg_{}", self.response_id),
                    text: String::new(),
                });
                self.message_index = Some(index);
                index
            }
        };
        let mut output = Vec::new();
        let item_id = match self.outputs.get_mut(&index) {
            Some(Output::Message { id, text }) => {
                if text.is_empty() {
                    output.extend(event_bytes(&mut self.sequence, &self.response_id, "response.output_item.added", json!({"output_index":index,"item":{"id":id,"type":"message","status":"in_progress","role":"assistant","content":[]}})));
                    output.extend(event_bytes(&mut self.sequence, &self.response_id, "response.content_part.added", json!({"item_id":id,"output_index":index,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}})));
                }
                text.push_str(delta);
                id.clone()
            }
            _ => return output,
        };
        output.extend(self.event(
            "response.output_text.delta",
            json!({"item_id":item_id,"output_index":index,"content_index":0,"delta":delta}),
        ));
        output
    }

    fn allocate(&mut self, state: Output) -> usize {
        let index = self.next_index;
        self.next_index += 1;
        self.outputs.insert(index, state);
        index
    }

    fn event(&mut self, kind: &str, fields: Value) -> Vec<u8> {
        event_bytes(&mut self.sequence, &self.response_id, kind, fields)
    }
}
