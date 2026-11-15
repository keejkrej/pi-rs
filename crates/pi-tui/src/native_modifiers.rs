//! Port of packages/tui/src/native-modifiers.ts

pub use crate::native_platform::ModifierKey;
use crate::native_platform::get_native_platform_helper;

pub(crate) fn query_platform_modifier_pressed(key: ModifierKey) -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSEvent, NSEventModifierFlags};
        let flags = NSEvent::modifierFlags_class();
        let flag = match key {
            ModifierKey::Shift => NSEventModifierFlags::Shift,
            ModifierKey::Command => NSEventModifierFlags::Command,
            ModifierKey::Control => NSEventModifierFlags::Control,
            ModifierKey::Option => NSEventModifierFlags::Option,
        };
        return Some(flags.contains(flag));
    }

    #[cfg(target_os = "linux")]
    {
        use x11rb::connection::Connection as _;
        use x11rb::protocol::xproto::{ConnectionExt as _, KeyButMask};
        if !pi_js::env::var("DISPLAY").is_some_and(|v| !v.is_empty()) {
            return None;
        }
        let (conn, screen_num) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots.get(screen_num)?.root;
        let reply = conn.query_pointer(root).ok()?.reply().ok()?;
        let bit = match key {
            ModifierKey::Shift => KeyButMask::SHIFT,
            ModifierKey::Control => KeyButMask::CONTROL,
            ModifierKey::Option => KeyButMask::MOD1,
            ModifierKey::Command => KeyButMask::MOD4,
        };
        return Some((reply.mask & bit) == bit);
    }

    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
        };
        // SAFETY: `GetAsyncKeyState` only inspects global virtual-key state for an integer key code and holds no pointers.
        let pressed = unsafe {
            match key {
                ModifierKey::Shift => GetAsyncKeyState(i32::from(VK_SHIFT)) < 0,
                ModifierKey::Control => GetAsyncKeyState(i32::from(VK_CONTROL)) < 0,
                ModifierKey::Option => GetAsyncKeyState(i32::from(VK_MENU)) < 0,
                ModifierKey::Command => {
                    GetAsyncKeyState(i32::from(VK_LWIN)) < 0 || GetAsyncKeyState(i32::from(VK_RWIN)) < 0
                }
            }
        };
        return Some(pressed);
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = key;
        None
    }
}

pub fn is_native_modifier_pressed(key: ModifierKey) -> bool {
    let Some(helper) = get_native_platform_helper() else {
        return false;
    };
    matches!(helper.is_modifier_pressed(key), Ok(Some(true)))
}
