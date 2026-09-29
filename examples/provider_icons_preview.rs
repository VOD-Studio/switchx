//! Render synthetic provider avatar previews with the real Slint UI, without native windows.
//! Run: cargo run --example provider_icons_preview -- /absolute/output/directory

slint::include_modules!();

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{
    ComponentHandle, LogicalPosition, ModelRc, PhysicalSize, Rgb8Pixel, SharedPixelBuffer, VecModel,
};
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

fn set_theme(window: &MinimalSoftwareWindow, dark: bool, width: u32) {
    // Theme is private in the generated module. Exercise its existing button in memory.
    if (render(window).as_slice()[0].r < 128) != dark {
        let position = LogicalPosition::new(width as f32 - 192.0, 64.0);
        window.dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        window.dispatch_event(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
    }
    assert_eq!(render(window).as_slice()[0].r < 128, dark);
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
    app.set_provider_icons(ModelRc::new(VecModel::from(icons.clone())));

    for (width, height) in [(1180, 800), (1040, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            set_theme(&window, dark, width);
            app.set_icon_picker_open(false);
            app.set_editor_open(false);
            app.set_subscription_editor_open(false);
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
    window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: 2.0 });
    app.window().set_size(PhysicalSize::new(2360, 1600));
    for dark in [false, true] {
        set_theme(&window, dark, 1180);
        render_icons(&app, &window, &icons);
    }
    println!("Synthetic software-rendered previews: {}", output.display());
    Ok(())
}
