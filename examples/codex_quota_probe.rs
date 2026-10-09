//! Deliberate non-inference check; may renew the managed account's private credentials.
//! cargo run --example codex_quota_probe -- /absolute/switchx-data /absolute/codex-home
use std::{fs, io, path::PathBuf};

fn read_optional(path: &std::path::Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let data = PathBuf::from(
        args.next()
            .ok_or("pass an absolute existing account data directory")?,
    );
    let home = PathBuf::from(args.next().ok_or("pass an absolute native Codex home")?);
    if !data.is_absolute()
        || !data.join("codex_oauth_auth.json").is_file()
        || !home.is_absolute()
        || args.next().is_some()
    {
        return Err(
            "expected an existing absolute account data directory and an absolute Codex home"
                .into(),
        );
    }
    let paths = ["auth.json", "config.toml", ".switchx-account.json"].map(|name| home.join(name));
    let before = paths
        .iter()
        .map(|path| read_optional(path))
        .collect::<io::Result<Vec<_>>>()?;
    let manager = switchx::accounts::AccountManager::open(&data)?;
    let id = manager
        .default_id()?
        .ok_or("no default managed ChatGPT account")?;
    let result = manager.quota(&id, &home).await;
    for (path, original) in paths.iter().zip(before) {
        if read_optional(path)? != original {
            return Err("native Codex files changed during quota query".into());
        }
    }
    let quota = result?;
    for window in quota.windows {
        println!(
            "{}: remaining {:.1}%; reset time provided: {}",
            window.period_label,
            window.remaining_percent,
            window.resets_at.is_some()
        );
    }
    println!(
        "Credits balance provided: {}; available resets: {}; native files unchanged",
        quota.credits_balance.is_some(),
        quota.reset_expires_at.len()
    );
    Ok(())
}
