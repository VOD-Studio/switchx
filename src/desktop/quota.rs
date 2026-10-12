//! Account quota cards: queries, results, and relative time labels.

use slint::Model;
use switchx::{
    chatgpt::{self, accounts::AccountManager},
    id::new_id,
    ui::{AppWindow, CodexQuotaRow, XaiQuotaRow},
    xai,
};
use tokio::sync::mpsc;

use super::{codex_config::codex_home, command::Command, rows::sync_account_rows, worker::Worker};

pub(super) fn update_quota_times(quota: &mut XaiQuotaRow, now: i64) {
    if let Ok(queried) = quota.queried_at.parse::<i64>() {
        quota.updated_label = quota_updated_label(queried, now).into();
    }
    quota.reset_label = match quota.resets_at.parse::<i64>() {
        Ok(reset) if reset <= now => "已到重置时间，请刷新".into(),
        Ok(reset) => {
            let minutes = ((reset - now) + 59) / 60;
            match minutes {
                0..=59 => format!("{minutes} 分钟后重置"),
                60..=1439 => format!("{} 小时 {} 分后重置", minutes / 60, minutes % 60),
                _ => format!("{} 天 {} 小时后重置", minutes / 1440, minutes / 60 % 24),
            }
            .into()
        }
        _ => "官方未提供重置时间".into(),
    };
}

pub(super) fn quota_updated_label(queried: i64, now: i64) -> String {
    let elapsed = (now - queried).max(0);
    match elapsed {
        0..=59 => "刚刚更新".into(),
        60..=3599 => format!("{} 分钟前更新", elapsed / 60),
        3600..=86399 => format!("{} 小时前更新", elapsed / 3600),
        _ => format!("{} 天前更新", elapsed / 86400),
    }
}

pub(super) fn quota_reset_detail(reset: Option<i64>) -> String {
    reset
        .and_then(|v| chrono::DateTime::from_timestamp(v, 0))
        .map(|v| {
            format!(
                "重置于 {}",
                v.with_timezone(&chrono::Local).format("%m月%d日 %H:%M")
            )
        })
        .unwrap_or_default()
}

pub(super) fn update_codex_quota_times(quota: &mut CodexQuotaRow, now: i64) {
    if let Ok(queried) = quota.queried_at.parse::<i64>() {
        quota.updated_label = quota_updated_label(queried, now).into();
    }
    update_quota_times(&mut quota.primary_window, now);
    update_quota_times(&mut quota.secondary_window, now);
    let expiries = serde_json::from_str::<Vec<Option<i64>>>(&quota.reset_expiries)
        .unwrap_or_default()
        .into_iter()
        .filter(|at| at.is_none_or(|at| at > now))
        .collect::<Vec<_>>();
    let earliest = expiries.iter().flatten().min().copied();
    quota.resets_warning = earliest.is_some_and(|at| at - now <= 3 * 86400);
    quota.resets_label = if expiries.is_empty() {
        String::new()
    } else {
        let expiry = earliest
            .and_then(|at| chrono::DateTime::from_timestamp(at, 0))
            .map(|at| {
                format!(
                    " · 最早到期 {}",
                    at.with_timezone(&chrono::Local).format("%m月%d日 %H:%M")
                )
            })
            .unwrap_or_default();
        format!("可用额度重置 {} 次{expiry}", expiries.len())
    }
    .into();
}

pub(super) fn tick_codex_quotas(app: &AppWindow, now: i64) {
    let rows = app
        .get_accounts()
        .iter()
        .map(|mut row| {
            update_codex_quota_times(&mut row.codex_quota, now);
            row
        })
        .collect::<Vec<_>>();
    sync_account_rows(app.get_accounts(), rows, |model| app.set_accounts(model));
}

