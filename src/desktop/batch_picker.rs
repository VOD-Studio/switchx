//! The batch model picker shared by the connection form, mapping editor, and directory.

use slint::{ComponentHandle, Model, ModelRc, VecModel};
use switchx::{
    app, catalog,
    ui::{AppWindow, BatchModelRow, BatchSummary},
};
use tokio::sync::mpsc;

use super::command::{BatchForm, Command, queue};

pub(super) fn update_batch_reasoning_options(app: &AppWindow) {
    let text = app.get_batch_levels();
    let levels = catalog::reasoning_levels(&text).unwrap_or_default();
    let mut options = vec![slint::SharedString::from("未设置")];
    options.extend(levels.into_iter().map(slint::SharedString::from));
    if !options.contains(&app.get_batch_default()) {
        app.set_batch_default("未设置".into());
    }
    app.set_batch_levels_label(catalog::reasoning_label(&text).into());
    app.set_batch_reasoning_options(ModelRc::new(VecModel::from(options)));
}

pub(super) fn show_batch_candidates(app: &AppWindow, candidates: Vec<app::BatchCandidate>) {
    let query = app.get_batch_query().trim().to_lowercase();
    app.set_batch_models(ModelRc::new(VecModel::from(
        candidates
            .into_iter()
            .map(|model| BatchModelRow {
                matched: model.model_id.to_lowercase().contains(&query),
                id: model.model_id.into(),
                added: model.added,
                supported: model.supported,
                checked: false,
                source: model.source.into(),
                template_context: model.context_window.into(),
                template_levels_label: catalog::reasoning_label(&model.reasoning_levels).into(),
                template_levels: model.reasoning_levels.into(),
                template_default: model.default_reasoning.into(),
                context: "".into(),
                levels: "".into(),
                levels_label: "".into(),
            })
            .collect::<Vec<_>>(),
    )));
    update_batch_summary(app);
}

pub(super) fn update_batch_summary(app: &AppWindow) {
    let mut summary = BatchSummary::default();
    for row in app.get_batch_models().iter() {
        let selected = row.checked && row.supported && !row.added;
        summary.total += 1;
        summary.added += i32::from(row.added);
        summary.selected += i32::from(selected);
        if row.matched {
            summary.matched += 1;
            summary.visible_selectable += i32::from(row.supported && !row.added);
            summary.visible_selected += i32::from(selected);
        }
    }
    app.set_batch_summary(summary);
}

/// Changes rows in place so each row animates instead of the list rebuilding.
pub(super) fn update_batch_rows(
    app: &AppWindow,
    mut change: impl FnMut(usize, &mut BatchModelRow) -> bool,
) {
    let rows = app.get_batch_models();
    for index in 0..rows.row_count() {
        if let Some(mut row) = rows.row_data(index)
            && change(index, &mut row)
        {
            rows.set_row_data(index, row);
        }
    }
    update_batch_summary(app);
}

// In the upstream form, preset models are always saved and the default model
// stays picked, so neither can be unticked.
pub(super) fn batch_row_locked(app: &AppWindow, row: &BatchModelRow) -> bool {
    app.get_discovery_scope() == 0
        && (!row.source.is_empty() || row.id == app.get_edit_model().as_str())
}

pub(super) fn lock_batch_rows(app: &AppWindow) {
    update_batch_rows(app, |_, row| {
        let lock = row.supported && !row.added && !row.checked && batch_row_locked(app, row);
        if lock {
            row.checked = true;
        }
        lock
    });
}

pub(super) fn toggle_batch_row(app: &AppWindow, index: usize) {
    update_batch_rows(app, |row_index, row| {
        let toggle = row_index == index
            && row.supported
            && !row.added
            && !(row.checked && batch_row_locked(app, row));
        if toggle {
            row.checked = !row.checked;
        }
        toggle
    });
}

// Only rows matching the search change, like the workbench list.
pub(super) fn select_batch_rows(app: &AppWindow, checked: bool) {
    update_batch_rows(app, |_, row| {
        let change = row.matched
            && row.supported
            && !row.added
            && row.checked != checked
            && (checked || !batch_row_locked(app, row));
        if change {
            row.checked = checked;
        }
        change
    });
}

