use serde_json::{Value, json};

use super::{Output, Translator, frame::event_bytes};

impl Translator {
    pub(super) fn tool_delta(&mut self, tool: &Value) -> Vec<u8> {
        if self.terminal {
            return Vec::new();
        }
        let slot = tool.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        let index = if let Some(index) = self.tool_indexes.get(&slot) {
            *index
        } else {
            let Some(call_id) = tool
                .get("id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            else {
                return self.fail(&json!({
                    "type": "invalid_tool_call",
                    "message": "Kimi returned a function call without an id"
                }));
            };
            let Some(name) = tool
                .pointer("/function/name")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            else {
                return self.fail(&json!({
                    "type": "invalid_tool_call",
                    "message": "Kimi returned a function call without a name"
                }));
            };
            let index = self.allocate(Output::Tool {
                id: format!("fc_{call_id}"),
                call_id: call_id.into(),
                name: name.into(),
                arguments: String::new(),
                started: false,
            });
            self.tool_indexes.insert(slot, index);
            index
        };
        let arguments = tool
            .pointer("/function/arguments")
            .and_then(Value::as_str)
            .unwrap_or("");
        let mut output = Vec::new();
        let item_id = match self.outputs.get_mut(&index) {
            Some(Output::Tool {
                id,
                call_id,
                name,
                arguments: accumulated,
                started,
            }) => {
                if !*started {
                    output.extend(event_bytes(&mut self.sequence, &self.response_id, "response.output_item.added", json!({"output_index":index,"item":{"id":id,"type":"function_call","status":"in_progress","call_id":call_id,"name":name,"arguments":""}})));
                    *started = true;
                }
                accumulated.push_str(arguments);
                id.clone()
            }
            _ => return output,
        };
        if !arguments.is_empty() {
            output.extend(self.event(
                "response.function_call_arguments.delta",
                json!({"item_id":item_id,"output_index":index,"delta":arguments}),
            ));
        }
        output
    }
}
