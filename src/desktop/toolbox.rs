//! The toolbox page's agent access: the registration command for the chosen
//! client, one sentence any receiving client can use to register itself, the
//! Codex home's current registration, and the switch for the app's agent
//! control channel.

use std::path::{Path, PathBuf};

use slint::{ComponentHandle, Global};
use switchx::{
    codex::client,
    credential_helper,
    mcp::{
        Access,
        setup::{self, Client, Launch},
    },
    ui::{AppWindow, McpSetup},
    upstream,
};
use tokio::sync::mpsc;

use super::command::{Command, queue};

pub(super) fn connect(app: &AppWindow, sender: &mpsc::Sender<Command>) {
    let executable = credential_helper::executable().ok();
    let registered_executable = executable.clone();
    let data_dir = std::env::var_os("SWITCHX_DATA_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    let default_home = client::default_home().ok();
    let mcp = McpSetup::get(app);
    mcp.set_executable_warning(
        match &executable {
            None => "无法定位 SwitchX 程序，请从安装位置重新打开 SwitchX。".into(),
            Some(path) => upstream::ensure_persistent_helper(path)
                .err()
                .map(|_| {
                    "SwitchX 正从磁盘映像或临时位置运行，注册的路径会在推出或重启后失效。请先移到「应用程序」再注册。"
                        .to_owned()
                })
                .unwrap_or_default(),
        }
        .into(),
    );
    let launch = move |home: &str, render: &dyn Fn(Launch) -> String| -> String {
        let Some(executable) = &executable else {
            return String::new();
        };
        let home = Path::new(home);
        render(Launch {
            executable,
            data_dir: data_dir.as_deref(),
            codex_home: (home.is_absolute() && Some(home) != default_home.as_deref())
                .then_some(home),
        })
    };
    let launch = std::rc::Rc::new(launch);
    let command = launch.clone();
    mcp.on_command(move |kind, writes, control, home| {
        command(&home, &|launch| {
            setup::registration(Client::from_index(kind), Access { writes, control }, launch)
        })
        .into()
    });
    mcp.on_request(move |_kind, writes, control, home| {
        launch(&home, &|launch| {
            setup::request(Access { writes, control }, launch)
        })
        .into()
    });
    mcp.on_tool_count(|writes, control| setup::tool_count(Access { writes, control }) as i32);

    let sender = sender.clone();
    let weak = app.as_weak();
    app.on_set_agent_channel(move |enabled| {
        if let Some(app) = weak.upgrade() {
            queue(&app, &sender, Command::SetAgentControl(enabled));
        }
    });

    let weak = app.as_weak();
    app.on_refresh_mcp_registration(move |home| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let home = Path::new(home.as_str());
        let registered = registered_executable
            .as_deref()
            .filter(|_| home.is_absolute())
            .and_then(|executable| setup::codex_registration(home, executable));
        app.set_mcp_registration(match registered {
            None => 0,
            Some(registered) if registered.current => 1,
            Some(_) => 2,
        });
        let access = registered
            .map(|registered| registered.access)
            .unwrap_or_default();
        app.set_mcp_registered_writes(access.writes);
        app.set_mcp_registered_control(access.control);
    });
}
