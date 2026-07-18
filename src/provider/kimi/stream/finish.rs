use serde_json::{Value, json};

use super::{Output, Translator};

impl Translator {
    pub(super) fn terminal(&mut self) -> Vec<u8> {
        match self.finish_reason.as_deref() {
            Some("stop" | "tool_calls") => self.complete(),
            Some("length") => self.incomplete(),
            Some(reason) => self.fail(&json!({
                "type": "upstream_error",
                "message": format!("Kimi stopped with unsupported finish reason {reason:?}")
            })),
            None => self.fail(&json!({
                "type": "upstream_error",
                "message": "Kimi stream ended before finish_reason"
            })),
        }
    }

    pub(super) fn complete(&mut self) -> Vec<u8> {
        if self.terminal {
            return Vec::new();
        }
        if self.outputs.values().any(|output| match output {
            Output::Tool { arguments, .. } => serde_json::from_str::<Value>(arguments).is_err(),
            Output::Reasoning { .. } | Output::Message { .. } => false,
        }) {
            return self.fail(&json!({
                "type": "invalid_tool_arguments",
                "message": "Kimi returned incomplete or invalid function arguments"
            }));
        }
        let mut output = self.start();
        let mut completed = Vec::new();
        for (index, state) in self.outputs.clone() {
            match state {
                Output::Reasoning { id, text } => {
                    output.extend(self.event(
                        "response.reasoning_summary_text.done",
                        json!({"item_id":id,"output_index":index,"summary_index":0,"text":text}),
                    ));
                    output.extend(self.event("response.reasoning_summary_part.done", json!({"item_id":id,"output_index":index,"summary_index":0,"part":{"type":"summary_text","text":text}})));
                    let item = json!({"id":id,"type":"reasoning","status":"completed","summary":[{"type":"summary_text","text":text}]});
                    output.extend(self.event(
                        "response.output_item.done",
                        json!({"output_index":index,"item":item}),
                    ));
                    completed.push(item);
                }
                Output::Message { id, text } => {
                    output.extend(self.event(
                        "response.output_text.done",
                        json!({"item_id":id,"output_index":index,"content_index":0,"text":text}),
                    ));
                    output.extend(self.event("response.content_part.done", json!({"item_id":id,"output_index":index,"content_index":0,"part":{"type":"output_text","text":text,"annotations":[]}})));
                    let item = json!({"id":id,"type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":text,"annotations":[]}]});
                    output.extend(self.event(
                        "response.output_item.done",
                        json!({"output_index":index,"item":item}),
                    ));
                    completed.push(item);
                }
                Output::Tool {
                    id,
                    call_id,
                    name,
                    arguments,
                    ..
                } => {
                    output.extend(self.event(
                        "response.function_call_arguments.done",
                        json!({"item_id":id,"output_index":index,"arguments":arguments}),
                    ));
                    let item = json!({"id":id,"type":"function_call","status":"completed","call_id":call_id,"name":name,"arguments":arguments});
                    output.extend(self.event(
                        "response.output_item.done",
                        json!({"output_index":index,"item":item}),
                    ));
                    completed.push(item);
                }
            }
        }
        self.terminal = true;
        let response = self.response("completed", completed);
        self.terminal_response = Some(response.clone());
        output.extend(self.event("response.completed", json!({"response":response})));
        output.extend_from_slice(b"data: [DONE]\n\n");
        output
    }

    fn incomplete(&mut self) -> Vec<u8> {
        let mut output = self.start();
        self.terminal = true;
        let mut response = self.response("incomplete", Vec::new());
        response.as_object_mut().unwrap().insert(
            "incomplete_details".into(),
            json!({"reason":"max_output_tokens"}),
        );
        self.terminal_response = Some(response.clone());
        output.extend(self.event("response.incomplete", json!({"response":response})));
        output.extend_from_slice(b"data: [DONE]\n\n");
        output
    }

    pub(super) fn fail(&mut self, error: &Value) -> Vec<u8> {
        let mut output = self.start();
        self.terminal = true;
        let mut response = self.response("failed", Vec::new());
        response
            .as_object_mut()
            .unwrap()
            .insert("error".into(), error.clone());
        self.terminal_response = Some(response.clone());
        output.extend(self.event("response.failed", json!({"response":response})));
        output.extend_from_slice(b"data: [DONE]\n\n");
        output
    }

    pub(super) fn response(&self, status: &str, output: Vec<Value>) -> Value {
        json!({
            "id":self.response_id,
            "object":"response",
            "created_at":self.created_at,
            "status":status,
            "model":self.model,
            "output":output,
            "usage":self.usage,
            "error":null,
            "incomplete_details":null
        })
    }
}
