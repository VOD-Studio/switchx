//! A read-only Model Context Protocol server on stdio, run as `switchx mcp`, so
//! agents can inspect SwitchX status, connections, model mappings, usage, and
//! request history. Tools open the existing database read-only, without
//! creating, migrating, or writing it, and never return API keys, OAuth
//! tokens, local router tokens, or credential references.

use std::{
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Local, NaiveDate, SecondsFormat, TimeZone};
use serde_json::{Map, Value, json};

use crate::{
    app::{self, AppError, RequestView, UsageQuery},
    codex,
    storage::{Currency, ProviderKind, RequestStatus, Store, UsageFilter},
    usage::{
        TokenUsage,
        report::{Granularity, Money, Period, Totals},
    },
};

/// Newest first; an unknown requested version gets the newest.
const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
/// Request history keeps this many recent records for statistics and listing.
const REQUEST_WINDOW: usize = 100;
const DEFAULT_ROWS: u64 = 20;

const INSTRUCTIONS: &str = "SwitchX 的只读视图：连接（上游）、模型映射、Codex 配置发布状态、用量与请求记录。\
所有工具都不会修改数据，也不会返回 API Key、OAuth 令牌或本地路由令牌。\
路由进程是否正在运行、账号额度等实时状态只能在 SwitchX 应用中查看。";

/// Serves MCP on stdin and stdout until stdin closes.
pub fn serve(data_dir: Result<PathBuf, AppError>) -> io::Result<()> {
    serve_on(io::stdin().lock(), &mut io::stdout().lock(), data_dir)
}

/// Serves newline-delimited JSON-RPC messages from `input`, answering on `output`.
pub fn serve_on(
    input: impl BufRead,
    output: &mut impl Write,
    data_dir: Result<PathBuf, AppError>,
) -> io::Result<()> {
    let server = Server { data_dir };
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = server.handle_line(&line) {
            serde_json::to_writer(&mut *output, &response)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
    Ok(())
}

struct Server {
    data_dir: Result<PathBuf, AppError>,
}

impl Server {
    fn handle_line(&self, line: &str) -> Option<Value> {
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            return Some(failure(Value::Null, -32700, "Parse error"));
        };
        match message {
            // Batches were part of the 2025-03-26 revision.
            Value::Array(messages) if !messages.is_empty() => {
                let responses: Vec<_> = messages
                    .into_iter()
                    .filter_map(|message| self.handle(message))
                    .collect();
                (!responses.is_empty()).then_some(Value::Array(responses))
            }
            message => self.handle(message),
        }
    }

    fn handle(&self, message: Value) -> Option<Value> {
        let Value::Object(message) = message else {
            return Some(failure(Value::Null, -32600, "Invalid Request"));
        };
        let id = message.get("id").cloned();
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            // A response to a request this server never sends, or a malformed request.
            return id
                .filter(|_| !message.contains_key("result") && !message.contains_key("error"))
                .map(|id| failure(id, -32600, "Invalid Request"));
        };
        // Notifications, including `notifications/initialized`, need no reply.
        let id = id?;
        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Some(failure(id, -32600, "Invalid Request"));
        }
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => self.call(&params),
            _ => Err((-32601, "Method not found".to_owned())),
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => failure(id, code, &message),
        })
    }

    fn call(&self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((-32602, "Missing tool name".to_owned()))?;
        let empty = Map::new();
        let arguments = match params.get("arguments") {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(arguments)) => arguments,
            Some(_) => return Err((-32602, "Tool arguments must be an object".into())),
        };
        let outcome = match name {
            "get_status" => Ok(self.status()),
            "list_providers" => self.with_store(providers),
            "list_models" => self.with_store(|store| models(store, arguments)),
            "get_usage" => self.with_store(|store| usage(store, arguments)),
            "list_requests" => self.with_store(|store| requests(store, arguments)),
            _ => return Err((-32602, format!("Unknown tool: {name}"))),
        };
        // Tool failures go back to the model so it can correct its arguments.
        Ok(match outcome {
            Ok(value) => json!({
                "content": [{ "type": "text", "text": value.to_string() }],
                "isError": false,
            }),
            Err(message) => json!({
                "content": [{ "type": "text", "text": message }],
                "isError": true,
            }),
        })
    }

    fn with_store(
        &self,
        tool: impl FnOnce(&Store) -> Result<Value, String>,
    ) -> Result<Value, String> {
        let data_dir = self
            .data_dir
            .as_deref()
            .map_err(|error| unavailable(*error))?;
        tool(&app::open_store_read_only(data_dir).map_err(unavailable)?)
    }

    fn status(&self) -> Value {
        let mut status = Map::new();
        status.insert("version".into(), env!("CARGO_PKG_VERSION").into());
        let data_dir = match &self.data_dir {
            Ok(path) => path,
            Err(error) => {
                status.insert("database".into(), unavailable(*error).into());
                return Value::Object(status);
            }
        };
        status.insert("data_dir".into(), data_dir.display().to_string().into());
        let store = app::open_store_read_only(data_dir);
        status.insert(
            "codex_config".into(),
            codex_config(data_dir, store.as_ref().ok()),
        );
        let store = match store {
            Ok(store) => store,
            Err(error) => {
                status.insert("database".into(), unavailable(error).into());
                return Value::Object(status);
            }
        };
        let summary = (|| -> Result<_, AppError> {
            Ok((
                app::snapshot_from(&store, false)?,
                app::requests_from(&store, REQUEST_WINDOW)?,
                app::usage_from(&store, &UsageQuery::default())?,
            ))
        })();
        let (snapshot, recent, today) = match summary {
            Ok(summary) => summary,
            Err(error) => {
                status.insert("database".into(), unavailable(error).into());
                return Value::Object(status);
            }
        };
        status.insert("database".into(), "available".into());
        let kind_count = |kind| {
            snapshot
                .providers
                .iter()
                .filter(|provider| provider.kind == kind)
                .count()
        };
        status.insert(
            "connections".into(),
            json!({
                "total": snapshot.providers.len(),
                "api_key": kind_count(ProviderKind::ApiKey),
                "chatgpt": kind_count(ProviderKind::Chatgpt),
                "xai_oauth": kind_count(ProviderKind::XaiOAuth),
            }),
        );
        let saved = snapshot.models.iter().filter(|model| model.saved);
        status.insert(
            "models".into(),
            json!({
                "saved": saved.clone().count(),
                "ready": saved.clone().filter(|model| model.ready).count(),
                "enabled": saved.filter(|model| model.enabled).count(),
            }),
        );
        let mut requests = request_stats(&recent);
        if let Some(latest) = recent.first() {
            requests.insert(
                "latest".into(),
                json!({
                    "time": latest.time,
                    "model": latest.model,
                    "status": latest.status.as_str(),
                }),
            );
        }
        status.insert("recent_requests".into(), Value::Object(requests));
        status.insert("usage_today".into(), totals(today.report.totals));
        Value::Object(status)
    }
}

