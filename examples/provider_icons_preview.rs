//! Render synthetic previews with the real Slint UI and no account or configuration access.
//! Run: cargo run --example provider_icons_preview -- /absolute/output/directory
//! Connection cards: add --connections-design (snapshots) or --connections-native (window).
//! Tab transitions: add --connections-motion (frames and interruption checks).
//! Batch model picker: add --batch-models.

#[cfg(target_os = "macos")]
#[path = "../src/macos.rs"]
pub mod macos;

use switchx::ui::{
    AccountRow, AppWindow, BatchModelRow, BatchSummary, CodexQuotaRow, ModelRow, ProviderIconRow,
    ProviderRow, RequestRow, Theme, XaiQuotaRow,
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

fn batch_rows(query: &str) -> Vec<BatchModelRow> {
    let row = |id: &str| BatchModelRow {
        id: id.into(),
        supported: true,
        matched: id.contains(query),
        ..Default::default()
    };
    vec![
        BatchModelRow {
            checked: true,
            source: "预设参数".into(),
            template_context: "1048576".into(),
            template_levels: "low, high, max".into(),
            template_levels_label: "low, high, max".into(),
            ..row("synthetic-coder-pro")
        },
        BatchModelRow {
            checked: true,
            context: "1050000".into(),
            levels: "low, medium, high, xhigh".into(),
            levels_label: "low → xhigh".into(),
            ..row("synthetic-coder-flash")
        },
        BatchModelRow {
            checked: true,
            ..row("synthetic-chat-max")
        },
        row("synthetic-chat-mini"),
        BatchModelRow {
            added: true,
            ..row("synthetic-model-3")
        },
        row("synthetic-vision-plus"),
        BatchModelRow {
            supported: false,
            ..row("synthetic reasoner@beta")
        },
        row("synthetic-embedding-large"),
    ]
}

fn summarize(rows: &[BatchModelRow]) -> BatchSummary {
    let mut summary = BatchSummary::default();
    for row in rows {
        let selected = row.checked && row.supported && !row.added;
        summary.total += 1;
        summary.added += i32::from(row.added);
        summary.selected += i32::from(selected);
        if row.matched {
            summary.matched += 1;
            summary.visible_selectable += i32::from(row.supported && !row.added);
            summary.visible_selected += i32::from(selected);
        }
    }
    summary
}

fn render_batch_models(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    let show = |rows: Vec<BatchModelRow>| {
        app.set_batch_summary(summarize(&rows));
        app.set_batch_models(ModelRc::new(VecModel::from(rows)));
    };
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            set_theme(app, window, dark);
            app.set_active_page(0);
            app.set_model_provider_id("preview-custom".into());
            app.set_model_provider_name("合成 API 连接".into());
            app.set_model_original_id("".into());
            app.set_model_batch_mode(true);
            app.set_model_editor_open(true);
            // Let the open-time reset run before filling the picker.
            render(window);

            app.set_discovery_scope(1);
            app.set_fetching_models(true);
            app.set_discovery_tone(0);
            app.set_discovery_message("正在获取模型列表…".into());
            snapshot(window, output, &format!("batch-loading-{suffix}"))?;

            app.set_fetching_models(false);
            app.set_discovery_tone(1);
            app.set_discovery_message("已获取 8 个模型，其中 1 个已添加".into());
            app.set_batch_context("200000".into());
            app.set_batch_levels("low, medium, high".into());
            app.set_batch_levels_label("low → high".into());
            app.set_batch_reasoning_options(ModelRc::new(VecModel::from(vec![
                "未设置".into(),
                "low".into(),
                "medium".into(),
                "high".into(),
            ])));
            app.set_batch_default("medium".into());
            show(batch_rows(""));
            snapshot(window, output, &format!("batch-list-{suffix}"))?;
            // The parameter chip of the first row expands its inline editor.
            click(window, width as f32 - 178.0, 369.0);
            snapshot(window, output, &format!("batch-expanded-{suffix}"))?;
            click(window, width as f32 - 178.0, 369.0);

            app.set_batch_query("coder".into());
            show(batch_rows("coder"));
            snapshot(window, output, &format!("batch-search-{suffix}"))?;

            app.set_batch_query("no-such-model".into());
            show(batch_rows("no-such-model"));
            snapshot(window, output, &format!("batch-no-match-{suffix}"))?;

            app.set_batch_query("".into());
            show(Vec::new());
            app.set_discovery_tone(3);
            app.set_discovery_message("无法连接上游或连接超时".into());
            snapshot(window, output, &format!("batch-error-{suffix}"))?;

            app.set_model_batch_mode(false);
            app.set_discovery_tone(0);
            app.set_discovery_message("".into());
            snapshot(window, output, &format!("batch-single-{suffix}"))?;
            app.set_model_editor_open(false);
            render(window);

            let models = app.get_models();
            for index in 2..5 {
                let mut model = models.row_data(index).unwrap();
                model.fresh = true;
                models.set_row_data(index, model);
            }
            snapshot(window, output, &format!("batch-fresh-{suffix}"))?;
            for index in 2..5 {
                let mut model = models.row_data(index).unwrap();
                model.fresh = false;
                models.set_row_data(index, model);
            }

            // A new upstream can tick discovered models; ★ marks its default.
            app.set_active_page(1);
            app.set_edit_id("".into());
            app.set_edit_name("合成 API 连接".into());
            app.set_edit_url("https://example.invalid/v1".into());
            app.set_edit_model("synthetic-coder-pro".into());
            app.set_edit_config_preview("model = \"synthetic-coder-pro\"\n".into());
            app.set_editor_open(true);
            render(window);
            app.set_discovery_scope(0);
            app.set_discovery_tone(1);
            app.set_discovery_message("已获取 8 个模型".into());
            let mut rows = batch_rows("");
            for row in &mut rows {
                row.checked = row.id == "synthetic-coder-pro" || row.id == "synthetic-chat-mini";
                row.added = false;
            }
            rows[0].checked = true;
            show(rows);
            snapshot(window, output, &format!("provider-picker-{suffix}"))?;
            app.set_batch_query("chat".into());
            show(
                batch_rows("chat")
                    .into_iter()
                    .map(|row| BatchModelRow {
                        checked: row.id == "synthetic-coder-pro" || row.id == "synthetic-chat-mini",
                        added: false,
                        ..row
                    })
                    .collect(),
            );
            snapshot(window, output, &format!("provider-picker-search-{suffix}"))?;
            app.set_batch_query("".into());
            app.set_editor_open(false);
            app.set_active_page(0);
            render(window);
        }
    }

    // Frames from the staggered arrival and a row being picked, with motion on.
    app.window().set_size(PhysicalSize::new(1200, 820));
    set_theme(app, window, false);
    app.global::<Theme>().set_animations_enabled(true);
    app.set_model_batch_mode(true);
    app.set_model_editor_open(true);
    render(window);
    app.set_discovery_scope(1);
    app.set_discovery_tone(1);
    app.set_discovery_message("已获取 8 个模型，其中 1 个已添加".into());
    let mut rows = batch_rows("");
    rows[2].checked = false;
    show(rows.clone());
    let mut elapsed = 0;
    for at in [16, 120, 260, 420, 1400] {
        frame(at - elapsed);
        elapsed = at;
        snapshot_now(window, output, &format!("batch-motion-arrive-{at}ms"))?;
    }
    rows[2].checked = true;
    let models = app.get_batch_models();
    models.set_row_data(2, rows[2].clone());
    app.set_batch_summary(summarize(&rows));
    let mut elapsed = 0;
    for at in [60, 140, 400] {
        frame(at - elapsed);
        elapsed = at;
        snapshot_now(window, output, &format!("batch-motion-pick-{at}ms"))?;
    }
    app.global::<Theme>().set_animations_enabled(false);
    Ok(())
}

