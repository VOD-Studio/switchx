//! Observe Responses completion without storing prompts, output, or upstream error text.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::Value;
use tokio::sync::watch;

use crate::{
    app,
    storage::{RequestRecord, RequestStatus, Store},
};

pub const REQUEST_ID_HEADER: &str = "x-switchx-request-id";
pub(crate) const MAX_EVENT_BYTES: usize = 2 * 1024 * 1024;

pub(crate) struct RequestLog {
    store: Mutex<Store>,
    generation: String,
    pub failed: AtomicBool,
}

impl RequestLog {
    pub fn new(store: Store, generation: String) -> Self {
        Self {
            store: Mutex::new(store),
            generation,
            failed: AtomicBool::new(false),
        }
    }
}

pub(crate) struct RequestTracker {
    pub record: RequestRecord,
    start: Instant,
    log: Option<Arc<RequestLog>>,
    cancellation: watch::Receiver<bool>,
    finished: bool,
}

impl RequestTracker {
    pub fn new(
        log: Option<Arc<RequestLog>>,
        cancellation: watch::Receiver<bool>,
    ) -> Result<Self, String> {
        Ok(Self {
            record: RequestRecord {
                id: app::new_id()?,
                started_at_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64,
                public_model: None,
                provider_id: None,
                upstream_model: None,
                generation: log
                    .as_ref()
                    .map(|log| log.generation.clone())
                    .unwrap_or_default(),
                http_status: None,
                headers_ms: None,
                first_event_ms: None,
                duration_ms: 0,
                status: RequestStatus::Cancelled,
                error_code: None,
                fallback_from: None,
            },
            start: Instant::now(),
            log,
            cancellation,
            finished: false,
        })
    }

    pub fn elapsed_ms(&self) -> i64 {
        self.start.elapsed().as_millis() as i64
    }

    pub fn first_event(&mut self) {
        if self.record.first_event_ms.is_none() {
            self.record.first_event_ms = Some(self.elapsed_ms());
        }
    }

    pub fn finish(&mut self, status: RequestStatus, code: Option<&'static str>) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.record.status = status;
        self.record.error_code = code.map(str::to_owned);
        self.record.duration_ms = self.elapsed_ms();
        if let Some(log) = &self.log {
            // ponytail: short SQLite writes on the router worker; use a dedicated
            // writer if sustained request volume makes this lock contentious.
            let saved = log
                .store
                .lock()
                .ok()
                .is_some_and(|store| store.put_request(&self.record).is_ok());
            if !saved {
                log.failed.store(true, Ordering::Relaxed);
                eprintln!("SwitchX: request_record_write_failed");
            }
        }
    }
}

impl Drop for RequestTracker {
    fn drop(&mut self) {
        if *self.cancellation.borrow() {
            self.finish(RequestStatus::Interrupted, Some("router_stopping"));
        } else {
            self.finish(RequestStatus::Cancelled, Some("client_disconnected"));
        }
    }
}

#[derive(Default)]
struct SseObserver {
    line: Vec<u8>,
    data: Vec<u8>,
    event: String,
    size: usize,
    skip_lf: bool,
    first_line: bool,
}