pub(super) fn filter_batch_rows(app: &AppWindow, query: &str) {
    let query = query.trim().to_lowercase();
    update_batch_rows(app, |_, row| {
        let matched = row.id.to_lowercase().contains(&query);
        let change = row.matched != matched;
        row.matched = matched;
        change
    });
}

pub(super) fn edit_batch_row(app: &AppWindow, index: usize, context: &str, levels: &str) {
    let rows = app.get_batch_models();
    if let Some(mut row) = rows.row_data(index) {
        row.context = context.trim().into();
        row.levels = levels.trim().into();
        row.levels_label = catalog::reasoning_label(levels).into();
        rows.set_row_data(index, row);
    }
}

// Selection survives searching, so hidden picks are still added.
pub(super) fn batch_save_command(app: &AppWindow) -> Option<Command> {
    let models = app
        .get_batch_models()
        .iter()
        .filter(|row| row.checked && row.supported && !row.added)
        .map(|row| {
            (
                row.id.to_string(),
                row.context.trim().to_owned(),
                row.levels.trim().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    (!models.is_empty()).then(|| {
        Command::SaveModels(BatchForm {
            request_overrides: super::models::request_draft(app),
            provider: app.get_model_provider_id().into(),
            models,
            context: app.get_batch_context().trim().into(),
            levels: app.get_batch_levels().into(),
            default_reasoning: if app.get_batch_default() == "未设置" {
                String::new()
            } else {
                app.get_batch_default().into()
            },
        })
    })
}

// Picks only count for a new upstream whose list came from this form.
pub(super) fn picked_provider_models(app: &AppWindow) -> Vec<String> {
    if !app.get_edit_id().is_empty() || app.get_discovery_scope() != 0 {
        return Vec::new();
    }
    app.get_batch_models()
        .iter()
        .filter(|row| row.checked && row.supported && !row.added)
        .map(|row| row.id.to_string())
        .collect()
}

pub(super) fn connect(app: &AppWindow, sender: &mpsc::Sender<Command>) {
    let weak = app.as_weak();
    app.on_batch_toggle(move |index| {
        if let Some(app) = weak.upgrade() {
            toggle_batch_row(&app, index as usize);
        }
    });
    let weak = app.as_weak();
    app.on_batch_select_all(move |checked| {
        if let Some(app) = weak.upgrade() {
            select_batch_rows(&app, checked);
        }
    });
    let weak = app.as_weak();
    app.on_batch_filter(move |query| {
        if let Some(app) = weak.upgrade() {
            filter_batch_rows(&app, &query);
        }
    });
    let weak = app.as_weak();
    app.on_batch_edit(move |index, context, levels| {
        if let Some(app) = weak.upgrade() {
            edit_batch_row(&app, index as usize, &context, &levels);
        }
    });
    let weak = app.as_weak();
    app.on_lock_batch_rows(move || {
        if let Some(app) = weak.upgrade() {
            lock_batch_rows(&app);
        }
    });
    let weak = app.as_weak();
    app.on_update_batch_reasoning_options(move || {
        if let Some(app) = weak.upgrade() {
            update_batch_reasoning_options(&app);
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_save_batch_models(move || {
        if let Some(app) = weak.upgrade()
            && let Some(command) = batch_save_command(&app)
        {
            queue(&app, &callback_sender, command);
            app.set_batch_saving(app.get_busy());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use switchx::{headless, ui::ModelRow};

    use crate::desktop::workbench::mark_fresh_models;

    #[test]
    fn batch_picker_rows_select_filter_edit_and_build_one_save_command() {
        headless::install();
        let app = AppWindow::new().unwrap();
        let candidate = |id: &str, added: bool, supported: bool| app::BatchCandidate {
            model_id: id.into(),
            added,
            supported,
            source: "",
            context_window: String::new(),
            reasoning_levels: String::new(),
            default_reasoning: String::new(),
        };
        app.set_batch_query(" Coder ".into());
        show_batch_candidates(
            &app,
            vec![
                candidate("coder-pro", false, true),
                candidate("coder-mini", true, true),
                candidate("chat-max", false, true),
                candidate("coder bad", false, false),
            ],
        );
        let summary = app.get_batch_summary();
        assert_eq!((summary.total, summary.matched, summary.added), (4, 3, 1));
        assert_eq!(
            summary.visible_selectable, 1,
            "added and unsupported rows stay out"
        );
        assert!(batch_save_command(&app).is_none());

        select_batch_rows(&app, true);
        let rows = app.get_batch_models();
        assert!(rows.row_data(0).unwrap().checked);
        assert!(!rows.row_data(1).unwrap().checked && !rows.row_data(3).unwrap().checked);
        assert!(
            !rows.row_data(2).unwrap().checked,
            "hidden rows keep their state"
        );
        filter_batch_rows(&app, "");
        toggle_batch_row(&app, 2);
        toggle_batch_row(&app, 1);
        assert_eq!(app.get_batch_summary().selected, 2);
        assert_eq!(app.get_batch_summary().visible_selected, 2);

        edit_batch_row(&app, 2, " 64000 ", "max low medium high");
        let edited = app.get_batch_models().row_data(2).unwrap();
        assert_eq!(edited.context, "64000");
        assert_eq!(edited.levels_label, "low → high, max");
        filter_batch_rows(&app, "chat");
        app.set_model_provider_id("provider".into());
        app.set_batch_context(" 200000 ".into());
        app.set_batch_levels("low, high".into());
        update_batch_reasoning_options(&app);
        assert_eq!(app.get_batch_levels_label(), "low, high");
        app.set_batch_default("high".into());
        app.set_model_user_agent("synthetic-agent/1".into());
        let Some(Command::SaveModels(BatchForm {
            request_overrides,
            provider,
            models,
            context,
            levels,
            default_reasoning,
        })) = batch_save_command(&app)
        else {
            panic!("batch save command")
        };
        assert_eq!(request_overrides.0, "synthetic-agent/1");
        assert_eq!(provider, "provider");
        assert_eq!(
            models,
            vec![
                ("coder-pro".into(), String::new(), String::new()),
                (
                    "chat-max".into(),
                    "64000".into(),
                    "max low medium high".into()
                ),
            ]
        );
        assert_eq!((context.as_str(), levels.as_str()), ("200000", "low, high"));
        assert_eq!(default_reasoning, "high");

        app.set_batch_levels("medium".into());
        update_batch_reasoning_options(&app);
        assert_eq!(
            app.get_batch_default(),
            "未设置",
            "a default outside the levels resets"
        );

        app.set_models(ModelRc::new(VecModel::from(vec![
            ModelRow {
                public_id: "sx-old".into(),
                ..Default::default()
            },
            ModelRow {
                public_id: "sx-new".into(),
                ..Default::default()
            },
        ])));
        mark_fresh_models(&app, &["sx-new".into()]);
        let models = app.get_models();
        assert!(!models.row_data(0).unwrap().fresh && models.row_data(1).unwrap().fresh);
    }

    #[test]
    fn upstream_form_locks_preset_and_default_picks_for_new_upstreams_only() {
        headless::install();
        let app = AppWindow::new().unwrap();
        app.set_discovery_scope(0);
        app.set_edit_model("custom-default".into());
        let candidates = app::draft_candidates(
            "https://api.deepseek.com",
            &["deepseek-flash", "custom-default", "custom-other"].map(String::from),
        );
        assert_eq!(candidates[0].source, "预设参数");
        show_batch_candidates(&app, candidates);
        lock_batch_rows(&app);
        let checked = || {
            app.get_batch_models()
                .iter()
                .map(|row| row.checked)
                .collect::<Vec<_>>()
        };
        assert_eq!(checked(), [true, true, false]);
        select_batch_rows(&app, false);
        toggle_batch_row(&app, 1);
        assert_eq!(checked(), [true, true, false], "locked rows stay picked");

        app.set_edit_model("custom-other".into());
        lock_batch_rows(&app);
        toggle_batch_row(&app, 1);
        assert_eq!(checked(), [true, false, true], "the old default unlocks");
        assert_eq!(
            picked_provider_models(&app),
            ["deepseek-flash", "custom-other"]
        );
        app.set_edit_id("saved-upstream".into());
        assert!(picked_provider_models(&app).is_empty());
    }
}
