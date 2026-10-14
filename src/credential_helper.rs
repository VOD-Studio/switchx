//! The `credential` and `local-token` helper commands that Codex runs to read
//! secrets. Each names an explicit absolute data directory and opens its
//! database read-only, without creating or migrating it.

use std::{ffi::OsString, path::PathBuf};

use crate::{credentials::Secret, storage::Store};

/// The persistent executable Codex should invoke, including after this launch exits.
pub fn executable() -> Result<PathBuf, String> {
    let current = std::env::current_exe().map_err(|_| "无法定位 SwitchX 凭据程序")?;
    #[cfg(target_os = "linux")]
    return appimage_executable(
        current,
        std::env::var_os("APPIMAGE")
            .as_deref()
            .map(std::path::Path::new),
        std::env::var_os("APPDIR")
            .as_deref()
            .map(std::path::Path::new),
    );
    #[cfg(not(target_os = "linux"))]
    Ok(current)
}

#[cfg(target_os = "linux")]
fn appimage_executable(
    current: PathBuf,
    image: Option<&std::path::Path>,
    directory: Option<&std::path::Path>,
) -> Result<PathBuf, String> {
    use std::{io::Read, os::unix::fs::PermissionsExt, path::Path};

    // These variables can be inherited from an unrelated AppImage (e.g. a terminal).
    // Only trust them when our executable is inside our own AppDir layout.
    let Some(directory) = directory.and_then(|path| path.canonicalize().ok()) else {
        return Ok(current);
    };
    if current.strip_prefix(&directory).ok() != Some(Path::new("usr/bin/switchx")) {
        return Ok(current);
    }
    let Some(image) = image else {
        // A manually extracted AppDir can also be installed in a permanent location.
        return Ok(current);
    };
    let invalid = "AppImage 文件路径不可用，请将文件保存在固定位置并重新打开 SwitchX";
    if !image.is_absolute() {
        return Err(invalid.into());
    }
    let image = image.canonicalize().map_err(|_| invalid)?;
    let metadata = image.metadata().map_err(|_| invalid)?;
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o111 == 0
        || image.starts_with(&directory)
    {
        return Err(invalid.into());
    }
    let mut header = [0_u8; 11];
    std::fs::File::open(&image)
        .and_then(|mut file| file.read_exact(&mut header))
        .map_err(|_| invalid)?;
    if &header[..4] != b"\x7fELF" || &header[8..] != b"AI\x02" {
        return Err(invalid.into());
    }
    Ok(image)
}

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

    #[cfg(target_os = "linux")]
    #[test]
    fn appimage_helper_survives_unmount_and_rejects_invalid_images() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = std::env::temp_dir().join(format!("switchx-appimage-{}", new_id().unwrap()));
        let appdir = root.join(".mount_SwitchX-test");
        let current = appdir.join("usr/bin/switchx");
        std::fs::create_dir_all(current.parent().unwrap()).unwrap();
        std::fs::write(&current, b"synthetic executable").unwrap();
        let image = root.join("SwitchX 中文 ' space.AppImage");
        std::fs::write(&image, b"\x7fELF\x02\x01\x01\x00AI\x02synthetic image").unwrap();
        std::fs::set_permissions(&image, std::fs::Permissions::from_mode(0o755)).unwrap();
        let link = root.join("SwitchX.AppImage");
        symlink(&image, &link).unwrap();
        let resolve =
            |image: &Path| appimage_executable(current.clone(), Some(image), Some(&appdir));
        assert_eq!(resolve(&link).unwrap(), image);
        for invalid in [
            Path::new("relative.AppImage"),
            &root.join("missing"),
            &current,
        ] {
            assert!(resolve(invalid).is_err());
        }
        std::fs::set_permissions(&image, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(resolve(&image).is_err());
        std::fs::set_permissions(&image, std::fs::Permissions::from_mode(0o755)).unwrap();
        let saved = resolve(&image).unwrap();
        std::fs::remove_dir_all(&appdir).unwrap();
        assert!(saved.is_file());
        assert!(!current.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn native_helper_ignores_another_appimages_environment() {
        let current = std::env::current_exe().unwrap();
        assert_eq!(
            appimage_executable(current.clone(), None, None).unwrap(),
            current
        );
        assert_eq!(
            appimage_executable(
                current.clone(),
                Some(Path::new("/missing.AppImage")),
                Some(current.parent().unwrap())
            )
            .unwrap(),
            current,
        );
        assert_eq!(
            appimage_executable(
                current.clone(),
                Some(Path::new("/missing.AppImage")),
                Some(Path::new("/nonexistent/AppDir"))
            )
            .unwrap(),
            current,
        );
    }

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
