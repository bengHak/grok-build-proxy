use crate::proxy::CompatMode;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use tracing::warn;

const MAX_STATE_BYTES: usize = 16 << 20;

#[derive(Clone, Default)]
struct OutputState {
    item: Value,
    id: String,
    call_id: String,
    kind: String,
    text: String,
    refusal: String,
    arguments: String,
    input: String,
    input_seen: bool,
    item_done: bool,
    arguments_done: bool,
    input_done: bool,
}

#[derive(Default)]
struct Assembler {
    mode: Option<CompatMode>,
    model: String,
    response_id: String,
    snapshot: Option<Value>,
    seq: u64,
    next_index: usize,
    outputs: BTreeMap<usize, OutputState>,
    by_item: HashMap<String, usize>,
    by_call: HashMap<String, usize>,
    terminal: bool,
    state_bytes: usize,
}

impl Assembler {
    fn new(mode: CompatMode, model: &str, request_id: &str) -> Self {
        Self {
            mode: Some(mode),
            model: model.into(),
            response_id: format!("resp_{request_id}"),
            ..Default::default()
        }
    }

    fn allocate(&mut self, event: &Value, compatible: &[&str]) -> usize {
        if let Some(index) = event.get("output_index").and_then(Value::as_u64) {
            self.next_index = self.next_index.max(index as usize + 1);
            return index as usize;
        }
        if let Some(id) = event
            .get("item_id")
            .and_then(Value::as_str)
            .and_then(|id| self.by_item.get(id))
        {
            return *id;
        }
        if let Some(id) = event
            .get("call_id")
            .and_then(Value::as_str)
            .and_then(|id| self.by_call.get(id))
        {
            return *id;
        }
        if let Some(item) = event.get("item") {
            if let Some(id) = item
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| self.by_item.get(id))
            {
                return *id;
            }
            if let Some(id) = item
                .get("call_id")
                .and_then(Value::as_str)
                .and_then(|id| self.by_call.get(id))
            {
                return *id;
            }
        }
        if compatible.is_empty() {
            let index = self.next_index;
            self.next_index += 1;
            return index;
        }
        let candidates: Vec<_> = self
            .outputs
            .iter()
            .filter(|(_, state)| compatible.is_empty() || compatible.contains(&state.kind.as_str()))
            .map(|(i, _)| *i)
            .collect();
        if candidates.len() == 1 {
            return candidates[0];
        }
        let index = self.next_index;
        self.next_index += 1;
        index
    }

    fn bind(&mut self, index: usize, event: &Value) {
        let state = self.outputs.entry(index).or_default();
        let item = event.get("item");
        let id = event
            .get("item_id")
            .and_then(Value::as_str)
            .or_else(|| item.and_then(|v| v.get("id")).and_then(Value::as_str));
        let call = event
            .get("call_id")
            .and_then(Value::as_str)
            .or_else(|| item.and_then(|v| v.get("call_id")).and_then(Value::as_str));
        if let Some(id) = id.filter(|s| !s.is_empty()) {
            state.id = id.into();
            self.by_item.insert(id.into(), index);
        }
        if let Some(call) = call.filter(|s| !s.is_empty()) {
            state.call_id = call.into();
            self.by_call.insert(call.into(), index);
        }
    }

    fn normalize(
        &mut self,
        mut event: Value,
        event_name: &str,
    ) -> Result<Option<Value>, &'static str> {
        let typ = event
            .get("type")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(event_name)
            .to_owned();
        if typ == "response.metadata" {
            return Ok(None);
        }
        if !typ.starts_with("response.") && typ != "error" {
            return Ok(Some(event));
        }
        if let Some(n) = event.get("sequence_number").and_then(Value::as_u64) {
            self.seq = self.seq.max(n);
        } else {
            self.seq += 1;
            event
                .as_object_mut()
                .ok_or("proxy_stream_state_error")?
                .insert("sequence_number".into(), self.seq.into());
        }
        event
            .as_object_mut()
            .ok_or("proxy_stream_state_error")?
            .entry("type")
            .or_insert(typ.clone().into());
        if let Some(id) = event
            .pointer("/response/id")
            .and_then(Value::as_str)
            .or_else(|| event.get("response_id").and_then(Value::as_str))
            .filter(|s| !s.is_empty())
        {
            self.response_id = id.into();
        }
        event
            .as_object_mut()
            .unwrap()
            .entry("response_id")
            .or_insert(self.response_id.clone().into());

        if matches!(
            typ.as_str(),
            "response.created" | "response.in_progress" | "response.queued"
        ) {
            if let Some(response) = event.get_mut("response") {
                normalize_response(
                    response,
                    &self.response_id,
                    &self.model,
                    if typ == "response.created" {
                        "in_progress"
                    } else {
                        typ.trim_start_matches("response.")
                    },
                );
                self.snapshot = Some(response.clone());
            }
        }

        let compatible: &[&str] =
            match typ.as_str() {
                "response.output_text.delta" | "response.output_text.done" => &["message"],
                "response.refusal.delta" | "response.refusal.done" => &["message"],
                "response.function_call_arguments.delta"
                | "response.function_call_arguments.done" => &["function_call"],
                "response.custom_tool_call_input.delta"
                | "response.custom_tool_call_input.done" => &["custom_tool_call"],
                _ => &[],
            };
        if !compatible.is_empty()
            || matches!(
                typ.as_str(),
                "response.output_item.added" | "response.output_item.done"
            )
        {
            let index = self.allocate(&event, compatible);
            self.bind(index, &event);
            event
                .as_object_mut()
                .unwrap()
                .insert("output_index".into(), index.into());
            let state = self.outputs.entry(index).or_default();
            match typ.as_str() {
                "response.output_item.added" | "response.output_item.done" => {
                    if let Some(item) = event.get("item") {
                        state.item = item.clone();
                        state.kind = item
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .into();
                        state.id = item
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or(&state.id)
                            .into();
                        state.call_id = item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .unwrap_or(&state.call_id)
                            .into();
                        if let Some(v) = item.get("arguments").and_then(Value::as_str) {
                            state.arguments = v.into()
                        }
                        if let Some(v) = item.get("input").and_then(Value::as_str) {
                            state.input = v.into();
                            state.input_seen = true
                        }
                    }
                    state.item_done = typ.ends_with(".done");
                }
                "response.output_text.delta" => {
                    if let Some(v) = event.get("delta").and_then(Value::as_str) {
                        state.kind = "message".into();
                        state.text.push_str(v);
                        self.state_bytes += v.len()
                    }
                }
                "response.output_text.done" => {
                    state.kind = "message".into();
                    if event
                        .get("text")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                    {
                        event
                            .as_object_mut()
                            .unwrap()
                            .insert("text".into(), state.text.clone().into());
                    }
                }
                "response.refusal.delta" => {
                    if let Some(v) = event.get("delta").and_then(Value::as_str) {
                        state.kind = "message".into();
                        state.refusal.push_str(v);
                        self.state_bytes += v.len()
                    }
                }
                "response.refusal.done" => {
                    state.kind = "message".into();
                    if event
                        .get("refusal")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                    {
                        event
                            .as_object_mut()
                            .unwrap()
                            .insert("refusal".into(), state.refusal.clone().into());
                    }
                }
                "response.function_call_arguments.delta" if self.mode == Some(CompatMode::Full) => {
                    if let Some(v) = event.get("delta").and_then(Value::as_str) {
                        state.kind = "function_call".into();
                        state.arguments.push_str(v);
                        self.state_bytes += v.len()
                    }
                }
                "response.function_call_arguments.done" if self.mode == Some(CompatMode::Full) => {
                    state.kind = "function_call".into();
                    state.arguments_done = true;
                    let done = event.get("arguments").and_then(Value::as_str).unwrap_or("");
                    if valid_json(done) {
                        state.arguments = done.into()
                    } else if valid_json(&state.arguments) {
                        event
                            .as_object_mut()
                            .unwrap()
                            .insert("arguments".into(), state.arguments.clone().into());
                    }
                }
                "response.custom_tool_call_input.delta" if self.mode == Some(CompatMode::Full) => {
                    if let Some(v) = event.get("delta").and_then(Value::as_str) {
                        state.kind = "custom_tool_call".into();
                        state.input.push_str(v);
                        state.input_seen = true;
                        self.state_bytes += v.len()
                    }
                }
                "response.custom_tool_call_input.done" if self.mode == Some(CompatMode::Full) => {
                    state.kind = "custom_tool_call".into();
                    state.input_done = true;
                    if let Some(done) = event.get("input").and_then(Value::as_str) {
                        state.input_seen = true;
                        if !done.is_empty() || state.input.is_empty() {
                            state.input = done.into()
                        } else {
                            event
                                .as_object_mut()
                                .unwrap()
                                .insert("input".into(), state.input.clone().into());
                        }
                    } else if state.input_seen {
                        event
                            .as_object_mut()
                            .unwrap()
                            .insert("input".into(), state.input.clone().into());
                    }
                }
                _ => {}
            }
            if state.id.is_empty() {
                state.id = match state.kind.as_str() {
                    "function_call" => format!(
                        "fc_{}",
                        if state.call_id.is_empty() {
                            format!("{index}")
                        } else {
                            state.call_id.clone()
                        }
                    ),
                    "custom_tool_call" => format!(
                        "ct_{}",
                        if state.call_id.is_empty() {
                            format!("{index}")
                        } else {
                            state.call_id.clone()
                        }
                    ),
                    _ => format!(
                        "msg_{}_{index}",
                        self.response_id.trim_start_matches("resp_")
                    ),
                };
            }
            event
                .as_object_mut()
                .unwrap()
                .entry("item_id")
                .or_insert(state.id.clone().into());
            if typ.contains("output_text") || typ.contains("refusal") {
                event
                    .as_object_mut()
                    .unwrap()
                    .entry("content_index")
                    .or_insert(0.into());
            }
        }
        if matches!(
            typ.as_str(),
            "response.content_part.added"
                | "response.content_part.done"
                | "response.reasoning_summary_part.added"
                | "response.reasoning_summary_part.done"
                | "response.reasoning_summary_text.delta"
                | "response.reasoning_summary_text.done"
        ) || typ.starts_with("response.web_search_call.")
            || typ.starts_with("response.file_search_call.")
            || typ.starts_with("response.image_generation_call.")
            || typ.starts_with("response.code_interpreter_call.")
            || typ.starts_with("response.mcp_")
        {
            let kind = if typ.contains("reasoning_summary") {
                "reasoning"
            } else if typ.contains("content_part") {
                "message"
            } else {
                "auxiliary"
            };
            let index = self.allocate(&event, &[kind]);
            self.bind(index, &event);
            let state = self.outputs.entry(index).or_default();
            if state.kind.is_empty() {
                state.kind = kind.into();
            }
            if state.id.is_empty() {
                state.id = format!(
                    "item_{}_{index}",
                    self.response_id.trim_start_matches("resp_")
                );
            }
            let object = event.as_object_mut().unwrap();
            object.entry("output_index").or_insert(index.into());
            object.entry("item_id").or_insert(state.id.clone().into());
            if typ.contains("content_part") {
                object.entry("content_index").or_insert(0.into());
                let part = object
                    .entry("part")
                    .or_insert_with(|| json!({"type":"output_text","text":""}));
                if !part.is_object() {
                    *part = json!({"type":"output_text","text":""});
                }
                if typ.ends_with(".done")
                    && part
                        .get("text")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                {
                    part.as_object_mut()
                        .unwrap()
                        .insert("text".into(), state.text.clone().into());
                }
            }
            if typ.contains("reasoning_summary") {
                object.entry("summary_index").or_insert(0.into());
                if typ.contains("_part.") {
                    object
                        .entry("part")
                        .or_insert_with(|| json!({"type":"summary_text","text":""}));
                }
            }
        }
        if self.state_bytes > MAX_STATE_BYTES {
            return Err("proxy_stream_state_error");
        }

        match typ.as_str() {
            "response.completed" => {
                self.terminal = true;
                let response = event
                    .get_mut("response")
                    .ok_or("proxy_invalid_terminal_response")?;
                normalize_response(response, &self.response_id, &self.model, "completed");
                self.fill_terminal(response)?;
            }
            "response.incomplete" | "response.failed" => {
                self.terminal = true;
                if let Some(response) = event.get_mut("response") {
                    normalize_response(
                        response,
                        &self.response_id,
                        &self.model,
                        typ.trim_start_matches("response."),
                    );
                }
            }
            "error" => self.terminal = true,
            _ => {}
        }
        Ok(Some(event))
    }

    fn fill_terminal(&self, response: &mut Value) -> Result<(), &'static str> {
        let existing = response
            .get("output")
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty())
            .cloned();
        let output = if existing
            .as_ref()
            .is_some_and(|items| items.iter().all(valid_output))
        {
            existing.unwrap()
        } else {
            self.build_outputs().unwrap_or_default()
        };
        if output.is_empty() {
            return Err(if self.outputs.is_empty() {
                "proxy_missing_terminal_output"
            } else {
                "proxy_incomplete_output"
            });
        }
        if output.iter().any(|item| !valid_output(item)) {
            return Err("proxy_incomplete_output");
        }
        response
            .as_object_mut()
            .unwrap()
            .insert("output".into(), output.into());
        Ok(())
    }

    fn build_outputs(&self) -> Result<Vec<Value>, &'static str> {
        let mut result = Vec::new();
        for (index, state) in &self.outputs {
            match state.kind.as_str() {
                "message" => {
                    let mut content = Vec::new();
                    if !state.text.is_empty() {
                        content.push(json!({"type":"output_text","text":state.text,"annotations":[],"logprobs":[]}));
                    }
                    if !state.refusal.is_empty() {
                        content.push(json!({"type":"refusal","refusal":state.refusal}));
                    }
                    if !content.is_empty() {
                        result.push(json!({"id":state.id,"type":"message","status":"completed","role":"assistant","content":content}));
                    }
                }
                "function_call" if self.mode == Some(CompatMode::Full) => {
                    let name = state.item.get("name").and_then(Value::as_str).unwrap_or("");
                    let call = if state.call_id.is_empty() {
                        state
                            .item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                    } else {
                        &state.call_id
                    };
                    let args = if valid_json(&state.arguments) {
                        &state.arguments
                    } else {
                        state
                            .item
                            .get("arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                    };
                    if name.is_empty()
                        || call.is_empty()
                        || !valid_json(args)
                        || !(state.item_done || state.arguments_done)
                    {
                        return Err("proxy_incomplete_output");
                    }
                    result.push(json!({"id":state.id,"type":"function_call","status":"completed","name":name,"call_id":call,"arguments":args}));
                }
                "custom_tool_call" if self.mode == Some(CompatMode::Full) => {
                    let name = state.item.get("name").and_then(Value::as_str).unwrap_or("");
                    let call = if state.call_id.is_empty() {
                        state
                            .item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                    } else {
                        &state.call_id
                    };
                    if name.is_empty()
                        || call.is_empty()
                        || !state.input_seen
                        || !(state.item_done || state.input_done)
                    {
                        return Err("proxy_incomplete_output");
                    }
                    result.push(json!({"id":state.id,"type":"custom_tool_call","status":"completed","name":name,"call_id":call,"input":state.input}));
                }
                "function_call" | "custom_tool_call" => return Err("proxy_incomplete_output"),
                _ => {
                    let _ = index;
                }
            }
        }
        Ok(result)
    }

    fn synthetic_terminal(&mut self) -> Option<Value> {
        if self.terminal {
            return None;
        }
        self.terminal = true;
        let mut response = self.snapshot.clone().unwrap_or_else(|| json!({}));
        normalize_response(&mut response, &self.response_id, &self.model, "completed");
        match self.fill_terminal(&mut response) {
            Ok(()) => {
                self.seq += 1;
                Some(
                    json!({"type":"response.completed","sequence_number":self.seq,"response_id":self.response_id,"response":response}),
                )
            }
            Err(kind) => Some(self.error(kind)),
        }
    }
    fn error(&mut self, kind: &str) -> Value {
        self.terminal = true;
        self.seq += 1;
        warn!(
            error_type = kind,
            response_id = self.response_id,
            output_state_count = self.outputs.len(),
            buffered_state_bytes = self.state_bytes,
            "response stream normalization failed"
        );
        json!({"type":"error","sequence_number":self.seq,"response_id":self.response_id,"error":{"type":kind,"message":"The proxy could not safely assemble a complete Responses stream."}})
    }
}

