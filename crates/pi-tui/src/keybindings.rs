//! Port of packages/tui/src/keybindings.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, LazyLock, Mutex};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::keys::KeyId;

/// Global keybinding registry.
///
/// Downstream packages can add keybindings via declaration merging.
///
/// PORT: Rust has no declaration merging. [`Keybinding`] is the id string.
/// This package's ids are the keys of [`TUI_KEYBINDINGS`], in that order.
#[derive(Clone, Copy, Debug, Default)]
pub struct Keybindings;

/// `keyof Keybindings`.
///
/// PORT: open string, not a closed enum, because downstream packages add ids.
pub type Keybinding = String;

/// `KeyId | KeyId[]`. A single id and a one-element list stay distinct.
/// Empty `defaultKeys: []` is [`KeyIdOrList::Many`] with no elements.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum KeyIdOrList {
    One(KeyId),
    Many(Vec<KeyId>),
}

/// Field order is `defaultKeys`, then `description`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeybindingDefinition {
    pub default_keys: KeyIdOrList,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

pub type KeybindingDefinitions = IndexMap<String, KeybindingDefinition>;

/// `Record<string, KeyId | KeyId[] | undefined>`.
///
/// `None` is an explicit `undefined` value. A missing key is also `undefined` on lookup.
/// `Object.entries` order is this map's order.
pub type KeybindingsConfig = IndexMap<String, Option<KeyIdOrList>>;

/// Field order is `key`, then `keybindings`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeybindingConflict {
    pub key: KeyId,
    pub keybindings: Vec<String>,
}

fn definition(default_keys: KeyIdOrList, description: &str) -> KeybindingDefinition {
    KeybindingDefinition {
        default_keys,
        description: Some(description.to_string()),
    }
}

fn one(key: &str) -> KeyIdOrList {
    KeyIdOrList::One(key.to_string())
}

fn many(keys: &[&str]) -> KeyIdOrList {
    KeyIdOrList::Many(keys.iter().copied().map(str::to_string).collect())
}

