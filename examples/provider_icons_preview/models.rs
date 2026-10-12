use std::{cell::Cell, error::Error, path::Path, rc::Rc};

use slint::platform::software_renderer::MinimalSoftwareWindow;
use slint::{ComponentHandle, Model, ModelRc, PhysicalSize, VecModel};
use switchx::{
    headless,
    ui::{AppWindow, BatchModelRow, BatchSummary, ConnectionModelRow, Theme},
};

use crate::support::{every, frame, play, render, set_theme, snapshot, snapshot_now};

pub(super) fn batch_rows(query: &str) -> Vec<BatchModelRow> {
    let row = |id: &str| BatchModelRow {
        id: id.into(),
        supported: true,
        matched: id.contains(query),
        ..Default::default()
    };
    vec![
        BatchModelRow {
            checked: true,
            source: "预设参数".into(),
            template_context: "1048576".into(),
            template_levels: "low, high, max".into(),
            template_levels_label: "low, high, max".into(),
            ..row("synthetic-coder-pro")
        },
        BatchModelRow {
            checked: true,
            context: "1050000".into(),
            levels: "low, medium, high, xhigh".into(),
            levels_label: "low → xhigh".into(),
            ..row("synthetic-coder-flash")
        },
        BatchModelRow {
            checked: true,
            ..row("synthetic-chat-max")
        },
        row("synthetic-chat-mini"),
        BatchModelRow {
            added: true,
            ..row("synthetic-model-3")
        },
        row("synthetic-vision-plus"),
        BatchModelRow {
            supported: false,
            ..row("synthetic reasoner@beta")
        },
        row("synthetic-embedding-large"),
    ]
}

