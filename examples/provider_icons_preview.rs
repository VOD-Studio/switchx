//! Render synthetic provider avatar previews with the real Slint UI, without native windows.
//! Run: cargo run --example provider_icons_preview -- /absolute/output/directory

slint::include_modules!();

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, ModelRc, PhysicalSize, Rgb8Pixel, SharedPixelBuffer, VecModel};
use std::{cell::Cell, error::Error, fs, io::Write, path::Path, rc::Rc, time::Duration};
use switchx::provider_icons;

thread_local! { static PREVIEW_TIME: Cell<Duration> = const { Cell::new(Duration::ZERO) }; }

struct PreviewPlatform(Rc<MinimalSoftwareWindow>);

impl Platform for PreviewPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
    fn duration_since_start(&self) -> Duration {
        PREVIEW_TIME.with(Cell::get)
    }
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
            snapshot(&window, output, &format!("connections-{suffix}"))?;
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
