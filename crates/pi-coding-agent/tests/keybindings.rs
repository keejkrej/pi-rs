//! Port of packages/coding-agent/test/keybindings.test.ts

use indexmap::IndexMap;
use pi_coding_agent::core::keybindings::{KEYBINDINGS, KeyIdOrList, build_keybindings, use_windows_keybindings};
use pi_js::env::testing::EnvGuard;

fn one(k: &str) -> KeyIdOrList {
    KeyIdOrList::One(k.to_owned())
}

fn many(keys: &[&str]) -> KeyIdOrList {
    KeyIdOrList::Many(keys.iter().map(|s| (*s).to_owned()).collect())
}

#[test]
fn uses_windows_keybindings_on_native_windows() {
    let empty = IndexMap::new();
    assert!(use_windows_keybindings(Some("win32"), Some(&empty)));
}

#[test]
fn uses_windows_keybindings_in_wsl_without_relying_on_windows_terminal_detection() {
    let wsl_distro = IndexMap::from([("WSL_DISTRO_NAME".to_owned(), "Ubuntu".to_owned())]);
    let wsl_interop = IndexMap::from([("WSL_INTEROP".to_owned(), "/run/WSL/123_interop".to_owned())]);
    assert!(use_windows_keybindings(Some("linux"), Some(&wsl_distro)));
    assert!(use_windows_keybindings(Some("linux"), Some(&wsl_interop)));
}

#[test]
fn does_not_use_windows_keybindings_from_wt_session_alone() {
    let wt = IndexMap::from([("WT_SESSION".to_owned(), "session".to_owned())]);
    assert!(!use_windows_keybindings(Some("linux"), Some(&wt)));
}

#[test]
fn keeps_non_windows_defaults_on_other_platforms() {
    let empty = IndexMap::new();
    assert!(!use_windows_keybindings(Some("linux"), Some(&empty)));
    assert!(!use_windows_keybindings(Some("darwin"), Some(&empty)));
}

#[test]
fn keeps_ctrl_v_as_a_paste_key_on_wsl() {
    let _wsl = EnvGuard::set("WSL_DISTRO_NAME", Some("Ubuntu"));
    let wsl_keybindings = build_keybindings("linux", None);
    assert_eq!(
        wsl_keybindings["app.clipboard.pasteImage"].default_keys,
        many(&["alt+v", "ctrl+v"])
    );
}

#[test]
fn applies_the_detected_defaults_consistently() {
    let windows_keybindings = use_windows_keybindings(None, None);
    let native_windows = pi_js::env::platform() == "win32";

    assert_eq!(
        KEYBINDINGS["app.clipboard.pasteImage"].default_keys,
        if windows_keybindings {
            many(&["alt+v", "ctrl+v"])
        } else {
            one("ctrl+v")
        }
    );
    assert_eq!(
        KEYBINDINGS["tui.altScreen.search"].default_keys,
        one(if windows_keybindings { "ctrl+f" } else { "ctrl+shift+f" })
    );
    assert_eq!(
        KEYBINDINGS["app.message.followUp"].default_keys,
        one(if windows_keybindings { "ctrl+q" } else { "alt+enter" })
    );
    assert_eq!(
        KEYBINDINGS["app.model.cycleBackward"].default_keys,
        one(if windows_keybindings { "alt+p" } else { "shift+ctrl+p" })
    );
    assert_eq!(
        KEYBINDINGS["tui.editor.undo"].default_keys,
        one(if native_windows {
            "ctrl+z"
        } else if windows_keybindings {
            "alt+z"
        } else {
            "ctrl+-"
        })
    );
    assert_eq!(
        KEYBINDINGS["tui.altScreen.previousPrompt"].default_keys,
        if windows_keybindings {
            one("ctrl+up")
        } else {
            many(&["ctrl+shift+up", "ctrl+up"])
        }
    );
    assert_eq!(
        KEYBINDINGS["tui.altScreen.nextPrompt"].default_keys,
        if windows_keybindings {
            one("ctrl+down")
        } else {
            many(&["ctrl+shift+down", "ctrl+down"])
        }
    );
    assert_eq!(
        KEYBINDINGS["app.message.dequeue"].default_keys,
        one(if windows_keybindings { "alt+q" } else { "alt+up" })
    );
}
