use std::{path::Path, time::Duration};

use reqwest::{Client, Url, redirect::Policy};
use serde_json::Value;

use crate::storage::ProviderRecord;

pub fn validate_provider(name: &str, base_url: &str, model_id: &str) -> Result<Url, String> {
    if name.trim().is_empty() || name.len() > 120 || name.chars().any(char::is_control) {
        return Err("上游名称无效".into());
    }
    validate_model_id(model_id)?;
    validate_base_url(base_url)
}

pub fn validate_model_id(model_id: &str) -> Result<(), String> {
    if model_id.is_empty()
        || model_id.len() > 128
        || !model_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._/:".contains(&byte))
    {
        return Err("模型 ID 只能包含 ASCII 字母、数字、-._/:".into());
    }
    Ok(())
}

pub fn validate_base_url(base_url: &str) -> Result<Url, String> {
    let url = Url::parse(base_url).map_err(|_| "上游地址无效")?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("上游地址不能包含凭据、查询参数或片段".into());
    }
    match url.scheme() {
        "https" => {}
        "http" if url.host_str() == Some("127.0.0.1") => {}
        _ => return Err("上游地址必须是 HTTPS 或 127.0.0.1 HTTP".into()),
    }
    if url.path_segments().is_none_or(|segments| {
        segments
            .filter(|segment| !segment.is_empty())
            .any(|segment| segment == "." || segment == "..")
    }) {
        return Err("上游地址路径无效".into());
    }
    Ok(url)
}

pub async fn check_models(provider: &ProviderRecord, token: &str) -> Result<(), String> {
    validate_provider(&provider.name, &provider.base_url, &provider.model_id)?;
    let models = fetch_models(&provider.base_url, token).await?;
    if !models.iter().any(|model| model == &provider.model_id) {
        return Err("模型目录中没有所选模型 ID".into());
    }
    Ok(())
}

pub async fn fetch_models(base_url: &str, token: &str) -> Result<Vec<String>, String> {
    let mut url = validate_base_url(base_url)?;
    if token.is_empty() {
        return Err("上游凭据缺失".into());
    }
    let base_path = url.path().trim_end_matches('/').to_owned();
    let paths = [
        format!("{base_path}/models"),
        format!("{base_path}/v1/models"),
    ];
    let client = Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(12))
        .build()
        .map_err(|_| "无法创建连接检查客户端")?;
    // Only a missing endpoint triggers another candidate; never retry an auth error.
    for (index, path) in paths.iter().enumerate() {
        url.set_path(path);
        let mut response = client
            .get(url.clone())
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| "无法连接上游或连接超时")?;
        if index == 0
            && matches!(response.status().as_u16(), 404 | 405)
            && !base_path.ends_with("/v1")
        {
            continue;
        }
        if !response.status().is_success() {
            return Err(format!(
                "上游模型目录返回 HTTP {}",
                response.status().as_u16()
            ));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "读取模型目录失败")? {
            if body.len() + chunk.len() > 512 * 1024 {
                return Err("上游模型目录过大".into());
            }
            body.extend_from_slice(&chunk);
        }
        let catalog: Value = serde_json::from_slice(&body).map_err(|_| "上游模型目录不是 JSON")?;
        return parse_models(&catalog);
    }
    Err("上游没有可用的模型列表接口，请手动填写模型 ID".into())
}

fn parse_models(catalog: &Value) -> Result<Vec<String>, String> {
    let (entries, keys) = if let Some(entries) = catalog["data"].as_array() {
        (entries, &["id"][..])
    } else if let Some(entries) = catalog["models"].as_array() {
        (entries, &["slug", "id"][..])
    } else {
        return Err("模型列表缺少 data 或 models 数组，请手动填写模型 ID".into());
    };
    let mut models: Vec<String> = entries
        .iter()
        .filter_map(|entry| {
            keys.iter()
                .find_map(|key| {
                    entry[*key]
                        .as_str()
                        .filter(|id| validate_model_id(id).is_ok())
                })
                .map(str::to_owned)
        })
        .collect();
    models.sort();
    models.dedup();
    Ok(models)
}

