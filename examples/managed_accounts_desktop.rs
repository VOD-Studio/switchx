//! Isolated desktop fixture for local account binding edits. All accounts are
//! synthetic and Codex CLI operations are disabled. Quit the fixture app before
//! pressing Enter to remove its bundle and data. Do not use online login actions.

use std::{
    io,
    path::{Path, PathBuf},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;
use switchx::{accounts::AccountManager, app, chatgpt, storage::AccountBinding};

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn xml_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn fixture_bundle(root: &Path, binary: &Path, data: &Path, home: &Path) -> io::Result<PathBuf> {
    let bundle = root.join("SwitchX Account Fixture.app");
    let contents = bundle.join("Contents");
    std::fs::create_dir_all(contents.join("MacOS"))?;
    std::fs::copy(binary, contents.join("MacOS/switchx"))?;
    let identifier = format!(
        "dev.switchx.fixture.accounts.{}",
        app::new_id().map_err(io::Error::other)?
    );
    let data = xml_text(&data.to_string_lossy());
    let home = xml_text(&home.to_string_lossy());
    // A separate bundle identity prevents LaunchServices selecting an installed
    // SwitchX. Its environment is part of the bundle, including when opened via UI.
    std::fs::write(
        contents.join("Info.plist"),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>{identifier}</string>
<key>CFBundleName</key><string>SwitchX Account Fixture</string>
<key>CFBundleDisplayName</key><string>SwitchX Account Fixture</string>
<key>CFBundleExecutable</key><string>switchx</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>NSHighResolutionCapable</key><true/>
<key>LSEnvironment</key><dict>
<key>SWITCHX_DATA_DIR</key><string>{data}</string>
<key>CODEX_HOME</key><string>{home}</string>
<key>SWITCHX_CODEX_CLI</key><string>/usr/bin/false</string>
<key>OPENAI_API_KEY</key><string></string>
<key>CODEX_API_KEY</key><string></string>
<key>CODEX_ACCESS_TOKEN</key><string></string>
<key>OPENAI_BASE_URL</key><string></string>
</dict></dict></plist>
"#
        ),
    )?;
    Ok(bundle)
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
    let mut account_ids = Vec::new();
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
        if label == "B" {
            manager.set_default(&account.id)?;
        }
        account_ids.push(account.id);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(manager.activate(&account_ids[1], &home))?;
    let catalog: serde_json::Value =
        serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json"))?;
    let mut model = catalog["models"][1].clone();
    model["slug"] = json!("synthetic-shared-chatgpt");
    model["display_name"] = json!("Synthetic shared ChatGPT model");
    let models = [model];
    // Keep the old connection/public ID visible alongside a new independent connection.
    chatgpt::save_connection(&data, &models)?;
    chatgpt::save_subscription(
        &data,
        Some(chatgpt::PROVIDER_ID),
        "ChatGPT · 合成账号 A",
        AccountBinding::Fixed(account_ids[0].clone()),
        &[],
    )?;
    chatgpt::save_subscription(
        &data,
        None,
        "ChatGPT · 合成账号 B",
        AccountBinding::Fixed(account_ids[1].clone()),
        &models,
    )?;
    app::save_provider(
        &data,
        None,
        "API · 合成连接",
        "http://127.0.0.1:9/v1",
        "synthetic-api-model",
        "synthetic-api-key".into(),
    )?;
    let bundle = fixture_bundle(&root.0, &binary, &data, &home)?;
    println!("Open the synthetic fixture bundle: {}", bundle.display());
    println!(
        "Synthetic A/B providers bind independently to the same model slug; B is the native/default entry login. Verify local rename/rebind, distinct public IDs, default changes, and shared references after rebinding B to A. Codex CLI is disabled; do not use login, refresh, or other online actions. Quit the fixture app, then press Enter to clean up."
    );
    io::stdin().read_line(&mut String::new())?;
    Ok(())
}
