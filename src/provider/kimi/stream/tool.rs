use serde_json::{Value, json};

use super::{Output, Translator, frame::event_bytes};

impl Translator {
    pub(super) fn tool_delta(&mut self, tool: &Value) -> Vec<u8> {
        if self.terminal {
            return Vec::new();
        }
        let Some(slot) = tool
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
        else {
            return self.fail(&json!({
                "type": "invalid_tool_call",
                "message": "Kimi returned a function call without a valid index"
            }));
        };
        let call_id = match tool.get("id") {
            None => None,
            Some(Value::String(value)) if !value.is_empty() => Some(value.as_str()),
            Some(_) => {
                return self.fail(&json!({
                    "type": "invalid_tool_call",
                    "message": "Kimi returned a function call with an invalid id"
                }));
            }
        };
        let function = match tool.get("function") {
            None => None,
            Some(Value::Object(value)) => Some(value),
            Some(_) => {
                return self.fail(&json!({
                    "type": "invalid_tool_call",
                    "message": "Kimi returned an invalid function call payload"
                }));
            }
        };
        let name = match function.and_then(|value| value.get("name")) {
            None => None,
            Some(Value::String(value)) if !value.is_empty() => Some(value.as_str()),
            Some(_) => {
                return self.fail(&json!({
                    "type": "invalid_tool_call",
                    "message": "Kimi returned a function call with an invalid name"
                }));
            }
        };
        let arguments = match function.and_then(|value| value.get("arguments")) {
            None => None,
            Some(Value::String(value)) => Some(value.as_str()),
            Some(_) => {
                return self.fail(&json!({
                    "type": "invalid_tool_call",
                    "message": "Kimi returned non-string function arguments"
                }));
            }
        };
        let index = if let Some(index) = self.tool_indexes.get(&slot) {
            let Some(Output::Tool {
                call_id: expected_call_id,
                name: expected_name,
                ..
            }) = self.outputs.get(index)
            else {
                return self.fail(&json!({
                    "type": "invalid_tool_call",
                    "message": "Kimi returned an invalid function call index"
                }));
            };
            if call_id.is_some_and(|value| value != expected_call_id)
                || name.is_some_and(|value| value != expected_name)
            {
                return self.fail(&json!({
                    "type": "invalid_tool_call",
                    "message": "Kimi changed a function call id or name mid-stream"
                }));
            }
            if call_id.is_none() && name.is_none() && arguments.is_none() {
                return self.fail(&json!({
                    "type": "invalid_tool_call",
                    "message": "Kimi returned an empty function call fragment"
                }));
            }
            *index
        } else {
            let Some(call_id) = call_id else {
                return self.fail(&json!({
                    "type": "invalid_tool_call",
                    "message": "Kimi returned a function call without an id"
                }));
            };
            let Some(name) = name else {
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
        let arguments = arguments.unwrap_or("");
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
