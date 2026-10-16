//! Native Messages observation. Payloads are forwarded, never persisted.
use crate::{
    requests::{MAX_EVENT_BYTES, RequestTracker},
    storage::RequestStatus,
    usage::TokenUsage,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub(super) fn error_event() -> Vec<u8> {
    b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"The upstream stream did not complete.\"}}\n\n".to_vec()
}
fn response_id(tracker: &RequestTracker) -> String {
    format!("msg_sx_{}", tracker.record.id)
}
pub(super) fn message(
    value: &mut Value,
    model: &str,
    tracker: &mut RequestTracker,
) -> Result<(), &'static str> {
    if value.get("error").is_some_and(|error| !error.is_null()) {
        return Err("upstream_response_failed");
    }
    if value["type"] != "message"
        || !value["content"].is_array()
        || value["stop_reason"].as_str().is_none()
    {
        return Err("invalid_upstream_message");
    }
    value["id"] = response_id(tracker).into();
    value["model"] = model.into();
    tracker.record.response_id = Some(format!("claude:{}", response_id(tracker)));
    tracker.record.usage = TokenUsage::from_claude(&value["usage"]);
    tracker.first_event();
    tracker.finish(RequestStatus::Completed, None);
    Ok(())
}
#[derive(Default)]
pub(super) struct Stream {
    line: Vec<u8>,
    frame: Vec<u8>,
    skip_lf: bool,
    started: bool,
    terminal_delta: bool,
    blocks: BTreeSet<u64>,
    usage: serde_json::Map<String, Value>,
    pub done: bool,
}
impl Stream {
    pub fn feed(
        &mut self,
        bytes: &[u8],
        model: &str,
        tracker: &mut RequestTracker,
    ) -> Result<Vec<u8>, &'static str> {
        let mut out = Vec::new();
        for &byte in bytes {
            if self.done {
                break;
            }
            if self.skip_lf && byte == b'\n' {
                self.skip_lf = false;
                continue;
            }
            self.skip_lf = byte == b'\r';
            if self.frame.len() + self.line.len() > MAX_EVENT_BYTES {
                return Err("upstream_event_too_large");
            }
            if matches!(byte, b'\r' | b'\n') {
                let empty = self.line.is_empty();
                self.frame.append(&mut self.line);
                self.frame.push(b'\n');
                if empty {
                    let frame = std::mem::take(&mut self.frame);
                    out.extend(self.event(&frame, model, tracker)?);
                }
            } else {
                self.line.push(byte);
            }
        }
        Ok(out)
    }
    fn event(
        &mut self,
        frame: &[u8],
        model: &str,
        tracker: &mut RequestTracker,
    ) -> Result<Vec<u8>, &'static str> {
        let text = std::str::from_utf8(frame)
            .map_err(|_| "invalid_upstream_event")?
            .trim_start_matches('\u{feff}');
        let mut data = String::new();
        let mut name = "";
        for line in text.lines() {
            if let Some(value) = line.strip_prefix("data:") {
                data.push_str(value.strip_prefix(' ').unwrap_or(value));
                data.push('\n');
            } else if let Some(value) = line.strip_prefix("event:") {
                name = value.trim();
            }
        }
        if data.is_empty() {
            return Ok(frame.to_vec());
        }
        let mut value: Value = serde_json::from_str(&data).map_err(|_| "invalid_upstream_event")?;
        let kind = value["type"]
            .as_str()
            .ok_or("invalid_upstream_event")?
            .to_owned();
        if !name.is_empty() && name != kind {
            return Err("invalid_upstream_event");
        }
        tracker.first_event();
        match kind.as_str() {
            "message_start" => {
                if self.started || !value["message"].is_object() {
                    return Err("invalid_upstream_event");
                }
                self.started = true;
                value["message"]["id"] = response_id(tracker).into();
                value["message"]["model"] = model.into();
                tracker.record.response_id = Some(format!("claude:{}", response_id(tracker)));
                self.update_usage(&value["message"]["usage"], tracker);
                return Ok(format!("event: message_start\ndata: {value}\n\n").into_bytes());
            }
            "content_block_start" => {
                if !self.started
                    || !self
                        .blocks
                        .insert(value["index"].as_u64().ok_or("invalid_upstream_event")?)
                {
                    return Err("invalid_upstream_event");
                }
            }
            "content_block_delta" => {
                if !self
                    .blocks
                    .contains(&value["index"].as_u64().ok_or("invalid_upstream_event")?)
                {
                    return Err("invalid_upstream_event");
                }
            }
            "content_block_stop" => {
                if !self
                    .blocks
                    .remove(&value["index"].as_u64().ok_or("invalid_upstream_event")?)
                {
                    return Err("invalid_upstream_event");
                }
            }
            "message_delta" => {
                if !self.started {
                    return Err("invalid_upstream_event");
                }
                self.terminal_delta |= value["delta"]["stop_reason"].is_string();
                self.update_usage(&value["usage"], tracker);
            }
            "message_stop" => {
                if !self.started || !self.terminal_delta || !self.blocks.is_empty() {
                    return Err("incomplete_upstream_stream");
                }
                self.done = true;
                tracker.finish(RequestStatus::Completed, None);
            }
            "error" => {
                self.done = true;
                tracker.finish(RequestStatus::Failed, Some("upstream_stream_error"));
                return Ok(error_event());
            }
            _ => {}
        }
        Ok(frame.to_vec())
    }
    fn update_usage(&mut self, value: &Value, tracker: &mut RequestTracker) {
        if let Some(fields) = value.as_object() {
            for (key, value) in fields {
                self.usage.insert(key.clone(), value.clone());
            }
        }
        tracker.record.usage = TokenUsage::from_claude(&Value::Object(self.usage.clone()));
    }
    pub fn eof(&mut self, tracker: &mut RequestTracker) {
        if !self.done {
            self.done = true;
            tracker.finish(
                RequestStatus::Interrupted,
                Some("incomplete_upstream_stream"),
            );
        }
    }
}

