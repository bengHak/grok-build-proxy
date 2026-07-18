use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use super::required_string;

#[derive(Default)]
struct AssistantTurn {
    content: String,
    reasoning: String,
    tool_calls: Vec<Value>,
}

impl AssistantTurn {
    fn is_empty(&self) -> bool {
        self.content.is_empty() && self.reasoning.is_empty() && self.tool_calls.is_empty()
    }
}

pub(super) fn translate_input(items: &[Value], messages: &mut Vec<Value>) -> Result<()> {
    let mut assistant = AssistantTurn::default();
    for item in items {
        match item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("message")
        {
            "reasoning" => append_reasoning(item, &mut assistant),
            "function_call" => append_function_call(item, &mut assistant)?,
            "function_call_output" => {
                flush_assistant(&mut assistant, messages);
                messages.push(json!({
                    "role":"tool",
                    "tool_call_id":required_string(item,"call_id")?,
                    "content":tool_output(item.get("output")),
                }));
            }
            "message" => append_message(item, &mut assistant, messages)?,
            other => bail!("unsupported Responses input item {other:?}"),
        }
    }
    flush_assistant(&mut assistant, messages);
    Ok(())
}

fn append_message(
    item: &Value,
    assistant: &mut AssistantTurn,
    messages: &mut Vec<Value>,
) -> Result<()> {
    let role = required_string(item, "role")?;
    if role == "assistant" {
        assistant
            .content
            .push_str(&plain_text(item.get("content"))?);
        return Ok(());
    }
    flush_assistant(assistant, messages);
    let role = match role {
        "developer" | "system" => "system",
        "user" => "user",
        other => bail!("unsupported message role {other:?}"),
    };
    messages.push(json!({"role":role,"content":chat_content(item.get("content"))?}));
    Ok(())
}

fn append_reasoning(item: &Value, assistant: &mut AssistantTurn) {
    let text = item
        .get("summary")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    if !text.is_empty() {
        if !assistant.reasoning.is_empty() {
            assistant.reasoning.push('\n');
        }
        assistant.reasoning.push_str(&text);
    }
}

fn append_function_call(item: &Value, assistant: &mut AssistantTurn) -> Result<()> {
    assistant.tool_calls.push(json!({
        "id":required_string(item,"call_id")?,
        "type":"function",
        "function":{
            "name":required_string(item,"name")?,
            "arguments":item.get("arguments").and_then(Value::as_str).unwrap_or("{}")
        }
    }));
    Ok(())
}

fn flush_assistant(assistant: &mut AssistantTurn, messages: &mut Vec<Value>) {
    if assistant.is_empty() {
        return;
    }
    let mut message = json!({"role":"assistant","content":assistant.content});
    let object = message.as_object_mut().unwrap();
    if !assistant.reasoning.is_empty() {
        object.insert(
            "reasoning_content".into(),
            assistant.reasoning.clone().into(),
        );
    }
    if !assistant.tool_calls.is_empty() {
        object.insert(
            "tool_calls".into(),
            std::mem::take(&mut assistant.tool_calls).into(),
        );
    }
    messages.push(message);
    *assistant = AssistantTurn::default();
}

fn chat_content(content: Option<&Value>) -> Result<Value> {
    match content {
        Some(Value::String(text)) => Ok(text.clone().into()),
        Some(Value::Array(parts)) => parts
            .iter()
            .map(chat_part)
            .collect::<Result<Vec<_>>>()
            .map(Value::Array),
        Some(Value::Null) | None => Ok(String::new().into()),
        Some(_) => bail!("message content must be a string or array"),
    }
}

fn chat_part(part: &Value) -> Result<Value> {
    match part.get("type").and_then(Value::as_str) {
        Some("input_text" | "output_text" | "text") => Ok(json!({
            "type":"text",
            "text":part.get("text").and_then(Value::as_str).unwrap_or("")
        })),
        Some("input_image") => {
            let url = part
                .get("image_url")
                .and_then(Value::as_str)
                .or_else(|| part.pointer("/image_url/url").and_then(Value::as_str))
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow!("input_image image_url is required"))?;
            Ok(json!({"type":"image_url","image_url":{"url":url}}))
        }
        Some(kind) => bail!("unsupported message content part {kind:?}"),
        None => bail!("message content part type is required"),
    }
}

fn plain_text(content: Option<&Value>) -> Result<String> {
    match content {
        Some(Value::String(text)) => Ok(text.clone()),
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|part| match part.get("type").and_then(Value::as_str) {
                Some("input_text" | "output_text" | "text") => Ok(part
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned()),
                Some(kind) => bail!("unsupported assistant content part {kind:?}"),
                None => bail!("assistant content part type is required"),
            })
            .collect::<Result<Vec<_>>>()
            .map(|parts| parts.concat()),
        Some(Value::Null) | None => Ok(String::new()),
        Some(_) => bail!("assistant content must be a string or array"),
    }
}

fn tool_output(output: Option<&Value>) -> Value {
    match output {
        Some(Value::String(text)) => text.clone().into(),
        Some(value) => serde_json::to_string(value).unwrap_or_default().into(),
        None => "".into(),
    }
}
