use super::*;
use crate::{claude::messages, requests::RequestTracker, storage::RequestStatus};
use tokio::sync::watch;

fn input() -> Value {
    json!({"model":"synthetic-model","max_tokens":128,"messages":[{"role":"user","content":"hello"}]})
}

fn output() -> Value {
    json!({
        "id":"resp_synthetic","model":"synthetic-model","status":"completed",
        "output":[{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"你好","annotations":[]}]}],
        "usage":{"input_tokens":12,"input_tokens_details":{"cached_tokens":5},"output_tokens":7,"output_tokens_details":{"reasoning_tokens":2}}
    })
}

#[test]
fn history_images_tools_and_options_keep_order_and_call_identity() {
    let mut source = input();
    source["system"] =
        json!([{"type":"text","text":"system","cache_control":{"type":"ephemeral"}}]);
    source["metadata"] = json!({"user_id":"synthetic-user"});
    source["temperature"] = json!(0.2);
    source["top_p"] = json!(0.9);
    source["tools"] = json!([{"name":"lookup","description":"Lookup","input_schema":{"type":"object","properties":{"query":{"type":"string"}}}}]);
    source["tool_choice"] = json!({"type":"tool","name":"lookup","disable_parallel_tool_use":true});
    source["messages"] = json!([
        {"role":"user","content":[{"type":"text","text":"look"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"c3ludGhldGlj"}},{"type":"image","source":{"type":"url","url":"https://example.test/image.png"}}]},
        {"role":"assistant","content":[{"type":"text","text":"before"},{"type":"tool_use","id":"call_one","name":"lookup","input":{"query":"one"}},{"type":"text","text":"after"},{"type":"tool_use","id":"call_two","name":"lookup","input":{}}]},
        {"role":"user","content":[{"type":"tool_result","tool_use_id":"call_one","content":[{"type":"text","text":"result one"}]},{"type":"tool_result","tool_use_id":"call_two","content":"result two","is_error":false},{"type":"text","text":"continue"}]}
    ]);
    let converted = request(&source).unwrap();
    assert_eq!(converted["instructions"], "system");
    assert_eq!(converted["stream"], false);
    assert_eq!(converted["store"], false);
    assert!(converted.get("metadata").is_none());
    assert_eq!(converted["tools"][0]["strict"], false);
    assert_eq!(
        converted["tools"][0]["parameters"],
        source["tools"][0]["input_schema"]
    );
    assert_eq!(
        converted["tool_choice"],
        json!({"type":"function","name":"lookup"})
    );
    assert_eq!(converted["parallel_tool_calls"], false);
    assert_eq!(converted["temperature"], 0.2);
    let items = converted["input"].as_array().unwrap();
    assert_eq!(items.len(), 8);
    assert_eq!(
        items[0]["content"][1]["image_url"],
        "data:image/png;base64,c3ludGhldGlj"
    );
    assert_eq!(
        items[0]["content"][2]["image_url"],
        "https://example.test/image.png"
    );
    assert_eq!(items[1]["content"], "before");
    assert_eq!(items[2]["call_id"], "call_one");
    assert_eq!(items[2]["arguments"], r#"{"query":"one"}"#);
    assert_eq!(items[3]["content"], "after");
    assert_eq!(items[4]["call_id"], "call_two");
    assert_eq!(
        items[5],
        json!({"type":"function_call_output","call_id":"call_one","output":"result one"})
    );
    assert_eq!(items[6]["call_id"], "call_two");
    assert_eq!(items[7]["content"][0]["text"], "continue");
}

#[test]
fn unsupported_requests_fail_without_silently_dropping_semantics() {
    for (pointer, value) in [
        ("/thinking", json!({"type":"enabled","budget_tokens":32})),
        ("/stop_sequences", json!(["stop"])),
        (
            "/messages/0/content",
            json!([{"type":"document","source":{}}]),
        ),
        (
            "/messages/0/content",
            json!([{"type":"tool_result","tool_use_id":"x","content":"failed","is_error":true}]),
        ),
        (
            "/messages/0/content",
            json!([{"type":"image","source":{"type":"url","url":"http://example.test/image"}}]),
        ),
        ("/max_tokens", json!(15)),
        ("/tool_choice", json!({"type":"unknown"})),
        ("/metadata", json!({"unknown":"private"})),
        ("/messages/0/role", json!("system")),
        ("/messages/0/role", json!("assistant")),
    ] {
        let mut source = input();
        // Top-level optional fields are inserted separately because pointers require a value.
        let field = pointer.strip_prefix('/').unwrap();
        if !field.contains('/') {
            source[field] = value;
        } else {
            *source.pointer_mut(pointer).unwrap() = value;
        }
        assert!(request(&source).is_err(), "accepted {pointer}");
    }
    let mut source = input();
    source["unknown_extension"] = json!("private");
    assert_eq!(request(&source), Err("unsupported_responses_input"));
    source.as_object_mut().unwrap().remove("unknown_extension");
    source["thinking"] = json!({"type":"disabled"});
    assert!(request(&source).is_ok());
}

#[test]
fn tool_choices_map_explicitly() {
    for (from, to) in [("auto", "auto"), ("any", "required"), ("none", "none")] {
        let mut source = input();
        source["tool_choice"] = json!({"type":from,"disable_parallel_tool_use":false});
        let converted = request(&source).unwrap();
        assert_eq!(converted["tool_choice"], to);
        assert_eq!(converted["parallel_tool_calls"], true);
    }
}