fn valid_json(value: &str) -> bool {
    !value.is_empty() && serde_json::from_str::<Value>(value).is_ok()
}
fn valid_output(item: &Value) -> bool {
    match item.get("type").and_then(Value::as_str) {
        Some("message") => item
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|c| {
                c.iter().any(|p| {
                    p.get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.is_empty())
                        || p.get("refusal")
                            .and_then(Value::as_str)
                            .is_some_and(|s| !s.is_empty())
                })
            }),
        Some("function_call") => {
            !item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .is_empty()
                && !item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .is_empty()
                && !item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .is_empty()
                && item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .is_some_and(valid_json)
        }
        Some("custom_tool_call") => {
            !item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .is_empty()
                && !item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .is_empty()
                && !item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .is_empty()
                && item.get("input").and_then(Value::as_str).is_some()
        }
        _ => false,
    }
}
fn normalize_response(response: &mut Value, id: &str, model: &str, status: &str) {
    if !response.is_object() {
        *response = json!({})
    }
    let o = response.as_object_mut().unwrap();
    o.insert("object".into(), "response".into());
    o.insert("status".into(), status.into());
    if o.get("id")
        .and_then(Value::as_str)
        .is_none_or(|s| s.is_empty())
    {
        o.insert("id".into(), id.into());
    }
    if o.get("model")
        .and_then(Value::as_str)
        .is_none_or(|s| s.is_empty())
    {
        o.insert(
            "model".into(),
            if model.is_empty() {
                "unknown".into()
            } else {
                model.into()
            },
        );
    }
    if o.get("created_at").and_then(Value::as_u64).is_none() {
        o.insert(
            "created_at".into(),
            chrono::Utc::now().timestamp().max(0).into(),
        );
    }
    if !o.get("output").is_some_and(Value::is_array) {
        o.insert("output".into(), json!([]));
    }
    if let Some(usage) = o.get_mut("usage") {
        normalize_usage(usage)
    }
}
fn normalize_usage(usage: &mut Value) {
    if usage.is_null() {
        return;
    }
    if !usage.is_object() {
        *usage = Value::Null;
        return;
    }
    let o = usage.as_object_mut().unwrap();
    let input = o.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
    let output = o.get("output_tokens").and_then(Value::as_u64).unwrap_or(0);
    o.insert("input_tokens".into(), input.into());
    o.insert("output_tokens".into(), output.into());
    o.insert(
        "total_tokens".into(),
        o.get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(input + output)
            .into(),
    );
    let input_details = o.entry("input_tokens_details").or_insert_with(|| json!({}));
    if !input_details.is_object() {
        *input_details = json!({});
    }
    let input_object = input_details.as_object_mut().unwrap();
    if input_object
        .get("cached_tokens")
        .is_none_or(|value| value.as_u64().is_none())
    {
        input_object.insert("cached_tokens".into(), 0.into());
    }
    let output_details = o
        .entry("output_tokens_details")
        .or_insert_with(|| json!({}));
    if !output_details.is_object() {
        *output_details = json!({});
    }
    let output_object = output_details.as_object_mut().unwrap();
    if output_object
        .get("reasoning_tokens")
        .is_none_or(|value| value.as_u64().is_none())
    {
        output_object.insert("reasoning_tokens".into(), 0.into());
    }
}

