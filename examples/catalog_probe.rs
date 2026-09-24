use switchx::catalog::{Selection, publish};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let templates = serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json"))?;
    let publication = publish(
        &templates,
        &[
            Selection {
                public_id: "sx-ds-flash",
                display_name: "DeepSeek · Flash",
                provider_id: "deepseek",
                upstream_model: "deepseek-flash",
            },
            Selection {
                public_id: "sx-oai-coding",
                display_name: "OpenAI · Coding",
                provider_id: "openai-api",
                upstream_model: "gpt-5.5",
            },
        ],
    )?;
    println!("{}", serde_json::to_string_pretty(&publication.catalog)?);
    Ok(())
}
