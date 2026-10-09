//! Read-only Codex subscription billing, following CC Switch's wham protocol.
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use crate::accounts::ManagedCredential;

pub(crate) const USAGE_ENDPOINT: &str = "https://chatgpt.com/backend-api/wham/usage";
pub(crate) const RESETS_ENDPOINT: &str =
    "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits";

#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub remaining_percent: f64,
    pub period_label: String,
    pub resets_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Quota {
    pub windows: Vec<Window>,
    pub credits_balance: Option<f64>,
    /// Available resets' expiries; None means the service supplied no expiry.
    pub reset_expires_at: Vec<Option<i64>>,
    pub queried_at: i64,
}

pub(crate) async fn fetch(
    client: &reqwest::Client,
    usage_endpoint: &str,
    resets_endpoint: &str,
    credential: &ManagedCredential,
) -> Result<Quota, String> {
    let (usage, resets) = tokio::join!(
        get(client, usage_endpoint, credential, 15, false),
        get(client, resets_endpoint, credential, 8, true),
    );
    let now = chrono::Utc::now().timestamp();
    let mut quota = parse_usage(&usage?, now)?;
    // Supplemental billing data must never fail the main quota query.
    quota.reset_expires_at = resets
        .ok()
        .map(|bytes| parse_resets(&bytes, now))
        .unwrap_or_default();
    Ok(quota)
}

