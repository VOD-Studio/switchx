//! System credential round trip for local router tokens; provider keys live in SQLite.

use switchx::credentials::{CredentialError, CredentialStore, Secret};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let service = format!(
        "dev.switchx.probe.{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    let store = CredentialStore::new(&service)?;
    let test_secret = Secret::new("switchx-synthetic-keychain-probe".into());
    store.put("synthetic", &test_secret)?;
    let observed = store.get("synthetic");
    let cleanup = store.delete("synthetic");
    if observed?.expose() != test_secret.expose() {
        return Err("Keychain returned a different value".into());
    }
    cleanup?;
    if !matches!(store.get("synthetic"), Err(CredentialError::Missing)) {
        return Err("Keychain probe entry still exists after cleanup".into());
    }
    println!("system credential round-trip and cleanup succeeded");
    Ok(())
}
