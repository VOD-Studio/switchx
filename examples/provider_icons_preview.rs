//! Render synthetic previews with the real Slint UI and no account or configuration access.
//! Run: cargo run --example provider_icons_preview -- /absolute/output/directory
//! Connection cards: add --connections-design (snapshots) or --connections-native (window).
//! Tab transitions: add --connections-motion (frames and interruption checks).
//! Sidebar navigation: add --sidebar-motion (frames and settle checks).
//! macOS titlebar spacing: add --native-titlebar-overlay to software previews.
//! Batch model picker: add --batch-models.
//! Connection directory/workbench: add --connection-workbench.
//! Shared workspace menu and operation feedback: add --workspace-feedback.
//! Activity timeline: add --activity-design (entry, arrivals, pause, details, filter).
//! API connection editor: add --provider-editor-design.
//! Add-connection navigation: add --connection-picker-motion (frames and back actions).
//! ChatGPT subscription editor: add --subscription-editor-design.
//! Reasoning multi-select: add --reasoning-picker (themes, sizes, and motion).
//! Shared configuration: add --common-config-design or --common-config-native.

#[cfg(target_os = "macos")]
#[path = "../src/macos.rs"]
pub mod macos;

use switchx::ui::{
    AccountRow, AppWindow, BatchModelRow, BatchSummary, CodeSpan, CodexQuotaRow,
    ConnectionModelRow, ModelRow, ProviderIconRow, ProviderPresetRow, ProviderRow, RequestBar,
    RequestRow, RequestStats, SyntaxHighlighting, Theme, XaiQuotaRow,
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
            click(window, width as f32 - 178.0, 329.0);
            snapshot(window, output, &format!("batch-expanded-{suffix}"))?;
            click(window, width as f32 - 178.0, 329.0);

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
                click(window, if target == 1 { 360.0 } else { 260.0 }, 182.0);
                assert_eq!(app.get_connections_tab(), target);
                let mut elapsed = 0;
                for at in [
                    0, 16, 32, 64, 96, 128, 160, 192, 224, 256, 288, 340, 400, 480, 600, 760, 920,
                ] {
                    frame(at - elapsed);
                    elapsed = at;
                    let pixels = render_now(window);
                    // Tab changes keep the title/subtitle still throughout the transition.
                    for y in 20..154 {
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
                        click(window, width as f32 - 208.0, 244.0);
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

fn render_reasoning_picker(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    app.set_active_page(1);
    app.set_connection_models_name("ChatGPT 订阅".into());
    app.set_connection_models_subscription(true);
    app.set_connection_models_open(true);
    let weak = app.as_weak();
    app.on_connection_model_edited(move || {
        if let Some(app) = weak.upgrade() {
            app.set_connection_models_dirty(true);
        }
    });
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            app.global::<Theme>().set_animations_enabled(false);
            set_theme(app, window, dark);
            app.set_connection_models(ModelRc::new(VecModel::from(
                (0..6)
                    .map(|index| ConnectionModelRow {
                        public_id: format!("sx-synthetic-{index}").into(),
                        display_name: format!("coder-{}/ChatGPT 订阅", index + 1).into(),
                        upstream_model: format!("coder-{}", index + 1).into(),
                        context_window: "272000".into(),
                        reasoning_levels: "low, medium, high, xhigh, max".into(),
                        default_reasoning: "high".into(),
                        ..Default::default()
                    })
                    .collect::<Vec<_>>(),
            )));
            app.set_connection_models_dirty(false);
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            snapshot(window, output, &format!("reasoning-directory-{suffix}"))?;
            app.global::<Theme>().set_animations_enabled(true);
            click(window, width as f32 - 140.0, 338.0);
            play(
                window,
                output,
                &format!("reasoning-open-{suffix}"),
                &(0..=20).map(|frame| frame * 16).collect::<Vec<_>>(),
            )?;
            snapshot(window, output, &format!("reasoning-picker-{suffix}"))?;
            click(window, 300.0, 600.0);
            play(
                window,
                output,
                &format!("reasoning-close-{suffix}"),
                &(0..=10).map(|frame| frame * 16).collect::<Vec<_>>(),
            )?;
        }
    }
    println!("Reasoning picker previews saved to {}", output.display());
    Ok(())
}

fn render_connection_workbench(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    let make_provider =
        |id: &str, name: &str, icon_id: &str, subscription: bool, grok: bool, selected: bool| {
            let icon = provider_icons::icon(icon_id).unwrap();
            let models: Vec<_> = ["coder-pro", "coder-mini", "reasoner-max", "chat-flash"]
                .iter()
                .enumerate()
                .map(|(index, model)| ModelRow {
                    provider_id: id.into(),
                    provider_name: name.into(),
                    upstream_model: (*model).into(),
                    public_id: format!("sx-{id}-{index}").into(),
                    display_name: format!("{model}/{name}").into(),
                    context_window: "1000000".into(),
                    reasoning_levels: "low, medium, high, max".into(),
                    saved: true,
                    ready: true,
                    included: selected,
                    is_subscription: subscription,
                    ..Default::default()
                })
                .collect();
            ProviderRow {
                id: id.into(),
                name: name.into(),
                icon: slint::Image::load_from_svg_data(icon.data).unwrap(),
                monochrome: icon.monochrome,
                endpoint: "https://example.invalid/v1".into(),
                base_url: "https://example.invalid/v1".into(),
                binding_label: "绑定：工作账号".into(),
                is_subscription: subscription,
                is_grok: grok,
                model_count: 4,
                ready_model_count: 4,
                selected_model_count: if selected { 4 } else { 0 },
                models: ModelRc::new(VecModel::from(models)),
                ..Default::default()
            }
        };
    let providers = vec![
        make_provider("chatgpt", "ChatGPT · 工作", "openai", true, false, true),
        make_provider("grok", "Grok", "grok", true, true, false),
        make_provider("podlink", "Podlink", "anthropic", false, false, false),
    ];
    app.set_providers(ModelRc::new(VecModel::from(providers.clone())));
    app.set_models(ModelRc::new(VecModel::from(
        providers
            .iter()
            .flat_map(|provider| provider.models.iter())
            .collect::<Vec<_>>(),
    )));
    app.set_selected_model_count(4);
    app.set_selected_connection_count(1);
    app.set_default_model_label("coder-pro/ChatGPT · 工作".into());
    app.set_selectable_model_count(12);
    app.set_default_model("sx-chatgpt-0".into());
    app.set_config_home("/isolated/codex".into());
    app.set_status_text("合成连接与模型 · 未读取账号或修改 Codex".into());
    let toggles = Rc::new(Cell::new(0));
    let calls = toggles.clone();
    app.on_select_connection(move |id, selected| {
        assert_eq!(id, "chatgpt");
        assert!(!selected);
        calls.set(calls.get() + 1);
    });
    let weak = app.as_weak();
    app.on_connection_model_edited(move || {
        if let Some(app) = weak.upgrade() {
            app.set_connection_models_dirty(true);
        }
    });
    let saves = Rc::new(Cell::new(0));
    let save_calls = saves.clone();
    app.on_save_connection_models(move || save_calls.set(save_calls.get() + 1));
    let weak = app.as_weak();
    app.on_edit_connection(move |id| {
        let app = weak.upgrade().unwrap();
        assert_eq!(id, "chatgpt");
        app.set_busy(true);
        app.set_connection_models_open(false);
        app.set_subscription_editor_open(true);
    });
    let weak = app.as_weak();
    app.on_begin_connection_models(move |id| {
        let app = weak.upgrade().unwrap();
        assert_eq!(id, "chatgpt");
        app.set_busy(true);
        app.set_subscription_editor_open(false);
        app.set_connection_models_open(true);
    });
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            set_theme(app, window, dark);
            app.set_active_page(0);
            snapshot(window, output, &format!("workbench-connections-{suffix}"))?;
            if width == 1200 && !dark {
                click(window, 1090.0, 290.0);
                render(window);
                assert_eq!(
                    toggles.get(),
                    1,
                    "whole-connection selection must be reachable"
                );
            }
            let expand_x = width as f32 - 100.0;
            click(window, expand_x, 350.0);
            snapshot(window, output, &format!("workbench-expanded-{suffix}"))?;
            let pixels = render(window);
            let sample_x = width as usize - 430;
            let pixel = |y: usize| pixels.as_slice()[y * width as usize + sample_x];
            // Empty space beside the model label must stay clear through the
            // row midpoint; the separator belongs below all row content.
            for y in 401..411 {
                assert_eq!(
                    pixel(y),
                    pixel(399),
                    "divider crosses the model row at {suffix}, y={y}"
                );
            }
            if height == 820 {
                assert_ne!(
                    pixel(437),
                    pixel(399),
                    "row boundary separator is missing at {suffix}"
                );
            }
            click(window, expand_x, 350.0);
            if width == 1200 && dark {
                click(window, 860.0, 186.0);
                snapshot(window, output, &format!("workbench-direct-{suffix}"))?;
                click(window, 785.0, 186.0);
            }
            app.set_active_page(1);
            app.set_connection_models_id("podlink".into());
            app.set_connection_models_name("Podlink".into());
            app.set_connection_models_open(true);
            app.set_connection_models(ModelRc::new(VecModel::from(
                (0..4)
                    .map(|index| ConnectionModelRow {
                        public_id: format!("sx-podlink-{index}").into(),
                        display_name: [
                            "Claude Opus · Podlink",
                            "Claude Sonnet · Podlink",
                            "Coder Pro · Podlink",
                            "Reasoner Max · Podlink",
                        ][index]
                            .into(),
                        upstream_model: [
                            "claude-opus",
                            "claude-sonnet",
                            "coder-pro",
                            "reasoner-max",
                        ][index]
                            .into(),
                        context_window: "1000000".into(),
                        reasoning_levels: "low, medium, high, max".into(),
                        default_reasoning: "high".into(),
                        ..Default::default()
                    })
                    .collect::<Vec<_>>(),
            )));
            snapshot(window, output, &format!("connection-directory-{suffix}"))?;
            assert!(app.get_connection_models_open());
            app.set_busy(true);
            app.set_discovery_scope(2);
            app.set_fetching_models(true);
            app.set_discovery_message("正在获取模型列表…".into());
            app.global::<Theme>().set_animations_enabled(true);
            play(
                window,
                output,
                &format!("connection-directory-loading-{suffix}"),
                &[640, 1280, 1424],
            )?;
            assert_ne!(
                fs::read(output.join(format!("connection-directory-loading-{suffix}-1280ms.ppm")))?,
                fs::read(output.join(format!("connection-directory-loading-{suffix}-1424ms.ppm")))?,
                "model discovery feedback must keep moving at {suffix}"
            );
            app.global::<Theme>().set_animations_enabled(false);
            app.set_fetching_models(false);
            app.set_discovery_message("".into());
            app.set_connection_models_saving(true);
            snapshot(
                window,
                output,
                &format!("connection-directory-saving-{suffix}"),
            )?;
            app.set_connection_models_saving(false);
            app.set_busy(false);
            render(window);
            if width == 1200 && dark {
                click(window, 1132.0, 338.0);
                render(window);
                assert!(
                    app.get_connection_models().row_data(0).unwrap().removing,
                    "directory removal must stay in the draft"
                );
                assert!(app.get_connection_models_dirty());
                snapshot(window, output, "connection-directory-remove-dark")?;
                click(window, 1110.0, 325.0);
                render(window);
                assert!(
                    !app.get_connection_models().row_data(0).unwrap().removing,
                    "removal must be undoable before save"
                );
                click(window, 1095.0, 775.0);
                render(window);
                assert_eq!(saves.get(), 1, "directory save must remain reachable");
            }
            app.set_connection_models_dirty(false);
            app.set_connection_models_open(false);
            app.set_subscription_id("chatgpt".into());
            app.set_subscription_name("ChatGPT · 工作".into());
            app.set_subscription_account_ids(ModelRc::new(VecModel::from(vec!["".into()])));
            app.set_subscription_account_options(ModelRc::new(VecModel::from(vec![
                "跟随 Codex 登录".into(),
            ])));
            app.set_edit_icon(providers[0].icon.clone());
            app.set_subscription_binding_label("跟随所选 Codex 目录的登录。".into());
            app.set_subscription_editor_open(true);
            snapshot(window, output, &format!("subscription-clean-{suffix}"))?;
            app.set_connection_models_id("chatgpt".into());
            app.set_connection_models_name("ChatGPT · 工作".into());
            app.set_connection_models_subscription(true);
            let left = width as f32 - 820.0 + 26.0;
            for directory in [true, false, true, false] {
                click(window, left + if directory { 240.0 } else { 80.0 }, 106.0);
                // Match the asynchronous editor load: focus moves while controls
                // are disabled, then the new editor becomes ready.
                render(window);
                click(window, left + if directory { 80.0 } else { 240.0 }, 106.0);
                assert_eq!(app.get_connection_models_open(), directory);
                app.set_busy(false);
                render(window);
                assert_eq!(app.get_connection_models_open(), directory);
                assert_eq!(app.get_subscription_editor_open(), !directory);
                snapshot(
                    window,
                    output,
                    &format!(
                        "connection-tabs-{}-{suffix}",
                        if directory { "models" } else { "settings" }
                    ),
                )?;
            }
            for (directory, key) in [
                (true, slint::platform::Key::RightArrow),
                (false, slint::platform::Key::LeftArrow),
            ] {
                // Focus the current segment, then select its neighbor by keyboard.
                click(window, left + if directory { 80.0 } else { 240.0 }, 106.0);
                window.dispatch_event(WindowEvent::KeyPressed { text: key.into() });
                window.dispatch_event(WindowEvent::KeyReleased { text: key.into() });
                render(window);
                app.set_busy(false);
                render(window);
                assert_eq!(app.get_connection_models_open(), directory);
                assert_eq!(app.get_subscription_editor_open(), !directory);
            }
            app.set_config_managed(true);
            click(window, left + 240.0, 106.0);
            render(window);
            assert!(app.get_subscription_editor_open());
            assert!(!app.get_connection_models_open());
            snapshot(window, output, &format!("connection-tabs-managed-{suffix}"))?;
            app.set_config_managed(false);
            app.set_subscription_editor_open(false);
        }
    }
    app.window().set_size(PhysicalSize::new(1200, 820));
    set_theme(app, window, true);
    app.set_active_page(0);
    app.global::<Theme>().set_animations_enabled(true);
    render(window);
    click(window, 1100.0, 418.0);
    for index in 0..12 {
        frame(30);
        snapshot_now(window, output, &format!("workbench-motion-{index:02}"))?;
    }
    assert_ne!(
        fs::read(output.join("workbench-motion-00.ppm"))?,
        fs::read(output.join("workbench-motion-11.ppm"))?,
        "disclosure must animate between captured frames"
    );
    app.global::<Theme>().set_system_reduced_motion(true);
    snapshot(window, output, "workbench-reduced-motion")?;
    Ok(())
}

