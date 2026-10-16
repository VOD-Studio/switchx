//! What a user hands an agent to register `switchx mcp`: the chosen client's
//! command or configuration, and one sentence that asks whichever client
//! receives it to register the server on itself. Only paths and flags appear;
//! never credentials, tokens, or the control endpoint.

use std::{fs, path::Path};

use serde_json::Value;

use super::{Access, tools};

/// The server name agents register.
pub const SERVER_NAME: &str = "switchx";

/// Where the server is registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Client {
    Codex,
    ClaudeCode,
    /// An `mcpServers` JSON entry, for clients configured by file.
    Json,
}

impl Client {
    pub fn from_index(index: i32) -> Self {
        match index {
            1 => Self::ClaudeCode,
            2 => Self::Json,
            _ => Self::Codex,
        }
    }
}

/// How this installation starts the server.
#[derive(Debug, Clone, Copy)]
pub struct Launch<'a> {
    /// The persistent SwitchX executable.
    pub executable: &'a Path,
    /// The app's explicit `SWITCHX_DATA_DIR`, passed on so the server reads the same data.
    pub data_dir: Option<&'a Path>,
    /// A Codex home other than the default one, for `codex mcp add` to write into.
    pub codex_home: Option<&'a Path>,
}

/// The `switchx` arguments for `access`.
pub fn arguments(access: Access) -> Vec<&'static str> {
    let mut arguments = vec!["mcp"];
    if access.writes {
        arguments.push("--allow-writes");
    }
    if access.control {
        arguments.push("--allow-control");
    }
    arguments
}

/// How many tools agents see with `access`.
pub fn tool_count(access: Access) -> usize {
    tools(access).as_array().map_or(0, Vec::len)
}

/// The shell command, or for [`Client::Json`] the configuration, that registers the server.
pub fn registration(client: Client, access: Access, launch: Launch) -> String {
    let executable = launch.executable.display().to_string();
    let data_dir = launch.data_dir.map(|path| path.display().to_string());
    if client == Client::Json {
        return json_entry(access, &executable, data_dir.as_deref(), true);
    }
    let prefix: &[&str] = if client == Client::Codex {
        &["codex", "mcp", "add", SERVER_NAME]
    } else {
        // User scope makes the server available in every project.
        &["claude", "mcp", "add", "--scope", "user", SERVER_NAME]
    };
    let mut words: Vec<String> = prefix.iter().map(|word| (*word).to_owned()).collect();
    if let Some(data_dir) = data_dir {
        words.push(
            if client == Client::Codex {
                "--env"
            } else {
                "-e"
            }
            .into(),
        );
        words.push(quote(&format!("SWITCHX_DATA_DIR={data_dir}")));
    }
    words.push("--".into());
    words.push(quote(&executable));
    words.extend(arguments(access).into_iter().map(String::from));
    let command = words.join(" ");
    match launch.codex_home.filter(|_| client == Client::Codex) {
        Some(home) => with_codex_home(&home.display().to_string(), &command),
        None => command,
    }
}

/// The `mcpServers` configuration, with `command` ahead of `args` as clients document it.
fn json_entry(access: Access, executable: &str, data_dir: Option<&str>, pretty: bool) -> String {
    let text = |value: &str| Value::from(value).to_string();
    let arguments = Value::from(arguments(access)).to_string();
    let mut fields = vec![
        format!("\"command\":{}", text(executable)),
        format!("\"args\":{arguments}"),
    ];
    if let Some(data_dir) = data_dir {
        fields.push(format!(
            "\"env\":{{\"SWITCHX_DATA_DIR\":{}}}",
            text(data_dir)
        ));
    }
    if !pretty {
        return format!("{{{}}}", fields.join(","));
    }
    let fields: Vec<String> = fields
        .iter()
        .map(|field| format!("      {}", field.replacen("\":", "\": ", 1)))
        .collect();
    format!(
        "{{\n  \"mcpServers\": {{\n    \"{SERVER_NAME}\": {{\n{}\n    }}\n  }}\n}}",
        fields.join(",\n")
    )
}

