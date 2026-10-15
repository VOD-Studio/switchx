//! The SwitchX desktop app. Codex also runs this binary as the credential
//! helper named in the configurations SwitchX writes, and agents run
//! `switchx mcp` as a read-only MCP server.

mod desktop;

use switchx::{app, credential_helper, mcp};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() == 1 && (arguments[0] == "--version" || arguments[0] == "-V") {
        println!("SwitchX {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if arguments.first().is_some_and(|command| command == "mcp") {
        let usage = "usage: switchx mcp [--allow-writes] [--allow-control]";
        let mut access = mcp::Access::default();
        for flag in &arguments[1..] {
            let enabled = match flag.to_str() {
                Some("--allow-writes") => &mut access.writes,
                Some("--allow-control") => &mut access.control,
                _ => return Err(usage.into()),
            };
            if std::mem::replace(enabled, true) {
                return Err(usage.into());
            }
        }
        mcp::serve(app::data_directory(), access)?;
        return Ok(());
    }
    if let Some(secret) = credential_helper::read(arguments.into_iter())? {
        println!("{}", secret.expose());
        return Ok(());
    }
    desktop::run()
}
