use slint::{ModelRc, VecModel};
use switchx::{
    provider_icons,
    ui::{AccountRow, AppWindow, ModelRow, ProviderIconRow, ProviderRow},
};

use crate::activity::activity_row;

/// Every bundled provider icon as a picker row.
pub(super) fn icons() -> Result<Vec<ProviderIconRow>, slint::LoadImageError> {
    provider_icons::PROVIDER_ICONS
        .iter()
        .map(|entry| {
            Ok(ProviderIconRow {
                id: entry.id.into(),
                name: entry.name.into(),
                icon: provider_icons::load_image(entry)?,
                monochrome: entry.monochrome,
            })
        })
        .collect::<Result<Vec<_>, slint::LoadImageError>>()
}

pub(super) fn icon(icons: &[ProviderIconRow], id: &str) -> ProviderIconRow {
    icons.iter().find(|row| row.id == id).unwrap().clone()
}

/// A subscription, a Grok, and a custom API connection.
pub(super) fn providers(icons: &[ProviderIconRow]) -> ModelRc<ProviderRow> {
    let openai = icon(icons, "openai");
    let grok = icon(icons, "grok");
    let custom = icon(icons, "google");
    ModelRc::new(VecModel::from(vec![
        ProviderRow {
            id: "preview-subscription".into(),
            name: "ChatGPT · 默认 OpenAI 头像".into(),
            endpoint: "ChatGPT 订阅连接".into(),
            model_id: "预览模型".into(),
            is_subscription: true,
            binding_label: "预览账号（合成资料）".into(),
            credential_status: "预览".into(),
            icon_id: openai.id.clone(),
            icon: openai.icon.clone(),
            monochrome: openai.monochrome,
            ..Default::default()
        },
        ProviderRow {
            id: "preview-grok".into(),
            name: "Grok".into(),
            is_subscription: true,
            is_grok: true,
            binding_label: "绑定：预览账号（合成资料）".into(),
            credential_status: "Grok OAuth · 发布时核对账号绑定".into(),
            icon_id: grok.id.clone(),
            icon: grok.icon.clone(),
            monochrome: grok.monochrome,
            ..Default::default()
        },
        ProviderRow {
            id: "preview-custom".into(),
            name: "自定义 API · Google 头像".into(),
            endpoint: "https://example.invalid/v1".into(),
            model_id: "preview-model".into(),
            credential_status: "预览".into(),
            icon_id: custom.id.clone(),
            icon: custom.icon.clone(),
            monochrome: custom.monochrome,
            ..Default::default()
        },
    ]))
}

/// Workbench connections, accounts, models, and one request.
pub(super) fn seed_workbench(
    app: &AppWindow,
    providers: ModelRc<ProviderRow>,
    icons: &[ProviderIconRow],
) {
    app.set_providers(providers.clone());
    app.set_filtered_providers(providers);
    app.set_model_provider_ids(ModelRc::new(VecModel::from(vec![
        "preview-subscription".into(),
        "preview-custom".into(),
    ])));
    app.set_model_provider_options(ModelRc::new(VecModel::from(vec![
        "ChatGPT".into(),
        "合成 API 连接".into(),
    ])));
    app.set_provider_icons(ModelRc::new(VecModel::from(icons.to_vec())));

    app.set_config_home("/isolated/codex".into());
    app.set_account_status("已保存 2 个账号 · 各上游绑定独立管理".into());
    app.set_auth_status("此目录的官方登录状态尚未检查".into());
    app.set_accounts(ModelRc::new(VecModel::from(vec![
        AccountRow {
            id: "synthetic-account-a".into(),
            label: "synthetic-a@example.invalid".into(),
            workspace: "synthetic-workspace-a".into(),
            bound_provider_count: 1,
            ..Default::default()
        },
        AccountRow {
            id: "synthetic-account-b".into(),
            label: "synthetic-b@example.invalid".into(),
            workspace: "synthetic-workspace-b".into(),
            is_default: true,
            is_active: true,
            ..Default::default()
        },
    ])));
    app.set_models(ModelRc::new(VecModel::from(
        (0..9)
            .map(|index| ModelRow {
                provider_id: if index < 2 {
                    "preview-subscription"
                } else {
                    "preview-custom"
                }
                .into(),
                provider_name: if index < 2 {
                    "ChatGPT"
                } else {
                    "合成 API 连接"
                }
                .into(),
                display_name: match index {
                    0 => "Codex · 主力".into(),
                    1 => "Codex · 轻量".into(),
                    _ => format!("编程模型 {}", index + 1).into(),
                },
                public_id: format!("sx-preview-{}", index + 1).into(),
                upstream_model: format!("synthetic-model-{}", index + 1).into(),
                saved: true,
                ready: true,
                included: index < 3,
                is_subscription: index < 2,
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    )));
    app.set_selected_model_count(3);
    app.set_default_model("sx-preview-1".into());
    app.set_requests(ModelRc::new(VecModel::from(vec![activity_row(0, 0)])));
}