impl SseObserver {
    fn feed(&mut self, bytes: &[u8], tracker: &mut RequestTracker) -> Result<(), &'static str> {
        for &byte in bytes {
            if self.skip_lf && byte == b'\n' {
                self.skip_lf = false;
                continue;
            }
            self.skip_lf = byte == b'\r';
            self.size += 1;
            if self.size > MAX_EVENT_BYTES {
                return Err("upstream_event_too_large");
            }
            if matches!(byte, b'\r' | b'\n') {
                self.line(tracker)?;
                if tracker.finished {
                    return Ok(());
                }
            } else {
                self.line.push(byte);
            }
        }
        Ok(())
    }

    fn line(&mut self, tracker: &mut RequestTracker) -> Result<(), &'static str> {
        let bytes = std::mem::take(&mut self.line);
        let line = std::str::from_utf8(&bytes).map_err(|_| "invalid_upstream_event")?;
        let line = if self.first_line {
            self.first_line = false;
            line.strip_prefix('\u{feff}').unwrap_or(line)
        } else {
            line
        };
        if line.is_empty() {
            if !self.data.is_empty() {
                tracker.first_event();
                // [DONE] alone is not a Responses completion signal.
                if self.data != b"[DONE]\n" {
                    let value: Value =
                        serde_json::from_slice(&self.data).map_err(|_| "invalid_upstream_event")?;
                    if !value.is_object() {
                        return Err("invalid_upstream_event");
                    }
                    let kind = value["type"].as_str().unwrap_or(&self.event);
                    if !self.event.is_empty() && self.event != "message" && kind != self.event {
                        return Err("invalid_upstream_event");
                    }
                    match kind {
                        "response.completed" => {
                            let response = &value["response"];
                            if response.get("error").is_some_and(|error| !error.is_null()) {
                                tracker.finish(
                                    RequestStatus::Failed,
                                    Some("upstream_response_failed"),
                                );
                            } else if response["status"].is_null()
                                || response["status"] == "completed"
                            {
                                // The event itself is the terminal signal; compatible
                                // providers can omit the optional nested status.
                                tracker.finish(RequestStatus::Completed, None);
                            } else {
                                return Err("invalid_upstream_event");
                            }
                        }
                        "response.failed" => {
                            tracker.finish(RequestStatus::Failed, Some("upstream_response_failed"))
                        }
                        "response.incomplete" => tracker
                            .finish(RequestStatus::Failed, Some("upstream_response_incomplete")),
                        "error" => {
                            tracker.finish(RequestStatus::Failed, Some("upstream_stream_error"))
                        }
                        _ => {}
                    }
                }
            }
            self.data.clear();
            self.event.clear();
            self.size = 0;
        } else {
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "event" => self.event = value.into(),
                "data" => {
                    self.data.extend_from_slice(value.as_bytes());
                    self.data.push(b'\n');
                }
                _ => {}
            }
        }
        Ok(())
    }
}

pub(crate) struct ResponseObserver {
    sse: Option<SseObserver>,
    json: Vec<u8>,
    inspect: bool,
}

impl ResponseObserver {
    pub fn new(sse: bool, inspect: bool) -> Self {
        Self {
            sse: sse.then(|| SseObserver {
                first_line: true,
                ..Default::default()
            }),
            json: Vec::new(),
            inspect,
        }
    }