fn failure(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn initialize(params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let version = PROTOCOL_VERSIONS
        .into_iter()
        .find(|version| Some(*version) == requested)
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": {
            "name": "switchx",
            "title": "SwitchX",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "instructions": INSTRUCTIONS,
    })
}

fn tools() -> Value {
    let tool = |name: &str, title: &str, description: &str, properties: Value| {
        json!({
            "name": name,
            "title": title,
            "description": description,
            "inputSchema": {
                "type": "object",
                "properties": properties,
                "additionalProperties": false,
            },
            "annotations": {
                "title": title,
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false,
            },
        })
    };
    json!([
        tool(
            "get_status",
            "SwitchX 状态",
            "概览：Codex 配置当前是否由 SwitchX 托管（route 路由 / direct 直连 / none 未托管）、\
             连接与模型数量、最近请求统计和今日用量。不反映路由进程是否正在运行。",
            json!({}),
        ),
        tool(
            "list_providers",
            "连接列表",
            "列出保存的连接（上游）：ID、名称、类型、服务地址、默认模型、凭据是否已配置、账号绑定和模型映射数量。\
             不返回任何凭据内容。",
            json!({}),
        ),
        tool(
            "list_models",
            "模型映射",
            "列出已保存的模型映射：Codex 中使用的公开模型 ID、所属连接、实际上游模型、上下文窗口、推理档位、\
             是否启用以及备用连接。",
            json!({
                "provider_id": {
                    "type": "string",
                    "description": "只列出这个连接的模型，取自 list_providers 的 id。",
                },
                "enabled_only": {
                    "type": "boolean",
                    "description": "只列出已启用（会发布到 Codex）的模型。默认 false。",
                },
            }),
        ),
        tool(
            "get_usage",
            "用量统计",
            "按时间段汇总 token 用量和估算费用，包括与上一同长时段的对比、按连接和按模型的分布、未定价模型，\
             以及最近的明细行。费用只按用户填写的价格估算；订阅连接的费用是等价 API 价格。",
            json!({
                "period": {
                    "type": "string",
                    "enum": ["today", "24h", "7d", "14d", "30d", "90d", "all"],
                    "description": "预设时间段，默认 today。给出 from 时忽略。",
                },
                "from": {
                    "type": "string",
                    "description": "自定义起点：本地日期 YYYY-MM-DD（当天 00:00）或 RFC 3339 时间。",
                },
                "to": {
                    "type": "string",
                    "description": "自定义终点：本地日期 YYYY-MM-DD（包含当天）或 RFC 3339 时间（不含）。\
                                    省略时到现在为止。",
                },
                "connection": {
                    "type": "string",
                    "description": "只统计一个连接分组，取自返回结果 connections[].key，如 p:<provider_id>。",
                },
                "model": {
                    "type": "string",
                    "description": "只统计一个上游模型，取自返回结果 models[].key。",
                },
                "rows": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 100,
                    "description": "返回的最近明细行数，默认 20。",
                },
                "include_trend": {
                    "type": "boolean",
                    "description": "同时返回按小时、天、周或月分桶的趋势。默认 false。",
                },
            }),
        ),
        tool(
            "list_requests",
            "请求记录",
            "列出经过 SwitchX 路由的最近请求（最多保留 100 条）：时间、模型、连接、状态、HTTP 状态、耗时、\
             首个事件延迟、错误原因、备用切换和 token 数，并附带这些记录的统计。不包含提示词或响应内容。",
            json!({
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "description": "返回条数，默认 20。",
                },
                "status": {
                    "type": "string",
                    "enum": ["completed", "failed", "interrupted", "cancelled"],
                    "description": "只列出这个状态的请求。",
                },
            }),
        ),
    ])
}

