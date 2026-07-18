use serde_json::{Value, json};

use super::{Output, Translator, frame::event_bytes};

impl Translator {
    pub(super) fn start(&mut self) -> Vec<u8> {
        if self.started {
            return Vec::new();
        }
        self.started = true;
        self.event(
            "response.created",
            json!({"response":self.response("in_progress",Vec::new())}),
        )
    }

    pub(super) fn reasoning_delta(&mut self, delta: &str) -> Vec<u8> {
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

    pub(super) fn text_delta(&mut self, delta: &str) -> Vec<u8> {
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

    pub(super) fn allocate(&mut self, state: Output) -> usize {
        let index = self.next_index;
        self.next_index += 1;
        self.outputs.insert(index, state);
        index
    }

    pub(super) fn event(&mut self, kind: &str, fields: Value) -> Vec<u8> {
        event_bytes(&mut self.sequence, &self.response_id, kind, fields)
    }
}