fn render_workspace_feedback(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    let switches = Rc::new(Cell::new(0));
    let calls = switches.clone();
    app.on_subscription_action(move |action| {
        assert_eq!(
            action, 4,
            "workspace action must keep the existing review flow"
        );
        calls.set(calls.get() + 1);
    });
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            set_theme(app, window, dark);
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            app.set_active_page(0);
            app.set_error_code("".into());
            app.set_action_message("".into());
            app.set_notification_open(false);
            snapshot(window, output, &format!("workspace-workbench-{suffix}"))?;
            // The top-bar workspace entry is removed; exercise the menu directly.
            app.set_workspace_open(true);
            assert!(app.get_workspace_open());
            snapshot(window, output, &format!("workspace-menu-{suffix}"))?;
            click(window, 490.0, 270.0);
            render(window);
            assert_eq!(app.get_active_page(), 5);
            assert!(!app.get_workspace_open());
            app.set_workspace_open(true);
            render(window);
            click(window, 490.0, 314.0);
            render(window);
            assert!(!app.get_workspace_open());
            app.set_active_page(0);
            app.set_action_message("已取消发布预览".into());
            render(window);
            assert!(app.get_notification_open());
            snapshot(window, output, &format!("workspace-notice-{suffix}"))?;
            app.set_active_page(1);
            snapshot(
                window,
                output,
                &format!("workspace-notice-connections-{suffix}"),
            )?;
            app.set_error_code("synthetic_conflict".into());
            app.set_error_message("目标配置已在外部修改，操作尚未完成。".into());
            app.set_error_action("请在配置与恢复中检查目标文件，再重新预览。".into());
            snapshot(window, output, &format!("workspace-error-{suffix}"))?;
            app.set_notification_open(false);
        }
    }
    assert_eq!(switches.get(), 4);

    // Busy and either OAuth login keep API switching disabled in the relocated menu.
    for pending in 0..3 {
        app.set_busy(pending == 0);
        app.set_chatgpt_login_pending(pending == 1);
        app.set_xai_pending(pending == 2);
        app.set_workspace_open(true);
        render(window);
        click(window, 490.0, 314.0);
        render(window);
        assert_eq!(switches.get(), 4);
        assert!(app.get_workspace_open());
        app.set_workspace_open(false);
    }
    app.set_busy(false);
    app.set_chatgpt_login_pending(false);
    app.set_xai_pending(false);

    app.window().set_size(PhysicalSize::new(1200, 820));
    app.set_error_code("".into());
    app.set_error_message("".into());
    app.set_error_action("".into());
    app.set_action_message("已取消发布预览".into());
    render(window);
    click(window, 1148.0, 99.0);
    assert!(
        !app.get_notification_open(),
        "toast close action must remain reachable"
    );
    click(window, 1158.0, 29.0);
    render(window);
    click(window, 1110.0, 161.0);
    render(window);
    assert!(
        app.get_drawer_open(),
        "feedback details must open connection status"
    );
    app.set_drawer_open(false);
    render(window);
    app.set_notification_open(false);
    app.set_feedback_sequence(app.get_feedback_sequence() + 1);
    render(window);
    assert!(
        app.get_notification_open(),
        "identical feedback must replay"
    );
    window.dispatch_event(WindowEvent::PointerMoved {
        position: slint::LogicalPosition::new(850.0, 125.0),
    });
    for _ in 0..65 {
        frame(100);
    }
    assert!(
        app.get_notification_open(),
        "hover must pause auto dismissal"
    );
    window.dispatch_event(WindowEvent::PointerExited);
    for _ in 0..65 {
        frame(100);
    }
    assert!(
        !app.get_notification_open(),
        "info must close after six seconds"
    );
    click(window, 1158.0, 29.0);
    render(window);
    assert!(
        app.get_notification_open(),
        "toolbar must reopen the latest feedback"
    );
    for _ in 0..65 {
        frame(100);
    }
    assert!(
        app.get_notification_open(),
        "manually opened feedback stays readable"
    );
    window.dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Escape.into(),
    });
    assert!(!app.get_notification_open());
    app.set_error_code("synthetic_conflict".into());
    app.set_error_message("合成配置冲突".into());
    for _ in 0..65 {
        frame(100);
    }
    assert!(
        app.get_notification_open(),
        "errors must persist until dismissed"
    );
    app.set_notification_open(false);
    app.set_error_code("".into());

    // The same message waits for a modal to close instead of covering its actions.
    app.set_drawer_open(true);
    app.set_feedback_sequence(app.get_feedback_sequence() + 1);
    for _ in 0..65 {
        frame(100);
    }
    assert!(app.get_notification_open());
    app.set_drawer_open(false);
    for _ in 0..65 {
        frame(100);
    }
    assert!(!app.get_notification_open());

    app.set_active_page(0);
    app.global::<Theme>().set_animations_enabled(true);
    render(window);
    app.set_workspace_open(true);
    let mut elapsed = 0;
    for at in [16, 48, 80, 120, 160, 200, 240, 280, 340, 420] {
        frame(at - elapsed);
        elapsed = at;
        snapshot_now(window, output, &format!("workspace-motion-open-{at}ms"))?;
    }
    assert_ne!(
        fs::read(output.join("workspace-motion-open-16ms.ppm"))?,
        fs::read(output.join("workspace-motion-open-420ms.ppm"))?,
        "the workspace menu must animate between captured frames"
    );
    window.dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Escape.into(),
    });
    assert!(!app.get_workspace_open());
    frame(420);
    app.set_feedback_sequence(app.get_feedback_sequence() + 1);
    let mut elapsed = 0;
    for at in [16, 48, 80, 120, 160, 200, 240, 280, 340, 420, 520] {
        frame(at - elapsed);
        elapsed = at;
        snapshot_now(window, output, &format!("workspace-motion-notice-{at}ms"))?;
    }
    assert_ne!(
        fs::read(output.join("workspace-motion-notice-16ms.ppm"))?,
        fs::read(output.join("workspace-motion-notice-520ms.ppm"))?,
        "operation feedback must animate between captured frames"
    );
    app.global::<Theme>().set_system_reduced_motion(true);
    app.set_notification_open(false);
    app.set_workspace_open(true);
    snapshot(window, output, "workspace-reduced-motion")?;
    click(window, 760.0, 400.0);
    assert!(
        !app.get_workspace_open(),
        "outside click must dismiss the menu"
    );
    println!(
        "Synthetic workspace and feedback previews: {}",
        output.display()
    );
    Ok(())
}

