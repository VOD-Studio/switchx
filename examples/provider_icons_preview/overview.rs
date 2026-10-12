use std::{error::Error, path::Path};

use slint::platform::{WindowEvent, software_renderer::MinimalSoftwareWindow};
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};
use switchx::{
    provider_icons,
    ui::{AppWindow, ProviderIconRow},
};

use crate::{
    fixtures::icon,
    support::{render_icons, set_theme, snapshot},
};

/// Every page, the connection editor, and the avatar picker; then high-DPI icons.
pub(super) fn render_overview(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
    icons: &[ProviderIconRow],
    layout_only: bool,
    route_launch: bool,
) -> Result<(), Box<dyn Error>> {
    let custom = icon(icons, "google");
    let openai = icon(icons, "openai");
    if route_launch {
        app.set_route_running(true);
        app.set_config_managed(true);
    }
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            set_theme(app, window, dark);
            app.set_icon_picker_open(false);
            app.set_editor_open(false);
            app.set_subscription_editor_open(false);
            app.set_active_page(0);
            snapshot(window, output, &format!("workbench-{suffix}"))?;
            app.set_active_page(3);
            snapshot(window, output, &format!("activity-{suffix}"))?;
            app.set_active_page(5);
            snapshot(window, output, &format!("settings-{suffix}"))?;
            app.set_active_page(4);
            snapshot(window, output, &format!("toolbox-{suffix}"))?;
            app.set_active_page(1);
            app.set_connections_tab(0);
            snapshot(window, output, &format!("connections-{suffix}"))?;
            app.set_connections_tab(1);
            snapshot(window, output, &format!("accounts-{suffix}"))?;
            window.dispatch_event(WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(width as f32 - 50.0, height as f32 - 80.0),
                delta_x: 0.0,
                delta_y: -1000.0,
            });
            snapshot(window, output, &format!("accounts-actions-{suffix}"))?;
            app.set_connections_tab(0);
            if layout_only {
                continue;
            }
            // Exercise every icon at the actual picker size, including those below the first screen.
            render_icons(app, window, icons);
            snapshot(window, output, &format!("providers-{suffix}"))?;

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
            snapshot(window, output, &format!("custom-editor-{suffix}"))?;

            app.set_filtered_provider_icons(ModelRc::new(VecModel::from(icons.to_vec())));
            app.set_provider_icon_query("".into());
            app.set_icon_picker_open(true);
            snapshot(window, output, &format!("picker-{suffix}"))?;

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
            snapshot(window, output, &format!("search-openai-{suffix}"))?;

            app.set_provider_icon_query("no-match-for-this-icon".into());
            app.set_filtered_provider_icons(ModelRc::new(VecModel::from(Vec::new())));
            snapshot(window, output, &format!("search-empty-{suffix}"))?;
        }
    }
    if layout_only {
        println!("Synthetic Slint layout previews: {}", output.display());
        return Ok(());
    }
    window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: 2.0 });
    app.window().set_size(PhysicalSize::new(2360, 1600));
    for dark in [false, true] {
        set_theme(app, window, dark);
        render_icons(app, window, icons);
    }
    println!("Synthetic software-rendered previews: {}", output.display());
    Ok(())
}
