use std::{env, path::PathBuf};

use switchx::{
    catalog::{Selection, publish},
    config::preview_route,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let catalog_path = PathBuf::from(
        env::args()
            .nth(1)
            .ok_or("usage: config_probe ABSOLUTE_CATALOG_PATH")?,
    );
    let templates = serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json"))?;
    let publication = publish(
        &templates,
        &[Selection {
            public_id: "sx-ds-flash",
            display_name: "DeepSeek · Flash",
            provider_id: "deepseek",
            upstream_model: "deepseek-flash",
        }],
    )?;
    let preview = preview_route(
        include_str!("../tests/fixtures/codex-user-config.toml"),
        &publication,
        &catalog_path,
        "127.0.0.1:18731".parse()?,
        "sx-ds-flash",
    )?;
    eprintln!("changed fields: {}", preview.changed_fields.join(", "));
    eprintln!(
        "requires environment variable: {}",
        preview.required_environment_variable
    );
    print!("{}", preview.proposed);
    Ok(())
}