    pub fn feed(&mut self, bytes: &[u8], tracker: &mut RequestTracker) -> Result<(), &'static str> {
        if !self.inspect || tracker.finished {
            return Ok(());
        }
        if let Some(sse) = &mut self.sse {
            sse.feed(bytes, tracker)
        } else if self.json.len().saturating_add(bytes.len()) > MAX_EVENT_BYTES {
            Err("upstream_body_too_large")
        } else {
            self.json.extend_from_slice(bytes);
            Ok(())
        }
    }

    pub fn eof(&mut self, tracker: &mut RequestTracker) {
        if tracker.finished {
            return;
        }
        if self.sse.is_some() {
            tracker.finish(RequestStatus::Interrupted, Some("missing_completion"));
            return;
        }
        match serde_json::from_slice::<Value>(&self.json) {
            Ok(value) => {
                if value.get("error").is_some_and(|error| !error.is_null()) {
                    tracker.finish(RequestStatus::Failed, Some("upstream_response_failed"));
                } else {
                    match value["status"].as_str() {
                        Some("completed") => tracker.finish(RequestStatus::Completed, None),
                        Some("failed") => {
                            tracker.finish(RequestStatus::Failed, Some("upstream_response_failed"))
                        }
                        Some("incomplete") => tracker
                            .finish(RequestStatus::Failed, Some("upstream_response_incomplete")),
                        Some("cancelled") => tracker
                            .finish(RequestStatus::Failed, Some("upstream_response_cancelled")),
                        _ => tracker.finish(RequestStatus::Interrupted, Some("missing_completion")),
                    }
                }
            }
            Err(_) => tracker.finish(RequestStatus::Failed, Some("invalid_upstream_response")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observe(sse: bool, bytes: &[u8], chunk_size: usize) -> RequestRecord {
        let mut tracker = RequestTracker::new(None, watch::channel(false).1).unwrap();
        let mut observer = ResponseObserver::new(sse, true);
        for chunk in bytes.chunks(chunk_size) {
            if let Err(code) = observer.feed(chunk, &mut tracker) {
                tracker.finish(RequestStatus::Interrupted, Some(code));
                break;
            }
        }
        observer.eof(&mut tracker);
        tracker.record.clone()
    }

    #[test]
    fn completion_survives_every_byte_boundary_utf8_bom_and_sse_line_endings() {
        for newline in ["\n", "\r\n", "\r"] {
            let wire = [
                "\u{feff}: heartbeat", "", "event: response.output_text.delta",
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"中文 response.completed\"}", "",
                "event: response.completed", "data: {\"type\":\"response.completed\",",
                "data: \"response\":{\"status\":\"completed\"}}", "", "",
            ].join(newline);
            for chunk_size in 1..=wire.len() {
                let record = observe(true, wire.as_bytes(), chunk_size);
                assert_eq!(
                    record.status,
                    RequestStatus::Completed,
                    "chunk {chunk_size}"
                );
                assert!(record.error_code.is_none());
                assert!(record.first_event_ms.is_some());
            }
        }
    }

    #[test]
    fn eof_done_markers_and_text_cannot_fabricate_completion() {
        for bytes in [
            "",
            ": heartbeat\n\n",
            "data: [DONE]\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"response.completed\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n",
            "event: response.completed\n\n",
        ] {
            let record = observe(true, bytes.as_bytes(), 1);
            assert_eq!(record.status, RequestStatus::Interrupted);
            assert_eq!(record.error_code.as_deref(), Some("missing_completion"));
        }
        let malformed = observe(
            true,
            b"event: response.completed\ndata: {\"response\":{\"status\":\"in_progress\"}}\n\n",
            1,
        );
        assert_eq!(
            malformed.error_code.as_deref(),
            Some("invalid_upstream_event")
        );
    }

    #[test]
    fn sse_errors_and_json_terminal_statuses_are_not_http_success() {
        for (kind, code) in [
            ("response.failed", "upstream_response_failed"),
            ("response.incomplete", "upstream_response_incomplete"),
            ("error", "upstream_stream_error"),
        ] {
            let body = format!("data: {{\"type\":\"{kind}\",\"message\":\"private text\"}}\n\n");
            let record = observe(true, body.as_bytes(), 1);
            assert_eq!(record.status, RequestStatus::Failed);
            assert_eq!(record.error_code.as_deref(), Some(code));
        }
        for (body, status) in [
            (r#"{"status":"completed"}"#, RequestStatus::Completed),
            (r#"{"status":"failed"}"#, RequestStatus::Failed),
            (r#"{"status":"incomplete"}"#, RequestStatus::Failed),
            (r#"{"status":"cancelled"}"#, RequestStatus::Failed),
            (
                r#"{"status":"completed","error":{"message":"private"}}"#,
                RequestStatus::Failed,
            ),
            (r#"{"status":"in_progress"}"#, RequestStatus::Interrupted),
            ("{", RequestStatus::Failed),
        ] {
            let record = observe(false, body.as_bytes(), 1);
            assert_eq!(record.status, status, "{body}");
            assert!(record.first_event_ms.is_none());
        }
    }

    #[test]
    fn observation_is_bounded_and_first_terminal_result_wins() {
        for sse in [true, false] {
            let record = observe(sse, &vec![b'x'; MAX_EVENT_BYTES + 1], 4096);
            assert_eq!(record.status, RequestStatus::Interrupted);
            assert_eq!(
                record.error_code.as_deref(),
                Some(if sse {
                    "upstream_event_too_large"
                } else {
                    "upstream_body_too_large"
                })
            );
        }
        let record = observe(true, b"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\ninvalid trailer\xff\n\n", 4096);
        assert_eq!(record.status, RequestStatus::Completed);
    }

    #[test]
    fn drop_records_once_and_distinguishes_client_disconnect_from_shutdown() {
        let log = Arc::new(RequestLog::new(
            Store::open(std::path::Path::new(":memory:")).unwrap(),
            "test-generation".into(),
        ));
        let (cancel, receiver) = watch::channel(false);
        drop(RequestTracker::new(Some(log.clone()), receiver.clone()).unwrap());
        let mut complete = RequestTracker::new(Some(log.clone()), receiver.clone()).unwrap();
        complete.finish(RequestStatus::Completed, None);
        drop(complete);
        cancel.send_replace(true);
        drop(RequestTracker::new(Some(log.clone()), receiver).unwrap());
        let records = log.store.lock().unwrap().requests(10).unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].status, RequestStatus::Interrupted);
        assert_eq!(records[1].status, RequestStatus::Completed);
        assert_eq!(records[2].status, RequestStatus::Cancelled);
        assert!(
            records
                .iter()
                .all(|record| record.generation == "test-generation")
        );
    }
}
