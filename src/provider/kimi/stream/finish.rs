use serde_json::{Value, json};

use super::{Output, Translator};

impl Translator {
    pub(super) fn complete(&mut self) -> Vec<u8> {
        if self.terminal {
            return Vec::new();
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

    pub(super) fn fail(&mut self, error: &Value) -> Vec<u8> {
        let mut output = self.start();
        self.terminal = true;
        let response = json!({"id":self.response_id,"object":"response","status":"failed","model":self.model,"output":[],"error":error});
        self.terminal_response = Some(response.clone());
        output.extend(self.event("response.failed", json!({"response":response})));
        output.extend_from_slice(b"data: [DONE]\n\n");
        output
    }

    pub(super) fn response(&self, status: &str, output: Vec<Value>) -> Value {
        json!({"id":self.response_id,"object":"response","status":status,"model":self.model,"output":output,"usage":self.usage})
    }
}
