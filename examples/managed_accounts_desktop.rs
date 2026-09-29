//! Isolated desktop fixture. All accounts are synthetic; network and Codex CLI
//! operations are disabled. Press Enter to stop this fixture and remove its data.

use std::{
    io,
    path::PathBuf,
    process::{Command, Stdio},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;
use switchx::{accounts::AccountManager, app, chatgpt};

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let binary = std::env::current_exe()?
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("SwitchX.app/Contents/MacOS/switchx");
    if !binary.is_file() {
        return Err("run sh scripts/bundle-macos.sh first".into());
    }
    let root =
        Fixture(std::env::temp_dir().join(format!("switchx-accounts-desktop-{}", app::new_id()?)));
    let home = root.0.join("codex");
    let data = root.0.join("data");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir(&data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root.0, std::fs::Permissions::from_mode(0o700))?;
    }
    std::fs::write(
        home.join("config.toml"),
        "# synthetic desktop fixture\ncli_auth_credentials_store = \"file\"\n",
    )?;
    let manager = AccountManager::open(&data)?;
    for label in ["A", "B"] {
        let claims = json!({"sub":format!("synthetic-user-{label}"),
            "email":format!("synthetic-{label}@example.invalid"), "exp":4102444800u64,
            "https://api.openai.com/auth":{"chatgpt_account_id":format!("synthetic-workspace-{label}"), "chatgpt_plan_type":"plus"}});
        let token = format!(
            "{}.{}.synthetic-signature",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims)?)
        );
        let auth = json!({"auth_mode":"chatgpt","OPENAI_API_KEY":null,
            "tokens":{"id_token":token,"access_token":token,"refresh_token":format!("synthetic-refresh-{label}"),"account_id":format!("synthetic-workspace-{label}")},
            "last_refresh":"2099-01-01T00:00:00.000Z"});
        std::fs::write(home.join("auth.json"), serde_json::to_vec_pretty(&auth)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                home.join("auth.json"),
                std::fs::Permissions::from_mode(0o600),
            )?;
        }
        let account = manager.import_current(&home)?;
        if label == "A" {
            chatgpt::bind_managed_account(&data, Some(&account.id))?;
        } else {
            manager.set_default(&account.id)?;
        }
    }
    let mut child = Command::new(&binary)
        .env("SWITCHX_DATA_DIR", &data)
        .env("CODEX_HOME", &home)
        .env("SWITCHX_CODEX_CLI", "/usr/bin/false")
        .env_remove("OPENAI_API_KEY")
        .env_remove("CODEX_API_KEY")
        .env_remove("CODEX_ACCESS_TOKEN")
        .env_remove("OPENAI_BASE_URL")
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("ALL_PROXY", "http://127.0.0.1:9")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    println!("Isolated desktop PID {} · {}", child.id(), root.0.display());
    println!(
        "B is the native/default account; A is selected. Verify list, import B, use/default B, set default A, and removal. Press Enter to clean up."
    );
    let result = io::stdin().read_line(&mut String::new());
    let _ = child.kill();
    let _ = child.wait();
    result?;
    Ok(())
}
