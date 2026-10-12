use std::{cell::Cell, error::Error, path::Path, rc::Rc};

use slint::platform::{WindowEvent, software_renderer::MinimalSoftwareWindow};
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};
use switchx::{
    headless,
    ui::{AppWindow, ProviderRow},
};

use crate::support::{render, set_theme, snapshot};

/// The connection status dialog: states, directory disclosure, copying, and restore.
pub(super) fn render_connection_status(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
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
            set_theme(app, window, dark);
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            for state in ["collapsed", "expanded", "recovery", "busy"] {
                app.set_drawer_open(false);
                render(window);
                app.set_drawer_open(true);
                render(window);
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
                snapshot(window, output, &format!("status-{state}-{suffix}"))?;
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
    render(window);
    app.set_drawer_open(true);
    render(window);
    headless::click(window, 500.0, 548.0);
    render(window);
    assert!(
        app.get_status_directories_expanded(),
        "directory disclosure must expand"
    );
    headless::click(window, 816.0, 526.0);
    render(window);
    assert_eq!(headless::clipboard(), app.get_config_home().as_str());
    headless::click(window, 816.0, 580.0);
    render(window);
    assert_eq!(headless::clipboard(), app.get_data_path().as_str());
    headless::click(window, 760.0, 684.0);
    render(window);
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
    render(window);
    assert!(
        !app.get_status_directories_expanded(),
        "reopening must collapse directories"
    );
    app.set_busy(true);
    render(window);
    headless::click(window, 550.0, 625.0);
    headless::click(window, 20.0, 200.0);
    assert_eq!(restored.get(), 0, "busy restore must not run");
    assert!(app.get_drawer_open(), "busy outside click must not dismiss");
    app.set_busy(false);
    render(window);
    headless::click(window, 550.0, 625.0);
    assert_eq!(
        restored.get(),
        1,
        "restore must target the current Codex home"
    );
    headless::click(window, 20.0, 200.0);
    render(window);
    assert!(
        !app.get_drawer_open(),
        "outside click must dismiss when idle"
    );
    println!(
        "Synthetic connection-status previews and interaction checks: {}",
        output.display()
    );
    Ok(())
}
