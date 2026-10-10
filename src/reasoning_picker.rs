//! Presentation helpers for the reasoning picker. Catalog validation remains authoritative.
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::{
    catalog,
    ui::{AppWindow, ReasoningChoice, ReasoningChoices},
};

pub fn connect(app: &AppWindow) {
    let choices = app.global::<ReasoningChoices>();
    choices.on_label(|text| catalog::reasoning_label(&text).into());
    choices.on_choices(|text, query| {
        let selected = catalog::reasoning_levels(&text).unwrap_or_default();
        let query = query.trim().to_lowercase();
        let descriptions = [
            "不思考",
            "最少",
            "轻量",
            "均衡",
            "深入",
            "更深入",
            "最大",
            "极致",
        ];
        ModelRc::new(VecModel::from(
            catalog::REASONING_LEVELS
                .iter()
                .zip(descriptions)
                .filter(|(level, description)| {
                    level.contains(&query) || description.contains(&query)
                })
                .map(|(level, description)| ReasoningChoice {
                    effort: (*level).into(),
                    description: description.into(),
                    selected: selected.contains(level),
                })
                .collect::<Vec<_>>(),
        ))
    });
    choices.on_toggle(|text, effort| {
        let mut selected = catalog::reasoning_levels(&text).unwrap_or_default();
        if let Some(index) = selected.iter().position(|level| *level == effort.as_str()) {
            selected.remove(index);
        } else if let Some(level) = catalog::REASONING_LEVELS
            .iter()
            .find(|level| **level == effort.as_str())
        {
            selected.push(level);
        }
        catalog::reasoning_levels(&selected.join(", "))
            .unwrap_or_default()
            .join(", ")
            .into()
    });
    choices.on_defaults(|text| {
        ModelRc::new(VecModel::from(
            std::iter::once("未设置".into())
                .chain(
                    catalog::reasoning_levels(&text)
                        .unwrap_or_default()
                        .into_iter()
                        .map(Into::into),
                )
                .collect::<Vec<_>>(),
        ))
    });
    choices.on_includes(|text, effort| {
        catalog::reasoning_levels(&text)
            .unwrap_or_default()
            .contains(&effort.as_str())
    });
}
