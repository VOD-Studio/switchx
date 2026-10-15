//! Agent control of the running app: a loopback listener for
//! `switchx mcp --allow-control`, and the worker requests it queues. Applying
//! needs the ID of a preview made through this channel, which any window
//! command invalidates, so an agent never applies a preview it did not review.

use std::{
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};

use serde_json::{Map, Value, json};
use slint::ComponentHandle;
use switchx::{
    codex::{self, client},
    control::{self, MESSAGE_LIMIT, Request, Response},
    credentials::Secret,
    route_session::RoutePreview,
    ui::AppWindow,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::TcpListener,
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

use super::{codex_config::DirectPreview, command::Command, worker::Worker};

/// A client has this long to send its request after connecting.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// One agent request, with the window's current choices filled in.
pub(super) enum ControlRequest {
    Status {
        home: String,
    },
    PreviewRoute {
        home: String,
        port: String,
        model: String,
    },
    ApplyRoute {
        preview: String,
    },
    PreviewDirect {
        home: String,
        provider: String,
    },
    ApplyDirect {
        preview: String,
    },
    CancelPreview,
    Restore {
        home: String,
    },
}

impl ControlRequest {
    pub(super) fn codex_home(&self) -> Option<&str> {
        match self {
            Self::Status { home }
            | Self::PreviewRoute { home, .. }
            | Self::PreviewDirect { home, .. }
            | Self::Restore { home } => Some(home),
            _ => None,
        }
    }

    fn allowed_during_recovery(&self) -> bool {
        matches!(self, Self::Status { .. } | Self::Restore { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PreviewTarget {
    Route {
        home: String,
        port: String,
        model: String,
    },
    Direct {
        home: String,
    },
}

/// The one preview an agent may apply, until anything else changes the window's state.
#[derive(Default)]
pub(super) struct ControlPreviews(Option<(String, PreviewTarget)>);

impl ControlPreviews {
    fn issue(&mut self, target: PreviewTarget) -> Result<String, String> {
        let mut bytes = [0_u8; 12];
        getrandom::fill(&mut bytes).map_err(|_| "无法生成预览 ID")?;
        let id: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        self.0 = Some((id.clone(), target));
        Ok(id)
    }

    /// Consumes the preview when `id` names it; a mismatch leaves no preview to retry with.
    fn take(&mut self, id: &str) -> Result<PreviewTarget, String> {
        match self.0.take() {
            Some((current, target)) if control::verify(&current, id) => Ok(target),
            _ => Err("预览已失效或不存在；应用中的其他操作也会使预览失效，请重新预览。".into()),
        }
    }

    pub(super) fn clear(&mut self) {
        self.0 = None;
    }

    fn pending(&self) -> Option<&'static str> {
        self.0.as_ref().map(|(_, target)| match target {
            PreviewTarget::Route { .. } => "route",
            PreviewTarget::Direct { .. } => "direct",
        })
    }
}

/// The listener and its published endpoint, removed when the app exits.
pub(super) struct ControlServer {
    data_dir: PathBuf,
    token: Secret,
    task: JoinHandle<()>,
}

impl ControlServer {
    pub(super) fn stop(self) {
        self.task.abort();
        control::remove_endpoint(&self.data_dir, &self.token);
    }
}

/// Starts listening on loopback; agents find the port and token in the data directory.
pub(super) fn start(
    runtime: &tokio::runtime::Runtime,
    app: &AppWindow,
    sender: &mpsc::Sender<Command>,
    data_dir: PathBuf,
) -> Result<ControlServer, String> {
    let listener = runtime
        .block_on(TcpListener::bind(SocketAddr::from((
            Ipv4Addr::LOCALHOST,
            0,
        ))))
        .map_err(|error| error.to_string())?;
    let port = listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    let token = control::publish_endpoint(&data_dir, port).map_err(|error| error.to_string())?;
    let expected = Secret::new(token.expose().to_owned());
    let weak = app.as_weak();
    let sender = sender.clone();
    let task = runtime.spawn(async move {
        let expected = std::sync::Arc::new(expected);
        while let Ok((stream, _)) = listener.accept().await {
            let expected = expected.clone();
            let weak = weak.clone();
            let sender = sender.clone();
            tokio::spawn(async move {
                serve(stream, expected.expose(), |method, params| {
                    dispatch(weak, sender, method, params)
                })
                .await;
            });
        }
    });
    Ok(ControlServer {
        data_dir,
        token,
        task,
    })
}

/// Reads one request, checks its token, and writes the dispatched result.
async fn serve<S, F, Fut>(stream: S, expected: &str, dispatch: F)
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce(String, Value) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let mut line = Vec::new();
    let read = tokio::time::timeout(
        REQUEST_TIMEOUT,
        BufReader::new(reader)
            .take(MESSAGE_LIMIT as u64 + 1)
            .read_until(b'\n', &mut line),
    )
    .await;
    let response = match read {
        Ok(Ok(_)) if line.len() <= MESSAGE_LIMIT && line.ends_with(b"\n") => {
            match serde_json::from_slice::<Request>(&line) {
                Ok(request) if control::verify(expected, &request.token) => {
                    dispatch(request.method, request.params).await.into()
                }
                Ok(_) => Response::Error {
                    message: "控制令牌无效；请重新启动 MCP 服务后重试。".into(),
                },
                Err(_) => Response::Error {
                    message: "控制请求格式无效".into(),
                },
            }
        }
        _ => Response::Error {
            message: "控制请求不完整或过大".into(),
        },
    };
    zeroize::Zeroize::zeroize(&mut line);
    if let Ok(mut bytes) = serde_json::to_vec(&response) {
        bytes.push(b'\n');
        let _ = writer.write_all(&bytes).await;
        let _ = writer.shutdown().await;
    }
}

/// The window's current Codex home, route port, and default model.
struct WindowChoices {
    home: String,
    port: String,
    model: String,
}

async fn window_choices(weak: &slint::Weak<AppWindow>) -> Result<WindowChoices, String> {
    let (sender, receiver) = oneshot::channel();
    weak.upgrade_in_event_loop(move |app| {
        let _ = sender.send(WindowChoices {
            home: app.get_config_home().into(),
            port: app.get_route_port().into(),
            model: app.get_default_model().into(),
        });
    })
    .map_err(|_| "SwitchX 窗口已关闭")?;
    receiver.await.map_err(|_| "SwitchX 窗口已关闭".into())
}

async fn dispatch(
    weak: slint::Weak<AppWindow>,
    sender: mpsc::Sender<Command>,
    method: String,
    params: Value,
) -> Result<Value, String> {
    let empty = Map::new();
    let params = match &params {
        Value::Null => &empty,
        Value::Object(params) => params,
        _ => return Err("控制参数必须是对象".into()),
    };
    let text = |key: &str| match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if !text.trim().is_empty() => Ok(Some(text.trim().to_owned())),
        Some(Value::Number(number)) if key == "port" => Ok(Some(number.to_string())),
        Some(_) => Err(format!("{key} 参数无效")),
    };
    let required = |key: &str| text(key)?.ok_or_else(|| format!("缺少 {key}"));
    let choices = window_choices(&weak).await?;
    let home = text("codex_home")?.unwrap_or(choices.home);
    let request = match method.as_str() {
        "status" => ControlRequest::Status { home },
        "preview_route" => ControlRequest::PreviewRoute {
            home,
            port: text("port")?.unwrap_or(choices.port),
            model: text("model")?.unwrap_or(choices.model),
        },
        "apply_route" => ControlRequest::ApplyRoute {
            preview: required("preview_id")?,
        },
        "preview_direct" => ControlRequest::PreviewDirect {
            home,
            provider: required("provider_id")?,
        },
        "apply_direct" => ControlRequest::ApplyDirect {
            preview: required("preview_id")?,
        },
        "cancel_preview" => ControlRequest::CancelPreview,
        "restore_config" => ControlRequest::Restore { home },
        _ => return Err(format!("未知的控制方法：{method}")),
    };
    let (reply, result) = oneshot::channel();
    sender
        .send(Command::Control { request, reply })
        .await
        .map_err(|_| "SwitchX 后台已停止")?;
    result.await.map_err(|_| "SwitchX 后台已停止".to_owned())?
}

impl Worker {
    pub(super) async fn control(&mut self, request: ControlRequest) -> Result<Value, String> {
        if self.recovery_only && !request.allowed_during_recovery() {
            return Err(
                "旧版资料有配置待恢复；请先调用 restore_config 或在应用中恢复原配置。".into(),
            );
        }
        match request {
            ControlRequest::Status { home } => self.control_status(&home),
            ControlRequest::PreviewRoute { home, port, model } => {
                self.control_previews.clear();
                let preview = self
                    .inspect_route(home.clone(), port.clone(), model.clone())
                    .await?;
                let id = self
                    .control_previews
                    .issue(PreviewTarget::Route { home, port, model })?;
                Ok(route_preview_json(&preview, &id))
            }
            ControlRequest::ApplyRoute { preview } => {
                let PreviewTarget::Route { home, port, model } =
                    self.control_previews.take(&preview)?
                else {
                    return Err("这个预览 ID 属于直连预览；请调用 apply_direct。".into());
                };
                self.apply_route(home.clone(), port, model).await?;
                Ok(json!({
                    "applied": "route",
                    "message": "模型路由已开启；运行 codex --no-daemon 新建会话。已有会话不会切换到路由。",
                    "status": self.control_status(&home)?,
                }))
            }
            ControlRequest::PreviewDirect { home, provider } => {
                self.control_previews.clear();
                let preview = self.inspect_direct(provider, home.clone())?;
                let id = self
                    .control_previews
                    .issue(PreviewTarget::Direct { home })?;
                Ok(direct_preview_json(&preview, &id))
            }
            ControlRequest::ApplyDirect { preview } => {
                let PreviewTarget::Direct { home } = self.control_previews.take(&preview)? else {
                    return Err("这个预览 ID 属于路由预览；请调用 apply_route。".into());
                };
                let message = self.apply_direct(home.clone()).await?;
                Ok(json!({
                    "applied": "direct",
                    "message": message,
                    "status": self.control_status(&home)?,
                }))
            }
            ControlRequest::CancelPreview => {
                let had = self.control_previews.pending();
                self.control_previews.clear();
                self.prepared = None;
                self.cancel_route_preview();
                Ok(json!({ "cancelled": had }))
            }
            ControlRequest::Restore { home } => {
                self.control_previews.clear();
                self.restore_config(home.clone()).await?;
                Ok(json!({
                    "restored": true,
                    "message": "原配置的受管字段已恢复，本地路由已停止；请重启目标 Codex 客户端。",
                    "status": self.control_status(&home)?,
                }))
            }
        }
    }

    /// Live router and Codex configuration state, without addresses' tokens or credentials.
    fn control_status(&self, home: &str) -> Result<Value, String> {
        let data_dir = self.data_dir.path()?;
        let route = &self.route_session;
        let mut status = json!({
            "router": {
                "running": route.is_running(),
                "request_recording_failed": route.recording_failed(),
            },
            "recovery_required": self.recovery_only,
            "pending_agent_preview": self.control_previews.pending(),
        });
        if let Some(address) = route.address() {
            status["router"]["endpoint"] = format!("http://{address}/v1").into();
        }
        if let Some(error) = route.chatgpt_error() {
            status["router"]["subscription_error"] = error.into();
        }
        status["codex"] = codex_status(Path::new(home), data_dir);
        Ok(status)
    }
}

fn codex_status(home: &Path, data_dir: &Path) -> Value {
    let managed = if data_dir.join(codex::ROUTE_JOURNAL).exists() {
        "route"
    } else if data_dir.join(codex::DIRECT_JOURNAL).exists() {
        "direct"
    } else {
        "none"
    };
    let mut status = json!({ "home": home.display().to_string(), "managed_by_switchx": managed });
    match client::inspect(home, data_dir) {
        Ok(config) => {
            status["config_exists"] = config.config_exists.into();
            status["mode"] = config.mode.into();
            status["provider"] = config.provider.into();
            status["model"] = config.model.into();
        }
        Err(error) => status["error"] = error.into(),
    }
    status
}

fn route_preview_json(preview: &RoutePreview, id: &str) -> Value {
    let destination = |destination: &switchx::route_session::PreviewDestination| json!({ "name": destination.name, "origin": destination.origin });
    json!({
        "preview_id": id,
        "kind": "route",
        "codex_client": preview.client_version,
        "router_endpoint": preview.endpoint,
        "config_path": preview.target.display().to_string(),
        "models": preview.mappings.iter().map(|mapping| {
            let mut value = json!({
                "model": mapping.model,
                "destination": destination(&mapping.destination),
            });
            if let Some(fallback) = &mapping.fallback {
                value["fallback"] = destination(fallback);
            }
            value
        }).collect::<Vec<_>>(),
        "changed_fields": preview.changed_fields,
        "account_bindings": preview.bindings.iter().map(|binding| json!({
            "connection": binding.provider,
            "facts": binding.facts.iter().map(|fact| json!({
                "label": fact.label,
                "value": fact.value,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "notes": preview.notes,
        "next": "向用户说明这些变更并取得同意后，调用 apply_route 并传入 preview_id。",
    })
}

fn direct_preview_json(preview: &DirectPreview, id: &str) -> Value {
    json!({
        "preview_id": id,
        "kind": "direct",
        "provider_id": preview.provider_id,
        "provider_name": preview.provider_name,
        "model": preview.model,
        "config_path": preview.target.display().to_string(),
        "changed_fields": preview.changes,
        "next": "向用户说明这些变更并取得同意后，调用 apply_direct 并传入 preview_id。",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn exchange(request: &[u8], expected: &str) -> (Response, Option<(String, Value)>) {
        let (mut client, server) = tokio::io::duplex(MESSAGE_LIMIT * 2);
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let record = seen.clone();
        let expected = expected.to_owned();
        let task = tokio::spawn(async move {
            serve(server, &expected, |method, params| async move {
                *record.lock().unwrap() = Some((method, params));
                Ok(json!({ "done": true }))
            })
            .await;
        });
        client.write_all(request).await.unwrap();
        // Closing the write side ends an unterminated request without waiting for the timeout.
        client.shutdown().await.unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).await.unwrap();
        task.await.unwrap();
        let seen = seen.lock().unwrap().take();
        (serde_json::from_str(&response).unwrap(), seen)
    }

    #[tokio::test]
    async fn serve_dispatches_only_authorized_complete_requests() {
        let token = "a".repeat(64);
        let request = |token: &str| {
            format!(
                "{}\n",
                json!({ "token": token, "method": "status", "params": { "codex_home": "/x" } })
            )
        };
        let (response, seen) = exchange(request(&token).as_bytes(), &token).await;
        assert_eq!(
            response,
            Response::Ok {
                result: json!({ "done": true })
            }
        );
        assert_eq!(seen, Some(("status".into(), json!({ "codex_home": "/x" }))));

        let (response, seen) = exchange(request(&"b".repeat(64)).as_bytes(), &token).await;
        assert!(matches!(response, Response::Error { .. }));
        assert!(seen.is_none());
        assert!(!serde_json::to_string(&response).unwrap().contains(&token));

        // Unterminated and oversized requests are refused without dispatch.
        let (_, seen) = exchange(request(&token).trim_end().as_bytes(), &token).await;
        assert!(seen.is_none());
        let huge = format!("{}\n", "x".repeat(MESSAGE_LIMIT + 10));
        let (response, seen) = exchange(huge.as_bytes(), &token).await;
        assert!(matches!(response, Response::Error { .. }));
        assert!(seen.is_none());
    }

    #[test]
    fn previews_apply_once_and_only_by_their_id() {
        let mut previews = ControlPreviews::default();
        assert!(previews.take("missing").is_err());
        let route = PreviewTarget::Route {
            home: "/codex".into(),
            port: "18731".into(),
            model: "sx-flash".into(),
        };
        let id = previews.issue(route.clone()).unwrap();
        assert_eq!(previews.pending(), Some("route"));
        assert!(previews.take("wrong").is_err());
        // A wrong ID also discards the preview.
        assert!(previews.take(&id).is_err());

        let id = previews.issue(route.clone()).unwrap();
        assert_eq!(previews.take(&id).unwrap(), route);
        assert!(previews.take(&id).is_err());

        let id = previews
            .issue(PreviewTarget::Direct {
                home: "/codex".into(),
            })
            .unwrap();
        previews.clear();
        assert!(previews.take(&id).is_err());
        assert_eq!(previews.pending(), None);
    }
}
