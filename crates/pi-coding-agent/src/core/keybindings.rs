//! Port of packages/coding-agent/src/core/keybindings.ts

use std::sync::{Arc, LazyLock, Mutex};

use indexmap::IndexMap;
use serde_json::Value;

pub use pi_tui::keybindings::KeyIdOrList;
pub use pi_tui::{KeyId, Keybinding, KeybindingDefinition, KeybindingDefinitions, KeybindingsConfig};
use pi_tui::{KeybindingsManager as TuiKeybindingsManager, TUI_KEYBINDINGS};

use crate::config::get_agent_dir;
use crate::utils::text::strip_bom;

pub type AppKeybinding = String;

pub fn use_windows_keybindings(platform: Option<&str>, env: Option<&IndexMap<String, String>>) -> bool {
    let p = platform.unwrap_or_else(|| pi_js::env::platform());
    if p == "win32" {
        return true;
    }
    if p != "linux" {
        return false;
    }
    let has = |k: &str| match env {
        Some(map) => map.get(k).is_some_and(|v| !v.is_empty()),
        None => pi_js::env::var(k).is_some_and(|v| !v.is_empty()),
    };
    has("WSL_DISTRO_NAME") || has("WSL_INTEROP")
}

fn def(default_keys: KeyIdOrList, description: &str) -> KeybindingDefinition {
    KeybindingDefinition {
        default_keys,
        description: Some(description.to_owned()),
    }
}

fn one(k: &str) -> KeyIdOrList {
    KeyIdOrList::One(k.to_owned())
}

fn many(keys: &[&str]) -> KeyIdOrList {
    KeyIdOrList::Many(keys.iter().map(|s| (*s).to_owned()).collect())
}

