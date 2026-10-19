use super::*;
use crate::clients::ClaudeCapabilities;
use std::{fs, path::PathBuf};

struct Fixture {
    root: PathBuf,
    data: PathBuf,
    home: PathBuf,
    workspace: ClientWorkspace,
}
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("switchx-claude-{}", crate::id::new_id().unwrap()));
        let data = root.join("data");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let workspace = claude_workspace(&data, &home).unwrap();
        Self {
            root,
            data,
            home,
            workspace,
        }
    }
    fn connection(&self, name: &str, auth: AuthMode) -> Connection {
        let settings = if auth == AuthMode::Native {
            ConnectionSettings {
                auth,
                picker: PickerPolicy::Preserve,
                ..Default::default()
            }
        } else {
            ConnectionSettings {
                auth,
                protocol: crate::claude::UpstreamProtocol::Messages,
                picker: PickerPolicy::Replace,
                default_model: "custom/sonnet".into(),
                models: vec![
                    ModelOption::new("custom/sonnet"),
                    ModelOption::new("custom/opus"),
                ],
                roles: Roles {
                    haiku: "custom/sonnet".into(),
                    ..Default::default()
                },
            }
        };
        save_claude_connection(
            &self.data,
            &self.home,
            Connection {
                id: crate::id::new_id().unwrap(),
                workspace_id: self.workspace.id.clone(),
                name: name.into(),
                base_url: "https://gateway.example.test/anthropic".into(),
                settings,
                revision: 0,
                has_key: false,
            },
            (auth != AuthMode::Native).then(|| Secret::new("synthetic-claude-key".into())),
            None,
        )
        .unwrap()
    }
    fn prepare(&self, c: &Connection) -> claude::config::Prepared {
        prepare_claude_direct(
            &self.data,
            &self.workspace,
            &c.id,
            &self.root.join("SwitchX binary"),
            ClaudeCapabilities::from_version("2.1.301"),
        )
        .unwrap()
    }
    fn apply(&self, c: &Connection) {
        apply_claude_change(&self.data, &self.home, self.prepare(c)).unwrap();
    }
    fn current(&self) -> Value {
        serde_json::from_slice(&fs::read(self.home.join("settings.json")).unwrap()).unwrap()
    }
    fn state(&self) -> claude::config::Status {
        claude::config::status(&self.data, &self.workspace).unwrap()
    }
    fn restore(&self, keep: bool) -> Vec<String> {
        apply_claude_change(
            &self.data,
            &self.home,
            preview_claude_restore(&self.data, &self.home, keep).unwrap(),
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn scoped_crud_preserves_blank_keys_and_rejects_stale_revisions() {
    let f = Fixture::new();
    crate::app::save_provider(
        &f.data,
        None,
        "Codex API",
        "https://codex.example.test/v1",
        "test-model",
        "synthetic-codex-key".into(),
    )
    .unwrap();
    let c = f.connection("Claude API", AuthMode::Helper);
    let store = open_store(&f.data).unwrap();
    assert_eq!(store.providers().unwrap().len(), 1);
    assert!(store.provider(&c.id).unwrap().is_none());
    let other = claude_workspace(&f.data, &f.root.join("other")).unwrap();
    assert!(store.claude_connection(&other.id, &c.id).unwrap().is_none());
    let mut edit = c.clone();
    edit.name = "Updated".into();
    let updated =
        save_claude_connection(&f.data, &f.home, edit.clone(), None, Some(c.revision)).unwrap();
    assert_eq!(updated.revision, 2);
    assert_eq!(
        store.provider_api_key(&c.id).unwrap().unwrap().expose(),
        "synthetic-claude-key"
    );
    assert!(save_claude_connection(&f.data, &f.home, edit, None, Some(c.revision)).is_err());
    assert!(!f.home.join("settings.json").exists());
    assert!(crate::app::delete_provider(&f.data, &c.id).is_err());
    delete_claude_connection(&f.data, &f.home, &updated.id, updated.revision).unwrap();
    assert!(store.provider_api_key(&updated.id).unwrap().is_none());
}

#[test]
fn helper_menus_preserve_unowned_bytes_and_restore_original_credentials_privately() {
    let f = Fixture::new();
    let before = "{\n  \"theme\" : \"中文\",\n  \"hooks\": {\"keep\":[]},\n  \"env\":{\"USER_SETTING\":\"keep\",\"ANTHROPIC_API_KEY\":\"synthetic-original-key\"},\n  \"modelPicker\":{\"options\":[{\"model\":\"original\"}]}\n}\n";
    fs::write(f.home.join("settings.json"), before).unwrap();
    fs::write(f.home.join(".credentials.json"), "synthetic-native-login").unwrap();
    let c = f.connection("Claude helper", AuthMode::Helper);
    let prepared = f.prepare(&c);
    let preview = serde_json::to_string(&prepared.preview).unwrap();
    assert!(!preview.contains("synthetic-original-key"));
    assert!(!preview.contains("synthetic-claude-key"));
    assert_eq!(
        fs::read_to_string(f.home.join("settings.json")).unwrap(),
        before
    );
    apply_claude_change(&f.data, &f.home, prepared).unwrap();
    let text = fs::read_to_string(f.home.join("settings.json")).unwrap();
    assert!(text.contains("\"theme\" : \"中文\""));
    assert!(text.contains("\"hooks\": {\"keep\":[]}"));
    assert!(!text.contains("synthetic-claude-key"));
    assert!(!text.contains("synthetic-original-key"));
    assert_eq!(
        f.current()["modelPicker"]["options"][0]["model"],
        "custom/sonnet"
    );
    assert_eq!(
        f.current()["modelPicker"]["options"][1]["model"],
        "custom/opus"
    );
    assert!(
        f.current()["apiKeyHelper"]
            .as_str()
            .unwrap()
            .contains(" credential ")
    );
    let journal = fs::read_to_string(
        f.data
            .join("claude")
            .join(&f.workspace.id)
            .join("active.json"),
    )
    .unwrap();
    for secret in [
        "synthetic-original-key",
        "synthetic-claude-key",
        "synthetic-native-login",
    ] {
        assert!(!journal.contains(secret));
    }
    assert!(f.restore(false).is_empty());
    assert_eq!(
        fs::read_to_string(f.home.join("settings.json")).unwrap(),
        before
    );
    assert_eq!(
        fs::read_to_string(f.home.join(".credentials.json")).unwrap(),
        "synthetic-native-login"
    );
    assert!(!f.state().managed);
}

#[test]
fn auth_modes_and_active_edit_locks_are_client_scoped() {
    let f = Fixture::new();
    let api = f.connection("API", AuthMode::ApiKey);
    let bearer = f.connection("Bearer", AuthMode::Bearer);
    let native = f.connection("Official", AuthMode::Native);
    f.apply(&api);
    assert_eq!(
        f.current()["env"]["ANTHROPIC_API_KEY"],
        "synthetic-claude-key"
    );
    assert!(f.current()["env"].get("ANTHROPIC_AUTH_TOKEN").is_none());
    assert!(
        save_claude_connection(&f.data, &f.home, api.clone(), None, Some(api.revision)).is_err()
    );
    assert!(delete_claude_connection(&f.data, &f.home, &bearer.id, bearer.revision).is_err());
    crate::app::save_provider(
        &f.data,
        None,
        "Still Codex",
        "https://codex.example.test/v1",
        "test-model",
        "synthetic-codex-key".into(),
    )
    .unwrap();
    f.apply(&bearer);
    assert!(f.current()["env"].get("ANTHROPIC_API_KEY").is_none());
    assert_eq!(
        f.current()["env"]["ANTHROPIC_AUTH_TOKEN"],
        "synthetic-claude-key"
    );
    f.apply(&native);
    assert!(f.current().get("apiKeyHelper").is_none());
    assert!(
        f.current()
            .get("env")
            .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
            .is_none()
    );
    assert!(f.restore(false).is_empty());
    assert!(!f.home.join("settings.json").exists());
}

#[test]
fn stale_file_metadata_and_secret_snapshots_cannot_be_applied() {
    let f = Fixture::new();
    let c = f.connection("First", AuthMode::Helper);
    let prepared = f.prepare(&c);
    fs::write(f.home.join("settings.json"), "{\"external\":true}").unwrap();
    assert!(apply_claude_change(&f.data, &f.home, prepared).is_err());
    assert_eq!(f.current()["external"], true);
    let prepared = f.prepare(&c);
    let mut edit = c.clone();
    edit.name = "Second".into();
    let updated = save_claude_connection(&f.data, &f.home, edit, None, Some(c.revision)).unwrap();
    assert!(apply_claude_change(&f.data, &f.home, prepared).is_err());
    let prepared = f.prepare(&updated);
    let db = rusqlite::Connection::open(f.data.join("switchx.sqlite")).unwrap();
    db.execute("UPDATE providers SET settings_config=json_set(settings_config,'$.auth.OPENAI_API_KEY','synthetic-rotated-key') WHERE id=?1",[&updated.id]).unwrap();
    assert!(apply_claude_change(&f.data, &f.home, prepared).is_err());
    assert!(!f.state().managed);
}

#[test]
fn external_changes_survive_switches_and_restore_conflicts_are_explicit() {
    let f = Fixture::new();
    fs::write(
        f.home.join("settings.json"),
        "{\"model\":\"original\",\"hooks\":{\"original\":true}}",
    )
    .unwrap();
    let one = f.connection("One", AuthMode::Helper);
    let two = f.connection("Two", AuthMode::Bearer);
    f.apply(&one);
    let mut current = f.current();
    current["hooks"]["external"] = true.into();
    fs::write(
        f.home.join("settings.json"),
        serde_json::to_string_pretty(&current).unwrap(),
    )
    .unwrap();
    f.apply(&two);
    current = f.current();
    current["model"] = "external-model".into();
    fs::write(
        f.home.join("settings.json"),
        serde_json::to_string(&current).unwrap(),
    )
    .unwrap();
    assert_eq!(f.restore(false), vec!["model"]);
    assert_eq!(f.current()["model"], "external-model");
    assert!(f.state().managed);
    assert!(f.restore(true).is_empty());
    assert!(!f.state().managed);
    assert_eq!(f.current()["model"], "external-model");
    assert_eq!(f.current()["hooks"]["original"], true);
    assert_eq!(f.current()["hooks"]["external"], true);
}

#[test]
fn pending_transactions_recover_on_either_side_of_the_atomic_write() {
    for did_write in [false, true] {
        let f = Fixture::new();
        let c = f.connection("Pending", AuthMode::ApiKey);
        let before = "{\"permissions\":{\"allow\":[]},\"model\":\"original\"}";
        fs::write(f.home.join("settings.json"), before).unwrap();
        f.apply(&c);
        let path = f
            .data
            .join("claude")
            .join(&f.workspace.id)
            .join("active.json");
        let mut journal: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        journal["pending"] = true.into();
        fs::write(path, serde_json::to_vec(&journal).unwrap()).unwrap();
        if !did_write {
            fs::write(f.home.join("settings.json"), before).unwrap();
        }
        assert!(f.state().pending);
        assert!(f.restore(false).is_empty());
        assert_eq!(
            fs::read_to_string(f.home.join("settings.json")).unwrap(),
            before
        );
    }
}

#[test]
fn imports_do_not_write_settings_or_execute_helper_commands() {
    let f = Fixture::new();
    let text = r#"{"env":{"ANTHROPIC_BASE_URL":"https://gateway.example.test","ANTHROPIC_AUTH_TOKEN":"synthetic-import-key","ANTHROPIC_DEFAULT_HAIKU_MODEL":"small"},"model":"first","modelPicker":{"replaceBuiltInOptions":true,"options":[{"model":"second","label":"Second","behavesAs":"claude-sonnet-4-6"},{"model":"first","label":"First"}]}}"#;
    fs::write(f.home.join("settings.json"), text).unwrap();
    let c = import_claude_current(&f.data, &f.home).unwrap();
    assert_eq!(c.settings.auth, AuthMode::Bearer);
    assert_eq!(c.settings.models[0].model, "second");
    assert_eq!(c.settings.roles.haiku, "small");
    assert!(!format!("{c:?}").contains("synthetic-import-key"));
    assert_eq!(
        fs::read_to_string(f.home.join("settings.json")).unwrap(),
        text
    );
    assert!(!f.state().managed);
    fs::write(
        f.home.join("settings.json"),
        r#"{"apiKeyHelper":"echo must-not-run"}"#,
    )
    .unwrap();
    assert!(import_claude_current(&f.data, &f.home).is_err());
}

#[cfg(unix)]
#[test]
fn symlinks_fail_closed_and_private_files_are_restricted() {
    use std::os::unix::{fs::PermissionsExt, fs::symlink};
    let f = Fixture::new();
    let c = f.connection("Private", AuthMode::ApiKey);
    let outside = f.root.join("outside.json");
    fs::write(&outside, "{}").unwrap();
    symlink(&outside, f.home.join("settings.json")).unwrap();
    assert!(
        prepare_claude_direct(
            &f.data,
            &f.workspace,
            &c.id,
            &f.root.join("exe"),
            ClaudeCapabilities::from_version("2.1.301")
        )
        .is_err()
    );
    assert_eq!(fs::read_to_string(outside).unwrap(), "{}");
    fs::remove_file(f.home.join("settings.json")).unwrap();
    f.apply(&c);
    assert_eq!(
        fs::metadata(f.home.join("settings.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let private = f.data.join("claude").join(&f.workspace.id);
    assert_eq!(
        fs::metadata(&private).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for file in fs::read_dir(private).unwrap() {
        assert_eq!(
            file.unwrap().metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn discovery_honors_auth_choice_and_redacts_error_bodies() {
    use axum::{
        Json, Router,
        http::{HeaderMap, StatusCode},
        routing::get,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router=Router::new().route("/v1/models",get(|headers:HeaderMap|async move{assert_eq!(headers["x-api-key"],"synthetic-key");assert!(!headers.contains_key("authorization"));Json(serde_json::json!({"data":[{"id":"custom/model","display_name":"My model"},{"id":"custom/model"}]}))})).route("/bad/v1/models",get(||async{(StatusCode::UNAUTHORIZED,"synthetic-private-upstream-error")}));
    let router = router.route(
        "/gateway/v1/v1/models",
        get(|headers: HeaderMap| async move {
            assert_eq!(headers["authorization"], "Bearer synthetic-key");
            assert!(!headers.contains_key("x-api-key"));
            Json(serde_json::json!({"data":[{"id":"prefixed/model"}]}))
        }),
    );
    let router = router.route(
        "/responses/v1/models",
        get(|headers: HeaderMap| async move {
            assert_eq!(headers["authorization"], "Bearer synthetic-key");
            assert!(!headers.contains_key("x-api-key"));
            assert!(!headers.contains_key("anthropic-version"));
            Json(serde_json::json!({"data":[{"id":"responses/model"}]}))
        }),
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let key = Secret::new("synthetic-key".into());
    let models = discover_claude_models(
        &format!("http://{address}"),
        UpstreamProtocol::Messages,
        AuthMode::ApiKey,
        &key,
    )
    .await
    .unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].label, "My model");
    let prefixed = discover_claude_models(
        &format!("http://{address}/gateway/v1"),
        UpstreamProtocol::Messages,
        AuthMode::Bearer,
        &key,
    )
    .await
    .unwrap();
    assert_eq!(prefixed[0].model, "prefixed/model");
    let responses = discover_claude_models(
        &format!("http://{address}/responses"),
        UpstreamProtocol::Responses,
        AuthMode::Bearer,
        &key,
    )
    .await
    .unwrap();
    assert_eq!(responses[0].model, "responses/model");
    assert!(
        discover_claude_models(
            &format!("http://{address}/responses"),
            UpstreamProtocol::Responses,
            AuthMode::Helper,
            &key,
        )
        .await
        .is_err()
    );
    let error = discover_claude_models(
        &format!("http://{address}/bad"),
        UpstreamProtocol::Messages,
        AuthMode::ApiKey,
        &key,
    )
    .await
    .unwrap_err();
    assert!(error.contains("401"));
    assert!(!error.contains("synthetic-private"));
    task.abort();
}

#[test]
fn empty_menu_keeps_independent_default_and_native_roles_are_written() {
    let f = Fixture::new();
    fs::write(f.home.join("settings.json"),r#"{"model":"outside/menu","modelPicker":{"replaceBuiltInOptions":true,"options":[]},"env":{"ANTHROPIC_API_KEY":"synthetic-import-key","ANTHROPIC_BASE_URL":"https://example.test"}}"#).unwrap();
    let c = import_claude_current(&f.data, &f.home).unwrap();
    assert!(c.settings.models.is_empty());
    assert_eq!(c.settings.default_model, "outside/menu");
    f.apply(&c);
    assert_eq!(f.current()["modelPicker"]["options"], serde_json::json!([]));
    assert_eq!(f.current()["model"], "outside/menu");
    assert!(f.restore(false).is_empty());
    let mut preserve = c.clone();
    preserve.settings.picker = PickerPolicy::Preserve;
    preserve.settings.models = vec![ModelOption::new("saved/not-published")];
    let preserve =
        save_claude_connection(&f.data, &f.home, preserve, None, Some(c.revision)).unwrap();
    assert!(f.prepare(&preserve).preview.models.is_empty());
    f.apply(&preserve);
    assert_eq!(f.current()["modelPicker"]["options"], serde_json::json!([]));
    assert!(f.restore(false).is_empty());
    let mut native = f.connection("Native roles", AuthMode::Native);
    native.settings.roles.sonnet = "native/role".into();
    let revision = native.revision;
    let native = save_claude_connection(&f.data, &f.home, native, None, Some(revision)).unwrap();
    f.apply(&native);
    assert!(
        f.current().get("model").is_none(),
        "empty native default must select the CLI default instead of retaining a gateway ID"
    );
    assert_eq!(
        f.current()["env"]["ANTHROPIC_DEFAULT_SONNET_MODEL"],
        "native/role"
    );
    assert!(f.restore(false).is_empty());
    assert_eq!(f.current()["model"], "outside/menu");
}
