use super::*;
use rusqlite::Connection;
use serde_json::json;
use std::io::Write;

struct Fixture {
    root: PathBuf,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("switchx-clients-{}", crate::id::new_id().unwrap()));
        fs::create_dir_all(&root).unwrap();
        let store = Store::open(&root.join("usage.sqlite")).unwrap();
        Self { root, store }
    }
    fn append(&self, path: &str, bytes: &[u8]) {
        let path = self.root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }
    fn line(&self, path: &str, value: Value) {
        self.append(path, format!("{value}\n").as_bytes());
    }
    fn scan(&self, client: UsageClient, path: &str) -> ImportSummary {
        import(
            &self.store,
            client,
            &self.root.join(path),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap()
    }
    fn rows(&self) -> Vec<UsageRecord> {
        self.store.usage_records(0, i64::MAX).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
const TIME: &str = "2026-10-15T08:00:00Z";

#[test]
fn gemini_json_jsonl_migration_updates_checkpoints_and_partial_tail() {
    let f = Fixture::new();
    let base = "gemini/tmp/project/chats/session-one";
    let msg = |out| json!({"type":"gemini","id":"message","timestamp":TIME,"model":"gemini-test","content":"private-body", "tokens":{"input":100,"cached":40,"output":out,"thoughts":5}});
    f.line(
        &format!("{base}.json"),
        json!({"sessionId":"session","messages":[msg(10)]}),
    );
    assert_eq!(f.scan(UsageClient::Gemini, "gemini").added, 1);
    assert_eq!(
        (
            f.rows()[0].usage.input,
            f.rows()[0].usage.output,
            f.rows()[0].usage.reasoning
        ),
        (100, 15, 5)
    );
    assert_eq!(f.scan(UsageClient::Gemini, "gemini").read, 0);
    f.line(&format!("{base}.jsonl"), json!({"sessionId":"session"}));
    f.line(&format!("{base}.jsonl"), msg(10));
    assert_eq!(f.scan(UsageClient::Gemini, "gemini").added, 0);
    f.line(
        &format!("{base}.jsonl"),
        json!({"$set":{"messages":[msg(20)]}}),
    );
    assert_eq!(f.scan(UsageClient::Gemini, "gemini").added, 1);
    assert_eq!(f.rows().len(), 1);
    let updated = msg(30).to_string();
    f.append(&format!("{base}.jsonl"), updated.as_bytes());
    assert_eq!(f.scan(UsageClient::Gemini, "gemini").added, 0);
    f.append(&format!("{base}.jsonl"), b"\n");
    assert_eq!(f.scan(UsageClient::Gemini, "gemini").added, 1);
    assert_eq!(f.rows()[0].usage.output, 35);
    f.line(&format!("{base}.jsonl"), json!({"$rewindTo":"message"}));
    assert_eq!(f.scan(UsageClient::Gemini, "gemini").added, 0);
    assert_eq!(
        f.rows().len(),
        1,
        "rewinding a conversation does not refund usage"
    );
    assert!(
        f.store
            .usage_log_files()
            .unwrap()
            .iter()
            .all(|c| !c.state.contains("private-body"))
    );
}

#[test]
fn grok_turns_are_independent_count_model_calls_and_keep_only_complete_native_costs() {
    let f = Fixture::new();
    let path = "grok/sessions/project/session/updates.jsonl";
    let turn = |prompt, partial, ticks| json!({"timestamp":TIME,"method":"_x.ai/session/update","params":{"update":{"sessionUpdate":"turn_completed","prompt_id":prompt,"usage":{"costIsPartial":partial,"modelUsage":{"grok-test":{"inputTokens":100,"cachedReadTokens":80,"outputTokens":20,"reasoningTokens":10,"modelCalls":3,"apiDurationMs":2500,"costUsdTicks":ticks}}}}}});
    f.line(path, turn("p1", false, 12_345_000));
    f.line(path, turn("p2", true, 5_000_000));
    assert_eq!(f.scan(UsageClient::GrokBuild, "grok").added, 2);
    let rows = f.rows();
    assert_eq!(rows.iter().map(|r| r.request_count).sum::<u64>(), 6);
    assert!(rows.iter().all(|r| r.duration_ms == Some(2_500)));
    assert_eq!(rows.iter().map(|r| r.usage.total()).sum::<u64>(), 240);
    assert_eq!(
        rows.iter()
            .filter_map(|r| r.reported_cost_micros)
            .collect::<Vec<_>>(),
        [1235]
    );
    f.line(path, turn("p2", false, 20_000_000));
    assert_eq!(f.scan(UsageClient::GrokBuild, "grok").added, 1);
    assert_eq!(
        f.rows()
            .iter()
            .filter_map(|r| r.reported_cost_micros)
            .sum::<u64>(),
        3235
    );
    let copy = "grok/archived_sessions/project/session/updates.jsonl";
    f.append(copy, &fs::read(f.root.join(path)).unwrap());
    assert_eq!(f.scan(UsageClient::GrokBuild, "grok").added, 0);
    assert_eq!(f.rows().len(), 2);
    let mut ignored = turn("not-complete", false, 0);
    ignored["params"]["update"]["sessionUpdate"] = "turn_started".into();
    f.line(path, ignored);
    assert_eq!(f.scan(UsageClient::GrokBuild, "grok").added, 0);
}

#[test]
fn pi_forked_ancestors_and_all_usage_carriers_are_deduplicated() {
    let f = Fixture::new();
    let path = "pi/project/session.jsonl";
    let message = |output, cost| json!({"type":"message","id":"entry","timestamp":TIME,"message":{"role":"assistant","provider":"provider","model":"requested","responseModel":"actual","content":"private-pi","usage":{"input":100,"cacheRead":40,"cacheWrite":5,"output":output,"cost":{"total":cost}}}});
    f.line(path, json!({"type":"session","id":"session"}));
    f.line(path, message(10, "1.2345675"));
    assert_eq!(f.scan(UsageClient::Pi, "pi").added, 1);
    assert_eq!(f.rows()[0].duration_ms, None);
    assert_eq!(f.rows()[0].reported_cost_micros, Some(1_234_568));
    assert_eq!(f.rows()[0].model, "actual");
    assert_eq!(f.rows()[0].public_model.as_deref(), Some("requested"));
    assert_eq!(f.rows()[0].usage.input, 145);
    f.line(
        "pi/project/fork.jsonl",
        json!({"type":"session","id":"fork"}),
    );
    f.line("pi/project/fork.jsonl", message(10, "1.2345675"));
    assert_eq!(f.scan(UsageClient::Pi, "pi").added, 0);
    f.line(path, message(20, "2e-3"));
    assert_eq!(f.scan(UsageClient::Pi, "pi").added, 1);
    for kind in ["compaction", "branch_summary"] {
        f.line(path,json!({"type":kind,"id":kind,"timestamp":TIME,"summary":"private-summary","usage":{"input":2,"output":3,"cost":{"input":"0.001","output":"0.002"}}}));
    }
    f.line(path,json!({"type":"message","id":"tool","timestamp":TIME,"message":{"role":"toolResult","toolCallId":"call","usage":{"input":1,"output":1}}}));
    assert_eq!(f.scan(UsageClient::Pi, "pi").added, 3);
    assert_eq!(f.rows().len(), 4);
    assert_eq!(
        f.rows()
            .iter()
            .filter_map(|r| r.reported_cost_micros)
            .sum::<u64>(),
        8000
    );
    assert_eq!(f.scan(UsageClient::Pi, "pi").read, 0);
    assert!(
        f.store
            .usage_log_files()
            .unwrap()
            .iter()
            .all(|c| !c.state.contains("private"))
    );
}

#[test]
fn pi_assistant_duration_uses_message_start_and_entry_end() {
    let f = Fixture::new();
    let path = "pi/s.jsonl";
    f.line(path, json!({"type":"session","id":"s"}));
    f.line(path, json!({"type":"message","id":"slow","timestamp":"2026-10-15T08:00:04Z","message":{"role":"assistant","timestamp":"2026-10-15T08:00:00Z","model":"m","usage":{"input":1,"output":2}}}));
    f.line(path, json!({"type":"message","id":"fast","timestamp":"2026-10-15T08:00:01.050Z","message":{"role":"assistant","timestamp":"2026-10-15T08:00:01.000Z","model":"m","usage":{"input":1,"output":2}}}));
    f.line(path, json!({"type":"message","id":"tool","timestamp":"2026-10-15T08:00:09Z","message":{"role":"toolResult","timestamp":"2026-10-15T08:00:00Z","toolCallId":"c","usage":{"input":1,"output":1}}}));
    assert_eq!(f.scan(UsageClient::Pi, "pi").added, 3);
    let duration = |id: &str| {
        f.rows()
            .into_iter()
            .find(|row| row.id.contains(id))
            .unwrap()
            .duration_ms
    };
    assert_eq!(duration("slow"), Some(4_000));
    assert_eq!(duration("fast"), None);
    assert_eq!(duration("tool"), None);
}

fn opencode_db(f: &Fixture) -> Connection {
    let db = Connection::open(f.root.join("opencode.db")).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE session(id TEXT PRIMARY KEY,time_updated INTEGER);CREATE TABLE message(id TEXT PRIMARY KEY,session_id TEXT,time_created INTEGER,time_updated INTEGER,data TEXT);INSERT INTO session VALUES ('s',1);").unwrap();
    db
}
fn opencode_message(done: bool, cost: f64) -> Value {
    json!({"role":"assistant","modelID":"model","time":{"created":1792051200000i64,"completed":if done {Some(1792051201000i64)}else{None}},"tokens":{"input":100,"output":10,"reasoning":3,"cache":{"read":40,"write":5}},"cost":cost})
}

#[test]
fn opencode_reads_wal_completion_cost_changes_and_v2_compaction_without_v1_duplicates() {
    let f = Fixture::new();
    let db = opencode_db(&f);
    db.execute(
        "INSERT INTO message VALUES ('m','s',1792051200000,1,?1)",
        [opencode_message(false, 0.0).to_string()],
    )
    .unwrap();
    assert_eq!(f.scan(UsageClient::OpenCode, "opencode.db").added, 0);
    db.execute(
        "UPDATE message SET data=?1 WHERE id='m'",
        [opencode_message(true, 0.25).to_string()],
    )
    .unwrap();
    assert_eq!(f.scan(UsageClient::OpenCode, "opencode.db").added, 1);
    assert_eq!(
        (
            f.rows()[0].usage.input,
            f.rows()[0].usage.output,
            f.rows()[0].reported_cost_micros
        ),
        (145, 13, Some(250000))
    );
    assert_eq!(f.rows()[0].duration_ms, Some(1_000));
    db.execute(
        "UPDATE message SET data=?1 WHERE id='m'",
        [opencode_message(true, 0.5).to_string()],
    )
    .unwrap();
    assert_eq!(f.scan(UsageClient::OpenCode, "opencode.db").added, 1);
    assert_eq!(f.rows()[0].reported_cost_micros, Some(500000));
    db.execute_batch("CREATE TABLE session_v2(id TEXT PRIMARY KEY,time_updated INTEGER);CREATE TABLE session_message(id TEXT PRIMARY KEY,session_id TEXT,time_created INTEGER,time_updated INTEGER,type TEXT,data TEXT);INSERT INTO session_v2 VALUES('s',2);INSERT INTO session_message SELECT id,session_id,time_created,time_updated,'assistant',data FROM message;").unwrap();
    let compact = json!({"status":"failed","model":{"id":"compact-model","providerID":"p"},"tokens":{"input":5,"output":2,"reasoning":1},"cost":0.0});
    db.execute(
        "INSERT INTO session_message VALUES ('compact','s',1792051200000,2,'compaction',?1)",
        [compact.to_string()],
    )
    .unwrap();
    assert_eq!(f.scan(UsageClient::OpenCode, "opencode.db").added, 1);
    assert_eq!(f.rows().len(), 2);
    assert!(
        f.rows()
            .iter()
            .any(|r| r.model == "compact-model" && r.usage.output == 3 && r.duration_ms.is_none())
    );
    assert!(
        f.rows()
            .iter()
            .any(|r| r.model == "model" && r.duration_ms == Some(1_000))
    );
    assert_eq!(f.scan(UsageClient::OpenCode, "opencode.db").read, 0);
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM message", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn mcode_keeps_zero_cost_namespaces_and_corrected_committed_rows() {
    let f = Fixture::new();
    let db = Connection::open(f.root.join("mcode.db")).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL;CREATE TABLE local_runtime_token_usage(id INTEGER PRIMARY KEY,session_id TEXT,model TEXT,ts INTEGER,input_tokens INTEGER,output_tokens INTEGER,reasoning_tokens INTEGER,cache_read_tokens INTEGER,cache_write_tokens INTEGER,cost_usd REAL);INSERT INTO local_runtime_token_usage VALUES(1,'s','provider/vendor/model',1792051200000,10,20,3,40,5,0);").unwrap();
    assert_eq!(f.scan(UsageClient::Mcode, "mcode.db").added, 1);
    let row = &f.rows()[0];
    assert_eq!(row.model, "vendor/model");
    assert_eq!(row.reported_cost_micros, Some(0));
    assert_eq!((row.usage.input, row.usage.output), (55, 23));
    db.execute_batch("INSERT INTO local_runtime_token_usage VALUES(2,'s','provider/next/model',1792051200000,1,2,0,0,0,NULL);UPDATE local_runtime_token_usage SET cost_usd=0.5 WHERE id=1;").unwrap();
    assert_eq!(f.scan(UsageClient::Mcode, "mcode.db").added, 2);
    assert_eq!(f.rows().len(), 2);
    assert!(
        f.rows()
            .iter()
            .any(|r| r.reported_cost_micros == Some(500000))
    );
    assert_eq!(f.scan(UsageClient::Mcode, "mcode.db").read, 0);
    db.execute_batch("DELETE FROM local_runtime_token_usage;INSERT INTO local_runtime_token_usage VALUES(1,'replacement','provider/another',1792051200000,1,2,0,0,0,1);").unwrap();
    assert_eq!(f.scan(UsageClient::Mcode, "mcode.db").added, 1);
    assert_eq!(f.rows().len(), 3);
}

#[test]
fn decimal_timestamps_missing_sources_and_cancelled_imports_are_bounded() {
    assert_eq!(usd_micros(&json!("0.0000005")), Some(1));
    assert_eq!(usd_micros(&json!("1e-3")), Some(1000));
    assert_eq!(usd_micros(&json!("-1")), None);
    assert_eq!(usd_micros(&json!("NaN")), None);
    assert_eq!(usd_micros(&json!("1e100")), None);
    assert_eq!(timestamp(&json!(i64::MIN)), None);
    let f = Fixture::new();
    for client in UsageClient::EXTERNAL {
        assert_eq!(f.scan(client, "missing").files, 0);
    }
    assert!(matches!(
        import(
            &f.store,
            UsageClient::Pi,
            &f.root,
            &AtomicBool::new(true),
            |_| {}
        ),
        Err(ImportError::Cancelled)
    ));
    assert!(f.rows().is_empty());
}

#[test]
fn reports_filter_all_five_clients_mix_reported_and_estimated_costs_and_count_calls() {
    use crate::{
        app::{self, UsageQuery},
        storage::{Currency, PriceRecord, UsageFilter},
        usage::report::Period,
    };
    let f = Fixture::new();
    let at = chrono::Local::now().timestamp_millis();
    let cursor = UsageLogFile {
        path: "synthetic".into(),
        size: 0,
        modified_ms: 0,
        read_offset: 0,
        state: String::new(),
    };
    f.store
        .put_model_price(&PriceRecord {
            provider_id: String::new(),
            model: "model".into(),
            currency: Currency::Cny,
            input_micros: 1_000_000,
            cached_input_micros: 100_000,
            cache_write_micros: 1_250_000,
            cache_write_1h_micros: 2_000_000,
            output_micros: 2_000_000,
        })
        .unwrap();
    for client in UsageClient::EXTERNAL {
        let mut free = record(
            client,
            "session",
            "free",
            "model".into(),
            at,
            usage(100, 20, 10, 10, 0, true, false),
        );
        free.reported_cost_micros = Some(0);
        let mut reported = record(
            client,
            "session",
            "reported",
            "model".into(),
            at,
            free.usage,
        );
        reported.reported_cost_micros = Some(500000);
        let mut estimated = record(
            client,
            "session",
            "estimated",
            "model".into(),
            at,
            free.usage,
        );
        estimated.request_count = 3;
        assert_eq!(
            f.store
                .put_imported_usage(&[free, reported, estimated], &cursor)
                .unwrap(),
            3
        );
        let query = UsageQuery {
            period: Period::Today,
            filter: UsageFilter {
                client: Some(client),
                ..Default::default()
            },
            ..Default::default()
        };
        let view = app::usage_from(&f.store, &query).unwrap();
        assert_eq!(view.report.totals.requests, 5);
        assert_eq!(view.report.totals.cost.api.usd, 500000);
        assert_eq!(view.report.totals.cost.api.cny, 135);
        assert_eq!(view.report.totals.cost.unpriced_requests, 0);
        assert_eq!(view.total_rows, 3);
        assert_eq!(view.report.models.len(), 1);
        assert_eq!(view.report.connections.len(), 1);
        assert_eq!(
            view.report
                .points
                .iter()
                .map(|p| p.totals.requests)
                .sum::<u64>(),
            5
        );
        assert_eq!(view.report.heat.iter().flatten().sum::<u64>(), 420);
        assert_eq!(view.rows.iter().filter(|r| r.reported_cost).count(), 2);
        assert!(view.rows.iter().any(|r| r.cost == Some((Currency::Usd, 0))));
    }
    let all = app::usage_from(&f.store, &UsageQuery::default()).unwrap();
    assert_eq!(all.report.totals.requests, 25);
    assert_eq!(all.report.totals.cost.api.usd, 2_500_000);
}

#[test]
fn exact_response_ids_merge_router_usage_and_preserve_reported_cost_in_both_orders() {
    use crate::storage::{RequestRecord, RequestStatus};
    let f = Fixture::new();
    let at = chrono::Local::now().timestamp_millis();
    for (index, log_first) in [true, false].into_iter().enumerate() {
        let response = format!("response-{index}");
        let mut logged = record(
            UsageClient::Pi,
            "session",
            &index.to_string(),
            "model".into(),
            at,
            usage(10, 0, 0, 20, 0, true, false),
        );
        logged.response_id = Some(response.clone());
        logged.reported_cost_micros = Some(123);
        let routed = RequestRecord {
            client: crate::storage::UsageClient::Unknown,
            workspace_id: None,
            id: format!("route-{index}"),
            started_at_ms: at,
            public_model: Some("public".into()),
            provider_id: Some("provider".into()),
            upstream_model: Some("model".into()),
            generation: "g".into(),
            http_status: Some(200),
            headers_ms: Some(5),
            first_event_ms: Some(10),
            duration_ms: 100,
            status: RequestStatus::Completed,
            error_code: None,
            fallback_from: None,
            usage: Some(logged.usage),
            response_id: Some(response),
        };
        let cursor = UsageLogFile {
            path: format!("file-{index}"),
            size: 1,
            modified_ms: 1,
            read_offset: 1,
            state: String::new(),
        };
        if log_first {
            f.store
                .put_imported_usage(&[logged.clone()], &cursor)
                .unwrap();
        }
        f.store.put_request(&routed).unwrap();
        f.store.put_imported_usage(&[logged], &cursor).unwrap();
    }
    let rows = f.rows();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.source == UsageSource::Router
        && r.client == UsageClient::Pi
        && r.reported_cost_micros == Some(123)));
    f.store.clear_imported_usage().unwrap();
    assert_eq!(f.rows().len(), 2);
}

#[test]
fn corrupt_database_reports_failure_without_advancing_a_cursor() {
    let f = Fixture::new();
    fs::write(f.root.join("bad.db"), "not sqlite").unwrap();
    let result = f.scan(UsageClient::OpenCode, "bad.db");
    assert_eq!(result.unreadable, 1);
    assert!(f.store.usage_log_files().unwrap().is_empty());
    assert!(f.rows().is_empty());
}
