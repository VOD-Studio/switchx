//! Render synthetic provider avatar previews with the real Slint UI, without native windows.
//! Run: cargo run --example provider_icons_preview -- /absolute/output/directory

use switchx::ui::{
    AccountRow, AppWindow, CodexQuotaRow, ModelRow, ProviderIconRow, ProviderRow, RequestRow,
    Theme, XaiQuotaRow,
};

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Clipboard, Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{
    ComponentHandle, Model, ModelRc, PhysicalSize, Rgb8Pixel, SharedPixelBuffer, VecModel,
};
use std::{
    cell::{Cell, RefCell},
    error::Error,
    fs,
    io::Write,
    path::Path,
    rc::Rc,
    time::Duration,
};
use switchx::provider_icons;

thread_local! {
    static PREVIEW_TIME: Cell<Duration> = const { Cell::new(Duration::ZERO) };
    static CLIPBOARD: RefCell<String> = const { RefCell::new(String::new()) };
}

struct PreviewPlatform(Rc<MinimalSoftwareWindow>);

impl Platform for PreviewPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
    fn duration_since_start(&self) -> Duration {
        PREVIEW_TIME.with(Cell::get)
    }
    fn set_clipboard_text(&self, text: &str, clipboard: Clipboard) {
        if clipboard == Clipboard::DefaultClipboard {
            CLIPBOARD.with(|value| *value.borrow_mut() = text.into());
        }
    }
    fn clipboard_text(&self, _: Clipboard) -> Option<String> {
        Some(CLIPBOARD.with(|value| value.borrow().clone()))
    }
}

fn click(window: &MinimalSoftwareWindow, x: f32, y: f32) {
    let position = slint::LogicalPosition::new(x, y);
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
}

fn render(window: &MinimalSoftwareWindow) -> SharedPixelBuffer<Rgb8Pixel> {
    slint::platform::update_timers_and_animations();
    let size = WindowAdapter::size(window);
    let mut pixels = SharedPixelBuffer::<Rgb8Pixel>::new(size.width, size.height);
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(pixels.make_mut_slice(), size.width as usize);
    });
    PREVIEW_TIME.with(|clock| clock.set(clock.get() + Duration::from_millis(250)));
    slint::platform::update_timers_and_animations();
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(pixels.make_mut_slice(), size.width as usize);
    });
    pixels
}

fn snapshot(
    window: &MinimalSoftwareWindow,
    output: &Path,
    name: &str,
) -> Result<(), Box<dyn Error>> {
    let pixels = render(window);
    let mut file = std::io::BufWriter::new(fs::File::create(output.join(format!("{name}.ppm")))?);
    write!(file, "P6\n{} {}\n255\n", pixels.width(), pixels.height())?;
    file.write_all(pixels.as_bytes())?;
    file.flush()?;
    Ok(())
}

fn set_theme(app: &AppWindow, window: &MinimalSoftwareWindow, dark: bool) {
    app.invoke_set_appearance(dark);
    render(window);
    assert_eq!(app.global::<Theme>().get_dark(), dark);
}

fn render_icons(app: &AppWindow, window: &MinimalSoftwareWindow, icons: &[ProviderIconRow]) {
    app.set_icon_picker_open(true);
    for row in icons {
        app.set_filtered_provider_icons(ModelRc::new(VecModel::from(vec![row.clone()])));
        render(window);
    }
    app.set_icon_picker_open(false);
}

