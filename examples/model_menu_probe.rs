//! Keep a synthetic Codex home and the production launcher available for TUI inspection.
//! Run the printed launcher, inspect /model without sending a prompt, then press Ctrl-C here.

use std::{env, fs, path::PathBuf};

use serde_json::{Value, json};
use switchx::{app, client};

struct ProbeDirectory(PathBuf);

impl Drop for ProbeDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if env::args().skip(1).any(|argument| argument != "--desktop") {
        return Err("Usage: cargo run --example model_menu_probe -- --desktop".into());
    }
    let path = env::temp_dir().join(format!("switchx-model-menu-probe-{}", app::new_id()?));
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&path)?;
    let root = ProbeDirectory(path);
    let home = root.0.join("codex");
    fs::create_dir(&home)?;
    let mut catalog: Value =
        serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json"))?;
    for (model, (id, name)) in catalog["models"]
        .as_array_mut()
        .ok_or("fixture models missing")?
        .iter_mut()
        .zip([
            ("sx-mock-alpha", "SwitchX Mock Alpha"),
            ("sx-mock-beta", "SwitchX Mock Beta"),
        ])
    {
        model["slug"] = id.into();
        model["display_name"] = name.into();
        model["visibility"] = "list".into();
    }
    let catalog_path = home.join("catalog.json");
    fs::write(&catalog_path, serde_json::to_vec_pretty(&catalog)?)?;
    fs::write(
        home.join("config.toml"),
        format!(
            "cli_auth_credentials_store = \"file\"\nmodel = \"sx-mock-alpha\"\nmodel_provider = \"switchx_mock\"\nmodel_catalog_json = {}\ncheck_for_update_on_startup = false\nopenai_base_url = \"http://127.0.0.1:1/v1\"\nchatgpt_base_url = \"http://127.0.0.1:1\"\n[analytics]\nenabled = false\n[model_providers.switchx_mock]\nname = \"Synthetic loopback only\"\nbase_url = \"http://127.0.0.1:1/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = false\n",
            toml_edit::Value::from(catalog_path.to_str().ok_or("fixture path is not UTF-8")?)
        ),
    )?;
    let auth_path = home.join("auth.json");
    fs::write(
        &auth_path,
        br#"{"OPENAI_API_KEY":"synthetic-model-menu-key"}"#,
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&auth_path, fs::Permissions::from_mode(0o600))?;
    }
    let executable = client::cli_path()?;
    let version = client::check_catalog_using(&catalog, &executable).await?;
    let launcher = client::write_codex_launcher(&root.0, &home, &executable).await?;
    println!(
        "{}",
        json!({
            "launcher":launcher,
            "codex_home":home,
            "client":executable,
            "version":version,
            "models":["sx-mock-alpha", "sx-mock-beta"],
            "mode":"synthetic catalog; model menu only; no inference requests",
        })
    );
    tokio::signal::ctrl_c().await?;
    Ok(())
}
