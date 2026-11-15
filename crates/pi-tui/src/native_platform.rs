//! Port of packages/tui/src/native-platform.ts

use std::sync::{Arc, LazyLock, Mutex};

use async_trait::async_trait;
use image::ImageEncoder as _;
use image::codecs::png::PngEncoder;
use indexmap::IndexMap;
use pi_js::Result;
use serde::{Deserialize, Serialize};

/// `"shift" | "command" | "control" | "option"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModifierKey {
    #[serde(rename = "shift")]
    Shift,
    #[serde(rename = "command")]
    Command,
    #[serde(rename = "control")]
    Control,
    #[serde(rename = "option")]
    Option,
}

/// Native clipboard surface shared by the platform helpers.
///
/// PORT: `Ok(None)` is JS `undefined`, `Ok(Some(None))` is `null`, `Ok(Some(Some(value)))` is a value,
/// and `Err` is a rejected transfer.
#[async_trait]
pub trait NativeClipboard: Send + Sync {
    /// Undefined means unavailable, null means no text; transfer failures reject.
    async fn get_text(&self) -> Result<Option<Option<String>>>;

    /// Undefined means unavailable, null means no image; transfer failures reject.
    async fn get_image(&self) -> Result<Option<Option<Vec<u8>>>>;

    /// POSIX paths of file URLs on the clipboard. Undefined means unsupported, null means no files.
    ///
    /// Default is a missing method (`undefined` / unsupported).
    async fn get_file_paths(&self) -> Result<Option<Option<Vec<String>>>> {
        Ok(None)
    }

    /// PORT: optional TS `setText`. `false` means this helper did not export it.
    /// Linux uses command-line tools to retain clipboard ownership instead.
    fn has_set_text(&self) -> bool {
        false
    }

    /// Linux uses command-line tools to retain clipboard ownership instead.
    async fn set_text(&self, text: &str) -> Result<()>;
}

/// Platform helper returned by [`get_native_platform_helper`] and [`get_native_clipboard`].
///
/// PORT: TS does not export this name. It is public because those functions return it.
/// Clipboard methods are [`NativeClipboard`]. There is no `.node` module object (§2.6, §9.5);
/// `platform` and `suffix` record which addon would have been loaded (`""` or `"-x11"`).
#[derive(Clone)]
pub struct NativePlatformHelper {
    inner: Arc<NativePlatformHelperInner>,
}

struct NativePlatformHelperInner {
    platform: String,
    #[allow(dead_code)]
    suffix: String,
}

fn current_host_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        std::env::consts::OS
    }
}

fn open_clipboard_for_platform(platform: &str) -> Option<arboard::Clipboard> {
    if platform != current_host_platform() {
        return None;
    }
    arboard::Clipboard::new().ok()
}

fn encode_rgba_png(img: arboard::ImageData<'_>) -> Result<Vec<u8>> {
    let width = u32::try_from(img.width)
        .map_err(|_| pi_js::Error::msg(format!("Clipboard image width out of range: {}", img.width)))?;
    let height = u32::try_from(img.height)
        .map_err(|_| pi_js::Error::msg(format!("Clipboard image height out of range: {}", img.height)))?;
    let mut png_bytes = Vec::new();
    PngEncoder::new(&mut png_bytes)
        .write_image(&img.bytes, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|e| pi_js::Error::msg(e.to_string()))?;
    Ok(png_bytes)
}

impl NativePlatformHelper {
    fn new(platform: String, suffix: String) -> Self {
        Self {
            inner: Arc::new(NativePlatformHelperInner { platform, suffix }),
        }
    }

    /// PORT: optional TS `enableVirtualTerminalInput`. `Ok(None)` means the method is not exported.
    /// `Err` is a throw. The boolean result is `Ok(Some(_))`.
    pub fn enable_virtual_terminal_input(&self) -> Result<Option<bool>> {
        if let Some(f) = testing::OVERRIDE_ENABLE_VT_INPUT.with(|s| s.get()) {
            return f();
        }
        if self.inner.platform != "win32" {
            return Ok(None);
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
            use windows_sys::Win32::System::Console::{
                ENABLE_VIRTUAL_TERMINAL_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
            };
            // SAFETY: Standard Win32 console handle lookup and mode update on STD_INPUT_HANDLE; `mode` is valid stack storage.
            unsafe {
                let handle = GetStdHandle(STD_INPUT_HANDLE);
                if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                    return Ok(Some(false));
                }
                let mut mode = 0u32;
                if GetConsoleMode(handle, &mut mode) == 0 {
                    return Ok(Some(false));
                }
                if (mode & ENABLE_VIRTUAL_TERMINAL_INPUT) != 0 {
                    return Ok(Some(true));
                }
                Ok(Some(SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_INPUT) != 0))
            }
        }
        #[cfg(not(windows))]
        {
            Ok(None)
        }
    }

    /// PORT: optional TS `isModifierPressed`. `Ok(None)` means the method is not exported.
    /// `Err` is a throw. The boolean result is `Ok(Some(_))`.
    pub fn is_modifier_pressed(&self, name: ModifierKey) -> Result<Option<bool>> {
        if let Some(f) = testing::OVERRIDE_IS_MODIFIER_PRESSED.with(|s| s.get()) {
            return f(name);
        }
        if self.inner.platform != "darwin" && self.inner.platform != "win32" {
            return Ok(None);
        }
        if self.inner.platform != current_host_platform() {
            return Ok(None);
        }
        Ok(crate::native_modifiers::query_platform_modifier_pressed(name))
    }
}