fn encode(event: &Value) -> String {
    let typ = event.get("type").and_then(Value::as_str).unwrap_or("");
    format!(
        "event: {typ}\ndata: {}\n\n",
        serde_json::to_string(event).unwrap()
    )
}

pub struct StreamNormalizer {
    assembler: Assembler,
    pending: Vec<u8>,
}

impl StreamNormalizer {
    pub fn new(mode: CompatMode, model: &str, request_id: &str) -> Self {
        Self {
            assembler: Assembler::new(mode, model, request_id),
            pending: Vec::new(),
        }
    }
    pub fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(chunk);
        let mut output = Vec::new();
        while let Some((position, separator_len)) = frame_boundary(&self.pending) {
            let frame: Vec<u8> = self.pending.drain(..position + separator_len).collect();
            output.extend(self.process_frame(&frame));
        }
        output
    }
    pub fn finish(&mut self) -> Vec<u8> {
        let mut output = if self.pending.is_empty() {
            Vec::new()
        } else {
            let frame = std::mem::take(&mut self.pending);
            self.process_frame(&frame)
        };
        if !self.assembler.terminal {
            if let Some(terminal) = self.assembler.synthetic_terminal() {
                output.extend(encode(&terminal).into_bytes());
            }
        }
        output
    }
    fn process_frame(&mut self, frame: &[u8]) -> Vec<u8> {
        let raw = String::from_utf8_lossy(frame).replace("\r\n", "\n");
        let mut name = None;
        let mut lines = Vec::new();
        for line in raw.lines() {
            if let Some(value) = line.strip_prefix("event:") {
                name = Some(value.trim_start().to_owned())
            } else if let Some(value) = line.strip_prefix("data:") {
                lines.push(value.strip_prefix(' ').unwrap_or(value).to_owned())
            }
        }
        if name.as_deref() == Some("response.metadata") {
            return Vec::new();
        }
        if lines.is_empty() {
            return frame.to_vec();
        }
        let data = lines.join("\n");
        if data == "[DONE]" {
            let mut out = Vec::new();
            if let Some(terminal) = self.assembler.synthetic_terminal() {
                out.extend(encode(&terminal).into_bytes());
            }
            out.extend_from_slice(b"data: [DONE]\n\n");
            return out;
        }
        let Ok(event) = serde_json::from_str::<Value>(&data) else {
            return frame.to_vec();
        };
        match self
            .assembler
            .normalize(event, name.as_deref().unwrap_or(""))
        {
            Ok(Some(event)) => encode(&event).into_bytes(),
            Ok(None) => Vec::new(),
            Err(kind) => {
                let event = self.assembler.error(kind);
                encode(&event).into_bytes()
            }
        }
    }
}

