# Repository Guidelines

## Project Structure & Module Organization

SwitchX is a Rust 2024 desktop app with a Slint interface. `src/main.rs` starts the app; `src/lib.rs` exposes modules for the catalog, routing, storage, credentials, and Codex configuration. `crates/switchx-ui/` compiles the generated UI separately; `switchx::ui` re-exports its types. Slint views and design tokens live in `ui/`, with the tray icon in `assets/`. `tests/router.rs` holds integration tests; `tests/fixtures/` contains synthetic catalog and configuration samples. Runnable probes live in `examples/`. See `README.md` for implemented behavior and `docs/SWITCHX-PLAN.md` for the longer term design. macOS bundle inputs are in `packaging/macos/` and `scripts/`.

## Build, Test, and Development Commands

- `cargo run`: build and launch the native app.
- `make check-lib`: check business-library changes without generating machine code.
- `make check-app`: check the desktop binary during development.
- `make test-router`: run the focused routing integration tests.
- During iteration, use the focused target relevant to the change; run `make check` for the complete formatting, Clippy, and test checks before committing a completed feature.
- `cargo test`: run unit and integration tests.
- `cargo test --test router`: check exact model routing, streaming, and credential isolation against local mock upstreams.
- `cargo fmt --all -- --check`: check Rust formatting; run `cargo fmt --all` to apply it.
- `cargo clippy --all-targets -- -D warnings`: check Rust code across targets.
- `sh scripts/bundle-macos.sh`: create a local debug app at `target/debug/SwitchX.app` on macOS.
- `sh scripts/bundle-macos.sh --release`: create a local release app at `target/release/SwitchX.app` on macOS.

## Coding Style & Naming Conventions

Use `rustfmt` defaults (four-space indentation), `snake_case` for Rust modules/functions and test names, and `PascalCase` for types. Keep UI declarations in `.slint` files and business logic in `src/`. Follow existing module boundaries rather than adding new crates for small features. Use explicit public model IDs and route mappings; never infer an upstream from a model name.

## Testing Guidelines

Add focused `#[test]` or `#[tokio::test]` cases near the changed module; put cross-module routing checks in `tests/router.rs`. Use synthetic fixtures and temporary directories so routine tests need no account or API key. No coverage threshold is configured. The `deepseek_live_probe` makes real, potentially billable requests; run it only for deliberate live verification. State clearly whether a result came from mocks, an isolated Codex CLI probe, or a real upstream.

## Commit & Pull Request Guidelines

Commit each completed feature point separately after verification. Follow the history's `feat: ...` and `test: ...` prefix style. In pull requests, describe the behavior, affected modules, and verification commands. Link a related issue when one exists; include screenshots for Slint UI changes. Call out any changes to Codex configuration, credential handling, or live-provider behavior.

## Security & Configuration

Provider API keys are stored in plaintext in SQLite `providers.settings_config.auth.OPENAI_API_KEY`, following the user's selected CC Switch storage model. Random local router tokens are also stored in plaintext in SQLite `app_settings`, under `local_token:router-<32hex>` keys. Grok OAuth refresh credentials are stored separately in private `xai_oauth_auth.json` (mode 0600 on Unix); access tokens stay in memory. Grok routes pin the managed account and actual model per session, inject credentials only into the fixed xAI endpoint, and never write Codex native login. ChatGPT OAuth credentials are stored separately in the private `codex_oauth_auth.json` file; its optional complete `auth.json` snapshot may include access tokens, and legacy records without that field remain readable.

Raw OAuth JSON may be shown and edited only in the dedicated, user-authorized `auth.json` editor. Never expose API keys or OAuth tokens in lists, status, TOML or other configuration previews, recovery journals, logs, or exports. Clear the editor's credential contents on cancel, close, or navigation, and compare the opened credential snapshot before saving to avoid overwriting external token rotations. Subscription TOML may contain non-sensitive MCP settings; shared snippets must exclude MCP, provider routing, and credentials. Saving a subscription stores its account binding and configuration without activating Codex; route publication applies the saved configuration, and writing the native Codex login remains a separate explicit operation.

Local router tokens must not enter UI output, logs, exports, or upstream requests; subscription routing may place its independent local token in permission-restricted Codex configuration and recovery journals. Keep API-key inputs masked; an empty key when editing preserves the saved key. Credential helpers require an explicit absolute data directory; `local-token REF ABS_DATA_DIR` reads SQLite without creating or migrating it. Do not depend on keyring, support old helper forms, or read, migrate, or delete old keychain entries. Missing SQLite API keys must be entered again; old helper configurations must be restored and republished. Keep the local token and route usable when recovery has conflicts, and remove the token only after successful recovery. Do not commit real keys, account data, databases, or user `config.toml` files. Use an absolute `SWITCHX_DATA_DIR` and an isolated `CODEX_HOME` when exercising configuration flows.
