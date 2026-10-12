//! Per-connection checks that run beside other operations.

use std::path::PathBuf;

use slint::{ComponentHandle, Model};
use switchx::{
    app,
    chatgpt::{self, accounts::AccountManager},
    id::new_id,
    storage::{ProviderKind, ProviderRecord},
    ui::{AppWindow, ProviderRow},
    upstream, xai,
};
use tokio::sync::mpsc;

use super::{
    codex_config::codex_home,
    command::{Command, send_error},
    feedback::show_action_feedback,
    rows::sync_rows,
    workbench::filter_providers,
    worker::Worker,
};

pub(super) fn connect(app: &AppWindow, sender: &mpsc::Sender<Command>) {
    let sender = sender.clone();
    let weak = app.as_weak();
    app.on_check_provider(move |id| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if app.get_busy()
            || app.get_recovery_only()
            || !app
                .get_providers()
                .iter()
                .any(|row| row.id == id && row.check_id.is_empty())
        {
            return;
        }
        let check_id = match new_id() {
            Ok(id) => id,
            Err(error) => {
                show_action_feedback(&app, Err(error));
                return;
            }
        };
        set_provider_check_id(&app, &id, &check_id);
        if let Err(error) = sender.try_send(Command::Check {
            id: id.to_string(),
            home: app.get_config_home().into(),
            check_id: check_id.clone(),
            started: std::time::Instant::now(),
        }) {
            finish_provider_check(&app, &id, &check_id, 0, Err(send_error(&error).into()));
        }
    });
}

pub(super) fn set_provider_check_id(app: &AppWindow, id: &str, check_id: &str) {
    let rows = app
        .get_providers()
        .iter()
        .map(|mut row| {
            if row.id == id {
                row.check_id = check_id.into();
                row.check_stage = if check_id.is_empty() {
                    ""
                } else {
                    "读取连接资料"
                }
                .into();
                row.check_elapsed_ms = 0;
                row.check_message = "".into();
                row.check_detail = "".into();
                row.check_failed = false;
            }
            row
        })
        .collect::<Vec<_>>();
    sync_rows(
        app.get_providers(),
        rows,
        |row| row.id.clone(),
        |model| app.set_providers(model),
    );
    filter_providers(app, &app.get_provider_query());
}

pub(super) fn update_provider_check(
    app: &AppWindow,
    id: &str,
    check_id: &str,
    update: impl Fn(&mut ProviderRow),
) {
    let mut changed = false;
    let rows = app
        .get_providers()
        .iter()
        .map(|mut row| {
            if row.id == id && row.check_id == check_id {
                update(&mut row);
                changed = true;
            }
            row
        })
        .collect::<Vec<_>>();
    if changed {
        sync_rows(
            app.get_providers(),
            rows,
            |row| row.id.clone(),
            |model| app.set_providers(model),
        );
        filter_providers(app, &app.get_provider_query());
    }
}

pub(super) struct ProviderCheckResult {
    message: String,
    detail: String,
}

pub(super) fn finish_provider_check(
    app: &AppWindow,
    id: &str,
    check_id: &str,
    elapsed_ms: i32,
    result: Result<ProviderCheckResult, String>,
) {
    update_provider_check(app, id, check_id, |row| {
        row.check_id = "".into();
        row.check_stage = "".into();
        row.check_elapsed_ms = elapsed_ms;
        row.check_failed = result.is_err();
        let (message, detail) = match &result {
            Ok(result) => (result.message.as_str(), result.detail.as_str()),
            Err(error) => ("检查失败", error.as_str()),
        };
        row.check_message = message.into();
        row.check_detail = detail.into();
    });
}

