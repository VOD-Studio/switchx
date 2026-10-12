use std::{cell::Cell, error::Error, path::Path, rc::Rc};

use slint::platform::{WindowEvent, software_renderer::MinimalSoftwareWindow};
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};
use switchx::{
    headless,
    ui::{AccountRow, AppWindow, CodexQuotaRow, Theme, XaiQuotaRow},
};

use crate::{
    connections::render_connections_motion,
    support::{render, set_theme, snapshot},
};

/// Account cards, details, native login, and the login confirmation.
pub(super) fn render_design(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
    motion: bool,
) -> Result<(), Box<dyn Error>> {
    app.set_active_page(1);
    app.set_connections_tab(1);
    app.set_status_text("合成账号界面检查；未访问实际账号或 Codex 配置".into());
    app.show()?;
    let chatgpt_account = AccountRow {
        id: "design-chatgpt".into(),
        label: "personal@example.invalid".into(),
        initial: "P".into(),
        workspace: "synthetic-workspace-0001".into(),
        is_default: true,
        is_active: true,
        codex_quota: CodexQuotaRow {
            has_value: true,
            primary_window: XaiQuotaRow {
                has_value: true,
                remaining_percent: 78.0,
                period_label: "每周额度".into(),
                reset_label: "5 天 17 小时后重置".into(),
                reset_detail: "重置于 10月15日 09:30".into(),
                ..Default::default()
            },
            resets_label: "可用额度重置 3 次 · 最早到期 10月23日 04:26".into(),
            updated_label: "刚刚更新".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    let grok_account = AccountRow {
        id: "design-grok".into(),
        label: "grok@example.invalid".into(),
        initial: "G".into(),
        workspace: "已保存授权".into(),
        is_default: true,
        bound_provider_count: 1,
        bound_provider_names: "Grok 订阅连接".into(),
        quota: XaiQuotaRow {
            has_value: true,
            remaining_percent: 97.0,
            period_label: "每周额度".into(),
            reset_label: "4 天 19 小时后重置".into(),
            reset_detail: "重置于 10月14日 11:36".into(),
            updated_label: "刚刚更新".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    if motion {
        app.set_accounts(ModelRc::new(VecModel::from(vec![chatgpt_account])));
        app.set_xai_accounts(ModelRc::new(VecModel::from(vec![grok_account])));
        return render_connections_motion(app, window, output);
    }
    let writes = Rc::new(Cell::new(0));
    let removals = Rc::new(Cell::new(0));
    let write_count = writes.clone();
    let removal_count = removals.clone();
    app.on_account_action(move |action, id| {
        if action == 3 || action == 4 {
            assert_eq!(id, "design-chatgpt");
            if action == 3 {
                write_count.set(write_count.get() + 1);
            } else {
                removal_count.set(removal_count.get() + 1);
            }
        }
    });
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            set_theme(app, window, dark);
            app.set_accounts(ModelRc::new(VecModel::from(vec![chatgpt_account.clone()])));
            app.set_xai_accounts(ModelRc::new(VecModel::from(vec![grok_account.clone()])));
            app.set_expanded_chatgpt_account_id("".into());
            app.set_expanded_grok_account_id("".into());
            app.set_native_login_expanded(false);
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            snapshot(window, output, &format!("accounts-ready-{suffix}"))?;
            app.set_expanded_chatgpt_account_id(chatgpt_account.id.clone());
            app.set_expanded_grok_account_id(grok_account.id.clone());
            snapshot(window, output, &format!("accounts-details-{suffix}"))?;
            app.set_accounts(ModelRc::new(VecModel::from(vec![chatgpt_account.clone()])));
            assert_eq!(app.get_expanded_chatgpt_account_id(), chatgpt_account.id);
            app.set_expanded_chatgpt_account_id("".into());
            app.set_expanded_grok_account_id("".into());
            app.set_native_login_expanded(true);
            snapshot(window, output, &format!("accounts-native-{suffix}"))?;
            app.set_native_login_expanded(false);
            app.set_account_login_confirm_label(chatgpt_account.label.clone());
            app.set_account_login_confirm_id(chatgpt_account.id.clone());
            app.set_account_login_confirm_open(true);
            snapshot(window, output, &format!("accounts-confirm-{suffix}"))?;
            window.dispatch_event(WindowEvent::KeyPressed {
                text: slint::platform::Key::Escape.into(),
            });
            window.dispatch_event(WindowEvent::KeyReleased {
                text: slint::platform::Key::Escape.into(),
            });
            assert!(!app.get_account_login_confirm_open());
        }
    }
    app.window().set_size(PhysicalSize::new(1200, 820));
    app.global::<Theme>().set_animations_enabled(false);
    app.set_native_login_expanded(false);
    render(window);
    headless::click(window, 1126.0, 308.0);
    snapshot(window, output, "accounts-menu-dark-1200x820")?;
    headless::click(window, 1000.0, 434.0);
    render(window);
    assert!(app.get_account_login_confirm_open());
    assert_eq!(writes.get(), 0, "Selecting login must only open its review");
    headless::click(window, 720.0, 468.0);
    render(window);
    assert_eq!(writes.get(), 1);
    assert!(!app.get_account_login_confirm_open());

    app.global::<Theme>().set_animations_enabled(true);
    app.set_account_removing_id("design-chatgpt".into());
    render(window);
    assert_eq!(removals.get(), 1);
    render(window);
    assert_eq!(removals.get(), 1, "Confirmed removal must dispatch once");
    app.set_account_removing_id("".into());
    println!("Synthetic account design previews: {}", output.display());
    Ok(())
}

/// ChatGPT quota card states.
pub(super) fn render_codex_quota(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    app.set_xai_accounts(ModelRc::default());
    app.set_account_status("已保存 1 个 ChatGPT 账号（合成资料）".into());
    app.set_active_page(1);
    app.set_connections_tab(1);
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            set_theme(app, window, dark);
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            for state in [
                "ready",
                "low",
                "empty",
                "loading",
                "refreshing",
                "failed",
                "stale",
                "monthly",
                "single",
                "credits-only",
                "multiple",
            ] {
                let has_value = !matches!(state, "loading" | "failed");
                let mut quota = CodexQuotaRow {
                    has_value,
                    primary_window: XaiQuotaRow {
                        has_value: has_value && state != "credits-only",
                        remaining_percent: match state {
                            "low" => 8.0,
                            "empty" => 0.0,
                            _ => 75.0,
                        },
                        period_label: "5 小时额度".into(),
                        reset_label: "2 小时 18 分后重置".into(),
                        reset_detail: "重置于 10月09日 18:00".into(),
                        ..Default::default()
                    },
                    secondary_window: XaiQuotaRow {
                        has_value: has_value && !matches!(state, "single" | "credits-only"),
                        remaining_percent: 62.0,
                        period_label: if state == "monthly" {
                            "30 天额度"
                        } else {
                            "每周额度"
                        }
                        .into(),
                        reset_label: "4 天 21 小时后重置".into(),
                        reset_detail: "重置于 10月14日 11:00".into(),
                        ..Default::default()
                    },
                    credits_label: if has_value {
                        "Codex Credits 余额：62500"
                    } else {
                        ""
                    }
                    .into(),
                    resets_label: if has_value && state != "credits-only" {
                        "可用额度重置 2 次 · 最早到期 10月11日 12:00"
                    } else {
                        ""
                    }
                    .into(),
                    resets_warning: true,
                    updated_label: if has_value { "2 分钟前更新" } else { "" }.into(),
                    loading: matches!(state, "loading" | "refreshing"),
                    ..Default::default()
                };
                if matches!(state, "failed" | "stale") {
                    quota.error = "额度查询连接失败，请稍后刷新".into();
                }
                let account = AccountRow {
                    id: "synthetic-chatgpt-account".into(),
                    label: "preview@example.invalid".into(),
                    workspace: "synthetic-workspace".into(),
                    is_default: true,
                    is_active: true,
                    bound_provider_count: 1,
                    codex_quota: quota,
                    ..Default::default()
                };
                let mut rows = vec![account.clone()];
                if state == "multiple" {
                    rows.push(AccountRow {
                        id: "second-account".into(),
                        label: "second@example.invalid".into(),
                        is_default: false,
                        is_active: false,
                        ..account
                    });
                }
                app.set_accounts(ModelRc::new(VecModel::from(rows)));
                snapshot(window, output, &format!("codex-quota-{state}-{suffix}"))?;
            }
        }
    }
    println!("Synthetic ChatGPT quota previews: {}", output.display());
    Ok(())
}

