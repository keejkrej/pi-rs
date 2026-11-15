//! Port of packages/tui/test/native-platform.test.ts

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_js::env;
use pi_js::env::testing::{EnvGuard, PlatformGuard};
use pi_tui::native_modifiers::{ModifierKey, is_native_modifier_pressed};
use pi_tui::native_platform::testing::{
    clear_helpers_cache, override_get_image, override_get_text, override_set_text, same_helper,
};
use pi_tui::native_platform::{NativeClipboard, get_native_clipboard, get_native_platform_helper};
use serial_test::serial;

struct HelperCacheGuard;

impl Drop for HelperCacheGuard {
    fn drop(&mut self) {
        clear_helpers_cache();
    }
}

/// TS test "Linux ARM64 prebuild supports 64 KB system pages" verified the ELF header of the
/// compiled Node `.node` N-API native addon. In `pi-rs` (PORTING.md §2.6 / §9.5), native platform
/// helpers and modifiers use direct Rust FFI (`objc2-app-kit`, `x11rb`, `windows-sys`, `arboard`,
/// `image`) compiled directly into the binary with no `.node` dynamic addon prebuilds.
/// This test exercises the direct Rust FFI modifier query safely in headless CI.
#[test]
#[serial]
fn direct_rust_ffi_modifier_query_works_without_prebuild_binaries() {
    let _ = is_native_modifier_pressed(ModifierKey::Shift);
    let _ = is_native_modifier_pressed(ModifierKey::Command);
    let _ = is_native_modifier_pressed(ModifierKey::Control);
    let _ = is_native_modifier_pressed(ModifierKey::Option);
}

#[tokio::test]
#[serial]
async fn writes_windows_clipboard_text_through_the_native_helper_without_command_fallbacks() {
    let _cache = HelperCacheGuard;
    clear_helpers_cache();

    // Deterministic seam verification mirroring the TS clipboard roundtrip on Windows.
    {
        let _platform = PlatformGuard::set("win32");
        let stored = Arc::new(Mutex::new(String::new()));
        let stored_set = Arc::clone(&stored);
        let stored_get = Arc::clone(&stored);
        let _set = override_set_text(move |_platform, text| {
            *stored_set.lock().unwrap() = text.to_string();
            Ok(())
        });
        let _get = override_get_text(move |_platform| {
            let s = stored_get.lock().unwrap().clone();
            Ok(Some(Some(s)))
        });
        let _img = override_get_image(|_platform| Ok(Some(None)));

        let clipboard = get_native_clipboard().expect("clipboard helper on win32");
        assert!(clipboard.has_set_text());
        for text in ["clipboard café 日本語", "", "second write"] {
            clipboard.set_text(text).await.unwrap();
            assert_eq!(clipboard.get_text().await.unwrap(), Some(Some(text.to_string())));
            assert_eq!(clipboard.get_image().await.unwrap(), Some(None));
        }
    }

    // Opt-in live Windows desktop test matching TS `{ skip: process.platform !== "win32" || PI_TEST_NATIVE_CLIPBOARD !== "1" }`.
    if cfg!(target_os = "windows") && env::var("PI_TEST_NATIVE_CLIPBOARD").as_deref() == Some("1") {
        clear_helpers_cache();
        let clipboard = get_native_clipboard().expect("live win32 native clipboard");
        assert!(clipboard.has_set_text());
        for text in ["clipboard café 日本語", "", "second write"] {
            clipboard.set_text(text).await.unwrap();
            assert_eq!(clipboard.get_text().await.unwrap(), Some(Some(text.to_string())));
            assert_eq!(clipboard.get_image().await.unwrap(), Some(None));
        }
    }
}

#[test]
#[serial]
fn uses_the_native_platform_helper_directly_as_the_clipboard_api() {
    let _cache = HelperCacheGuard;
    for platform in ["darwin", "win32"] {
        clear_helpers_cache();
        let _platform = PlatformGuard::set(platform);
        let clipboard = get_native_clipboard().expect("clipboard helper");
        assert!(clipboard.has_set_text());
        let helper = get_native_platform_helper().expect("platform helper");
        assert!(same_helper(&clipboard, &helper));
        let clipboard_again = get_native_clipboard().expect("cached clipboard helper");
        assert!(same_helper(&clipboard, &clipboard_again));
    }
}

#[tokio::test]
#[serial]
async fn linux_loads_x11_lazily_and_rechecks_display() {
    let _cache = HelperCacheGuard;
    clear_helpers_cache();
    let _platform = PlatformGuard::set("linux");

    let _wayland_only = EnvGuard::set_many(&[("DISPLAY", None), ("WAYLAND_DISPLAY", Some("wayland-0"))]);
    assert!(get_native_clipboard().is_none()); // Wayland paste uses wl-paste.

    let available = Arc::new(AtomicBool::new(false));
    let get_text_calls = Arc::new(AtomicUsize::new(0));

    let available_clone = Arc::clone(&available);
    let calls_clone = Arc::clone(&get_text_calls);
    let _get_text = override_get_text(move |_platform| {
        calls_clone.fetch_add(1, Ordering::SeqCst);
        Ok(if available_clone.load(Ordering::SeqCst) {
            Some(Some("X11".to_string()))
        } else {
            None
        })
    });
    let _get_image = override_get_image(|_platform| Ok(Some(None)));

    let clipboard = {
        let _display0 = EnvGuard::set("DISPLAY", Some(":0"));
        let clipboard = get_native_clipboard().expect("clipboard with DISPLAY=:0");
        assert_eq!(get_text_calls.load(Ordering::SeqCst), 0);
        assert_eq!(clipboard.get_text().await.unwrap(), None);
        available.store(true, Ordering::SeqCst);
        assert_eq!(clipboard.get_text().await.unwrap(), Some(Some("X11".to_string())));
        assert_eq!(clipboard.get_image().await.unwrap(), Some(None));
        clipboard
    };

    {
        let _no_display = EnvGuard::set("DISPLAY", None);
        assert!(get_native_clipboard().is_none());
    }

    {
        let _display1 = EnvGuard::set("DISPLAY", Some(":1"));
        let clipboard2 = get_native_clipboard().expect("clipboard with DISPLAY=:1");
        assert!(same_helper(&clipboard, &clipboard2));
    }
}
