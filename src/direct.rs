use std::{path::Path, time::Duration};

use reqwest::{Client, Url, redirect::Policy};
use serde_json::Value;

use crate::storage::ProviderRecord;

pub fn validate_provider(name: &str, base_url: &str, model_id: &str) -> Result<Url, String> {
    if name.trim().is_empty() || name.len() > 120 || name.chars().any(char::is_control) {
        return Err("上游名称无效".into());
    }
    if model_id.is_empty()
        || model_id.len() > 128
        || !model_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._/".contains(&byte))
    {
        return Err("模型 ID 只能包含 ASCII 字母、数字、-._/".into());
    }
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
    let mut url = validate_provider(&provider.name, &provider.base_url, &provider.model_id)?;
    if token.is_empty() {
        return Err("上游凭据缺失".into());
    }
    let path = format!("{}/models", url.path().trim_end_matches('/'));
    url.set_path(&path);
    let client = Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(12))
        .build()
        .map_err(|_| "无法创建连接检查客户端")?;
    let mut response = client
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| "无法连接上游或连接超时")?;
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
    let found = catalog["data"]
        .as_array()
        .is_some_and(|models| models.iter().any(|model| model["id"] == provider.model_id));
    if !found {
        return Err("模型目录中没有所选模型 ID".into());
    }
    Ok(())
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
        assert!(validate_provider("ok", "http://192.168.1.2/v1", "model-1").is_err());
        assert!(validate_provider("ok", "https://user:secret@example.test/v1", "m").is_err());
        assert!(validate_provider("ok", "https://example.test/v1?token=secret", "m").is_err());
        assert!(validate_provider("ok", "https://example.test/v1", "model with spaces").is_err());
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
