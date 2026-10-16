//! Edits only owned JSON values, retaining bytes belonging to other settings.

use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::HashSet;

struct Field {
    key: String,
    start: usize,
    value: usize,
    end: usize,
    comma: Option<usize>,
}

fn skip(bytes: &[u8], mut i: usize) -> usize {
    while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    i
}

fn parse<T: DeserializeOwned>(text: &str, start: usize) -> Result<(T, usize), String> {
    let mut stream = serde_json::Deserializer::from_str(&text[start..]).into_iter::<T>();
    let value = stream
        .next()
        .ok_or("配置 JSON 不完整")?
        .map_err(|_| "配置 JSON 无效")?;
    Ok((value, start + stream.byte_offset()))
}

fn fields(text: &str) -> Result<(Vec<Field>, usize), String> {
    let bytes = text.as_bytes();
    let mut i = skip(bytes, 0);
    if bytes.get(i) != Some(&b'{') {
        return Err("配置节点必须是 JSON 对象".into());
    }
    i = skip(bytes, i + 1);
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    while bytes.get(i) != Some(&b'}') {
        let start = i;
        let (key, end) = parse::<String>(text, i)?;
        if !seen.insert(key.clone()) {
            return Err("配置包含重复字段，请先合并重复项".into());
        }
        i = skip(bytes, end);
        if bytes.get(i) != Some(&b':') {
            return Err("配置 JSON 缺少冒号".into());
        }
        let value = skip(bytes, i + 1);
        let (_, end) = parse::<Value>(text, value)?;
        i = skip(bytes, end);
        let comma = (bytes.get(i) == Some(&b',')).then_some(i);
        result.push(Field {
            key,
            start,
            value,
            end,
            comma,
        });
        if comma.is_some() {
            i = skip(bytes, i + 1);
        } else if bytes.get(i) != Some(&b'}') {
            return Err("配置 JSON 无效".into());
        }
    }
    Ok((result, i))
}

pub(super) fn value(text: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_str(text).map_err(|error| {
        format!(
            "配置 JSON 无效（第 {} 行，第 {} 列）",
            error.line(),
            error.column()
        )
    })?;
    fields(text)?;
    if let Some(env) = value.get("env") {
        if !env.is_object() {
            return Err("env 必须是 JSON 对象".into());
        }
        let (root, _) = fields(text)?;
        let env_field = root
            .iter()
            .find(|field| field.key == "env")
            .ok_or("env 字段无效")?;
        fields(&text[env_field.value..env_field.end])?;
    }
    Ok(value)
}

pub(super) fn get<'a>(value: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter().try_fold(value, |node, key| node.get(key))
}

pub(super) fn set(text: &str, path: &[String], value: Option<&Value>) -> Result<String, String> {
    let key = path.first().ok_or("配置字段路径不能为空")?;
    let (members, close) = fields(text)?;
    let found = members.iter().position(|field| &field.key == key);
    let replacement = if path.len() == 1 {
        value
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| "无法生成配置值".to_owned())?
    } else {
        let child = found
            .map(|i| &text[members[i].value..members[i].end])
            .unwrap_or("{}");
        if found.is_none() && value.is_none() {
            return Ok(text.into());
        }
        Some(set(child, &path[1..], value)?)
    };
    let mut result = text.to_owned();
    match (found, replacement) {
        (Some(i), Some(replacement)) => {
            result.replace_range(members[i].value..members[i].end, &replacement)
        }
        (Some(i), None) => {
            let field = &members[i];
            let start = if field.comma.is_none() && i > 0 {
                members[i - 1].comma.unwrap()
            } else {
                field.start
            };
            let end = field.comma.map_or(field.end, |comma| comma + 1);
            result.replace_range(start..end, "");
        }
        (None, Some(replacement)) => {
            let key = serde_json::to_string(key).map_err(|_| "无法生成配置字段")?;
            let prefix = if members.is_empty() { "" } else { "," };
            result.insert_str(close, &format!("{prefix}\n  {key}: {replacement}\n"));
        }
        (None, None) => {}
    }
    serde_json::from_str::<Value>(&result).map_err(|_| "生成的配置 JSON 无效".to_owned())?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn path(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).into()).collect()
    }
    #[test]
    fn edits_and_deletions_keep_unrelated_bytes_and_unicode() {
        let before = "{\n \"theme\" : \"中文🦀\",\n \"env\":{\"KEEP\" : \"a,b}c\", \"OLD\": \"x\"},\n \"hooks\" : {\"a\":[]}\n}\n";
        let changed = set(before, &path(&["env", "OLD"]), Some(&Value::from("new"))).unwrap();
        assert!(changed.contains("\"theme\" : \"中文🦀\""));
        assert!(changed.contains("\"KEEP\" : \"a,b}c\""));
        assert!(changed.contains("\"hooks\" : {\"a\":[]}"));
        let changed = set(&changed, &path(&["env", "OLD"]), None).unwrap();
        let changed = set(
            &changed,
            &path(&["modelPicker"]),
            Some(&serde_json::json!({"options":[]})),
        )
        .unwrap();
        assert!(value(&changed).unwrap()["env"].get("OLD").is_none());
        for key in ["theme", "hooks", "modelPicker", "env"] {
            let result = set(&changed, &path(&[key]), None).unwrap();
            assert!(value(&result).is_ok());
        }
        assert_eq!(
            value(&set("{}", &path(&["env", "KEY"]), Some(&Value::from("v"))).unwrap()).unwrap()["env"]
                ["KEY"],
            "v"
        );
    }
    #[test]
    fn malformed_or_ambiguous_settings_fail_without_echoing_contents() {
        for text in [
            "{\"env\":{},\"env\":{}}",
            "{\"env\":{\"A\":1,\"A\":2}}",
            "{\"env\":null}",
            "{\"key\":synthetic-private}",
        ] {
            let error = value(text).unwrap_err();
            assert!(!error.contains("synthetic-private"));
        }
    }
}
