//! Navigation and workspace metadata are separate from configuration changes.

use std::path::{Path, PathBuf};

use slint::ComponentHandle;
use switchx::{
    app,
    clients::{ClientKind, WorkspaceKey},
    ui::AppWindow,
};
use tokio::sync::mpsc;

use super::{
    command::{Command, queue},
    feedback::show_action,
    worker::Worker,
};

pub(super) fn initialize(window: &AppWindow) {
    if let Ok(home) = ClientKind::ClaudeCode.default_home() {
        window.set_claude_home(home.to_string_lossy().as_ref().into());
    }
}

fn load_workspaces(
    data_dir: Option<&Path>,
) -> Vec<(
    ClientKind,
    PathBuf,
    Option<switchx::clients::ClientWorkspace>,
)> {
    let mut loaded = Vec::new();
    for client in ClientKind::ALL {
        let default = client.default_home();
        let home = data_dir
            .filter(|_| std::env::var_os(client.home_variable()).is_none())
            .and_then(|dir| app::selected_client_workspace(dir, client).ok().flatten())
            .map(|workspace| workspace.key.home)
            .or_else(|| default.ok());
        let Some(home) = home else {
            continue;
        };
        let workspace =
            data_dir.and_then(|dir| app::remember_client_workspace(dir, client, &home).ok());
        loaded.push((client, home, workspace));
    }
    loaded
}

pub(super) fn connect(window: &AppWindow, sender: &mpsc::Sender<Command>) {
    let weak = window.as_weak();
    let navigation = sender.clone();
    window.on_client_navigation(move |previous, page| {
        if let Some(window) = weak.upgrade() {
            let page = if previous == 0 && !matches!(page, 0 | 1 | 3) {
                window.get_codex_last_page()
            } else {
                page
            };
            let workspace = if previous == 1 {
                window.get_claude_workspace_id()
            } else {
                window.get_codex_workspace_id()
            };
            queue(
                &window,
                &navigation,
                Command::ClientNavigation {
                    workspace_id: workspace.into(),
                    page,
                },
            );
        }
    });
    let weak = window.as_weak();
    let inspect = sender.clone();
    window.on_inspect_claude(move |home| {
        if let Some(window) = weak.upgrade() {
            queue(&window, &inspect, Command::InspectClaude(home.into()));
        }
    });
}

impl Worker {
    pub(super) fn initialize_client_workspaces(&mut self) {
        // Database creation, online backups and migrations stay off the UI thread.
        let data_dir = if self.recovery_only {
            None
        } else {
            self.data_dir.ok()
        };
        let workspaces = load_workspaces(data_dir);
        let codex_home = workspaces
            .iter()
            .find(|(client, _, _)| *client == ClientKind::Codex)
            .map(|(_, home, _)| home.to_string_lossy().into_owned())
            .unwrap_or_default();
        let _ = self.weak.upgrade_in_event_loop(move |window| {
            for (client, home, workspace) in workspaces {
                match client {
                    ClientKind::Codex => {
                        window.set_config_home(home.to_string_lossy().as_ref().into());
                        if let Some(workspace) = workspace {
                            window.set_codex_workspace_id(workspace.id.into());
                            if matches!(workspace.last_page, 0 | 1 | 3) {
                                window.set_codex_last_page(workspace.last_page);
                            }
                        }
                    }
                    ClientKind::ClaudeCode => {
                        window.set_claude_home(home.to_string_lossy().as_ref().into());
                        if let Some(workspace) = workspace {
                            window.set_claude_workspace_id(workspace.id.into());
                        }
                    }
                }
            }
        });
        self.inspect_config(codex_home);
        let _ = self
            .weak
            .upgrade_in_event_loop(|window| window.invoke_usage_refresh());
    }

    pub(super) fn client_navigation(&mut self, workspace_id: String, page: i32) {
        self.discard_previews();
        let result = if !self.recovery_only && !workspace_id.is_empty() && matches!(page, 0 | 1 | 3)
        {
            self.data_dir
                .path()
                .and_then(|dir| app::save_client_page(dir, &workspace_id, page))
        } else {
            Ok(())
        };
        let _ = self.weak.upgrade_in_event_loop(move |window| {
            window.set_busy(false);
            window.set_direct_preview_ready(false);
            window.set_route_preview_ready(false);
            if let Err(error) = result {
                show_action(&window, Err(error));
            }
        });
    }

    pub(super) fn inspect_claude(&mut self, home: String) {
        let result: Result<_, String> = (|| {
            let key = WorkspaceKey::new(ClientKind::ClaudeCode, &PathBuf::from(home))?;
            let status = match std::fs::metadata(key.config_path()) {
                Ok(metadata) if metadata.is_file() => "发现 settings.json；尚未接管配置",
                Ok(_) => return Err("settings.json 不是普通文件".into()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    "settings.json 尚不存在；未创建目录或配置"
                }
                Err(_) => return Err("无法访问 Claude Code 配置文件".into()),
            };
            let workspace = if self.recovery_only {
                None
            } else {
                Some(app::remember_client_workspace(
                    self.data_dir.path()?,
                    ClientKind::ClaudeCode,
                    &key.home,
                )?)
            };
            Ok((key, workspace, status))
        })();
        let _ = self.weak.upgrade_in_event_loop(move |window| match result {
            Ok((key, workspace, status)) => {
                window.set_claude_home(key.home.to_string_lossy().as_ref().into());
                window.set_claude_status(status.into());
                if let Some(workspace) = workspace {
                    window.set_claude_workspace_id(workspace.id.into());
                }
                show_action(
                    &window,
                    Ok("已检查 Claude Code 目录，未改写客户端配置".into()),
                );
            }
            Err(error) => {
                window.set_claude_status(error.as_str().into());
                show_action(&window, Err(error));
            }
        });
    }
}