fn unavailable(error: AppError) -> String {
    let mut message = format!("{} · {}", error.message(), error.action());
    if matches!(error, AppError::DataDirectory | AppError::Database) {
        message.push_str("如果刚刚安装或升级了 SwitchX，请先打开一次应用完成数据库初始化或升级。");
    }
    message
}

fn codex_config(data_dir: &Path, store: Option<&Store>) -> Value {
    let route = data_dir.join(codex::ROUTE_JOURNAL).exists();
    let direct = data_dir.join(codex::DIRECT_JOURNAL).exists();
    match (route, direct) {
        (false, false) => json!({ "mode": "none" }),
        (true, _) => match codex::route_switch::recovery(data_dir) {
            Ok(Some(recovery)) => json!({
                "mode": "route",
                "config_path": recovery.config_path.display().to_string(),
            }),
            _ => json!({ "mode": "route", "journal": "unreadable" }),
        },
        (false, true) => {
            let mut config = Map::new();
            config.insert("mode".into(), "direct".into());
            match (
                codex::direct_switch::active_target(data_dir),
                codex::direct_switch::active_provider(data_dir),
            ) {
                (Ok(target), Ok(provider)) => {
                    if let Some(target) = target {
                        config.insert("config_path".into(), target.display().to_string().into());
                    }
                    if let Some(provider) = provider {
                        let name = store
                            .and_then(|store| store.providers().ok())
                            .and_then(|providers| {
                                providers.into_iter().find(|record| record.id == provider)
                            })
                            .map(|record| record.name);
                        config.insert("provider_id".into(), provider.into());
                        if let Some(name) = name {
                            config.insert("provider_name".into(), name.into());
                        }
                    }
                }
                _ => {
                    config.insert("journal".into(), "unreadable".into());
                }
            }
            Value::Object(config)
        }
    }
}

fn providers(store: &Store) -> Result<Value, String> {
    let snapshot = app::snapshot_from(store, true).map_err(unavailable)?;
    let providers: Vec<_> = snapshot
        .providers
        .iter()
        .map(|provider| {
            let mappings = snapshot
                .models
                .iter()
                .filter(|model| model.saved && model.provider_id == provider.id);
            let mut view = json!({
                "id": provider.id,
                "name": provider.name,
                "kind": provider.kind.as_str(),
                "endpoint": provider.endpoint,
                "default_model": provider.model_id,
                "credential_status": provider.credential_status,
                "models": mappings.clone().count(),
                "enabled_models": mappings.filter(|model| model.enabled).count(),
            });
            let fields = [
                ("base_url", &provider.base_url),
                ("account_binding", &provider.binding_label),
            ];
            for (key, value) in fields {
                if !value.is_empty() {
                    view[key] = value.as_str().into();
                }
            }
            if !provider.preset_id.is_empty() {
                view["preset"] = provider.preset_id.into();
            }
            view
        })
        .collect();
    Ok(json!({ "providers": providers }))
}