// Sidebar item centers without a native titlebar overlay.
const SIDEBAR_ITEMS: [(i32, f32); 5] = [(0, 111.0), (1, 157.0), (3, 203.0), (4, 275.0), (5, 321.0)];

fn sidebar_pixels(pixels: &SharedPixelBuffer<Rgb8Pixel>) -> Vec<u8> {
    let row = pixels.width() as usize * 3;
    pixels
        .as_bytes()
        .chunks(row)
        .flat_map(|line| line[..180 * 3].iter().copied())
        .collect()
}

// Longest vertical brand run in the marker column just inside the focus ring.
fn marker_extent(pixels: &SharedPixelBuffer<Rgb8Pixel>, brand: slint::Color) -> usize {
    let width = pixels.width() as usize;
    let (mut longest, mut run) = (0, 0);
    for y in 0..pixels.height() as usize {
        let pixel = pixels.as_slice()[y * width + 16];
        let close = pixel.r.abs_diff(brand.red()) <= 8
            && pixel.g.abs_diff(brand.green()) <= 8
            && pixel.b.abs_diff(brand.blue()) <= 8;
        run = if close { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest
}

fn render_sidebar_motion(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    let center = |page: i32| {
        SIDEBAR_ITEMS
            .iter()
            .find(|(item, _)| *item == page)
            .map(|(_, y)| {
                *y + if app.get_native_titlebar_overlay() {
                    18.0
                } else {
                    0.0
                }
            })
            .unwrap()
    };
    let path = [1, 5, 0, 4, 3, 0];
    app.window().set_size(PhysicalSize::new(1200, 820));
    for dark in [false, true] {
        let suffix = if dark { "dark" } else { "light" };
        app.global::<Theme>().set_animations_enabled(false);
        app.set_active_page(0);
        set_theme(app, window, dark);
        let brand = app.global::<Theme>().get_brand();
        let mut settled = std::collections::HashMap::new();
        for page in path {
            click(window, 90.0, center(page));
            assert_eq!(app.get_active_page(), page);
            settled.insert(page, render(window));
        }
        let rest = marker_extent(&settled[&0], brand);

        app.global::<Theme>().set_animations_enabled(true);
        for _ in 0..24 {
            frame(32);
            render_now(window);
        }
        let mut from = 0;
        for page in path {
            click(window, 90.0, center(page));
            assert_eq!(app.get_active_page(), page);
            let (mut elapsed, mut stretch) = (0, 0);
            for at in [
                0, 16, 32, 48, 64, 96, 128, 160, 200, 240, 300, 340, 420, 600,
            ] {
                frame(at - elapsed);
                elapsed = at;
                let pixels = render_now(window);
                let sidebar = sidebar_pixels(&pixels);
                if at == 64 {
                    assert_ne!(sidebar, sidebar_pixels(&settled[&from]));
                    assert_ne!(
                        sidebar,
                        sidebar_pixels(&settled[&page]),
                        "the selection must still be travelling"
                    );
                }
                stretch = stretch.max(marker_extent(&pixels, brand));
                snapshot_now(
                    window,
                    output,
                    &format!("sidebar-{from}-to-{page}-{suffix}-{at:03}ms"),
                )?;
            }
            assert!(
                stretch >= rest + 4,
                "the marker must stretch toward page {page}: {stretch} vs {rest}"
            );
            assert_eq!(
                sidebar_pixels(&render_now(window)),
                sidebar_pixels(&settled[&page]),
                "the selection must settle exactly on page {page}"
            );
            from = page;
        }

        // Retargeting mid-flight still settles on the latest page.
        click(window, 90.0, center(5));
        frame(64);
        render_now(window);
        click(window, 90.0, center(1));
        frame(600);
        assert_eq!(
            sidebar_pixels(&render_now(window)),
            sidebar_pixels(&settled[&1])
        );

        // Reduced motion lands on the first frame, without a timer tick.
        app.global::<Theme>().set_system_reduced_motion(true);
        for page in [5, 3] {
            click(window, 90.0, center(page));
            assert_eq!(
                sidebar_pixels(&render_now(window)),
                sidebar_pixels(&settled[&page]),
                "reduced motion must move the selection immediately"
            );
        }
        app.global::<Theme>().set_system_reduced_motion(false);
    }
    app.global::<Theme>().set_animations_enabled(false);
    println!(
        "Synthetic sidebar transitions and settle checks: {}",
        output.display()
    );
    Ok(())
}

fn activity_row(index: usize, stamp: i32) -> RequestRow {
    let kind = match index % 9 {
        3 => 1,
        6 => 3,
        8 => 2,
        _ => 0,
    };
    let ms = match kind {
        1 => 0,
        3 => 1_600 + (index as i32 % 4) * 700,
        _ => 380 + (index as i32 * 2_731) % 11_000,
    };
    let (model, provider, upstream) = [
        ("gpt-6-luna", "ChatGPT 订阅", "gpt-6-luna"),
        (
            "sx-chatgpt-01b7c588f74e632446a86553f624da4d-gpt-6-luna",
            "ChatGPT 订阅",
            "gpt-6-luna",
        ),
        ("sx-podlink-gpt-6-luna", "Podlink", "gpt-6-luna"),
        ("grok-5-fast", "Grok 账号", "grok-5-fast-reasoning"),
    ][index % 4];
    let (provider, upstream) = if kind == 1 {
        ("未选择上游", "—")
    } else {
        (provider, upstream)
    };
    let clock = format!("09:{:02}:{:02}", 20 + index / 60, index % 60);
    RequestRow {
        id: format!("synthetic-{index:04}").into(),
        time: format!("2026-10-10 {clock}").into(),
        clock: clock.into(),
        model: model.into(),
        provider: provider.into(),
        upstream: upstream.into(),
        duration: if ms >= 1_000 {
            format!("{:.1} s", ms as f64 / 1_000.0)
        } else {
            format!("{ms} ms")
        }
        .into(),
        duration_ms: ms,
        level: ((ms as f64 + 1.0).log10() / 60_001_f64.log10()) as f32,
        headers_ms: if kind == 1 { -1 } else { ms / 9 },
        first_event_ms: if kind == 1 || kind == 3 { -1 } else { ms / 4 },
        http: if kind == 1 { "" } else { "200" }.into(),
        generation: "synthetic-generation".into(),
        status: ["正常完成", "请求失败", "流中断", "用户取消 / 客户端断开"][kind as usize].into(),
        kind,
        error: [
            "",
            "模型未发布 · unknown_model",
            "响应结束，但未收到正常完成信号 · missing_completion",
            "完成前客户端断开；可能是用户取消或连接丢失 · client_disconnected",
        ][kind as usize]
            .into(),
        fallback: if index % 13 == 5 {
            "主连接 建立连接失败（请求未发送）→ 尝试备用 Podlink".into()
        } else {
            "".into()
        },
        stamp,
    }
}

fn activity_bar(row: &RequestRow) -> RequestBar {
    RequestBar {
        id: row.id.clone(),
        level: row.level,
        kind: row.kind,
        label: format!("{} · {} · {}", row.model, row.duration, row.clock).into(),
        stamp: row.stamp,
    }
}

fn activity_stats(rows: &[RequestRow]) -> RequestStats {
    let count = |kind: &[i32]| rows.iter().filter(|row| kind.contains(&row.kind)).count() as i32;
    let mut done = rows
        .iter()
        .filter(|row| row.kind == 0)
        .map(|row| row.duration_ms as f32)
        .collect::<Vec<_>>();
    done.sort_by(f32::total_cmp);
    let rank = |p: usize| {
        if done.is_empty() {
            -1.0
        } else {
            done[(done.len() * p).div_ceil(100).max(1) - 1]
        }
    };
    let (completed, failed) = (count(&[0]), count(&[1, 2]));
    RequestStats {
        total: rows.len() as i32,
        completed,
        failed,
        cancelled: count(&[3]),
        success_rate: if completed + failed == 0 {
            -1.0
        } else {
            completed as f32 * 100.0 / (completed + failed) as f32
        },
        p50_ms: rank(50),
        p95_ms: rank(95),
        first_event_ms: rank(50) / 4.0,
    }
}

// Plays real 16 ms frames so per-frame tweens behave as in the app, saving the listed moments.
fn play(
    window: &MinimalSoftwareWindow,
    output: &Path,
    name: &str,
    moments: &[u64],
) -> Result<(), Box<dyn Error>> {
    let end = moments.iter().copied().max().unwrap_or(0);
    let mut at = 0;
    loop {
        if moments.contains(&at) {
            snapshot_now(window, output, &format!("{name}-{at:04}ms"))?;
        } else {
            render_now(window);
        }
        if at >= end {
            return Ok(());
        }
        frame(16);
        at += 16;
    }
}

fn render_activity_design(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    let filters = Rc::new(RefCell::new(Vec::<RequestRow>::new()));
    let log = filters.clone();
    let weak = app.as_weak();
    app.on_filter_requests(move |filter| {
        let app = weak.upgrade().unwrap();
        let rows = log
            .borrow()
            .iter()
            .filter(|row| match filter {
                1 => row.kind == 0,
                2 => row.kind == 1 || row.kind == 2,
                3 => row.kind == 3,
                _ => true,
            })
            .cloned()
            .collect::<Vec<_>>();
        app.set_requests(ModelRc::new(VecModel::from(rows)));
    });
    let polls = Rc::new(Cell::new(0));
    let counter = polls.clone();
    app.on_refresh_requests(move || counter.set(counter.get() + 1));
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            app.global::<Theme>().set_animations_enabled(false);
            app.set_active_page(0);
            set_theme(app, window, dark);
            let rows = (0..36)
                .map(|index| activity_row(index, 0))
                .collect::<Vec<_>>();
            *filters.borrow_mut() = rows.clone();
            let model = Rc::new(VecModel::from(rows.clone()));
            let bars = Rc::new(VecModel::from(
                rows.iter()
                    .rev()
                    .take(48)
                    .rev()
                    .map(activity_bar)
                    .collect::<Vec<_>>(),
            ));
            app.set_requests(ModelRc::from(model.clone()));
            app.set_request_bars(ModelRc::from(bars.clone()));
            app.set_request_stats(activity_stats(&rows));
            app.set_request_sync(1);
            app.set_request_filter(0);
            app.set_route_running(true);

            app.global::<Theme>().set_animations_enabled(true);
            frame(600);
            render_now(window);
            let before = polls.get();
            app.set_active_page(3);
            render_now(window);
            assert_eq!(
                polls.get(),
                before + 1,
                "entering the page refreshes at once"
            );
            // Entry motion starts only after the page has painted.
            play(
                window,
                output,
                &format!("activity-enter-{suffix}"),
                &[64, 144, 256, 416, 640, 1008, 1600],
            )?;
            frame(2_000);
            assert!(polls.get() >= before + 2, "the page polls while it is open");
            assert!(app.get_request_following());
            snapshot_now(window, output, &format!("activity-settled-{suffix}"))?;

            // A live arrival lands at the bottom and the view follows it.
            for index in 36..38 {
                app.set_request_sync(app.get_request_sync() + 1);
                let row = activity_row(index, app.get_request_sync());
                filters.borrow_mut().push(row.clone());
                bars.push(activity_bar(&row));
                model.push(row);
            }
            app.set_request_stats(activity_stats(&filters.borrow()));
            play(
                window,
                output,
                &format!("activity-arrival-{suffix}"),
                &[0, 80, 176, 320, 528, 896, 1408],
            )?;
            assert!(app.get_request_following(), "arrivals keep following");

            // Scrolling back pauses following; arrivals then wait behind the pill.
            window.dispatch_event(WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(width as f32 * 0.6, height as f32 * 0.6),
                delta_x: 0.0,
                delta_y: 420.0,
            });
            frame(32);
            assert!(
                !app.get_request_following(),
                "scrolling back pauses following"
            );
            for index in 38..41 {
                app.set_request_sync(app.get_request_sync() + 1);
                let row = activity_row(index, app.get_request_sync());
                bars.push(activity_bar(&row));
                model.push(row);
            }
            app.set_request_unseen(3);
            play(
                window,
                output,
                &format!("activity-paused-{suffix}"),
                &[0, 128, 400],
            )?;
            let pill = (
                180.0 + (width as f32 - 180.0) / 2.0 - 8.0,
                height as f32 - 16.0 - 14.0 - 14.0 - 17.0 - 14.0,
            );
            click(window, pill.0, pill.1);
            frame(16);
            assert!(app.get_request_following(), "the pill resumes following");
            assert_eq!(app.get_request_unseen(), 0);
            play(
                window,
                output,
                &format!("activity-resumed-{suffix}"),
                &[160, 896],
            )?;

            // Expand the newest entry; it stays in view while following.
            let last_row = height as f32 - 16.0 - 14.0 - 14.0 - 34.0;
            click(window, width as f32 * 0.6, last_row);
            play(
                window,
                output,
                &format!("activity-details-{suffix}"),
                &[0, 128, 256, 608],
            )?;

            // Hovering a pulse bar names its request.
            let mut position = slint::LogicalPosition::new(width as f32 - 90.0, 250.0);
            window.dispatch_event(WindowEvent::PointerMoved { position });
            frame(200);
            snapshot_now(window, output, &format!("activity-pulse-hover-{suffix}"))?;
            position.x = 10.0;
            window.dispatch_event(WindowEvent::PointerMoved { position });

            app.invoke_filter_requests(2);
            app.set_request_filter(2);
            frame(700);
            snapshot_now(window, output, &format!("activity-filter-{suffix}"))?;
            app.set_request_filter(0);

            app.set_requests(ModelRc::new(VecModel::from(Vec::new())));
            app.set_request_bars(ModelRc::new(VecModel::from(Vec::new())));
            app.set_request_stats(activity_stats(&[]));
            app.set_active_page(0);
            play(window, output, "unused", &[])?;
            app.set_active_page(3);
            play(
                window,
                output,
                &format!("activity-empty-{suffix}"),
                &[400, 1200],
            )?;

            // Leaving the page stops polling.
            app.set_active_page(0);
            render_now(window);
            let left = polls.get();
            frame(6_000);
            assert_eq!(polls.get(), left, "polling stops once the page is left");
            app.global::<Theme>().set_animations_enabled(false);
        }
    }
    println!("Synthetic activity timeline frames: {}", output.display());
    Ok(())
}

