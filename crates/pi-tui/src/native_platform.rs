//! Port of packages/tui/src/native-platform.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, LazyLock, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use pi_js::Result;

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
    suffix: String,
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
        todo!("port: NativePlatformHelper::enable_virtual_terminal_input")
    }

    /// PORT: optional TS `isModifierPressed`. `Ok(None)` means the method is not exported.
    /// `Err` is a throw. The boolean result is `Ok(Some(_))`.
    pub fn is_modifier_pressed(&self, name: ModifierKey) -> Result<Option<bool>> {
        todo!("port: NativePlatformHelper::is_modifier_pressed")
    }
}

#[async_trait]
impl NativeClipboard for NativePlatformHelper {
    async fn get_text(&self) -> Result<Option<Option<String>>> {
        todo!("port: NativePlatformHelper::get_text")
    }

    async fn get_image(&self) -> Result<Option<Option<Vec<u8>>>> {
        todo!("port: NativePlatformHelper::get_image")
    }

    async fn get_file_paths(&self) -> Result<Option<Option<Vec<String>>>> {
        todo!("port: NativePlatformHelper::get_file_paths")
    }

    fn has_set_text(&self) -> bool {
        todo!("port: NativePlatformHelper::has_set_text")
    }

    async fn set_text(&self, text: &str) -> Result<()> {
        todo!("port: NativePlatformHelper::set_text")
    }
}

// Cache module loading, not display availability: a disconnected display can recover.
// PORT: key is `{platform}{suffix}` (`""` or `"-x11"`), not a `.node` path. A failed load is stored as `None`.
static HELPERS: LazyLock<Mutex<IndexMap<String, Option<NativePlatformHelper>>>> =
    LazyLock::new(|| Mutex::new(IndexMap::new()));

/// PORT: does not dlopen a `.node` file (§2.6, §9.5). `suffix` defaults to `""` when `None`.
fn load_native_platform_helper(platform: &str, suffix: Option<&str>) -> Option<NativePlatformHelper> {
    todo!("port: load_native_platform_helper")
}

pub fn get_native_platform_helper() -> Option<NativePlatformHelper> {
    todo!("port: get_native_platform_helper")
}

/// Load a clipboard helper without opening the display until a read is requested.
pub fn get_native_clipboard() -> Option<NativePlatformHelper> {
    todo!("port: get_native_clipboard")
}