pub(super) fn update_codex_quota(
    app: &AppWindow,
    id: &str,
    request_id: &str,
    result: Result<chatgpt::accounts::Quota, String>,
) {
    let mut rows = app.get_accounts().iter().collect::<Vec<_>>();
    let Some(row) = rows
        .iter_mut()
        .find(|row| row.id == id && row.codex_quota.request_id == request_id)
    else {
        return;
    };
    let quota = &mut row.codex_quota;
    quota.loading = false;
    match result {
        Ok(value) => {
            quota.has_value = true;
            let mut windows = value.windows.into_iter().map(|window| XaiQuotaRow {
                has_value: true,
                remaining_percent: window.remaining_percent as f32,
                period_label: window.period_label.into(),
                resets_at: window
                    .resets_at
                    .map(|v| v.to_string())
                    .unwrap_or_default()
                    .into(),
                reset_detail: quota_reset_detail(window.resets_at).into(),
                ..Default::default()
            });
            quota.primary_window = windows.next().unwrap_or_default();
            quota.secondary_window = windows.next().unwrap_or_default();
            quota.credits_label = value
                .credits_balance
                .map(|balance| format!("Codex Credits 余额：{balance}"))
                .unwrap_or_default()
                .into();
            quota.reset_expiries = serde_json::to_string(&value.reset_expires_at)
                .unwrap_or_default()
                .into();
            quota.queried_at = value.queried_at.to_string().into();
            quota.error = "".into();
        }
        Err(error) => quota.error = error.into(),
    }
    update_codex_quota_times(quota, chrono::Utc::now().timestamp());
    sync_account_rows(app.get_accounts(), rows, |model| app.set_accounts(model));
}

pub(super) fn refresh_codex_quota(app: &AppWindow, sender: &mpsc::Sender<Command>, id: &str) {
    if app.get_recovery_only() || app.get_chatgpt_login_pending() {
        return;
    }
    let mut rows = app.get_accounts().iter().collect::<Vec<_>>();
    let mut requests = Vec::new();
    for row in &mut rows {
        if (id.is_empty() || row.id == id) && !row.codex_quota.loading {
            let Ok(request_id) = new_id() else {
                continue;
            };
            row.codex_quota.request_id = request_id.clone().into();
            row.codex_quota.loading = true;
            row.codex_quota.error = "".into();
            requests.push((row.id.to_string(), request_id));
        }
    }
    sync_account_rows(app.get_accounts(), rows, |model| app.set_accounts(model));
    if !requests.is_empty()
        && let Err(error) = sender.try_send(Command::CodexQuota {
            requests,
            home: app.get_config_home().into(),
        })
        && let Command::CodexQuota { requests, .. } = error.into_inner()
    {
        for (id, request_id) in requests {
            update_codex_quota(
                app,
                &id,
                &request_id,
                Err("后台繁忙，请稍后刷新额度".into()),
            );
        }
    }
}

pub(super) fn update_xai_quota_times(app: &AppWindow, now: i64) {
    let mut rows = app.get_xai_accounts().iter().collect::<Vec<_>>();
    for row in &mut rows {
        update_quota_times(&mut row.quota, now);
    }
    sync_account_rows(app.get_xai_accounts(), rows, |model| {
        app.set_xai_accounts(model)
    });
}

pub(super) fn update_xai_quota(
    app: &AppWindow,
    id: &str,
    request_id: &str,
    result: Result<xai::Quota, String>,
) {
    let mut rows = app.get_xai_accounts().iter().collect::<Vec<_>>();
    let Some(row) = rows
        .iter_mut()
        .find(|row| row.id == id && row.quota.request_id == request_id)
    else {
        return; // Removed accounts and superseded requests must not receive old results.
    };
    row.quota.loading = false;
    match result {
        Ok(quota) => {
            row.quota.has_value = true;
            row.quota.remaining_percent = quota.remaining_percent;
            row.quota.period_label = quota.period_label.into();
            row.quota.resets_at = quota
                .resets_at
                .map(|v| v.to_string())
                .unwrap_or_default()
                .into();
            row.quota.queried_at = quota.queried_at.to_string().into();
            row.quota.reset_detail = quota
                .resets_at
                .and_then(|v| chrono::DateTime::from_timestamp(v, 0))
                .map(|v| {
                    format!(
                        "重置于 {}",
                        v.with_timezone(&chrono::Local).format("%m月%d日 %H:%M")
                    )
                })
                .unwrap_or_default()
                .into();
            row.quota.error = "".into();
        }
        Err(error) => row.quota.error = error.into(),
    }
    update_quota_times(&mut row.quota, chrono::Utc::now().timestamp());
    sync_account_rows(app.get_xai_accounts(), rows, |model| {
        app.set_xai_accounts(model)
    });
}