fn connect_code_highlighting(app: &AppWindow) {
    app.global::<SyntaxHighlighting>().on_line_count(|source| {
        source
            .as_str()
            .split('\n')
            .count()
            .try_into()
            .unwrap_or(i32::MAX)
    });
    app.global::<SyntaxHighlighting>()
        .on_spans(|source, language| {
            let spans = switchx::code_highlight::spans(source.as_str(), language.as_str())
                .into_iter()
                .map(|span| CodeSpan {
                    text: span.text.into(),
                    prefix: span.prefix.into(),
                    line_text: span.line_text.into(),
                    line: span.line,
                    kind: span.kind as i32,
                })
                .collect::<Vec<_>>();
            ModelRc::new(VecModel::from(spans))
        });
}

fn render_subscription_editor(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    connect_code_highlighting(app);
    app.set_active_page(1);
    app.set_subscription_id("preview-subscription".into());
    app.set_subscription_name("ChatGPT · 工作账号".into());
    app.set_subscription_account_ids(ModelRc::new(VecModel::from(vec![
        "".into(),
        "synthetic-account".into(),
    ])));
    app.set_subscription_account_options(ModelRc::new(VecModel::from(vec![
        "跟随 Codex 登录".into(),
        "工作账号 · Plus".into(),
    ])));
    app.set_subscription_binding_label(
        "跟随所选 Codex 目录的登录；也可选择保存账号或粘贴完整 auth.json。".into(),
    );
    app.set_edit_icon_name("OpenAI".into());
    app.set_edit_icon(provider_icons::load_image(
        provider_icons::icon("openai").unwrap(),
    )?);
    app.set_edit_use_common_config(true);
    app.set_edit_context_1m(true);
    app.set_edit_compact_limit("900000".into());
    app.set_edit_config_preview(
        concat!(
            "model = \"synthetic-codex\"\n",
            "model_reasoning_effort = \"high\"\n",
            "model_context_window = 1000000\n",
            "model_auto_compact_token_limit = 900000\n\n",
            "web_search = \"live\"\n",
            "service_tier = \"default\"\n\n",
            "[features]\n",
            "shell_tool = true\n",
            "apply_patch_freeform = true\n"
        )
        .into(),
    );
    const SYNTHETIC_AUTH: &str = "{\n  \"auth_mode\": \"chatgpt\",\n  \"tokens\": {\n    \"access_token\": \"synthetic-preview-only\"\n  }\n}";
    let weak = app.as_weak();
    app.on_load_subscription_auth(move || {
        weak.upgrade()
            .unwrap()
            .set_subscription_auth_json(SYNTHETIC_AUTH.into());
    });
    let weak = app.as_weak();
    app.on_cancel_subscription_editor(move || {
        let app = weak.upgrade().unwrap();
        app.set_subscription_editor_open(false);
        app.set_subscription_auth_json("".into());
    });
    let saves = Rc::new(Cell::new(0));
    let saved = saves.clone();
    app.on_save_subscription(move |id, name, account| {
        assert_eq!(id, "preview-subscription");
        assert_eq!(name, "ChatGPT · 工作账号");
        assert_eq!(account, "synthetic-account");
        saved.set(saved.get() + 1);
    });
    let formats = Rc::new(Cell::new(0));
    let formatted = formats.clone();
    app.on_format_subscription_auth(move || formatted.set(formatted.get() + 1));
    let commons = Rc::new(Cell::new(0));
    let common = commons.clone();
    app.on_update_subscription_common(move || common.set(common.get() + 1));
    let contexts = Rc::new(Cell::new(0));
    let context = contexts.clone();
    app.on_update_subscription_context(move || context.set(context.get() + 1));
    let icons = Rc::new(Cell::new(0));
    let icon = icons.clone();
    app.on_open_provider_icon_picker(move || icon.set(icon.get() + 1));
    let deletes = Rc::new(Cell::new(0));
    let deleted = deletes.clone();
    app.on_delete_provider(move |id| {
        assert_eq!(id, "preview-subscription");
        deleted.set(deleted.get() + 1);
    });
    let models = Rc::new(Cell::new(0));
    let model = models.clone();
    app.on_begin_connection_models(move |id| {
        assert_eq!(id, "preview-subscription");
        model.set(model.get() + 1);
    });
    for (width, height) in [(1200, 820), (1000, 680), (1600, 1000)] {
        app.window().set_size(PhysicalSize::new(width, height));
        let left = (width as f32 - 1120.0).max(60.0) + 26.0;
        let right = left + 312.0;
        for dark in [false, true] {
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            set_theme(app, window, dark);
            app.set_delete_confirm(false);
            app.set_edit_use_common_config(true);
            app.set_edit_context_1m(true);
            app.set_subscription_account_choice(0);
            app.set_subscription_binding_label(
                "跟随所选 Codex 目录的登录；也可选择保存账号或粘贴完整 auth.json。".into(),
            );
            app.set_subscription_editor_open(true);
            snapshot(window, output, &format!("subscription-editor-{suffix}"))?;

            // A credential draft cannot change the public configuration view.
            let public = render_now(window);
            app.set_subscription_auth_json("synthetic-hidden-credential".into());
            assert!(
                public.as_bytes() == render_now(window).as_bytes(),
                "credentials stay out of the configuration view"
            );
            app.set_subscription_auth_json(SYNTHETIC_AUTH.into());

            let count = icons.get();
            click(window, left + 40.0, 180.0);
            assert_eq!(icons.get(), count + 1, "the avatar opens the icon picker");
            window.dispatch_event(WindowEvent::PointerMoved {
                position: slint::LogicalPosition::new(10.0, 10.0),
            });
            click(window, width as f32 - 126.0, 164.0);
            snapshot(window, output, &format!("subscription-options-{suffix}"))?;
            let count = commons.get();
            click(window, right + 70.0, 273.0);
            assert!(!app.get_edit_use_common_config());
            assert_eq!(
                commons.get(),
                count + 1,
                "the switch updates the configuration"
            );
            click(window, right + 70.0, 273.0);
            let count = contexts.get();
            let context_x = right + (width as f32 - 26.0 - right) / 2.0 + 70.0;
            click(window, context_x, 273.0);
            assert!(!app.get_edit_context_1m());
            assert_eq!(contexts.get(), count + 1);
            snapshot(
                window,
                output,
                &format!("subscription-context-off-{suffix}"),
            )?;
            click(window, context_x, 273.0);
            click(window, width as f32 - 126.0, 164.0);
            render(window);

            click(window, right + 195.0, 215.0);
            snapshot(window, output, &format!("subscription-auth-{suffix}"))?;
            let count = formats.get();
            click(window, width as f32 - 110.0, 164.0);
            assert_eq!(formats.get(), count + 1, "JSON formatting stays reachable");
            app.set_subscription_auth_error("合成 JSON 错误：请检查格式。".into());
            snapshot(window, output, &format!("subscription-auth-error-{suffix}"))?;
            let count = saves.get();
            click(window, width as f32 - 90.0, height as f32 - 46.0);
            assert_eq!(saves.get(), count, "invalid JSON cannot be saved");
            app.set_subscription_auth_error("".into());
            click(window, right + 65.0, 215.0);
            render(window);

            click(window, width as f32 - 44.0, 164.0);
            snapshot(window, output, &format!("subscription-expanded-{suffix}"))?;
            click(window, width as f32 - 44.0, 164.0);
            render(window);
            app.set_subscription_account_choice(1);
            app.set_subscription_binding_label(
                "绑定到已保存的工作账号；发布路由时核对账号。".into(),
            );
            snapshot(window, output, &format!("subscription-bound-{suffix}"))?;
            let count = saves.get();
            click(window, width as f32 - 90.0, height as f32 - 46.0);
            assert_eq!(
                saves.get(),
                count + 1,
                "save uses the selected account at every size"
            );
            app.set_config_managed(true);
            click(window, width as f32 - 90.0, height as f32 - 46.0);
            assert_eq!(
                saves.get(),
                count + 1,
                "managed configuration prevents saving"
            );
            app.set_config_managed(false);
            app.set_edit_config_error("合成配置错误：压缩阈值需要修正。".into());
            click(window, width as f32 - 126.0, 164.0);
            snapshot(
                window,
                output,
                &format!("subscription-config-error-{suffix}"),
            )?;
            click(window, width as f32 - 90.0, height as f32 - 46.0);
            assert_eq!(saves.get(), count + 1, "invalid TOML cannot be saved");
            app.set_edit_config_error("".into());
            app.set_subscription_name("".into());
            click(window, width as f32 - 90.0, height as f32 - 46.0);
            assert_eq!(saves.get(), count + 1, "a connection needs a name");
            app.set_subscription_name("ChatGPT · 工作账号".into());
            let count = deletes.get();
            click(window, left + 56.0, height as f32 - 46.0);
            assert!(app.get_delete_confirm());
            assert_eq!(deletes.get(), count, "delete requires confirmation");
            click(window, left + 56.0, height as f32 - 46.0);
            assert_eq!(deletes.get(), count + 1);
            if height >= 820 {
                let count = models.get();
                click(window, left + 115.0, 646.0);
                assert_eq!(
                    models.get(),
                    count + 1,
                    "the model directory shortcut works"
                );
            }
            app.set_subscription_auth_json("synthetic-unsaved-credential".into());
            click(window, width as f32 - 202.0, height as f32 - 46.0);
            assert!(
                !app.get_subscription_editor_open(),
                "cancel stays reachable"
            );
            assert!(app.get_subscription_auth_json().is_empty());
            render(window);
        }
    }

    app.window().set_size(PhysicalSize::new(1200, 820));
    set_theme(app, window, true);
    app.global::<Theme>().set_animations_enabled(true);
    app.set_subscription_editor_open(true);
    play(
        window,
        output,
        "subscription-enter",
        &[0, 64, 144, 256, 416, 640, 960],
    )?;
    window.dispatch_event(WindowEvent::PointerMoved {
        position: slint::LogicalPosition::new(150.0, 180.0),
    });
    play(window, output, "subscription-avatar", &[0, 80, 160, 320])?;
    click(window, 590.0, 215.0);
    play(window, output, "subscription-tabs", &[0, 64, 144, 256, 416])?;
    click(window, 460.0, 215.0);
    play(
        window,
        output,
        "subscription-tabs-return",
        &[0, 64, 144, 416],
    )?;
    click(window, 1074.0, 164.0);
    play(
        window,
        output,
        "subscription-options",
        &[0, 64, 144, 256, 416],
    )?;
    click(window, 1074.0, 164.0);
    play(
        window,
        output,
        "subscription-options-close",
        &[0, 64, 144, 416],
    )?;
    click(window, 1156.0, 164.0);
    let first = render_now(window);
    play(
        window,
        output,
        "subscription-expand",
        &[0, 64, 144, 256, 416, 640],
    )?;
    let last = render_now(window);
    assert!(
        first.as_bytes() != last.as_bytes(),
        "the editor width animates"
    );
    // Reverse twice during the transition and compare with a clean settle.
    click(window, 1156.0, 164.0);
    render_now(window);
    frame(96);
    click(window, 1156.0, 164.0);
    render_now(window);
    frame(640);
    assert!(
        last.as_bytes() == render_now(window).as_bytes(),
        "an interrupted transition settles to the same layout"
    );
    for reduced in [false, true] {
        app.global::<Theme>().set_animations_enabled(reduced);
        app.global::<Theme>().set_system_reduced_motion(reduced);
        click(window, 1156.0, 164.0);
        let immediate = render_now(window);
        frame(640);
        assert!(
            immediate.as_bytes() == render_now(window).as_bytes(),
            "disabled motion settles immediately"
        );
        app.set_subscription_editor_open(false);
        render_now(window);
        app.set_subscription_editor_open(true);
        let immediate = render_now(window);
        frame(960);
        assert!(
            immediate.as_bytes() == render_now(window).as_bytes(),
            "disabled entry motion settles immediately"
        );
    }
    app.global::<Theme>().set_system_reduced_motion(false);
    app.global::<Theme>().set_animations_enabled(false);
    app.set_subscription_editor_open(false);
    app.set_subscription_id("".into());
    app.set_subscription_name("ChatGPT · 新连接".into());
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            set_theme(app, window, dark);
            app.set_subscription_editor_open(true);
            snapshot(
                window,
                output,
                &format!(
                    "subscription-new-{}-{width}x{height}",
                    if dark { "dark" } else { "light" }
                ),
            )?;
            app.set_subscription_editor_open(false);
        }
    }
    Ok(())
}