fn models(store: &Store, arguments: &Map<String, Value>) -> Result<Value, String> {
    let provider = optional_string(arguments, "provider_id")?;
    let enabled_only = optional_bool(arguments, "enabled_only")?.unwrap_or(false);
    let snapshot = app::snapshot_from(store, false).map_err(unavailable)?;
    if let Some(provider) = provider
        && !snapshot
            .providers
            .iter()
            .any(|record| record.id == provider)
    {
        return Err(format!(
            "没有 ID 为 {provider} 的连接；请先调用 list_providers。"
        ));
    }
    let models: Vec<_> = snapshot
        .models
        .iter()
        .filter(|model| model.saved)
        .filter(|model| provider.is_none_or(|provider| model.provider_id == provider))
        .filter(|model| !enabled_only || model.enabled)
        .map(|model| {
            let mut view = json!({
                "public_id": model.public_id,
                "display_name": model.display_name,
                "provider_id": model.provider_id,
                "provider_name": model.provider_name,
                "upstream_model": model.upstream_model,
                "ready": model.ready,
                "enabled": model.enabled,
            });
            if !model.ready {
                view["problem"] = model.detail.as_str().into();
            }
            if let Ok(window) = model.context_window.parse::<i64>() {
                view["context_window"] = window.into();
            }
            if !model.reasoning_levels.is_empty() {
                view["reasoning_levels"] = model.reasoning_levels.split(", ").collect();
            }
            if !model.default_reasoning.is_empty() {
                view["default_reasoning"] = model.default_reasoning.as_str().into();
            }
            if !model.fallback_provider_id.is_empty() {
                view["fallback_provider_id"] = model.fallback_provider_id.as_str().into();
            }
            if !model.binding_label.is_empty() {
                view["account_binding"] = model.binding_label.as_str().into();
            }
            view
        })
        .collect();
    Ok(json!({ "models": models }))
}

fn usage(store: &Store, arguments: &Map<String, Value>) -> Result<Value, String> {
    let now = Local::now();
    let period = match optional_string(arguments, "from")? {
        Some(from) => {
            let from_ms = instant(from, false)?;
            let to_ms = optional_string(arguments, "to")?
                .map(|to| instant(to, true))
                .transpose()?;
            if to_ms.is_some_and(|to_ms| to_ms <= from_ms) {
                return Err("to 必须晚于 from。".into());
            }
            if from_ms > now.timestamp_millis() {
                return Err("from 不能晚于现在。".into());
            }
            Period::Custom { from_ms, to_ms }
        }
        None if arguments.contains_key("to") => return Err("给出 to 时也必须给出 from。".into()),
        None => match optional_string(arguments, "period")?.unwrap_or("today") {
            "today" => Period::Today,
            "24h" => Period::Hours24,
            "7d" => Period::Days7,
            "14d" => Period::Days14,
            "30d" => Period::Days30,
            "90d" => Period::Days90,
            "all" => Period::All,
            other => return Err(format!("未知的 period：{other}")),
        },
    };
    let rows = optional_count(arguments, "rows", 0, 100)?.unwrap_or(DEFAULT_ROWS) as usize;
    let include_trend = optional_bool(arguments, "include_trend")?.unwrap_or(false);
    let query = UsageQuery {
        period,
        filter: UsageFilter {
            group: optional_string(arguments, "connection")?.map(str::to_owned),
            model: optional_string(arguments, "model")?.map(str::to_owned),
        },
        rows,
    };
    let view = app::usage_from(store, &query).map_err(unavailable)?;
    let report = &view.report;
    let window = &report.window;
    let share = |share: &crate::usage::report::Share| {
        let mut value = json!({
            "key": share.key,
            "name": share.name,
            "kind": share.kind.label(),
            "totals": totals(share.totals),
        });
        if let Some(provider) = &share.provider_id {
            value["provider_id"] = provider.as_str().into();
        }
        value
    };
    let mut result = json!({
        "period": {
            "from": timestamp(window.from_ms),
            "to": timestamp(window.to_ms),
        },
        "totals": totals(report.totals),
        "connections": report.connections.iter().map(share).collect::<Vec<_>>(),
        "models": report.models.iter().map(share).collect::<Vec<_>>(),
        "detail_rows": view.total_rows,
        "rows": view.rows.iter().take(rows).map(|row| {
            let mut value = json!({
                "time": timestamp(row.started_at_ms),
                "connection": row.connection,
                "connection_key": row.group,
                "model": row.model,
                "tokens": tokens(row.usage),
                "routed": row.routed,
            });
            if let Some(public) = &row.public_model {
                value["public_model"] = public.as_str().into();
            }
            if let Some((currency, micros)) = row.cost {
                value["cost"] = amount(currency, micros);
            }
            if let Some(duration) = row.duration_ms {
                value["duration_ms"] = duration.into();
            }
            if let Some(speed) = row.speed {
                value["output_tokens_per_second"] = round(speed, 1).into();
            }
            value
        }).collect::<Vec<_>>(),
        "session_log_import": view.log_import,
    });
    if let Some(previous) = report.previous {
        result["previous_period_totals"] = totals(previous);
    }
    if !report.unpriced.is_empty() {
        result["unpriced_models"] = report
            .unpriced
            .iter()
            .map(|(model, requests)| json!({ "model": model, "requests": requests }))
            .collect();
    }
    if let Some(speed) = report.speed_median {
        result["median_output_tokens_per_second"] = round(speed, 1).into();
    }
    if include_trend {
        result["trend_granularity"] = match window.granularity {
            Granularity::Hour => "hour",
            Granularity::Day => "day",
            Granularity::Week => "week",
            Granularity::Month => "month",
        }
        .into();
        result["trend"] = report
            .points
            .iter()
            .map(|point| {
                json!({
                    "start": timestamp(point.bucket.start_ms),
                    "label": point.bucket.label,
                    "totals": totals(point.totals),
                })
            })
            .collect();
    }
    Ok(result)
}

