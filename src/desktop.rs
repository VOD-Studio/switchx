//! The desktop shell: window callbacks, the views they update, and the
//! background worker that runs their commands. Business rules live in the
//! `switchx` library.

mod accounts;
mod activity;
mod batch_picker;
mod claude;
mod clients;
mod codex_config;
mod command;
mod common_config;
mod connection_models;
mod control;
mod feedback;
mod grok;
mod icon_picker;
mod models;
mod provider_checks;
mod provider_editor;
mod quota;
mod rows;
mod subscription_editor;
mod toolbox;
mod usage;
pub(crate) mod usage_view;
mod workbench;
mod worker;

use slint::{ComponentHandle, ModelRc, VecModel};
use switchx::{
    app::{self, data_directory},
    code_highlight,
    codex::client,
    ui::{AppWindow, ProviderPresetRow, SwitchXTray},
};
#[cfg(target_os = "macos")]
use switchx::{macos, ui::Theme};
use tokio::sync::mpsc;

use command::{Command, queue};

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "macos")]
    macos::configure_window()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let app = AppWindow::new()?;
    #[cfg(target_os = "linux")]
    slint::set_xdg_app_id("switchx")?;
    app.set_app_version(env!("CARGO_PKG_VERSION").into());
    switchx::model_picker::connect(&app);
    switchx::reasoning_picker::connect(&app);
    #[cfg(target_os = "macos")]
    {
        app.set_native_titlebar_overlay(true);
        app.global::<Theme>()
            .set_system_reduced_motion(macos::prefers_reduced_motion());
        let weak = app.as_weak();
        app.on_refresh_system_appearance(move || {
            if let Some(app) = weak.upgrade() {
                app.global::<Theme>()
                    .set_system_reduced_motion(macos::prefers_reduced_motion());
            }
        });
    }
    code_highlight::connect(&app);
    switchx::request_overrides::connect(&app);
    initialize_provider_presets(&app)?;
    icon_picker::initialize_provider_icons(&app)?;
    let tray = SwitchXTray::new()?;
    #[cfg(target_os = "macos")]
    {
        // Slint defers native tray creation until its change handlers run.
        slint::platform::update_timers_and_animations();
        if let Err(error) = macos::use_template_tray_icon("switchx") {
            eprintln!("SwitchX menu bar appearance: {error}");
        }
    }
    let directory = data_directory();
    app.set_data_path(
        directory
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "不可用".into())
            .into(),
    );
    app.set_config_home(
        client::default_home()
            .map(|path| path.display().to_string())
            .unwrap_or_default()
            .into(),
    );
    clients::initialize(&app);
    let (sender, receiver) = mpsc::channel::<Command>(8);
    let background = runtime.spawn(worker::run(
        receiver,
        sender.downgrade(),
        app.as_weak(),
        directory.clone(),
    ));
    let _tray_status = connect_tray(&app, &tray, &sender);
    #[cfg(target_os = "macos")]
    {
        let quit_sender = sender.clone();
        let weak = app.as_weak();
        macos::install_quit_handler(move || {
            if let Some(app) = weak.upgrade() {
                queue(&app, &quit_sender, Command::Quit);
            }
        })?;
    }
    let usage_service = usage::UsageService::start(app.as_weak());
    usage::connect(&app, &usage_service);
    // Client initialization starts log syncing after restoring the saved directory.
    if let Ok(path) = &directory {
        workbench::watch_external_changes(&app, &usage_service, path.clone());
    }
    workbench::connect(&app, &sender);
    activity::connect(&app, &sender);
    provider_editor::connect(&app, &sender);
    icon_picker::connect(&app);
    subscription_editor::connect(&app, &sender);
    grok::connect(&app, &sender);
    common_config::connect(&app, &sender);
    provider_checks::connect(&app, &sender);
    connection_models::connect(&app, &sender);
    connection_models::connect_discovery(&app, &sender);
    accounts::connect(&app, &sender);
    models::connect(&app, &sender);
    batch_picker::connect(&app, &sender);
    codex_config::connect(&app, &sender);
    clients::connect(&app, &sender);
    claude::connect(&app, &sender);
    toolbox::connect(&app, &sender);
    queue(&app, &sender, Command::InitializeClients);
    queue(&app, &sender, Command::Refresh(false));
    let weak = app.as_weak();
    app.window().on_close_requested(move || {
        if let Some(app) = weak.upgrade() {
            app.set_window_visible(false);
            let claude = app.global::<switchx::ui::ClaudeUi>();
            if claude.get_preview_open() || claude.get_preview_ready() {
                claude.invoke_cancel_preview();
            }
            claude.set_editor_generation(claude.get_editor_generation().wrapping_add(1));
            claude.set_editor_open(false);
            app.global::<switchx::ui::ClaudeUi>()
                .set_edit_key("".into());
            app.global::<switchx::ui::ClaudeUi>()
                .set_preview_open(false);
            app.set_icon_picker_open(false);
            if app.get_subscription_editor_open() || app.get_subscription_editor_pending() {
                app.invoke_cancel_subscription_editor();
            }
        }
        slint::CloseRequestResponse::HideWindow
    });
    let result = app.run();
    usage_service.stop();
    #[cfg(target_os = "macos")]
    macos::clear_quit_handler();
    drop(app);
    drop(tray);
    drop(sender);
    runtime.block_on(background)??;
    drop(runtime);
    result.map_err(Into::into)
}

