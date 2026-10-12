//! The `credential` and `local-token` helper commands that Codex runs to read
//! secrets. Each names an explicit absolute data directory and opens its
//! database read-only, without creating or migrating it.

use std::{ffi::OsString, path::PathBuf};

use crate::{credentials::Secret, storage::Store};

/// Reads the secret a helper command names; `None` when no command was given.
pub fn read(
    mut args: impl Iterator<Item = OsString>,
) -> Result<Option<Secret>, Box<dyn std::error::Error>> {
    let Some(command) = args.next() else {
        return Ok(None);
    };
    if command != "credential" && command != "local-token" {
        return Err("unknown SwitchX command".into());
    }
    let reference = args.next().ok_or("credential reference is missing")?;
    let data_dir = PathBuf::from(
        args.next()
            .ok_or("credential data directory is missing; regenerate the SwitchX configuration")?,
    );
    if args.next().is_some() {
        return Err("unexpected credential command argument".into());
    }
    let reference = reference
        .to_str()
        .ok_or("credential reference is invalid")?;
    if !data_dir.is_absolute() {
        return Err("credential data directory must be absolute".into());
    }
    if command == "local-token"
        && !reference
            .strip_prefix("router-")
            .is_some_and(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err("local token reference is invalid".into());
    }
    let database = data_dir.join("switchx.sqlite");
    let directory_metadata = std::fs::symlink_metadata(&data_dir)
        .map_err(|_| "local token data directory is unavailable")?;
    let database_metadata =
        std::fs::symlink_metadata(&database).map_err(|_| "local token database is unavailable")?;
    if !directory_metadata.is_dir()
        || directory_metadata.file_type().is_symlink()
        || !database_metadata.is_file()
        || database_metadata.file_type().is_symlink()
    {
        return Err("credential storage must be a regular directory and database".into());
    }
    let store = Store::open_credentials_read_only(&database)?;
    let secret = if command == "credential" {
        store
            .provider_api_key(reference)?
            .ok_or("upstream API key is missing")?
    } else {
        store
            .local_token(reference)?
            .ok_or("local token is missing")?
    };
    Ok(Some(secret))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::{
        id::new_id,
        storage::{ProviderKind, ProviderRecord},
    };

    #[test]
    fn credential_helper_reads_api_key_without_migrating_v8_v9_or_v10() {
        let root = std::env::temp_dir().join(format!("switchx-key-helper-{}", new_id().unwrap()));
        for version in [8, 9, 10] {
            let data = root.join(format!("v{version}"));
            std::fs::create_dir_all(&data).unwrap();
            let database = data.join("switchx.sqlite");
            let provider = ProviderRecord {
                id: "synthetic-api".into(),
                name: "Synthetic API".into(),
                base_url: "https://example.invalid/v1".into(),
                model_id: "synthetic-model".into(),
                credential_ref: None,
                icon_id: None,
                kind: ProviderKind::ApiKey,
                account_binding: None,
            };
            {
                let store = Store::open(&database).unwrap();
                store
                    .put_provider_with_models_options_and_key(
                        &provider,
                        &[],
                        None,
                        &Secret::new("synthetic-key".into()),
                    )
                    .unwrap();
            }
            {
                let connection = rusqlite::Connection::open(&database).unwrap();
                connection
                    .pragma_update(None, "user_version", version)
                    .unwrap();
            }
            let before = std::fs::read(&database).unwrap();
            let value = read(
                [
                    std::ffi::OsString::from("credential"),
                    provider.id.into(),
                    data.as_os_str().to_owned(),
                ]
                .into_iter(),
            )
            .unwrap()
            .unwrap();
            assert_eq!(value.expose(), "synthetic-key");
            assert_eq!(std::fs::read(&database).unwrap(), before);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    fn local_token_helper_args(reference: &str, data_dir: &Path) -> [std::ffi::OsString; 3] {
        [
            "local-token".into(),
            reference.into(),
            data_dir.as_os_str().to_owned(),
        ]
    }

    #[test]
    fn local_token_helper_reads_only_its_explicit_database() {
        let root = std::env::temp_dir().join(format!("switchx-local-helper-{}", new_id().unwrap()));
        let reference = format!("router-{}", "a".repeat(32));
        for (directory, value) in [(root.join("first"), "a"), (root.join("second"), "b")] {
            std::fs::create_dir_all(&directory).unwrap();
            let database = directory.join("switchx.sqlite");
            let token = Secret::new(value.repeat(64));
            {
                let store = Store::open(&database).unwrap();
                store.put_local_token(&reference, &token).unwrap();
            }
            let before = std::fs::read(&database).unwrap();
            let result = read(local_token_helper_args(&reference, &directory).into_iter())
                .unwrap()
                .unwrap();
            assert_eq!(result.expose(), token.expose());
            assert_eq!(std::fs::read(&database).unwrap(), before);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_token_helper_rejects_bad_args_before_opening_a_database() {
        let missing = std::env::temp_dir().join(format!(
            "switchx-local-helper-missing-{}",
            new_id().unwrap()
        ));
        let reference = format!("router-{}", "a".repeat(32));
        let valid = local_token_helper_args(&reference, &missing);
        let cases = [
            (valid[..2].to_vec(), "data directory is missing"),
            (
                local_token_helper_args(&reference, Path::new("relative")).to_vec(),
                "data directory must be absolute",
            ),
            (
                valid.clone().into_iter().chain(["extra".into()]).collect(),
                "unexpected credential command argument",
            ),
            (
                local_token_helper_args("../outside", &missing).to_vec(),
                "local token reference is invalid",
            ),
        ];
        for (args, expected) in cases {
            let error = read(args.into_iter()).unwrap_err().to_string();
            assert!(error.contains(expected), "{error}");
            assert!(!missing.exists());
        }
        assert!(read(std::iter::empty()).unwrap().is_none());
    }

    #[test]
    fn local_token_helper_does_not_create_or_upgrade_databases() {
        let directory = std::env::temp_dir().join(format!(
            "switchx-local-helper-read-only-{}",
            new_id().unwrap()
        ));
        std::fs::create_dir(&directory).unwrap();
        let database = directory.join("switchx.sqlite");
        let reference = format!("router-{}", "a".repeat(32));
        let args = local_token_helper_args(&reference, &directory);
        assert!(read(args.clone().into_iter()).is_err());
        assert!(!database.exists());
        drop(Store::open(&database).unwrap());
        let before = std::fs::read(&database).unwrap();
        let error = read(args.clone().into_iter()).unwrap_err().to_string();
        assert_eq!(error, "local token is missing");
        assert_eq!(std::fs::read(&database).unwrap(), before);
        {
            let connection = rusqlite::Connection::open(&database).unwrap();
            connection
                .execute_batch("PRAGMA user_version = 7;")
                .unwrap();
        }
        let before = std::fs::read(&database).unwrap();
        assert!(read(args.into_iter()).is_err());
        assert_eq!(std::fs::read(&database).unwrap(), before);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn local_token_helper_rejects_symlinked_storage() {
        let root = std::env::temp_dir().join(format!(
            "switchx-local-helper-symlink-{}",
            new_id().unwrap()
        ));
        let data = root.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let database = data.join("switchx.sqlite");
        let reference = format!("router-{}", "a".repeat(32));
        {
            let store = Store::open(&database).unwrap();
            store
                .put_local_token(&reference, &Secret::new("a".repeat(64)))
                .unwrap();
        }
        let before = std::fs::read(&database).unwrap();
        let linked_directory = root.join("linked");
        std::os::unix::fs::symlink(&data, &linked_directory).unwrap();
        assert!(read(local_token_helper_args(&reference, &linked_directory).into_iter()).is_err());
        let linked_database_directory = root.join("linked-database");
        std::fs::create_dir(&linked_database_directory).unwrap();
        std::os::unix::fs::symlink(&database, linked_database_directory.join("switchx.sqlite"))
            .unwrap();
        assert!(
            read(local_token_helper_args(&reference, &linked_database_directory).into_iter())
                .is_err()
        );
        assert_eq!(std::fs::read(&database).unwrap(), before);
        std::fs::remove_dir_all(root).unwrap();
    }
}
