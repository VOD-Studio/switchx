//! Import supported clients' session usage into a scratch database and print totals.
//! Reads only usage counts; never writes the SwitchX data directory or CODEX_HOME.
//! Run: cargo run --example usage_import_probe -- /abs/client/home /abs/new-scratch.sqlite [--claude|--gemini|--opencode|--grokbuild|--pi|--mcode]

use std::{collections::BTreeMap, path::Path, sync::atomic::AtomicBool, time::Instant};

use switchx::{
    storage::{Store, UsageClient},
    usage::{claude_logs, codex_logs, session_logs},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(home), Some(database)) = (args.next(), args.next()) else {
        return Err("pass an absolute CODEX_HOME and an absolute scratch database path".into());
    };
    let (home, database) = (Path::new(&home), Path::new(&database));
    if !home.is_absolute() || !database.is_absolute() {
        return Err("both paths must be absolute".into());
    }
    let client = match args.next().as_deref() {
        None => UsageClient::Codex,
        Some("--claude") => UsageClient::ClaudeCode,
        Some(flag) => UsageClient::parse(flag.trim_start_matches("--"))
            .filter(|c| UsageClient::EXTERNAL.contains(c))
            .ok_or("unsupported client flag")?,
    };
    // A probe must never overwrite an existing database, including real user data.
    if database.exists() {
        return Err("scratch database must not already exist".into());
    }
    let store = Store::open(database)?;
    for pass in ["first", "repeat"] {
        let started = Instant::now();
        let summary = match client {
            UsageClient::ClaudeCode => {
                claude_logs::import(&store, home, &AtomicBool::new(false), |_| {})
            }
            UsageClient::Codex => codex_logs::import(&store, home, &AtomicBool::new(false), |_| {}),
            _ => session_logs::import(&store, client, home, &AtomicBool::new(false), |_| {}),
        }
        .map_err(|error| format!("{error:?}"))?;
        println!(
            "{pass} scan: {} files, {} read, {} rows inserted/updated, {} unreadable in {:.2?}",
            summary.files,
            summary.read,
            summary.added,
            summary.unreadable,
            started.elapsed()
        );
    }
    let mut providers = BTreeMap::<String, (usize, u64, u64)>::new();
    for row in store.usage_records(0, i64::MAX)? {
        let client_label = row.client.label();
        let key = row
            .provider_id
            .map(|id| format!("connection {id}"))
            .or(row.codex_provider)
            .unwrap_or_else(|| client_label.into());
        let entry = providers.entry(key).or_default();
        entry.0 += 1;
        entry.1 += row.request_count;
        entry.2 += row.usage.total();
    }
    for (provider, (rows, calls, tokens)) in providers {
        println!("{provider}: {rows} records, {calls} calls, {tokens} tokens");
    }
    Ok(())
}