pub fn build_keybindings(platform: &str, env: Option<&IndexMap<String, String>>) -> KeybindingDefinitions {
    let win_kb = use_windows_keybindings(Some(platform), env);
    let mut defs = (&*TUI_KEYBINDINGS).clone();

    if let Some(d) = defs.get_mut("tui.editor.undo") {
        d.default_keys = if platform == "win32" {
            one("ctrl+z")
        } else if win_kb {
            one("alt+z")
        } else {
            one("ctrl+-")
        };
    }
    if let Some(d) = defs.get_mut("tui.altScreen.previousPrompt") {
        d.default_keys = if win_kb {
            one("ctrl+up")
        } else {
            many(&["ctrl+shift+up", "ctrl+up"])
        };
    }
    if let Some(d) = defs.get_mut("tui.altScreen.nextPrompt") {
        d.default_keys = if win_kb {
            one("ctrl+down")
        } else {
            many(&["ctrl+shift+down", "ctrl+down"])
        };
    }
    if let Some(d) = defs.get_mut("tui.altScreen.search") {
        d.default_keys = if win_kb { one("ctrl+f") } else { one("ctrl+shift+f") };
    }

    let app_entries: Vec<(&str, KeyIdOrList, &str)> = vec![
        ("app.interrupt", one("escape"), "Cancel or abort"),
        ("app.clear", one("ctrl+c"), "Clear editor"),
        ("app.exit", one("ctrl+d"), "Exit when editor is empty"),
        (
            "app.suspend",
            if platform == "win32" { many(&[]) } else { one("ctrl+z") },
            "Suspend to background",
        ),
        ("app.thinking.cycle", one("shift+tab"), "Cycle thinking level"),
        ("app.thinking.save", one("ctrl+s"), "Save thinking level"),
        ("app.model.cycleForward", one("ctrl+p"), "Cycle to next model"),
        (
            "app.model.cycleBackward",
            if win_kb { one("alt+p") } else { one("shift+ctrl+p") },
            "Cycle to previous model",
        ),
        ("app.model.select", one("ctrl+l"), "Open model selector"),
        ("app.tools.expand", one("ctrl+o"), "Toggle tool output"),
        ("app.thinking.toggle", one("ctrl+t"), "Toggle thinking blocks"),
        (
            "app.session.toggleNamedFilter",
            one("ctrl+n"),
            "Toggle named session filter",
        ),
        ("app.editor.external", one("ctrl+g"), "Open external editor"),
        (
            "app.message.copy",
            one("ctrl+x"),
            "Copy selection or last assistant message",
        ),
        (
            "app.message.followUp",
            if win_kb { one("ctrl+q") } else { one("alt+enter") },
            "Queue follow-up message",
        ),
        (
            "app.message.dequeue",
            if win_kb { one("alt+q") } else { one("alt+up") },
            "Restore queued messages",
        ),
        (
            "app.clipboard.pasteImage",
            if win_kb { one("alt+v") } else { one("ctrl+v") },
            "Paste files on macOS, images, or text from clipboard",
        ),
        ("app.session.new", many(&[]), "Start a new session"),
        ("app.session.tree", many(&[]), "Open session tree"),
        ("app.session.fork", many(&[]), "Fork current session"),
        ("app.session.resume", many(&[]), "Resume a session"),
        (
            "app.tree.foldOrUp",
            if platform == "darwin" {
                many(&["alt+left", "ctrl+left"])
            } else {
                many(&["ctrl+left", "alt+left"])
            },
            "Fold tree branch or move up",
        ),
        (
            "app.tree.unfoldOrDown",
            if platform == "darwin" {
                many(&["alt+right", "ctrl+right"])
            } else {
                many(&["ctrl+right", "alt+right"])
            },
            "Unfold tree branch or move down",
        ),
        ("app.tree.editLabel", one("shift+l"), "Edit tree label"),
        (
            "app.tree.toggleLabelTimestamp",
            one("shift+t"),
            "Toggle tree label timestamps",
        ),
        ("app.session.togglePath", one("ctrl+p"), "Toggle session path display"),
        ("app.session.toggleSort", one("ctrl+s"), "Toggle session sort mode"),
        ("app.session.rename", one("ctrl+r"), "Rename session"),
        ("app.session.delete", one("ctrl+d"), "Delete session"),
        (
            "app.session.deleteNoninvasive",
            one("ctrl+backspace"),
            "Delete session when query is empty",
        ),
        ("app.models.save", one("ctrl+s"), "Save model selection"),
        ("app.models.enableAll", one("ctrl+a"), "Enable all models"),
        ("app.models.clearAll", one("ctrl+x"), "Clear all models"),
        (
            "app.models.toggleProvider",
            one("ctrl+p"),
            "Toggle all models for provider",
        ),
        ("app.models.reorderUp", one("alt+up"), "Move model up in order"),
        ("app.models.reorderDown", one("alt+down"), "Move model down in order"),
        ("app.tree.filter.default", one("ctrl+d"), "Tree filter: default view"),
        (
            "app.tree.filter.noTools",
            one("ctrl+t"),
            "Tree filter: hide tool results",
        ),
        (
            "app.tree.filter.userOnly",
            one("ctrl+u"),
            "Tree filter: user messages only",
        ),
        (
            "app.tree.filter.labeledOnly",
            one("ctrl+l"),
            "Tree filter: labeled entries only",
        ),
        ("app.tree.filter.all", one("ctrl+a"), "Tree filter: show all entries"),
        (
            "app.tree.filter.cycleForward",
            one("ctrl+o"),
            "Tree filter: cycle forward",
        ),
        (
            "app.tree.filter.cycleBackward",
            one("shift+ctrl+o"),
            "Tree filter: cycle backward",
        ),
    ];

    for (id, keys, desc) in app_entries {
        defs.insert(id.to_owned(), def(keys, desc));
    }
    defs
}

pub static KEYBINDINGS: LazyLock<KeybindingDefinitions> =
    LazyLock::new(|| build_keybindings(pi_js::env::platform(), None));