async fn get(
    client: &reqwest::Client,
    endpoint: &str,
    credential: &ManagedCredential,
    timeout_seconds: u64,
    resets: bool,
) -> Result<Vec<u8>, String> {
    let mut request = client
        .get(endpoint)
        .bearer_auth(credential.access_token.expose())
        .header("ChatGPT-Account-Id", &credential.workspace_id)
        .header("User-Agent", "codex-cli")
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(timeout_seconds));
    if resets {
        request = request.header("OpenAI-Beta", "codex-1");
    }
    let mut response = request
        .send()
        .await
        .map_err(|_| "额度查询连接失败，请稍后刷新")?;
    let status = response.status();
    if matches!(status.as_u16(), 401 | 403) {
        return Err("额度查询授权被拒绝，请重新登录 ChatGPT".into());
    }
    if !status.is_success() {
        return Err(format!("额度服务暂不可用（HTTP {}）", status.as_u16()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "额度响应读取失败")? {
        if bytes.len() + chunk.len() > 64 * 1024 {
            return Err("额度响应过大".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[derive(Deserialize)]
struct Usage {
    rate_limit: Option<RateLimit>,
    credits: Option<Value>,
}

#[derive(Deserialize)]
struct RateLimit {
    primary_window: Option<UsageWindow>,
    secondary_window: Option<UsageWindow>,
}

#[derive(Deserialize)]
struct UsageWindow {
    used_percent: Option<f64>,
    limit_window_seconds: Option<i64>,
    reset_at: Option<i64>,
}

fn parse_usage(bytes: &[u8], now: i64) -> Result<Quota, String> {
    let body: Usage =
        serde_json::from_slice(bytes).map_err(|_| "未能识别官方额度数据，请稍后刷新")?;
    let mut windows = Vec::new();
    if let Some(limits) = body.rate_limit {
        for window in [limits.primary_window, limits.secondary_window]
            .into_iter()
            .flatten()
        {
            let Some(used) = window.used_percent.filter(|v| v.is_finite() && *v >= 0.0) else {
                continue;
            };
            windows.push(Window {
                remaining_percent: (100.0 - used).clamp(0.0, 100.0),
                period_label: period_label(window.limit_window_seconds),
                resets_at: window
                    .reset_at
                    .filter(|v| chrono::DateTime::from_timestamp(*v, 0).is_some()),
            });
        }
    }
    let credits_balance = body.credits.as_ref().and_then(|credits| {
        if credits["has_credits"].as_bool() != Some(true)
            || credits["unlimited"].as_bool() == Some(true)
        {
            return None;
        }
        let balance = credits["balance"]
            .as_f64()
            .or_else(|| credits["balance"].as_str()?.trim().parse::<f64>().ok())?;
        (balance.is_finite() && balance > 0.0).then_some(balance)
    });
    // A missing utilization must not become a fictitious 100% remaining.
    if windows.is_empty() && credits_balance.is_none() {
        return Err("官方未提供可用额度窗口或 Credits 余额".into());
    }
    Ok(Quota {
        windows,
        credits_balance,
        reset_expires_at: Vec::new(),
        queried_at: now,
    })
}

fn period_label(seconds: Option<i64>) -> String {
    match seconds {
        Some(18_000) => "5 小时额度".into(),
        Some(604_800) => "每周额度".into(),
        Some(2_592_000) => "30 天额度".into(),
        Some(s) if s > 0 && s % 86_400 == 0 => format!("{} 天额度", s / 86_400),
        Some(s) if s > 0 && s % 3600 == 0 => format!("{} 小时额度", s / 3600),
        Some(s) if s > 0 && s % 60 == 0 => format!("{} 分钟额度", s / 60),
        Some(s) if s > 0 => format!("{s} 秒额度"),
        _ => "订阅额度".into(),
    }
}

fn parse_resets(bytes: &[u8], now: i64) -> Vec<Option<i64>> {
    let Ok(body) = serde_json::from_slice::<Value>(bytes) else {
        return Vec::new();
    };
    let Some(credits) = body["credits"].as_array() else {
        return Vec::new();
    };
    let mut expiries = credits
        .iter()
        .filter(|credit| credit["status"] == "available")
        .filter_map(|credit| match credit.get("expires_at") {
            None | Some(Value::Null) => Some(None),
            Some(Value::String(raw)) => chrono::DateTime::parse_from_rfc3339(raw)
                .ok()
                .map(|at| at.timestamp())
                .filter(|at| *at > now)
                .map(Some),
            _ => None,
        })
        .collect::<Vec<_>>();
    expiries.sort_by_key(|at| (at.is_none(), *at));
    expiries
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn usage(primary: Value, secondary: Value, credits: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({"rate_limit":{"primary_window":primary,"secondary_window":secondary},"credits":credits})).unwrap()
    }

    #[test]
    fn windows_follow_returned_duration_and_never_invent_missing_usage() {
        let primary = json!({"used_percent":25.5,"limit_window_seconds":18000,"reset_at":2000});
        for (seconds, label) in [
            (604800, "每周额度"),
            (2592000, "30 天额度"),
            (900, "15 分钟额度"),
        ] {
            let quota = parse_usage(
                &usage(
                    primary.clone(),
                    json!({"used_percent":110,"limit_window_seconds":seconds}),
                    Value::Null,
                ),
                1000,
            )
            .unwrap();
            assert_eq!(quota.windows.len(), 2);
            assert_eq!(quota.windows[0].remaining_percent, 74.5);
            assert_eq!(quota.windows[0].resets_at, Some(2000));
            assert_eq!(quota.windows[1].remaining_percent, 0.0);
            assert_eq!(quota.windows[1].period_label, label);
            assert_eq!(quota.windows[1].resets_at, None);
        }
        for window in [Value::Null, json!({}), json!({"used_percent":-1})] {
            assert!(parse_usage(&usage(window, Value::Null, Value::Null), 1000).is_err());
        }
        assert!(parse_usage(b"private upstream error", 1000).is_err());
    }

    #[test]
    fn optional_credit_fields_cannot_break_valid_windows() {
        let window = json!({"used_percent":2});
        for balance in [json!("62500"), json!(62500)] {
            let quota = parse_usage(
                &usage(
                    window.clone(),
                    Value::Null,
                    json!({"has_credits":true,"balance":balance}),
                ),
                1000,
            )
            .unwrap();
            assert_eq!(quota.credits_balance, Some(62500.0));
        }
        for credits in [
            json!({"has_credits":true,"balance":"NaN"}),
            json!({"has_credits":true,"balance":0}),
            json!({"has_credits":true,"balance":10,"unlimited":true}),
            json!("bad shape"),
        ] {
            let quota = parse_usage(&usage(window.clone(), Value::Null, credits), 1000).unwrap();
            assert_eq!(quota.credits_balance, None);
            assert_eq!(quota.windows[0].remaining_percent, 98.0);
        }
    }

    #[test]
    fn resets_count_available_unexpired_records_and_ignore_invalid_expiries() {
        let bytes = serde_json::to_vec(&json!({"available_count":99,"credits":[
            {"status":"available","expires_at":"1970-01-01T00:33:20Z"},
            {"status":"available","expires_at":null},
            {"status":"available","expires_at":"1970-01-01T00:01:00Z"},
            {"status":"used","expires_at":null},
            {"status":"available","expires_at":"invalid"}
        ]}))
        .unwrap();
        assert_eq!(parse_resets(&bytes, 1000), vec![Some(2000), None]);
        assert_eq!(parse_resets(&bytes, 2000), vec![None]);
    }

    #[tokio::test]
    async fn request_pins_account_headers_redacts_errors_and_optional_query_is_best_effort() {
        use crate::credentials::Secret;
        use axum::{
            Router,
            http::{HeaderMap, StatusCode},
            routing::get,
        };
        let router = Router::new()
            .route(
                "/usage",
                get(|headers: HeaderMap| async move {
                    assert_eq!(headers["authorization"], "Bearer synthetic-token");
                    assert_eq!(headers["chatgpt-account-id"], "synthetic-workspace");
                    assert_eq!(headers["user-agent"], "codex-cli");
                    axum::Json(json!({"rate_limit":{"primary_window":{"used_percent":30}}}))
                }),
            )
            .route(
                "/resets",
                get(|headers: HeaderMap| async move {
                    assert_eq!(headers["openai-beta"], "codex-1");
                    (StatusCode::INTERNAL_SERVER_ERROR, "private-secret")
                }),
            )
            .route(
                "/denied",
                get(|| async { (StatusCode::UNAUTHORIZED, "private-secret") }),
            )
            .route("/oversize", get(|| async { "x".repeat(65 * 1024) }))
            .route(
                "/redirect",
                get(|| async { axum::response::Redirect::temporary("/usage") }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let credential = ManagedCredential {
            access_token: Secret::new("synthetic-token".into()),
            workspace_id: "synthetic-workspace".into(),
        };
        let resets = format!("{origin}/resets");
        let quota = fetch(&client, &format!("{origin}/usage"), &resets, &credential)
            .await
            .unwrap();
        assert_eq!(quota.windows[0].remaining_percent, 70.0);
        assert!(quota.reset_expires_at.is_empty());
        for path in ["denied", "oversize", "redirect"] {
            let error = fetch(&client, &format!("{origin}/{path}"), &resets, &credential)
                .await
                .unwrap_err();
            assert!(!error.contains("private-secret"));
            assert!(!error.contains("synthetic-token"));
            assert!(!error.contains(&origin));
        }
        task.abort();
    }
}
