use std::{error::Error, path::Path};

use i_slint_backend_testing::ElementHandle;
use slint::platform::{WindowEvent, software_renderer::MinimalSoftwareWindow};
use slint::{ComponentHandle, LogicalPosition, PhysicalSize};
use switchx::{
    headless,
    ui::{AppWindow, Theme},
};

use crate::support::{every, frame, play, render, set_theme, snapshot};

fn control_any(app: &AppWindow, window: &MinimalSoftwareWindow, labels: &[&str]) -> ElementHandle {
    let height = app.window().size().height as f32;
    for attempt in 0..16 {
        let item = labels.iter().find_map(|label| {
            let mut matches = ElementHandle::find_by_accessible_label(app, label);
            let item = matches.next();
            assert!(
                matches.next().is_none(),
                "ambiguous settings control: {label}"
            );
            item
        });
        let mut delta = if attempt == 0 { 10_000.0 } else { -150.0 };
        if let Some(item) = item {
            let y = item.absolute_position().y + item.size().height / 2.0;
            if y > 175.0 && y < height - 30.0 {
                return item;
            }
            delta = if y < 175.0 { 150.0 } else { -150.0 };
        }
        headless::scroll(window, 700.0, height - 100.0, delta);
        render(window);
    }
    panic!("settings controls are not reachable: {labels:?}");
}

fn point(app: &AppWindow, window: &MinimalSoftwareWindow, label: &str) -> LogicalPosition {
    let item = control_any(app, window, &[label]);
    let position = item.absolute_position();
    let size = item.size();
    LogicalPosition::new(
        position.x + size.width / 2.0,
        position.y + size.height / 2.0,
    )
}

fn click(app: &AppWindow, window: &MinimalSoftwareWindow, label: &str) {
    let position = point(app, window, label);
    headless::click(window, position.x, position.y);
    headless::render(window);
}

fn reset(app: &AppWindow, window: &MinimalSoftwareWindow) {
    app.set_active_page(0);
    headless::render(window);
    app.set_busy(false);
    app.set_config_managed(false);
    app.set_recovery_only(false);
    app.set_chatgpt_login_pending(false);
    app.set_xai_pending(false);
    app.set_config_home("/Users/preview/.codex".into());
    app.set_route_port("18731".into());
    app.set_config_status("Codex 官方连接 · 登录由 Codex 管理 · openai · synthetic-coder".into());
    app.set_active_page(5);
    render(window);
    let disclosure = control_any(app, window, &["收起配置与恢复", "展开配置与恢复"]);
    if disclosure.accessible_expanded() == Some(true) {
        click(app, window, "收起配置与恢复");
        render(window);
    }
    headless::scroll(
        window,
        700.0,
        app.window().size().height as f32 - 100.0,
        10_000.0,
    );
    render(window);
}

fn reveal_recovery(app: &AppWindow, window: &MinimalSoftwareWindow) {
    render(window);
    headless::scroll(
        window,
        700.0,
        app.window().size().height as f32 - 100.0,
        -10_000.0,
    );
    render(window);
}

pub(super) fn render_settings_design(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            app.global::<Theme>().set_animations_enabled(false);
            set_theme(app, window, dark);
            reset(app, window);
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            app.global::<Theme>().set_animations_enabled(true);
            for _ in 0..30 {
                frame(16);
                headless::render(window);
            }
            snapshot(window, output, &format!("settings-{suffix}"))?;
            app.global::<Theme>().set_animations_enabled(false);
            click(app, window, "展开配置与恢复");
            reveal_recovery(app, window);
            snapshot(window, output, &format!("settings-recovery-{suffix}"))?;
            app.set_config_status(
                format!(
                    "合成服务连接 · {} · {}",
                    "synthetic-provider-".repeat(6),
                    "synthetic-model-".repeat(8)
                )
                .into(),
            );
            reveal_recovery(app, window);
            snapshot(window, output, &format!("settings-long-status-{suffix}"))?;
            app.set_config_managed(true);
            app.set_config_status("SwitchX 正在管理此工作空间 · 配置恢复后可继续编辑。".into());
            snapshot(window, output, &format!("settings-managed-{suffix}"))?;
            app.set_busy(true);
            snapshot(window, output, &format!("settings-busy-{suffix}"))?;
            reset(app, window);
            app.set_recovery_only(true);
            app.set_config_managed(true);
            app.set_config_status(
                "检测到外部配置修改。查看当前配置并处理冲突后，再恢复原配置。".into(),
            );
            reveal_recovery(app, window);
            snapshot(window, output, &format!("settings-conflict-{suffix}"))?;
        }
    }

    app.window().set_size(PhysicalSize::new(1200, 820));
    app.global::<Theme>().set_animations_enabled(false);
    set_theme(app, window, true);
    reset(app, window);
    app.set_active_page(0);
    render(window);
    app.global::<Theme>().set_animations_enabled(true);
    app.set_active_page(5);
    play(window, output, "settings-entry", &every(16, 640))?;

    window.dispatch_event(WindowEvent::PointerMoved {
        position: point(app, window, "浅色主题"),
    });
    play(window, output, "settings-theme-hover", &every(16, 240))?;
    click(app, window, "浅色主题");
    play(window, output, "settings-theme-select", &every(16, 400))?;
    click(app, window, "播放动效预览");
    window.dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(190.0, 70.0),
    });
    play(window, output, "settings-motion-preview", &every(16, 640))?;
    click(app, window, "展开配置与恢复");
    play(window, output, "settings-disclosure", &every(16, 400))?;
    click(app, window, "收起配置与恢复");
    frame(48);
    headless::render(window);
    click(app, window, "展开配置与恢复");
    play(
        window,
        output,
        "settings-disclosure-reversed",
        &every(16, 400),
    )?;

    app.global::<Theme>().set_system_reduced_motion(true);
    reset(app, window);
    snapshot(window, output, "settings-reduced-motion")?;
    app.global::<Theme>().set_system_reduced_motion(false);
    app.global::<Theme>().set_animations_enabled(false);
    println!("Synthetic settings previews: {}", output.display());
    Ok(())
}