fn frame_boundary(bytes: &[u8]) -> Option<(usize, usize)> {
    let lf = bytes.windows(2).position(|w| w == b"\n\n").map(|p| (p, 2));
    let crlf = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| (p, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (Some(a), None) => Some(a),
        (None, Some(a)) => Some(a),
        (None, None) => None,
    }
}

pub fn normalize_sse(input: &[u8], mode: CompatMode, model: &str, request_id: &str) -> Vec<u8> {
    let mut normalizer = StreamNormalizer::new(mode, model, request_id);
    let mut output = normalizer.push(input);
    output.extend(normalizer.finish());
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    fn terminal(stream: &str) -> Value {
        let out = normalize_sse(stream.as_bytes(), CompatMode::Full, "gpt-5.6-sol", "test");
        let text = String::from_utf8(out).unwrap();
        text.split("\n\n")
            .filter_map(|f| f.lines().find_map(|l| l.strip_prefix("data: ")))
            .filter_map(|d| serde_json::from_str::<Value>(d).ok())
            .last()
            .unwrap()
    }
    #[test]
    fn indexless_function_is_assembled() {
        let s = r#"event: response.created
data: {"type":"response.created","response":{"id":"resp_x"}}

event: response.output_item.added
data: {"type":"response.output_item.added","item":{"type":"function_call","id":"fc_x","call_id":"call_x","name":"update_goal"}}

event: response.function_call_arguments.delta
data: {"type":"response.function_call_arguments.delta","call_id":"call_x","delta":"{\"completed\":"}

event: response.function_call_arguments.delta
data: {"type":"response.function_call_arguments.delta","call_id":"call_x","delta":"true}"}

event: response.function_call_arguments.done
data: {"type":"response.function_call_arguments.done","call_id":"call_x","arguments":""}

data: [DONE]

"#;
        let t = terminal(s);
        assert_eq!(t["response"]["output"][0]["call_id"], "call_x");
        assert_eq!(
            t["response"]["output"][0]["arguments"],
            "{\"completed\":true}"
        );
    }
    #[test]
    fn incomplete_function_fails_closed() {
        let s = r#"event: response.created
data: {"type":"response.created","response":{"id":"resp_x"}}

event: response.output_item.added
data: {"type":"response.output_item.added","item":{"type":"function_call","id":"fc_x","call_id":"call_x","name":"danger","arguments":"{\"x\":"}}

data: [DONE]

"#;
        let t = terminal(s);
        assert_eq!(t["error"]["type"], "proxy_incomplete_output");
    }
    #[test]
    fn failed_terminal_is_not_upgraded() {
        let s = "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{}}\n\ndata: [DONE]\n\n";
        let out =
            String::from_utf8(normalize_sse(s.as_bytes(), CompatMode::Full, "m", "r")).unwrap();
        assert!(out.contains("response.failed"));
        assert!(!out.contains("response.completed"));
    }
    #[test]
    fn interleaved_tools_remain_distinct() {
        let stream = r#"event: response.created
data: {"type":"response.created","response":{"id":"resp_multi"}}

event: response.output_item.added
data: {"type":"response.output_item.added","item":{"type":"function_call","id":"fc_a","call_id":"call_a","name":"first"}}

event: response.output_item.added
data: {"type":"response.output_item.added","item":{"type":"function_call","id":"fc_b","call_id":"call_b","name":"second"}}

event: response.function_call_arguments.delta
data: {"type":"response.function_call_arguments.delta","call_id":"call_b","delta":"{\"b\":2}"}

event: response.function_call_arguments.done
data: {"type":"response.function_call_arguments.done","call_id":"call_b","arguments":""}

event: response.function_call_arguments.delta
data: {"type":"response.function_call_arguments.delta","call_id":"call_a","delta":"{\"a\":1}"}

event: response.function_call_arguments.done
data: {"type":"response.function_call_arguments.done","call_id":"call_a","arguments":""}

data: [DONE]

"#;
        let t = terminal(stream);
        let output = t["response"]["output"].as_array().unwrap();
        assert_eq!(output.len(), 2);
        assert_eq!(output[0]["call_id"], "call_a");
        assert_eq!(output[1]["call_id"], "call_b");
    }
    #[test]
    fn text_id_rebind_and_done_fallback() {
        let stream = r#"event: response.created
data: {"type":"response.created","response":{"id":"resp_text"}}

event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"hello"}

event: response.output_item.done
data: {"type":"response.output_item.done","output_index":0,"item":{"type":"message","id":"msg_real","role":"assistant","content":[]}}

event: response.completed
data: {"type":"response.completed","response":{"id":"resp_text","output":[]}}

"#;
        let t = terminal(stream);
        assert_eq!(t["response"]["output"][0]["id"], "msg_real");
        assert_eq!(t["response"]["output"][0]["content"][0]["text"], "hello");
    }
    #[test]
    fn metadata_is_dropped_and_crlf_frames_parse() {
        let stream = "event: response.metadata\r\ndata: {\"type\":\"response.metadata\",\"secret\":\"drop\"}\r\n\r\nevent: response.created\r\ndata: {\"type\":\"response.created\",\"response\":{}}\r\n\r\nevent: response.output_text.delta\r\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\r\n\r\ndata: [DONE]\r\n\r\n";
        let out = String::from_utf8(normalize_sse(stream.as_bytes(), CompatMode::Full, "m", "r"))
            .unwrap();
        assert!(!out.contains("secret"));
        assert!(out.contains("response.completed"));
        assert!(out.contains("ok"));
    }
    #[test]
    fn custom_input_delta_fallback_and_missing_input_policy() {
        let complete = r#"event: response.created
data: {"type":"response.created","response":{}}

event: response.output_item.added
data: {"type":"response.output_item.added","item":{"type":"custom_tool_call","id":"ct","call_id":"call","name":"shell"}}

event: response.custom_tool_call_input.delta
data: {"type":"response.custom_tool_call_input.delta","call_id":"call","delta":"echo hi"}

event: response.custom_tool_call_input.done
data: {"type":"response.custom_tool_call_input.done","call_id":"call","input":""}

data: [DONE]

"#;
        assert_eq!(
            terminal(complete)["response"]["output"][0]["input"],
            "echo hi"
        );
        let missing = r#"event: response.created
data: {"type":"response.created","response":{}}

event: response.output_item.added
data: {"type":"response.output_item.added","item":{"type":"custom_tool_call","id":"ct","call_id":"call","name":"shell"}}

event: response.custom_tool_call_input.done
data: {"type":"response.custom_tool_call_input.done","call_id":"call"}

data: [DONE]

"#;
        assert_eq!(
            terminal(missing)["error"]["type"],
            "proxy_incomplete_output"
        );
    }
    #[test]
    fn compact_auxiliary_and_usage_envelopes_are_filled() {
        let stream = r#"event: response.created
data: {"response":{"id":"resp_aux","usage":{"input_tokens":2,"output_tokens":3,"input_tokens_details":null}}}

event: response.content_part.added
data: {"type":"response.content_part.added","part":{}}

event: response.reasoning_summary_part.added
data: {"type":"response.reasoning_summary_part.added"}

event: response.failed
data: {"type":"response.failed","response":{}}

"#;
        let text = String::from_utf8(normalize_sse(
            stream.as_bytes(),
            CompatMode::Full,
            "model",
            "req",
        ))
        .unwrap();
        let events: Vec<Value> = text
            .split("\n\n")
            .filter_map(|f| f.lines().find_map(|l| l.strip_prefix("data: ")))
            .filter_map(|d| serde_json::from_str(d).ok())
            .collect();
        assert!(events[0]["response"]["created_at"].is_number());
        assert_eq!(events[0]["response"]["usage"]["total_tokens"], 5);
        assert_eq!(
            events[0]["response"]["usage"]["input_tokens_details"]["cached_tokens"],
            0
        );
        assert!(events[1]["item_id"].is_string());
        assert_eq!(events[1]["content_index"], 0);
        assert_eq!(events[2]["summary_index"], 0);
    }
    #[test]
    fn custom_empty_input_is_valid() {
        let s = r#"event: response.created
data: {"type":"response.created","response":{}}

event: response.output_item.added
data: {"type":"response.output_item.added","output_index":0,"item":{"type":"custom_tool_call","id":"ct","call_id":"call","name":"shell"}}

event: response.custom_tool_call_input.done
data: {"type":"response.custom_tool_call_input.done","output_index":0,"input":""}

data: [DONE]

"#;
        assert_eq!(terminal(s)["response"]["output"][0]["input"], "");
    }
}