fn requests(store: &Store, arguments: &Map<String, Value>) -> Result<Value, String> {
    let limit = optional_count(arguments, "limit", 1, 100)?.unwrap_or(DEFAULT_ROWS) as usize;
    let status = optional_string(arguments, "status")?
        .map(|status| {
            [
                RequestStatus::Completed,
                RequestStatus::Failed,
                RequestStatus::Interrupted,
                RequestStatus::Cancelled,
            ]
            .into_iter()
            .find(|candidate| candidate.as_str() == status)
            .ok_or_else(|| format!("未知的 status：{status}"))
        })
        .transpose()?;
    let recent = app::requests_from(store, REQUEST_WINDOW).map_err(unavailable)?;
    let stats = request_stats(&recent);
    let requests: Vec<_> = recent
        .into_iter()
        .filter(|request| status.is_none_or(|status| request.status == status))
        .take(limit)
        .map(|request| {
            let mut value = json!({
                "id": request.id,
                "time": request.time,
                "model": request.model,
                "provider": request.provider,
                "upstream_model": request.upstream,
                "status": request.status.as_str(),
                "status_label": request.status.label(),
                "duration_ms": request.duration_ms,
            });
            let optional = [
                ("http_status", request.http_status.map(i64::from)),
                ("headers_ms", request.headers_ms),
                ("first_event_ms", request.first_event_ms),
            ];
            for (key, field) in optional {
                if let Some(field) = field {
                    value[key] = field.into();
                }
            }
            if !request.error.is_empty() {
                value["error"] = request.error.into();
            }
            if !request.fallback.is_empty() {
                value["fallback"] = request.fallback.into();
            }
            if let Some(usage) = request.usage {
                value["tokens"] = tokens(usage);
            }
            value
        })
        .collect();
    Ok(json!({ "stats": stats, "requests": requests }))
}

fn request_stats(requests: &[RequestView]) -> Map<String, Value> {
    let stats = app::request_stats(requests);
    let mut value = Map::new();
    value.insert("total".into(), stats.total.into());
    value.insert("completed".into(), stats.completed.into());
    value.insert("failed".into(), stats.failed.into());
    value.insert("cancelled".into(), stats.cancelled.into());
    let latencies = [
        ("duration_p50_ms", stats.p50_ms),
        ("duration_p95_ms", stats.p95_ms),
        ("first_event_p50_ms", stats.first_event_p50_ms),
    ];
    for (key, latency) in latencies {
        if let Some(latency) = latency {
            value.insert(key.into(), latency.into());
        }
    }
    value
}