fn initialize_provider_presets(app: &AppWindow) -> Result<(), slint::LoadImageError> {
    let presets = app::PROVIDER_PRESETS
        .iter()
        .map(|preset| {
            slint::Image::load_from_svg_data(preset.icon).map(|icon| ProviderPresetRow {
                id: preset.id.into(),
                name: preset.name.into(),
                icon,
                monochrome: preset.monochrome,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    app.set_provider_presets(ModelRc::new(VecModel::from(presets)));
    Ok(())
}

fn connect_tray(
    app: &AppWindow,
    tray: &SwitchXTray,
    sender: &mpsc::Sender<Command>,
) -> slint::Timer {
    let timer = slint::Timer::default();
    let window = app.as_weak();
    let icon = tray.as_weak();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_secs(1),
        move || {
            if let (Some(window), Some(icon)) = (window.upgrade(), icon.upgrade()) {
                let ui = window.global::<switchx::ui::ClaudeUi>();
                icon.set_claude_route_status(
                    if ui.get_route_running() {
                        "Claude 当前工作空间 · 路由运行中"
                    } else if ui.get_recovering() {
                        "Claude 当前工作空间 · 待恢复"
                    } else {
                        "Claude 当前工作空间"
                    }
                    .into(),
                );
            }
        },
    );
    let window = app.as_weak();
    tray.on_show_app(move || {
        if let Some(app) = window.upgrade() {
            app.set_window_visible(true);
            let _ = app.show();
        }
    });
    let window = app.as_weak();
    let restore_sender = sender.clone();
    tray.on_restore_config(move || {
        if let Some(app) = window.upgrade() {
            app.set_window_visible(true);
            let _ = app.show();
            queue(
                &app,
                &restore_sender,
                Command::RestoreConfig(app.get_config_home().to_string()),
            );
        }
    });
    let window = app.as_weak();
    let restore_sender = sender.clone();
    tray.on_restore_claude(move || {
        if let Some(app) = window.upgrade() {
            app.set_window_visible(true);
            let _ = app.show();
            app.set_active_client(1);
            app.set_active_page(6);
            queue(
                &app,
                &restore_sender,
                Command::ClaudeRestorePreview {
                    home: app.get_claude_home().into(),
                    keep_conflicts: false,
                },
            );
        }
    });
    let quit_sender = sender.clone();
    let weak = app.as_weak();
    tray.on_quit_app(move || {
        if let Some(app) = weak.upgrade() {
            queue(&app, &quit_sender, Command::Quit);
        }
    });
    timer
}

/// Opens a provider page or login URL in the system browser.
fn open_url(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = std::process::Command::new("xdg-open");
    command
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| "无法打开系统浏览器，请检查默认浏览器设置".into())
}