fn render_common_config(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    connect_code_highlighting(app);
    app.set_active_page(1);
    app.set_common_config_saved(
        concat!(
            "# Shared across your Codex connections\n",
            "web_search = \"live\"\n",
            "model_reasoning_effort = \"high\"\n",
            "service_tier = \"default\"\n\n",
            "[features]\n",
            "shell_tool = true\n",
            "apply_patch_freeform = true\n\n",
            "[history]\n",
            "persistence = \"save-all\"\n\n",
            "[shell_environment_policy]\n",
            "inherit = \"core\"\n",
            "exclude = [\"EXAMPLE_PRIVATE_*\"]\n"
        )
        .into(),
    );
    app.set_common_config_draft(app.get_common_config_saved());
    app.set_common_config_current_source("连接表单当前 config.toml".into());
    let native = std::env::args().any(|arg| arg == "--common-config-native");
    let saves = Rc::new(Cell::new(0));
    let extracts = Rc::new(Cell::new(0));
    let saved = saves.clone();
    let weak = app.as_weak();
    app.on_save_common_config(move || {
        saved.set(saved.get() + 1);
        if let Some(app) = weak.upgrade() {
            app.set_busy(true);
            if native {
                let weak = app.as_weak();
                slint::Timer::single_shot(Duration::from_millis(900), move || {
                    if let Some(app) = weak.upgrade() {
                        app.set_common_config_saved(app.get_common_config_draft());
                        app.set_busy(false);
                        app.set_common_config_editor_open(false);
                    }
                });
            }
        }
    });
    let extracted = extracts.clone();
    let weak = app.as_weak();
    app.on_extract_common_config(move || {
        extracted.set(extracted.get() + 1);
        if let Some(app) = weak.upgrade() {
            app.set_busy(true);
            if native {
                let weak = app.as_weak();
                slint::Timer::single_shot(Duration::from_millis(1100), move || {
                    if let Some(app) = weak.upgrade() {
                        app.set_common_config_draft(app.get_common_config_saved());
                        app.set_busy(false);
                        app.set_common_config_message(
                            "已从合成连接表单提取并保存；取消不会撤销此次提取。".into(),
                        );
                    }
                });
            }
        }
    });
    let weak = app.as_weak();
    app.on_cancel_common_config(move || {
        if let Some(app) = weak.upgrade()
            && !app.get_busy()
        {
            app.set_common_config_draft(app.get_common_config_saved());
            app.set_common_config_error("".into());
            app.set_common_config_message("".into());
            app.set_common_config_editor_open(false);
        }
    });
    if native {
        app.window().set_size(PhysicalSize::new(1200, 820));
        app.global::<Theme>().set_animations_enabled(true);
        app.invoke_set_appearance(true);
        app.set_common_config_editor_open(true);
        return Ok(app.run()?);
    }
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            set_theme(app, window, dark);
            app.set_common_config_draft(app.get_common_config_saved());
            app.set_common_config_editor_open(true);
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            snapshot(window, output, &format!("common-config-{suffix}"))?;
            let left = width as f32 - 920.0 + 26.0;
            let help_y = if height < 760 { 230.0 } else { 258.0 };
            click(window, width as f32 - 80.0, help_y);
            snapshot(window, output, &format!("common-help-{suffix}"))?;
            click(window, width as f32 - 80.0, help_y);
            render(window);
            click(window, width as f32 - 56.0, help_y + 60.0);
            snapshot(window, output, &format!("common-focus-{suffix}"))?;
            click(
                window,
                width as f32 - 56.0,
                help_y + 60.0 - if height < 760 { 124.0 } else { 148.0 },
            );
            render(window);
            // Exercise native input focus and the text binding, then save.
            click(window, left + 140.0, help_y + 110.0);
            window.dispatch_event(WindowEvent::KeyPressed { text: " ".into() });
            window.dispatch_event(WindowEvent::KeyReleased { text: " ".into() });
            assert_ne!(
                app.get_common_config_draft(),
                app.get_common_config_saved(),
                "the editor must accept keyboard input"
            );
            snapshot(window, output, &format!("common-dirty-{suffix}"))?;
            let count = saves.get();
            click(window, width as f32 - 89.0, height as f32 - 47.0);
            assert_eq!(
                saves.get(),
                count + 1,
                "save remains reachable at every size"
            );
            assert!(app.get_busy());
            snapshot(window, output, &format!("common-saving-{suffix}"))?;
            click(window, width as f32 - 89.0, height as f32 - 47.0);
            click(window, left + 93.0, height as f32 - 47.0);
            window.dispatch_event(WindowEvent::KeyPressed {
                text: slint::platform::Key::Escape.into(),
            });
            assert!(
                app.get_common_config_editor_open(),
                "busy operations cannot be dismissed"
            );
            assert_eq!(
                saves.get(),
                count + 1,
                "busy operations cannot be submitted twice"
            );
            app.set_busy(false);

            let count = extracts.get();
            click(window, left + 93.0, height as f32 - 47.0);
            assert_eq!(
                extracts.get(),
                count + 1,
                "extract remains reachable at every size"
            );
            snapshot(window, output, &format!("common-extracting-{suffix}"))?;
            app.set_common_config_draft(app.get_common_config_saved());
            app.set_busy(false);
            app.set_common_config_message("已从连接表单提取并保存；取消不会撤销此次提取。".into());
            snapshot(window, output, &format!("common-extracted-{suffix}"))?;
            app.set_common_config_message("".into());
            app.set_common_config_error("TOML 第 3 行缺少右引号，请检查后重新保存。".into());
            snapshot(window, output, &format!("common-error-{suffix}"))?;
            app.set_common_config_error("".into());
            app.set_config_managed(true);
            snapshot(window, output, &format!("common-managed-{suffix}"))?;
            let saved = saves.get();
            let extracted = extracts.get();
            click(window, width as f32 - 89.0, height as f32 - 47.0);
            click(window, left + 93.0, height as f32 - 47.0);
            assert_eq!(saves.get(), saved, "managed configuration cannot be saved");
            assert_eq!(
                extracts.get(),
                extracted,
                "managed configuration cannot be extracted"
            );
            app.set_config_managed(false);
            app.set_common_config_draft("".into());
            snapshot(window, output, &format!("common-empty-{suffix}"))?;
            click(window, width as f32 - 205.0, height as f32 - 47.0);
            assert!(!app.get_common_config_editor_open());
            assert_eq!(
                app.get_common_config_draft(),
                app.get_common_config_saved(),
                "cancel discards only the manual draft"
            );
            app.set_common_config_editor_open(false);
            render(window);
        }
    }

    app.window().set_size(PhysicalSize::new(1200, 820));
    set_theme(app, window, true);
    app.global::<Theme>().set_animations_enabled(true);
    render_now(window);
    app.set_common_config_editor_open(true);
    let first = render_now(window);
    let mut elapsed = 0;
    for at in [0, 16, 48, 96, 160, 240, 340, 480, 640] {
        frame(at - elapsed);
        elapsed = at;
        snapshot_now(window, output, &format!("common-enter-{at:03}ms"))?;
    }
    assert_ne!(
        first.as_bytes(),
        render_now(window).as_bytes(),
        "the drawer and its sections must animate in"
    );
    click(window, 1120.0, 258.0);
    render_now(window);
    frame(96);
    snapshot_now(window, output, "common-help-opening-096ms")?;
    // Reverse twice before settling; controls must stay reachable.
    click(window, 1120.0, 258.0);
    render_now(window);
    frame(48);
    click(window, 1120.0, 258.0);
    render_now(window);
    frame(480);
    snapshot_now(window, output, "common-help-interrupted-settled")?;
    app.set_common_config_editor_open(false);
    render_now(window);
    frame(96);
    snapshot_now(window, output, "common-exit-096ms")?;
    app.set_common_config_editor_open(true);
    render_now(window);
    frame(640);
    snapshot_now(window, output, "common-reopened")?;

    for reduced in [false, true] {
        app.set_common_config_editor_open(false);
        frame(640);
        app.global::<Theme>().set_animations_enabled(reduced);
        app.global::<Theme>().set_system_reduced_motion(reduced);
        app.set_common_config_editor_open(true);
        let still = render_now(window);
        frame(1500);
        assert_eq!(
            still.as_bytes(),
            render_now(window).as_bytes(),
            "disabled or reduced motion settles immediately, including the floating artwork"
        );
        snapshot_now(
            window,
            output,
            if reduced {
                "common-system-reduced-motion"
            } else {
                "common-motion-disabled"
            },
        )?;
        app.set_common_config_editor_open(false);
        render_now(window);
    }
    app.global::<Theme>().set_system_reduced_motion(false);
    app.global::<Theme>().set_animations_enabled(true);
    render_now(window);
    // A deterministic animation reel for visual review, using only fixtures.
    for index in 0..120 {
        match index {
            1 => app.set_common_config_editor_open(true),
            24 | 44 => click(window, 1120.0, 258.0),
            60 => click(window, 1144.0, 318.0),
            80 => app.set_common_config_draft(
                format!(
                    "{}\n# A fresh shared preference\n",
                    app.get_common_config_saved()
                )
                .into(),
            ),
            90 => click(window, 1111.0, 773.0),
            105 => {
                app.set_busy(false);
                app.set_common_config_editor_open(false);
            }
            _ => {}
        }
        snapshot_now(window, output, &format!("common-reel-{index:03}"))?;
        frame(40);
    }
    println!(
        "Synthetic shared configuration previews: {}",
        output.display()
    );
    Ok(())
}

