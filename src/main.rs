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
    if arguments.len() == 1 && arguments[0] == "mcp" {
        mcp::serve(app::data_directory())?;
        return Ok(());
    }
    if let Some(secret) = credential_helper::read(arguments.into_iter())? {
        println!("{}", secret.expose());
        return Ok(());
    }
    desktop::run()
}
