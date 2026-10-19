//! Explicit Messages-to-Responses conversion. Unsupported semantics fail before dispatch.
use serde_json::{Value, json};

#[cfg(test)]
mod tests;

fn only_fields(value: &Value, fields: &[&str]) -> Result<(), &'static str> {
    let object = value.as_object().ok_or("invalid_responses_input")?;
    if object.keys().any(|key| !fields.contains(&key.as_str())) {
        return Err("unsupported_responses_input");
    }
    Ok(())
}

fn text_parts(value: &Value) -> Result<String, &'static str> {
    if let Some(text) = value.as_str() {
        return Ok(text.to_owned());
    }
    let blocks = value.as_array().ok_or("unsupported_responses_input")?;
    let mut text = String::new();
    for block in blocks {
        only_fields(block, &["type", "text", "cache_control"])?;
        if block["type"] != "text" {
            return Err("unsupported_responses_input");
        }
        text.push_str(block["text"].as_str().ok_or("invalid_responses_input")?);
    }
    Ok(text)
}

fn image(block: &Value) -> Result<Value, &'static str> {
    only_fields(block, &["type", "source", "cache_control"])?;
    let source = &block["source"];
    let url = match source["type"].as_str() {
        Some("base64") => {
            only_fields(source, &["type", "media_type", "data"])?;
            let media = source["media_type"]
                .as_str()
                .ok_or("invalid_responses_input")?;
            if !matches!(
                media,
                "image/png" | "image/jpeg" | "image/gif" | "image/webp"
            ) {
                return Err("unsupported_responses_image");
            }
            let data = source["data"].as_str().ok_or("invalid_responses_input")?;
            format!("data:{media};base64,{data}")
        }
        Some("url") => {
            only_fields(source, &["type", "url"])?;
            source["url"]
                .as_str()
                .filter(|url| url.starts_with("https://"))
                .ok_or("unsupported_responses_image")?
                .to_owned()
        }
        _ => return Err("unsupported_responses_image"),
    };
    Ok(json!({"type":"input_image","image_url":url,"detail":"auto"}))
}

fn append_message(input: &mut Vec<Value>, role: &str, parts: &mut Vec<Value>) {
    if !parts.is_empty() {
        let content = std::mem::take(parts);
        input.push(if role == "assistant" {
            let text = content
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect::<String>();
            json!({"role":"assistant","content":text})
        } else {
            json!({"role":"user","content":content})
        });
    }
}