pub(super) fn error_body(message: &str) -> Value {
    json!({"type":"error","error":{"type":"api_error","message":message}})
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::watch;
    fn observe(text: &str, chunk: usize) -> (Stream, RequestTracker, Vec<u8>) {
        let mut tracker = RequestTracker::new(None, watch::channel(false).1).unwrap();
        let mut stream = Stream::default();
        let mut output = Vec::new();
        for bytes in text.as_bytes().chunks(chunk) {
            output.extend(stream.feed(bytes, "sx-synthetic", &mut tracker).unwrap());
        }
        (stream, tracker, output)
    }
    #[test]
    fn fragmented_crlf_tool_events_keep_payloads_and_accumulate_usage() {
        let source = concat!(
            "\u{feff}: ping\r\n\r\n",
            "event: message_start\r\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_private_upstream\",\"model\":\"original\",\"usage\":{\"input_tokens\":2,\"cache_read_input_tokens\":4}}}\r\n\r\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tool1\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"合成文本\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":6}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
        );
        for size in [1, 3, 7, 5000] {
            let (stream, tracker, out) = observe(source, size);
            let out = String::from_utf8(out).unwrap();
            assert!(stream.done);
            assert_eq!(tracker.record.status, RequestStatus::Completed);
            assert_eq!(tracker.record.usage.unwrap().input, 6);
            assert_eq!(tracker.record.usage.unwrap().output, 6);
            assert!(out.contains("合成文本"));
            assert!(!out.contains("msg_private_upstream"));
            assert!(out.contains("sx-synthetic"));
        }
    }
    #[test]
    fn terminal_stop_requires_message_delta_and_closed_blocks() {
        for source in [
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ] {
            let mut tracker = RequestTracker::new(None, watch::channel(false).1).unwrap();
            assert!(
                Stream::default()
                    .feed(source.as_bytes(), "model", &mut tracker)
                    .is_err()
            );
        }
        let (mut stream, mut tracker, _) = observe(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{}}\n\n",
            1,
        );
        stream.eof(&mut tracker);
        assert_eq!(tracker.record.status, RequestStatus::Interrupted);
        let mut tracker = RequestTracker::new(None, watch::channel(false).1).unwrap();
        assert!(
            Stream::default()
                .feed(&vec![b'x'; MAX_EVENT_BYTES + 2], "model", &mut tracker)
                .is_err()
        );
    }
}
