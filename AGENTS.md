# Repository Guidelines

## Project Structure & Module Organization

SwitchX is a Rust 2024 desktop app with a Slint interface. `src/main.rs` starts the app; `src/lib.rs` exposes modules for the catalog, routing, storage, credentials, and Codex configuration. Slint views and design tokens live in `ui/`, with the tray icon in `assets/`. `tests/router.rs` holds integration tests; `tests/fixtures/` contains synthetic catalog and configuration samples. Runnable probes live in `examples/`. See `README.md` for implemented behavior and `docs/SWITCHX-PLAN.md` for the longer term design. macOS bundle inputs are in `packaging/macos/` and `scripts/`.

## Build, Test, and Development Commands

- `cargo run`: build and launch the native app.
- `cargo test`: run unit and integration tests.
- `cargo test --test router`: check exact model routing, streaming, and credential isolation against local mock upstreams.
- `cargo fmt --all -- --check`: check Rust formatting; run `cargo fmt --all` to apply it.
- `cargo clippy --all-targets -- -D warnings`: check Rust code across targets.
- `sh scripts/bundle-macos.sh`: create a local debug app at `target/debug/SwitchX.app` on macOS.

## Coding Style & Naming Conventions

Use `rustfmt` defaults (four-space indentation), `snake_case` for Rust modules/functions and test names, and `PascalCase` for types. Keep UI declarations in `.slint` files and business logic in `src/`. Follow existing module boundaries rather than adding new crates for small features. Use explicit public model IDs and route mappings; never infer an upstream from a model name.

## Testing Guidelines

Add focused `#[test]` or `#[tokio::test]` cases near the changed module; put cross-module routing checks in `tests/router.rs`. Use synthetic fixtures and temporary directories so routine tests need no account or API key. No coverage threshold is configured. The `deepseek_live_probe` makes real, potentially billable requests; run it only for deliberate live verification. State clearly whether a result came from mocks, an isolated Codex CLI probe, or a real upstream.

## Commit & Pull Request Guidelines

Commit each completed feature point separately after verification. Follow the history's `feat: ...` and `test: ...` prefix style. In pull requests, describe the behavior, affected modules, and verification commands. Link a related issue when one exists; include screenshots for Slint UI changes. Call out any changes to Codex configuration, credential handling, or live-provider behavior.

## Security & Configuration

Provider API keys are stored in plaintext in SQLite `providers.settings_config.auth.OPENAI_API_KEY`, following the user's selected CC Switch storage model. ChatGPT OAuth credentials are stored separately in the private `codex_oauth_auth.json` file; local router tokens remain in the system credential store. Never expose API keys or OAuth tokens in lists, status, configuration previews, recovery journals, logs, or exports. Keep API-key inputs masked; an empty key when editing preserves the saved key. For legacy API-key migration, save the key in SQLite before clearing only SwitchX-owned keychain entries; failure must preserve the old credential. Do not commit real keys, account data, databases, or user `config.toml` files. Use an absolute `SWITCHX_DATA_DIR` and an isolated `CODEX_HOME` when exercising configuration flows.