fn totals(totals: Totals) -> Value {
    let mut cost = json!({
        "api": money(totals.cost.api),
        "subscription_equivalent": money(totals.cost.subscription),
    });
    if totals.cost.unpriced_requests > 0 {
        cost["unpriced_requests"] = totals.cost.unpriced_requests.into();
    }
    json!({
        "requests": totals.requests,
        "tokens": tokens(totals.usage),
        "cost": cost,
    })
}

fn tokens(usage: TokenUsage) -> Value {
    json!({
        "input": usage.input,
        "cached_input": usage.cached_input,
        "cache_write": usage.cache_write,
        "output": usage.output,
        "reasoning": usage.reasoning,
        "total": usage.total(),
    })
}

/// Amounts per currency in whole units; empty when nothing was priced.
fn money(money: Money) -> Value {
    let mut amounts = Map::new();
    for currency in [Currency::Usd, Currency::Cny] {
        let micros = money.get(currency);
        if micros > 0 {
            amounts.insert(currency.as_str().into(), units(micros).into());
        }
    }
    Value::Object(amounts)
}

fn amount(currency: Currency, micros: u128) -> Value {
    json!({ currency.as_str(): units(micros) })
}

fn units(micros: u128) -> f64 {
    round(micros as f64 / 1_000_000.0, 6)
}

fn round(value: f64, digits: i32) -> f64 {
    let scale = 10_f64.powi(digits);
    (value * scale).round() / scale
}

fn timestamp(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Secs, false))
        .unwrap_or_default()
}

/// A local date (its midnight, or the next one for an inclusive end) or an RFC 3339 instant.
fn instant(text: &str, end: bool) -> Result<i64, String> {
    if let Ok(time) = DateTime::parse_from_rfc3339(text) {
        return Ok(time.timestamp_millis());
    }
    let date = NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .map_err(|_| format!("无法识别的时间 {text}；请使用 YYYY-MM-DD 或 RFC 3339。"))?;
    let date = if end {
        date.succ_opt().ok_or("日期超出范围。")?
    } else {
        date
    };
    let midnight = date.and_hms_opt(0, 0, 0).unwrap_or_default();
    Local
        .from_local_datetime(&midnight)
        .earliest()
        .or_else(|| {
            Local
                .from_local_datetime(&(midnight + chrono::Duration::hours(1)))
                .earliest()
        })
        .map(|time| time.timestamp_millis())
        .ok_or_else(|| format!("本地时区中没有 {text} 的零点。"))
}

fn optional_string<'a>(
    arguments: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a str>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if !text.trim().is_empty() => Ok(Some(text.trim())),
        Some(_) => Err(format!("{key} 必须是非空字符串。")),
    }
}

fn optional_bool(arguments: &Map<String, Value>, key: &str) -> Result<Option<bool>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(format!("{key} 必须是布尔值。")),
    }
}

fn optional_count(
    arguments: &Map<String, Value>,
    key: &str,
    min: u64,
    max: u64,
) -> Result<Option<u64>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|count| (min..=max).contains(count))
            .map(Some)
            .ok_or_else(|| format!("{key} 必须是 {min} 到 {max} 之间的整数。")),
    }
}

#[cfg(test)]
mod tests {
    use std::{env, fs};

    use super::*;
    use crate::{
        credentials::Secret,
        storage::{ModelRecord, ProviderRecord, RequestRecord},
    };

    const API_KEY: &str = "sk-mcp-probe-api-key-never-shown";
    const LOCAL_TOKEN: &str = "5ec7e75ec7e75ec7e75ec7e75ec7e75ec7e75ec7e75ec7e75ec7e75ec7e75ec7";
    const CREDENTIAL_REF: &str = "credential-reference-never-shown";

    struct Data(PathBuf);

    impl Data {
        fn new() -> Self {
            let mut nonce = [0_u8; 8];
            getrandom::fill(&mut nonce).unwrap();
            let suffix: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
            Self(env::temp_dir().join(format!("switchx-mcp-{suffix}")))
        }
    }

    impl Drop for Data {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn exchange(data_dir: &Path, messages: &[Value]) -> Vec<Value> {
        let input: String = messages
            .iter()
            .map(|message| format!("{message}\n"))
            .collect();
        let mut output = Vec::new();
        serve_on(input.as_bytes(), &mut output, Ok(data_dir.to_owned())).unwrap();
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn call(id: i64, name: &str, arguments: Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        })
    }

