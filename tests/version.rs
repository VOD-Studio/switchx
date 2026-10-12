use std::{fs, process::Command};

#[test]
fn version_is_available_without_initializing_the_desktop_or_credentials() {
    let root = std::env::temp_dir().join(format!("switchx-version-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    for argument in ["--version", "-V"] {
        let output = Command::new(env!("CARGO_BIN_EXE_switchx"))
            .arg(argument)
            .env("SWITCHX_DATA_DIR", root.join("data"))
            .env("CODEX_HOME", root.join("codex"))
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("SwitchX {}\n", env!("CARGO_PKG_VERSION"))
        );
        assert!(output.stderr.is_empty());
    }
    assert!(!root.exists());
}
