pub mod app;
pub mod catalog;
pub mod chatgpt;
pub mod code_highlight;
pub mod codex;
pub mod credentials;
pub mod id;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod provider_icons;
pub mod reasoning_picker;
pub mod requests;
pub mod route_session;
pub mod router;
pub mod storage;
pub use switchx_ui as ui;
pub mod upstream;
pub mod xai;
