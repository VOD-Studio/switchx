//! Import Codex session-log usage into a scratch database and print totals.
//! Reads only usage counts; never writes the SwitchX data directory or CODEX_HOME.
//! Run: cargo run --release --example usage_import_probe -- /abs/codex/home /abs/scratch.sqlite

use std::{collections::BTreeMap, path::Path, sync::atomic::AtomicBool, time::Instant};

use switchx::{storage::Store, usage::codex_logs};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(home), Some(database)) = (args.next(), args.next()) else {
        return Err("pass an absolute CODEX_HOME and an absolute scratch database path".into());
    };
    let (home, database) = (Path::new(&home), Path::new(&database));
    if !home.is_absolute() || !database.is_absolute() {
        return Err("both paths must be absolute".into());
    }
    let store = Store::open(database)?;
    for pass in ["first", "repeat"] {
        let started = Instant::now();
        let summary = codex_logs::import(&store, home, &AtomicBool::new(false), |_| {})
            .map_err(|error| format!("{error:?}"))?;
        println!(
            "{pass} scan: {} files, {} read, {} rows added, {} unreadable in {:.2?}",
            summary.files,
            summary.read,
            summary.added,
            summary.unreadable,
            started.elapsed()
        );
    }
    let mut providers = BTreeMap::<String, (usize, u64)>::new();
    for row in store.usage_records(0, i64::MAX)? {
        let key = row
            .provider_id
            .map(|id| format!("connection {id}"))
            .or(row.codex_provider)
            .unwrap_or_else(|| "unknown".into());
        let entry = providers.entry(key).or_default();
        entry.0 += 1;
        entry.1 += row.usage.total();
    }
    for (provider, (rows, tokens)) in providers {
        println!("{provider}: {rows} responses, {tokens} tokens");
    }
    Ok(())
}