pub(super) async fn run_check(
    data: Result<(PathBuf, ProviderRecord), String>,
    target: &str,
    xai_manager: Option<xai::AccountManager>,
    progress: impl Fn(&str),
) -> Result<ProviderCheckResult, String> {
    let (data_dir, provider) = data?;
    match provider.kind {
        ProviderKind::XaiOAuth => {
            progress("核对账号绑定");
            let manager = xai_manager.ok_or("Grok 账号状态不可用")?;
            let account = manager.resolve_binding(
                provider
                    .account_binding
                    .as_ref()
                    .ok_or("Grok 账号绑定缺失")?,
            )?;
            progress("读取账号凭据");
            let token = manager.credential(&account.id).await?;
            progress("读取模型目录");
            upstream::check_models(&provider, token.expose()).await?;
            Ok(ProviderCheckResult {
                message: "模型目录已连通".into(),
                detail: "尚未验证实际推理权限".into(),
            })
        }
        ProviderKind::Chatgpt => {
            progress("核对账号绑定");
            let target = codex_home(target)?;
            let manager = AccountManager::open(&data_dir)?;
            let account = manager
                .resolve_binding(
                    provider
                        .account_binding
                        .as_ref()
                        .ok_or("订阅账号绑定缺失")?,
                )
                .await?;
            if let Some(account) = account {
                progress("读取账号凭据");
                manager.credential(&account.id, &target).await?;
                Ok(ProviderCheckResult {
                    message: "账号凭据可读取".into(),
                    detail: "尚未验证实际官方请求权限".into(),
                })
            } else {
                progress("读取登录状态");
                let status = chatgpt::account(&target, false).await?;
                status.require_chatgpt()?;
                Ok(ProviderCheckResult {
                    message: "登录状态可读取".into(),
                    detail: "尚未验证实际官方请求权限".into(),
                })
            }
        }
        ProviderKind::ApiKey => {
            progress("读取 API 凭据");
            let token = app::provider_credential(&data_dir, &provider)?;
            progress("读取模型目录");
            upstream::check_models(&provider, token.expose()).await?;
            Ok(ProviderCheckResult {
                message: "模型目录已连通".into(),
                detail: format!("已找到 {}；尚未验证推理与工具调用", provider.model_id),
            })
        }
    }
}