    fn tool_result(response: &Value) -> Value {
        assert_eq!(response["result"]["isError"], false, "{response}");
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    fn seed(data_dir: &Path) {
        let store = app::open_store(data_dir).unwrap();
        let catalog: Value =
            serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json")).unwrap();
        let metadata = catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["slug"] == "deepseek-flash")
            .unwrap()
            .to_string();
        let provider = ProviderRecord {
            kind: ProviderKind::ApiKey,
            account_binding: None,
            icon_id: None,
            id: "probe".into(),
            name: "Probe Upstream".into(),
            base_url: "https://api.example.invalid/v1".into(),
            model_id: "deepseek-flash".into(),
            credential_ref: Some(CREDENTIAL_REF.into()),
        };
        let model = ModelRecord {
            request_overrides: Default::default(),
            provider_id: "probe".into(),
            public_id: "sx-flash".into(),
            display_name: "Probe Flash".into(),
            upstream_model: "deepseek-flash".into(),
            metadata,
            enabled: true,
            fallback_provider_id: None,
        };
        store
            .put_api_provider(
                &provider,
                &[model],
                None,
                Some(&Secret::new(API_KEY.into())),
                None,
            )
            .unwrap();
        store
            .put_local_token(
                "router-0123456789abcdef0123456789abcdef",
                &Secret::new(LOCAL_TOKEN.into()),
            )
            .unwrap();
        let now = Local::now().timestamp_millis();
        let request = |id: &str, status, usage| RequestRecord {
            id: id.into(),
            started_at_ms: now - 1_000,
            public_model: Some("sx-flash".into()),
            provider_id: Some("probe".into()),
            upstream_model: Some("deepseek-flash".into()),
            generation: "generation".into(),
            http_status: Some(200),
            headers_ms: Some(40),
            first_event_ms: Some(80),
            duration_ms: 900,
            status,
            error_code: None,
            fallback_from: None,
            usage,
            response_id: None,
        };
        store
            .put_request(&request(
                "request-ok",
                RequestStatus::Completed,
                Some(TokenUsage {
                    input: 1_000_000,
                    output: 500_000,
                    ..TokenUsage::default()
                }),
            ))
            .unwrap();
        store
            .put_request(&RequestRecord {
                error_code: Some("upstream_timeout".into()),
                ..request("request-failed", RequestStatus::Failed, None)
            })
            .unwrap();
        drop(store);
        app::save_price(
            data_dir,
            &app::PriceInput {
                provider_id: "probe".into(),
                model: "deepseek-flash".into(),
                currency: "USD".into(),
                input: "1".into(),
                cached_input: "0.1".into(),
                output: "2".into(),
            },
        )
        .unwrap();
    }

