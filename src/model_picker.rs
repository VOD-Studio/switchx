//! Model suggestions for editable pickers; manually entered IDs stay authoritative.
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::ui::{AppWindow, ModelChoices};

pub fn connect(app: &AppWindow) {
    app.global::<ModelChoices>().on_filter(filter);
}

pub fn filter(models: ModelRc<SharedString>, query: SharedString) -> ModelRc<SharedString> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return models;
    }
    ModelRc::new(VecModel::from(
        models
            .iter()
            .filter(|model| model.to_lowercase().contains(&query))
            .collect::<Vec<_>>(),
    ))
}