/// Grok quota card states.
pub(super) fn render_xai_quota(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    app.set_accounts(ModelRc::new(VecModel::from(Vec::<AccountRow>::new())));
    app.set_account_status("尚未保存 ChatGPT 账号（合成资料）".into());
    app.set_xai_status("已保存 1 个 Grok 账号".into());
    app.set_active_page(1);
    app.set_connections_tab(1);
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            set_theme(app, window, dark);
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            for state in [
                "ready",
                "low",
                "empty",
                "loading",
                "refreshing",
                "failed",
                "stale",
                "reauth",
            ] {
                let mut quota = XaiQuotaRow {
                    has_value: !matches!(state, "loading" | "failed" | "reauth"),
                    remaining_percent: if state == "low" {
                        8.0
                    } else if state == "empty" {
                        0.0
                    } else {
                        98.0
                    },
                    period_label: "每周额度".into(),
                    reset_label: "4 天 21 小时后重置".into(),
                    reset_detail: "重置于 10月14日 11:00".into(),
                    updated_label: if matches!(state, "loading" | "failed" | "reauth") {
                        ""
                    } else {
                        "2 分钟前更新"
                    }
                    .into(),
                    loading: matches!(state, "loading" | "refreshing"),
                    ..Default::default()
                };
                if matches!(state, "failed" | "stale") {
                    quota.error = "额度查询连接失败，请稍后刷新".into();
                }
                app.set_xai_accounts(ModelRc::new(VecModel::from(vec![AccountRow {
                    id: "synthetic-grok-account".into(),
                    label: "preview@example.invalid".into(),
                    workspace: if state == "reauth" {
                        "凭据失效，请重新登录"
                    } else {
                        "已保存授权"
                    }
                    .into(),
                    is_default: true,
                    requires_reauth: state == "reauth",
                    bound_provider_count: 1,
                    quota,
                    ..Default::default()
                }])));
                render(window);
                window.dispatch_event(WindowEvent::PointerScrolled {
                    position: slint::LogicalPosition::new(
                        width as f32 - 60.0,
                        height as f32 - 100.0,
                    ),
                    delta_x: 0.0,
                    delta_y: -2000.0,
                });
                snapshot(window, output, &format!("grok-quota-{state}-{suffix}"))?;
                if state == "low" {
                    let pixels = render(window);
                    // A short quota fill must remain anchored at the meter's left edge.
                    let expected = if dark {
                        (225, 184, 121)
                    } else {
                        (148, 102, 33)
                    };
                    let meter = (255..height - 58)
                        .flat_map(|y| (300..330).map(move |x| (y * width + x) as usize))
                        .find(|&offset| {
                            pixels.as_slice()[offset..offset + 8]
                                .iter()
                                .all(|pixel| (pixel.r, pixel.g, pixel.b) == expected)
                        })
                        .expect("low quota fill must remain at the track's left edge");
                    let track = pixels.as_slice()[meter + 80];
                    let expected_track = if dark { (51, 58, 53) } else { (228, 231, 225) };
                    assert_eq!((track.r, track.g, track.b), expected_track);
                }
            }
        }
    }
    println!("Synthetic Grok quota previews: {}", output.display());
    Ok(())
}
