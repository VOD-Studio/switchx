use std::{error::Error, path::Path};

use i_slint_backend_testing::ElementHandle;
use slint::platform::{WindowEvent, software_renderer::MinimalSoftwareWindow};
use slint::{ComponentHandle, Global, LogicalPosition, PhysicalSize};
use switchx::{
    headless,
    mcp::{
        Access,
        setup::{self, Client, Launch},
    },
    ui::{AppWindow, McpSetup, Theme},
};

use crate::support::{every, frame, play, render, set_theme, snapshot};

const EXECUTABLE: &str = "/Applications/SwitchX.app/Contents/MacOS/switchx";

/// Serves the registration text for a synthetic installation.
fn connect(app: &AppWindow) {
    let mcp = McpSetup::get(app);
    let launch = || Launch {
        executable: Path::new(EXECUTABLE),
        data_dir: None,
        codex_home: None,
    };
    mcp.on_command(move |client, writes, control, _| {
        setup::registration(
            Client::from_index(client),
            Access { writes, control },
            launch(),
        )
        .into()
    });
    mcp.on_request(move |client, writes, control, _| {
        setup::request(
            Client::from_index(client),
            Access { writes, control },
            launch(),
        )
        .into()
    });
    mcp.on_tool_count(|writes, control| setup::tool_count(Access { writes, control }) as i32);
}

fn point(app: &AppWindow, label: &str) -> LogicalPosition {
    let item = ElementHandle::find_by_accessible_label(app, label)
        .next()
        .unwrap_or_else(|| panic!("toolbox control is missing: {label}"));
    let position = item.absolute_position();
    let size = item.size();
    LogicalPosition::new(
        position.x + size.width / 2.0,
        position.y + size.height / 2.0,
    )
}

fn click(app: &AppWindow, window: &MinimalSoftwareWindow, label: &str) {
    let position = point(app, label);
    headless::click(window, position.x, position.y);
    headless::render(window);
}

fn reset(app: &AppWindow, window: &MinimalSoftwareWindow) {
    app.set_active_page(0);
    render(window);
    app.set_busy(false);
    app.set_recovery_only(false);
    app.set_config_home("/Users/preview/.codex".into());
    app.set_mcp_client(0);
    app.set_mcp_allow_writes(false);
    app.set_mcp_allow_control(false);
    app.set_mcp_registration(0);
    app.set_mcp_registered_writes(false);
    app.set_mcp_registered_control(false);
    app.set_agent_channel(1);
    app.set_agent_channel_problem("".into());
    app.set_active_page(4);
    render(window);
    render(window);
}

fn scroll_to_end(app: &AppWindow, window: &MinimalSoftwareWindow) {
    headless::scroll(
        window,
        700.0,
        app.window().size().height as f32 - 100.0,
        -10_000.0,
    );
    render(window);
}

pub(super) fn render_toolbox_design(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    connect(app);
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            app.global::<Theme>().set_animations_enabled(false);
            set_theme(app, window, dark);
            reset(app, window);
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            snapshot(window, output, &format!("toolbox-{suffix}"))?;
            scroll_to_end(app, window);
            snapshot(window, output, &format!("toolbox-end-{suffix}"))?;

            reset(app, window);
            app.set_mcp_allow_writes(true);
            app.set_mcp_allow_control(true);
            app.set_agent_channel(0);
            app.set_mcp_registration(1);
            app.set_mcp_registered_writes(true);
            render(window);
            snapshot(window, output, &format!("toolbox-full-access-{suffix}"))?;

            reset(app, window);
            app.set_mcp_client(1);
            app.set_mcp_allow_writes(true);
            render(window);
            snapshot(window, output, &format!("toolbox-claude-{suffix}"))?;

            reset(app, window);
            app.set_mcp_client(2);
            app.set_agent_channel(2);
            app.set_agent_channel_problem("Address already in use".into());
            render(window);
            snapshot(window, output, &format!("toolbox-json-{suffix}"))?;

            reset(app, window);
            app.set_mcp_registration(2);
            render(window);
            snapshot(window, output, &format!("toolbox-other-copy-{suffix}"))?;
        }
    }

    app.window().set_size(PhysicalSize::new(1200, 820));
    app.global::<Theme>().set_animations_enabled(false);
    set_theme(app, window, false);
    reset(app, window);
    app.set_active_page(0);
    render(window);
    app.global::<Theme>().set_animations_enabled(true);
    app.set_active_page(4);
    play(window, output, "toolbox-entry", &every(80, 2960))?;
    click(app, window, "允许修改模型与价格");
    play(window, output, "toolbox-writes", &every(80, 640))?;
    click(app, window, "复制一句话");
    play(window, output, "toolbox-copy", &every(80, 2000))?;
    click(app, window, "允许 agent 控制 SwitchX");
    play(window, output, "toolbox-channel-off", &every(80, 480))?;
    app.set_busy(false);
    app.set_agent_channel(0);
    play(window, output, "toolbox-channel-closed", &every(80, 480))?;
    app.set_agent_channel(1);
    play(window, output, "toolbox-channel-open", &every(80, 1360))?;
    window.dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(190.0, 70.0),
    });
    frame(16);
    app.global::<Theme>().set_animations_enabled(false);
    println!("Synthetic toolbox previews: {}", output.display());
    Ok(())
}
