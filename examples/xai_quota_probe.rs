//! Deliberate, non-inference live check of the default managed Grok account.
//! cargo run --example xai_quota_probe -- /absolute/SwitchX/data/directory
//! May rotate OAuth credentials. Prints billing values only, never credentials or account IDs.
use std::path::PathBuf;
use switchx::xai::AccountManager;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("pass an absolute data directory")?,
    );
    if !path.is_absolute() || !path.join("xai_oauth_auth.json").is_file() {
        return Err("an existing absolute Grok account directory is required".into());
    }
    let manager = AccountManager::open(&path)?;
    let account = manager
        .list()?
        .into_iter()
        .find(|a| a.is_default)
        .ok_or("no default Grok account")?;
    let quota = manager.quota(&account.id).await?;
    println!(
        "{}: remaining {:.1}%; resets at {}",
        quota.period_label,
        quota.remaining_percent,
        quota
            .resets_at
            .and_then(|v| chrono::DateTime::from_timestamp(v, 0))
            .map(|v| v.to_rfc3339())
            .unwrap_or_else(|| "unspecified".into())
    );
    Ok(())
}
