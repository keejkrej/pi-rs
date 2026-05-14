use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};

use crate::keys::{KeyId, matches_key};

pub type Keybinding = String;
pub type KeybindingsConfig = BTreeMap<Keybinding, Vec<KeyId>>;

pub const TUI_KEYBINDINGS: &[(&str, &[&str], &str)] = &[
    ("tui.editor.cursorUp", &["up"], "Move cursor up"),
    ("tui.editor.cursorDown", &["down"], "Move cursor down"),
    (
        "tui.editor.cursorLeft",
        &["left", "ctrl+b"],
        "Move cursor left",
    ),
    (
        "tui.editor.cursorRight",
        &["right", "ctrl+f"],
        "Move cursor right",
    ),
    (
        "tui.editor.cursorWordLeft",
        &["alt+left", "ctrl+left", "alt+b"],
        "Move cursor word left",
    ),
    (
        "tui.editor.cursorWordRight",
        &["alt+right", "ctrl+right", "alt+f"],
        "Move cursor word right",
    ),
    (
        "tui.editor.cursorLineStart",
        &["home", "ctrl+a"],
        "Move to line start",
    ),
    (
        "tui.editor.cursorLineEnd",
        &["end", "ctrl+e"],
        "Move to line end",
    ),
    (
        "tui.editor.deleteCharBackward",
        &["backspace"],
        "Delete character backward",
    ),
    (
        "tui.editor.deleteCharForward",
        &["delete", "ctrl+d"],
        "Delete character forward",
    ),
    (
        "tui.editor.deleteWordBackward",
        &["ctrl+w", "alt+backspace"],
        "Delete word backward",
    ),
    (
        "tui.editor.deleteWordForward",
        &["alt+d", "alt+delete"],
        "Delete word forward",
    ),
    (
        "tui.editor.deleteToLineStart",
        &["ctrl+u"],
        "Delete to line start",
    ),
    (
        "tui.editor.deleteToLineEnd",
        &["ctrl+k"],
        "Delete to line end",
    ),
    ("tui.editor.yank", &["ctrl+y"], "Yank"),
    ("tui.editor.yankPop", &["alt+y"], "Yank pop"),
    ("tui.editor.undo", &["ctrl+-"], "Undo"),
    ("tui.input.newLine", &["shift+enter"], "Insert newline"),
    ("tui.input.submit", &["enter"], "Submit input"),
    ("tui.input.tab", &["tab"], "Tab / autocomplete"),
    ("tui.input.copy", &["ctrl+c"], "Copy selection"),
    ("tui.select.up", &["up"], "Move selection up"),
    ("tui.select.down", &["down"], "Move selection down"),
    ("tui.select.pageUp", &["pageUp"], "Selection page up"),
    ("tui.select.pageDown", &["pageDown"], "Selection page down"),
    ("tui.select.confirm", &["enter"], "Confirm selection"),
    (
        "tui.select.cancel",
        &["escape", "ctrl+c"],
        "Cancel selection",
    ),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingDefinition {
    pub default_keys: Vec<KeyId>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingConflict {
    pub key: KeyId,
    pub keybindings: Vec<Keybinding>,
}

pub fn tui_keybindings() -> BTreeMap<Keybinding, KeybindingDefinition> {
    TUI_KEYBINDINGS
        .iter()
        .map(|(id, keys, description)| {
            (
                (*id).to_string(),
                KeybindingDefinition {
                    default_keys: keys.iter().map(|key| (*key).to_string()).collect(),
                    description: Some((*description).to_string()),
                },
            )
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct KeybindingsManager {
    definitions: BTreeMap<Keybinding, KeybindingDefinition>,
    user_bindings: KeybindingsConfig,
    keys_by_id: KeybindingsConfig,
    conflicts: Vec<KeybindingConflict>,
}

impl Default for KeybindingsManager {
    fn default() -> Self {
        Self::new(tui_keybindings(), BTreeMap::new())
    }
}

impl KeybindingsManager {
    pub fn new(
        definitions: BTreeMap<Keybinding, KeybindingDefinition>,
        user_bindings: KeybindingsConfig,
    ) -> Self {
        let mut manager = Self {
            definitions,
            user_bindings,
            keys_by_id: BTreeMap::new(),
            conflicts: Vec::new(),
        };
        manager.rebuild();
        manager
    }

    fn rebuild(&mut self) {
        self.keys_by_id.clear();
        self.conflicts.clear();

        let mut user_claims: BTreeMap<KeyId, BTreeSet<Keybinding>> = BTreeMap::new();
        for (keybinding, keys) in &self.user_bindings {
            if !self.definitions.contains_key(keybinding) {
                continue;
            }
            for key in normalize_keys(keys) {
                user_claims
                    .entry(key)
                    .or_default()
                    .insert(keybinding.clone());
            }
        }
        for (key, keybindings) in user_claims {
            if keybindings.len() > 1 {
                self.conflicts.push(KeybindingConflict {
                    key,
                    keybindings: keybindings.into_iter().collect(),
                });
            }
        }

        for (id, definition) in &self.definitions {
            let keys = self
                .user_bindings
                .get(id)
                .cloned()
                .unwrap_or_else(|| definition.default_keys.clone());
            self.keys_by_id.insert(id.clone(), normalize_keys(&keys));
        }
    }

    pub fn matches(&self, data: &str, keybinding: &str) -> bool {
        self.keys_by_id
            .get(keybinding)
            .is_some_and(|keys| keys.iter().any(|key| matches_key(data, key)))
    }

    pub fn get_keys(&self, keybinding: &str) -> Vec<KeyId> {
        self.keys_by_id.get(keybinding).cloned().unwrap_or_default()
    }

    pub fn get_definition(&self, keybinding: &str) -> Option<&KeybindingDefinition> {
        self.definitions.get(keybinding)
    }

    pub fn get_conflicts(&self) -> Vec<KeybindingConflict> {
        self.conflicts.clone()
    }

    pub fn set_user_bindings(&mut self, user_bindings: KeybindingsConfig) {
        self.user_bindings = user_bindings;
        self.rebuild();
    }

    pub fn get_user_bindings(&self) -> KeybindingsConfig {
        self.user_bindings.clone()
    }

    pub fn get_resolved_bindings(&self) -> KeybindingsConfig {
        self.keys_by_id.clone()
    }
}

fn normalize_keys(keys: &[KeyId]) -> Vec<KeyId> {
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();
    for key in keys {
        if seen.insert(key.clone()) {
            result.push(key.clone());
        }
    }
    result
}

static GLOBAL_KEYBINDINGS: OnceLock<Mutex<KeybindingsManager>> = OnceLock::new();

pub fn set_keybindings(keybindings: KeybindingsManager) {
    let mutex = GLOBAL_KEYBINDINGS.get_or_init(|| Mutex::new(KeybindingsManager::default()));
    *mutex.lock().expect("keybindings mutex poisoned") = keybindings;
}

pub fn get_keybindings() -> KeybindingsManager {
    GLOBAL_KEYBINDINGS
        .get_or_init(|| Mutex::new(KeybindingsManager::default()))
        .lock()
        .expect("keybindings mutex poisoned")
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bindings_match() {
        assert!(KeybindingsManager::default().matches("\u{1b}", "tui.select.cancel"));
    }
}
