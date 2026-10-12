//! Native xAI Responses compatibility. Keep these rewrites out of other providers.
//! Protocol reference: CC Switch transform_codex_responses_namespace.rs and
//! transform_codex_responses_xai_sanitize.rs (MIT, Copyright 2025 Jason Young).
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const MAX_BYTES: usize = 2 * 1024 * 1024;
#[derive(Clone, Default)]
pub struct Compatibility {
    names: BTreeMap<String, (String, String)>,
    buffer: Vec<u8>,
    sse: bool,
}
impl Compatibility {
    pub fn request(body: &mut Value) -> Result<Self, &'static str> {
        let mut result = Self::default();
        let object = body.as_object_mut().ok_or("invalid_xai_request")?;
        for key in ["prompt_cache_retention", "safety_identifier"] {
            object.remove(key);
        }
        if object.get("model").and_then(Value::as_str) == Some("grok-4.5") {
            for key in [
                "presence_penalty",
                "frequency_penalty",
                "presencePenalty",
                "frequencyPenalty",
                "stop",
            ] {
                object.remove(key);
            }
        }
        object.insert("store".into(), false.into());
        let mut tools = object
            .remove("tools")
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        if let Some(input) = object.get_mut("input").and_then(Value::as_array_mut) {
            for item in input
                .iter()
                .filter(|item| item["type"] == "additional_tools")
            {
                if let Some(extra) = item["tools"].as_array() {
                    tools.extend(extra.iter().cloned());
                }
            }
            input.retain(|item| item["type"] != "additional_tools");
            for item in input.iter_mut() {
                if item["type"] == "reasoning" && item["content"].is_null() {
                    item.as_object_mut()
                        .ok_or("invalid_xai_input")?
                        .remove("content");
                }
                if item["type"] == "agent_message" {
                    let content = match &item["content"] {
                        Value::String(text) => vec![json!({"type":"input_text","text":text})],
                        Value::Array(parts) => parts
                            .iter()
                            .filter_map(|p| {
                                p["text"]
                                    .as_str()
                                    .or_else(|| p["encrypted_content"].as_str())
                            })
                            .map(|text| json!({"type":"input_text","text":text}))
                            .collect(),
                        _ => return Err("unsupported_xai_input"),
                    };
                    *item = json!({"type":"message","role":"user","content":content});
                }
            }
        }
        let mut occupied = BTreeSet::new();
        for tool in &tools {
            if tool["type"] != "namespace"
                && let Some(name) = tool["name"].as_str()
            {
                occupied.insert(name.to_owned());
            }
        }
        let mut flattened = Vec::new();
        for tool in tools {
            if tool["type"] == "namespace" {
                let namespace = tool["name"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .ok_or("invalid_xai_tool")?;
                let children = tool["tools"].as_array().ok_or("invalid_xai_tool")?;
                for child in children {
                    if child["type"] != "function" {
                        return Err("unsupported_xai_tool");
                    }
                    let name = child["name"]
                        .as_str()
                        .filter(|v| !v.is_empty())
                        .ok_or("invalid_xai_tool")?;
                    let flat = format!("{namespace}__{name}");
                    let identity = (namespace.to_owned(), name.to_owned());
                    if occupied.contains(&flat)
                        || result.names.get(&flat).is_some_and(|v| v != &identity)
                    {
                        return Err("xai_tool_name_collision");
                    }
                    if result.names.insert(flat.clone(), identity).is_none() {
                        let mut child = child.clone();
                        child["name"] = flat.into();
                        flattened.push(child);
                    }
                }
            } else if tool["type"] != "tool_search" {
                // Tool search is a Codex backend carrier; its declared functions are already
                // lifted above. Reject other unsupported tools instead of silently losing them.
                flattened.push(tool);
            }
        }
        for tool in &mut flattened {
            let kind = tool["type"].as_str().ok_or("invalid_xai_tool")?;
            if ![
                "function",
                "web_search",
                "x_search",
                "image_generation",
                "collections_search",
                "file_search",
                "code_execution",
                "code_interpreter",
                "mcp",
                "shell",
            ]
            .contains(&kind)
            {
                return Err("unsupported_xai_tool");
            }
            if kind == "function" {
                if tool["description"].is_null() {
                    tool.as_object_mut()
                        .ok_or("invalid_xai_tool")?
                        .remove("description");
                }
                normalize_parameters(tool)?;
            }
        }
        if !flattened.is_empty() {
            object.insert("tools".into(), flattened.into());
        }
        if object
            .get("tool_choice")
            .is_some_and(|v| v["type"] == "namespace" || v["type"] == "tool_search")
        {
            object.insert("tool_choice".into(), "auto".into());
        }
        if let Some(input) = object.get_mut("input") {
            result.flatten_history(input);
        }
        strip_private(body);
        Ok(result)
    }
    fn flatten_history(&self, value: &mut Value) {
        if let Value::Object(object) = value {
            if object.get("type").is_some_and(|v| v == "function_call") {
                let namespace = object.get("namespace").and_then(Value::as_str);
                let name = object.get("name").and_then(Value::as_str);
                if let (Some(namespace), Some(name)) = (namespace, name) {
                    let flat = format!("{namespace}__{name}");
                    if self.names.contains_key(&flat) {
                        object.insert("name".into(), flat.into());
                        object.remove("namespace");
                    }
                }
            }
            for child in object.values_mut() {
                self.flatten_history(child);
            }
        } else if let Value::Array(values) = value {
            for child in values {
                self.flatten_history(child);
            }
        }
    }
    pub fn restore(&self, value: &mut Value) {
        match value {
            Value::Object(object) => {
                if object.get("type").is_some_and(|v| v == "function_call")
                    && let Some((namespace, name)) = object
                        .get("name")
                        .and_then(Value::as_str)
                        .and_then(|name| self.names.get(name))
                {
                    object.insert("name".into(), name.clone().into());
                    object.insert("namespace".into(), namespace.clone().into());
                }
                for child in object.values_mut() {
                    self.restore(child);
                }
            }
            Value::Array(values) => {
                for child in values {
                    self.restore(child);
                }
            }
            _ => {}
        }
    }
    pub fn with_sse(mut self, sse: bool) -> Self {
        self.sse = sse;
        self
    }
    pub fn feed(&mut self, bytes: &[u8], eof: bool) -> Result<Vec<u8>, &'static str> {
        if self.buffer.len() + bytes.len() > MAX_BYTES {
            return Err("xai_response_too_large");
        }
        self.buffer.extend_from_slice(bytes);
        if !self.sse {
            if !eof {
                return Ok(Vec::new());
            }
            let mut value: Value =
                serde_json::from_slice(&self.buffer).map_err(|_| "invalid_xai_response")?;
            self.restore(&mut value);
            self.buffer.clear();
            return serde_json::to_vec(&value).map_err(|_| "invalid_xai_response");
        }
        let mut output = Vec::new();
        while let Some(end) = event_end(&self.buffer) {
            let block: Vec<_> = self.buffer.drain(..end).collect();
            let text = std::str::from_utf8(&block).map_err(|_| "invalid_xai_response")?;
            let data = text
                .lines()
                .filter_map(|line| {
                    line.strip_prefix("data:")
                        .map(|line| line.strip_prefix(' ').unwrap_or(line))
                })
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() || data.trim() == "[DONE]" {
                output.extend(block);
                continue;
            }
            let mut value: Value =
                serde_json::from_str(&data).map_err(|_| "invalid_xai_response")?;
            self.restore(&mut value);
            for line in text
                .lines()
                .filter(|line| !line.starts_with("data:") && !line.is_empty())
            {
                output.extend_from_slice(line.as_bytes());
                output.push(b'\n');
            }
            output.extend_from_slice(b"data: ");
            output.extend(serde_json::to_vec(&value).map_err(|_| "invalid_xai_response")?);
            output.extend_from_slice(b"\n\n");
        }
        if eof && self.buffer.iter().any(|byte| !byte.is_ascii_whitespace()) {
            return Err("invalid_xai_response");
        }
        Ok(output)
    }
}
fn event_end(bytes: &[u8]) -> Option<usize> {
    (0..bytes.len()).find_map(|i| {
        let tail = &bytes[i..];
        if tail.starts_with(b"\n\n") {
            Some(i + 2)
        } else if tail.starts_with(b"\r\n\r\n") {
            Some(i + 4)
        } else {
            None
        }
    })
}
fn strip_private(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.remove("external_web_access");
            for child in object.values_mut() {
                strip_private(child);
            }
        }
        Value::Array(values) => values.iter_mut().for_each(strip_private),
        _ => {}
    }
}
fn normalize_parameters(tool: &mut Value) -> Result<(), &'static str> {
    let mut simplified = false;
    let params = &mut tool["parameters"];
    if params.is_null() {
        *params = json!({"type":"object","properties":{}});
    }
    for keyword in ["oneOf", "anyOf"] {
        if let Some(branches) = params[keyword].as_array() {
            let mut properties = serde_json::Map::new();
            let mut first_required: Option<BTreeSet<String>> = None;
            let mut count = 0;
            for branch in branches {
                if branch["type"] == "null" {
                    continue;
                }
                if branch["type"] != "object" {
                    return Err("unsupported_xai_tool_schema");
                }
                count += 1;
                if let Some(fields) = branch["properties"].as_object() {
                    for (key, value) in fields {
                        if properties.get(key).is_some_and(|saved| saved != value) {
                            properties.insert(key.clone(), json!({}));
                        } else {
                            properties.insert(key.clone(), value.clone());
                        }
                    }
                }
                let required: BTreeSet<_> = branch["required"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
                first_required = Some(match first_required {
                    Some(saved) => saved.intersection(&required).cloned().collect(),
                    None => required,
                });
            }
            if count == 0 {
                return Err("unsupported_xai_tool_schema");
            }
            *params = json!({"type":"object","properties":properties,"required":first_required.unwrap_or_default()});
            simplified = true;
        }
    }
    if params["type"] != "object" {
        return Err("unsupported_xai_tool_schema");
    }
    if simplified {
        tool["strict"] = false.into();
    }
    Ok(())
}
/// Only reasoning blobs may be replayed. Compaction and encrypted function arguments
/// remain unsupported; the router separately requires an existing pinned session.
pub fn replayable_input(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().all(replayable_input),
        Value::Object(object) => {
            if object.get("type").is_some_and(|v| v == "compaction")
                || object
                    .get("encrypted_function_args")
                    .is_some_and(|v| !v.is_null())
            {
                return false;
            }
            if object
                .get("encrypted_content")
                .is_some_and(|v| !v.is_null())
                && (object.get("type").is_none_or(|v| v != "reasoning")
                    || object["encrypted_content"].as_str().is_none())
            {
                return false;
            }
            object.values().all(replayable_input)
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn namespace_tools_and_history_round_trip_with_fragmented_utf8_sse() {
        let mut request = json!({"model":"grok-4.5","prompt_cache_retention":"24h","tools":[{"type":"namespace","name":"tools","tools":[{"type":"function","name":"shell","parameters":{"type":"object","properties":{"cmd":{"type":"string"}}},"external_web_access":false}]}],"input":[{"type":"function_call","name":"shell","namespace":"tools","arguments":"{}"},{"type":"reasoning","content":null,"encrypted_content":"opaque"},{"type":"agent_message","content":[{"type":"encrypted_content","encrypted_content":"Review this."}]}]});
        let mut compatibility = Compatibility::request(&mut request).unwrap().with_sse(true);
        assert_eq!(request["tools"][0]["name"], "tools__shell");
        assert_eq!(request["input"][0]["name"], "tools__shell");
        assert!(request["input"][0].get("namespace").is_none());
        assert!(request["input"][1].get("content").is_none());
        assert_eq!(request["input"][1]["encrypted_content"], "opaque");
        assert_eq!(request["input"][2]["type"], "message");
        assert_eq!(request["store"], false);
        assert!(!request.to_string().contains("external_web_access"));
        let event = "event: response.output_item.done\r\ndata: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"name\":\"tools__shell\",\"arguments\":\"{}\",\"note\":\"你好\"}}\r\n\r\n";
        let mut output = Vec::new();
        for byte in event.as_bytes() {
            output.extend(compatibility.feed(&[*byte], false).unwrap());
        }
        output.extend(compatibility.feed(&[], true).unwrap());
        let text = String::from_utf8(output).unwrap();
        let data = text
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .unwrap();
        let value: Value = serde_json::from_str(data).unwrap();
        assert_eq!(value["item"]["namespace"], "tools");
        assert_eq!(value["item"]["name"], "shell");
        assert_eq!(value["item"]["note"], "你好");
    }
    #[test]
    fn function_union_schema_is_optional_and_name_collisions_fail_closed() {
        let mut request = json!({"model":"grok-4.5","tools":[{"type":"function","name":"automation","strict":true,"parameters":{"oneOf":[{"type":"object","properties":{"mode":{"const":"a"},"id":{"type":"string"}},"required":["mode","id"]},{"type":"object","properties":{"mode":{"const":"b"}},"required":["mode"]},{"type":"null"}]}}]});
        Compatibility::request(&mut request).unwrap();
        assert_eq!(request["tools"][0]["parameters"]["type"], "object");
        assert_eq!(
            request["tools"][0]["parameters"]["required"],
            json!(["mode"])
        );
        assert_eq!(request["tools"][0]["strict"], false);
        let mut collision = json!({"tools":[{"type":"function","name":"a__b"},{"type":"namespace","name":"a","tools":[{"type":"function","name":"b"}]}]});
        assert!(matches!(
            Compatibility::request(&mut collision),
            Err("xai_tool_name_collision")
        ));
        let mut custom = json!({"tools":[{"type":"custom","name":"apply_patch"}]});
        assert!(matches!(
            Compatibility::request(&mut custom),
            Err("unsupported_xai_tool")
        ));
        assert!(!replayable_input(
            &json!([{"type":"compaction","encrypted_content":"opaque"}])
        ));
        assert!(!replayable_input(
            &json!([{"type":"function_call","encrypted_function_args":"opaque"}])
        ));
        assert!(replayable_input(
            &json!([{"type":"reasoning","encrypted_content":"opaque"}])
        ));
    }
}