impl Worker {
    /// Checks run as tracked tasks so a slow upstream never blocks other commands.
    pub(super) fn check_provider(
        &mut self,
        id: String,
        target: String,
        check_id: String,
        started: std::time::Instant,
    ) {
        let data = self
            .data_dir
            .path()
            .and_then(|path| Ok((path.to_path_buf(), app::load_provider(path, &id)?)))
            .and_then(|(path, provider)| {
                if provider.kind == ProviderKind::XaiOAuth {
                    self.xai_manager()?;
                }
                Ok((path, provider))
            });
        let manager = self.xai_manager.clone();
        let weak = self.weak.clone();
        self.provider_checks.spawn(async move {
            let progress = |stage: &str| {
                let stage = stage.to_owned();
                let id = id.clone();
                let check_id = check_id.clone();
                let _ = weak.upgrade_in_event_loop(move |app| {
                    update_provider_check(&app, &id, &check_id, |row| {
                        row.check_stage = stage.as_str().into();
                    });
                });
            };
            let request = run_check(data, &target, manager, progress);
            tokio::pin!(request);
            let mut timer = tokio::time::interval(std::time::Duration::from_secs(1));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                let elapsed_ms = || started.elapsed().as_millis().min(i32::MAX as u128) as i32;
                tokio::select! {
                    result = &mut request => {
                        let elapsed_ms = elapsed_ms();
                        let id = id.clone();
                        let check_id = check_id.clone();
                        let _ = weak.upgrade_in_event_loop(move |app| {
                            finish_provider_check(&app, &id, &check_id, elapsed_ms, result);
                        });
                        break;
                    }
                    _ = timer.tick() => {
                        let elapsed_ms = elapsed_ms();
                        let id = id.clone();
                        let check_id = check_id.clone();
                        let _ = weak.upgrade_in_event_loop(move |app| {
                            update_provider_check(&app, &id, &check_id, |row| {
                                row.check_elapsed_ms = elapsed_ms;
                            });
                        });
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::{ModelRc, VecModel};
    use switchx::{
        app::Snapshot,
        credentials::Secret,
        headless,
        storage::Store,
        ui::{ModelRow, Theme},
    };

    use crate::desktop::{
        command::queue,
        icon_picker::{initialize_provider_icons, provider_icon_row},
        workbench::show_result,
        worker,
    };

    #[test]
    fn provider_checks_preserve_workbench_animation_and_expanded_models() {
        use slint::platform::{PointerEventButton, WindowEvent};

        let window = headless::install_with_manual_time();
        let app = AppWindow::new().unwrap();
        app.global::<Theme>().set_animations_enabled(true);
        app.global::<Theme>().set_system_reduced_motion(false);
        app.set_loading(false);
        initialize_provider_icons(&app).unwrap();
        app.show().unwrap();

        let draw = |state: &str| {
            let pixels = headless::render(&window);
            if !state.is_empty() {
                let theme = if app.global::<Theme>().get_dark() {
                    "dark"
                } else {
                    "light"
                };
                headless::save_snapshot(
                    "SWITCHX_WORKBENCH_CHECK_SNAPSHOTS",
                    &format!("{theme}-{}x{}-{state}", pixels.width(), pixels.height()),
                    &pixels,
                );
            }
            pixels
        };
        let settle = || {
            draw("");
            headless::advance(200);
            draw("");
            headless::advance(500);
            draw("");
        };

        for (width, height) in [(1200, 820), (1000, 680)] {
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            for dark in [false, true] {
                app.invoke_set_appearance(dark);
                app.set_active_page(1);
                app.set_active_page(0);
                app.set_model_scroll_y(0.0);
                let rows = [
                    ("chatgpt", "ChatGPT", "openai", true, false),
                    ("grok", "Grok", "grok", true, true),
                    ("api", "API", "anthropic", false, false),
                ]
                .map(|(id, name, icon, subscription, grok)| {
                    let brand = provider_icon_row(&app, icon);
                    ProviderRow {
                        id: id.into(),
                        name: name.into(),
                        icon: brand.icon,
                        icon_id: brand.id,
                        monochrome: brand.monochrome,
                        is_subscription: subscription,
                        is_grok: grok,
                        model_count: 3,
                        ready_model_count: 3,
                        selected_model_count: 3,
                        models: ModelRc::new(VecModel::from(
                            (0..3)
                                .map(|index| ModelRow {
                                    public_id: format!("{id}-{index}").into(),
                                    display_name: format!("Synthetic model {index}").into(),
                                    saved: true,
                                    ready: true,
                                    included: true,
                                    ..Default::default()
                                })
                                .collect::<Vec<_>>(),
                        )),
                        ..Default::default()
                    }
                });
                app.set_providers(ModelRc::new(VecModel::from(rows.to_vec())));
                filter_providers(&app, "");
                app.set_selected_model_count(9);
                app.set_selectable_model_count(9);
                settle();
                let collapsed = draw("collapsed");
                let position = slint::LogicalPosition::new(width as f32 - 100.0, 350.0);
                window.dispatch_event(WindowEvent::PointerPressed {
                    position,
                    button: PointerEventButton::Left,
                });
                window.dispatch_event(WindowEvent::PointerReleased {
                    position,
                    button: PointerEventButton::Left,
                });
                window.dispatch_event(WindowEvent::PointerMoved {
                    position: slint::LogicalPosition::new(500.0, 200.0),
                });
                settle();
                let expanded = draw("");
                assert!(
                    collapsed.as_bytes() != expanded.as_bytes(),
                    "expanding models must change the workbench"
                );
                headless::scroll(&window, 500.0, 500.0, -24.0);
                settle();
                let expanded = draw("expanded");
                let providers = app.get_providers();
                let filtered = app.get_filtered_providers();
                let scroll_y = app.get_model_scroll_y();
                assert!(scroll_y < 0.0, "expanded models must be scrolled into view");
                let unchanged = |state: &str| {
                    let frame = draw(state);
                    assert!(
                        frame.as_bytes() == expanded.as_bytes(),
                        "{state} must preserve visible cards and expanded models at {width}x{height}, dark={dark}"
                    );
                    assert_eq!(app.get_providers(), providers);
                    assert_eq!(app.get_filtered_providers(), filtered);
                    assert_eq!(app.get_model_scroll_y(), scroll_y);
                    headless::advance(100);
                    let frame = draw("");
                    assert!(
                        frame.as_bytes() == expanded.as_bytes(),
                        "{state} must not replay entry animations"
                    );
                };
                for id in ["chatgpt", "grok", "api"] {
                    set_provider_check_id(&app, id, id);
                    unchanged("started");
                }
                for elapsed_ms in [1000, 2000, 10000, 14000] {
                    headless::advance(1000);
                    for id in ["chatgpt", "grok", "api"] {
                        update_provider_check(&app, id, id, |row| {
                            row.check_stage = if id == "chatgpt" {
                                "读取登录状态"
                            } else {
                                "读取模型目录"
                            }
                            .into();
                            row.check_elapsed_ms = elapsed_ms;
                        });
                        unchanged("progress");
                    }
                }
                for id in ["chatgpt", "grok", "api"] {
                    let result = if id == "grok" {
                        Err("合成检查失败".into())
                    } else {
                        Ok(ProviderCheckResult {
                            message: "合成检查完成".into(),
                            detail: "合成检查详情".into(),
                        })
                    };
                    finish_provider_check(&app, id, id, 15000, result);
                    unchanged("finished");
                }
            }
        }
    }

    #[test]
    fn checking_one_provider_keeps_other_connections_interactive() {
        let window = headless::install();
        let app = AppWindow::new().unwrap();
        app.global::<Theme>().set_animations_enabled(false);
        app.set_loading(false);
        app.set_active_page(1);
        initialize_provider_icons(&app).unwrap();
        let openai = provider_icon_row(&app, "openai");
        let grok = provider_icon_row(&app, "grok");
        let providers = ModelRc::new(VecModel::from(vec![
            ProviderRow {
                id: "synthetic-chatgpt".into(),
                name: "ChatGPT".into(),
                is_subscription: true,
                icon: openai.icon,
                icon_id: openai.id,
                monochrome: openai.monochrome,
                ..Default::default()
            },
            ProviderRow {
                id: "synthetic-grok".into(),
                name: "Grok".into(),
                is_subscription: true,
                is_grok: true,
                icon: grok.icon,
                icon_id: grok.id,
                monochrome: grok.monochrome,
                ..Default::default()
            },
        ]));
        app.set_providers(providers.clone());
        filter_providers(&app, "");
        assert_ne!(app.get_filtered_providers(), providers);
        let (sender, mut receiver) = mpsc::channel(8);
        connect(&app, &sender);
        app.on_begin_xai_editor({
            let sender = sender.clone();
            let weak = app.as_weak();
            move |id| {
                queue(
                    &weak.upgrade().unwrap(),
                    &sender,
                    Command::BeginXaiEditor(id.into()),
                )
            }
        });
        app.show().unwrap();
        let draw = || {
            let pixels = headless::render(&window);
            let theme = if app.global::<Theme>().get_dark() {
                "dark"
            } else {
                "light"
            };
            let checking = app
                .get_providers()
                .iter()
                .filter(|row| !row.check_id.is_empty())
                .count();
            headless::save_snapshot(
                "SWITCHX_PROVIDER_CHECK_SNAPSHOTS",
                &format!(
                    "{theme}-{}x{}-{checking}checking-busy{}",
                    pixels.width(),
                    pixels.height(),
                    app.get_busy()
                ),
                &pixels,
            );
        };
        let click = |x, y| {
            headless::click(&window, x, y);
            draw();
        };
        for (width, height) in [(1200, 820), (1000, 680)] {
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            for dark in [false, true] {
                app.invoke_set_appearance(dark);
                app.set_error_code("".into());
                app.set_error_message("".into());
                app.set_error_action("".into());
                app.set_action_message("".into());
                set_provider_check_id(&app, "synthetic-chatgpt", "");
                set_provider_check_id(&app, "synthetic-grok", "");
                draw();
                headless::scroll(&window, 500.0, 400.0, 2000.0);
                draw();
                let visible_cards = app.get_filtered_providers();
                // Use the footer check action, then the other card's controls.
                click(width as f32 - 88.0, 405.0);
                assert!(
                    !app.get_busy(),
                    "one provider check must not lock the whole app"
                );
                let Ok(Command::Check { id, check_id, .. }) = receiver.try_recv() else {
                    panic!("the first check must be queued");
                };
                assert_eq!(id, "synthetic-chatgpt");
                let chatgpt_check = check_id;
                assert_eq!(
                    app.get_filtered_providers(),
                    visible_cards,
                    "checking must keep cards mounted"
                );
                update_provider_check(&app, "synthetic-chatgpt", &chatgpt_check, |row| {
                    row.check_stage = "读取登录状态".into();
                    row.check_elapsed_ms = 12500;
                });
                assert_eq!(
                    app.get_providers().row_data(0).unwrap().check_stage,
                    "读取登录状态"
                );
                assert_eq!(
                    app.get_filtered_providers()
                        .row_data(0)
                        .unwrap()
                        .check_elapsed_ms,
                    12500
                );
                // Reset the slow hint before the fixed-coordinate interaction checks.
                update_provider_check(&app, "synthetic-chatgpt", &chatgpt_check, |row| {
                    row.check_elapsed_ms = 2000
                });
                draw();
                app.invoke_check_provider("synthetic-chatgpt".into());
                assert!(
                    receiver.try_recv().is_err(),
                    "duplicate checks must be ignored"
                );
                click(width as f32 - 88.0, 568.0);
                let Ok(Command::Check { id, check_id, .. }) = receiver.try_recv() else {
                    panic!("the other connection must still be checkable");
                };
                assert_eq!(id, "synthetic-grok");
                let grok_check = check_id;
                click(width as f32 - 122.0, 497.0);
                assert!(
                    matches!(receiver.try_recv(), Ok(Command::BeginXaiEditor(id)) if id == "synthetic-grok")
                );
                assert!(app.get_busy());
                finish_provider_check(
                    &app,
                    "synthetic-chatgpt",
                    &chatgpt_check,
                    2300,
                    Ok(ProviderCheckResult {
                        message: "登录状态可读取".into(),
                        detail: "尚未验证实际官方请求权限".into(),
                    }),
                );
                assert!(app.get_busy(), "a check must not unlock another operation");
                let completed = app.get_providers().row_data(0).unwrap();
                assert_eq!(completed.check_message, "登录状态可读取");
                assert_eq!(completed.check_elapsed_ms, 2300);
                assert!(!completed.check_failed);
                assert!(
                    app.get_action_message().is_empty(),
                    "check results must remain in their own row"
                );
                assert!(!app.get_providers().row_data(1).unwrap().check_id.is_empty());
                app.set_busy(false);
                // Saving and rechecking a connection invalidates its old completion.
                set_provider_check_id(&app, "synthetic-grok", "");
                app.invoke_check_provider("synthetic-grok".into());
                let Ok(Command::Check { check_id, .. }) = receiver.try_recv() else {
                    panic!("a saved connection can be checked again");
                };
                finish_provider_check(
                    &app,
                    "synthetic-grok",
                    &grok_check,
                    3000,
                    Err("过时结果".into()),
                );
                update_provider_check(&app, "synthetic-grok", &grok_check, |row| {
                    row.check_stage = "过时步骤".into()
                });
                assert_eq!(app.get_providers().row_data(1).unwrap().check_id, check_id);
                assert_ne!(
                    app.get_providers().row_data(1).unwrap().check_stage,
                    "过时步骤"
                );
                finish_provider_check(
                    &app,
                    "synthetic-grok",
                    &check_id,
                    1500,
                    Err("合成失败".into()),
                );
                assert!(app.get_providers().row_data(1).unwrap().check_id.is_empty());
                assert!(app.get_providers().row_data(1).unwrap().check_failed);
                assert_eq!(
                    app.get_providers().row_data(1).unwrap().check_detail,
                    "合成失败"
                );
                assert!(app.get_error_message().is_empty());
            }
        }
        let mut snapshot = Snapshot {
            providers: app
                .get_providers()
                .iter()
                .map(|row| app::ProviderView {
                    id: row.id.into(),
                    name: row.name.into(),
                    icon_id: row.icon_id.into(),
                    endpoint: row.endpoint.into(),
                    base_url: row.base_url.into(),
                    model_id: row.model_id.into(),
                    credential_status: "合成凭据状态",
                    preset_id: "",
                    kind: if row.is_grok {
                        ProviderKind::XaiOAuth
                    } else {
                        ProviderKind::Chatgpt
                    },
                    binding_label: row.binding_label.into(),
                })
                .collect(),
            models: Vec::new(),
            credentials_checked: false,
        };
        app.set_provider_query("Grok".into());
        show_result(&app, Ok(snapshot.clone()));
        assert_eq!(app.get_providers().row_count(), 2);
        assert_eq!(
            app.get_filtered_providers()
                .row_data(0)
                .unwrap()
                .check_detail,
            "合成失败"
        );
        set_provider_check_id(&app, "synthetic-grok", "refresh-check");
        update_provider_check(&app, "synthetic-grok", "refresh-check", |row| {
            row.check_stage = "读取模型目录".into();
            row.check_elapsed_ms = 3500;
        });
        show_result(&app, Ok(snapshot.clone()));
        let refreshed = app.get_filtered_providers().row_data(0).unwrap();
        assert_eq!(refreshed.check_id, "refresh-check");
        assert_eq!(refreshed.check_stage, "读取模型目录");
        assert_eq!(refreshed.check_elapsed_ms, 3500);
        snapshot.providers[1].name = "Grok Updated".into();
        show_result(&app, Ok(snapshot));
        finish_provider_check(
            &app,
            "synthetic-grok",
            "refresh-check",
            4000,
            Err("过时配置的结果".into()),
        );
        assert!(
            app.get_filtered_providers()
                .row_data(0)
                .unwrap()
                .check_message
                .is_empty()
        );
        drop(receiver);
        app.invoke_check_provider("synthetic-grok".into());
        assert!(app.get_providers().row_data(1).unwrap().check_id.is_empty());
        assert_eq!(
            app.get_providers().row_data(1).unwrap().check_detail,
            "后台状态通道已停止"
        );
        assert!(app.get_error_message().is_empty());
    }

    #[tokio::test]
    async fn provider_checks_report_real_stages_and_keep_upstream_errors_private() {
        use axum::{Json, Router, http::StatusCode, routing::get};
        use std::sync::Mutex;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route(
                        "/ok/models",
                        get(|| async {
                            Json(serde_json::json!({"data": [{"id": "synthetic-model"}]}))
                        }),
                    )
                    .route(
                        "/denied/models",
                        get(|| async {
                            (StatusCode::UNAUTHORIZED, "synthetic-secret-from-upstream")
                        }),
                    ),
            )
            .into_future(),
        );
        let data =
            std::env::temp_dir().join(format!("switchx-check-progress-{}", new_id().unwrap()));
        std::fs::create_dir_all(&data).unwrap();
        let store = Store::open(&data.join("switchx.sqlite")).unwrap();
        for id in ["ok", "denied"] {
            let provider = ProviderRecord {
                id: id.into(),
                name: id.into(),
                base_url: format!("http://{address}/{id}"),
                model_id: "synthetic-model".into(),
                credential_ref: None,
                kind: ProviderKind::ApiKey,
                account_binding: None,
                icon_id: None,
            };
            store
                .put_provider_with_models_options_and_key(
                    &provider,
                    &[],
                    None,
                    &Secret::new("synthetic-key".into()),
                )
                .unwrap();
            let stages = Mutex::new(Vec::new());
            let result = run_check(Ok((data.clone(), provider)), "", None, |stage| {
                stages.lock().unwrap().push(stage.to_owned())
            })
            .await;
            assert_eq!(*stages.lock().unwrap(), ["读取 API 凭据", "读取模型目录"]);
            if id == "ok" {
                let result = result.unwrap();
                assert_eq!(result.message, "模型目录已连通");
                assert!(result.detail.contains("尚未验证推理与工具调用"));
            } else {
                assert_eq!(result.err().unwrap(), "上游模型目录返回 HTTP 401");
            }
        }
        drop(store);
        server.abort();
        std::fs::remove_dir_all(data).unwrap();
    }

    #[tokio::test]
    async fn provider_checks_overlap_without_blocking_other_commands() {
        use axum::{Json, Router, routing::get};
        use std::{sync::Arc, time::Duration};
        use tokio::{net::TcpListener, sync::Notify, time::timeout};

        let slow_started = Arc::new(Notify::new());
        let release_slow = Arc::new(Notify::new());
        let fast_started = Arc::new(Notify::new());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route(
                        "/slow/models",
                        get({
                            let started = slow_started.clone();
                            let release = release_slow.clone();
                            move || {
                                let started = started.clone();
                                let release = release.clone();
                                async move {
                                    started.notify_one();
                                    release.notified().await;
                                    Json(serde_json::json!({"data": [{"id": "synthetic-model"}]}))
                                }
                            }
                        }),
                    )
                    .route(
                        "/fast/models",
                        get({
                            let started = fast_started.clone();
                            move || {
                                let started = started.clone();
                                async move {
                                    started.notify_one();
                                    Json(serde_json::json!({"data": [{"id": "synthetic-model"}]}))
                                }
                            }
                        }),
                    ),
            )
            .into_future(),
        );
        let data = std::env::temp_dir().join(format!("switchx-checks-{}", new_id().unwrap()));
        std::fs::create_dir_all(&data).unwrap();
        let store = Store::open(&data.join("switchx.sqlite")).unwrap();
        for id in ["slow", "fast"] {
            store
                .put_provider_with_models_options_and_key(
                    &ProviderRecord {
                        id: id.into(),
                        name: id.into(),
                        base_url: format!("http://{address}/{id}"),
                        model_id: "synthetic-model".into(),
                        credential_ref: None,
                        kind: ProviderKind::ApiKey,
                        account_binding: None,
                        icon_id: None,
                    },
                    &[],
                    None,
                    &Secret::new("synthetic-key".into()),
                )
                .unwrap();
        }
        drop(store);
        let (sender, receiver) = mpsc::channel(8);
        let background = tokio::spawn(worker::run(
            receiver,
            slint::Weak::default(),
            Ok(data.clone()),
        ));
        sender
            .send(Command::Check {
                id: "slow".into(),
                home: String::new(),
                check_id: "slow-check".into(),
                started: std::time::Instant::now(),
            })
            .await
            .unwrap();
        timeout(Duration::from_secs(2), slow_started.notified())
            .await
            .unwrap();
        sender
            .send(Command::Check {
                id: "fast".into(),
                home: String::new(),
                check_id: "fast-check".into(),
                started: std::time::Instant::now(),
            })
            .await
            .unwrap();
        timeout(Duration::from_secs(2), fast_started.notified())
            .await
            .expect("the second check must start while the first is waiting");
        sender.send(Command::Delete("fast".into())).await.unwrap();
        timeout(Duration::from_secs(2), async {
            while app::load_provider(&data, "fast").is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("other commands must proceed while a check is waiting");
        release_slow.notify_one();
        drop(sender);
        timeout(Duration::from_secs(2), background)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        server.abort();
        std::fs::remove_dir_all(data).unwrap();
    }
}
