use super::{K3_MODEL, canonical_model};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value, json};

mod history;

use history::translate_input;

const DEFAULT_MAX_TOKENS: u64 = 32_000;

pub fn translate_request(raw: &[u8], prompt_cache_key: Option<&str>) -> Result<Value> {
    let request: Value = serde_json::from_slice(raw).context("invalid JSON request")?;
    let request = request
        .as_object()
        .ok_or_else(|| anyhow!("request body must be a JSON object"))?;
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("model is required"))?;
    let wire_model =
        canonical_model(model).ok_or_else(|| anyhow!("unsupported Kimi model {model:?}"))?;

    let mut messages = Vec::new();
    if let Some(instructions) = request
        .get("instructions")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
    {
        messages.push(json!({"role":"system","content":instructions}));
    }
    match request.get("input") {
        Some(Value::String(text)) => messages.push(json!({"role":"user","content":text})),
        Some(Value::Array(items)) => translate_input(items, &mut messages)?,
        Some(Value::Null) | None => {}
        Some(_) => bail!("input must be a string or array"),
    }

    let max_tokens = request
        .get("max_output_tokens")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MAX_TOKENS)
        .min(DEFAULT_MAX_TOKENS);
    let requested_effort = request
        .get("reasoning")
        .and_then(|reasoning| reasoning.get("effort"))
        .and_then(Value::as_str);
    let effort = if wire_model == K3_MODEL {
        match requested_effort {
            None | Some("xhigh" | "max" | "ultra") => "max",
            Some("medium" | "high") => "high",
            Some("low" | "minimum" | "light") => "low",
            Some(value) => bail!("unsupported K3 reasoning effort {value:?}"),
        }
    } else {
        match requested_effort {
            Some("low") => "low",
            Some("high" | "xhigh" | "max") => "high",
            Some("medium") | None => "medium",
            Some(_) => "medium",
        }
    };
    let mut translated = json!({
        "model": wire_model,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage":true},
        "max_tokens": max_tokens,
        "reasoning_effort": effort,
        "thinking": {"type":"enabled"},
    });
    let output = translated.as_object_mut().unwrap();
    if let Some(prompt_cache_key) = prompt_cache_key.filter(|value| !value.is_empty()) {
        output.insert("prompt_cache_key".into(), prompt_cache_key.into());
    }
    if let Some(tools) = translate_tools(request.get("tools"))? {
        output.insert("tools".into(), tools);
    }
    if let Some(choice) = translate_tool_choice(request.get("tool_choice"))? {
        output.insert("tool_choice".into(), choice);
    }
    Ok(translated)
}

fn required_string<'a>(item: &'a Value, key: &str) -> Result<&'a str> {
    item.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("{key} is required"))
}

fn translate_tools(tools: Option<&Value>) -> Result<Option<Value>> {
    let Some(tools) = tools else {
        return Ok(None);
    };
    let tools = tools
        .as_array()
        .ok_or_else(|| anyhow!("tools must be an array"))?;
    if tools.is_empty() {
        return Ok(None);
    }
    let translated = tools
        .iter()
        .map(|tool| {
            if tool.get("type").and_then(Value::as_str) != Some("function") {
                bail!("Kimi currently supports function tools only")
            }
            Ok(json!({"type":"function","function":{
                "name":required_string(tool,"name")?,
                "description":tool.get("description").and_then(Value::as_str).unwrap_or(""),
                "parameters":tool.get("parameters").cloned().unwrap_or_else(|| json!({"type":"object"})),
            }}))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(translated.into()))
}

fn translate_tool_choice(choice: Option<&Value>) -> Result<Option<Value>> {
    match choice {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value == "auto" => Ok(None),
        Some(Value::String(value)) if matches!(value.as_str(), "none" | "required") => {
            Ok(Some(value.clone().into()))
        }
        Some(Value::Object(object)) => translate_named_choice(object).map(Some),
        Some(_) => bail!("invalid tool_choice"),
    }
}

fn translate_named_choice(choice: &Map<String, Value>) -> Result<Value> {
    if choice.get("type").and_then(Value::as_str) != Some("function") {
        bail!("invalid tool_choice type")
    }
    let name = choice
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow!("tool_choice name is required"))?;
    Ok(json!({"type":"function","function":{"name":name}}))
}