/// One sentence asking whichever client receives it to register the server on
/// itself and confirm it works. The window's client choice does not appear:
/// Codex, Claude Code, and a JSON config each get the same sentence, and a
/// Codex home only changes the manual command.
pub fn request(access: Access, launch: Launch) -> String {
    let consent = match (access.writes, access.control) {
        (false, false) => "",
        (true, false) => "；之后修改 SwitchX 的模型或价格前，先向我说明并等我同意",
        (false, true) => "；之后修改 Codex 配置前，先把预览告诉我并等我同意",
        (true, true) => "；之后修改 SwitchX 的模型、价格或 Codex 配置前，先向我说明并等我同意",
    };
    let executable = launch.executable.display().to_string();
    let data_dir = launch.data_dir.map(|path| path.display().to_string());
    let entry = json_entry(access, &executable, data_dir.as_deref(), false);
    format!(
        "请把名为 switchx 的 stdio MCP 服务 `{entry}` 注册到你自己：你就是收到这句话的客户端，\
         用你自己的注册命令或配置文件，保留已有服务，并让各个项目都能使用。\
         完成后提醒我新开一个会话或重新加载，再调用 switchx 的 get_status 工具确认可用{consent}。"
    )
}

/// The access a Codex home's `[mcp_servers.switchx]` grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Registered {
    pub access: Access,
    /// Whether it starts `executable`; another path may be a moved or older copy.
    pub current: bool,
}

/// Reads the SwitchX registration in `home`'s `config.toml`, if any.
pub fn codex_registration(home: &Path, executable: &Path) -> Option<Registered> {
    let text = fs::read_to_string(home.join("config.toml")).ok()?;
    let document = text.parse::<toml_edit::DocumentMut>().ok()?;
    let server = document.get("mcp_servers")?.get(SERVER_NAME)?;
    let command = server.get("command")?.as_str()?;
    let arguments: Vec<&str> = server
        .get("args")
        .and_then(toml_edit::Item::as_array)
        .map(|arguments| {
            arguments
                .iter()
                .filter_map(|value| value.as_str())
                .collect()
        })
        .unwrap_or_default();
    if arguments.first() != Some(&"mcp") {
        return None;
    }
    Some(Registered {
        access: Access {
            writes: arguments.contains(&"--allow-writes"),
            control: arguments.contains(&"--allow-control"),
        },
        current: Path::new(command) == executable,
    })
}

