//! Leaf changes owned by a provider's optional Codex configuration.

use std::{
    collections::BTreeSet,
    hash::{DefaultHasher, Hash, Hasher},
};

use serde::{Deserialize, Serialize};
use toml_edit::{
    Array, ArrayOfTables, Decor, DocumentMut, Formatted, InlineTable, Item, Key, Table, TableLike,
    Value,
};

const MANAGED: [&str; 4] = [
    "model",
    "model_provider",
    "model_catalog_json",
    "model_providers",
];

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Overlay {
    leaves: Vec<LeafChange>,
    added_tables: Vec<AddedTable>,
}

#[derive(Debug, Serialize, Deserialize)]
struct LeafChange {
    path: Vec<String>,
    before: Option<String>,
    applied: Option<String>,
    // Retain external comments without copying user comments into the journal.
    applied_comments: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct AddedTable {
    path: Vec<String>,
    inline: bool,
    implicit: bool,
    dotted: bool,
    applied_comments: u64,
}

impl Overlay {
    pub(crate) fn between(before: &DocumentMut, applied: &DocumentMut) -> Result<Self, String> {
        let mut result = Self::default();
        result.diff(before.as_table(), applied.as_table(), &[])?;
        Ok(result)
    }

    pub(crate) fn paths(&self) -> Vec<String> {
        self.leaves
            .iter()
            .map(|change| display_path(&change.path))
            .chain(
                self.added_tables
                    .iter()
                    .map(|change| display_path(&change.path)),
            )
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn diff(
        &mut self,
        before: &dyn TableLike,
        applied: &dyn TableLike,
        parent: &[String],
    ) -> Result<(), String> {
        let keys = before
            .iter()
            .chain(applied.iter())
            .map(|(key, _)| key)
            .collect::<BTreeSet<_>>();
        for key in keys {
            if parent.is_empty() && MANAGED.contains(&key) {
                continue;
            }
            let mut path = parent.to_vec();
            path.push(key.to_owned());
            let old = before.get(key);
            let new = applied.get(key);
            match (
                old.and_then(Item::as_table_like),
                new.and_then(Item::as_table_like),
            ) {
                (Some(old_table), Some(new_table)) => {
                    if old.unwrap().is_table() != new.unwrap().is_table() {
                        return Err(format!(
                            "通用配置不能改变已有表的类型：{}",
                            display_path(&path)
                        ));
                    }
                    self.diff(old_table, new_table, &path)?;
                }
                (None, Some(new_table)) if old.is_none() => {
                    self.added_tables.push(AddedTable {
                        path: path.clone(),
                        inline: new.unwrap().is_inline_table(),
                        implicit: new.unwrap().as_table().is_some_and(Table::is_implicit),
                        dotted: new_table.is_dotted(),
                        applied_comments: comments_hash(applied.key(key), new),
                    });
                    self.diff(&toml_edit::Table::new(), new_table, &path)?;
                }
                (Some(_), _) | (_, Some(_)) => {
                    return Err(format!(
                        "通用配置不能替换已有表或把值改为表：{}",
                        display_path(&path)
                    ));
                }
                (None, None) => {
                    let before_value = old.map(snapshot).transpose()?;
                    let applied_value = new.map(snapshot).transpose()?;
                    if before_value == applied_value {
                        continue;
                    }
                    if new.is_none() && has_comments(before.key(key), old) {
                        return Err(format!(
                            "{} 带有注释；请先手动移走该注释再切换",
                            display_path(&path)
                        ));
                    }
                    if old.is_some_and(has_inner_comments) {
                        return Err(format!(
                            "{} 的数组内容带有注释；请先手动移走该注释再切换",
                            display_path(&path)
                        ));
                    }
                    self.leaves.push(LeafChange {
                        path,
                        before: before_value,
                        applied: applied_value,
                        applied_comments: leaf_comments_hash(applied.key(key), new),
                    });
                }
            }
        }
        Ok(())
    }

    pub(crate) fn restore(
        &self,
        document: &mut DocumentMut,
        conflicts: &mut Vec<String>,
    ) -> Result<bool, String> {
        let mut changed = false;
        for leaf in &self.leaves {
            validate_path(&leaf.path)?;
            let name = display_path(&leaf.path);
            let Some(parent) = parent_table(document.as_table(), &leaf.path) else {
                // A missing parent may be already restored; a replaced parent belongs to an external writer.
                if leaf.before.is_some() || blocked_parent(document.as_table(), &leaf.path) {
                    conflicts.push(name);
                }
                continue;
            };
            let key = leaf.path.last().unwrap();
            let current = parent.get(key);
            if current.is_some_and(Item::is_table_like) {
                conflicts.push(name);
                continue;
            }
            let found = current.map(snapshot).transpose()?;
            if found == leaf.before {
                continue;
            }
            if found != leaf.applied
                || (leaf.before.is_none()
                    && leaf_comments_hash(parent.key(key), current) != leaf.applied_comments)
            {
                conflicts.push(name);
                continue;
            }
            let parent = parent_table_mut(document.as_table_mut(), &leaf.path).unwrap();
            if let Some(before) = &leaf.before {
                let mut restored = parse_snapshot(before)?;
                if let (Some(old), Some(new)) = (
                    parent.get(key).and_then(Item::as_value),
                    restored.as_value_mut(),
                ) {
                    *new.decor_mut() = old.decor().clone();
                }
                parent.insert(key, restored);
            } else {
                parent.remove(key);
            }
            changed = true;
        }
        // Children are removed before their newly introduced parent tables.
        for table in self.added_tables.iter().rev() {
            validate_path(&table.path)?;
            let Some(parent) = parent_table(document.as_table(), &table.path) else {
                if blocked_parent(document.as_table(), &table.path) {
                    conflicts.push(display_path(&table.path));
                }
                continue;
            };
            let key = table.path.last().unwrap();
            let Some(current) = parent.get(key) else {
                continue;
            };
            let Some(found) = current.as_table_like() else {
                conflicts.push(display_path(&table.path));
                continue;
            };
            if !found.is_empty() {
                continue;
            }
            if current.is_inline_table() != table.inline
                || current.as_table().is_some_and(Table::is_implicit) != table.implicit
                || found.is_dotted() != table.dotted
                || comments_hash(parent.key(key), Some(current)) != table.applied_comments
            {
                conflicts.push(display_path(&table.path));
                continue;
            }
            parent_table_mut(document.as_table_mut(), &table.path)
                .unwrap()
                .remove(key);
            changed = true;
        }
        Ok(changed)
    }
}

fn validate_path(path: &[String]) -> Result<(), String> {
    if path
        .first()
        .is_none_or(|key| MANAGED.contains(&key.as_str()))
    {
        return Err("SwitchX journal has an invalid overlay path".into());
    }
    Ok(())
}

fn display_path(path: &[String]) -> String {
    path.iter()
        .map(|key| {
            if !key.is_empty()
                && key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
            {
                key.clone()
            } else {
                Value::from(key.as_str()).to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn parent_table<'a>(mut table: &'a dyn TableLike, path: &[String]) -> Option<&'a dyn TableLike> {
    for key in &path[..path.len() - 1] {
        table = table.get(key)?.as_table_like()?;
    }
    Some(table)
}

fn parent_table_mut<'a>(
    mut table: &'a mut dyn TableLike,
    path: &[String],
) -> Option<&'a mut dyn TableLike> {
    for key in &path[..path.len() - 1] {
        table = table.get_mut(key)?.as_table_like_mut()?;
    }
    Some(table)
}

fn blocked_parent(mut table: &dyn TableLike, path: &[String]) -> bool {
    for key in &path[..path.len() - 1] {
        let Some(item) = table.get(key) else {
            return false;
        };
        let Some(next) = item.as_table_like() else {
            return true;
        };
        table = next;
    }
    false
}

fn snapshot(item: &Item) -> Result<String, String> {
    if item.is_none() {
        return Err("invalid Codex overlay value".into());
    }
    let mut document = DocumentMut::new();
    document
        .as_table_mut()
        .insert("value", canonical_item(item));
    Ok(document.to_string())
}

fn canonical_item(item: &Item) -> Item {
    match item {
        Item::Value(value) => Item::Value(canonical_value(value)),
        Item::Table(original) => {
            let mut table = Table::new();
            for (key, item) in original.iter() {
                table.insert(key, canonical_item(item));
            }
            Item::Table(table)
        }
        Item::ArrayOfTables(original) => {
            let mut tables = ArrayOfTables::new();
            for original in original.iter() {
                let Item::Table(table) = canonical_item(&Item::Table(original.clone())) else {
                    unreachable!("table remains a table")
                };
                tables.push(table);
            }
            Item::ArrayOfTables(tables)
        }
        Item::None => Item::None,
    }
}

fn parse_snapshot(text: &str) -> Result<Item, String> {
    let mut document: DocumentMut = text
        .parse()
        .map_err(|_| "invalid Codex overlay journal value")?;
    if document.as_table().len() != 1 {
        return Err("invalid Codex overlay journal value".into());
    }
    document
        .as_table_mut()
        .remove("value")
        .ok_or_else(|| "invalid Codex overlay journal value".into())
}

fn canonical_value(value: &Value) -> Value {
    match value {
        Value::String(value) => Value::from(value.value().as_str()),
        Value::Integer(value) => Value::from(*value.value()),
        Value::Float(value) => Value::from(*value.value()),
        Value::Boolean(value) => Value::from(*value.value()),
        Value::Datetime(value) => Value::Datetime(Formatted::new(*value.value())),
        Value::Array(values) => {
            let mut array = Array::new();
            for value in values.iter() {
                array.push(canonical_value(value));
            }
            Value::Array(array)
        }
        Value::InlineTable(values) => {
            let mut table = InlineTable::new();
            let mut keys = values.iter().map(|(key, _)| key).collect::<Vec<_>>();
            keys.sort_unstable();
            for key in keys {
                table.insert(key, canonical_value(&values[key]));
            }
            Value::InlineTable(table)
        }
    }
}

fn decor_has_comment(decor: &Decor) -> bool {
    [decor.prefix(), decor.suffix()]
        .into_iter()
        .flatten()
        .any(|raw| raw.as_str().is_none_or(|text| !text.trim().is_empty()))
}

fn has_comments(key: Option<&Key>, item: Option<&Item>) -> bool {
    key.is_some_and(|key| {
        decor_has_comment(key.leaf_decor()) || decor_has_comment(key.dotted_decor())
    }) || item.is_some_and(|item| {
        item.as_value()
            .is_some_and(|value| decor_has_comment(value.decor()))
            || item
                .as_table()
                .is_some_and(|table| decor_has_comment(table.decor()))
            || has_inner_comments(item)
    })
}

fn has_inner_comments(item: &Item) -> bool {
    if let Some(array) = item.as_array() {
        return array
            .trailing()
            .as_str()
            .is_none_or(|text| !text.trim().is_empty())
            || array
                .iter()
                .any(|value| has_comments(None, Some(&Item::Value(value.clone()))));
    }
    if let Some(tables) = item.as_array_of_tables() {
        return tables.iter().any(|table| {
            decor_has_comment(table.decor())
                || table
                    .iter()
                    .any(|(key, value)| has_comments(table.key(key), Some(value)))
        });
    }
    if let Some(table) = item.as_inline_table() {
        return table
            .iter()
            .any(|(key, value)| has_comments(table.key(key), Some(&Item::Value(value.clone()))));
    }
    false
}

fn comments_hash(key: Option<&Key>, item: Option<&Item>) -> u64 {
    let mut hash = DefaultHasher::new();
    hash_comments(key, item, &mut hash);
    hash.finish()
}

fn leaf_comments_hash(key: Option<&Key>, item: Option<&Item>) -> u64 {
    let mut hash = DefaultHasher::new();
    hash_comments(key, item, &mut hash);
    if let Some(item) = item {
        hash_inner_comments(item, &mut hash);
    }
    hash.finish()
}

fn hash_comments(key: Option<&Key>, item: Option<&Item>, hash: &mut DefaultHasher) {
    let mut decor = |decor: &Decor| {
        for raw in [decor.prefix(), decor.suffix()] {
            raw.and_then(|raw| raw.as_str())
                .unwrap_or("")
                .trim()
                .hash(hash);
        }
    };
    if let Some(key) = key {
        decor(key.leaf_decor());
        decor(key.dotted_decor());
    }
    if let Some(item) = item {
        if let Some(value) = item.as_value() {
            decor(value.decor());
        } else if let Some(table) = item.as_table() {
            decor(table.decor());
        }
    }
}

fn hash_inner_comments(item: &Item, hash: &mut DefaultHasher) {
    if let Some(table) = item.as_table_like() {
        for (key, item) in table.iter() {
            hash_comments(table.key(key), Some(item), hash);
            hash_inner_comments(item, hash);
        }
    } else if let Some(array) = item.as_array() {
        array.trailing().as_str().unwrap_or("").trim().hash(hash);
        for value in array.iter() {
            let item = Item::Value(value.clone());
            hash_comments(None, Some(&item), hash);
            hash_inner_comments(&item, hash);
        }
    } else if let Some(tables) = item.as_array_of_tables() {
        for table in tables.iter() {
            let item = Item::Table(table.clone());
            hash_comments(None, Some(&item), hash);
            hash_inner_comments(&item, hash);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_leaves_and_empty_added_tables_without_reverting_external_edits() {
        let before: DocumentMut = "model = \"old\"\napproval_policy = \"on-request\" # private\n\n[features]\nhooks = false\n".parse().unwrap();
        let applied: DocumentMut = "model = \"new\"\napproval_policy = \"never\" # private\nmodel_context_window = 1000000\n\n[features]\nhooks = true\nmemories = true\n\n[tui]\nnotifications = true\n".parse().unwrap();
        let overlay = Overlay::between(&before, &applied).unwrap();
        assert!(!serde_json::to_string(&overlay).unwrap().contains("private"));
        let mut current = applied;
        current["features"]["hooks"] = toml_edit::value(false);
        current["features"]["external"] = toml_edit::value(true);
        current["model_context_window"] = toml_edit::value(123456);
        let mut conflicts = Vec::new();
        assert!(overlay.restore(&mut current, &mut conflicts).unwrap());
        assert_eq!(conflicts, ["model_context_window"]);
        assert_eq!(current["approval_policy"].as_str(), Some("on-request"));
        assert!(current.to_string().contains("# private"));
        assert_eq!(current["features"]["external"].as_bool(), Some(true));
        assert!(current["features"].get("memories").is_none());
        assert!(current.as_table().get("tui").is_none());
        assert_eq!(current["model"].as_str(), Some("new"));
        current["model_context_window"] = toml_edit::value(1000000);
        assert!(overlay.restore(&mut current, &mut Vec::new()).unwrap());
        assert!(current.as_table().get("model_context_window").is_none());
    }

    #[test]
    fn rejects_comment_loss_and_preserves_comments_added_to_owned_fields() {
        let before: DocumentMut = "model_context_window = 123 # private\n".parse().unwrap();
        assert!(
            Overlay::between(&before, &DocumentMut::new())
                .unwrap_err()
                .contains("注释")
        );
        let before = DocumentMut::new();
        let applied: DocumentMut = "[tui]\nnotifications = true\n".parse().unwrap();
        let overlay = Overlay::between(&before, &applied).unwrap();
        let mut current: DocumentMut = "[tui]\nnotifications = true # external\n".parse().unwrap();
        let mut conflicts = Vec::new();
        assert!(!overlay.restore(&mut current, &mut conflicts).unwrap());
        assert_eq!(conflicts, ["tui.notifications"]);
        assert!(current.to_string().contains("# external"));
    }

    #[test]
    fn restores_inline_members_and_preserves_external_parent_structure() {
        let before: DocumentMut = "features = { hooks = false } # keep-parent\n"
            .parse()
            .unwrap();
        let applied: DocumentMut = "features = { hooks = true, memories = true } # keep-parent\n"
            .parse()
            .unwrap();
        let overlay = Overlay::between(&before, &applied).unwrap();
        let mut precommit = before.clone();
        assert!(!overlay.restore(&mut precommit, &mut Vec::new()).unwrap());
        let mut current = applied;
        current["features"]["external"] = toml_edit::value(true);
        assert!(overlay.restore(&mut current, &mut Vec::new()).unwrap());
        assert_eq!(current["features"]["hooks"].as_bool(), Some(false));
        assert_eq!(current["features"]["external"].as_bool(), Some(true));
        assert!(current["features"].get("memories").is_none());
        assert!(current.to_string().contains("# keep-parent"));

        let applied: DocumentMut = "[features.nested]\nhooks = true\n".parse().unwrap();
        let overlay = Overlay::between(&DocumentMut::new(), &applied).unwrap();
        let mut current: DocumentMut = "[features]\n[features.nested]\nhooks = true\n"
            .parse()
            .unwrap();
        let mut conflicts = Vec::new();
        assert!(overlay.restore(&mut current, &mut conflicts).unwrap());
        assert_eq!(conflicts, ["features"]);
        assert!(current.to_string().contains("[features]"));
        assert!(!current.to_string().contains("nested"));
    }
}
