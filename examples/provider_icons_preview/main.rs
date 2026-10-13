//! Render synthetic previews with the real Slint UI and no account or configuration access.
//! Run: cargo run --example provider_icons_preview -- /absolute/output/directory
//! Connection cards: add --connections-design (snapshots) or --connections-native (window).
//! Tab transitions: add --connections-motion (frames and interruption checks).
//! Sidebar navigation: add --sidebar-motion (frames and settle checks).
//! macOS titlebar spacing: add --native-titlebar-overlay to software previews.
//! Batch model picker: add --batch-models.
//! Connection directory/workbench: add --connection-workbench; hover cards: --workbench-hover.
//! Shared workspace menu and operation feedback: add --workspace-feedback.
//! Activity timeline: add --activity-design (entry, arrivals, pause, details, filter).
//! API connection editor: add --provider-editor-design.
//! Add-connection navigation: add --connection-picker-motion (frames and back actions).
//! ChatGPT subscription editor: add --subscription-editor-design.
//! Grok subscription editor: add --xai-editor-design.
//! Reasoning multi-select: add --reasoning-picker (themes, sizes, and motion).
//! Shared configuration: add --common-config-design or --common-config-native.
//! Settings: add --settings-design (themes, layouts, states, and motion).
//! Model mapping editor: add --model-editor-design (states, actions, and motion)
//! or --model-editor-native (window; add --light for the light theme).
//! Route confirmation: add --route-publish-design or --route-publish-native.
//! Also: --connection-status, --accounts-design, --codex-quota, --xai-quota,
//! --quota-loading-motion, --check-progress, --route-launch, and --layout-only.

mod accounts;
mod activity;
mod connections;
mod editors;
mod fixtures;
mod models;
mod overview;
mod publish;
mod settings;
mod status;
mod support;
mod workspace;

use std::{error::Error, fs, path::Path};

use slint::ComponentHandle;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
#[cfg(target_os = "macos")]
use switchx::macos;
use switchx::{
    headless,
    ui::{AppWindow, Theme},
};

fn flag(name: &str) -> bool {
    std::env::args().any(|arg| arg == name)
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
    let native_connections = flag("--connections-native");
    let native_provider = flag("--provider-editor-native");
    let native_common = flag("--common-config-native");
    let native_model = flag("--model-editor-native");
    let native_publish = flag("--route-publish-native");
    let window =
        if native_connections || native_provider || native_common || native_model || native_publish
        {
            #[cfg(target_os = "macos")]
            macos::configure_window()?;
            MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer)
        } else {
            headless::install_with_manual_time()
        };
    let app = AppWindow::new()?;
    app.set_app_version(env!("CARGO_PKG_VERSION").into());
    switchx::model_picker::connect(&app);
    app.set_native_titlebar_overlay(flag("--native-titlebar-overlay"));
    switchx::reasoning_picker::connect(&app);
    app.set_loading(false);
    app.global::<Theme>().set_animations_enabled(false);
    if native_publish || flag("--route-publish-design") {
        app.show()?;
        return publish::render_route_publish(&app, &window, output, native_publish);
    }
    if flag("--settings-design") {
        app.show()?;
        return settings::render_settings_design(&app, &window, output);
    }
    if flag("--connection-picker-motion") {
        app.show()?;
        return connections::render_connection_picker_motion(&app, &window, output);
    }
    if flag("--subscription-editor-design") {
        app.show()?;
        return editors::render_subscription_editor(&app, &window, output);
    }
    if native_common || flag("--common-config-design") {
        app.show()?;
        return editors::render_common_config(&app, &window, output);
    }
    if flag("--xai-editor-design") {
        app.show()?;
        return editors::render_xai_editor(&app, &window, output);
    }
    if native_provider || flag("--provider-editor-design") {
        app.show()?;
        return editors::render_provider_editor(&app, &window, output);
    }
    if flag("--reasoning-picker") {
        app.show()?;
        return models::render_reasoning_picker(&app, &window, output);
    }
    if native_model || flag("--model-editor-design") {
        app.show()?;
        return models::render_model_editor(&app, &window, output);
    }
    if flag("--connection-workbench") || flag("--workbench-hover") {
        app.show()?;
        return connections::render_connection_workbench(
            &app,
            &window,
            output,
            flag("--workbench-hover"),
        );
    }
    if flag("--connection-status") {
        return status::render_connection_status(&app, &window, output);
    }
    app.set_active_page(1);
    app.set_status_text("离屏预览 · 合成资料".into());
    let icons = fixtures::icons()?;
    let providers = fixtures::providers(&icons);
    if native_connections || flag("--connections-design") {
        return connections::render_cards(&app, &window, output, &providers, native_connections);
    }
    fixtures::seed_workbench(&app, providers, &icons);
    if flag("--activity-design") {
        app.show()?;
        return activity::render_activity_design(&app, &window, output);
    }
    if flag("--workspace-feedback") {
        app.show()?;
        return workspace::render_workspace_feedback(&app, &window, output);
    }
    if flag("--sidebar-motion") {
        app.show()?;
        return workspace::render_sidebar_motion(&app, &window, output);
    }
    let motion = flag("--connections-motion");
    if motion || flag("--accounts-design") {
        return accounts::render_design(&app, &window, output, motion);
    }
    if flag("--codex-quota") {
        return accounts::render_codex_quota(&app, &window, output);
    }
    if flag("--xai-quota") {
        return accounts::render_xai_quota(&app, &window, output);
    }
    if flag("--quota-loading-motion") {
        return accounts::render_quota_loading_motion(&app, &window, output);
    }
    if flag("--check-progress") {
        return connections::render_check_progress(&app, &window, output);
    }
    if flag("--batch-models") {
        models::render_batch_models(&app, &window, output)?;
        println!("Synthetic batch model previews: {}", output.display());
        return Ok(());
    }
    overview::render_overview(
        &app,
        &window,
        output,
        &icons,
        flag("--layout-only"),
        flag("--route-launch"),
    )
}