fn main() -> Result<(), Box<dyn Error>> {
    let output = std::env::args()
        .nth(1)
        .ok_or("pass an absolute output directory")?;
    let output = Path::new(&output);
    if !output.is_absolute() {
        return Err("output directory must be absolute".into());
    }
    fs::create_dir_all(output)?;
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(PreviewPlatform(window.clone())))?;
    let app = AppWindow::new()?;
    app.set_loading(false);
    app.global::<Theme>().set_animations_enabled(false);
    let layout_only = std::env::args().any(|arg| arg == "--layout-only");
    if std::env::args().any(|arg| arg == "--connection-status") {
        app.set_local_data_ready(true);
        app.set_config_provider("openai".into());
        app.set_config_model("gpt-6.1-sol".into());
        app.set_config_exists(true);
        app.set_config_mode("Codex 官方连接 · 登录由 Codex 管理".into());
        app.set_config_home("/isolated/codex".into());
        app.set_data_path("/isolated/Library/Application Support/SwitchX".into());
        app.set_action_message("已读取目标 config.toml；未修改配置".into());
        app.set_providers(ModelRc::new(VecModel::from(vec![
            ProviderRow::default(),
            ProviderRow::default(),
        ])));
        app.show()?;
        for (width, height) in [(1200, 820), (1000, 680)] {
            app.window().set_size(PhysicalSize::new(width, height));
            for dark in [false, true] {
                set_theme(&app, &window, dark);
                let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
                for state in ["collapsed", "expanded", "recovery", "busy"] {
                    app.set_drawer_open(false);
                    render(&window);
                    app.set_drawer_open(true);
                    render(&window);
                    assert!(!app.get_status_directories_expanded());
                    app.set_busy(state == "busy");
                    app.set_config_managed(state == "recovery");
                    app.set_route_managed(state == "recovery");
                    app.set_config_provider(
                        if state == "recovery" {
                            "switchx_router"
                        } else {
                            "openai"
                        }
                        .into(),
                    );
                    app.set_config_mode(
                        if state == "recovery" {
                            "SwitchX 路由配置已写入 · 请核对本地路由状态"
                        } else {
                            "Codex 官方连接 · 登录由 Codex 管理"
                        }
                        .into(),
                    );
                    app.set_route_status("路由未运行 · 有配置待恢复".into());
                    app.set_error_code(if state == "recovery" { "preview" } else { "" }.into());
                    app.set_error_message("配置包含外部更改，请先核对配置再恢复。".into());
                    app.set_error_action("前往配置与恢复查看详情。".into());
                    app.set_status_directories_expanded(state == "expanded" || state == "recovery");
                    snapshot(&window, output, &format!("status-{state}-{suffix}"))?;
                    window.dispatch_event(WindowEvent::KeyPressed {
                        text: slint::platform::Key::Escape.into(),
                    });
                    window.dispatch_event(WindowEvent::KeyReleased {
                        text: slint::platform::Key::Escape.into(),
                    });
                    assert_eq!(app.get_drawer_open(), state == "busy");
                    app.set_busy(false);
                }
            }
        }
        app.window().set_size(PhysicalSize::new(1200, 820));
        app.set_config_managed(false);
        app.set_route_managed(false);
        app.set_error_code("".into());
        app.set_drawer_open(false);
        render(&window);
        app.set_drawer_open(true);
        render(&window);
        click(&window, 500.0, 568.0);
        render(&window);
        assert!(
            app.get_status_directories_expanded(),
            "directory disclosure must expand"
        );
        click(&window, 816.0, 546.0);
        render(&window);
        assert_eq!(
            CLIPBOARD.with(|value| value.borrow().clone()),
            app.get_config_home().as_str()
        );
        click(&window, 816.0, 600.0);
        render(&window);
        assert_eq!(
            CLIPBOARD.with(|value| value.borrow().clone()),
            app.get_data_path().as_str()
        );
        click(&window, 760.0, 704.0);
        render(&window);
        assert!(!app.get_drawer_open());
        assert_eq!(
            app.get_active_page(),
            5,
            "configure action must navigate to recovery settings"
        );

        let restored = Rc::new(Cell::new(0));
        let restore_calls = restored.clone();
        app.on_restore_config(move |target| {
            assert_eq!(target, "/isolated/codex");
            restore_calls.set(restore_calls.get() + 1);
        });
        app.set_config_managed(true);
        app.set_route_managed(true);
        app.set_drawer_open(true);
        render(&window);
        assert!(
            !app.get_status_directories_expanded(),
            "reopening must collapse directories"
        );
        app.set_busy(true);
        render(&window);
        click(&window, 550.0, 645.0);
        click(&window, 20.0, 200.0);
        assert_eq!(restored.get(), 0, "busy restore must not run");
        assert!(app.get_drawer_open(), "busy outside click must not dismiss");
        app.set_busy(false);
        render(&window);
        click(&window, 550.0, 645.0);
        assert_eq!(
            restored.get(),
            1,
            "restore must target the current Codex home"
        );
        click(&window, 20.0, 200.0);
        render(&window);
        assert!(
            !app.get_drawer_open(),
            "outside click must dismiss when idle"
        );
        println!(
            "Synthetic connection-status previews and interaction checks: {}",
            output.display()
        );
        return Ok(());
    }
    app.set_active_page(1);
    app.set_status_text("离屏预览 · 合成资料".into());
    let icons = provider_icons::PROVIDER_ICONS
        .iter()
        .map(|entry| {
            Ok(ProviderIconRow {
                id: entry.id.into(),
                name: entry.name.into(),
                icon: provider_icons::load_image(entry)?,
                monochrome: entry.monochrome,
            })
        })
        .collect::<Result<Vec<_>, slint::LoadImageError>>()?;
    let openai = icons.iter().find(|row| row.id == "openai").unwrap().clone();
    let grok = icons.iter().find(|row| row.id == "grok").unwrap().clone();
    let custom = icons.iter().find(|row| row.id == "google").unwrap().clone();
    let providers = ModelRc::new(VecModel::from(vec![
        ProviderRow {
            id: "preview-subscription".into(),
            name: "ChatGPT · 默认 OpenAI 头像".into(),
            endpoint: "ChatGPT 订阅连接".into(),
            model_id: "预览模型".into(),
            is_subscription: true,
            binding_label: "预览账号（合成资料）".into(),
            credential_status: "预览".into(),
            icon_id: openai.id.clone(),
            icon: openai.icon.clone(),
            monochrome: openai.monochrome,
            ..Default::default()
        },
        ProviderRow {
            id: "preview-grok".into(),
            name: "Grok".into(),
            is_subscription: true,
            is_grok: true,
            binding_label: "绑定：预览账号（合成资料）".into(),
            credential_status: "Grok OAuth · 发布时核对账号绑定".into(),
            icon_id: grok.id.clone(),
            icon: grok.icon.clone(),
            monochrome: grok.monochrome,
            ..Default::default()
        },
        ProviderRow {
            id: "preview-custom".into(),
            name: "自定义 API · Google 头像".into(),
            endpoint: "https://example.invalid/v1".into(),
            model_id: "preview-model".into(),
            credential_status: "预览".into(),
            icon_id: custom.id.clone(),
            icon: custom.icon.clone(),
            monochrome: custom.monochrome,
            ..Default::default()
        },
    ]));
    app.set_providers(providers.clone());
    app.set_filtered_providers(providers);
    app.set_model_provider_ids(ModelRc::new(VecModel::from(vec![
        "preview-subscription".into(),
        "preview-custom".into(),
    ])));
    app.set_model_provider_options(ModelRc::new(VecModel::from(vec![
        "ChatGPT".into(),
        "合成 API 连接".into(),
    ])));
    app.set_provider_icons(ModelRc::new(VecModel::from(icons.clone())));

    app.set_config_home("/isolated/codex".into());
    app.set_account_status("已保存 2 个账号 · 各上游绑定独立管理".into());
    app.set_auth_status("此目录的官方登录状态尚未检查".into());
    app.set_accounts(ModelRc::new(VecModel::from(vec![
        AccountRow {
            id: "synthetic-account-a".into(),
            label: "synthetic-a@example.invalid".into(),
            workspace: "synthetic-workspace-a".into(),
            bound_provider_count: 1,
            ..Default::default()
        },
        AccountRow {
            id: "synthetic-account-b".into(),
            label: "synthetic-b@example.invalid".into(),
            workspace: "synthetic-workspace-b".into(),
            is_default: true,
            is_active: true,
            ..Default::default()
        },
    ])));
    app.set_models(ModelRc::new(VecModel::from(
        (0..9)
            .map(|index| ModelRow {
                provider_id: if index < 2 {
                    "preview-subscription"
                } else {
                    "preview-custom"
                }
                .into(),
                provider_name: if index < 2 {
                    "ChatGPT"
                } else {
                    "合成 API 连接"
                }
                .into(),
                display_name: match index {
                    0 => "Codex · 主力".into(),
                    1 => "Codex · 轻量".into(),
                    _ => format!("编程模型 {}", index + 1).into(),
                },
                public_id: format!("sx-preview-{}", index + 1).into(),
                upstream_model: format!("synthetic-model-{}", index + 1).into(),
                saved: true,
                ready: true,
                included: index < 3,
                is_subscription: index < 2,
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    )));
    app.set_selected_model_count(3);
    app.set_default_model("sx-preview-1".into());
    app.set_requests(ModelRc::new(VecModel::from(vec![RequestRow {
        time: "2026-09-30 10:42:18".into(),
        route: "sx-preview-3 → 合成 API 连接".into(),
        duration: "1.8 s".into(),
        timing: "总耗时 1800 ms · 响应头 120 ms · 首事件 340 ms · 上游 HTTP 200".into(),
        detail: "合成请求 · 仅用于布局检查".into(),
        status: "正常完成".into(),
        completed: true,
        ..Default::default()
    }])));
    if std::env::args().any(|arg| arg == "--codex-quota") {
        app.set_xai_accounts(ModelRc::default());
        app.set_account_status("已保存 1 个 ChatGPT 账号（合成资料）".into());
        app.set_active_page(1);
        app.set_connections_tab(1);
        for (width, height) in [(1200, 820), (1000, 680)] {
            app.window().set_size(PhysicalSize::new(width, height));
            for dark in [false, true] {
                set_theme(&app, &window, dark);
                let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
                for state in [
                    "ready",
                    "low",
                    "empty",
                    "loading",
                    "refreshing",
                    "failed",
                    "stale",
                    "monthly",
                    "single",
                    "credits-only",
                    "multiple",
                ] {
                    let has_value = !matches!(state, "loading" | "failed");
                    let mut quota = CodexQuotaRow {
                        has_value,
                        primary_window: XaiQuotaRow {
                            has_value: has_value && state != "credits-only",
                            remaining_percent: match state {
                                "low" => 8.0,
                                "empty" => 0.0,
                                _ => 75.0,
                            },
                            period_label: "5 小时额度".into(),
                            reset_label: "2 小时 18 分后重置".into(),
                            reset_detail: "重置于 10月09日 18:00".into(),
                            ..Default::default()
                        },
                        secondary_window: XaiQuotaRow {
                            has_value: has_value && !matches!(state, "single" | "credits-only"),
                            remaining_percent: 62.0,
                            period_label: if state == "monthly" {
                                "30 天额度"
                            } else {
                                "每周额度"
                            }
                            .into(),
                            reset_label: "4 天 21 小时后重置".into(),
                            reset_detail: "重置于 10月14日 11:00".into(),
                            ..Default::default()
                        },
                        credits_label: if has_value {
                            "Codex Credits 余额：62500"
                        } else {
                            ""
                        }
                        .into(),
                        resets_label: if has_value && state != "credits-only" {
                            "可用额度重置 2 次 · 最早到期 10月11日 12:00"
                        } else {
                            ""
                        }
                        .into(),
                        resets_warning: true,
                        updated_label: if has_value { "2 分钟前更新" } else { "" }.into(),
                        loading: matches!(state, "loading" | "refreshing"),
                        ..Default::default()
                    };
                    if matches!(state, "failed" | "stale") {
                        quota.error = "额度查询连接失败，请稍后刷新".into();
                    }
                    let account = AccountRow {
                        id: "synthetic-chatgpt-account".into(),
                        label: "preview@example.invalid".into(),
                        workspace: "synthetic-workspace".into(),
                        is_default: true,
                        is_active: true,
                        bound_provider_count: 1,
                        codex_quota: quota,
                        ..Default::default()
                    };
                    let mut rows = vec![account.clone()];
                    if state == "multiple" {
                        rows.push(AccountRow {
                            id: "second-account".into(),
                            label: "second@example.invalid".into(),
                            is_default: false,
                            is_active: false,
                            ..account
                        });
                    }
                    app.set_accounts(ModelRc::new(VecModel::from(rows)));
                    snapshot(&window, output, &format!("codex-quota-{state}-{suffix}"))?;
                }
            }
        }
        println!("Synthetic ChatGPT quota previews: {}", output.display());
        return Ok(());
    }
    if std::env::args().any(|arg| arg == "--xai-quota") {
        app.set_accounts(ModelRc::new(VecModel::from(Vec::<AccountRow>::new())));
        app.set_account_status("尚未保存 ChatGPT 账号（合成资料）".into());
        app.set_xai_status("已保存 1 个 Grok 账号".into());
        app.set_active_page(1);
        app.set_connections_tab(1);
        for (width, height) in [(1200, 820), (1000, 680)] {
            app.window().set_size(PhysicalSize::new(width, height));
            for dark in [false, true] {
                set_theme(&app, &window, dark);
                let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
                for state in [
                    "ready",
                    "low",
                    "empty",
                    "loading",
                    "refreshing",
                    "failed",
                    "stale",
                    "reauth",
                ] {
                    let mut quota = XaiQuotaRow {
                        has_value: !matches!(state, "loading" | "failed" | "reauth"),
                        remaining_percent: if state == "low" {
                            8.0
                        } else if state == "empty" {
                            0.0
                        } else {
                            98.0
                        },
                        period_label: "每周额度".into(),
                        reset_label: "4 天 21 小时后重置".into(),
                        reset_detail: "重置于 10月14日 11:00".into(),
                        updated_label: if matches!(state, "loading" | "failed" | "reauth") {
                            ""
                        } else {
                            "2 分钟前更新"
                        }
                        .into(),
                        loading: matches!(state, "loading" | "refreshing"),
                        ..Default::default()
                    };
                    if matches!(state, "failed" | "stale") {
                        quota.error = "额度查询连接失败，请稍后刷新".into();
                    }
                    app.set_xai_accounts(ModelRc::new(VecModel::from(vec![AccountRow {
                        id: "synthetic-grok-account".into(),
                        label: "preview@example.invalid".into(),
                        workspace: if state == "reauth" {
                            "凭据失效，请重新登录"
                        } else {
                            "已保存授权"
                        }
                        .into(),
                        is_default: true,
                        requires_reauth: state == "reauth",
                        bound_provider_count: 1,
                        quota,
                        ..Default::default()
                    }])));
                    render(&window);
                    window.dispatch_event(WindowEvent::PointerScrolled {
                        position: slint::LogicalPosition::new(
                            width as f32 - 60.0,
                            height as f32 - 100.0,
                        ),
                        delta_x: 0.0,
                        delta_y: -2000.0,
                    });
                    snapshot(&window, output, &format!("grok-quota-{state}-{suffix}"))?;
                    if state == "low" {
                        let pixels = render(&window);
                        // A short quota fill must remain anchored at the meter's left edge.
                        let pixel = pixels.as_slice()[((height - 95) * width + 296) as usize];
                        let expected = if dark {
                            (225, 184, 121)
                        } else {
                            (148, 102, 33)
                        };
                        assert_eq!((pixel.r, pixel.g, pixel.b), expected);
                    }
                }
            }
        }
        println!("Synthetic Grok quota previews: {}", output.display());
        return Ok(());
    }
    if std::env::args().any(|arg| arg == "--check-progress") {
        let original = app.get_providers().iter().collect::<Vec<_>>();
        for (width, height) in [(1200, 820), (1000, 680)] {
            app.window().set_size(PhysicalSize::new(width, height));
            for dark in [false, true] {
                set_theme(&app, &window, dark);
                let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
                for state in ["checking", "slow", "results", "concurrent"] {
                    let mut rows = original.clone();
                    rows[0].name = "ChatGPT 订阅".into();
                    rows[0].binding_label = "绑定：原生 Codex 登录（合成资料）".into();
                    if state == "results" {
                        rows[0].check_message = "登录状态可读取".into();
                        rows[0].check_detail = "尚未验证实际官方请求权限".into();
                        rows[0].check_elapsed_ms = 2300;
                        rows[1].check_failed = true;
                        rows[1].check_message = "检查失败".into();
                        rows[1].check_detail = "上游模型目录返回 HTTP 401".into();
                        rows[1].check_elapsed_ms = 1600;
                    } else {
                        rows[0].check_id = "synthetic-check-a".into();
                        rows[0].check_stage = "读取登录状态".into();
                        rows[0].check_elapsed_ms = if state == "slow" { 12500 } else { 2300 };
                        if state == "concurrent" {
                            rows[1].check_id = "synthetic-check-b".into();
                            rows[1].check_stage = "读取模型目录".into();
                            rows[1].check_elapsed_ms = 1800;
                        }
                    }
                    let rows = ModelRc::new(VecModel::from(rows));
                    app.set_providers(rows.clone());
                    app.set_filtered_providers(rows);
                    snapshot(&window, output, &format!("check-{state}-{suffix}"))?;
                }
            }
        }
        println!("Synthetic connection-check previews: {}", output.display());
        return Ok(());
    }
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            set_theme(&app, &window, dark);
            app.set_icon_picker_open(false);
            app.set_editor_open(false);
            app.set_subscription_editor_open(false);
            app.set_active_page(0);
            snapshot(&window, output, &format!("workbench-{suffix}"))?;
            app.set_active_page(3);
            snapshot(&window, output, &format!("activity-{suffix}"))?;
            app.set_active_page(5);
            snapshot(&window, output, &format!("settings-{suffix}"))?;
            app.set_active_page(4);
            snapshot(&window, output, &format!("toolbox-{suffix}"))?;
            app.set_active_page(1);
            app.set_connections_tab(0);
            snapshot(&window, output, &format!("connections-{suffix}"))?;
            app.set_connections_tab(1);
            snapshot(&window, output, &format!("accounts-{suffix}"))?;
            window.dispatch_event(WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(width as f32 - 50.0, height as f32 - 80.0),
                delta_x: 0.0,
                delta_y: -1000.0,
            });
            snapshot(&window, output, &format!("accounts-actions-{suffix}"))?;
            app.set_connections_tab(0);
            if layout_only {
                continue;
            }
            // Exercise every icon at the actual picker size, including those below the first screen.
            render_icons(&app, &window, &icons);
            snapshot(&window, output, &format!("providers-{suffix}"))?;

            app.set_edit_id("preview-custom".into());
            app.set_edit_name("自定义 API".into());
            app.set_edit_url("https://example.invalid/v1".into());
            app.set_edit_model("preview-model".into());
            app.set_edit_icon_id(custom.id.clone());
            app.set_edit_icon_name(custom.name.clone());
            app.set_edit_icon(custom.icon.clone());
            app.set_edit_icon_monochrome(custom.monochrome);
            app.set_edit_config_preview("model = \"preview-model\"\n".into());
            app.set_editor_open(true);
            snapshot(&window, output, &format!("custom-editor-{suffix}"))?;

            app.set_filtered_provider_icons(ModelRc::new(VecModel::from(icons.clone())));
            app.set_provider_icon_query("".into());
            app.set_icon_picker_open(true);
            snapshot(&window, output, &format!("picker-{suffix}"))?;

            app.set_provider_icon_query("OpenAI".into());
            let matches = provider_icons::search("OpenAI");
            let search = icons
                .iter()
                .filter(|row| matches.iter().any(|entry| row.id == entry.id))
                .cloned()
                .collect::<Vec<_>>();
            app.set_filtered_provider_icons(ModelRc::new(VecModel::from(search)));
            app.set_edit_icon_id(openai.id.clone());
            app.set_edit_icon_name(openai.name.clone());
            snapshot(&window, output, &format!("search-openai-{suffix}"))?;

            app.set_provider_icon_query("no-match-for-this-icon".into());
            app.set_filtered_provider_icons(ModelRc::new(VecModel::from(Vec::new())));
            snapshot(&window, output, &format!("search-empty-{suffix}"))?;
        }
    }
    if layout_only {
        println!("Synthetic Slint layout previews: {}", output.display());
        return Ok(());
    }
    window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: 2.0 });
    app.window().set_size(PhysicalSize::new(2360, 1600));
    for dark in [false, true] {
        set_theme(&app, &window, dark);
        render_icons(&app, &window, &icons);
    }
    println!("Synthetic software-rendered previews: {}", output.display());
    Ok(())
}
