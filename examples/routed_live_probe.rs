//! Explicitly selected saved models through the production route, in a disposable home.
//! Real upstream calls may be billable. Source metadata and provider credentials are retained.

#[path = "support/live_probe.rs"]
mod live_probe;

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};

use live_probe::ProbeResult;
use switchx::{
    config_transaction, direct,
    routed::RouteSession,
    storage::{ModelRecord, ProviderRecord, RequestRecord, RequestStatus, Store},
};
use tokio::{net::TcpListener, time::timeout};

const ORIGINAL: &str = "# isolated routed live acceptance\nmodel = \"original-model\" # keep\napproval_policy = \"never\"\n\n[mcp_servers.acceptance_disabled]\ncommand = \"false\"\nenabled = false\n";

#[tokio::main]
async fn main() -> ProbeResult {
    let mut args = std::env::args_os().skip(1);
    let data = PathBuf::from(
        args.next()
            .ok_or("usage: routed_live_probe DATA_DIR PUBLIC_MODEL_ID [SECOND_PUBLIC_MODEL_ID]")?,
    );
    let ids: Vec<String> = args
        .map(|id| id.into_string().map_err(|_| "model ID must be UTF-8"))
        .collect::<Result<_, _>>()?;
    if !data.is_absolute() || !data.join("switchx.sqlite").is_file() {
        return Err("use an absolute existing SwitchX data directory".into());
    }
    let source = Store::open_read_only(&data.join("switchx.sqlite"))
        .map_err(|_| "cannot inspect source metadata; open it in SwitchX to update its schema")?;
    let selected = select_models(&source, &ids)?;
    let keys = selected
        .iter()
        .map(|(_, provider)| {
            source.provider_api_key(&provider.id)?.ok_or_else(|| {
                "provider key is not stored in SQLite; enter an API Key in SwitchX first".into()
            })
        })
        .collect::<ProbeResult<Vec<_>>>()?;
    drop(source);
    let helper = std::env::current_exe()?
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("switchx");
    if !direct::helper_is_usable(&helper) {
        return Err("run cargo build --locked --bin switchx first".into());
    }

    let mut nonce = [0_u8; 8];
    getrandom::fill(&mut nonce).map_err(|_| "system randomness is unavailable")?;
    let suffix: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    let root = std::env::temp_dir().join(format!("switchx-routed-live-{suffix}"));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&root)?;
    println!("Isolated workspace: {}", root.display());
    let home = root.join("codex");
    let state = root.join("state");
    let mut session = RouteSession::default();
    // Register Ctrl-C while preparing, but finish the transaction before cancelling requests.
    let mut interrupted = tokio::spawn(tokio::signal::ctrl_c());
    let outcome = async {
        std::fs::create_dir(&home)?;
        std::fs::create_dir(&state)?;
        std::fs::write(home.join("config.toml"), ORIGINAL)?;
        let store = Store::open(&state.join("switchx.sqlite"))?;
        for ((model, provider), key) in selected.iter().zip(&keys) {
            store.put_provider_with_models_options_and_key(
                provider,
                std::slice::from_ref(model),
                None,
                key,
            )?;
        }
        drop(store);
        let default_model = &selected[0].0.public_id;
        println!(
            "{}",
            session
                .prepare(&state, &home, 0, default_model, &helper)
                .await?
        );
        session.apply(&state, &home, 0, default_model).await?;
        tokio::select! {
            biased;
            signal = &mut interrupted => {
                signal??;
                Err("probe interrupted by Ctrl-C".into())
            }
            result = exercise(&home, &state, &selected, &suffix) => result,
        }
    }
    .await;
    interrupted.abort();
    if let Err(error) = &outcome {
        eprintln!("Acceptance failed: {error}");
    }
    if let Err(error) = cleanup(&mut session, &home, &state).await {
        return Err(format!("{error}; isolated files retained at {}", root.display()).into());
    }
    std::fs::remove_dir_all(&root)?;
    println!(
        "Original isolated config restored exactly; route, local token and temporary files cleaned up"
    );
    outcome?;
    println!("Selected API route checks passed; source records and provider credentials retained");
    Ok(())
}

fn select_models(
    source: &Store,
    ids: &[String],
) -> ProbeResult<Vec<(ModelRecord, ProviderRecord)>> {
    if ids.is_empty() || ids.len() > 2 || ids.iter().collect::<HashSet<_>>().len() != ids.len() {
        return Err("explicitly select one or two distinct saved public model IDs".into());
    }
    let models = source.models()?;
    ids.iter()
        .map(|id| {
            let mut model = models
                .iter()
                .find(|model| &model.public_id == id)
                .cloned()
                .ok_or_else(|| format!("saved public model was not found: {id}"))?;
            let provider = source
                .provider(&model.provider_id)?
                .ok_or("selected model's provider was not found")?;
            // Only the explicitly selected destinations participate in acceptance.
            model.enabled = true;
            model.fallback_provider_id = None;
            Ok((model, provider))
        })
        .collect()
}

