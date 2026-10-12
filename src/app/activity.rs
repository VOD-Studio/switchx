//! Request history for the activity page.

use std::path::Path;

use super::{AppError, open_store};
use crate::storage::RequestStatus;

pub struct RequestView {
    pub id: String,
    pub time: String,
    pub model: String,
    pub provider: String,
    pub upstream: String,
    pub duration_ms: i64,
    pub headers_ms: Option<i64>,
    pub first_event_ms: Option<i64>,
    pub http_status: Option<u16>,
    pub generation: String,
    pub status: RequestStatus,
    pub error: String,
    pub fallback: String,
}

/// Aggregates for the activity header. Latency percentiles cover completed
/// requests only, so instant local rejections do not flatter the numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RequestStats {
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub p50_ms: Option<i64>,
    pub p95_ms: Option<i64>,
    pub first_event_p50_ms: Option<i64>,
}

pub fn request_stats(requests: &[RequestView]) -> RequestStats {
    let count = |status| {
        requests
            .iter()
            .filter(|request| request.status == status)
            .count()
    };
    let completed = requests
        .iter()
        .filter(|request| request.status == RequestStatus::Completed);
    let mut durations = completed
        .clone()
        .map(|request| request.duration_ms)
        .collect::<Vec<_>>();
    let mut first_events = completed
        .filter_map(|request| request.first_event_ms)
        .collect::<Vec<_>>();
    // Nearest-rank percentile over the sorted sample.
    let percentile = |values: &mut Vec<i64>, rank: usize| {
        values.sort_unstable();
        (!values.is_empty()).then(|| values[(values.len() * rank).div_ceil(100).max(1) - 1])
    };
    RequestStats {
        total: requests.len(),
        completed: count(RequestStatus::Completed),
        failed: count(RequestStatus::Failed) + count(RequestStatus::Interrupted),
        cancelled: count(RequestStatus::Cancelled),
        p50_ms: percentile(&mut durations, 50),
        p95_ms: percentile(&mut durations, 95),
        first_event_p50_ms: percentile(&mut first_events, 50),
    }
}

pub fn format_duration(ms: i64) -> String {
    if ms >= 1_000 {
        format!("{:.1} s", ms as f64 / 1_000.0)
    } else {
        format!("{ms} ms")
    }
}

pub fn load_requests(data_dir: &Path) -> Result<Vec<RequestView>, AppError> {
    let store = open_store(data_dir)?;
    let providers = store.providers().map_err(|_| AppError::Database)?;
    let provider_name = |id: &String| {
        providers
            .iter()
            .find(|provider| &provider.id == id)
            .map(|provider| provider.name.clone())
            .unwrap_or_else(|| id.clone())
    };
    store
        .requests(100)
        .map_err(|_| AppError::Database)?
        .into_iter()
        .map(|record| {
            let provider = record
                .provider_id
                .as_ref()
                .map(provider_name)
                .unwrap_or_else(|| "未选择上游".into());
            Ok(RequestView {
                time: store
                    .request_time(record.started_at_ms)
                    .map_err(|_| AppError::Database)?,
                model: record.public_model.unwrap_or_else(|| "未指定模型".into()),
                upstream: record.upstream_model.unwrap_or_else(|| "—".into()),
                duration_ms: record.duration_ms,
                headers_ms: record.headers_ms,
                first_event_ms: record.first_event_ms,
                http_status: record.http_status,
                status: record.status,
                error: record
                    .error_code
                    .map(|code| format!("{} · {code}", request_error_message(&code)))
                    .unwrap_or_default(),
                fallback: record
                    .fallback_from
                    .as_ref()
                    .map(|id| {
                        format!(
                            "{} 建立连接失败（请求未发送）→ 尝试备用 {provider}",
                            provider_name(id)
                        )
                    })
                    .unwrap_or_default(),
                provider,
                id: record.id,
                generation: record.generation,
            })
        })
        .collect()
}

fn request_error_message(code: &str) -> &'static str {
    match code {
        "client_disconnected" => "完成前客户端断开；可能是用户取消或连接丢失",
        "router_stopping" => "路由停止，未收到完成信号",
        "missing_completion" => "响应结束，但未收到正常完成信号",
        "upstream_timeout" => "上游请求超时",
        "upstream_unavailable" => "无法连接上游",
        "no_eligible_upstream" => "主备上游均无法建立连接",
        "upstream_read_error" => "读取上游时连接中断",
        "upstream_http_error" => "上游返回非成功 HTTP 状态",
        "upstream_response_failed" | "upstream_stream_error" => "上游报告请求失败",
        "upstream_response_incomplete" => "上游报告响应未完成",
        "upstream_response_cancelled" => "上游报告响应已取消",
        "invalid_upstream_event" | "invalid_upstream_response" => "上游响应格式无效",
        "upstream_event_too_large" | "upstream_body_too_large" => "上游响应超过读取上限",
        "unknown_model" => "模型未发布",
        "chatgpt_unauthorized" => "官方认证被拒绝；Codex 会尝试续期，仍失败时请恢复并重新登录",
        "chatgpt_forbidden" => "官方账号没有此模型或工作区权限，请检查订阅与模型选择",
        "chatgpt_account_changed" => "工作区已变化，请恢复并重新发布路由",
        "xai_auth_required" | "xai_unauthorized" => "Grok 账号凭据不可用，请重新登录并发布路由",
        "xai_forbidden" => "Grok 账号没有此模型权限或额度，请检查账号",
        "unsupported_xai_tool" | "unsupported_xai_tool_schema" => {
            "Grok 不支持此工具形态，请使用函数工具模型资料"
        }
        "xai_tool_name_collision" => "Grok 工具名称展平后冲突，请修改工具名称",
        "invalid_xai_response" | "xai_response_too_large" => "Grok 响应格式或大小无效",
        "chatgpt_auth_required" | "chatgpt_account_required" => {
            "缺少订阅认证或工作区，请在目标 Codex 中完成 ChatGPT 登录"
        }
        "unsupported_capability" => "请求包含暂不支持的会话状态",
        _ => "本地请求校验或转发失败",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_stats_use_completed_latency_and_nearest_rank_percentiles() {
        let request = |status, duration_ms, first_event_ms| RequestView {
            id: String::new(),
            time: String::new(),
            model: String::new(),
            provider: String::new(),
            upstream: String::new(),
            duration_ms,
            headers_ms: None,
            first_event_ms,
            http_status: None,
            generation: String::new(),
            status,
            error: String::new(),
            fallback: String::new(),
        };
        assert_eq!(request_stats(&[]), RequestStats::default());
        let mut requests = (1..=20)
            .map(|second| request(RequestStatus::Completed, second * 1_000, Some(second * 10)))
            .collect::<Vec<_>>();
        requests.push(request(RequestStatus::Failed, 0, None));
        requests.push(request(RequestStatus::Interrupted, 90_000, Some(5)));
        requests.push(request(RequestStatus::Cancelled, 1, None));
        let stats = request_stats(&requests);
        assert_eq!(
            (stats.total, stats.completed, stats.failed, stats.cancelled),
            (23, 20, 2, 1)
        );
        assert_eq!(stats.p50_ms, Some(10_000));
        assert_eq!(stats.p95_ms, Some(19_000));
        assert_eq!(stats.first_event_p50_ms, Some(100));
        assert_eq!(format_duration(999), "999 ms");
        assert_eq!(format_duration(1_550), "1.6 s");
    }
}