fn render_provider_editor(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    connect_code_highlighting(app);
    app.set_active_page(1);
    app.set_edit_id("preview-api".into());
    app.set_edit_name("Podlink".into());
    app.set_edit_url("https://api.example.invalid/v1".into());
    app.set_edit_model("gpt-6-luna".into());
    app.set_edit_icon_name("Anthropic".into());
    app.set_edit_icon(provider_icons::load_image(
        provider_icons::icon("anthropic").unwrap(),
    )?);
    app.set_edit_use_common_config(true);
    app.set_edit_context_1m(true);
    app.set_edit_compact_limit("900000".into());
    app.set_edit_config_preview(
        concat!(
            "model_provider = \"switchx_direct_preview\"\n",
            "model = \"gpt-6-luna\"\n",
            "web_search = \"live\"\n",
            "model_context_window = 1000000\n",
            "model_auto_compact_token_limit = 900000\n\n",
            "approval_policy = \"never\"\n",
            "sandbox_mode = \"danger-full-access\"\n",
            "model_reasoning_effort = \"high\"\n",
            "service_tier = \"default\"\n\n",
            "[model_providers.switchx_direct_preview]\n",
            "name = \"Podlink\"\n",
            "base_url = \"https://api.example.invalid/v1\"\n",
            "wire_api = \"responses\"\n\n",
            "[features]\n",
            "shell_tool = true\n",
            "apply_patch_freeform = true\n"
        )
        .into(),
    );
    if std::env::args().any(|arg| arg == "--provider-editor-native") {
        app.window().set_size(PhysicalSize::new(1200, 820));
        app.global::<Theme>().set_animations_enabled(true);
        app.invoke_set_appearance(true);
        app.set_editor_open(true);
        return Ok(app.run()?);
    }
    let saves = Rc::new(Cell::new(0));
    let saved = saves.clone();
    app.on_save_provider(move |_, _, _, _, _| saved.set(saved.get() + 1));
    let deletes = Rc::new(Cell::new(0));
    let deleted = deletes.clone();
    app.on_delete_provider(move |_| deleted.set(deleted.get() + 1));
    let fetches = Rc::new(Cell::new(0));
    let fetched = fetches.clone();
    app.on_fetch_models(move |_| fetched.set(fetched.get() + 1));
    for (width, height) in [(1200, 820), (1000, 680), (1600, 1000)] {
        app.window().set_size(PhysicalSize::new(width, height));
        let left = (width as f32 - 1120.0).max(60.0) + 26.0;
        let right = left + 312.0;
        for dark in [false, true] {
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            set_theme(app, window, dark);
            app.set_delete_confirm(false);
            app.set_editor_open(true);
            snapshot(window, output, &format!("provider-editor-{suffix}"))?;

            // Configuration preferences expand without moving the footer.
            click(window, width as f32 - 126.0, 164.0);
            render(window);
            click(window, right + 72.0, 239.0);
            assert!(
                !app.get_edit_use_common_config(),
                "whole-row switch must toggle"
            );
            click(window, right + 72.0, 239.0);
            assert!(app.get_edit_use_common_config());
            click(window, right + 70.0, 292.0);
            assert!(!app.get_edit_context_1m(), "context control must toggle");
            snapshot(window, output, &format!("provider-context-off-{suffix}"))?;
            click(window, right + 70.0, 292.0);
            assert!(app.get_edit_context_1m());
            snapshot(window, output, &format!("provider-options-{suffix}"))?;
            click(window, width as f32 - 126.0, 164.0);
            render(window);

            click(window, width as f32 - 44.0, 164.0);
            snapshot(window, output, &format!("provider-expanded-{suffix}"))?;
            click(window, width as f32 - 44.0, 164.0);
            render(window);

            if width == 1200 {
                let count = fetches.get();
                click(window, left + 136.0, 588.0);
                assert_eq!(
                    fetches.get(),
                    count + 1,
                    "model discovery must be reachable"
                );
            }
            app.set_edit_key("synthetic-clear-check".into());
            let count = saves.get();
            click(window, width as f32 - 90.0, height as f32 - 46.0);
            assert_eq!(
                saves.get(),
                count + 1,
                "save must remain visible at every size"
            );
            assert!(
                app.get_edit_key().is_empty(),
                "saving clears the credential draft"
            );
            app.set_delete_confirm(false);
            let count = deletes.get();
            click(window, left + 56.0, height as f32 - 46.0);
            assert!(app.get_delete_confirm());
            assert_eq!(deletes.get(), count, "delete still requires a second click");
            click(window, left + 56.0, height as f32 - 46.0);
            assert_eq!(deletes.get(), count + 1);
            app.set_edit_key("synthetic-clear-check".into());
            click(window, width as f32 - 202.0, height as f32 - 46.0);
            assert!(!app.get_editor_open(), "cancel must remain reachable");
            assert!(
                app.get_edit_key().is_empty(),
                "cancelling clears the credential draft"
            );
            render(window);
        }
    }

    // Capture a reversible width transition and verify reduced-motion settling.
    app.window().set_size(PhysicalSize::new(1200, 820));
    set_theme(app, window, true);
    app.set_delete_confirm(false);
    app.set_editor_open(true);
    render(window);
    app.global::<Theme>().set_animations_enabled(true);
    click(window, 1156.0, 164.0);
    let first = render_now(window);
    let mut elapsed = 0;
    for at in [0, 16, 48, 96, 160, 240, 340, 480] {
        frame(at - elapsed);
        elapsed = at;
        snapshot_now(window, output, &format!("provider-expand-{at:03}ms"))?;
    }
    let last = render_now(window);
    assert_ne!(first.as_bytes(), last.as_bytes(), "expansion must animate");
    click(window, 1156.0, 164.0);
    render_now(window);
    frame(96);
    click(window, 1156.0, 164.0);
    render_now(window);
    frame(480);
    assert_eq!(
        last.as_bytes(),
        render_now(window).as_bytes(),
        "interrupted expansion settles to the same layout"
    );
    for reduced in [false, true] {
        app.global::<Theme>().set_animations_enabled(reduced);
        app.global::<Theme>().set_system_reduced_motion(reduced);
        click(window, 1156.0, 164.0);
        let immediate = render_now(window);
        frame(600);
        assert_eq!(
            immediate.as_bytes(),
            render_now(window).as_bytes(),
            "disabled motion settles immediately"
        );
    }
    app.global::<Theme>().set_system_reduced_motion(false);
    app.global::<Theme>().set_animations_enabled(false);
    click(window, 1156.0, 164.0);
    render(window);
    app.set_edit_config_error("合成配置错误：请检查压缩阈值。".into());
    snapshot(window, output, "provider-config-error-dark")?;
    let count = saves.get();
    click(window, 1110.0, 774.0);
    assert_eq!(saves.get(), count, "invalid configuration cannot be saved");
    app.set_edit_config_error("".into());
    app.set_editor_open(false);
    render(window);

    app.set_edit_id("".into());
    app.set_edit_key("synthetic-new-connection".into());
    app.set_editor_open(true);
    render(window);
    let rows = batch_rows("");
    app.set_batch_summary(summarize(&rows));
    app.set_batch_models(ModelRc::new(VecModel::from(rows)));
    snapshot(window, output, "provider-new-discovered-dark")?;
    app.set_editor_open(false);
    println!("Synthetic API editor previews: {}", output.display());
    Ok(())
}