    #[test]
    fn protocol_negotiates_lists_read_only_tools_and_rejects_unknown_calls() {
        let data = Data::new();
        let responses = exchange(
            &data.0,
            &[
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": { "protocolVersion": "2025-06-18", "capabilities": {} },
                }),
                json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
                json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
                json!({ "jsonrpc": "2.0", "id": 3, "method": "ping" }),
                json!({ "jsonrpc": "2.0", "id": 4, "method": "resources/list" }),
                call(5, "delete_everything", json!({})),
                json!({ "jsonrpc": "2.0", "id": 6, "result": {} }),
                json!({
                    "jsonrpc": "2.0",
                    "id": 7,
                    "method": "initialize",
                    "params": { "protocolVersion": "1999-01-01" },
                }),
            ],
        );
        // The notification and the stray response get no reply.
        assert_eq!(
            responses
                .iter()
                .map(|r| r["id"].clone())
                .collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 7]
        );
        assert_eq!(responses[0]["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(responses[0]["result"]["serverInfo"]["name"], "switchx");
        let tools = responses[1]["result"]["tools"].as_array().unwrap();
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "get_status",
                "list_providers",
                "list_models",
                "get_usage",
                "list_requests"
            ]
        );
        assert!(tools.iter().all(|tool| {
            tool["annotations"]["readOnlyHint"] == true && tool["inputSchema"]["type"] == "object"
        }));
        assert_eq!(responses[2]["result"], json!({}));
        assert_eq!(responses[3]["error"]["code"], -32601);
        assert_eq!(responses[4]["error"]["code"], -32602);
        assert_eq!(
            responses[5]["result"]["protocolVersion"],
            PROTOCOL_VERSIONS[0]
        );

        let mut output = Vec::new();
        serve_on("not json\n[]\n".as_bytes(), &mut output, Ok(data.0.clone())).unwrap();
        let errors: Vec<Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(errors[0]["error"]["code"], -32700);
        assert_eq!(errors[1]["error"]["code"], -32600);
    }

    #[test]
    fn tools_report_saved_data_without_credentials() {
        let data = Data::new();
        seed(&data.0);
        let responses = exchange(
            &data.0,
            &[
                call(1, "get_status", json!({})),
                call(2, "list_providers", json!({})),
                call(3, "list_models", json!({ "enabled_only": true })),
                call(
                    4,
                    "get_usage",
                    json!({ "period": "today", "include_trend": true }),
                ),
                call(5, "list_requests", json!({ "status": "failed" })),
                call(6, "list_models", json!({ "provider_id": "missing" })),
                call(7, "get_usage", json!({ "rows": 500 })),
                call(8, "get_usage", json!({ "from": "2026-13-01" })),
            ],
        );
        let everything = responses.iter().map(Value::to_string).collect::<String>();
        for secret in [
            API_KEY,
            LOCAL_TOKEN,
            CREDENTIAL_REF,
            "router-0123456789abcdef",
        ] {
            assert!(!everything.contains(secret), "{secret} leaked");
        }

        let status = tool_result(&responses[0]);
        assert_eq!(status["database"], "available");
        assert_eq!(status["codex_config"]["mode"], "none");
        assert_eq!(status["connections"]["api_key"], 1);
        assert_eq!(status["models"]["enabled"], 1);
        assert_eq!(status["recent_requests"]["total"], 2);
        assert_eq!(status["usage_today"]["tokens"]["total"], 1_500_000);

        let provider = &tool_result(&responses[1])["providers"][0];
        assert_eq!(provider["id"], "probe");
        assert_eq!(provider["endpoint"], "https://api.example.invalid");
        assert_eq!(provider["credential_status"], "凭据可读取");
        assert_eq!(provider["enabled_models"], 1);

        let model = &tool_result(&responses[2])["models"][0];
        assert_eq!(model["public_id"], "sx-flash");
        assert_eq!(model["upstream_model"], "deepseek-flash");
        assert_eq!(model["ready"], true);

        let usage = tool_result(&responses[3]);
        assert_eq!(usage["totals"]["requests"], 1);
        assert_eq!(usage["totals"]["cost"]["api"]["USD"], 2.0);
        assert_eq!(usage["connections"][0]["key"], "p:probe");
        assert_eq!(usage["rows"][0]["model"], "deepseek-flash");
        assert_eq!(usage["trend_granularity"], "hour");

        let requests = tool_result(&responses[4]);
        assert_eq!(requests["stats"]["total"], 2);
        assert_eq!(requests["requests"].as_array().unwrap().len(), 1);
        assert_eq!(requests["requests"][0]["id"], "request-failed");
        assert!(
            requests["requests"][0]["error"]
                .as_str()
                .unwrap()
                .ends_with("upstream_timeout")
        );

        for response in &responses[5..] {
            assert_eq!(response["result"]["isError"], true, "{response}");
        }
    }

    #[test]
    fn tools_never_create_or_migrate_the_database() {
        let data = Data::new();
        let responses = exchange(
            &data.0,
            &[
                call(1, "get_status", json!({})),
                call(2, "list_providers", json!({})),
            ],
        );
        assert!(!data.0.exists());
        assert!(
            tool_result(&responses[0])["database"]
                .as_str()
                .unwrap()
                .contains("数据目录")
        );
        assert_eq!(responses[1]["result"]["isError"], true);

        fs::create_dir_all(&data.0).unwrap();
        let database = data.0.join("switchx.sqlite");
        rusqlite::Connection::open(&database)
            .unwrap()
            .execute_batch("PRAGMA user_version = 8")
            .unwrap();
        let before = fs::read(&database).unwrap();
        let responses = exchange(&data.0, &[call(1, "list_models", json!({}))]);
        assert_eq!(responses[0]["result"]["isError"], true);
        assert!(
            responses[0]["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("打开一次应用")
        );
        assert_eq!(fs::read(&database).unwrap(), before);
    }
}