const KEYBINDING_NAME_MIGRATIONS: &[(&str, &str)] = &[
    ("cursorUp", "tui.editor.cursorUp"),
    ("cursorDown", "tui.editor.cursorDown"),
    ("cursorLeft", "tui.editor.cursorLeft"),
    ("cursorRight", "tui.editor.cursorRight"),
    ("cursorWordLeft", "tui.editor.cursorWordLeft"),
    ("cursorWordRight", "tui.editor.cursorWordRight"),
    ("cursorLineStart", "tui.editor.cursorLineStart"),
    ("cursorLineEnd", "tui.editor.cursorLineEnd"),
    ("jumpForward", "tui.editor.jumpForward"),
    ("jumpBackward", "tui.editor.jumpBackward"),
    ("pageUp", "tui.editor.pageUp"),
    ("pageDown", "tui.editor.pageDown"),
    ("deleteCharBackward", "tui.editor.deleteCharBackward"),
    ("deleteCharForward", "tui.editor.deleteCharForward"),
    ("deleteWordBackward", "tui.editor.deleteWordBackward"),
    ("deleteWordForward", "tui.editor.deleteWordForward"),
    ("deleteToLineStart", "tui.editor.deleteToLineStart"),
    ("deleteToLineEnd", "tui.editor.deleteToLineEnd"),
    ("yank", "tui.editor.yank"),
    ("yankPop", "tui.editor.yankPop"),
    ("undo", "tui.editor.undo"),
    ("newLine", "tui.input.newLine"),
    ("submit", "tui.input.submit"),
    ("tab", "tui.input.tab"),
    ("copy", "tui.input.copy"),
    ("selectUp", "tui.select.up"),
    ("selectDown", "tui.select.down"),
    ("selectPageUp", "tui.select.pageUp"),
    ("selectPageDown", "tui.select.pageDown"),
    ("selectConfirm", "tui.select.confirm"),
    ("selectCancel", "tui.select.cancel"),
    ("interrupt", "app.interrupt"),
    ("clear", "app.clear"),
    ("exit", "app.exit"),
    ("suspend", "app.suspend"),
    ("cycleThinkingLevel", "app.thinking.cycle"),
    ("cycleModelForward", "app.model.cycleForward"),
    ("cycleModelBackward", "app.model.cycleBackward"),
    ("selectModel", "app.model.select"),
    ("expandTools", "app.tools.expand"),
    ("toggleThinking", "app.thinking.toggle"),
    ("toggleSessionNamedFilter", "app.session.toggleNamedFilter"),
    ("externalEditor", "app.editor.external"),
    ("followUp", "app.message.followUp"),
    ("dequeue", "app.message.dequeue"),
    ("pasteImage", "app.clipboard.pasteImage"),
    ("newSession", "app.session.new"),
    ("tree", "app.session.tree"),
    ("fork", "app.session.fork"),
    ("resume", "app.session.resume"),
    ("treeFoldOrUp", "app.tree.foldOrUp"),
    ("treeUnfoldOrDown", "app.tree.unfoldOrDown"),
    ("treeEditLabel", "app.tree.editLabel"),
    ("treeToggleLabelTimestamp", "app.tree.toggleLabelTimestamp"),
    ("toggleSessionPath", "app.session.togglePath"),
    ("toggleSessionSort", "app.session.toggleSort"),
    ("renameSession", "app.session.rename"),
    ("deleteSession", "app.session.delete"),
    ("deleteSessionNoninvasive", "app.session.deleteNoninvasive"),
];

fn lookup_legacy_keybinding_name(key: &str) -> Option<&'static str> {
    KEYBINDING_NAME_MIGRATIONS
        .iter()
        .find(|&&(k, _)| k == key)
        .map(|&(_, v)| v)
}

fn to_keybindings_config(value: &serde_json::Map<String, Value>) -> KeybindingsConfig {
    let mut config = KeybindingsConfig::new();
    for (key, binding) in value {
        match binding {
            Value::String(s) => {
                config.insert(key.clone(), Some(KeyIdOrList::One(s.clone())));
            }
            Value::Array(arr) if arr.iter().all(Value::is_string) => {
                let list = arr.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect();
                config.insert(key.clone(), Some(KeyIdOrList::Many(list)));
            }
            _ => {}
        }
    }
    config
}