pub(super) fn refresh_xai_quota(app: &AppWindow, sender: &mpsc::Sender<Command>, id: &str) {
    if app.get_recovery_only() || app.get_xai_pending() {
        return;
    }
    let mut rows = app.get_xai_accounts().iter().collect::<Vec<_>>();
    let mut requests = Vec::new();
    for row in &mut rows {
        if (id.is_empty() || row.id == id) && !row.quota.loading && !row.requires_reauth {
            let Ok(request_id) = new_id() else {
                continue;
            };
            row.quota.request_id = request_id.clone().into();
            row.quota.loading = true;
            row.quota.error = "".into();
            requests.push((row.id.to_string(), request_id));
        }
    }
    sync_account_rows(app.get_xai_accounts(), rows, |model| {
        app.set_xai_accounts(model)
    });
    if !requests.is_empty()
        && let Err(error) = sender.try_send(Command::XaiQuota(requests))
    {
        let command = error.into_inner();
        if let Command::XaiQuota(requests) = command {
            for (id, request_id) in requests {
                update_xai_quota(
                    app,
                    &id,
                    &request_id,
                    Err("后台繁忙，请稍后刷新额度".into()),
                );
            }
        }
    }
}

impl Worker {
    pub(super) fn query_codex_quota(&mut self, requests: Vec<(String, String)>, target: String) {
        let context = self.data_dir.path().and_then(|path| {
            if self.quota_account_manager.is_none() {
                self.quota_account_manager = Some(AccountManager::open(path)?);
            }
            Ok((
                self.quota_account_manager.as_ref().unwrap().clone(),
                codex_home(&target)?,
            ))
        });
        for (id, request_id) in requests {
            let context = context.clone();
            let window = self.weak.clone();
            self.codex_quota_queries.spawn(async move {
                let result = match context {
                    Ok((manager, home)) => manager.quota(&id, &home).await,
                    Err(error) => Err(error),
                };
                let _ = window.upgrade_in_event_loop(move |app| {
                    update_codex_quota(&app, &id, &request_id, result)
                });
            });
        }
    }