pub(super) fn summarize(rows: &[BatchModelRow]) -> BatchSummary {
    let mut summary = BatchSummary::default();
    for row in rows {
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
    summary
}

pub(super) fn render_batch_models(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    let show = |rows: Vec<BatchModelRow>| {
        app.set_batch_summary(summarize(&rows));
        app.set_batch_models(ModelRc::new(VecModel::from(rows)));
    };
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            set_theme(app, window, dark);
            app.set_active_page(0);
            app.set_model_provider_id("preview-custom".into());
            app.set_model_provider_name("合成 API 连接".into());
            app.set_model_original_id("".into());
            app.set_model_batch_mode(true);
            app.set_model_editor_open(true);
            // Let the open-time reset run before filling the picker.
            render(window);

            app.set_discovery_scope(1);
            app.set_fetching_models(true);
            app.set_discovery_tone(0);
            app.set_discovery_message("正在获取模型列表…".into());
            snapshot(window, output, &format!("batch-loading-{suffix}"))?;

            app.set_fetching_models(false);
            app.set_discovery_tone(1);
            app.set_discovery_message("已获取 8 个模型，其中 1 个已添加".into());
            app.set_batch_context("200000".into());
            app.set_batch_levels("low, medium, high".into());
            app.set_batch_levels_label("low → high".into());
            app.set_batch_reasoning_options(ModelRc::new(VecModel::from(vec![
                "未设置".into(),
                "low".into(),
                "medium".into(),
                "high".into(),
            ])));
            app.set_batch_default("medium".into());
            show(batch_rows(""));
            snapshot(window, output, &format!("batch-list-{suffix}"))?;
            // The parameter chip of the first row expands its inline editor.
            headless::click(window, width as f32 - 160.0, 309.0);
            snapshot(window, output, &format!("batch-expanded-{suffix}"))?;
            headless::click(window, width as f32 - 160.0, 309.0);

            app.set_batch_query("coder".into());
            show(batch_rows("coder"));
            snapshot(window, output, &format!("batch-search-{suffix}"))?;

            app.set_batch_query("no-such-model".into());
            show(batch_rows("no-such-model"));
            snapshot(window, output, &format!("batch-no-match-{suffix}"))?;

            app.set_batch_query("".into());
            show(Vec::new());
            app.set_discovery_tone(3);
            app.set_discovery_message("无法连接上游或连接超时".into());
            snapshot(window, output, &format!("batch-error-{suffix}"))?;

            app.set_model_batch_mode(false);
            app.set_discovery_tone(0);
            app.set_discovery_message("".into());
            snapshot(window, output, &format!("batch-single-{suffix}"))?;
            app.set_model_editor_open(false);
            render(window);

            let models = app.get_models();
            for index in 2..5 {
                let mut model = models.row_data(index).unwrap();
                model.fresh = true;
                models.set_row_data(index, model);
            }
            snapshot(window, output, &format!("batch-fresh-{suffix}"))?;
            for index in 2..5 {
                let mut model = models.row_data(index).unwrap();
                model.fresh = false;
                models.set_row_data(index, model);
            }

            // A new upstream can tick discovered models; ★ marks its default.
            app.set_active_page(1);
            app.set_edit_id("".into());
            app.set_edit_name("合成 API 连接".into());
            app.set_edit_url("https://example.invalid/v1".into());
            app.set_edit_model("synthetic-coder-pro".into());
            app.set_edit_config_preview("model = \"synthetic-coder-pro\"\n".into());
            app.set_editor_open(true);
            render(window);
            app.set_discovery_scope(0);
            app.set_discovery_tone(1);
            app.set_discovery_message("已获取 8 个模型".into());
            let mut rows = batch_rows("");
            for row in &mut rows {
                row.checked = row.id == "synthetic-coder-pro" || row.id == "synthetic-chat-mini";
                row.added = false;
            }
            rows[0].checked = true;
            show(rows);
            snapshot(window, output, &format!("provider-picker-{suffix}"))?;
            app.set_batch_query("chat".into());
            show(
                batch_rows("chat")
                    .into_iter()
                    .map(|row| BatchModelRow {
                        checked: row.id == "synthetic-coder-pro" || row.id == "synthetic-chat-mini",
                        added: false,
                        ..row
                    })
                    .collect(),
            );
            snapshot(window, output, &format!("provider-picker-search-{suffix}"))?;
            app.set_batch_query("".into());
            app.set_editor_open(false);
            app.set_active_page(0);
            render(window);
        }
    }

    // Frames from the staggered arrival and a row being picked, with motion on.
    app.window().set_size(PhysicalSize::new(1200, 820));
    set_theme(app, window, false);
    app.global::<Theme>().set_animations_enabled(true);
    app.set_model_batch_mode(true);
    app.set_model_editor_open(true);
    render(window);
    app.set_discovery_scope(1);
    app.set_discovery_tone(1);
    app.set_discovery_message("已获取 8 个模型，其中 1 个已添加".into());
    let mut rows = batch_rows("");
    rows[2].checked = false;
    show(rows.clone());
    let mut elapsed = 0;
    for at in [16, 120, 260, 420, 1400] {
        frame(at - elapsed);
        elapsed = at;
        snapshot_now(window, output, &format!("batch-motion-arrive-{at}ms"))?;
    }
    rows[2].checked = true;
    let models = app.get_batch_models();
    models.set_row_data(2, rows[2].clone());
    app.set_batch_summary(summarize(&rows));
    let mut elapsed = 0;
    for at in [60, 140, 400] {
        frame(at - elapsed);
        elapsed = at;
        snapshot_now(window, output, &format!("batch-motion-pick-{at}ms"))?;
    }
    app.global::<Theme>().set_animations_enabled(false);
    Ok(())
}

