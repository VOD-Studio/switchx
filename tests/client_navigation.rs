use std::{cell::Cell, rc::Rc};

use i_slint_backend_testing::ElementHandle;
use slint::{
    ComponentHandle, ModelRc, PhysicalSize, VecModel,
    platform::software_renderer::MinimalSoftwareWindow,
};
use switchx::{
    headless,
    ui::{AppWindow, ProviderRow, Theme},
};

fn settle(window: &MinimalSoftwareWindow) {
    for _ in 0..3 {
        headless::advance(400);
        headless::render(window);
    }
}

fn click(window: &MinimalSoftwareWindow, app: &AppWindow, label: &str) {
    let mut items = ElementHandle::find_by_accessible_label(app, label);
    let item = items.next().unwrap_or_else(|| panic!("missing {label}"));
    assert!(items.next().is_none(), "ambiguous {label}");
    let point = item.absolute_position();
    let size = item.size();
    assert!(size.width > 0.0 && size.height > 0.0);
    assert!(point.x >= 0.0 && point.y >= 0.0);
    assert!(point.y + size.height <= app.window().size().height as f32);
    headless::click(
        window,
        point.x + size.width / 2.0,
        point.y + size.height / 2.0,
    );
    settle(window);
}

#[test]
fn switching_client_views_preserves_codex_runtime_and_clears_credential_editors() {
    let window = headless::install_with_manual_time();
    let app = AppWindow::new().unwrap();
    app.set_app_version(env!("CARGO_PKG_VERSION").into());
    app.set_loading(false);
    app.set_busy(false);
    app.set_config_home("/isolated/codex".into());
    app.set_claude_home("/isolated/claude".into());
    app.set_route_running(true);
    app.set_route_managed(true);
    app.set_config_managed(true);
    app.set_route_status("Codex · 合成路由运行中".into());
    app.set_claude_status("合成配置目录；未接管".into());
    let providers = ModelRc::new(VecModel::from(vec![ProviderRow {
        id: "synthetic-provider".into(),
        name: "示例 API".into(),
        endpoint: "api.example.test".into(),
        ..Default::default()
    }]));
    app.set_providers(providers.clone());
    app.set_filtered_providers(providers);
    let writes = Rc::new(Cell::new(0));
    let calls = writes.clone();
    app.on_apply_route(move || calls.set(calls.get() + 1));
    let calls = writes.clone();
    app.on_apply_direct(move || calls.set(calls.get() + 1));
    let calls = writes.clone();
    app.on_restore_config(move |_| calls.set(calls.get() + 1));
    app.show().unwrap();

    for (width, height) in [(1000, 680), (1400, 900)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            app.invoke_set_appearance(dark);
            app.global::<Theme>().set_animations_enabled(true);
            app.set_active_page(1);
            settle(&window);
            click(&window, &app, "管理 Claude Code");
            assert_eq!(app.get_active_client(), 1);
            assert_eq!(app.get_active_page(), 6);
            assert!(app.get_route_running());
            assert!(app.get_route_managed());
            assert_eq!(app.get_config_home(), "/isolated/codex");
            assert_eq!(app.get_claude_home(), "/isolated/claude");
            let pixels = headless::render(&window);
            headless::save_snapshot(
                "SWITCHX_CLIENT_SNAPSHOTS",
                &format!("claude-{width}-{}", if dark { "dark" } else { "light" }),
                &pixels,
            );
            click(&window, &app, "管理 Codex");
            assert_eq!(app.get_active_client(), 0);
            assert_eq!(app.get_active_page(), 1);
            let pixels = headless::render(&window);
            headless::save_snapshot(
                "SWITCHX_CLIENT_SNAPSHOTS",
                &format!("codex-{width}-{}", if dark { "dark" } else { "light" }),
                &pixels,
            );
        }
    }
    app.global::<Theme>().set_system_reduced_motion(true);
    // The client buttons retain keyboard activation when their page changes.
    app.set_active_page(6);
    settle(&window);
    headless::key(&window, slint::platform::Key::Return);
    settle(&window);
    assert_eq!(app.get_active_client(), 0);
    assert_eq!(app.get_active_page(), 1);
    app.set_active_page(3);
    settle(&window);
    app.set_active_page(2);
    settle(&window);
    app.invoke_choose_client(1);
    settle(&window);
    app.invoke_choose_client(0);
    settle(&window);
    assert_eq!(
        app.get_active_page(),
        3,
        "global pages must preserve the last client page"
    );
    app.set_active_page(1);
    app.set_editor_open(true);
    app.set_edit_key("synthetic-key-must-clear".into());
    app.set_direct_preview_ready(true);
    app.set_route_preview_ready(true);
    app.invoke_choose_client(1);
    settle(&window);
    assert!(!app.get_editor_open());
    assert!(app.get_edit_key().is_empty());
    assert!(!app.get_direct_preview_ready());
    assert!(!app.get_route_preview_ready());
    assert!(app.get_route_running());
    assert_eq!(writes.get(), 0);
}
