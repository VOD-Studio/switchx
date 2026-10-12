//! The SwitchX desktop app. Codex also runs this binary as the credential
//! helper named in the configurations SwitchX writes.

mod desktop;

use switchx::credential_helper;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Some(secret) = credential_helper::read(std::env::args_os().skip(1))? {
        println!("{}", secret.expose());
        return Ok(());
    }
    desktop::run()
}