fn frame(step_ms: u64) {
    PREVIEW_TIME.with(|clock| clock.set(clock.get() + Duration::from_millis(step_ms)));
    slint::platform::update_timers_and_animations();
}

// Unlike `snapshot`, keeps the clock where `frame` left it.
fn snapshot_now(
    window: &MinimalSoftwareWindow,
    output: &Path,
    name: &str,
) -> Result<(), Box<dyn Error>> {
    let pixels = render_now(window);
    let mut file = std::io::BufWriter::new(fs::File::create(output.join(format!("{name}.ppm")))?);
    write!(file, "P6\n{} {}\n255\n", pixels.width(), pixels.height())?;
    file.write_all(pixels.as_bytes())?;
    file.flush()?;
    Ok(())
}

fn render_now(window: &MinimalSoftwareWindow) -> SharedPixelBuffer<Rgb8Pixel> {
    slint::platform::update_timers_and_animations();
    let size = WindowAdapter::size(window);
    let mut pixels = SharedPixelBuffer::<Rgb8Pixel>::new(size.width, size.height);
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(pixels.make_mut_slice(), size.width as usize);
    });
    pixels
}

fn render_connections_motion(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    app.set_active_page(1);
    let checks = Rc::new(Cell::new(0));
    let check_count = checks.clone();
    app.on_refresh(move |_| check_count.set(check_count.get() + 1));
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            app.global::<Theme>().set_animations_enabled(false);
            app.set_connections_tab(0);
            set_theme(app, window, dark);
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            app.global::<Theme>().set_animations_enabled(true);
            // Settle the existing provider cards before measuring a tab switch.
            for _ in 0..24 {
                frame(32);
                render_now(window);
            }
            let initial = render_now(window);
            for (target, name) in [(1, "accounts"), (0, "upstreams")] {
                click(window, if target == 1 { 360.0 } else { 260.0 }, 222.0);
                assert_eq!(app.get_connections_tab(), target);
                let mut elapsed = 0;
                for at in [
                    0, 16, 32, 64, 96, 128, 160, 192, 224, 256, 288, 340, 400, 480, 600, 760, 920,
                ] {
                    frame(at - elapsed);
                    elapsed = at;
                    let pixels = render_now(window);
                    // Tab changes keep the title/subtitle still throughout the transition.
                    for y in 60..194 {
                        let start = (y * width as usize + 208) * 3;
                        let end = (y * width as usize + width as usize - 30) * 3;
                        assert_eq!(
                            &pixels.as_bytes()[start..end],
                            &initial.as_bytes()[start..end]
                        );
                    }
                    snapshot_now(window, output, &format!("tabs-{name}-{suffix}-{at:03}ms"))?;
                    if target == 1 && at == 32 {
                        // The still-visible outgoing toolbar must not dispatch actions.
                        click(window, width as f32 - 208.0, 284.0);
                        assert_eq!(checks.get(), 0);
                    }
                }
            }
            // Reverse twice while both pages are still mounted, then resize mid-flight.
            app.set_connections_tab(1);
            render_now(window);
            frame(96);
            snapshot_now(window, output, &format!("tabs-interrupt-before-{suffix}"))?;
            app.set_connections_tab(0);
            render_now(window);
            frame(48);
            app.set_connections_tab(1);
            render_now(window);
            app.window().set_size(PhysicalSize::new(width - 20, height));
            frame(920);
            snapshot_now(window, output, &format!("tabs-interrupt-settled-{suffix}"))?;
            assert_eq!(app.get_connections_tab(), 1);
            app.window().set_size(PhysicalSize::new(width, height));
            // Both motion settings must switch immediately, without a timer tick.
            for reduced in [false, true] {
                app.global::<Theme>().set_animations_enabled(reduced);
                app.global::<Theme>().set_system_reduced_motion(reduced);
                app.set_connections_tab(0);
                let upstreams = render_now(window);
                app.set_connections_tab(1);
                let accounts = render_now(window);
                assert_ne!(upstreams.as_bytes(), accounts.as_bytes());
                frame(1000);
                let settled = render_now(window);
                assert_eq!(
                    accounts.as_bytes(),
                    settled.as_bytes(),
                    "Motion-off content must be complete immediately"
                );
                snapshot_now(window, output, &format!("tabs-static-{reduced}-{suffix}"))?;
            }
            app.global::<Theme>().set_system_reduced_motion(false);
        }
    }
    println!(
        "Synthetic tab transitions and interruption checks: {}",
        output.display()
    );
    Ok(())
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
    let native_connections = std::env::args().any(|arg| arg == "--connections-native");
    if native_connections {
        #[cfg(target_os = "macos")]
        macos::configure_window()?;
    } else {
        slint::platform::set_platform(Box::new(PreviewPlatform(window.clone())))?;
    }
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
    if native_connections || std::env::args().any(|arg| arg == "--connections-design") {
        let mut rows: Vec<_> = providers.iter().collect();
        rows[0].name = "ChatGPT 订阅".into();
        rows[0].binding_label = "绑定：原生 Codex 登录".into();
        rows[0].credential_status = "订阅凭据由绑定账号提供".into();
        rows[1].binding_label = "绑定：Personal · 合成账号".into();
        rows[2].name = "Gemini API".into();
        rows[2].credential_status = "API Key 已保存 · 尚未检查".into();
        app.set_action_message("连接卡片预览 · 合成资料".into());
        let model = Rc::new(VecModel::from(rows.clone()));
        app.set_providers(model.clone().into());
        app.set_filtered_providers(model.clone().into());
        if native_connections {
            app.set_native_titlebar_overlay(cfg!(target_os = "macos"));
            #[cfg(target_os = "macos")]
            app.global::<Theme>()
                .set_system_reduced_motion(macos::prefers_reduced_motion());
            app.global::<Theme>().set_animations_enabled(true);
            app.invoke_set_appearance(true);
            app.on_check_provider(move |id| {
                if let Some(index) = model.iter().position(|row| row.id == id) {
                    let mut row = model.row_data(index).unwrap();
                    row.check_id = "synthetic-check".into();
                    row.check_stage = "检查凭据".into();
                    model.set_row_data(index, row);
                    let model = model.clone();
                    slint::Timer::single_shot(Duration::from_millis(1800), move || {
                        let mut row = model.row_data(index).unwrap();
                        row.check_id = "".into();
                        row.check_message = "合成检查完成".into();
                        row.check_elapsed_ms = 1800;
                        model.set_row_data(index, row);
                    });
                }
            });
            return Ok(app.run()?);
        }
        app.show()?;
        for (width, height) in [(1200, 820), (1000, 680)] {
            app.window().set_size(PhysicalSize::new(width, height));
            for dark in [false, true] {
                set_theme(&app, &window, dark);
                let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
                model.set_vec(rows.clone());
                snapshot(&window, output, &format!("connections-ready-{suffix}"))?;
                window.dispatch_event(WindowEvent::PointerMoved {
                    position: slint::LogicalPosition::new(500.0, 310.0),
                });
                snapshot(&window, output, &format!("connections-hover-{suffix}"))?;
                window.dispatch_event(WindowEvent::PointerExited);
                let mut pending = rows[0].clone();
                pending.check_id = "synthetic-check".into();
                pending.check_stage = "检查凭据".into();
                pending.check_elapsed_ms = 12500;
                model.set_row_data(0, pending);
                let mut failed = rows[1].clone();
                failed.check_failed = true;
                failed.check_message = "检查失败".into();
                failed.check_detail = "授权已失效，请在管理连接中重新绑定账号。".into();
                model.set_row_data(1, failed);
                snapshot(&window, output, &format!("connections-checks-{suffix}"))?;
                let mut long = rows[2].clone();
                long.name = "团队的 API 连接 · 一个需要截断的非常长的连接名称".into();
                long.endpoint =
                    "https://example.invalid/a/very/long/path/to/an/api/endpoint/v1".into();
                long.auth_error =
                    "无法读取凭据。请打开管理连接重新填写 API Key，然后再次检查。".into();
                model.set_vec(vec![long]);
                snapshot(&window, output, &format!("connections-long-{suffix}"))?;
                app.set_config_managed(true);
                snapshot(&window, output, &format!("connections-managed-{suffix}"))?;
                app.set_config_managed(false);
            }
        }
        println!("Synthetic connection-card previews: {}", output.display());
        return Ok(());
    }
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
    if std::env::args().any(|arg| arg == "--accounts-design" || arg == "--connections-motion") {
        app.set_active_page(1);
        app.set_connections_tab(1);
        app.set_action_message("合成账号界面检查；未访问实际账号或 Codex 配置".into());
        let chatgpt_account = AccountRow {
            id: "design-chatgpt".into(),
            label: "personal@example.invalid".into(),
            initial: "P".into(),
            workspace: "synthetic-workspace-0001".into(),
            is_default: true,
            is_active: true,
            codex_quota: CodexQuotaRow {
                has_value: true,
                primary_window: XaiQuotaRow {
                    has_value: true,
                    remaining_percent: 78.0,
                    period_label: "每周额度".into(),
                    reset_label: "5 天 17 小时后重置".into(),
                    reset_detail: "重置于 10月15日 09:30".into(),
                    ..Default::default()
                },
                resets_label: "可用额度重置 3 次 · 最早到期 10月23日 04:26".into(),
                updated_label: "刚刚更新".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let grok_account = AccountRow {
            id: "design-grok".into(),
            label: "grok@example.invalid".into(),
            initial: "G".into(),
            workspace: "已保存授权".into(),
            is_default: true,
            bound_provider_count: 1,
            bound_provider_names: "Grok 订阅连接".into(),
            quota: XaiQuotaRow {
                has_value: true,
                remaining_percent: 97.0,
                period_label: "每周额度".into(),
                reset_label: "4 天 19 小时后重置".into(),
                reset_detail: "重置于 10月14日 11:36".into(),
                updated_label: "刚刚更新".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        if std::env::args().any(|arg| arg == "--connections-motion") {
            app.set_accounts(ModelRc::new(VecModel::from(vec![chatgpt_account])));
            app.set_xai_accounts(ModelRc::new(VecModel::from(vec![grok_account])));
            return render_connections_motion(&app, &window, output);
        }
        let writes = Rc::new(Cell::new(0));
        let removals = Rc::new(Cell::new(0));
        let write_count = writes.clone();
        let removal_count = removals.clone();
        app.on_account_action(move |action, id| {
            if action == 3 || action == 4 {
                assert_eq!(id, "design-chatgpt");
                if action == 3 {
                    write_count.set(write_count.get() + 1);
                } else {
                    removal_count.set(removal_count.get() + 1);
                }
            }
        });
        for (width, height) in [(1200, 820), (1000, 680)] {
            app.window().set_size(PhysicalSize::new(width, height));
            for dark in [false, true] {
                set_theme(&app, &window, dark);
                app.set_accounts(ModelRc::new(VecModel::from(vec![chatgpt_account.clone()])));
                app.set_xai_accounts(ModelRc::new(VecModel::from(vec![grok_account.clone()])));
                app.set_expanded_chatgpt_account_id("".into());
                app.set_expanded_grok_account_id("".into());
                app.set_native_login_expanded(false);
                let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
                snapshot(&window, output, &format!("accounts-ready-{suffix}"))?;
                app.set_expanded_chatgpt_account_id(chatgpt_account.id.clone());
                app.set_expanded_grok_account_id(grok_account.id.clone());
                snapshot(&window, output, &format!("accounts-details-{suffix}"))?;
                app.set_accounts(ModelRc::new(VecModel::from(vec![chatgpt_account.clone()])));
                assert_eq!(app.get_expanded_chatgpt_account_id(), chatgpt_account.id);
                app.set_expanded_chatgpt_account_id("".into());
                app.set_expanded_grok_account_id("".into());
                app.set_native_login_expanded(true);
                snapshot(&window, output, &format!("accounts-native-{suffix}"))?;
                app.set_native_login_expanded(false);
                app.set_account_login_confirm_label(chatgpt_account.label.clone());
                app.set_account_login_confirm_id(chatgpt_account.id.clone());
                app.set_account_login_confirm_open(true);
                snapshot(&window, output, &format!("accounts-confirm-{suffix}"))?;
                window.dispatch_event(WindowEvent::KeyPressed {
                    text: slint::platform::Key::Escape.into(),
                });
                window.dispatch_event(WindowEvent::KeyReleased {
                    text: slint::platform::Key::Escape.into(),
                });
                assert!(!app.get_account_login_confirm_open());
            }
        }
        app.window().set_size(PhysicalSize::new(1200, 820));
        app.global::<Theme>().set_animations_enabled(false);
        app.set_native_login_expanded(false);
        render(&window);
        click(&window, 1126.0, 348.0);
        snapshot(&window, output, "accounts-menu-dark-1200x820")?;
        click(&window, 1000.0, 474.0);
        render(&window);
        assert!(app.get_account_login_confirm_open());
        assert_eq!(writes.get(), 0, "Selecting login must only open its review");
        click(&window, 720.0, 488.0);
        render(&window);
        assert_eq!(writes.get(), 1);
        assert!(!app.get_account_login_confirm_open());

        app.global::<Theme>().set_animations_enabled(true);
        app.set_account_removing_id("design-chatgpt".into());
        render(&window);
        assert_eq!(removals.get(), 1);
        render(&window);
        assert_eq!(removals.get(), 1, "Confirmed removal must dispatch once");
        app.set_account_removing_id("".into());
        println!("Synthetic account design previews: {}", output.display());
        return Ok(());
    }
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
                        let expected = if dark {
                            (225, 184, 121)
                        } else {
                            (148, 102, 33)
                        };
                        let meter = (255..height - 58)
                            .flat_map(|y| (300..330).map(move |x| (y * width + x) as usize))
                            .find(|&offset| {
                                pixels.as_slice()[offset..offset + 8]
                                    .iter()
                                    .all(|pixel| (pixel.r, pixel.g, pixel.b) == expected)
                            })
                            .expect("low quota fill must remain at the track's left edge");
                        let track = pixels.as_slice()[meter + 80];
                        let expected_track = if dark { (51, 58, 53) } else { (228, 231, 225) };
                        assert_eq!((track.r, track.g, track.b), expected_track);
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
    if std::env::args().any(|arg| arg == "--batch-models") {
        render_batch_models(&app, &window, output)?;
        println!("Synthetic batch model previews: {}", output.display());
        return Ok(());
    }
    if std::env::args().any(|arg| arg == "--route-launch") {
        app.set_route_running(true);
        app.set_config_managed(true);
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