pub fn helper_is_usable(path: &Path) -> bool {
    path.is_absolute() && path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        http::{HeaderMap, header},
        routing::get,
    };
    use serde_json::json;
    use tokio::net::TcpListener;

    #[test]
    fn rejects_unsafe_provider_inputs() {
        assert!(validate_provider("ok", "https://example.test/v1", "model-1").is_ok());
        assert!(validate_provider("ok", "https://example.test/v1", "vendor/model:free").is_ok());
        assert!(validate_provider("ok", "http://192.168.1.2/v1", "model-1").is_err());
        assert!(validate_provider("ok", "https://user:secret@example.test/v1", "m").is_err());
        assert!(validate_provider("ok", "https://example.test/v1?token=secret", "m").is_err());
        assert!(validate_provider("ok", "https://example.test/v1", "model with spaces").is_err());
    }

    #[test]
    fn discovery_accepts_both_shapes_and_returns_unique_valid_ids() {
        assert_eq!(
            parse_models(&json!({"data": [
            {"id":"z-model"}, {"id":"a/model"}, {"id":"z-model"},
            {"id":""}, {"id":"bad model"}, {"id":42}
        ], "models": "unrelated metadata"}))
            .unwrap(),
            ["a/model", "z-model"]
        );
        assert_eq!(
            parse_models(&json!({"models":[
                {"slug":"b-model", "id":"ignored"}, {"id":"a-model"}, {"slug":"b-model"}
            ]}))
            .unwrap(),
            ["a-model", "b-model"]
        );
        assert!(parse_models(&json!({"data":[]})).unwrap().is_empty());
        assert!(parse_models(&json!({"error":"secret upstream body"})).is_err());
    }

    #[tokio::test]
    async fn discovery_falls_back_only_for_missing_endpoints_and_redacts_errors() {
        use axum::http::StatusCode;
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let called = Arc::new(AtomicUsize::new(0));
        let calls = called.clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mock = Router::new()
                .route(
                    "/v1/models",
                    get(move |headers: HeaderMap| {
                        calls.fetch_add(1, Ordering::SeqCst);
                        async move {
                            assert_eq!(headers[header::AUTHORIZATION], "Bearer synthetic-test-key");
                            Json(json!({"models":[{"slug":"mock-model"}]}))
                        }
                    }),
                )
                .route(
                    "/auth/models",
                    get(|| async { (StatusCode::UNAUTHORIZED, "synthetic-test-key") }),
                )
                .route(
                    "/large/models",
                    get(|| async { "x".repeat(512 * 1024 + 1) }),
                )
                .route(
                    "/redirect/models",
                    get(|| async { (StatusCode::FOUND, [(header::LOCATION, "/v1/models")]) }),
                );
            axum::serve(listener, mock).await.unwrap();
        });
        assert_eq!(
            fetch_models(&format!("http://{address}"), "synthetic-test-key")
                .await
                .unwrap(),
            ["mock-model"]
        );
        assert_eq!(called.load(Ordering::SeqCst), 1);
        for (path, expected) in [("auth", "401"), ("large", "过大"), ("redirect", "302")] {
            let error = fetch_models(&format!("http://{address}/{path}"), "synthetic-test-key")
                .await
                .unwrap_err();
            assert!(error.contains(expected));
            assert!(!error.contains("synthetic-test-key"));
        }
        assert_eq!(called.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn checks_selected_model_with_provider_token() {
        async fn models(headers: HeaderMap) -> Json<Value> {
            assert_eq!(
                headers.get(header::AUTHORIZATION).unwrap(),
                "Bearer synthetic-test-key"
            );
            Json(json!({"data": [{"id": "mock-model"}]}))
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/v1/models", get(models)))
                .await
                .unwrap();
        });
        let mut provider = ProviderRecord {
            kind: crate::storage::ProviderKind::ApiKey,
            account_binding: None,
            icon_id: None,
            id: "mock".into(),
            name: "Mock".into(),
            base_url: format!("http://{address}/v1"),
            model_id: "mock-model".into(),
            credential_ref: Some("mock".into()),
        };
        check_models(&provider, "synthetic-test-key").await.unwrap();
        provider.model_id = "other-model".into();
        assert!(
            check_models(&provider, "synthetic-test-key")
                .await
                .unwrap_err()
                .contains("没有")
        );
        task.abort();
    }
}
