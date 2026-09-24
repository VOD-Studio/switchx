use std::collections::HashMap;

use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteBinding {
    pub provider_id: String,
    pub upstream_model: String,
}

#[derive(Clone, Copy)]
pub struct Selection<'a> {
    pub public_id: &'a str,
    pub display_name: &'a str,
    pub provider_id: &'a str,
    pub upstream_model: &'a str,
}

#[derive(Debug)]
pub struct Publication {
    pub catalog: Value,
    pub routes: HashMap<String, RouteBinding>,
}

pub fn publish(templates: &Value, selections: &[Selection<'_>]) -> Result<Publication, String> {
    let source = templates["models"]
        .as_array()
        .ok_or("catalog templates must contain a models array")?;
    let mut models = Vec::with_capacity(selections.len());
    let mut routes = HashMap::with_capacity(selections.len());

    for selection in selections {
        let id = selection.public_id;
        if id.is_empty()
            || id.len() > 64
            || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || id.starts_with('-')
            || id.ends_with('-')
        {
            return Err(format!("invalid public model ID: {id}"));
        }
        if selection.display_name.trim().is_empty()
            || selection.provider_id.is_empty()
            || selection.upstream_model.is_empty()
        {
            return Err(format!("incomplete binding for {id}"));
        }
        if routes.contains_key(id) {
            return Err(format!("duplicate public model ID: {id}"));
        }

        let mut matches = source
            .iter()
            .filter(|model| model["slug"].as_str() == Some(selection.upstream_model));
        let mut model = matches
            .next()
            .ok_or_else(|| format!("missing metadata for {}", selection.upstream_model))?
            .clone();
        if matches.next().is_some() {
            return Err(format!(
                "ambiguous metadata for {}",
                selection.upstream_model
            ));
        }
        model["slug"] = id.into();
        model["display_name"] = selection.display_name.into();
        model["visibility"] = "list".into();
        models.push(model);
        routes.insert(
            id.to_owned(),
            RouteBinding {
                provider_id: selection.provider_id.to_owned(),
                upstream_model: selection.upstream_model.to_owned(),
            },
        );
    }

    Ok(Publication {
        catalog: json!({ "models": models }),
        routes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn templates() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json")).unwrap()
    }

    #[test]
    fn publishes_distinct_aliases_without_changing_source_capabilities() {
        let selections = [
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
        ];
        let publication = publish(&templates(), &selections).unwrap();
        let models = publication.catalog["models"].as_array().unwrap();
        let expected: Value =
            serde_json::from_str(include_str!("../tests/fixtures/published-models.json")).unwrap();
        assert_eq!(publication.catalog, expected);

        assert_eq!(models[0]["slug"], "sx-ds-flash");
        assert_eq!(models[0]["context_window"], 1_048_576);
        assert_eq!(models[1]["slug"], "sx-oai-coding");
        assert_eq!(models[1]["context_window"], 272_000);
        assert_eq!(publication.routes["sx-ds-flash"].provider_id, "deepseek");
        assert_eq!(
            publication.routes["sx-oai-coding"].upstream_model,
            "gpt-5.5"
        );
    }

    #[test]
    fn rejects_ambiguous_or_unknown_routes() {
        let selection = Selection {
            public_id: "sx-known",
            display_name: "Known",
            provider_id: "p",
            upstream_model: "deepseek-flash",
        };
        assert!(
            publish(
                &templates(),
                &[
                    selection,
                    Selection {
                        public_id: "sx-known",
                        display_name: "Again",
                        provider_id: "p",
                        upstream_model: "gpt-5.5"
                    }
                ]
            )
            .unwrap_err()
            .contains("duplicate")
        );
        assert!(
            publish(
                &templates(),
                &[Selection {
                    public_id: "bad/name",
                    ..selection
                }]
            )
            .unwrap_err()
            .contains("invalid")
        );
        assert!(
            publish(
                &templates(),
                &[Selection {
                    upstream_model: "absent",
                    ..selection
                }]
            )
            .unwrap_err()
            .contains("missing")
        );
    }
}
