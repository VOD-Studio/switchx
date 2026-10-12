//! Provider avatars and the avatar picker shared by the connection editors.

use slint::{ComponentHandle, Model, ModelRc, VecModel};
use switchx::{
    provider_icons,
    storage::ProviderKind,
    ui::{AppWindow, ProviderIconRow},
};

pub(super) fn resolved_provider_icon_id(
    kind: ProviderKind,
    base_url: &str,
    icon_id: &str,
) -> &'static str {
    if let Some(icon) = provider_icons::icon(icon_id) {
        icon.id
    } else {
        provider_icons::default_icon_id(kind, base_url)
    }
}

pub(super) fn provider_icon_row(app: &AppWindow, id: &str) -> ProviderIconRow {
    app.get_provider_icons()
        .iter()
        .find(|row| row.id == id)
        .unwrap_or_default()
}

pub(super) fn set_editor_provider_icon(
    app: &AppWindow,
    icon_id: &str,
    kind: ProviderKind,
    base_url: &str,
) {
    let override_id = provider_icons::icon(icon_id).map_or("", |icon| icon.id);
    let row = provider_icon_row(app, resolved_provider_icon_id(kind, base_url, override_id));
    app.set_edit_icon_id(override_id.into());
    app.set_edit_icon_name(row.name);
    app.set_edit_icon(row.icon);
    app.set_edit_icon_monochrome(row.monochrome);
}

pub(super) fn update_editor_provider_icon(app: &AppWindow) {
    set_editor_provider_icon(
        app,
        &app.get_edit_icon_id(),
        if app.get_subscription_editor_open() {
            ProviderKind::Chatgpt
        } else {
            ProviderKind::ApiKey
        },
        &app.get_edit_url(),
    );
}

pub(super) fn filter_provider_icons(app: &AppWindow, query: &str) {
    let matches = provider_icons::search(query);
    let rows = app
        .get_provider_icons()
        .iter()
        .filter(|row| matches.iter().any(|icon| row.id == icon.id))
        .collect::<Vec<_>>();
    app.set_filtered_provider_icons(ModelRc::new(VecModel::from(rows)));
}

