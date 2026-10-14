use std::{cell::Cell, error::Error, fs, path::Path, rc::Rc, time::Duration};

use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::platform::{WindowEvent, software_renderer::MinimalSoftwareWindow};
use slint::{
    ComponentHandle, LogicalPosition, Model, ModelRc, PhysicalSize, SharedString, VecModel,
};
use switchx::{
    headless, provider_icons,
    ui::{
        AppWindow, ModelRow, PreviewBindingRow, PreviewFact, PreviewMappingRow, ProviderRow,
        RoutePreviewData, Theme,
    },
};

use crate::support::{every, frame, play, render, set_theme, snapshot, snapshot_now};

const DEFAULT_MODEL: &str = "sx-synthetic-chatgpt-gpt-5-5";
const DEFAULT_DISPLAY_NAME: &str = "星河旗舰 · GPT-5.5";
const INSPECT: &str = "重新检查启用条件";
const APPLY: &str = "确认启用模型";
const EXPAND: &str = "展开检查详情";
const COLLAPSE: &str = "收起检查详情";
const LAST_NOTE: &str = "Codex 窗口与通用配置使用默认模型的供应商设置。";

fn visible_text(app: &AppWindow) -> String {
    ElementQuery::from_root(app)
        .find_all()
        .into_iter()
        .flat_map(|element| [element.accessible_label(), element.accessible_value()])
        .flatten()
        .map(|text| text.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_display_name(app: &AppWindow) {
    let model = ElementHandle::find_by_element_id(app, "RoutePublishSheet::default-model-name")
        .next()
        .expect("visible default model name in the publish sheet");
    assert_eq!(
        model.accessible_label().as_deref(),
        Some(DEFAULT_DISPLAY_NAME)
    );
    assert!(
        !visible_text(app).contains("sx-synthetic-"),
        "internal public model IDs must not appear in visible UI text"
    );
}

fn assert_checking(app: &AppWindow) {
    assert!(app.get_busy());
    assert!(!app.get_route_preview_ready());
    assert_display_name(app);
    let text = visible_text(app);
    for label in [
        "模型目录",
        "账号绑定",
        "目标配置",
        "核对中",
        "正在检查启用条件",
    ] {
        assert!(text.contains(label), "missing checking state text: {label}");
    }
    assert!(
        !text.contains("模型目录与发布配置已核对"),
        "checking must not claim success"
    );
}

// Resolve the actual controls through their accessibility semantics. This keeps
// pointer dispatch faithful while allowing the responsive layout to change.
fn control(app: &AppWindow, label: &str) -> ElementHandle {
    let mut matches = ElementHandle::find_by_accessible_label(app, label);
    let item = matches
        .next()
        .unwrap_or_else(|| panic!("missing publish control: {label}"));
    assert!(
        matches.next().is_none(),
        "ambiguous publish control: {label}"
    );
    item
}

fn point(app: &AppWindow, label: &str) -> LogicalPosition {
    let item = control(app, label);
    let position = item.absolute_position();
    let size = item.size();
    LogicalPosition::new(
        position.x + size.width / 2.0,
        position.y + size.height / 2.0,
    )
}

fn click(app: &AppWindow, window: &MinimalSoftwareWindow, label: &str) {
    let position = point(app, label);
    let size = app.window().size();
    assert!(
        position.x > 0.0
            && position.y > 0.0
            && position.x < size.width as f32
            && position.y < size.height as f32,
        "publish control must be visible: {label} at {position:?}"
    );
    headless::click(window, position.x, position.y);
    headless::render(window);
}

fn scroll_body(window: &MinimalSoftwareWindow, viewport: &ElementHandle, delta: f32) {
    let position = viewport.absolute_position();
    headless::scroll(
        window,
        position.x + viewport.size().width / 2.0,
        position.y + 12.0,
        delta,
    );
    render(window);
}

// Scrolls the body to its end, checks the last diagnostic, and returns to the
// top so the disclosure control is visible again.
fn scroll_body_to_details(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
    name: &str,
) -> Result<(), Box<dyn Error>> {
    let viewport = ElementHandle::find_by_element_id(app, "RoutePublishSheet::content-scroll")
        .next()
        .expect("publish body scroll viewport");
    scroll_body(window, &viewport, -10_000.0);
    let note = ElementHandle::find_by_accessible_label(app, LAST_NOTE)
        .next()
        .expect("the last diagnostic is reachable by scrolling the body");
    let bottom = viewport.absolute_position().y + viewport.size().height;
    assert!(
        note.absolute_position().y + note.size().height <= bottom + 1.0,
        "expanded diagnostics must be reachable by scrolling the body"
    );
    snapshot(window, output, name)?;
    scroll_body(window, &viewport, 10_000.0);
    Ok(())
}

fn seed(app: &AppWindow) -> Result<(), Box<dyn Error>> {
    let groups = [
        ("chatgpt", "ChatGPT 订阅", "openai", true, false, 8),
        ("grok", "Grok", "grok", true, true, 3),
        ("api", "Podlink", "anthropic", false, false, 14),
    ];
    let mut providers = Vec::new();
    for (id, name, icon_id, subscription, grok, count) in groups {
        let icon = provider_icons::icon(icon_id).unwrap();
        let models = (0..count)
            .map(|index| {
                let model = if id == "chatgpt" && index == 0 {
                    "gpt-5.5".to_owned()
                } else {
                    format!("synthetic-{id}-{}", index + 1)
                };
                ModelRow {
                    provider_id: format!("preview-{id}").into(),
                    provider_name: name.into(),
                    upstream_model: model.clone().into(),
                    public_id: if id == "chatgpt" && index == 0 {
                        DEFAULT_MODEL.into()
                    } else {
                        format!("sx-synthetic-{id}-{index}").into()
                    },
                    display_name: if id == "chatgpt" && index == 0 {
                        DEFAULT_DISPLAY_NAME.into()
                    } else {
                        format!("{model}/{name}").into()
                    },
                    saved: true,
                    ready: true,
                    included: true,
                    is_subscription: subscription,
                    ..Default::default()
                }
            })
            .collect::<Vec<_>>();
        providers.push(ProviderRow {
            id: format!("preview-{id}").into(),
            name: name.into(),
            icon: slint::Image::load_from_svg_data(icon.data)?,
            monochrome: icon.monochrome,
            endpoint: "https://example.invalid/v1".into(),
            base_url: "https://example.invalid/v1".into(),
            binding_label: "绑定：合成预览账号".into(),
            is_subscription: subscription,
            is_grok: grok,
            model_count: count,
            ready_model_count: count,
            selected_model_count: count,
            models: ModelRc::new(VecModel::from(models)),
            ..Default::default()
        });
    }
    app.set_models(ModelRc::new(VecModel::from(
        providers
            .iter()
            .flat_map(|provider| provider.models.iter())
            .collect::<Vec<_>>(),
    )));
    let providers = ModelRc::new(VecModel::from(providers));
    app.set_providers(providers.clone());
    app.set_filtered_providers(providers);
    app.set_selected_model_count(25);
    app.set_selectable_model_count(25);
    app.set_selected_connection_count(3);
    app.set_default_model(DEFAULT_MODEL.into());
    app.set_route_preview_model(DEFAULT_MODEL.into());
    app.set_default_model_label(DEFAULT_DISPLAY_NAME.into());
    app.set_config_home("/Users/preview/.codex".into());
    app.set_status_text("合成 UI 预览 · 不访问账号或 Codex 配置".into());
    app.set_active_page(0);
    Ok(())
}

fn preview(app: &AppWindow) -> RoutePreviewData {
    let strings = |items: &[&str]| {
        ModelRc::new(VecModel::from(
            items
                .iter()
                .map(|item| SharedString::from(*item))
                .collect::<Vec<_>>(),
        ))
    };
    let facts = |items: &[(&str, &str)]| {
        ModelRc::new(VecModel::from(
            items
                .iter()
                .map(|(label, value)| PreviewFact {
                    label: (*label).into(),
                    value: (*value).into(),
                })
                .collect::<Vec<_>>(),
        ))
    };
    let mappings = app
        .get_models()
        .iter()
        .enumerate()
        .map(|(index, row)| PreviewMappingRow {
            model: row.display_name,
            provider: row.provider_name,
            origin: "https://example.invalid".into(),
            // One synthetic backup shows how a fallback destination reads.
            fallback: if index == 1 { "Podlink" } else { "" }.into(),
            fallback_origin: if index == 1 {
                "https://backup.example.invalid"
            } else {
                ""
            }
            .into(),
        })
        .collect::<Vec<_>>();
    RoutePreviewData {
        client: "codex-cli 0.162.1".into(),
        endpoint: "http://127.0.0.1:18731/v1".into(),
        target: "/Users/preview/.codex/config.toml".into(),
        mappings: ModelRc::new(VecModel::from(mappings)),
        changes: strings(&[
            "model",
            "model_provider",
            "model_catalog_json",
            "model_providers.switchx_router",
            "model_reasoning_effort",
            "service_tier",
            "desktop.conversationDetailMode",
            "desktop.enabled-reasoning-efforts",
            "desktop.selected-avatar-id",
        ]),
        bindings: ModelRc::new(VecModel::from(vec![
            PreviewBindingRow {
                provider: "ChatGPT 订阅".into(),
                grok: false,
                facts: facts(&[
                    ("账号", "原生登录"),
                    ("工作区", "synthetic-workspace"),
                    ("服务", "https://chatgpt.com"),
                    ("路由", "NO_CONSTRAINT"),
                ]),
            },
            PreviewBindingRow {
                provider: "Grok".into(),
                grok: true,
                facts: facts(&[("保存账号", "synthetic-grok-account")]),
            },
        ])),
        notes: strings(&[
            "订阅认证由目标 Codex 管理；仅官方模型传递认证。独立本地令牌会写入仅当前用户可读的配置及恢复记录。",
            "Grok 账号在发布时固定；更换账号或模型请启动新会话。",
            LAST_NOTE,
        ]),
    }
}

fn ready(app: &AppWindow) {
    app.set_busy(false);
    app.set_route_preview(preview(app));
    app.set_route_preview_ready(true);
    app.set_error_code("".into());
    app.set_error_message("".into());
    app.set_error_action("".into());
    app.set_action_message("发布预览已准备；确认启用后，在新的 Codex 会话中选择模型。".into());
}

fn checking(app: &AppWindow) {
    app.set_busy(true);
    app.set_route_preview_ready(false);
    app.set_route_preview(Default::default());
    app.set_error_code("".into());
    app.set_error_message("".into());
    app.set_error_action("".into());
    app.set_action_message("正在核对模型目录、连接与目标配置…".into());
}

fn reset(app: &AppWindow, window: &MinimalSoftwareWindow, initially_checking: bool) {
    app.set_publish_drawer_open(false);
    app.set_busy(false);
    app.set_config_managed(false);
    app.set_chatgpt_login_pending(false);
    app.set_xai_pending(false);
    app.set_default_model_label(DEFAULT_DISPLAY_NAME.into());
    app.set_config_home("/Users/preview/.codex".into());
    render(window);
    if initially_checking {
        checking(app);
    } else {
        ready(app);
    }
    app.set_publish_drawer_open(true);
    render(window);
}

pub(super) fn render_route_publish(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
    native: bool,
) -> Result<(), Box<dyn Error>> {
    seed(app)?;
    if native {
        app.window().set_size(PhysicalSize::new(1200, 820));
        app.invoke_set_appearance(!std::env::args().any(|arg| arg == "--light"));
        app.global::<Theme>().set_animations_enabled(true);
        let weak = app.as_weak();
        app.on_inspect_route(move || {
            let app = weak.upgrade().unwrap();
            checking(&app);
            app.set_publish_drawer_open(true);
            let weak = app.as_weak();
            slint::Timer::single_shot(Duration::from_millis(1400), move || {
                if let Some(app) = weak.upgrade() {
                    ready(&app);
                }
            });
        });
        let weak = app.as_weak();
        app.on_apply_route(move || {
            if let Some(app) = weak.upgrade() {
                app.set_busy(true);
                let weak = app.as_weak();
                slint::Timer::single_shot(Duration::from_secs(1), move || {
                    if let Some(app) = weak.upgrade() {
                        app.set_busy(false);
                        app.set_publish_drawer_open(false);
                    }
                });
            }
        });
        let weak = app.as_weak();
        slint::Timer::single_shot(Duration::ZERO, move || {
            let app = weak.upgrade().unwrap();
            app.invoke_inspect_route();
        });
        return Ok(app.run()?);
    }

    let inspections = Rc::new(Cell::new(0));
    let calls = inspections.clone();
    let weak = app.as_weak();
    app.on_inspect_route(move || {
        calls.set(calls.get() + 1);
        checking(&weak.upgrade().unwrap());
    });
    let applications = Rc::new(Cell::new(0));
    let calls = applications.clone();
    let weak = app.as_weak();
    app.on_apply_route(move || {
        calls.set(calls.get() + 1);
        weak.upgrade().unwrap().set_busy(true);
    });
    let cancellations = Rc::new(Cell::new(0));
    let calls = cancellations.clone();
    app.on_cancel_route_preview(move || calls.set(calls.get() + 1));

    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            app.global::<Theme>().set_animations_enabled(false);
            set_theme(app, window, dark);
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            reset(app, window, true);
            assert_checking(app);
            snapshot(
                window,
                output,
                &format!("publish-initial-checking-{suffix}"),
            )?;
            ready(app);
            render(window);
            assert_display_name(app);
            snapshot(window, output, &format!("publish-ready-{suffix}"))?;

            click(app, window, EXPAND);
            assert_eq!(control(app, COLLAPSE).accessible_expanded(), Some(true));
            assert_display_name(app);
            snapshot(window, output, &format!("publish-details-{suffix}"))?;
            scroll_body_to_details(
                app,
                window,
                output,
                &format!("publish-details-scrolled-{suffix}"),
            )?;
            click(app, window, COLLAPSE);
            assert_eq!(control(app, EXPAND).accessible_expanded(), Some(false));

            let inspect_count = inspections.get();
            click(app, window, INSPECT);
            assert_eq!(inspections.get(), inspect_count + 1);
            assert_checking(app);
            snapshot(window, output, &format!("publish-checking-{suffix}"))?;
            let apply_count = applications.get();
            click(app, window, APPLY);
            click(app, window, INSPECT);
            let cancel_count = cancellations.get();
            click(app, window, "关闭");
            assert!(app.get_publish_drawer_open(), "busy must block dismissing");
            assert_eq!(cancellations.get(), cancel_count);
            assert_eq!(applications.get(), apply_count, "busy must block enabling");
            assert_eq!(inspections.get(), inspect_count + 1, "no duplicate check");

            ready(app);
            render(window);
            click(app, window, APPLY);
            assert_eq!(applications.get(), apply_count + 1);
            assert!(app.get_busy(), "confirmation must enter a real busy state");
            assert!(visible_text(app).contains("正在启用你的模型…"));
            assert_display_name(app);
            snapshot(window, output, &format!("publish-applying-{suffix}"))?;
            click(app, window, APPLY);
            assert_eq!(applications.get(), apply_count + 1, "no duplicate apply");
            app.set_busy(false);
            app.set_route_preview_ready(false);
            render(window);
            click(app, window, APPLY);
            assert_eq!(
                applications.get(),
                apply_count + 1,
                "stale preview cannot apply"
            );
            snapshot(window, output, &format!("publish-awaiting-check-{suffix}"))?;

            app.set_error_code("SYNTHETIC_CONNECTION_UNAVAILABLE".into());
            app.set_error_message("无法连接合成 API 服务，模型目录检查未完成。".into());
            app.set_error_action("检查连接设置后重试".into());
            app.set_action_message("".into());
            snapshot(window, output, &format!("publish-error-{suffix}"))?;
            click(app, window, APPLY);
            assert_eq!(
                applications.get(),
                apply_count + 1,
                "failed check cannot apply"
            );

            click(app, window, INSPECT);
            assert_eq!(
                inspections.get(),
                inspect_count + 2,
                "failed checks can retry"
            );
            assert_checking(app);
            assert!(
                app.get_error_message().is_empty(),
                "retry clears the previous error"
            );
            snapshot(window, output, &format!("publish-retrying-{suffix}"))?;
            ready(app);
            render(window);
            assert_display_name(app);
            assert!(visible_text(app).contains("模型目录与发布配置已核对"));
            snapshot(window, output, &format!("publish-retry-passed-{suffix}"))?;

            app.set_default_model_label(
                "synthetic-extremely-long-model-display-name/合成团队的共享订阅连接".into(),
            );
            app.set_config_home(
                "/Users/preview/Developer/synthetic-workspaces/review-environment/.codex".into(),
            );
            render(window);
            app.set_route_preview_ready(true);
            snapshot(window, output, &format!("publish-long-labels-{suffix}"))?;

            app.set_config_managed(true);
            render(window);
            click(app, window, INSPECT);
            click(app, window, APPLY);
            assert_eq!(
                inspections.get(),
                inspect_count + 2,
                "managed config blocks checks"
            );
            assert_eq!(
                applications.get(),
                apply_count + 1,
                "managed config blocks apply"
            );
            app.set_config_managed(false);

            click(app, window, EXPAND);
            let cancel_count = cancellations.get();
            click(app, window, "关闭");
            assert!(!app.get_publish_drawer_open());
            assert_eq!(cancellations.get(), cancel_count + 1);
            // Finish dismissing before a fresh opening; interrupted exits may
            // intentionally preserve the still-visible diagnostic layout.
            render(window);
            app.set_publish_drawer_open(true);
            render(window);
            assert_eq!(control(app, EXPAND).accessible_expanded(), Some(false));
            app.set_publish_drawer_open(false);
            render(window);
        }
    }

    app.window().set_size(PhysicalSize::new(1200, 820));
    set_theme(app, window, true);
    reset(app, window, true);
    app.set_publish_drawer_open(false);
    render(window);
    app.global::<Theme>().set_animations_enabled(true);
    app.set_publish_drawer_open(true);
    play(window, output, "publish-entry", &every(16, 640))?;
    assert_checking(app);
    play(window, output, "publish-checking", &every(16, 1920))?;
    ready(app);
    play(window, output, "publish-check-complete", &every(16, 640))?;
    window.dispatch_event(WindowEvent::PointerMoved {
        position: point(app, APPLY),
    });
    play(window, output, "publish-confirm-hover", &every(16, 256))?;
    click(app, window, EXPAND);
    play(window, output, "publish-details-open", &every(16, 480))?;
    click(app, window, COLLAPSE);
    frame(64);
    headless::render(window);
    click(app, window, EXPAND);
    play(window, output, "publish-details-reversed", &every(16, 480))?;
    click(app, window, COLLAPSE);
    play(window, output, "publish-details-close", &every(16, 480))?;
    click(app, window, APPLY);
    assert!(app.get_busy());
    assert!(visible_text(app).contains("正在启用你的模型…"));
    play(window, output, "publish-applying", &every(16, 960))?;
    app.set_busy(false);
    app.set_publish_drawer_open(false);
    play(window, output, "publish-exit", &every(16, 480))?;

    // An external workbench action can reopen the sheet before its closing
    // animation finishes. A previous failed apply must not label a new check
    // as an application operation while the delayed cleanup is still pending.
    reset(app, window, false);
    render(window);
    click(app, window, APPLY);
    assert!(app.get_busy());
    assert!(visible_text(app).contains("正在启用你的模型…"));
    app.set_busy(false);
    app.set_route_preview_ready(false);
    app.set_error_message("合成启用失败，请重新检查。".into());
    app.set_publish_drawer_open(false);
    headless::render(window);
    frame(48);
    headless::render(window);
    checking(app);
    app.set_publish_drawer_open(true);
    render(window);
    assert_checking(app);
    assert!(
        !visible_text(app).contains("正在启用你的模型…"),
        "a rapid external reopen must clear the previous applying operation"
    );

    // System reduced motion must settle without decorative movement, including
    // the checking state, while keeping controls and state transitions usable.
    app.global::<Theme>().set_system_reduced_motion(true);
    reset(app, window, false);
    snapshot_now(window, output, "publish-reduced-ready")?;
    checking(app);
    render(window);
    assert_checking(app);
    window.dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(5.0, 5.0),
    });
    let before = headless::render(window);
    snapshot_now(window, output, "publish-reduced-checking-0000ms")?;
    frame(640);
    let after = headless::render(window);
    snapshot_now(window, output, "publish-reduced-checking-0640ms")?;
    assert!(
        before.as_bytes() == after.as_bytes(),
        "reduced motion must stop ambient animation"
    );
    ready(app);
    render(window);
    click(app, window, EXPAND);
    assert_eq!(control(app, COLLAPSE).accessible_expanded(), Some(true));
    snapshot_now(window, output, "publish-reduced-details")?;
    app.global::<Theme>().set_system_reduced_motion(false);
    app.global::<Theme>().set_animations_enabled(false);

    fs::write(
        output.join("publish-validation.txt"),
        format!(
            "Synthetic data only; no account, filesystem configuration, or upstream access.\n\
             Themes: light and dark. Sizes: 1000x680 and 1200x820.\n\
             States: initial checking, ready, details, rechecking, applying, awaiting-check, error, retry, passed, long labels.\n\
             Model labels: custom display name is visible; internal sx-synthetic public IDs are absent.\n\
             Real pointer callbacks: {} checks, {} confirmations, {} cancellations.\n\
             Blocked actions: checking/applying busy, stale preview, failed check, managed configuration.\n\
             Recovery: failed check, actual retry callback, loading, successful check.\n\
             Interrupted exit: failed apply, close, external checking reopen after 48 ms.\n\
             Disclosure: expand, collapse, rapid reversal, reset after close.\n\
             Animation frames: initial checking entry, checking, completion, hover, details, applying, applied exit.\n\
             Reduced motion: checking frame pixels unchanged after 640 ms.\n",
            inspections.get(),
            applications.get(),
            cancellations.get()
        ),
    )?;
    println!("Synthetic route publication previews: {}", output.display());
    Ok(())
}
