//! Codex CLI integration: the target `config.toml`, the switches SwitchX
//! manages in it, and their recovery.

pub mod client;
pub mod direct_switch;
pub mod overlay;
pub mod provider_config;
pub mod route_config;
pub mod route_switch;

use std::path::Path;

/// Recovery journal of an applied direct switch, kept in the SwitchX data directory.
pub const DIRECT_JOURNAL: &str = "direct-journal.json";
/// Recovery journal of a published route, kept in the SwitchX data directory.
pub const ROUTE_JOURNAL: &str = "switch-journal.json";
pub const JOURNALS: [&str; 2] = [DIRECT_JOURNAL, ROUTE_JOURNAL];

/// Whether SwitchX manages a Codex configuration that must be restored before
/// saved connections, models, or accounts may change.
pub fn config_managed(state_dir: &Path) -> bool {
    JOURNALS.iter().any(|name| state_dir.join(name).exists())
}