fn tui_keybindings() -> KeybindingDefinitions {
    let mut m = IndexMap::new();
    let _ = m.insert(
        "tui.editor.cursorUp".to_string(),
        definition(one("up"), "Move cursor up"),
    );
    let _ = m.insert(
        "tui.editor.cursorDown".to_string(),
        definition(one("down"), "Move cursor down"),
    );
    let _ = m.insert(
        "tui.editor.historyPrevious".to_string(),
        definition(many(&[]), "Select previous prompt history entry"),
    );
    let _ = m.insert(
        "tui.editor.historyNext".to_string(),
        definition(many(&[]), "Select next prompt history entry"),
    );
    let _ = m.insert(
        "tui.editor.cursorLeft".to_string(),
        definition(many(&["left", "ctrl+b"]), "Move cursor left"),
    );
    let _ = m.insert(
        "tui.editor.cursorRight".to_string(),
        definition(many(&["right", "ctrl+f"]), "Move cursor right"),
    );
    let _ = m.insert(
        "tui.editor.cursorWordLeft".to_string(),
        definition(many(&["alt+left", "ctrl+left", "alt+b"]), "Move cursor word left"),
    );
    let _ = m.insert(
        "tui.editor.cursorWordRight".to_string(),
        definition(many(&["alt+right", "ctrl+right", "alt+f"]), "Move cursor word right"),
    );
    let _ = m.insert(
        "tui.editor.cursorLineStart".to_string(),
        definition(many(&["home", "ctrl+home", "ctrl+a"]), "Move to line start"),
    );
    let _ = m.insert(
        "tui.editor.cursorLineEnd".to_string(),
        definition(many(&["end", "ctrl+end", "ctrl+e"]), "Move to line end"),
    );
    let _ = m.insert(
        "tui.editor.jumpForward".to_string(),
        definition(one("ctrl+]"), "Jump forward to character"),
    );
    let _ = m.insert(
        "tui.editor.jumpBackward".to_string(),
        definition(one("ctrl+alt+]"), "Jump backward to character"),
    );
    let _ = m.insert(
        "tui.editor.pageUp".to_string(),
        definition(many(&["pageUp", "ctrl+pageUp"]), "Page up"),
    );
    let _ = m.insert(
        "tui.editor.pageDown".to_string(),
        definition(many(&["pageDown", "ctrl+pageDown"]), "Page down"),
    );
    let _ = m.insert(
        "tui.editor.deleteCharBackward".to_string(),
        definition(one("backspace"), "Delete character backward"),
    );
    let _ = m.insert(
        "tui.editor.deleteCharForward".to_string(),
        definition(many(&["delete", "ctrl+d"]), "Delete character forward"),
    );
    let _ = m.insert(
        "tui.editor.deleteWordBackward".to_string(),
        definition(many(&["ctrl+w", "alt+backspace"]), "Delete word backward"),
    );
    let _ = m.insert(
        "tui.editor.deleteWordForward".to_string(),
        definition(many(&["alt+d", "alt+delete"]), "Delete word forward"),
    );
    let _ = m.insert(
        "tui.editor.deleteToLineStart".to_string(),
        definition(one("ctrl+u"), "Delete to line start"),
    );
    let _ = m.insert(
        "tui.editor.deleteToLineEnd".to_string(),
        definition(one("ctrl+k"), "Delete to line end"),
    );
    let _ = m.insert("tui.editor.yank".to_string(), definition(one("ctrl+y"), "Yank"));
    let _ = m.insert("tui.editor.yankPop".to_string(), definition(one("alt+y"), "Yank pop"));
    let _ = m.insert("tui.editor.undo".to_string(), definition(one("ctrl+-"), "Undo"));
    let _ = m.insert(
        "tui.input.newLine".to_string(),
        definition(many(&["shift+enter", "ctrl+j"]), "Insert newline"),
    );
    let _ = m.insert("tui.input.submit".to_string(), definition(one("enter"), "Submit input"));
    let _ = m.insert(
        "tui.input.tab".to_string(),
        definition(one("tab"), "Tab / autocomplete"),
    );
    let _ = m.insert(
        "tui.input.copy".to_string(),
        definition(one("ctrl+c"), "Copy selection"),
    );
    let _ = m.insert("tui.select.up".to_string(), definition(one("up"), "Move selection up"));
    let _ = m.insert(
        "tui.select.down".to_string(),
        definition(one("down"), "Move selection down"),
    );
    let _ = m.insert(
        "tui.select.pageUp".to_string(),
        definition(one("pageUp"), "Selection page up"),
    );
    let _ = m.insert(
        "tui.select.pageDown".to_string(),
        definition(one("pageDown"), "Selection page down"),
    );
    let _ = m.insert(
        "tui.select.confirm".to_string(),
        definition(one("enter"), "Confirm selection"),
    );
    let _ = m.insert(
        "tui.select.cancel".to_string(),
        definition(many(&["escape", "ctrl+c"]), "Cancel selection"),
    );
    // These intentionally shadow the unmodified editor bindings in fullscreen mode.
    let _ = m.insert(
        "tui.altScreen.pageUp".to_string(),
        definition(one("pageUp"), "Scroll viewport up one page"),
    );
    let _ = m.insert(
        "tui.altScreen.pageDown".to_string(),
        definition(one("pageDown"), "Scroll viewport down one page"),
    );
    let _ = m.insert(
        "tui.altScreen.halfPageUp".to_string(),
        definition(many(&[]), "Scroll viewport up half a page"),
    );
    let _ = m.insert(
        "tui.altScreen.halfPageDown".to_string(),
        definition(many(&[]), "Scroll viewport down half a page"),
    );
    let _ = m.insert(
        "tui.altScreen.lineUp".to_string(),
        definition(many(&[]), "Scroll viewport up one line"),
    );
    let _ = m.insert(
        "tui.altScreen.lineDown".to_string(),
        definition(many(&[]), "Scroll viewport down one line"),
    );
    let _ = m.insert(
        "tui.altScreen.previousPrompt".to_string(),
        definition(many(&["ctrl+shift+up", "ctrl+up"]), "Jump to previous semantic prompt"),
    );
    let _ = m.insert(
        "tui.altScreen.nextPrompt".to_string(),
        definition(many(&["ctrl+shift+down", "ctrl+down"]), "Jump to next semantic prompt"),
    );
    let _ = m.insert(
        "tui.altScreen.search".to_string(),
        definition(one("ctrl+shift+f"), "Search the primary scroll view"),
    );
    let _ = m.insert(
        "tui.altScreen.searchNext".to_string(),
        definition(many(&["enter", "ctrl+g"]), "Select the next search match"),
    );
    let _ = m.insert(
        "tui.altScreen.searchPrevious".to_string(),
        definition(
            many(&["shift+enter", "ctrl+shift+g"]),
            "Select the previous search match",
        ),
    );
    let _ = m.insert(
        "tui.altScreen.searchClose".to_string(),
        definition(one("escape"), "Close transcript search"),
    );
    let _ = m.insert(
        "tui.altScreen.top".to_string(),
        definition(one("home"), "Scroll viewport to top"),
    );
    let _ = m.insert(
        "tui.altScreen.bottom".to_string(),
        definition(one("end"), "Scroll viewport to bottom"),
    );
    m
}