fn order_keybindings_config(config: serde_json::Map<String, Value>) -> serde_json::Map<String, Value> {
    let mut ordered = serde_json::Map::new();
    for kb in KEYBINDINGS.keys() {
        if let Some(v) = config.get(kb) {
            ordered.insert(kb.clone(), v.clone());
        }
    }
    let mut extras: Vec<String> = config.keys().filter(|k| !ordered.contains_key(*k)).cloned().collect();
    extras.sort();
    for key in extras {
        if let Some(v) = config.get(&key) {
            ordered.insert(key, v.clone());
        }
    }
    ordered
}

pub fn migrate_keybindings_config(
    raw_config: &serde_json::Map<String, Value>,
) -> (serde_json::Map<String, Value>, bool) {
    let mut config = serde_json::Map::new();
    let mut migrated = false;

    for (key, value) in raw_config {
        let next_key = lookup_legacy_keybinding_name(key).unwrap_or(key.as_str());
        if next_key != key {
            migrated = true;
            if raw_config.contains_key(next_key) {
                continue;
            }
        }
        config.insert(next_key.to_owned(), value.clone());
    }

    (order_keybindings_config(config), migrated)
}

fn load_raw_config(path: &str) -> Option<serde_json::Map<String, Value>> {
    if !pi_js::fs::exists(path) {
        return None;
    }
    let content = pi_js::fs::read_to_string(path).ok()?;
    let parsed = pi_js::json::parse(strip_bom(&content)).ok()?;
    parsed.as_object().cloned()
}

#[derive(Clone)]
pub struct KeybindingsManager {
    inner: Arc<TuiKeybindingsManager>,
    config_path: Arc<Mutex<Option<String>>>,
}

impl KeybindingsManager {
    pub fn new(user_bindings: Option<KeybindingsConfig>, config_path: Option<String>) -> Self {
        let defs = build_keybindings(pi_js::env::platform(), None);
        Self {
            inner: Arc::new(TuiKeybindingsManager::new(defs, user_bindings)),
            config_path: Arc::new(Mutex::new(config_path)),
        }
    }

    pub fn create(agent_dir: Option<&str>) -> Self {
        let owned;
        let dir = match agent_dir {
            Some(d) => d,
            None => {
                owned = get_agent_dir();
                &owned
            }
        };
        let config_path = pi_js::path::join(&[dir, "keybindings.json"]);
        let user_bindings = Self::load_from_file(&config_path);
        Self::new(Some(user_bindings), Some(config_path))
    }

    fn load_from_file(path: &str) -> KeybindingsConfig {
        let Some(raw) = load_raw_config(path) else {
            return KeybindingsConfig::new();
        };
        let (migrated, _) = migrate_keybindings_config(&raw);
        to_keybindings_config(&migrated)
    }

    pub fn reload(&self) {
        let path = self.config_path.lock().ok().and_then(|g| g.clone());
        if let Some(p) = path {
            self.inner.set_user_bindings(Self::load_from_file(&p));
        }
    }

    pub fn matches(&self, data: &str, keybinding: &str) -> bool {
        self.inner.matches(data, keybinding)
    }

    pub fn get_keys(&self, keybinding: &str) -> Vec<KeyId> {
        self.inner.get_keys(keybinding)
    }

    pub fn get_definition(&self, keybinding: &str) -> KeybindingDefinition {
        self.inner.get_definition(keybinding)
    }

    pub fn get_conflicts(&self) -> Vec<pi_tui::KeybindingConflict> {
        self.inner.get_conflicts()
    }

    pub fn set_user_bindings(&self, user_bindings: KeybindingsConfig) {
        self.inner.set_user_bindings(user_bindings);
    }

    pub fn get_user_bindings(&self) -> KeybindingsConfig {
        self.inner.get_user_bindings()
    }

    pub fn get_effective_config(&self) -> KeybindingsConfig {
        self.inner.get_resolved_bindings()
    }

    pub fn get_resolved_bindings(&self) -> KeybindingsConfig {
        self.inner.get_resolved_bindings()
    }
}
