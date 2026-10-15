use std::{cell::RefCell, path::Path, rc::Rc};

use i_slint_backend_testing::ElementHandle;
use slint::platform::software_renderer::MinimalSoftwareWindow;
use slint::{ComponentHandle, Global, PhysicalSize};
use switchx::{
    headless,
    mcp::{
        Access,
        setup::{self, Client, Launch},
    },
    ui::{AppWindow, McpSetup, Theme},
};

const HOME: &str = "/isolated/synthetic-codex";
const EXECUTABLE: &str = "/isolated/SwitchX.app/Contents/MacOS/switchx";

fn launch() -> Launch<'static> {
    Launch {
        executable: Path::new(EXECUTABLE),
        data_dir: None,
        codex_home: None,
    }
}

fn settle(window: &MinimalSoftwareWindow) {
    for _ in 0..3 {
        headless::advance(800);
        headless::render(window);
    }
}

fn control(window: &MinimalSoftwareWindow, app: &AppWindow, label: &str) -> ElementHandle {
    let height = app.window().size().height as f32;
    for attempt in 0..16 {
        let mut matches = ElementHandle::find_by_accessible_label(app, label);
        let item = matches.next();
        assert!(
            matches.next().is_none(),
            "ambiguous toolbox control: {label}"
        );
        let mut delta = if attempt == 0 { 10_000.0 } else { -150.0 };
        if let Some(item) = item {
            let y = item.absolute_position().y + item.size().height / 2.0;
            if y > 175.0 && y < height - 30.0 {
                return item;
            }
            delta = if y < 175.0 { 150.0 } else { -150.0 };
        }
        headless::scroll(window, 700.0, height - 100.0, delta);
        settle(window);
    }
    panic!("toolbox control is not reachable: {label}");
}

fn click(window: &MinimalSoftwareWindow, app: &AppWindow, label: &str) {
    let item = control(window, app, label);
    let position = item.absolute_position();
    let size = item.size();
    headless::click(
        window,
        position.x + size.width / 2.0,
        position.y + size.height / 2.0,
    );
    settle(window);
}

fn enabled(window: &MinimalSoftwareWindow, app: &AppWindow, label: &str) -> bool {
    control(window, app, label).accessible_enabled() == Some(true)
}

#[test]
fn toolbox_copies_the_registration_and_switches_agent_control() {
    let window = headless::install_with_manual_time();
    let app = AppWindow::new().unwrap();
    app.set_loading(false);
    app.global::<Theme>().set_animations_enabled(false);
    let mcp = McpSetup::get(&app);
    mcp.on_command(|client, writes, control, _| {
        setup::registration(
            Client::from_index(client),
            Access { writes, control },
            launch(),
        )
        .into()
    });
    mcp.on_request(|client, writes, control, _| {
        setup::request(
            Client::from_index(client),
            Access { writes, control },
            launch(),
        )
        .into()
    });
    mcp.on_tool_count(|writes, control| setup::tool_count(Access { writes, control }) as i32);
    let channel_requests = Rc::new(RefCell::new(Vec::new()));
    let requests = channel_requests.clone();
    app.on_set_agent_channel(move |enabled| requests.borrow_mut().push(enabled));
    let refreshed = Rc::new(RefCell::new(Vec::<String>::new()));
    let homes = refreshed.clone();
    app.on_refresh_mcp_registration(move |home| homes.borrow_mut().push(home.into()));
    app.set_config_home(HOME.into());
    app.show().unwrap();

    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            app.invoke_set_appearance(dark);
            app.set_active_page(0);
            settle(&window);
            app.set_mcp_client(0);
            app.set_mcp_allow_writes(false);
            app.set_mcp_allow_control(false);
            app.set_agent_channel(1);
            app.set_busy(false);
            app.set_recovery_only(false);
            refreshed.borrow_mut().clear();
            app.set_active_page(4);
            settle(&window);
            assert_eq!(refreshed.borrow().first().map(String::as_str), Some(HOME));

            // The sentence carries the read-only registration by default.
            headless::clear_clipboard();
            click(&window, &app, "复制一句话");
            let read = headless::clipboard();
            assert_eq!(
                read,
                setup::request(Client::Codex, Access::default(), launch())
            );
            assert!(read.contains(EXECUTABLE) && !read.contains("--allow"));

            // Permissions and the client feed both the command and the sentence.
            click(&window, &app, "允许修改模型与价格");
            assert!(app.get_mcp_allow_writes());
            click(&window, &app, "允许控制 Codex 配置");
            assert!(app.get_mcp_allow_control());
            click(&window, &app, "复制命令");
            let all = Access {
                writes: true,
                control: true,
            };
            assert_eq!(
                headless::clipboard(),
                setup::registration(Client::Codex, all, launch())
            );
            click(&window, &app, "Claude Code");
            assert_eq!(app.get_mcp_client(), 1);
            click(&window, &app, "复制一句话");
            assert_eq!(
                headless::clipboard(),
                setup::request(Client::ClaudeCode, all, launch())
            );
            click(&window, &app, "其他客户端");
            click(&window, &app, "复制配置");
            assert!(headless::clipboard().contains("\"mcpServers\""));
            click(&window, &app, "允许修改模型与价格");
            assert!(!app.get_mcp_allow_writes());

            // The channel switch asks the worker and never flips the state itself.
            channel_requests.borrow_mut().clear();
            click(&window, &app, "允许 agent 控制 SwitchX");
            assert_eq!(*channel_requests.borrow(), [false]);
            assert_eq!(app.get_agent_channel(), 1);
            app.set_busy(true);
            settle(&window);
            assert!(!enabled(&window, &app, "允许 agent 控制 SwitchX"));
            app.set_busy(false);
            app.set_agent_channel(0);
            settle(&window);
            assert_eq!(
                control(&window, &app, "允许 agent 控制 SwitchX").accessible_checked(),
                Some(false)
            );
            click(&window, &app, "允许 agent 控制 SwitchX");
            assert_eq!(*channel_requests.borrow(), [false, true]);
            app.set_recovery_only(true);
            settle(&window);
            assert!(!enabled(&window, &app, "允许 agent 控制 SwitchX"));
            app.set_recovery_only(false);
            app.set_agent_channel(-1);
            settle(&window);
            assert!(!enabled(&window, &app, "允许 agent 控制 SwitchX"));
            let pixels = headless::render(&window);
            headless::save_snapshot(
                "SWITCHX_TOOLBOX_SNAPSHOTS",
                &format!(
                    "toolbox-{}-{width}x{height}",
                    if dark { "dark" } else { "light" }
                ),
                &pixels,
            );
        }
    }
}