/// Clone with `(&*TUI_KEYBINDINGS).clone()`. Key order matches the TS object literal.
pub static TUI_KEYBINDINGS: LazyLock<KeybindingDefinitions> = LazyLock::new(tui_keybindings);

fn normalize_keys(keys: Option<&KeyIdOrList>) -> Vec<KeyId> {
    todo!("port: normalize_keys")
}

struct KeybindingsManagerState {
    user_bindings: KeybindingsConfig,
    keys_by_id: IndexMap<Keybinding, Vec<KeyId>>,
    conflicts: Vec<KeybindingConflict>,
}

struct KeybindingsManagerInner {
    definitions: KeybindingDefinitions,
    state: Mutex<KeybindingsManagerState>,
}

/// PORT: shared mutable registry. Methods take `&self` and lock [`KeybindingsManagerInner`].
/// Coding-agent subclasses this in TS; that subclass is composition over this handle, not inheritance.
#[derive(Clone)]
pub struct KeybindingsManager {
    inner: Arc<KeybindingsManagerInner>,
}

impl KeybindingsManager {
    /// `user_bindings` `None` is the TS default `{}`.
    pub fn new(definitions: KeybindingDefinitions, user_bindings: Option<KeybindingsConfig>) -> Self {
        todo!("port: KeybindingsManager::new")
    }

    fn rebuild(&self) {
        todo!("port: KeybindingsManager::rebuild")
    }

    pub fn matches(&self, data: &str, keybinding: &str) -> bool {
        todo!("port: KeybindingsManager::matches")
    }

    pub fn get_keys(&self, keybinding: &str) -> Vec<KeyId> {
        todo!("port: KeybindingsManager::get_keys")
    }

    /// TS returns the stored definition (typed as always present). Owned clone, not a live borrow.
    pub fn get_definition(&self, keybinding: &str) -> KeybindingDefinition {
        todo!("port: KeybindingsManager::get_definition")
    }

    pub fn get_conflicts(&self) -> Vec<KeybindingConflict> {
        todo!("port: KeybindingsManager::get_conflicts")
    }

    pub fn set_user_bindings(&self, user_bindings: KeybindingsConfig) {
        todo!("port: KeybindingsManager::set_user_bindings")
    }

    pub fn get_user_bindings(&self) -> KeybindingsConfig {
        todo!("port: KeybindingsManager::get_user_bindings")
    }

    pub fn get_resolved_bindings(&self) -> KeybindingsConfig {
        todo!("port: KeybindingsManager::get_resolved_bindings")
    }
}

static GLOBAL_KEYBINDINGS: LazyLock<Mutex<Option<KeybindingsManager>>> = LazyLock::new(|| Mutex::new(None));

pub fn set_keybindings(keybindings: KeybindingsManager) {
    todo!("port: set_keybindings")
}

pub fn get_keybindings() -> KeybindingsManager {
    todo!("port: get_keybindings")
}