async fn exercise(
    home: &Path,
    state: &Path,
    selected: &[(ModelRecord, ProviderRecord)],
    suffix: &str,
) -> ProbeResult {
    let store = Store::open(&state.join("switchx.sqlite"))?;
    for (index, (model, _)) in selected.iter().enumerate() {
        let before = request_ids(&store)?;
        live_probe::answer(
            home,
            &model.public_id,
            &format!("SWITCHX_ANSWER_{suffix}_{index}"),
        )
        .await?;
        check_records(&store, &before, model, 1).await?;
        let before = request_ids(&store)?;
        live_probe::file_round_trip(
            home,
            &model.public_id,
            &format!("SWITCHX_TOOL_{suffix}_{index}"),
        )
        .await?;
        let count = check_records(&store, &before, model, 2).await?;
        println!(
            "{}: exact answer, file tool and second-round answer passed; {count} completed tool-round requests mapped to {} / {}",
            model.public_id, model.provider_id, model.upstream_model
        );
    }
    Ok(())
}

fn request_ids(store: &Store) -> ProbeResult<HashSet<String>> {
    Ok(store
        .requests(1000)?
        .into_iter()
        .map(|record| record.id)
        .collect())
}

async fn check_records(
    store: &Store,
    before: &HashSet<String>,
    model: &ModelRecord,
    minimum: usize,
) -> ProbeResult<usize> {
    timeout(Duration::from_secs(3), async {
        loop {
            let records: Vec<_> = store
                .requests(1000)?
                .into_iter()
                .filter(|record| !before.contains(&record.id))
                .collect();
            if records.len() >= minimum {
                if !valid_records(&records, model, minimum) {
                    return Err(
                        "request records did not confirm completion and the exact destination"
                            .into(),
                    );
                }
                return Ok(records.len());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await?
}

fn valid_records(records: &[RequestRecord], model: &ModelRecord, minimum: usize) -> bool {
    records.len() >= minimum
        && records.iter().all(|record| {
            record.status == RequestStatus::Completed
                && record.http_status == Some(200)
                && record.public_model.as_deref() == Some(model.public_id.as_str())
                && record.provider_id.as_deref() == Some(model.provider_id.as_str())
                && record.upstream_model.as_deref() == Some(model.upstream_model.as_str())
                && record.generation.starts_with("catalog-")
                && record.headers_ms.is_some()
                && record.first_event_ms.is_some()
                && record.error_code.is_none()
                && record.fallback_from.is_none()
        })
}

async fn cleanup(session: &mut RouteSession, home: &Path, state: &Path) -> ProbeResult {
    let address = session.address();
    let recovery = config_transaction::recovery(state)?;
    let reference = recovery
        .as_ref()
        .and_then(|recovery| recovery.local_token_reference.clone());
    if recovery.is_some() {
        session.restore(state, home).await?;
    }
    session.discard_preview();
    if session.is_running() || config_transaction::recovery(state)?.is_some() {
        return Err("route or recovery journal remained after restore".into());
    }
    if home.join("config.toml").exists()
        && std::fs::read(home.join("config.toml"))? != ORIGINAL.as_bytes()
    {
        return Err("isolated config differed after restore".into());
    }
    if let Some(reference) = reference
        && Store::open_read_only(&state.join("switchx.sqlite"))?
            .local_token(&reference)?
            .is_some()
    {
        return Err("local route token was not removed".into());
    }
    if let Some(address) = address {
        let _released = TcpListener::bind(address)
            .await
            .map_err(|_| "route port remained occupied after restore")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_must_confirm_two_tool_rounds_and_the_selected_destination() {
        let model = ModelRecord {
            provider_id: "alpha".into(),
            public_id: "sx-alpha".into(),
            display_name: "Alpha".into(),
            upstream_model: "shared".into(),
            metadata: "{}".into(),
            enabled: true,
            fallback_provider_id: None,
        };
        let record = RequestRecord {
            id: "one".into(),
            started_at_ms: 1,
            public_model: Some(model.public_id.clone()),
            provider_id: Some(model.provider_id.clone()),
            upstream_model: Some(model.upstream_model.clone()),
            generation: "catalog-test.json".into(),
            http_status: Some(200),
            headers_ms: Some(1),
            first_event_ms: Some(2),
            duration_ms: 3,
            status: RequestStatus::Completed,
            error_code: None,
            fallback_from: None,
        };
        assert!(!valid_records(std::slice::from_ref(&record), &model, 2));
        assert!(valid_records(&[record.clone(), record.clone()], &model, 2));
        for change in [
            "provider", "public", "upstream", "status", "fallback", "timing",
        ] {
            let mut wrong = record.clone();
            match change {
                "provider" => wrong.provider_id = Some("beta".into()),
                "public" => wrong.public_model = Some("sx-beta".into()),
                "upstream" => wrong.upstream_model = Some("other".into()),
                "status" => wrong.status = RequestStatus::Interrupted,
                "fallback" => wrong.fallback_from = Some("beta".into()),
                "timing" => wrong.first_event_ms = None,
                _ => unreachable!(),
            }
            assert!(!valid_records(&[record.clone(), wrong], &model, 2));
        }
    }
}