    pub(super) fn query_xai_quota(&mut self, requests: Vec<(String, String)>) {
        let manager = self.xai_manager();
        for (id, request_id) in requests {
            let manager = manager.clone();
            let window = self.weak.clone();
            self.xai_quota_queries.spawn(async move {
                let result = match manager {
                    Ok(manager) => manager.quota(&id).await,
                    Err(error) => Err(error),
                };
                let _ = window.upgrade_in_event_loop(move |app| {
                    update_xai_quota(&app, &id, &request_id, result);
                });
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::{ComponentHandle, ModelRc, VecModel};
    use switchx::{
        headless,
        ui::{AccountRow, Theme},
    };

    use crate::desktop::{
        accounts::{AccountView, show_accounts},
        grok::show_xai_accounts,
    };

    #[test]
    fn chatgpt_quota_refresh_preserves_values_and_rejects_old_account_generations() {
        headless::install();
        let app = AppWindow::new().unwrap();
        app.set_accounts(ModelRc::new(VecModel::from(vec![
            AccountRow {
                id: "a".into(),
                credential_generation: "1".into(),
                ..Default::default()
            },
            AccountRow {
                id: "b".into(),
                credential_generation: "1".into(),
                ..Default::default()
            },
        ])));
        let (sender, mut receiver) = mpsc::channel(2);
        refresh_codex_quota(&app, &sender, "a");
        assert!(!app.get_busy());
        let Command::CodexQuota {
            requests: first, ..
        } = receiver.try_recv().unwrap()
        else {
            panic!("quota command")
        };
        refresh_codex_quota(&app, &sender, "");
        let Command::CodexQuota {
            requests: second, ..
        } = receiver.try_recv().unwrap()
        else {
            panic!("quota command")
        };
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].0, "b");
        let now = chrono::Utc::now().timestamp();
        let value = chatgpt::accounts::Quota {
            windows: vec![
                chatgpt::accounts::QuotaWindow {
                    remaining_percent: 75.0,
                    period_label: "5 小时额度".into(),
                    resets_at: Some(now + 3600),
                },
                chatgpt::accounts::QuotaWindow {
                    remaining_percent: 62.0,
                    period_label: "每周额度".into(),
                    resets_at: None,
                },
            ],
            credits_balance: Some(62500.0),
            reset_expires_at: vec![Some(now + 120), None],
            queried_at: now,
        };
        update_codex_quota(&app, "a", &first[0].1, Ok(value.clone()));
        let row = app.get_accounts().row_data(0).unwrap();
        assert_eq!(row.codex_quota.primary_window.remaining_percent, 75.0);
        assert_eq!(
            row.codex_quota.secondary_window.reset_label,
            "官方未提供重置时间"
        );
        assert_eq!(row.codex_quota.credits_label, "Codex Credits 余额：62500");
        assert!(
            row.codex_quota
                .resets_label
                .starts_with("可用额度重置 2 次")
        );
        let mut quota = row.codex_quota;
        update_codex_quota_times(&mut quota, now + 121);
        assert_eq!(quota.updated_label, "2 分钟前更新");
        assert_eq!(quota.resets_label, "可用额度重置 1 次");
        assert!(!quota.resets_warning);
        refresh_codex_quota(&app, &sender, "a");
        let Command::CodexQuota {
            requests: retry, ..
        } = receiver.try_recv().unwrap()
        else {
            panic!("quota command")
        };
        update_codex_quota(&app, "a", &first[0].1, Err("过时结果".into()));
        assert!(app.get_accounts().row_data(0).unwrap().codex_quota.loading);
        update_codex_quota(&app, "a", &retry[0].1, Err("模拟连接失败".into()));
        let row = app.get_accounts().row_data(0).unwrap();
        assert!(row.codex_quota.has_value);
        assert_eq!(row.codex_quota.primary_window.remaining_percent, 75.0);
        assert_eq!(row.codex_quota.error, "模拟连接失败");
        let rows = app
            .get_accounts()
            .iter()
            .map(|row| AccountRow {
                codex_quota: CodexQuotaRow::default(),
                ..row
            })
            .collect::<Vec<_>>();
        show_accounts(
            &app,
            Ok(AccountView {
                rows,
                selected: String::new(),
                status: String::new(),
            }),
        );
        assert!(
            app.get_accounts()
                .row_data(0)
                .unwrap()
                .codex_quota
                .has_value
        );
        let mut rows = app.get_accounts().iter().collect::<Vec<_>>();
        rows[0].credential_generation = "2".into();
        rows[0].codex_quota = CodexQuotaRow::default();
        show_accounts(
            &app,
            Ok(AccountView {
                rows,
                selected: String::new(),
                status: String::new(),
            }),
        );
        update_codex_quota(&app, "a", &retry[0].1, Ok(value.clone()));
        assert!(
            !app.get_accounts()
                .row_data(0)
                .unwrap()
                .codex_quota
                .has_value
        );
        app.set_accounts(ModelRc::default());
        update_codex_quota(&app, "b", &second[0].1, Ok(value));
        assert_eq!(app.get_accounts().row_count(), 0);
        app.set_accounts(ModelRc::new(VecModel::from(vec![AccountRow {
            id: "a".into(),
            ..Default::default()
        }])));
        drop(receiver);
        refresh_codex_quota(&app, &sender, "a");
        let row = app.get_accounts().row_data(0).unwrap();
        assert!(!row.codex_quota.loading);
        assert_eq!(row.codex_quota.error, "后台繁忙，请稍后刷新额度");
    }

    #[test]
    fn grok_quota_refresh_keeps_last_success_and_ignores_outdated_account_results() {
        let window = headless::install();
        let app = AppWindow::new().unwrap();
        app.set_xai_accounts(ModelRc::new(VecModel::from(vec![
            AccountRow {
                id: "a".into(),
                label: "A".into(),
                ..Default::default()
            },
            AccountRow {
                id: "b".into(),
                label: "B".into(),
                ..Default::default()
            },
            AccountRow {
                id: "expired".into(),
                requires_reauth: true,
                ..Default::default()
            },
        ])));
        let (sender, mut receiver) = mpsc::channel(2);
        refresh_xai_quota(&app, &sender, "a");
        assert!(!app.get_busy());
        let Command::XaiQuota(first) = receiver.try_recv().unwrap() else {
            panic!("quota command")
        };
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].0, "a");
        refresh_xai_quota(&app, &sender, "");
        let Command::XaiQuota(second) = receiver.try_recv().unwrap() else {
            panic!("quota command")
        };
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].0, "b");
        let now = chrono::Utc::now().timestamp();
        let quota = xai::Quota {
            remaining_percent: 98.0,
            resets_at: Some(now + 7 * 86400),
            queried_at: now,
            period_label: "每周额度",
        };
        update_xai_quota(&app, "a", &first[0].1, Ok(quota.clone()));
        assert!(app.get_xai_accounts().row_data(0).unwrap().quota.has_value);
        assert_eq!(
            app.get_xai_accounts()
                .row_data(0)
                .unwrap()
                .quota
                .updated_label,
            "刚刚更新"
        );
        let rows = app.get_xai_accounts().iter().collect::<Vec<_>>();
        show_xai_accounts(&app, Ok(rows));
        assert!(app.get_xai_accounts().row_data(1).unwrap().quota.loading);
        refresh_xai_quota(&app, &sender, "a");
        let Command::XaiQuota(retry) = receiver.try_recv().unwrap() else {
            panic!("quota command")
        };
        update_xai_quota(&app, "a", &first[0].1, Err("过时结果".into()));
        assert!(app.get_xai_accounts().row_data(0).unwrap().quota.loading);
        update_xai_quota(&app, "a", &retry[0].1, Err("模拟网络失败".into()));
        let row = app.get_xai_accounts().row_data(0).unwrap();
        assert!(!row.quota.loading);
        assert_eq!(row.quota.remaining_percent, 98.0);
        assert_eq!(row.quota.queried_at, now.to_string());
        assert_eq!(row.quota.error, "模拟网络失败");
        let mut time = row.quota;
        update_quota_times(&mut time, now + 3600);
        assert_eq!(time.updated_label, "1 小时前更新");
        update_quota_times(&mut time, now + 7 * 86400);
        assert_eq!(time.reset_label, "已到重置时间，请刷新");
        app.set_xai_accounts(ModelRc::default());
        update_xai_quota(&app, "b", &second[0].1, Ok(quota));
        assert_eq!(app.get_xai_accounts().row_count(), 0);
        show_xai_accounts(
            &app,
            Ok(vec![AccountRow {
                id: "a".into(),
                ..Default::default()
            }]),
        );
        drop(receiver);
        refresh_xai_quota(&app, &sender, "a");
        let row = app.get_xai_accounts().row_data(0).unwrap();
        assert!(!row.quota.loading);
        assert!(!row.quota.error.is_empty());
        assert!(!app.get_busy());

        // A timer or background response must not move the user's scrolled account page.
        app.set_xai_accounts(ModelRc::new(VecModel::from(vec![AccountRow {
            id: "a".into(),
            label: "preview@example.invalid".into(),
            quota: XaiQuotaRow {
                request_id: "scroll-check".into(),
                ..Default::default()
            },
            ..Default::default()
        }])));
        let quota = xai::Quota {
            remaining_percent: 98.0,
            resets_at: Some(now + 7 * 86400),
            queried_at: now,
            period_label: "每周额度",
        };
        update_xai_quota(&app, "a", "scroll-check", Ok(quota.clone()));
        app.global::<Theme>().set_animations_enabled(false);
        app.global::<Theme>().set_dark(true);
        app.set_loading(false);
        app.set_active_page(1);
        app.set_connections_tab(1);
        app.window().set_size(slint::PhysicalSize::new(1000, 680));
        app.show().unwrap();
        let draw = || headless::render(&window);
        draw();
        headless::scroll(&window, 940.0, 580.0, -2000.0);
        let before = draw();
        // Locate the meter in the current layout, then require both updates to
        // retain its position. Account headers no longer use a green fill.
        let meter = (255..620)
            .flat_map(|y| (300..650).map(move |x| y * 1000 + x))
            .find(|&offset| {
                before.as_slice()[offset..offset + 80]
                    .iter()
                    .all(|pixel| (pixel.r, pixel.g, pixel.b) == (167, 196, 171))
            })
            .expect("quota meter must be visible after scrolling")
            + 40;
        let pixel = before.as_slice()[meter];
        assert_eq!(
            (pixel.r, pixel.g, pixel.b),
            (167, 196, 171),
            "quota meter must be visible after scrolling"
        );
        update_xai_quota_times(&app, now + 120);
        assert_eq!(
            app.get_xai_accounts()
                .row_data(0)
                .unwrap()
                .quota
                .updated_label,
            "2 分钟前更新"
        );
        let after = draw();
        let pixel = after.as_slice()[meter];
        assert_eq!(
            (pixel.r, pixel.g, pixel.b),
            (167, 196, 171),
            "time update must keep the card in view"
        );
        update_xai_quota(&app, "a", "scroll-check", Ok(quota));
        let after = draw();
        let pixel = after.as_slice()[meter];
        assert_eq!(
            (pixel.r, pixel.g, pixel.b),
            (167, 196, 171),
            "background result must keep the card in view"
        );
    }
}
