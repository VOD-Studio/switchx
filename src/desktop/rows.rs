//! In-place updates of Slint list models.

use slint::{Model, ModelRc, VecModel};
use switchx::ui::AccountRow;

/// Updates visible cards in place so progress and quota refreshes preserve
/// scroll position, hover state, and completed entry animations.
pub(super) fn sync_rows<T: Clone + PartialEq + 'static>(
    model: ModelRc<T>,
    rows: Vec<T>,
    row_id: impl Fn(&T) -> slint::SharedString,
    replace: impl FnOnce(ModelRc<T>),
) {
    let Some(current) = model.as_any().downcast_ref::<VecModel<T>>() else {
        replace(ModelRc::new(VecModel::from(rows)));
        return;
    };
    let mut index = 0;
    while index < current.row_count() {
        let id = row_id(&current.row_data(index).unwrap());
        if rows.iter().any(|row| row_id(row) == id) {
            index += 1;
        } else {
            current.remove(index);
        }
    }
    let count = rows.len();
    for (index, row) in rows.into_iter().enumerate() {
        let found = (index..current.row_count()).find(|&old| {
            current
                .row_data(old)
                .is_some_and(|old| row_id(&old) == row_id(&row))
        });
        match found {
            Some(old) if old == index => {
                if current.row_data(index).as_ref() != Some(&row) {
                    current.set_row_data(index, row);
                }
            }
            Some(old) => {
                current.remove(old);
                current.insert(index, row);
            }
            None => current.insert(index, row),
        }
    }
    while current.row_count() > count {
        current.remove(current.row_count() - 1);
    }
}

pub(super) fn sync_account_rows(
    model: ModelRc<AccountRow>,
    rows: Vec<AccountRow>,
    replace: impl FnOnce(ModelRc<AccountRow>),
) {
    sync_rows(model, rows, |row| row.id.clone(), replace);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_updates_keep_the_model_so_cards_are_not_recreated() {
        let row = |id: &str, label: &str| AccountRow {
            id: id.into(),
            label: label.into(),
            ..Default::default()
        };
        let ids = |model: &ModelRc<AccountRow>| {
            model
                .iter()
                .map(|row| row.id.to_string())
                .collect::<Vec<_>>()
        };
        let model = ModelRc::new(VecModel::from(vec![row("a", "A"), row("b", "B")]));
        let replace = |_| panic!("an existing account model must be updated in place");
        sync_account_rows(model.clone(), vec![row("a", "A2"), row("b", "B")], replace);
        assert_eq!(model.row_data(0).unwrap().label, "A2");
        sync_account_rows(model.clone(), vec![row("c", "C"), row("a", "A2")], replace);
        assert_eq!(ids(&model), ["c", "a"]);
        sync_account_rows(model.clone(), vec![row("a", "A2"), row("c", "C")], replace);
        assert_eq!(ids(&model), ["a", "c"]);
        sync_account_rows(model.clone(), Vec::new(), replace);
        assert_eq!(model.row_count(), 0);

        let mut replaced = None;
        sync_account_rows(ModelRc::default(), vec![row("a", "A")], |model| {
            replaced = Some(model)
        });
        assert_eq!(ids(&replaced.unwrap()), ["a"]);
    }
}