pub(super) fn open_mapping(app: &AppWindow, original_id: &str) {
    app.set_active_page(0);
    app.set_model_provider_id("preview-subscription".into());
    app.set_model_provider_name("ChatGPT 订阅".into());
    app.set_model_original_id(original_id.into());
    app.set_model_display_name_custom(false);
    app.set_model_upstream_id("synthetic-sol".into());
    app.set_model_display_name("synthetic-sol/ChatGPT 订阅".into());
    app.set_model_public_id("sx-chatgpt-0000preview0000-synthetic-sol".into());
    app.set_model_context_window("272000".into());
    app.set_model_reasoning_levels("low, medium, high, xhigh, max, ultra".into());
    app.set_model_default_reasoning("low".into());
    app.set_model_source_path("".into());
    app.set_model_delete_confirm(false);
    app.set_model_batch_mode(false);
    app.set_discovery_scope(0);
    app.set_discovery_tone(0);
    app.set_discovery_message("".into());
    app.set_fetched_models(ModelRc::new(VecModel::from(Vec::new())));
    app.set_config_managed(false);
    app.set_model_editor_open(true);
}

pub(super) fn render_model_editor(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    const SAVED_ID: &str = "sx-chatgpt-0000preview0000-synthetic-sol";
    let weak = app.as_weak();
    app.on_update_model_display_name(move || {
        let app = weak.upgrade().unwrap();
        if !app.get_model_display_name_custom() {
            let name = format!(
                "{}/{}",
                app.get_model_upstream_id(),
                app.get_model_provider_name()
            );
            app.set_model_display_name(name.into());
        }
    });
    if std::env::args().any(|arg| arg == "--model-editor-native") {
        app.window().set_size(PhysicalSize::new(1200, 820));
        app.global::<Theme>().set_animations_enabled(true);
        app.invoke_set_appearance(!std::env::args().any(|arg| arg == "--light"));
        open_mapping(app, SAVED_ID);
        return Ok(app.run()?);
    }
    let saves = Rc::new(Cell::new(0));
    let saved = saves.clone();
    app.on_save_model(move || saved.set(saved.get() + 1));
    let deletes = Rc::new(Cell::new(0));
    let deleted = deletes.clone();
    app.on_delete_model(move || deleted.set(deleted.get() + 1));
    let fetches = Rc::new(Cell::new(0));
    let fetched = fetches.clone();
    app.on_fetch_models(move |scope| {
        assert_eq!(scope, 1);
        fetched.set(fetched.get() + 1);
    });
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        let (w, h) = (width as f32, height as f32);
        // The form fills a 648px column whose scroll gutter is 14px.
        let ladder_x = |index: usize| w - 623.0 + index as f32 * 76.0;
        for dark in [false, true] {
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            app.global::<Theme>().set_animations_enabled(false);
            set_theme(app, window, dark);
            open_mapping(app, SAVED_ID);
            snapshot(window, output, &format!("mapping-edit-{suffix}"))?;

            // Typing a name makes it custom; restoring derives it again.
            headless::click(window, w - 400.0, 364.0);
            headless::key(window, "!");
            assert!(app.get_model_display_name_custom());
            snapshot(window, output, &format!("mapping-custom-name-{suffix}"))?;
            headless::click(window, w - 152.0, 364.0);
            assert!(!app.get_model_display_name_custom());
            assert_eq!(app.get_model_display_name(), "synthetic-sol/ChatGPT 订阅");

            headless::clear_clipboard();
            headless::click(window, w - 75.0, 364.0);
            assert_eq!(headless::clipboard(), app.get_model_display_name().as_str());
            snapshot(window, output, &format!("mapping-copied-{suffix}"))?;

            let count = fetches.get();
            headless::click(window, w - 120.0, 490.0);
            assert_eq!(fetches.get(), count + 1, "the fetch button requests models");
            app.set_discovery_scope(1);
            app.set_discovery_tone(1);
            app.set_discovery_message("已获取 3 个模型".into());
            app.set_fetched_models(ModelRc::new(VecModel::from(vec![
                "synthetic-sol".into(),
                "synthetic-terra".into(),
                "synthetic-luna".into(),
            ])));
            snapshot(window, output, &format!("mapping-discovered-{suffix}"))?;
            app.set_discovery_scope(0);
            app.set_discovery_message("".into());
            app.set_fetched_models(ModelRc::new(VecModel::from(Vec::new())));
            render(window);

            // Scrolled to the end, the presets sit 486px above the window's bottom.
            headless::scroll(window, w - 350.0, 400.0, -4000.0);
            snapshot(window, output, &format!("mapping-bottom-{suffix}"))?;
            headless::click(window, w - 119.0, h - 486.0);
            assert_eq!(app.get_model_context_window(), "1000000");
            snapshot(window, output, &format!("mapping-context-1m-{suffix}"))?;
            app.set_model_context_window("0".into());
            snapshot(window, output, &format!("mapping-context-invalid-{suffix}"))?;
            app.set_model_context_window("".into());
            snapshot(window, output, &format!("mapping-context-default-{suffix}"))?;
            app.set_model_context_window("272000".into());

            headless::click(window, ladder_x(0), h - 290.0);
            assert_eq!(
                app.get_model_reasoning_levels(),
                "none, low, medium, high, xhigh, max, ultra"
            );
            // Removing the default level clears the default.
            headless::click(window, ladder_x(2), h - 290.0);
            assert_eq!(
                app.get_model_reasoning_levels(),
                "none, medium, high, xhigh, max, ultra"
            );
            assert_eq!(app.get_model_default_reasoning(), "");
            snapshot(window, output, &format!("mapping-ladder-{suffix}"))?;
            app.set_model_reasoning_levels("low, medium, high, xhigh, max, ultra".into());
            app.set_model_default_reasoning("low".into());

            headless::click(window, w - 360.0, h - 131.0);
            snapshot(window, output, &format!("mapping-advanced-{suffix}"))?;

            let count = deletes.get();
            headless::click(window, w - 624.0, h - 44.0);
            assert!(app.get_model_delete_confirm());
            assert_eq!(deletes.get(), count, "delete needs confirmation");
            snapshot(window, output, &format!("mapping-delete-armed-{suffix}"))?;
            headless::click(window, w - 624.0, h - 44.0);
            assert_eq!(deletes.get(), count + 1);
            app.set_model_delete_confirm(false);

            let count = saves.get();
            headless::click(window, w - 92.0, h - 44.0);
            assert_eq!(saves.get(), count + 1, "save is reachable at every size");
            app.set_config_managed(true);
            headless::click(window, w - 92.0, h - 44.0);
            assert_eq!(
                saves.get(),
                count + 1,
                "managed configuration blocks saving"
            );
            headless::scroll(window, w - 350.0, 400.0, 4000.0);
            snapshot(window, output, &format!("mapping-locked-{suffix}"))?;
            app.set_config_managed(false);

            headless::click(window, w - 210.0, h - 44.0);
            assert!(!app.get_model_editor_open(), "cancel closes the editor");
            render(window);

            open_mapping(app, "");
            app.set_model_public_id("".into());
            app.set_model_upstream_id("".into());
            app.set_model_display_name("".into());
            app.set_model_context_window("".into());
            app.set_model_reasoning_levels("".into());
            app.set_model_default_reasoning("未设置".into());
            snapshot(window, output, &format!("mapping-add-{suffix}"))?;
            app.set_batch_summary(summarize(&batch_rows("")));
            app.set_batch_models(ModelRc::new(VecModel::from(batch_rows(""))));
            app.set_model_batch_mode(true);
            snapshot(window, output, &format!("mapping-batch-{suffix}"))?;
            app.set_model_editor_open(false);
            render(window);
        }
    }

    // Motion: staggered entrance, the travelling route, and each control's response.
    app.window().set_size(PhysicalSize::new(1200, 820));
    for dark in [false, true] {
        let theme = if dark { "dark" } else { "light" };
        app.global::<Theme>().set_animations_enabled(false);
        set_theme(app, window, dark);
        app.global::<Theme>().set_animations_enabled(true);
        open_mapping(app, SAVED_ID);
        play(
            window,
            output,
            &format!("mapping-motion-open-{theme}"),
            &every(32, 1600),
        )?;
        play(
            window,
            output,
            &format!("mapping-motion-route-{theme}"),
            &every(32, 3392),
        )?;
        headless::click(window, 1081.0, 662.0);
        play(
            window,
            output,
            &format!("mapping-motion-context-{theme}"),
            &every(32, 800),
        )?;
        headless::click(window, 1125.0, 364.0);
        play(
            window,
            output,
            &format!("mapping-motion-copy-{theme}"),
            &every(32, 480),
        )?;
        headless::click(window, 576.0, 776.0);
        play(
            window,
            output,
            &format!("mapping-motion-delete-{theme}"),
            &every(32, 480),
        )?;
        app.set_model_delete_confirm(false);
        headless::scroll(window, 850.0, 400.0, -4000.0);
        play(window, output, "unused", &[400])?;
        headless::click(window, 577.0, 530.0);
        play(
            window,
            output,
            &format!("mapping-motion-ladder-{theme}"),
            &every(32, 480),
        )?;
        // The view eases after the last section as it expands.
        headless::click(window, 840.0, 689.0);
        play(
            window,
            output,
            &format!("mapping-motion-advanced-{theme}"),
            &every(32, 480),
        )?;
        app.set_model_editor_open(false);
        play(window, output, "unused", &[480])?;
        // The entrance replays on the next opening.
        open_mapping(app, SAVED_ID);
        play(
            window,
            output,
            &format!("mapping-motion-reopen-{theme}"),
            &[0, 160, 960],
        )?;
        app.set_model_editor_open(false);
        play(window, output, "unused", &[480])?;
    }
    app.global::<Theme>().set_animations_enabled(false);
    println!("Synthetic model mapping editor: {}", output.display());
    Ok(())
}