#[test]
fn cli_effort_maps_explicitly_and_conflicting_sampling_is_rejected() {
    for effort in ["low", "medium", "high", "max"] {
        let mut source = input();
        source["output_config"] = json!({"effort":effort});
        source["temperature"] = json!(1);
        let converted = request(&source).unwrap();
        assert_eq!(converted["reasoning"]["effort"], effort);
        assert!(converted.get("temperature").is_none());
        source["temperature"] = json!(0.2);
        assert_eq!(request(&source), Err("unsupported_responses_input"));
        source.as_object_mut().unwrap().remove("temperature");
        source["top_p"] = json!(0.9);
        assert_eq!(request(&source), Err("unsupported_responses_input"));
    }
    let mut source = input();
    source["output_config"] = json!({"format":{"type":"json_schema"}});
    assert_eq!(request(&source), Err("unsupported_responses_input"));
    source["output_config"] = json!({});
    assert!(request(&source).unwrap().get("reasoning").is_none());
}

#[test]
fn output_preserves_tools_cache_totals_and_finish_reasons() {
    let mut source = output();
    source["output"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"reasoning","summary":[]}));
    source["output"].as_array_mut().unwrap().push(json!({"type":"function_call","status":"completed","call_id":"call_one","name":"lookup","arguments":"{\"query\":\"one\"}"}));
    let converted = message(&source).unwrap();
    assert_eq!(converted["stop_reason"], "tool_use");
    assert_eq!(converted["content"].as_array().unwrap().len(), 2);
    assert_eq!(
        converted["content"][1],
        json!({"type":"tool_use","id":"call_one","name":"lookup","input":{"query":"one"}})
    );
    assert_eq!(
        converted["usage"],
        json!({"input_tokens":7,"cache_read_input_tokens":5,"output_tokens":7})
    );
    source["status"] = "incomplete".into();
    source["incomplete_details"] = json!({"reason":"max_output_tokens"});
    assert_eq!(message(&source), Err("responses_upstream_incomplete"));
    source["output"].as_array_mut().unwrap().pop();
    source["output"][0]["status"] = "incomplete".into();
    assert_eq!(message(&source).unwrap()["stop_reason"], "max_tokens");
}

#[test]
fn malformed_failed_or_unsupported_output_never_becomes_success() {
    for (pointer, value) in [
        ("/status", json!("failed")),
        ("/output/0/status", json!("in_progress")),
        ("/output/0/content/0/type", json!("refusal")),
        (
            "/output/0/content/0/annotations",
            json!([{"type":"url_citation"}]),
        ),
        ("/output/0/content/0/annotations", json!({})),
        ("/usage/input_tokens", json!(-1)),
        ("/usage/input_tokens_details/cached_tokens", json!(13)),
        ("/usage/input_tokens_details/cached_tokens", json!("5")),
    ] {
        let mut source = output();
        *source.pointer_mut(pointer).unwrap() = value;
        assert!(message(&source).is_err(), "accepted {pointer}");
    }
    let mut source = output();
    source["error"] = json!({"message":"synthetic-private-error"});
    assert!(message(&source).is_err());
    for arguments in ["{", "[]", "null"] {
        source = output();
        source["output"] =
            json!([{"type":"function_call","call_id":"x","name":"lookup","arguments":arguments}]);
        assert_eq!(message(&source), Err("invalid_responses_output"));
    }
}

#[test]
fn generated_sse_preserves_unicode_and_tool_arguments_with_bounded_events() {
    let mut source = output();
    let text = "中文🦀\n".repeat(20_000);
    source["output"][0]["content"][0]["text"] = text.clone().into();
    source["output"].as_array_mut().unwrap().push(json!({"type":"function_call","call_id":"call_one","name":"lookup","arguments":json!({"query":text}).to_string()}));
    let converted = message(&source).unwrap();
    let events = sse(&converted).unwrap();
    let mut observer = messages::Stream::default();
    let mut tracker = RequestTracker::new(None, watch::channel(false).1).unwrap();
    let mut observed = Vec::new();
    for chunk in events.chunks(101) {
        observed.extend(observer.feed(chunk, "public-model", &mut tracker).unwrap());
    }
    assert!(observer.done);
    assert_eq!(tracker.record.status, RequestStatus::Completed);
    assert_eq!(tracker.record.usage.unwrap().input, 12);
    let events = String::from_utf8(observed).unwrap();
    let mut recovered_text = String::new();
    let mut recovered_arguments = String::new();
    for frame in events.split("\n\n").filter(|frame| !frame.is_empty()) {
        assert!(frame.len() < crate::requests::MAX_EVENT_BYTES);
        let data = frame.split_once("data: ").unwrap().1;
        let event: Value = serde_json::from_str(data).unwrap();
        if event["type"] == "message_start" {
            assert_eq!(event["message"]["model"], "public-model");
            assert!(
                event["message"]["id"]
                    .as_str()
                    .unwrap()
                    .starts_with("msg_sx_")
            );
        }
        if let Some(part) = event["delta"]["text"].as_str() {
            recovered_text.push_str(part);
        }
        if let Some(part) = event["delta"]["partial_json"].as_str() {
            recovered_arguments.push_str(part);
        }
    }
    assert_eq!(recovered_text, text);
    assert_eq!(
        serde_json::from_str::<Value>(&recovered_arguments).unwrap(),
        json!({"query":text})
    );
}