pub(super) fn initialize_provider_icons(app: &AppWindow) -> Result<(), slint::LoadImageError> {
    let icons = provider_icons::PROVIDER_ICONS
        .iter()
        .map(|icon| {
            provider_icons::load_image(icon).map(|image| ProviderIconRow {
                id: icon.id.into(),
                name: icon.name.into(),
                icon: image,
                monochrome: icon.monochrome,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let icons = ModelRc::new(VecModel::from(icons));
    app.set_provider_icons(icons.clone());
    app.set_filtered_provider_icons(icons);
    Ok(())
}

pub(super) fn connect(app: &AppWindow) {
    let weak = app.as_weak();
    app.on_update_provider_icon(move || {
        if let Some(app) = weak.upgrade() {
            update_editor_provider_icon(&app);
        }
    });
    let weak = app.as_weak();
    app.on_open_provider_icon_picker(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if app.get_busy()
            || app.get_config_managed()
            || (!app.get_editor_open() && !app.get_subscription_editor_open())
        {
            return;
        }
        app.set_provider_icon_query("".into());
        filter_provider_icons(&app, "");
        app.set_icon_picker_open(true);
    });
    let weak = app.as_weak();
    app.on_filter_provider_icons(move |query| {
        if let Some(app) = weak.upgrade() {
            filter_provider_icons(&app, &query);
        }
    });
    let weak = app.as_weak();
    app.on_choose_provider_icon(move |id| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if app.get_busy()
            || app.get_config_managed()
            || !app.get_icon_picker_open()
            || (!id.is_empty() && provider_icons::icon(&id).is_none())
        {
            return;
        }
        app.set_edit_icon_id(id);
        update_editor_provider_icon(&app);
    });
    let weak = app.as_weak();
    app.on_close_provider_icon_picker(move || {
        if let Some(app) = weak.upgrade() {
            app.set_icon_picker_open(false);
            app.set_provider_icon_query("".into());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use switchx::{
        app, chatgpt, codex::provider_config::CodexOptions, credentials::Secret, headless,
        ui::Theme,
    };

    use crate::desktop::{
        provider_editor::show_provider_editor,
        subscription_editor::{close_subscription_editor, show_subscription_editor},
    };

    #[test]
    fn embedded_provider_logos_decode() {
        for preset in app::PROVIDER_PRESETS {
            let image = slint::Image::load_from_svg_data(preset.icon).unwrap();
            assert!(image.size().width > 0, "{}", preset.id);
            assert!(image.size().height > 0, "{}", preset.id);
        }
        for icon in provider_icons::PROVIDER_ICONS {
            let image = provider_icons::load_image(icon)
                .unwrap_or_else(|error| panic!("{}: {error}", icon.id));
            assert!(image.size().width > 0, "{}", icon.id);
            assert!(image.size().height > 0, "{}", icon.id);
            assert!(
                image
                    .to_rgba8_premultiplied()
                    .unwrap()
                    .as_slice()
                    .iter()
                    .all(|pixel| {
                        pixel.r <= pixel.a && pixel.g <= pixel.a && pixel.b <= pixel.a
                    }),
                "{}",
                icon.id
            );
        }
    }

    #[test]
    fn provider_avatar_picker_preserves_form_drafts_and_defaults() {
        headless::install();
        let app = AppWindow::new().unwrap();
        initialize_provider_icons(&app).unwrap();
        connect(&app);
        app.set_active_page(1);
        // Commit the initial page change to Slint's change trackers, as the
        // native event loop does before the next user interaction.
        slint::platform::update_timers_and_animations();
        show_provider_editor(&app, None, CodexOptions::default(), String::new());
        app.set_edit_name("Custom draft".into());
        app.set_edit_key("synthetic-draft-key".into());
        app.invoke_open_provider_icon_picker();
        assert!(app.get_icon_picker_open());
        assert_eq!(app.get_filtered_provider_icons().row_count(), 110);
        app.invoke_filter_provider_icons("CHATGPT".into());
        assert!(
            app.get_filtered_provider_icons()
                .iter()
                .any(|icon| icon.id == "openai")
        );
        app.invoke_choose_provider_icon("deepseek".into());
        app.invoke_close_provider_icon_picker();
        assert_eq!(app.get_edit_icon_id(), "deepseek");
        assert_eq!(app.get_edit_name(), "Custom draft");
        assert_eq!(app.get_edit_key(), "synthetic-draft-key");
        assert!(!app.get_icon_picker_open());
        app.invoke_open_provider_icon_picker();
        app.invoke_choose_provider_icon("https://example.invalid/icon.svg".into());
        assert_eq!(app.get_edit_icon_id(), "deepseek");
        app.invoke_choose_provider_icon("".into());
        assert!(app.get_edit_icon_id().is_empty());
        assert_eq!(app.get_edit_icon().size().width, 0);
        app.invoke_filter_provider_icons("no-such-provider-icon".into());
        assert_eq!(app.get_filtered_provider_icons().row_count(), 0);

        show_subscription_editor(
            &app,
            None,
            Vec::new(),
            String::new(),
            CodexOptions::default(),
            String::new(),
            Secret::new("{}".into()),
        );
        assert!(app.get_edit_icon_id().is_empty());
        assert_eq!(app.get_edit_icon_name(), "OpenAI");
        assert!(app.get_edit_icon().size().width > 0);
        app.invoke_open_provider_icon_picker();
        app.invoke_choose_provider_icon("kimi".into());
        app.invoke_close_provider_icon_picker();
        assert_eq!(app.get_edit_icon_id(), "kimi");
        assert_eq!(app.get_subscription_auth_json(), "{}");
        app.invoke_open_provider_icon_picker();
        app.invoke_choose_provider_icon("".into());
        assert_eq!(app.get_edit_icon_name(), "OpenAI");

        let weak = app.as_weak();
        app.on_cancel_subscription_editor(move || {
            if let Some(app) = weak.upgrade() {
                close_subscription_editor(&app);
            }
        });
        app.set_active_page(0);
        slint::platform::update_timers_and_animations();
        assert!(!app.get_subscription_editor_open());
        assert!(!app.get_icon_picker_open());
        assert!(app.get_subscription_auth_json().is_empty());

        app.set_active_page(1);
        show_provider_editor(&app, None, CodexOptions::default(), String::new());
        app.set_edit_key("synthetic-unsaved-key".into());
        app.set_active_page(5);
        slint::platform::update_timers_and_animations();
        assert!(!app.get_editor_open());
        assert!(app.get_edit_key().is_empty());

        app.set_config_managed(true);
        app.set_direct_active(true);
        assert_eq!(app.get_connection_label(), "直连已启用");
        app.set_direct_active(false);
        assert_eq!(app.get_connection_label(), "配置待恢复");
        app.set_route_running(true);
        assert_eq!(app.get_connection_label(), "路由已启用");
        app.set_route_running(false);
        app.set_config_managed(false);
        assert_eq!(app.get_connection_label(), "待启用");

        let theme = app.global::<Theme>();
        theme.set_animations_enabled(true);
        assert_eq!(theme.get_drawer_motion(), 340);
        theme.set_system_reduced_motion(true);
        assert_eq!(theme.get_fast(), 0);
        assert_eq!(theme.get_page_motion(), 0);
        assert_eq!(theme.get_drawer_motion(), 0);
        theme.set_system_reduced_motion(false);
        theme.set_animations_enabled(false);
        assert_eq!(theme.get_drawer_motion(), 0);
    }

    #[test]
    fn explicit_provider_avatar_overrides_default_branding() {
        assert_eq!(
            resolved_provider_icon_id(ProviderKind::Chatgpt, chatgpt::BASE_URL, ""),
            "openai"
        );
        assert_eq!(
            resolved_provider_icon_id(ProviderKind::Chatgpt, chatgpt::BASE_URL, "deepseek"),
            "deepseek"
        );
        assert_eq!(
            resolved_provider_icon_id(ProviderKind::ApiKey, "https://api.deepseek.com", "openai"),
            "openai"
        );
        assert_eq!(
            resolved_provider_icon_id(ProviderKind::ApiKey, "https://api.deepseek.com", ""),
            "deepseek"
        );
        assert_eq!(
            resolved_provider_icon_id(
                ProviderKind::ApiKey,
                "https://example.invalid/v1",
                "unknown"
            ),
            ""
        );
    }
}