pub(super) fn render_reasoning_picker(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    app.set_active_page(1);
    app.set_connection_models_name("ChatGPT 订阅".into());
    app.set_connection_models_subscription(true);
    app.set_connection_models_open(true);
    let weak = app.as_weak();
    app.on_connection_model_edited(move || {
        if let Some(app) = weak.upgrade() {
            app.set_connection_models_dirty(true);
        }
    });
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            app.global::<Theme>().set_animations_enabled(false);
            set_theme(app, window, dark);
            app.set_connection_models(ModelRc::new(VecModel::from(
                (0..6)
                    .map(|index| ConnectionModelRow {
                        public_id: format!("sx-synthetic-{index}").into(),
                        display_name: format!("coder-{}/ChatGPT 订阅", index + 1).into(),
                        upstream_model: format!("coder-{}", index + 1).into(),
                        context_window: "272000".into(),
                        reasoning_levels: "low, medium, high, xhigh, max".into(),
                        default_reasoning: "high".into(),
                        ..Default::default()
                    })
                    .collect::<Vec<_>>(),
            )));
            app.set_connection_models_dirty(false);
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            snapshot(window, output, &format!("reasoning-directory-{suffix}"))?;
            app.global::<Theme>().set_animations_enabled(true);
            headless::click(window, width as f32 - 140.0, 251.0);
            play(
                window,
                output,
                &format!("reasoning-open-{suffix}"),
                &(0..=20).map(|frame| frame * 16).collect::<Vec<_>>(),
            )?;
            snapshot(window, output, &format!("reasoning-picker-{suffix}"))?;
            headless::click(window, 300.0, 600.0);
            play(
                window,
                output,
                &format!("reasoning-close-{suffix}"),
                &(0..=10).map(|frame| frame * 16).collect::<Vec<_>>(),
            )?;
        }
    }
    println!("Reasoning picker previews saved to {}", output.display());
    Ok(())
}