fn render_connection_picker_motion(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    // Synthetic callbacks follow production's open-form / close-picker ordering.
    app.set_active_page(1);
    app.set_provider_presets(ModelRc::new(VecModel::from(
        switchx::app::PROVIDER_PRESETS
            .iter()
            .map(|preset| {
                Ok(ProviderPresetRow {
                    id: preset.id.into(),
                    name: preset.name.into(),
                    icon: slint::Image::load_from_svg_data(preset.icon)?,
                    monochrome: preset.monochrome,
                })
            })
            .collect::<Result<Vec<_>, slint::LoadImageError>>()?,
    )));
    let selections = Rc::new(Cell::new(0));
    let selected = selections.clone();
    let weak = app.as_weak();
    app.on_begin_provider_editor(move |_| {
        let app = weak.upgrade().unwrap();
        selected.set(selected.get() + 1);
        app.set_edit_id("".into());
        app.set_edit_name("DeepSeek".into());
        app.set_edit_url("https://api.example.invalid/v1".into());
        app.set_edit_model("synthetic-model".into());
        app.set_edit_key("".into());
        app.set_edit_preset_id("deepseek".into());
        app.set_edit_icon_name("DeepSeek".into());
        app.set_edit_icon(
            provider_icons::load_image(provider_icons::icon("deepseek").unwrap()).unwrap(),
        );
        app.set_edit_config_preview("model = \"synthetic-model\"\nweb_search = \"live\"\n".into());
        app.set_editor_open(true);
        app.set_connection_picker_open(false);
    });
    let weak = app.as_weak();
    app.on_begin_subscription_editor(move |_| {
        let app = weak.upgrade().unwrap();
        app.set_subscription_id("".into());
        app.set_subscription_name("ChatGPT".into());
        app.set_subscription_account_ids(ModelRc::new(VecModel::from(vec!["".into()])));
        app.set_subscription_account_options(ModelRc::new(VecModel::from(vec![
            "跟随 Codex 登录".into(),
        ])));
        app.set_subscription_editor_open(true);
        app.set_connection_picker_open(false);
    });
    let weak = app.as_weak();
    app.on_cancel_subscription_editor(move || {
        let app = weak.upgrade().unwrap();
        app.set_subscription_editor_open(false);
        app.set_subscription_editor_pending(false);
        app.set_subscription_auth_json("".into());
    });
    let weak = app.as_weak();
    app.on_begin_xai_editor(move |_| {
        let app = weak.upgrade().unwrap();
        app.set_xai_provider_id("".into());
        app.set_xai_provider_name("Grok".into());
        app.set_xai_editor_open(true);
        app.set_connection_picker_open(false);
    });
    let saves = Rc::new(Cell::new(0));
    let saved = saves.clone();
    app.on_save_provider(move |_, _, _, _, _| saved.set(saved.get() + 1));

    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        let picker_left = width as f32 - 820.0 + 26.0;
        let api_left = (width as f32 - 1120.0).max(60.0) + 26.0;
        for dark in [false, true] {
            app.global::<Theme>().set_animations_enabled(false);
            set_theme(app, window, dark);
            app.invoke_open_connection_picker();
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            snapshot(window, output, &format!("connection-picker-{suffix}"))?;
            app.global::<Theme>().set_animations_enabled(true);
            click(window, picker_left + 90.0, 280.0);
            assert!(app.get_editor_open());
            let mut elapsed = 0;
            for at in [0, 16, 48, 96, 160, 240, 340, 480] {
                frame(at - elapsed);
                elapsed = at;
                snapshot_now(
                    window,
                    output,
                    &format!("connection-forward-{suffix}-{at:03}ms"),
                )?;
            }
            app.set_edit_key("synthetic-unsaved-key".into());
            render_now(window);
            click(window, api_left + 50.0, 108.0);
            assert!(
                app.get_connection_picker_open(),
                "back returns one level to the picker"
            );
            assert!(!app.get_editor_open());
            assert!(
                app.get_edit_key().is_empty(),
                "back clears the draft immediately"
            );
            let before = saves.get();
            click(window, width as f32 - 90.0, height as f32 - 46.0);
            assert_eq!(saves.get(), before, "outgoing save is inert");
            let mut elapsed = 0;
            for at in [0, 16, 48, 96, 160, 240, 340, 480] {
                frame(at - elapsed);
                elapsed = at;
                snapshot_now(
                    window,
                    output,
                    &format!("connection-back-{suffix}-{at:03}ms"),
                )?;
            }
            assert_ne!(
                fs::read(output.join(format!("connection-back-{suffix}-016ms.ppm")))?,
                fs::read(output.join(format!("connection-back-{suffix}-480ms.ppm")))?,
                "return has intermediate animation frames"
            );

            // Rapid retargeting and resize settle on a usable picker.
            click(window, picker_left + 90.0, 280.0);
            render_now(window);
            frame(96);
            app.invoke_return_to_connection_picker();
            render_now(window);
            frame(48);
            app.invoke_begin_provider_editor("".into());
            render_now(window);
            frame(48);
            app.invoke_return_to_connection_picker();
            app.window().set_size(PhysicalSize::new(width + 40, height));
            render_now(window);
            frame(600);
            app.window().set_size(PhysicalSize::new(width, height));
            render_now(window);
            frame(600);
            let count = selections.get();
            click(window, picker_left + 90.0, 280.0);
            assert_eq!(
                selections.get(),
                count + 1,
                "picker works after rapid reversal and resize"
            );
            render_now(window);
            frame(600);
            app.invoke_return_to_connection_picker();
            render_now(window);
            frame(600);

            // The same back affordance applies to both subscription forms.
            for (x, chatgpt) in [(picker_left + 90.0, true), (picker_left + 440.0, false)] {
                click(window, x, 160.0);
                render_now(window);
                frame(600);
                assert_eq!(app.get_subscription_editor_open(), chatgpt);
                assert_eq!(app.get_xai_editor_open(), !chatgpt);
                app.set_subscription_auth_json("synthetic-unsaved-credential".into());
                let form_left = if chatgpt { api_left } else { picker_left };
                click(window, form_left + 50.0, 108.0);
                assert!(app.get_connection_picker_open());
                assert!(app.get_subscription_auth_json().is_empty());
                assert!(!app.get_subscription_editor_open());
                assert!(!app.get_xai_editor_open());
                render_now(window);
                frame(600);
            }

            for reduced in [false, true] {
                app.global::<Theme>().set_animations_enabled(reduced);
                app.global::<Theme>().set_system_reduced_motion(reduced);
                click(window, picker_left + 90.0, 280.0);
                render_now(window);
                click(window, api_left + 50.0, 108.0);
                assert!(app.get_connection_picker_open());
                window.dispatch_event(WindowEvent::PointerMoved {
                    position: slint::LogicalPosition::new(5.0, 5.0),
                });
                let immediate = render_now(window);
                frame(1000);
                assert_eq!(
                    immediate.as_bytes(),
                    render_now(window).as_bytes(),
                    "disabled/reduced motion returns immediately"
                );
            }
            app.global::<Theme>().set_system_reduced_motion(false);
        }
    }
    println!(
        "Synthetic connection navigation and motion checks: {}",
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
    let native_provider = std::env::args().any(|arg| arg == "--provider-editor-native");
    let native_common = std::env::args().any(|arg| arg == "--common-config-native");
    if native_connections || native_provider || native_common {
        #[cfg(target_os = "macos")]
        macos::configure_window()?;
    } else {
        slint::platform::set_platform(Box::new(PreviewPlatform(window.clone())))?;
    }
    let app = AppWindow::new()?;
    app.set_native_titlebar_overlay(std::env::args().any(|arg| arg == "--native-titlebar-overlay"));
    switchx::reasoning_picker::connect(&app);
    app.set_loading(false);
    app.global::<Theme>().set_animations_enabled(false);
    if std::env::args().any(|arg| arg == "--connection-picker-motion") {
        app.show()?;
        return render_connection_picker_motion(&app, &window, output);
    }
    if std::env::args().any(|arg| arg == "--subscription-editor-design") {
        app.show()?;
        return render_subscription_editor(&app, &window, output);
    }
    if native_common || std::env::args().any(|arg| arg == "--common-config-design") {
        app.show()?;
        return render_common_config(&app, &window, output);
    }
    if native_provider || std::env::args().any(|arg| arg == "--provider-editor-design") {
        app.show()?;
        return render_provider_editor(&app, &window, output);
    }
    if std::env::args().any(|arg| arg == "--reasoning-picker") {
        app.show()?;
        return render_reasoning_picker(&app, &window, output);
    }
    if std::env::args().any(|arg| arg == "--connection-workbench") {
        app.show()?;
        return render_connection_workbench(&app, &window, output);
    }
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
        click(&window, 500.0, 548.0);
        render(&window);
        assert!(
            app.get_status_directories_expanded(),
            "directory disclosure must expand"
        );
        click(&window, 816.0, 526.0);
        render(&window);
        assert_eq!(
            CLIPBOARD.with(|value| value.borrow().clone()),
            app.get_config_home().as_str()
        );
        click(&window, 816.0, 580.0);
        render(&window);
        assert_eq!(
            CLIPBOARD.with(|value| value.borrow().clone()),
            app.get_data_path().as_str()
        );
        click(&window, 760.0, 684.0);
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
        click(&window, 550.0, 625.0);
        click(&window, 20.0, 200.0);
        assert_eq!(restored.get(), 0, "busy restore must not run");
        assert!(app.get_drawer_open(), "busy outside click must not dismiss");
        app.set_busy(false);
        render(&window);
        click(&window, 550.0, 625.0);
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
                    position: slint::LogicalPosition::new(500.0, 270.0),
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
    app.set_requests(ModelRc::new(VecModel::from(vec![activity_row(0, 0)])));
    if std::env::args().any(|arg| arg == "--activity-design") {
        app.show()?;
        return render_activity_design(&app, &window, output);
    }
    if std::env::args().any(|arg| arg == "--workspace-feedback") {
        app.show()?;
        return render_workspace_feedback(&app, &window, output);
    }
    if std::env::args().any(|arg| arg == "--sidebar-motion") {
        app.show()?;
        return render_sidebar_motion(&app, &window, output);
    }
    if std::env::args().any(|arg| arg == "--accounts-design" || arg == "--connections-motion") {
        app.set_active_page(1);
        app.set_connections_tab(1);
        app.set_status_text("合成账号界面检查；未访问实际账号或 Codex 配置".into());
        app.show()?;
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
        click(&window, 1126.0, 308.0);
        snapshot(&window, output, "accounts-menu-dark-1200x820")?;
        click(&window, 1000.0, 434.0);
        render(&window);
        assert!(app.get_account_login_confirm_open());
        assert_eq!(writes.get(), 0, "Selecting login must only open its review");
        click(&window, 720.0, 468.0);
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