#[async_trait]
impl NativeClipboard for NativePlatformHelper {
    async fn get_text(&self) -> Result<Option<Option<String>>> {
        if let Some(f) = testing::OVERRIDE_GET_TEXT.with(|s| s.get()) {
            return f(&self.inner.platform);
        }
        let Some(mut clipboard) = open_clipboard_for_platform(&self.inner.platform) else {
            return Ok(None);
        };
        match clipboard.get_text() {
            Ok(text) => Ok(Some(Some(text))),
            Err(arboard::Error::ContentNotAvailable) => Ok(Some(None)),
            Err(arboard::Error::ClipboardNotSupported) => Ok(None),
            Err(err) => Err(pi_js::Error::msg(err.to_string())),
        }
    }

    async fn get_image(&self) -> Result<Option<Option<Vec<u8>>>> {
        if let Some(f) = testing::OVERRIDE_GET_IMAGE.with(|s| s.get()) {
            return f(&self.inner.platform);
        }
        let Some(mut clipboard) = open_clipboard_for_platform(&self.inner.platform) else {
            return Ok(None);
        };
        match clipboard.get_image() {
            Ok(img) => Ok(Some(Some(encode_rgba_png(img)?))),
            Err(arboard::Error::ContentNotAvailable) => Ok(Some(None)),
            Err(arboard::Error::ClipboardNotSupported) => Ok(None),
            Err(err) => Err(pi_js::Error::msg(err.to_string())),
        }
    }

    async fn get_file_paths(&self) -> Result<Option<Option<Vec<String>>>> {
        if let Some(f) = testing::OVERRIDE_GET_FILE_PATHS.with(|s| s.get()) {
            return f(&self.inner.platform);
        }
        if self.inner.platform != "darwin" {
            return Ok(None);
        }
        let Some(mut clipboard) = open_clipboard_for_platform(&self.inner.platform) else {
            return Ok(None);
        };
        match clipboard.get().file_list() {
            Ok(paths) => {
                if paths.is_empty() {
                    Ok(Some(None))
                } else {
                    let strings: Vec<String> = paths
                        .into_iter()
                        .filter_map(|p| p.into_os_string().into_string().ok())
                        .collect();
                    if strings.is_empty() {
                        Ok(Some(None))
                    } else {
                        Ok(Some(Some(strings)))
                    }
                }
            }
            Err(arboard::Error::ContentNotAvailable) => Ok(Some(None)),
            Err(arboard::Error::ClipboardNotSupported) => Ok(None),
            Err(err) => Err(pi_js::Error::msg(err.to_string())),
        }
    }

    fn has_set_text(&self) -> bool {
        self.inner.platform == "darwin" || self.inner.platform == "win32"
    }

    async fn set_text(&self, text: &str) -> Result<()> {
        if let Some(f) = testing::OVERRIDE_SET_TEXT.with(|s| s.get()) {
            return f(&self.inner.platform, text);
        }
        if !self.has_set_text() {
            return Err(pi_js::Error::msg("setText is not supported on this platform"));
        }
        let Some(mut clipboard) = open_clipboard_for_platform(&self.inner.platform) else {
            return Err(pi_js::Error::msg("Native clipboard is unavailable"));
        };
        clipboard
            .set_text(text.to_string())
            .map_err(|e| pi_js::Error::msg(e.to_string()))
    }
}

// Cache module loading, not display availability: a disconnected display can recover.
// PORT: key is `{platform}{suffix}` (`""` or `"-x11"`), not a `.node` path. A failed load is stored as `None`.
static HELPERS: LazyLock<Mutex<IndexMap<String, Option<NativePlatformHelper>>>> =
    LazyLock::new(|| Mutex::new(IndexMap::new()));