/// Quotes `text` as one word for the platform's usual shell.
fn quote(text: &str) -> String {
    let plain = !text.is_empty()
        && text.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || b"_-./:=@%+,".contains(&byte)
                || (cfg!(windows) && byte == b'\\')
        });
    if plain {
        text.to_owned()
    } else if cfg!(windows) {
        format!("\"{text}\"")
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

fn with_codex_home(home: &str, command: &str) -> String {
    if cfg!(windows) {
        format!("$env:CODEX_HOME={}; {command}", quote(home))
    } else {
        format!("CODEX_HOME={} {command}", quote(home))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;

    const READ: Access = Access {
        writes: false,
        control: false,
    };
    const ALL: Access = Access {
        writes: true,
        control: true,
    };

    fn launch(executable: &Path) -> Launch<'_> {
        Launch {
            executable,
            data_dir: None,
            codex_home: None,
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn commands_quote_paths_and_carry_access_and_data_directory() {
        let executable = PathBuf::from("/Applications/SwitchX.app/Contents/MacOS/switchx");
        assert_eq!(
            registration(Client::Codex, READ, launch(&executable)),
            "codex mcp add switchx -- /Applications/SwitchX.app/Contents/MacOS/switchx mcp"
        );
        assert_eq!(
            registration(Client::ClaudeCode, ALL, launch(&executable)),
            "claude mcp add --scope user switchx -- \
             /Applications/SwitchX.app/Contents/MacOS/switchx mcp --allow-writes --allow-control"
        );

        let spaced = PathBuf::from("/Users/me/My Apps/it's/switchx");
        let data = PathBuf::from("/isolated/switchx data");
        let home = PathBuf::from("/isolated/codex");
        let custom = Launch {
            executable: &spaced,
            data_dir: Some(&data),
            codex_home: Some(&home),
        };
        assert_eq!(
            registration(
                Client::Codex,
                Access {
                    writes: true,
                    control: false
                },
                custom
            ),
            "CODEX_HOME=/isolated/codex codex mcp add switchx \
             --env 'SWITCHX_DATA_DIR=/isolated/switchx data' -- \
             '/Users/me/My Apps/it'\\''s/switchx' mcp --allow-writes"
        );
        // The Codex home only matters to Codex.
        assert_eq!(
            registration(Client::ClaudeCode, READ, custom),
            "claude mcp add --scope user switchx -e 'SWITCHX_DATA_DIR=/isolated/switchx data' -- \
             '/Users/me/My Apps/it'\\''s/switchx' mcp"
        );
    }

    #[test]
    fn json_configuration_lists_the_server_with_its_environment() {
        let executable = PathBuf::from("/opt/switchx");
        let data = PathBuf::from("/isolated/data");
        let config: Value = serde_json::from_str(&registration(
            Client::Json,
            Access {
                writes: false,
                control: true,
            },
            Launch {
                executable: &executable,
                data_dir: Some(&data),
                codex_home: None,
            },
        ))
        .unwrap();
        assert_eq!(
            config,
            json!({ "mcpServers": { "switchx": {
                "command": "/opt/switchx",
                "args": ["mcp", "--allow-control"],
                "env": { "SWITCHX_DATA_DIR": "/isolated/data" },
            } } })
        );
    }

    #[test]
    fn requests_register_whichever_client_receives_them() {
        let executable = PathBuf::from("/opt/switchx");
        let read = request(READ, launch(&executable));
        assert!(read.contains(r#"`{"command":"/opt/switchx","args":["mcp"]}`"#));
        assert!(read.contains("收到这句话的客户端"));
        assert!(read.contains("get_status"));
        assert!(!read.contains("同意"));
        assert!(!read.to_ascii_lowercase().contains("codex"));
        assert!(!read.contains("Claude"));

        let home = PathBuf::from("/isolated/codex");
        let data = PathBuf::from("/isolated/data");
        let all = request(
            ALL,
            Launch {
                executable: &executable,
                data_dir: Some(&data),
                codex_home: Some(&home),
            },
        );
        assert!(all.contains(r#""args":["mcp","--allow-writes","--allow-control"]"#));
        assert!(all.contains(r#""SWITCHX_DATA_DIR":"/isolated/data""#));
        // The selected Codex home belongs to the manual command, not this sentence.
        assert!(!all.contains("CODEX_HOME"));
        assert!(!all.contains("/isolated/codex"));
        assert!(!all.contains("claude"));
        assert!(all.contains("Codex 配置"));
        assert!(all.contains("等我同意"));
        assert!(!read.contains('\n') && !all.contains('\n'));

        let writes = request(
            Access {
                writes: true,
                control: false,
            },
            launch(&executable),
        );
        assert!(writes.contains("模型或价格"));
        assert!(!writes.contains("Codex"));
    }

    #[test]
    fn tool_counts_grow_with_access() {
        let read = tool_count(READ);
        let writes = tool_count(Access {
            writes: true,
            control: false,
        });
        assert!(read > 0);
        assert!(writes > read);
        assert!(tool_count(ALL) > writes);
    }

    #[test]
    fn codex_registration_reports_access_and_whether_it_runs_this_copy() {
        let mut nonce = [0_u8; 8];
        getrandom::fill(&mut nonce).unwrap();
        let suffix: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let home = std::env::temp_dir().join(format!("switchx-mcp-setup-{suffix}"));
        fs::create_dir_all(&home).unwrap();
        let executable = PathBuf::from("/opt/switchx");
        assert_eq!(codex_registration(&home, &executable), None);

        fs::write(
            home.join("config.toml"),
            "model = \"m\"\n[mcp_servers.other]\ncommand = \"/opt/switchx\"\nargs = [\"mcp\"]\n",
        )
        .unwrap();
        assert_eq!(codex_registration(&home, &executable), None);

        fs::write(
            home.join("config.toml"),
            "[mcp_servers.switchx]\ncommand = \"/opt/switchx\"\nargs = [\"mcp\", \"--allow-control\"]\n",
        )
        .unwrap();
        assert_eq!(
            codex_registration(&home, &executable),
            Some(Registered {
                access: Access {
                    writes: false,
                    control: true,
                },
                current: true,
            })
        );
        assert_eq!(
            codex_registration(&home, Path::new("/elsewhere/switchx")).map(|found| found.current),
            Some(false)
        );
        fs::remove_dir_all(&home).unwrap();
    }
}