pub(super) fn request(value: &Value) -> Result<Value, &'static str> {
    only_fields(
        value,
        &[
            "model",
            "messages",
            "system",
            "max_tokens",
            "stream",
            "tools",
            "tool_choice",
            "temperature",
            "top_p",
            "thinking",
            "metadata",
            "stop_sequences",
            "output_config",
        ],
    )?;
    if value["model"].as_str().is_none() || !value["messages"].is_array() {
        return Err("invalid_responses_input");
    }
    if value["messages"]
        .as_array()
        .unwrap()
        .last()
        .is_some_and(|message| message["role"] == "assistant")
    {
        // Responses history cannot express Claude's partial assistant prefill.
        return Err("unsupported_responses_input");
    }
    if let Some(thinking) = value.get("thinking").filter(|v| !v.is_null()) {
        only_fields(thinking, &["type"])?;
    }
    if value
        .get("thinking")
        .is_some_and(|v| !v.is_null() && v["type"] != "disabled")
        || value
            .get("stop_sequences")
            .is_some_and(|v| !v.is_null() && v != &json!([]))
    {
        return Err("unsupported_responses_input");
    }
    if let Some(metadata) = value.get("metadata").filter(|v| !v.is_null()) {
        only_fields(metadata, &["user_id"])?;
        if metadata.get("user_id").is_some_and(|id| !id.is_string()) {
            return Err("invalid_responses_input");
        }
    }
    let max_tokens = value["max_tokens"]
        .as_u64()
        .ok_or("invalid_responses_input")?;
    if max_tokens < 16 {
        return Err("unsupported_responses_max_tokens");
    }
    let mut input = Vec::new();
    for message in value["messages"].as_array().unwrap() {
        only_fields(message, &["role", "content"])?;
        let role = message["role"].as_str().ok_or("invalid_responses_input")?;
        if !matches!(role, "user" | "assistant") {
            return Err("unsupported_responses_input");
        }
        let mut parts = Vec::new();
        if let Some(text) = message["content"].as_str() {
            parts.push(
                json!({"type":if role == "user" {"input_text"} else {"output_text"},"text":text}),
            );
        } else {
            let blocks = message["content"]
                .as_array()
                .ok_or("invalid_responses_input")?;
            for block in blocks {
                match block["type"].as_str() {
                    Some("text") => {
                        only_fields(block, &["type", "text", "cache_control"])?;
                        let text = block["text"].as_str().ok_or("invalid_responses_input")?;
                        parts.push(json!({"type":if role == "user" {"input_text"} else {"output_text"},"text":text}));
                    }
                    Some("image") if role == "user" => parts.push(image(block)?),
                    Some("tool_use") if role == "assistant" => {
                        only_fields(block, &["type", "id", "name", "input", "cache_control"])?;
                        append_message(&mut input, role, &mut parts);
                        let call_id = block["id"]
                            .as_str()
                            .filter(|v| !v.is_empty())
                            .ok_or("invalid_responses_input")?;
                        let name = block["name"]
                            .as_str()
                            .filter(|v| !v.is_empty())
                            .ok_or("invalid_responses_input")?;
                        let arguments = block["input"]
                            .as_object()
                            .ok_or("invalid_responses_input")?;
                        input.push(json!({"type":"function_call","call_id":call_id,"name":name,"arguments":Value::Object(arguments.clone()).to_string()}));
                    }
                    Some("tool_result") if role == "user" => {
                        only_fields(
                            block,
                            &[
                                "type",
                                "tool_use_id",
                                "content",
                                "is_error",
                                "cache_control",
                            ],
                        )?;
                        if let Some(error) = block.get("is_error") {
                            match error.as_bool() {
                                Some(false) => {}
                                Some(true) => return Err("unsupported_responses_tool_error"),
                                None => return Err("invalid_responses_input"),
                            }
                        }
                        append_message(&mut input, role, &mut parts);
                        let call_id = block["tool_use_id"]
                            .as_str()
                            .filter(|v| !v.is_empty())
                            .ok_or("invalid_responses_input")?;
                        input.push(json!({"type":"function_call_output","call_id":call_id,"output":text_parts(&block["content"])?}));
                    }
                    _ => return Err("unsupported_responses_input"),
                }
            }
        }
        append_message(&mut input, role, &mut parts);
    }
    if input.is_empty() {
        return Err("invalid_responses_input");
    }
    let mut body = json!({
        "model":value["model"], "input":input, "max_output_tokens":max_tokens,
        "stream":false, "store":false,
    });
    if let Some(system) = value.get("system").filter(|v| !v.is_null()) {
        body["instructions"] = text_parts(system)?.into();
    }
    let effort = if let Some(config) = value.get("output_config").filter(|v| !v.is_null()) {
        only_fields(config, &["effort"])?;
        match config.get("effort").filter(|v| !v.is_null()) {
            Some(effort) if matches!(effort.as_str(), Some("low" | "medium" | "high" | "max")) => {
                body["reasoning"] = json!({"effort":effort});
                true
            }
            Some(_) => return Err("unsupported_responses_input"),
            None => false,
        }
    } else {
        false
    };
    for field in ["temperature", "top_p"] {
        if let Some(number) = value.get(field).filter(|v| !v.is_null()) {
            if !number.is_number() {
                return Err("invalid_responses_input");
            }
            if effort {
                // Claude CLI includes its default temperature even with effort set.
                // Reasoning upstreams do not accept sampling overrides; reject any
                // non-default request instead of changing the caller's choice.
                if field == "temperature" && number.as_f64() == Some(1.0) {
                    continue;
                }
                return Err("unsupported_responses_input");
            }
            body[field] = number.clone();
        }
    }
    if let Some(tools) = value.get("tools").filter(|v| !v.is_null()) {
        let mut converted = Vec::new();
        for tool in tools.as_array().ok_or("invalid_responses_input")? {
            only_fields(
                tool,
                &["name", "description", "input_schema", "cache_control"],
            )?;
            let name = tool["name"]
                .as_str()
                .filter(|v| !v.is_empty())
                .ok_or("invalid_responses_input")?;
            let schema = tool["input_schema"]
                .as_object()
                .ok_or("invalid_responses_input")?;
            // Anthropic tools need not require every property or disallow extra keys.
            let mut converted_tool =
                json!({"type":"function","name":name,"parameters":schema,"strict":false});
            if let Some(description) = tool.get("description") {
                converted_tool["description"] = description
                    .as_str()
                    .ok_or("invalid_responses_input")?
                    .into();
            }
            converted.push(converted_tool);
        }
        body["tools"] = converted.into();
    }
    if let Some(choice) = value.get("tool_choice").filter(|v| !v.is_null()) {
        only_fields(choice, &["type", "name", "disable_parallel_tool_use"])?;
        body["tool_choice"] = match choice["type"].as_str() {
            Some("auto") => json!("auto"),
            Some("any") => json!("required"),
            Some("none") => json!("none"),
            Some("tool") => {
                json!({"type":"function","name":choice["name"].as_str().filter(|v| !v.is_empty()).ok_or("invalid_responses_input")?})
            }
            _ => return Err("unsupported_responses_tool_choice"),
        };
        if let Some(disable) = choice.get("disable_parallel_tool_use") {
            body["parallel_tool_calls"] =
                (!disable.as_bool().ok_or("invalid_responses_input")?).into();
        }
    }
    Ok(body)
}

