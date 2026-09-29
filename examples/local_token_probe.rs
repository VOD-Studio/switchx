//! Isolated SQLite round trip and cleanup for one synthetic local router token.

use switchx::{app, credentials::Secret, storage::Store};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("switchx-local-token-probe-{}", app::new_id()?));
    std::fs::create_dir(&root)?;
    let path = root.join("switchx.sqlite");
    let reference = format!("router-{}", app::new_id()?);
    let test_secret = Secret::new("a".repeat(64));
    let store = Store::open(&path)?;
    store.put_local_token(&reference, &test_secret)?;
    let reader = Store::open_read_only(&path)?;
    if reader
        .local_token(&reference)?
        .is_none_or(|observed| observed.expose() != test_secret.expose())
    {
        return Err("SQLite returned a different synthetic token".into());
    }
    store.delete_local_token(&reference)?;
    if reader.local_token(&reference)?.is_some() {
        return Err("synthetic local token still exists after cleanup".into());
    }
    drop(reader);
    drop(store);
    std::fs::remove_dir_all(&root)?;
    println!("SQLite synthetic local token round-trip and cleanup succeeded");
    Ok(())
}
