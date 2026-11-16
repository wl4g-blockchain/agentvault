// SPDX-License-Identifier: GPL-3.0-or-later
//! Generic environment overlays over a typed Serde configuration tree.
use anyhow::Result;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

pub struct EnvironmentTree;
impl EnvironmentTree {
    /// Applies a deterministic prefix/`__` overlay without a field-path inventory.
    ///
    /// # Errors
    /// Rejects empty path segments, scalar traversal and invalid array indices.
    pub fn merge(mut tree: Value, prefix: &str, source: HashMap<String, String>) -> Result<Value> {
        for (key, value) in source.into_iter().collect::<BTreeMap<_, _>>() {
            if let Some(path) = key.strip_prefix(prefix) {
                Self::apply(&mut tree, &path.split("__").collect::<Vec<_>>(), value)?;
            }
        }
        Ok(tree)
    }

    /// Visits a tree, including arbitrary map keys and sequence elements.
    pub fn visit(value: &Value, visitor: &mut impl FnMut(&Value)) {
        visitor(value);
        match value {
            Value::Object(fields) => {
                for value in fields.values() {
                    Self::visit(value, visitor);
                }
            }
            Value::Array(items) => {
                for value in items {
                    Self::visit(value, visitor);
                }
            }
            _ => {}
        }
    }

    fn apply(node: &mut serde_json::Value, path: &[&str], value: String) -> Result<()> {
        let Some((part, tail)) = path.split_first() else {
            *node = serde_json::Value::String(value);
            return Ok(());
        };
        if part.is_empty() {
            return Err(anyhow::anyhow!(
                "environment configuration path has an empty segment".to_owned(),
            ));
        }
        if node.is_null() {
            *node = if part.parse::<usize>().is_ok() {
                serde_json::Value::Array(Vec::new())
            } else {
                serde_json::json!({})
            };
        }
        if let Some(fields) = node.as_object_mut() {
            let key = fields
                .keys()
                .find(|key| key.eq_ignore_ascii_case(part))
                .cloned()
                .unwrap_or_else(|| part.to_ascii_lowercase());
            Self::apply(fields.entry(key).or_insert(serde_json::Value::Null), tail, value)
        } else if let Some(items) = node.as_array_mut() {
            let index = part
                .parse::<usize>()
                .ok()
                .filter(|index| *index < 1024)
                .ok_or_else(|| {
                    anyhow::anyhow!("environment configuration array index must be between 0 and 1023".to_owned())
                })?;
            if items.len() <= index {
                items.resize(index + 1, serde_json::Value::Null);
            }
            Self::apply(&mut items[index], tail, value)
        } else {
            Err(anyhow::anyhow!(format!(
                "environment configuration path traverses a scalar at {part}"
            )))
        }
    }
}