pub(super) fn message(value: &Value) -> Result<Value, &'static str> {
    if value.get("error").is_some_and(|v| !v.is_null()) {
        return Err("responses_upstream_incomplete");
    }
    let stop = match value["status"].as_str() {
        Some("completed") => "end_turn",
        Some("incomplete") if value["incomplete_details"]["reason"] == "max_output_tokens" => {
            "max_tokens"
        }
        _ => return Err("responses_upstream_incomplete"),
    };
    let mut content = Vec::new();
    let mut used_tool = false;
    for item in value["output"]
        .as_array()
        .ok_or("invalid_responses_output")?
    {
        if item.get("status").is_some_and(|status| {
            status != "completed"
                && !(stop == "max_tokens" && item["type"] == "message" && status == "incomplete")
        }) {
            return Err("responses_upstream_incomplete");
        }
        match item["type"].as_str() {
            Some("message") if item["role"] == "assistant" => {
                for part in item["content"]
                    .as_array()
                    .ok_or("invalid_responses_output")?
                {
                    if part["type"] != "output_text"
                        || part.get("annotations").is_some_and(|annotations| {
                            annotations.as_array().is_none_or(|v| !v.is_empty())
                        })
                    {
                        return Err("unsupported_responses_output");
                    }
                    content.push(json!({"type":"text","text":part["text"].as_str().ok_or("invalid_responses_output")?}));
                }
            }
            Some("function_call") => {
                let call_id = item["call_id"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .ok_or("invalid_responses_output")?;
                let name = item["name"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .ok_or("invalid_responses_output")?;
                let arguments = item["arguments"]
                    .as_str()
                    .ok_or("invalid_responses_output")?;
                let arguments: Value =
                    serde_json::from_str(arguments).map_err(|_| "invalid_responses_output")?;
                if !arguments.is_object() {
                    return Err("invalid_responses_output");
                }
                content.push(json!({"type":"tool_use","id":call_id,"name":name,"input":arguments}));
                used_tool = true;
            }
            Some("reasoning") => {
                // Responses reasoning is not a signed Claude Thinking block.
                // Its tokens remain included in the reported output total.
            }
            _ => return Err("unsupported_responses_output"),
        }
    }
    if used_tool && stop == "max_tokens" {
        return Err("responses_upstream_incomplete");
    }
    let usage = &value["usage"];
    let input = usage["input_tokens"]
        .as_u64()
        .ok_or("invalid_responses_usage")?;
    let cached = match usage["input_tokens_details"].get("cached_tokens") {
        Some(value) => value.as_u64().ok_or("invalid_responses_usage")?,
        None => 0,
    };
    if cached > input {
        return Err("invalid_responses_usage");
    }
    let output = usage["output_tokens"]
        .as_u64()
        .ok_or("invalid_responses_usage")?;
    Ok(json!({
        "id":value["id"].as_str().unwrap_or(""),"type":"message","role":"assistant",
        "model":value["model"].as_str().unwrap_or(""),"content":content,
        "stop_reason":if used_tool {"tool_use"} else {stop},"stop_sequence":null,
        "usage":{"input_tokens":input-cached,"cache_read_input_tokens":cached,"output_tokens":output},
    }))
}

fn event(out: &mut Vec<u8>, kind: &str, value: Value) {
    out.extend_from_slice(format!("event: {kind}\ndata: {value}\n\n").as_bytes());
}

fn chunks(text: &str) -> impl Iterator<Item = &str> {
    let mut start = 0;
    std::iter::from_fn(move || {
        if start >= text.len() {
            return None;
        }
        let mut end = (start + 32 * 1024).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let part = &text[start..end];
        start = end;
        Some(part)
    })
}

pub(super) fn sse(value: &Value) -> Result<Vec<u8>, &'static str> {
    let mut out = Vec::new();
    let mut start = value.clone();
    start["content"] = json!([]);
    start["stop_reason"] = Value::Null;
    start["usage"]["output_tokens"] = 0.into();
    event(
        &mut out,
        "message_start",
        json!({"type":"message_start","message":start}),
    );
    for (index, block) in value["content"]
        .as_array()
        .ok_or("invalid_responses_output")?
        .iter()
        .enumerate()
    {
        match block["type"].as_str() {
            Some("text") => {
                event(
                    &mut out,
                    "content_block_start",
                    json!({"type":"content_block_start","index":index,"content_block":{"type":"text","text":""}}),
                );
                for part in chunks(block["text"].as_str().ok_or("invalid_responses_output")?) {
                    event(
                        &mut out,
                        "content_block_delta",
                        json!({"type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":part}}),
                    );
                }
            }
            Some("tool_use") => {
                event(
                    &mut out,
                    "content_block_start",
                    json!({"type":"content_block_start","index":index,"content_block":{"type":"tool_use","id":block["id"],"name":block["name"],"input":{}}}),
                );
                let arguments = block["input"].to_string();
                for part in chunks(&arguments) {
                    event(
                        &mut out,
                        "content_block_delta",
                        json!({"type":"content_block_delta","index":index,"delta":{"type":"input_json_delta","partial_json":part}}),
                    );
                }
            }
            _ => return Err("invalid_responses_output"),
        }
        event(
            &mut out,
            "content_block_stop",
            json!({"type":"content_block_stop","index":index}),
        );
    }
    event(
        &mut out,
        "message_delta",
        json!({"type":"message_delta","delta":{"stop_reason":value["stop_reason"],"stop_sequence":null},"usage":{"output_tokens":value["usage"]["output_tokens"]}}),
    );
    event(&mut out, "message_stop", json!({"type":"message_stop"}));
    Ok(out)
}