/// PORT: does not dlopen a `.node` file (§2.6, §9.5). `suffix` defaults to `""` when `None`.
fn load_native_platform_helper(platform: &str, suffix: Option<&str>) -> Option<NativePlatformHelper> {
    if !matches!(std::env::consts::ARCH, "x86_64" | "aarch64") {
        return None;
    }
    let suffix_str = suffix.unwrap_or("");
    let key = format!("{platform}{suffix_str}");
    let mut cache = HELPERS.lock().unwrap();
    if let Some(cached) = cache.get(&key) {
        return cached.clone();
    }
    let helper = match (platform, suffix_str) {
        ("darwin", "") | ("win32", "") | ("linux", "-x11") => {
            Some(NativePlatformHelper::new(platform.to_string(), suffix_str.to_string()))
        }
        _ => None,
    };
    cache.insert(key, helper.clone());
    helper
}

pub fn get_native_platform_helper() -> Option<NativePlatformHelper> {
    let platform = pi_js::env::platform();
    if platform != "darwin" && platform != "win32" {
        return None;
    }
    load_native_platform_helper(platform, None)
}

/// Load a clipboard helper without opening the display until a read is requested.
pub fn get_native_clipboard() -> Option<NativePlatformHelper> {
    let platform = pi_js::env::platform();
    if platform != "linux" {
        return get_native_platform_helper();
    }
    if !pi_js::env::var("DISPLAY").is_some_and(|v| !v.is_empty()) {
        return None;
    }
    load_native_platform_helper("linux", Some("-x11"))
}

pub mod testing {
    use super::*;
    use pi_js::seam::{Guard, Slot};

    thread_local! {
        pub(super) static OVERRIDE_GET_TEXT: Slot<dyn Fn(&str) -> Result<Option<Option<String>>> + Send + Sync> = Slot::new();
        pub(super) static OVERRIDE_GET_IMAGE: Slot<dyn Fn(&str) -> Result<Option<Option<Vec<u8>>>> + Send + Sync> = Slot::new();
        pub(super) static OVERRIDE_GET_FILE_PATHS: Slot<dyn Fn(&str) -> Result<Option<Option<Vec<String>>>> + Send + Sync> = Slot::new();
        pub(super) static OVERRIDE_SET_TEXT: Slot<dyn Fn(&str, &str) -> Result<()> + Send + Sync> = Slot::new();
        pub(super) static OVERRIDE_IS_MODIFIER_PRESSED: Slot<dyn Fn(ModifierKey) -> Result<Option<bool>> + Send + Sync> = Slot::new();
        pub(super) static OVERRIDE_ENABLE_VT_INPUT: Slot<dyn Fn() -> Result<Option<bool>> + Send + Sync> = Slot::new();
    }

    pub fn override_get_text(f: impl Fn(&str) -> Result<Option<Option<String>>> + Send + Sync + 'static) -> Guard {
        OVERRIDE_GET_TEXT.with(|s| s.set(Arc::new(f)))
    }

    pub fn override_get_image(f: impl Fn(&str) -> Result<Option<Option<Vec<u8>>>> + Send + Sync + 'static) -> Guard {
        OVERRIDE_GET_IMAGE.with(|s| s.set(Arc::new(f)))
    }

    pub fn override_get_file_paths(
        f: impl Fn(&str) -> Result<Option<Option<Vec<String>>>> + Send + Sync + 'static,
    ) -> Guard {
        OVERRIDE_GET_FILE_PATHS.with(|s| s.set(Arc::new(f)))
    }

    pub fn override_set_text(f: impl Fn(&str, &str) -> Result<()> + Send + Sync + 'static) -> Guard {
        OVERRIDE_SET_TEXT.with(|s| s.set(Arc::new(f)))
    }

    pub fn override_is_modifier_pressed(
        f: impl Fn(ModifierKey) -> Result<Option<bool>> + Send + Sync + 'static,
    ) -> Guard {
        OVERRIDE_IS_MODIFIER_PRESSED.with(|s| s.set(Arc::new(f)))
    }

    pub fn override_enable_virtual_terminal_input(
        f: impl Fn() -> Result<Option<bool>> + Send + Sync + 'static,
    ) -> Guard {
        OVERRIDE_ENABLE_VT_INPUT.with(|s| s.set(Arc::new(f)))
    }

    pub fn same_helper(a: &NativePlatformHelper, b: &NativePlatformHelper) -> bool {
        Arc::ptr_eq(&a.inner, &b.inner)
    }

    pub fn clear_helpers_cache() {
        HELPERS.lock().unwrap().clear();
    }
}
