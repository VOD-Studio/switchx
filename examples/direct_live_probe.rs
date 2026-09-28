#[path = "support/live_probe.rs"]
mod live_probe;

use std::path::{Path, PathBuf};
use switchx::{
    credentials::{CredentialStore, PROVIDER_KEY_SERVICE},
    direct,
    direct_config::{self, PreparedDirectSwitch},
    storage::{ProviderRecord, Store},
};

const ORIGINAL_CONFIG: &str = "# isolated real direct acceptance\nmodel = \"gpt-5.5\" # preserve comment\napproval_policy = \"never\"\n\n[mcp_servers.acceptance_disabled]\ncommand = \"false\"\nenabled = false\n";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let data = PathBuf::from(
        args.next()
            .ok_or("usage: direct_live_probe DATA_DIR PROVIDER_ID")?,
    );
    let id = args
        .next()
        .and_then(|id| id.into_string().ok())
        .ok_or("provider ID is required")?;
    if args.next().is_some() || !data.is_absolute() || !data.join("switchx.sqlite").is_file() {
        return Err("use an absolute existing SwitchX data directory".into());
    }
    let provider = Store::open_read_only(&data.join("switchx.sqlite"))?
        .provider(&id)?
        .ok_or("provider was not found")?;
    let helper = std::env::current_exe()?
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("switchx");
    if !direct::helper_is_usable(&helper) {
        return Err("run cargo build --bin switchx first".into());
    }
    let mut nonce = [0_u8; 8];
    getrandom::fill(&mut nonce).map_err(|_| "system randomness is unavailable")?;
    let suffix: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    let root = std::env::temp_dir().join(format!("switchx-direct-live-{suffix}"));
    std::fs::create_dir(&root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    }
    let home = root.join("codex");
    let state = root.join("state");
    std::fs::create_dir(&home)?;
    std::fs::create_dir(&state)?;
    let config = home.join("config.toml");
    std::fs::write(&config, ORIGINAL_CONFIG)?;
    let result = run(&home, &state, &provider, &helper, &suffix).await;
    if state.join("direct-journal.json").exists() {
        let restored = direct_config::restore(&config, &state).map_err(|_| {
            format!(
                "restore failed; isolated files retained at {}",
                root.display()
            )
        })?;
        if !restored.conflicts.is_empty() {
            return Err(format!(
                "restore conflict; isolated files retained at {}",
                root.display()
            )
            .into());
        }
    }
    if std::fs::read_to_string(&config)? != ORIGINAL_CONFIG {
        return Err(format!("config differed after restore; inspect {}", root.display()).into());
    }
    std::fs::remove_dir_all(&root)?;
    println!("Isolated config restored exactly; temporary Codex home removed");
    result
}

async fn run(
    home: &Path,
    state: &Path,
    provider: &ProviderRecord,
    helper: &Path,
    suffix: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let credentials = CredentialStore::new(PROVIDER_KEY_SERVICE)?;
    let secret = credentials.get(
        provider
            .credential_ref
            .as_deref()
            .ok_or("provider has no credential reference")?,
    )?;
    direct::check_models(provider, secret.expose()).await?;
    drop(secret);
    println!("Real upstream /models contains the selected model");
    PreparedDirectSwitch::inspect(&home.join("config.toml"), state, provider, helper)?.apply()?;
    let greeting = format!("SWITCHX_DIRECT_{suffix}");
    live_probe::answer(home, &provider.model_id, &greeting).await?;
    println!("Codex CLI completed a real direct Responses answer using the keychain helper");
    let marker = format!("SWITCHX_TOOL_{suffix}");
    live_probe::file_round_trip(home, &provider.model_id, &marker).await?;
    println!("Real file tool completed; its result was used in the second-round answer");
    Ok(())
}
