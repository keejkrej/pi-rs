//! Port of packages/durable/src/harness/json.ts

use pi_chord::JsonValue;
use pi_chord::delta::tracker::{DraftInput, DraftOverlay};

/// Target key when mutating a [`DraftOverlay`] leaf-by-leaf.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonKey<'a> {
    Prop(&'a str),
    Index(i64),
}

/// Assign `value` at `target[key]` leaf by leaf.
///
/// Chord records a container assignment as one full set and only emits an append when a string
/// leaf is reassigned with a longer string, so writing a streaming partial whole would store and
/// publish the complete message on every flush.
pub fn assign_json(target: &DraftOverlay, key: JsonKey<'_>, value: &JsonValue) -> pi_js::Result<()> {
    let current = match key {
        JsonKey::Prop(prop) => target.get(prop)?,
        JsonKey::Index(index) => target.get_index(index)?,
    };

    if let Some(current_overlay) = current.as_overlay() {
        let is_array = current_overlay.is_array()?;
        match value {
            JsonValue::Object(map) if !is_array => {
                for name in current_overlay.own_keys()? {
                    if !map.contains_key(&name) {
                        current_overlay.delete(&name)?;
                    }
                }
                for (name, child) in map {
                    assign_json(current_overlay, JsonKey::Prop(name), child)?;
                }
                return Ok(());
            }
            JsonValue::Array(items) if is_array && current_overlay.len()? <= items.len() => {
                let len = current_overlay.len()?;
                for (index, item) in items.iter().enumerate() {
                    if index < len {
                        assign_json(current_overlay, JsonKey::Index(index as i64), item)?;
                    } else {
                        current_overlay.push(&[DraftInput::Json(item.clone())])?;
                    }
                }
                return Ok(());
            }
            _ => {}
        }
    }

    if current.to_json()?.as_ref() != Some(value) {
        match key {
            JsonKey::Prop(prop) => {
                target.set(prop, value.clone())?;
            }
            JsonKey::Index(index) => {
                target.set_index(index, value.clone())?;
            }
        }
    }
    Ok(())
}
