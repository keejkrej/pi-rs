//! Port of packages/coding-agent/src/core/resource-loader.ts
//!
//! Includes hook traits (`PromptLoaderHook`, `ThemeLoaderHook`, `ExtensionHostHook`) with
//! default implementations backed by `prompt_templates`, `modes::interactive::theme::theme`,
//! and `extensions`.

use std::sync::{Arc, Mutex, PoisonError};

use indexmap::{IndexMap, IndexSet};
use pi_js::console::error as console_error;
use pi_js::fs::{exists, read_to_string, readdir_with_file_types, stat};
use pi_js::path::{basename, dirname, join, resolve};
use pi_js::{BoxFuture, Error, Result};
use pi_tui::colors::{Color, ParseColorValue, TerminalColorMode, background_ansi, foreground_ansi, parse_color};
use pi_tui::terminal_image::{TerminalCapabilities, detect_capabilities, get_terminal_color_mode};
use serde_json::Value;

pub use crate::core::diagnostics::{ResourceCollision, ResourceDiagnostic, ResourceDiagnosticType, ResourceType};
pub use crate::core::prompt_templates::{
    LoadPromptTemplatesOptions, LoadPromptTemplatesResult, PromptTemplate, load_prompt_templates,
};

use crate::config::{APP_NAME, CONFIG_DIR_NAME};
use crate::core::event_bus::{EventBus, create_event_bus};
use crate::core::footer_data_provider::find_git_paths;
use crate::core::package_manager::{DefaultPackageManager, PackageInstallFailure, PathMetadata, ResolvedResource};
use crate::core::settings_manager::SettingsManager;
use crate::core::skills::{LoadSkillsOptions, Skill, load_skills};
use crate::core::source_info::{
    BUILTIN_PATH_PREFIX, SourceInfo, SourceOrigin, SourceScope, SyntheticSourceOptions, create_source_info,
    create_synthetic_source_info, get_resource_path_key, get_synthetic_path_source, is_synthetic_path,
};
use crate::core::timings::{TimingNamespaceKind, reset_timings};
use crate::utils::paths::{
    PathInputOptions, canonicalize_path, is_local_path, is_path_within, paths_equal, resolve_path,
};
use crate::utils::text::strip_bom;
use crate::vendor::chalk::yellow;

// ============================================================================
// Prompt template loader hook + seam types matching unowned TS modules (theme, extensions)
// ============================================================================

/// Hook for loading prompt templates from explicit paths or default directories.
pub trait PromptLoaderHook: Send + Sync {
    fn load_prompt_templates(
        &self,
        cwd: &str,
        agent_dir: &str,
        prompt_paths: &[String],
        include_defaults: bool,
    ) -> LoadPromptTemplatesResult;
}

/// Default prompt-template loader backed by [`crate::core::prompt_templates::load_prompt_templates`].
#[derive(Clone, Default)]
pub struct BasicPromptLoader;

impl PromptLoaderHook for BasicPromptLoader {
    fn load_prompt_templates(
        &self,
        cwd: &str,
        agent_dir: &str,
        prompt_paths: &[String],
        include_defaults: bool,
    ) -> LoadPromptTemplatesResult {
        load_prompt_templates(LoadPromptTemplatesOptions {
            cwd: cwd.to_owned(),
            agent_dir: agent_dir.to_owned(),
            prompt_paths: prompt_paths.to_vec(),
            include_defaults,
        })
    }
}

/// Loaded TUI theme instance bridged to `crate::modes::interactive::theme::theme::Theme`.
#[derive(Clone)]
pub struct Theme {
    pub name: Option<String>,
    pub source_path: Option<String>,
    pub source_info: Option<SourceInfo>,
    mode: TerminalColorMode,
    fg_ansi: IndexMap<String, String>,
    bg_ansi: IndexMap<String, String>,
    tui_theme: Option<crate::modes::interactive::theme::theme::Theme>,
}

impl std::fmt::Debug for Theme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Theme")
            .field("name", &self.name)
            .field("source_path", &self.source_path)
            .field("source_info", &self.source_info)
            .field("mode", &self.mode)
            .field("fg_ansi", &self.fg_ansi)
            .field("bg_ansi", &self.bg_ansi)
            .field("has_tui_theme", &self.tui_theme.is_some())
            .finish()
    }
}

impl PartialEq for Theme {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.source_path == other.source_path
            && self.source_info == other.source_info
            && self.mode == other.mode
            && self.fg_ansi == other.fg_ansi
            && self.bg_ansi == other.bg_ansi
    }
}

impl Theme {
    pub fn from_tui_theme(tui_theme: crate::modes::interactive::theme::theme::Theme) -> Self {
        use crate::modes::interactive::theme::theme::{ThemeBg, ThemeColor};
        let mut fg_ansi = IndexMap::new();
        for &tok in ThemeColor::all() {
            let ansi = tui_theme.get_fg_ansi(tok);
            fg_ansi.insert(
                tok.as_str().to_owned(),
                if ansi == "\x1b[39m" { String::new() } else { ansi },
            );
        }
        let mut bg_ansi = IndexMap::new();
        for &tok in ThemeBg::all() {
            let ansi = tui_theme.get_bg_ansi(tok);
            bg_ansi.insert(
                tok.as_str().to_owned(),
                if ansi == "\x1b[49m" { String::new() } else { ansi },
            );
        }
        Self {
            name: tui_theme.name().map(str::to_owned),
            source_path: tui_theme.source_path().map(str::to_owned),
            source_info: tui_theme.source_info().cloned(),
            mode: tui_theme.get_color_mode(),
            fg_ansi,
            bg_ansi,
            tui_theme: Some(tui_theme),
        }
    }

    pub fn to_tui_theme(&self) -> Option<crate::modes::interactive::theme::theme::Theme> {
        self.tui_theme.clone().or_else(|| {
            self.source_path.as_deref().and_then(|path| {
                crate::modes::interactive::theme::theme::load_theme_from_path(path, Some(self.mode)).ok()
            })
        })
    }

    pub fn from_json_str(content: &str, mode: TerminalColorMode, source_path: Option<String>) -> Result<Self> {
        let value: Value = serde_json::from_str(strip_bom(content))
            .map_err(|err| Error::msg(format!("Failed to parse theme JSON: {err}")))?;
        let obj = value
            .as_object()
            .ok_or_else(|| Error::msg("Theme JSON must be an object"))?;

        let name = obj.get("name").and_then(Value::as_str).map(str::to_owned);
        let vars = obj.get("vars").and_then(Value::as_object).cloned().unwrap_or_default();
        let colors = obj
            .get("colors")
            .and_then(Value::as_object)
            .ok_or_else(|| Error::msg("Theme JSON missing required \"colors\" object"))?;

        let mut fg_ansi = IndexMap::new();
        let mut bg_ansi = IndexMap::new();

        for (token, raw_val) in colors {
            let resolved = resolve_theme_color_value(raw_val, &vars, 0)?;
            if let Some(color) = resolved {
                fg_ansi.insert(token.clone(), foreground_ansi(color, mode));
                bg_ansi.insert(token.clone(), background_ansi(color, mode));
            } else {
                fg_ansi.insert(token.clone(), String::new());
                bg_ansi.insert(token.clone(), String::new());
            }
        }

        Ok(Self {
            name,
            source_path,
            source_info: None,
            mode,
            fg_ansi,
            bg_ansi,
            tui_theme: None,
        })
    }

    pub fn mode(&self) -> TerminalColorMode {
        self.mode
    }

    pub fn fg(&self, token: &str, text: &str) -> String {
        match self.fg_ansi.get(token) {
            Some(ansi) if !ansi.is_empty() => format!("{ansi}{text}\x1b[39m"),
            _ => text.to_owned(),
        }
    }

    pub fn bg(&self, token: &str, text: &str) -> String {
        match self.bg_ansi.get(token) {
            Some(ansi) if !ansi.is_empty() => format!("{ansi}{text}\x1b[49m"),
            _ => text.to_owned(),
        }
    }
}

fn resolve_theme_color_value(
    value: &Value,
    vars: &serde_json::Map<String, Value>,
    depth: usize,
) -> Result<Option<Color>> {
    if depth > 16 {
        return Err(Error::msg("Circular theme variable reference"));
    }
    match value {
        Value::String(s) => {
            if s.is_empty() {
                return Ok(None);
            }
            let stripped = s.strip_prefix('$');
            if let Some(target) = vars.get(stripped.unwrap_or(s)) {
                return resolve_theme_color_value(target, vars, depth + 1);
            }
            if stripped.is_some() {
                return Err(Error::msg(format!("Unknown theme variable: {s}")));
            }
            parse_color(ParseColorValue::Text(s)).map(Some)
        }
        Value::Number(n) => {
            let idx = n.as_f64().ok_or_else(|| Error::msg("Invalid numeric theme color"))?;
            parse_color(ParseColorValue::Number(idx)).map(Some)
        }
        _ => Err(Error::msg("Theme color value must be a string or number")),
    }
}

/// Hook for loading a theme from disk.
pub trait ThemeLoaderHook: Send + Sync {
    fn load_theme_from_path(&self, theme_path: &str, color_mode: TerminalColorMode) -> Result<Theme>;
}

#[derive(Clone, Default)]
pub struct BasicThemeLoader;

impl ThemeLoaderHook for BasicThemeLoader {
    fn load_theme_from_path(&self, theme_path: &str, color_mode: TerminalColorMode) -> Result<Theme> {
        if let Ok(tui_theme) =
            crate::modes::interactive::theme::theme::load_theme_from_path(theme_path, Some(color_mode))
        {
            return Ok(Theme::from_tui_theme(tui_theme));
        }
        let content = read_to_string(theme_path)?;
        Theme::from_json_str(&content, color_mode, Some(theme_path.to_owned()))
    }
}

/// Minimal tool registration metadata tracked for extension conflict / replacement checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisteredTool {
    pub name: String,
    pub description: String,
    pub source_info: SourceInfo,
}

impl From<&crate::core::extensions::types::RegisteredTool> for RegisteredTool {
    fn from(tool: &crate::core::extensions::types::RegisteredTool) -> Self {
        Self {
            name: tool.definition.name.clone(),
            description: tool.definition.description.clone(),
            source_info: tool.source_info.clone(),
        }
    }
}

/// Minimal slash command registration metadata tracked for extension conflict / replacement checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisteredCommand {
    pub name: String,
    pub description: Option<String>,
    pub source_info: SourceInfo,
}

impl From<&crate::core::extensions::types::RegisteredCommand> for RegisteredCommand {
    fn from(cmd: &crate::core::extensions::types::RegisteredCommand) -> Self {
        Self {
            name: cmd.name.clone(),
            description: cmd.description.clone(),
            source_info: cmd.source_info.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionFlagType {
    Boolean,
    String,
}

impl From<crate::core::extensions::types::FlagType> for ExtensionFlagType {
    fn from(flag_type: crate::core::extensions::types::FlagType) -> Self {
        match flag_type {
            crate::core::extensions::types::FlagType::Boolean => Self::Boolean,
            crate::core::extensions::types::FlagType::String => Self::String,
        }
    }
}

impl From<ExtensionFlagType> for crate::core::extensions::types::FlagType {
    fn from(flag_type: ExtensionFlagType) -> Self {
        match flag_type {
            ExtensionFlagType::Boolean => Self::Boolean,
            ExtensionFlagType::String => Self::String,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtensionFlagDefault {
    Boolean(bool),
    String(String),
}

impl From<&crate::core::extensions::types::FlagValue> for ExtensionFlagDefault {
    fn from(val: &crate::core::extensions::types::FlagValue) -> Self {
        match val {
            crate::core::extensions::types::FlagValue::Bool(b) => Self::Boolean(*b),
            crate::core::extensions::types::FlagValue::String(s) => Self::String(s.clone()),
        }
    }
}

impl From<&ExtensionFlagDefault> for crate::core::extensions::types::FlagValue {
    fn from(val: &ExtensionFlagDefault) -> Self {
        match val {
            ExtensionFlagDefault::Boolean(b) => Self::Bool(*b),
            ExtensionFlagDefault::String(s) => Self::String(s.clone()),
        }
    }
}

/// CLI flag registered by an extension.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionFlag {
    pub name: String,
    pub description: Option<String>,
    pub flag_type: ExtensionFlagType,
    pub default: Option<ExtensionFlagDefault>,
    pub extension_path: String,
}

impl From<&crate::core::extensions::types::ExtensionFlag> for ExtensionFlag {
    fn from(flag: &crate::core::extensions::types::ExtensionFlag) -> Self {
        Self {
            name: flag.name.clone(),
            description: flag.description.clone(),
            flag_type: flag.r#type.into(),
            default: flag.default.as_ref().map(Into::into),
            extension_path: flag.extension_path.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionLoadError {
    pub path: String,
    pub error: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionLoadWarning {
    pub path: String,
    pub warning: String,
}

/// Loaded extension handle tracked by `DefaultResourceLoader`.
#[derive(Clone, Debug)]
pub struct Extension {
    pub path: String,
    pub resolved_path: String,
    pub hidden: bool,
    pub replaceable: bool,
    pub source_info: SourceInfo,
    pub tools: IndexMap<String, RegisteredTool>,
    pub commands: IndexMap<String, RegisteredCommand>,
    pub flags: IndexMap<String, ExtensionFlag>,
}

impl From<&crate::core::extensions::types::LoadedExtension> for Extension {
    fn from(ext: &crate::core::extensions::types::LoadedExtension) -> Self {
        let tools = ext
            .tools
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(k, v)| (k.clone(), RegisteredTool::from(v)))
            .collect();
        let commands = ext
            .commands
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(k, v)| (k.clone(), RegisteredCommand::from(v)))
            .collect();
        let flags = ext
            .flags
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(k, v)| (k.clone(), ExtensionFlag::from(v)))
            .collect();
        Self {
            path: ext.path.clone(),
            resolved_path: ext.resolved_path.clone(),
            hidden: ext.hidden,
            replaceable: ext.replaceable,
            source_info: ext.source_info.clone(),
            tools,
            commands,
            flags,
        }
    }
}

/// Shared runtime handle created when loading extensions.
#[derive(Clone, Debug, Default)]
pub struct ExtensionRuntime {
    pub flag_values: IndexMap<String, ExtensionFlagDefault>,
    pub active: bool,
}

#[derive(Clone, Debug)]
pub struct LoadExtensionsResult {
    pub extensions: Vec<Extension>,
    pub errors: Vec<ExtensionLoadError>,
    pub warnings: Vec<ExtensionLoadWarning>,
    pub runtime: ExtensionRuntime,
}

impl Default for LoadExtensionsResult {
    fn default() -> Self {
        Self {
            extensions: Vec::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
            runtime: ExtensionRuntime {
                flag_values: IndexMap::new(),
                active: true,
            },
        }
    }
}

/// Mutable registrar handed to Rust `ExtensionFactory` closures during load.
pub struct ExtensionRegistrar {
    extension_path: String,
    source_info: SourceInfo,
    pub tools: IndexMap<String, RegisteredTool>,
    pub commands: IndexMap<String, RegisteredCommand>,
    pub flags: IndexMap<String, ExtensionFlag>,
}

impl ExtensionRegistrar {
    pub fn new(extension_path: String, source_info: SourceInfo) -> Self {
        Self {
            extension_path,
            source_info,
            tools: IndexMap::new(),
            commands: IndexMap::new(),
            flags: IndexMap::new(),
        }
    }

    pub fn register_tool(&mut self, name: impl Into<String>, description: impl Into<String>) {
        let name = name.into();
        self.tools.insert(
            name.clone(),
            RegisteredTool {
                name,
                description: description.into(),
                source_info: self.source_info.clone(),
            },
        );
    }

    pub fn register_command(&mut self, name: impl Into<String>, description: Option<String>) {
        let name = name.into();
        self.commands.insert(
            name.clone(),
            RegisteredCommand {
                name,
                description,
                source_info: self.source_info.clone(),
            },
        );
    }

    pub fn register_flag(
        &mut self,
        name: impl Into<String>,
        description: Option<String>,
        flag_type: ExtensionFlagType,
        default: Option<ExtensionFlagDefault>,
    ) {
        let name = name.into();
        self.flags.insert(
            name.clone(),
            ExtensionFlag {
                name,
                description,
                flag_type,
                default,
                extension_path: self.extension_path.clone(),
            },
        );
    }
}

pub type ExtensionFactory = Arc<dyn Fn(&mut ExtensionRegistrar) -> BoxFuture<Result<()>> + Send + Sync>;

#[derive(Clone)]
pub struct BuiltinExtension {
    pub name: String,
    pub factory: ExtensionFactory,
    pub hidden: bool,
    pub replaceable: bool,
    pub builtin: bool,
}

#[derive(Clone)]
pub enum InlineExtension {
    Factory(ExtensionFactory),
    Named(BuiltinExtension),
    Core(crate::core::extensions::types::InlineExtension),
}

impl From<crate::core::extensions::types::InlineExtension> for InlineExtension {
    fn from(ext: crate::core::extensions::types::InlineExtension) -> Self {
        Self::Core(ext)
    }
}

impl InlineExtension {
    pub fn named(name: impl Into<String>, factory: ExtensionFactory) -> Self {
        Self::Named(BuiltinExtension {
            name: name.into(),
            factory,
            hidden: false,
            replaceable: false,
            builtin: false,
        })
    }

    pub fn builtin(name: impl Into<String>, factory: ExtensionFactory, replaceable: bool) -> Self {
        Self::Named(BuiltinExtension {
            name: name.into(),
            factory,
            hidden: false,
            replaceable,
            builtin: true,
        })
    }
}

/// Built-in extensions supply the code of `builtin:<name>` extension paths.
pub fn is_builtin_extension(input: &InlineExtension) -> bool {
    matches!(input, InlineExtension::Named(ext) if ext.builtin)
        || matches!(
            input,
            InlineExtension::Core(crate::core::extensions::types::InlineExtension::Named(ext)) if ext.builtin
        )
}

/// Extension loading seam connecting `DefaultResourceLoader` to the extension runtime.
pub trait ExtensionLoaderHost: Send + Sync {
    fn clear_cache(&self);
    fn create_runtime(&self) -> ExtensionRuntime;
    fn load_extensions_cached(
        &self,
        paths: Vec<String>,
        cwd: String,
        event_bus: Arc<dyn EventBus>,
        runtime: Option<ExtensionRuntime>,
    ) -> BoxFuture<LoadExtensionsResult>;
    fn load_extension_from_factory(
        &self,
        factory: ExtensionFactory,
        cwd: String,
        event_bus: Arc<dyn EventBus>,
        runtime: ExtensionRuntime,
        extension_path: String,
    ) -> BoxFuture<Result<Extension>>;
    fn load_core_extension_from_factory(
        &self,
        _factory: crate::core::extensions::types::ExtensionFactory,
        _cwd: String,
        _event_bus: Arc<dyn EventBus>,
        extension_path: String,
        hidden: bool,
        replaceable: bool,
    ) -> BoxFuture<Result<Extension>> {
        let source = get_synthetic_path_source(&extension_path).unwrap_or_else(|| "local".to_owned());
        let base_dir = if is_synthetic_path(&extension_path) {
            None
        } else {
            Some(dirname(&extension_path))
        };
        let source_info = create_synthetic_source_info(
            &extension_path,
            SyntheticSourceOptions {
                source,
                scope: Some(SourceScope::Temporary),
                origin: Some(SourceOrigin::TopLevel),
                base_dir,
                changelog_path: None,
            },
        );
        Box::pin(async move {
            Ok(Extension {
                path: extension_path.clone(),
                resolved_path: extension_path,
                hidden,
                replaceable,
                source_info,
                tools: IndexMap::new(),
                commands: IndexMap::new(),
                flags: IndexMap::new(),
            })
        })
    }
    fn loaded_extensions_result(&self) -> Option<crate::core::extensions::types::LoadExtensionsResult> {
        None
    }
    fn sync_core_extensions(&self, _final_result: &LoadExtensionsResult) {}
}

/// Live extension loader host backed by [`crate::core::extensions::loader`].
#[derive(Default)]
pub struct CoreExtensionLoaderHost {
    runtime: Arc<Mutex<Option<crate::core::extensions::types::ExtensionRuntime>>>,
    loaded: Arc<Mutex<Vec<crate::core::extensions::types::LoadedExtension>>>,
    errors: Arc<Mutex<Vec<crate::core::extensions::types::LoadExtensionErrorEntry>>>,
    warnings: Arc<Mutex<Vec<crate::core::extensions::types::LoadExtensionWarningEntry>>>,
}

impl CoreExtensionLoaderHost {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn loaded_extensions_result(&self) -> Option<crate::core::extensions::types::LoadExtensionsResult> {
        let runtime = self.runtime.lock().unwrap_or_else(PoisonError::into_inner).clone()?;
        let extensions = self.loaded.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let errors = self.errors.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let warnings = self.warnings.lock().unwrap_or_else(PoisonError::into_inner).clone();
        Some(crate::core::extensions::types::LoadExtensionsResult {
            extensions,
            errors,
            warnings,
            runtime,
        })
    }

    fn ensure_runtime(&self) -> crate::core::extensions::types::ExtensionRuntime {
        let mut guard = self.runtime.lock().unwrap_or_else(PoisonError::into_inner);
        guard
            .get_or_insert_with(crate::core::extensions::loader::create_extension_runtime)
            .clone()
    }
}

impl ExtensionLoaderHost for CoreExtensionLoaderHost {
    fn clear_cache(&self) {
        crate::core::extensions::loader::clear_extension_cache();
        *self.runtime.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.loaded.lock().unwrap_or_else(PoisonError::into_inner).clear();
        self.errors.lock().unwrap_or_else(PoisonError::into_inner).clear();
        self.warnings.lock().unwrap_or_else(PoisonError::into_inner).clear();
    }

    fn create_runtime(&self) -> ExtensionRuntime {
        let rt = crate::core::extensions::loader::create_extension_runtime();
        *self.runtime.lock().unwrap_or_else(PoisonError::into_inner) = Some(rt);
        self.loaded.lock().unwrap_or_else(PoisonError::into_inner).clear();
        self.errors.lock().unwrap_or_else(PoisonError::into_inner).clear();
        self.warnings.lock().unwrap_or_else(PoisonError::into_inner).clear();
        ExtensionRuntime {
            flag_values: IndexMap::new(),
            active: true,
        }
    }

    fn load_extensions_cached(
        &self,
        paths: Vec<String>,
        cwd: String,
        event_bus: Arc<dyn EventBus>,
        runtime: Option<ExtensionRuntime>,
    ) -> BoxFuture<LoadExtensionsResult> {
        let real_rt = self.ensure_runtime();
        if let Some(ref rt) = runtime {
            let mut flags = real_rt.inner.flag_values.lock().unwrap_or_else(PoisonError::into_inner);
            for (k, v) in &rt.flag_values {
                flags.insert(k.clone(), v.into());
            }
        }
        let host_runtime_store = Arc::clone(&self.runtime);
        let host_loaded_store = Arc::clone(&self.loaded);
        Box::pin(async move {
            let res =
                crate::core::extensions::loader::load_extensions_cached(&paths, &cwd, Some(event_bus), Some(real_rt))
                    .await;
            *host_runtime_store.lock().unwrap_or_else(PoisonError::into_inner) = Some(res.runtime.clone());
            host_loaded_store
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend(res.extensions.iter().cloned());
            let flag_values = res
                .runtime
                .inner
                .flag_values
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .map(|(k, v)| (k.clone(), ExtensionFlagDefault::from(v)))
                .collect();
            LoadExtensionsResult {
                extensions: res.extensions.iter().map(Extension::from).collect(),
                errors: res
                    .errors
                    .into_iter()
                    .map(|e| ExtensionLoadError {
                        path: e.path,
                        error: e.error,
                    })
                    .collect(),
                warnings: res
                    .warnings
                    .into_iter()
                    .map(|w| ExtensionLoadWarning {
                        path: w.path,
                        warning: w.warning,
                    })
                    .collect(),
                runtime: ExtensionRuntime {
                    flag_values,
                    active: true,
                },
            }
        })
    }

    fn load_extension_from_factory(
        &self,
        factory: ExtensionFactory,
        cwd: String,
        event_bus: Arc<dyn EventBus>,
        _runtime: ExtensionRuntime,
        extension_path: String,
    ) -> BoxFuture<Result<Extension>> {
        let real_rt = self.ensure_runtime();
        let host_loaded_store = Arc::clone(&self.loaded);
        let path_for_source = extension_path.clone();
        let source = get_synthetic_path_source(&path_for_source).unwrap_or_else(|| "local".to_owned());
        let base_dir = if is_synthetic_path(&path_for_source) {
            None
        } else {
            Some(dirname(&path_for_source))
        };
        let source_info = create_synthetic_source_info(
            &path_for_source,
            SyntheticSourceOptions {
                source,
                scope: Some(SourceScope::Temporary),
                origin: Some(SourceOrigin::TopLevel),
                base_dir,
                changelog_path: None,
            },
        );
        let ext_factory: crate::core::extensions::types::ExtensionFactory =
            Arc::new(move |api: crate::core::extensions::types::ExtensionApi| {
                let factory = Arc::clone(&factory);
                let reg_path = path_for_source.clone();
                let reg_source = source_info.clone();
                Box::pin(async move {
                    let mut registrar = ExtensionRegistrar::new(reg_path, reg_source);
                    factory(&mut registrar).await?;
                    for (_name, t) in registrar.tools {
                        api.register_tool(crate::core::extensions::types::ToolDefinition {
                            name: t.name.clone(),
                            label: t.name,
                            description: t.description,
                            prompt_snippet: None,
                            prompt_guidelines: None,
                            parameters: pi_js::vendor::typebox::t::object::<
                                [(&str, pi_js::vendor::typebox::Schema); 0],
                                &str,
                            >([]),
                            constrained_sampling: None,
                            render_shell: None,
                            prepare_arguments: None,
                            output_schema: None,
                            exposure: None,
                            namespace: None,
                            annotations: None,
                            default_active: None,
                            prepare_loadout: None,
                            execution_mode: None,
                            execute: Arc::new(|_, _, _, _, _| {
                                Box::pin(async { Ok(pi_agent_core::AgentToolResult::text("")) })
                            }),
                            render_call: None,
                            render_result: None,
                        })?;
                    }
                    for (_name, c) in registrar.commands {
                        api.register_command(
                            &c.name,
                            crate::core::extensions::types::RegisteredCommandOptions {
                                description: c.description,
                                get_argument_completions: None,
                                handler: Arc::new(|_, _| Box::pin(async { Ok(()) })),
                            },
                        )?;
                    }
                    for (_name, f) in registrar.flags {
                        api.register_flag(
                            &f.name,
                            crate::core::extensions::types::FlagOptions {
                                description: f.description,
                                r#type: Some(f.flag_type.into()),
                                default: f.default.as_ref().map(Into::into),
                            },
                        )?;
                    }
                    Ok(())
                })
            });
        Box::pin(async move {
            let loaded = crate::core::extensions::loader::load_extension_from_factory(
                ext_factory,
                &cwd,
                event_bus,
                real_rt,
                Some(&extension_path),
            )
            .await?;
            host_loaded_store
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(loaded.clone());
            Ok(Extension::from(&loaded))
        })
    }

    fn load_core_extension_from_factory(
        &self,
        factory: crate::core::extensions::types::ExtensionFactory,
        cwd: String,
        event_bus: Arc<dyn EventBus>,
        extension_path: String,
        hidden: bool,
        replaceable: bool,
    ) -> BoxFuture<Result<Extension>> {
        let real_rt = self.ensure_runtime();
        let host_loaded_store = Arc::clone(&self.loaded);
        Box::pin(async move {
            let mut loaded = crate::core::extensions::loader::load_extension_from_factory(
                factory,
                &cwd,
                event_bus,
                real_rt,
                Some(&extension_path),
            )
            .await?;
            loaded.hidden = hidden;
            loaded.replaceable = replaceable;
            host_loaded_store
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(loaded.clone());
            Ok(Extension::from(&loaded))
        })
    }

    fn loaded_extensions_result(&self) -> Option<crate::core::extensions::types::LoadExtensionsResult> {
        CoreExtensionLoaderHost::loaded_extensions_result(self)
    }

    fn sync_core_extensions(&self, final_result: &LoadExtensionsResult) {
        let mut ext_by_resolved: IndexMap<String, &Extension> = IndexMap::new();
        for ext in &final_result.extensions {
            ext_by_resolved.insert(ext.resolved_path.clone(), ext);
        }
        {
            let mut loaded_guard = self.loaded.lock().unwrap_or_else(PoisonError::into_inner);
            let mut seen_resolved = IndexSet::new();
            let mut kept = Vec::new();
            for mut item in std::mem::take(&mut *loaded_guard) {
                if let Some(ext) = ext_by_resolved.get(&item.resolved_path)
                    && seen_resolved.insert(item.resolved_path.clone())
                {
                    item.source_info = ext.source_info.clone();
                    item.hidden = ext.hidden;
                    item.replaceable = ext.replaceable;
                    kept.push(item);
                }
            }
            *loaded_guard = kept;
        }
        *self.errors.lock().unwrap_or_else(PoisonError::into_inner) = final_result
            .errors
            .iter()
            .map(|e| crate::core::extensions::types::LoadExtensionErrorEntry {
                path: e.path.clone(),
                error: e.error.clone(),
            })
            .collect();
        *self.warnings.lock().unwrap_or_else(PoisonError::into_inner) = final_result
            .warnings
            .iter()
            .map(|w| crate::core::extensions::types::LoadExtensionWarningEntry {
                path: w.path.clone(),
                warning: w.warning.clone(),
            })
            .collect();
    }
}

/// Fallback extension host supporting inline factories, builtins, and lightweight disk stubs.
#[derive(Clone, Default)]
pub struct BasicExtensionLoaderHost {
    cache: Arc<Mutex<IndexMap<String, Extension>>>,
}

impl BasicExtensionLoaderHost {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ExtensionLoaderHost for BasicExtensionLoaderHost {
    fn clear_cache(&self) {
        let mut guard = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        guard.clear();
    }

    fn create_runtime(&self) -> ExtensionRuntime {
        ExtensionRuntime {
            flag_values: IndexMap::new(),
            active: true,
        }
    }

    fn load_extensions_cached(
        &self,
        paths: Vec<String>,
        cwd: String,
        _event_bus: Arc<dyn EventBus>,
        runtime: Option<ExtensionRuntime>,
    ) -> BoxFuture<LoadExtensionsResult> {
        let cache = self.cache.clone();
        let default_runtime = self.create_runtime();
        Box::pin(async move {
            let rt = runtime.unwrap_or(default_runtime);
            let mut result = LoadExtensionsResult {
                extensions: Vec::new(),
                errors: Vec::new(),
                warnings: Vec::new(),
                runtime: rt,
            };

            for raw_path in &paths {
                let resolved = resolve_path(
                    raw_path,
                    Some(&cwd),
                    PathInputOptions {
                        normalize_unicode_spaces: Some(true),
                        ..Default::default()
                    },
                );

                {
                    let guard = cache.lock().unwrap_or_else(PoisonError::into_inner);
                    if let Some(cached) = guard.get(&resolved) {
                        result.extensions.push(cached.clone());
                        continue;
                    }
                }

                if !exists(&resolved) {
                    result.errors.push(ExtensionLoadError {
                        path: resolved,
                        error: format!("Extension path does not exist: {raw_path}"),
                    });
                    continue;
                }

                let source = get_synthetic_path_source(raw_path).unwrap_or_else(|| "local".to_owned());
                let base_dir = if is_synthetic_path(raw_path) {
                    None
                } else {
                    Some(dirname(&resolved))
                };
                let source_info = create_synthetic_source_info(
                    raw_path,
                    SyntheticSourceOptions {
                        source,
                        scope: Some(SourceScope::Temporary),
                        origin: Some(SourceOrigin::TopLevel),
                        base_dir,
                        changelog_path: None,
                    },
                );

                let ext = Extension {
                    path: raw_path.clone(),
                    resolved_path: resolved.clone(),
                    hidden: false,
                    replaceable: false,
                    source_info,
                    tools: IndexMap::new(),
                    commands: IndexMap::new(),
                    flags: IndexMap::new(),
                };

                let mut guard = cache.lock().unwrap_or_else(PoisonError::into_inner);
                guard.insert(resolved, ext.clone());
                result.extensions.push(ext);
            }

            result
        })
    }

    fn load_extension_from_factory(
        &self,
        factory: ExtensionFactory,
        _cwd: String,
        _event_bus: Arc<dyn EventBus>,
        _runtime: ExtensionRuntime,
        extension_path: String,
    ) -> BoxFuture<Result<Extension>> {
        Box::pin(async move {
            let source = get_synthetic_path_source(&extension_path).unwrap_or_else(|| "local".to_owned());
            let base_dir = if is_synthetic_path(&extension_path) {
                None
            } else {
                Some(dirname(&extension_path))
            };
            let source_info = create_synthetic_source_info(
                &extension_path,
                SyntheticSourceOptions {
                    source,
                    scope: Some(SourceScope::Temporary),
                    origin: Some(SourceOrigin::TopLevel),
                    base_dir,
                    changelog_path: None,
                },
            );
            let mut registrar = ExtensionRegistrar::new(extension_path.clone(), source_info.clone());
            factory(&mut registrar).await?;
            Ok(Extension {
                path: extension_path.clone(),
                resolved_path: extension_path,
                hidden: false,
                replaceable: false,
                source_info,
                tools: registrar.tools,
                commands: registrar.commands,
                flags: registrar.flags,
            })
        })
    }
}

// ============================================================================
// ResourceLoader public API & options
// ============================================================================

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionResourcePath {
    pub path: String,
    pub metadata: PathMetadata,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResourceExtensionPaths {
    pub skill_paths: Option<Vec<ExtensionResourcePath>>,
    pub prompt_paths: Option<Vec<ExtensionResourcePath>>,
    pub theme_paths: Option<Vec<ExtensionResourcePath>>,
}

pub type ResolveProjectTrustFn = Arc<dyn Fn(LoadExtensionsResult) -> BoxFuture<bool> + Send + Sync>;

#[derive(Clone, Default)]
pub struct ResourceLoaderReloadOptions {
    pub resolve_project_trust: Option<ResolveProjectTrustFn>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextFile {
    pub path: String,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptSource {
    pub path: String,
}

const HOST_PROVIDED_EXTENSION_PACKAGES: &[&str] = &[
    "@earendil-works/pi-agent-core",
    "@earendil-works/pi-ai",
    "@earendil-works/pi-coding-agent",
    "@earendil-works/pi-tui",
    "@mariozechner/pi-agent-core",
    "@mariozechner/pi-ai",
    "@mariozechner/pi-coding-agent",
    "@mariozechner/pi-tui",
    "@sinclair/typebox",
    "typebox",
];

fn is_host_provided_extension_package(name: &str) -> bool {
    HOST_PROVIDED_EXTENSION_PACKAGES.contains(&name)
}

fn collect_extension_package_warnings(
    extension_paths: &[String],
    metadata_by_path: &IndexMap<String, PathMetadata>,
) -> Result<Vec<ExtensionLoadWarning>> {
    let mut warnings = Vec::new();
    let mut package_roots = IndexSet::new();
    for ext_path in extension_paths {
        if let Some(root) = metadata_by_path
            .get(ext_path)
            .and_then(|meta| meta.package_root.as_ref())
        {
            package_roots.insert(root.clone());
        }
    }

    for package_root in package_roots {
        let package_json_path = join(&[&package_root, "package.json"]);
        if !exists(&package_json_path) {
            continue;
        }
        let raw = read_to_string(&package_json_path)?;
        let manifest: Value =
            serde_json::from_str(strip_bom(&raw)).map_err(|err| Error::msg(format!("SyntaxError: {err}")))?;
        let Some(deps) = manifest.get("dependencies").and_then(Value::as_object) else {
            continue;
        };
        let mut host_deps: Vec<String> = deps
            .keys()
            .filter(|name| is_host_provided_extension_package(name))
            .cloned()
            .collect();
        host_deps.sort();
        if host_deps.is_empty() {
            continue;
        }
        warnings.push(ExtensionLoadWarning {
            path: package_json_path,
            warning: format!(
                "Host-provided extension packages must be declared in peerDependencies with a \"*\" range, not dependencies: {}. Installed copies can bypass the extension loader and create duplicate runtime modules.",
                host_deps.join(", ")
            ),
        });
    }

    Ok(warnings)
}

fn describe_package_install_failure(failure: &PackageInstallFailure) -> String {
    let hint = if failure.scope == SourceScope::Temporary {
        "Check the source and try again.".to_owned()
    } else {
        let local_flag = if failure.scope == SourceScope::Project {
            " --local"
        } else {
            ""
        };
        format!(
            "Run \"{} remove {}{}\" to remove it from settings.json.",
            *APP_NAME, failure.source, local_flag
        )
    };
    format!("failed to install ({}). {hint}", failure.message)
}

/// One-line startup warning for a package that could not be installed.
pub fn format_package_install_warning(failure: &PackageInstallFailure) -> String {
    format!(
        "Extension package \"{}\": {}",
        failure.source,
        describe_package_install_failure(failure)
    )
}

fn collect_package_install_warnings(groups: &[Option<&[PackageInstallFailure]>]) -> Vec<ExtensionLoadWarning> {
    let mut warnings = Vec::new();
    for group in groups.iter().flatten() {
        for failure in *group {
            warnings.push(ExtensionLoadWarning {
                path: failure.source.clone(),
                warning: describe_package_install_failure(failure),
            });
        }
    }
    warnings
}

fn merge_extension_warnings(result: &mut LoadExtensionsResult, warnings: Vec<ExtensionLoadWarning>) {
    let mut by_path: IndexMap<String, ExtensionLoadWarning> = IndexMap::new();
    for warning in result.warnings.drain(..).chain(warnings) {
        by_path.insert(warning.path.clone(), warning);
    }
    result.warnings = by_path.into_values().collect();
}

fn extension_registered_names(extension: &Extension) -> Vec<String> {
    let mut names = Vec::new();
    for name in extension.tools.keys() {
        names.push(format!("tool:{name}"));
    }
    for name in extension.commands.keys() {
        names.push(format!("command:{name}"));
    }
    for name in extension.flags.keys() {
        names.push(format!("flag:{name}"));
    }
    names
}

/// Leave out replaceable extensions that share a tool, command, or flag name
/// with another extension.
fn omit_replaced_extensions(extensions: Vec<Extension>, warnings: &mut Vec<ExtensionLoadWarning>) -> Vec<Extension> {
    let mut taken: IndexMap<String, Extension> = IndexMap::new();
    for ext in extensions.iter().filter(|ext| !ext.replaceable) {
        for name in extension_registered_names(ext) {
            taken.insert(name, ext.clone());
        }
    }

    extensions
        .into_iter()
        .filter(|ext| {
            if !ext.replaceable {
                return true;
            }
            let replacement = extension_registered_names(ext)
                .into_iter()
                .find_map(|name| taken.get(&name).map(|owner| (name, owner)));
            let Some((key, replacement_ext)) = replacement else {
                return true;
            };
            if let Some(builtin_name) = ext.path.strip_prefix(BUILTIN_PATH_PREFIX) {
                let (kind, raw_name) = key.split_once(':').unwrap_or(("tool", &key));
                let registered_name = match kind {
                    "command" => format!("/{raw_name}"),
                    "flag" => format!("--{raw_name}"),
                    _ => raw_name.to_owned(),
                };
                warnings.push(ExtensionLoadWarning {
                    path: ext.path.clone(),
                    warning: format!(
                        "Extension {} registers {} `{registered_name}`, so built-in extension `{builtin_name}` was not loaded. To use `{builtin_name}`, run `pi config` and make sure it is enabled under Built-in extensions, then disable or remove the existing extension. We recommend only having one or the other loaded at a time.",
                        replacement_ext.path, kind
                    ),
                });
            }
            false
        })
        .collect()
}

pub trait ResourceLoader: Send + Sync {
    fn get_extensions(&self) -> LoadExtensionsResult;
    fn get_core_extensions(&self) -> crate::core::extensions::types::LoadExtensionsResult {
        crate::core::extensions::types::LoadExtensionsResult {
            extensions: Vec::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
            runtime: crate::core::extensions::loader::create_extension_runtime(),
        }
    }
    fn get_skills(&self) -> (Vec<Skill>, Vec<ResourceDiagnostic>);
    fn get_prompts(&self) -> (Vec<PromptTemplate>, Vec<ResourceDiagnostic>);
    fn get_themes(&self) -> (Vec<Theme>, Vec<ResourceDiagnostic>);
    fn get_agents_files(&self) -> Vec<ContextFile>;
    fn get_system_prompt(&self) -> Option<String>;
    fn get_system_prompt_source(&self) -> Option<PromptSource>;
    fn get_append_system_prompt(&self) -> Vec<String>;
    fn get_append_system_prompt_sources(&self) -> Vec<PromptSource>;
    fn extend_resources(&self, paths: ResourceExtensionPaths);
    fn reload(&self, options: Option<ResourceLoaderReloadOptions>) -> BoxFuture<Result<()>>;
}

fn resolve_prompt_input(input: Option<&str>, description: &str) -> Option<String> {
    let input = input?;
    if input.is_empty() {
        return None;
    }

    if exists(input) {
        match read_to_string(input) {
            Ok(content) => return Some(strip_bom(&content).to_owned()),
            Err(error) => {
                console_error(&yellow(&format!(
                    "Warning: Could not read {description} file {input}: {error}"
                )));
                return Some(input.to_owned());
            }
        }
    }

    Some(input.to_owned())
}

fn load_context_file_from_dir(dir: &str) -> Option<ContextFile> {
    const CANDIDATES: &[&str] = &["AGENTS.override.md", "AGENTS.md", "AGENTS.MD", "CLAUDE.md", "CLAUDE.MD"];
    for filename in CANDIDATES {
        let file_path = join(&[dir, filename]);
        if exists(&file_path) {
            match stat(&file_path) {
                Ok(st) if st.is_file() => match read_to_string(&file_path) {
                    Ok(content) => {
                        return Some(ContextFile {
                            path: file_path,
                            content: strip_bom(&content).to_owned(),
                        });
                    }
                    Err(error) => {
                        console_error(&yellow(&format!("Warning: Could not read {file_path}: {error}")));
                    }
                },
                Ok(_) => {}
                Err(error) => {
                    console_error(&yellow(&format!("Warning: Could not read {file_path}: {error}")));
                }
            }
        }
    }
    None
}

/// The main repo's context file that a nested linked worktree's own copy shadows.
fn find_shadowed_context_file(cwd: &str) -> Option<String> {
    let git_paths = find_git_paths(cwd)?;
    let common_git_dir = canonicalize_path(&git_paths.common_git_dir);
    let worktree_root = canonicalize_path(&git_paths.repo_dir);
    let main_repo_root = dirname(&common_git_dir);
    if !is_path_within(&main_repo_root, &worktree_root) || paths_equal(&worktree_root, &main_repo_root) {
        return None;
    }
    if !paths_equal(&canonicalize_path(&join(&[&main_repo_root, ".git"])), &common_git_dir) {
        return None;
    }
    let worktree_context_file = load_context_file_from_dir(&worktree_root)?;
    Some(join(&[&main_repo_root, &basename(&worktree_context_file.path, None)]))
}

#[derive(Clone, Debug)]
pub struct LoadProjectContextFilesOptions {
    pub cwd: String,
    pub agent_dir: String,
}

pub fn load_project_context_files(options: LoadProjectContextFilesOptions) -> Vec<ContextFile> {
    let resolved_cwd = resolve_path(&options.cwd, None, PathInputOptions::default());
    let resolved_agent_dir = resolve_path(&options.agent_dir, None, PathInputOptions::default());

    let mut context_files = Vec::new();
    let mut seen_paths = IndexSet::new();

    if let Some(global_context) = load_context_file_from_dir(&resolved_agent_dir) {
        seen_paths.insert(global_context.path.clone());
        context_files.push(global_context);
    }

    let mut ancestor_context_files = Vec::new();
    let shadowed_context_file = find_shadowed_context_file(&resolved_cwd);
    let mut current_dir = resolved_cwd;

    loop {
        if let Some(context_file) = load_context_file_from_dir(&current_dir) {
            let is_shadowed = shadowed_context_file.as_ref().is_some_and(|shadowed| {
                paths_equal(
                    &join(&[&canonicalize_path(&current_dir), &basename(&context_file.path, None)]),
                    shadowed,
                )
            });
            if !is_shadowed && !seen_paths.contains(&context_file.path) {
                seen_paths.insert(context_file.path.clone());
                ancestor_context_files.insert(0, context_file);
            }
        }

        let parent_dir = dirname(&current_dir);
        if parent_dir == current_dir {
            break;
        }
        current_dir = parent_dir;
    }

    context_files.extend(ancestor_context_files);
    context_files
}

#[derive(Clone, Debug, Default)]
pub struct SkillsResult {
    pub skills: Vec<Skill>,
    pub diagnostics: Vec<ResourceDiagnostic>,
}

#[derive(Clone, Debug, Default)]
pub struct PromptsResult {
    pub prompts: Vec<PromptTemplate>,
    pub diagnostics: Vec<ResourceDiagnostic>,
}

#[derive(Clone, Debug, Default)]
pub struct ThemesResult {
    pub themes: Vec<Theme>,
    pub diagnostics: Vec<ResourceDiagnostic>,
}

pub type ExtensionsOverrideFn = Arc<dyn Fn(LoadExtensionsResult) -> LoadExtensionsResult + Send + Sync>;
pub type SkillsOverrideFn = Arc<dyn Fn(SkillsResult) -> SkillsResult + Send + Sync>;
pub type PromptsOverrideFn = Arc<dyn Fn(PromptsResult) -> PromptsResult + Send + Sync>;
pub type ThemesOverrideFn = Arc<dyn Fn(ThemesResult) -> ThemesResult + Send + Sync>;
pub type AgentsFilesOverrideFn = Arc<dyn Fn(Vec<ContextFile>) -> Vec<ContextFile> + Send + Sync>;
pub type SystemPromptOverrideFn = Arc<dyn Fn(Option<String>) -> Option<String> + Send + Sync>;
pub type AppendSystemPromptOverrideFn = Arc<dyn Fn(Vec<String>) -> Vec<String> + Send + Sync>;

#[derive(Clone, Default)]
pub struct DefaultResourceLoaderOptions {
    pub cwd: String,
    pub agent_dir: String,
    pub settings_manager: Option<SettingsManager>,
    pub event_bus: Option<Arc<dyn EventBus>>,
    pub additional_extension_paths: Vec<String>,
    pub additional_skill_paths: Vec<String>,
    pub additional_prompt_template_paths: Vec<String>,
    pub additional_theme_paths: Vec<String>,
    pub extension_factories: Vec<InlineExtension>,
    pub no_extensions: bool,
    /// Built-in extensions not to load, by name (such as `mcp`), even when settings or `-e` enable them.
    pub disabled_builtin_extensions: Option<Vec<String>>,
    pub no_skills: bool,
    pub no_prompt_templates: bool,
    pub no_themes: bool,
    pub no_context_files: bool,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<Vec<String>>,
    pub extensions_override: Option<ExtensionsOverrideFn>,
    pub skills_override: Option<SkillsOverrideFn>,
    pub prompts_override: Option<PromptsOverrideFn>,
    pub themes_override: Option<ThemesOverrideFn>,
    pub agents_files_override: Option<AgentsFilesOverrideFn>,
    pub system_prompt_override: Option<SystemPromptOverrideFn>,
    pub append_system_prompt_override: Option<AppendSystemPromptOverrideFn>,
    pub extension_host: Option<Arc<dyn ExtensionLoaderHost>>,
    pub prompt_loader: Option<Arc<dyn PromptLoaderHook>>,
    pub theme_loader: Option<Arc<dyn ThemeLoaderHook>>,
}

struct ResourceLoaderState {
    extensions_result: LoadExtensionsResult,
    skills: Vec<Skill>,
    skill_diagnostics: Vec<ResourceDiagnostic>,
    prompts: Vec<PromptTemplate>,
    prompt_diagnostics: Vec<ResourceDiagnostic>,
    themes: Vec<Theme>,
    theme_diagnostics: Vec<ResourceDiagnostic>,
    agents_files: Vec<ContextFile>,
    system_prompt: Option<String>,
    system_prompt_source_path: Option<String>,
    append_system_prompt: Vec<String>,
    append_system_prompt_source_paths: Vec<String>,
    last_skill_paths: Vec<String>,
    extension_skill_source_infos: IndexMap<String, SourceInfo>,
    extension_prompt_source_infos: IndexMap<String, SourceInfo>,
    extension_theme_source_infos: IndexMap<String, SourceInfo>,
    resource_metadata_by_path: IndexMap<String, PathMetadata>,
    last_prompt_paths: Vec<String>,
    last_theme_paths: Vec<String>,
    loaded: bool,
}

#[derive(Clone)]
pub struct DefaultResourceLoader {
    inner: Arc<ResourceLoaderInner>,
}

struct ResourceLoaderInner {
    cwd: String,
    agent_dir: String,
    settings_manager: SettingsManager,
    event_bus: Arc<dyn EventBus>,
    package_manager: DefaultPackageManager,
    additional_extension_paths: Vec<String>,
    additional_skill_paths: Vec<String>,
    additional_prompt_template_paths: Vec<String>,
    additional_theme_paths: Vec<String>,
    extension_factories: Vec<InlineExtension>,
    builtin_extensions: IndexMap<String, BuiltinExtension>,
    builtin_core_extensions: IndexMap<String, crate::core::extensions::types::NamedInlineExtension>,
    no_extensions: bool,
    disabled_builtin_extensions: IndexSet<String>,
    no_skills: bool,
    no_prompt_templates: bool,
    no_themes: bool,
    no_context_files: bool,
    system_prompt_source: Option<String>,
    append_system_prompt_source: Option<Vec<String>>,
    extensions_override: Option<ExtensionsOverrideFn>,
    skills_override: Option<SkillsOverrideFn>,
    prompts_override: Option<PromptsOverrideFn>,
    themes_override: Option<ThemesOverrideFn>,
    agents_files_override: Option<AgentsFilesOverrideFn>,
    system_prompt_override: Option<SystemPromptOverrideFn>,
    append_system_prompt_override: Option<AppendSystemPromptOverrideFn>,
    extension_host: Arc<dyn ExtensionLoaderHost>,
    prompt_loader: Arc<dyn PromptLoaderHook>,
    theme_loader: Arc<dyn ThemeLoaderHook>,
    state: Mutex<ResourceLoaderState>,
}

impl DefaultResourceLoader {
    pub fn new(options: DefaultResourceLoaderOptions) -> Self {
        let cwd = resolve_path(&options.cwd, None, PathInputOptions::default());
        let agent_dir = resolve_path(&options.agent_dir, None, PathInputOptions::default());
        let settings_manager = options
            .settings_manager
            .unwrap_or_else(|| SettingsManager::create(&cwd, Some(&agent_dir), None));
        let event_bus: Arc<dyn EventBus> = options
            .event_bus
            .unwrap_or_else(|| create_event_bus() as Arc<dyn EventBus>);

        let mut extension_factories = Vec::new();
        let mut builtin_extensions = IndexMap::new();
        let mut builtin_core_extensions = IndexMap::new();
        let mut builtin_names = Vec::new();
        for item in options.extension_factories {
            match item {
                InlineExtension::Named(ext) if ext.builtin => {
                    builtin_names.push(ext.name.clone());
                    builtin_extensions.insert(ext.name.clone(), ext);
                }
                InlineExtension::Core(crate::core::extensions::types::InlineExtension::Named(ext)) if ext.builtin => {
                    builtin_names.push(ext.name.clone());
                    builtin_core_extensions.insert(ext.name.clone(), ext);
                }
                other => extension_factories.push(other),
            }
        }

        let package_manager = DefaultPackageManager::new(crate::core::package_manager::PackageManagerOptions {
            cwd: cwd.clone(),
            agent_dir: agent_dir.clone(),
            settings_manager: settings_manager.clone(),
            builtin_extensions: builtin_names,
        });

        let extension_host: Arc<dyn ExtensionLoaderHost> = options
            .extension_host
            .unwrap_or_else(|| Arc::new(CoreExtensionLoaderHost::new()));
        let prompt_loader: Arc<dyn PromptLoaderHook> =
            options.prompt_loader.unwrap_or_else(|| Arc::new(BasicPromptLoader));
        let theme_loader: Arc<dyn ThemeLoaderHook> = options.theme_loader.unwrap_or_else(|| Arc::new(BasicThemeLoader));

        let initial_runtime = extension_host.create_runtime();
        let state = ResourceLoaderState {
            extensions_result: LoadExtensionsResult {
                extensions: Vec::new(),
                errors: Vec::new(),
                warnings: Vec::new(),
                runtime: initial_runtime,
            },
            skills: Vec::new(),
            skill_diagnostics: Vec::new(),
            prompts: Vec::new(),
            prompt_diagnostics: Vec::new(),
            themes: Vec::new(),
            theme_diagnostics: Vec::new(),
            agents_files: Vec::new(),
            system_prompt: None,
            system_prompt_source_path: None,
            append_system_prompt: Vec::new(),
            append_system_prompt_source_paths: Vec::new(),
            last_skill_paths: Vec::new(),
            extension_skill_source_infos: IndexMap::new(),
            extension_prompt_source_infos: IndexMap::new(),
            extension_theme_source_infos: IndexMap::new(),
            resource_metadata_by_path: IndexMap::new(),
            last_prompt_paths: Vec::new(),
            last_theme_paths: Vec::new(),
            loaded: false,
        };

        Self {
            inner: Arc::new(ResourceLoaderInner {
                cwd,
                agent_dir,
                settings_manager,
                event_bus,
                package_manager,
                additional_extension_paths: options.additional_extension_paths,
                additional_skill_paths: options.additional_skill_paths,
                additional_prompt_template_paths: options.additional_prompt_template_paths,
                additional_theme_paths: options.additional_theme_paths,
                extension_factories,
                builtin_extensions,
                builtin_core_extensions,
                no_extensions: options.no_extensions,
                disabled_builtin_extensions: options
                    .disabled_builtin_extensions
                    .unwrap_or_default()
                    .into_iter()
                    .collect(),
                no_skills: options.no_skills,
                no_prompt_templates: options.no_prompt_templates,
                no_themes: options.no_themes,
                no_context_files: options.no_context_files,
                system_prompt_source: options.system_prompt,
                append_system_prompt_source: options.append_system_prompt,
                extensions_override: options.extensions_override,
                skills_override: options.skills_override,
                prompts_override: options.prompts_override,
                themes_override: options.themes_override,
                agents_files_override: options.agents_files_override,
                system_prompt_override: options.system_prompt_override,
                append_system_prompt_override: options.append_system_prompt_override,
                extension_host,
                prompt_loader,
                theme_loader,
                state: Mutex::new(state),
            }),
        }
    }

    pub fn settings_manager(&self) -> &SettingsManager {
        &self.inner.settings_manager
    }

    pub fn get_extensions(&self) -> LoadExtensionsResult {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extensions_result
            .clone()
    }

    pub fn get_skills(&self) -> SkillsResult {
        let guard = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        SkillsResult {
            skills: guard.skills.clone(),
            diagnostics: guard.skill_diagnostics.clone(),
        }
    }

    pub fn get_prompts(&self) -> PromptsResult {
        let guard = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        PromptsResult {
            prompts: guard.prompts.clone(),
            diagnostics: guard.prompt_diagnostics.clone(),
        }
    }

    pub fn get_themes(&self) -> ThemesResult {
        let guard = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        ThemesResult {
            themes: guard.themes.clone(),
            diagnostics: guard.theme_diagnostics.clone(),
        }
    }

    pub fn get_agents_files(&self) -> Vec<ContextFile> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .agents_files
            .clone()
    }

    pub fn get_system_prompt(&self) -> Option<String> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .system_prompt
            .clone()
    }

    pub fn get_system_prompt_source(&self) -> Option<PromptSource> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .system_prompt_source_path
            .as_ref()
            .map(|p| PromptSource { path: p.clone() })
    }

    pub fn get_append_system_prompt(&self) -> Vec<String> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .append_system_prompt
            .clone()
    }

    pub fn get_append_system_prompt_sources(&self) -> Vec<PromptSource> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .append_system_prompt_source_paths
            .iter()
            .map(|p| PromptSource { path: p.clone() })
            .collect()
    }

    pub fn extend_resources(&self, paths: ResourceExtensionPaths) {
        let skill_paths = self.normalize_extension_paths(paths.skill_paths.as_deref().unwrap_or(&[]));
        let prompt_paths = self.normalize_extension_paths(paths.prompt_paths.as_deref().unwrap_or(&[]));
        let theme_paths = self.normalize_extension_paths(paths.theme_paths.as_deref().unwrap_or(&[]));

        let mut guard = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        for entry in &skill_paths {
            guard.extension_skill_source_infos.insert(
                entry.path.clone(),
                create_source_info(&entry.path, &entry.metadata.to_source_metadata()),
            );
        }
        for entry in &prompt_paths {
            guard.extension_prompt_source_infos.insert(
                entry.path.clone(),
                create_source_info(&entry.path, &entry.metadata.to_source_metadata()),
            );
        }
        for entry in &theme_paths {
            guard.extension_theme_source_infos.insert(
                entry.path.clone(),
                create_source_info(&entry.path, &entry.metadata.to_source_metadata()),
            );
        }

        if !skill_paths.is_empty() {
            let new_paths: Vec<String> = skill_paths.into_iter().map(|e| e.path).collect();
            let merged = self.merge_paths(&guard.last_skill_paths, &new_paths);
            guard.last_skill_paths = merged.clone();
            let metadata_by_path = guard.resource_metadata_by_path.clone();
            let ext_infos = guard.extension_skill_source_infos.clone();
            drop(guard);
            let res = self.compute_skills_from_paths(&merged, &ext_infos, Some(&metadata_by_path));
            let mut g = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            g.skills = res.skills;
            g.skill_diagnostics = res.diagnostics;
            guard = g;
        }

        if !prompt_paths.is_empty() {
            let new_paths: Vec<String> = prompt_paths.into_iter().map(|e| e.path).collect();
            let merged = self.merge_paths(&guard.last_prompt_paths, &new_paths);
            guard.last_prompt_paths = merged.clone();
            let metadata_by_path = guard.resource_metadata_by_path.clone();
            let ext_infos = guard.extension_prompt_source_infos.clone();
            drop(guard);
            let res = self.compute_prompts_from_paths(&merged, &ext_infos, Some(&metadata_by_path));
            let mut g = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            g.prompts = res.prompts;
            g.prompt_diagnostics = res.diagnostics;
            guard = g;
        }

        if !theme_paths.is_empty() {
            let new_paths: Vec<String> = theme_paths.into_iter().map(|e| e.path).collect();
            let merged = self.merge_paths(&guard.last_theme_paths, &new_paths);
            guard.last_theme_paths = merged.clone();
            let metadata_by_path = guard.resource_metadata_by_path.clone();
            let ext_infos = guard.extension_theme_source_infos.clone();
            drop(guard);
            let res = self.compute_themes_from_paths(&merged, &ext_infos, Some(&metadata_by_path));
            let mut g = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            g.themes = res.themes;
            g.theme_diagnostics = res.diagnostics;
        } else {
            drop(guard);
        }
    }

    pub async fn load_project_trust_extensions(&self) -> Result<LoadExtensionsResult> {
        self.inner.settings_manager.set_project_trusted(false);
        self.inner.settings_manager.reload().await;
        self.load_current_extension_set(true).await
    }

    pub async fn reload(&self, options: Option<ResourceLoaderReloadOptions>) -> Result<()> {
        reset_timings(TimingNamespaceKind::Extensions);

        let already_loaded = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner).loaded;
        if already_loaded {
            self.inner.extension_host.clear_cache();
        }

        let mut pre_trust_extensions: Option<LoadExtensionsResult> = None;
        if let Some(resolve_trust) = options.as_ref().and_then(|o| o.resolve_project_trust.as_ref()) {
            let preloaded = self.load_project_trust_extensions().await?;
            let project_trusted = resolve_trust(preloaded.clone()).await;
            self.inner.settings_manager.set_project_trusted(project_trusted);
            pre_trust_extensions = Some(preloaded);
        }

        self.inner.settings_manager.reload().await;
        let resolved_paths = self.inner.package_manager.resolve(None).await?;
        let cli_extension_paths = self
            .inner
            .package_manager
            .resolve_extension_sources(&self.inner.additional_extension_paths, false, true)
            .await?;
        let install_warnings = collect_package_install_warnings(&[
            resolved_paths.install_failures.as_deref(),
            cli_extension_paths.install_failures.as_deref(),
        ]);

        let mut metadata_by_path: IndexMap<String, PathMetadata> = IndexMap::new();

        let mut get_enabled_resources = |resources: &[ResolvedResource]| -> Vec<ResolvedResource> {
            for r in resources {
                metadata_by_path
                    .entry(r.path.clone())
                    .or_insert_with(|| r.metadata.clone());
            }
            resources.iter().filter(|r| r.enabled).cloned().collect()
        };

        let enabled_extensions: Vec<String> = get_enabled_resources(&resolved_paths.extensions)
            .into_iter()
            .map(|r| r.path)
            .collect();
        let enabled_skill_resources = get_enabled_resources(&resolved_paths.skills);
        let enabled_prompts: Vec<String> = get_enabled_resources(&resolved_paths.prompts)
            .into_iter()
            .map(|r| r.path)
            .collect();
        let enabled_themes: Vec<String> = get_enabled_resources(&resolved_paths.themes)
            .into_iter()
            .map(|r| r.path)
            .collect();

        let enabled_skills: Vec<String> = enabled_skill_resources
            .iter()
            .map(|resource| self.map_skill_path(resource, &mut metadata_by_path))
            .collect();

        for r in &cli_extension_paths.extensions {
            metadata_by_path.entry(r.path.clone()).or_insert_with(|| PathMetadata {
                source: "cli".to_owned(),
                scope: SourceScope::Temporary,
                origin: SourceOrigin::TopLevel,
                base_dir: None,
                package_root: None,
                changelog_path: None,
            });
        }
        for r in &cli_extension_paths.skills {
            metadata_by_path.entry(r.path.clone()).or_insert_with(|| PathMetadata {
                source: "cli".to_owned(),
                scope: SourceScope::Temporary,
                origin: SourceOrigin::TopLevel,
                base_dir: None,
                package_root: None,
                changelog_path: None,
            });
        }

        let cli_enabled_extensions: Vec<String> = cli_extension_paths
            .extensions
            .iter()
            .filter(|r| r.enabled)
            .map(|r| r.path.clone())
            .collect();
        let cli_enabled_skills: Vec<String> = cli_extension_paths
            .skills
            .iter()
            .filter(|r| r.enabled)
            .map(|r| r.path.clone())
            .collect();
        let cli_enabled_prompts: Vec<String> = cli_extension_paths
            .prompts
            .iter()
            .filter(|r| r.enabled)
            .map(|r| r.path.clone())
            .collect();
        let cli_enabled_themes: Vec<String> = cli_extension_paths
            .themes
            .iter()
            .filter(|r| r.enabled)
            .map(|r| r.path.clone())
            .collect();

        let extension_paths: Vec<String> = (if self.inner.no_extensions {
            cli_enabled_extensions.clone()
        } else {
            self.merge_paths(&cli_enabled_extensions, &enabled_extensions)
        })
        .into_iter()
        .filter(|path| {
            !path.starts_with(BUILTIN_PATH_PREFIX)
                || !self
                    .inner
                    .disabled_builtin_extensions
                    .contains(&path[BUILTIN_PATH_PREFIX.len()..])
        })
        .collect();

        let package_warnings = collect_extension_package_warnings(&extension_paths, &metadata_by_path)?;
        let mut extensions_result = self
            .load_final_extension_set(&extension_paths, pre_trust_extensions)
            .await;
        let mut combined_warnings = install_warnings;
        combined_warnings.extend(package_warnings);
        merge_extension_warnings(&mut extensions_result, combined_warnings);
        for p in &self.inner.additional_extension_paths {
            if is_local_path(p) {
                let resolved = self.resolve_resource_path(p);
                if !exists(&resolved) && !extensions_result.errors.iter().any(|e| e.path == resolved) {
                    extensions_result.errors.push(ExtensionLoadError {
                        path: resolved.clone(),
                        error: format!("Extension path does not exist: {resolved}"),
                    });
                }
            }
        }
        let mut final_extensions_result = match &self.inner.extensions_override {
            Some(override_fn) => override_fn(extensions_result),
            None => extensions_result,
        };
        self.apply_extension_source_info(&mut final_extensions_result.extensions, &metadata_by_path);

        let skill_paths = if self.inner.no_skills {
            self.merge_paths(&cli_enabled_skills, &self.inner.additional_skill_paths)
        } else {
            let mut primary = cli_enabled_skills;
            primary.extend(enabled_skills);
            self.merge_paths(&primary, &self.inner.additional_skill_paths)
        };

        let empty_infos = IndexMap::new();
        let mut skills_res = self.compute_skills_from_paths(&skill_paths, &empty_infos, Some(&metadata_by_path));
        for p in &self.inner.additional_skill_paths {
            if is_local_path(p) {
                let resolved = self.resolve_resource_path(p);
                if !exists(&resolved)
                    && !skills_res
                        .diagnostics
                        .iter()
                        .any(|d| d.path.as_deref() == Some(resolved.as_str()))
                {
                    skills_res.diagnostics.push(ResourceDiagnostic {
                        r#type: ResourceDiagnosticType::Error,
                        message: "Skill path does not exist".to_owned(),
                        path: Some(resolved),
                        collision: None,
                    });
                }
            }
        }

        let prompt_paths = if self.inner.no_prompt_templates {
            self.merge_paths(&cli_enabled_prompts, &self.inner.additional_prompt_template_paths)
        } else {
            let mut primary = cli_enabled_prompts;
            primary.extend(enabled_prompts);
            self.merge_paths(&primary, &self.inner.additional_prompt_template_paths)
        };
        let mut prompts_res = self.compute_prompts_from_paths(&prompt_paths, &empty_infos, Some(&metadata_by_path));
        for p in &self.inner.additional_prompt_template_paths {
            if is_local_path(p) {
                let resolved = self.resolve_resource_path(p);
                if !exists(&resolved)
                    && !prompts_res
                        .diagnostics
                        .iter()
                        .any(|d| d.path.as_deref() == Some(resolved.as_str()))
                {
                    prompts_res.diagnostics.push(ResourceDiagnostic {
                        r#type: ResourceDiagnosticType::Error,
                        message: "Prompt template path does not exist".to_owned(),
                        path: Some(resolved),
                        collision: None,
                    });
                }
            }
        }

        let theme_paths = if self.inner.no_themes {
            self.merge_paths(&cli_enabled_themes, &self.inner.additional_theme_paths)
        } else {
            let mut primary = cli_enabled_themes;
            primary.extend(enabled_themes);
            self.merge_paths(&primary, &self.inner.additional_theme_paths)
        };
        let mut themes_res = self.compute_themes_from_paths(&theme_paths, &empty_infos, Some(&metadata_by_path));
        for p in &self.inner.additional_theme_paths {
            let resolved = self.resolve_resource_path(p);
            if !exists(&resolved)
                && !themes_res
                    .diagnostics
                    .iter()
                    .any(|d| d.path.as_deref() == Some(resolved.as_str()))
            {
                themes_res.diagnostics.push(ResourceDiagnostic {
                    r#type: ResourceDiagnosticType::Error,
                    message: "Theme path does not exist".to_owned(),
                    path: Some(resolved),
                    collision: None,
                });
            }
        }

        let raw_agents_files = if self.inner.no_context_files {
            Vec::new()
        } else {
            load_project_context_files(LoadProjectContextFilesOptions {
                cwd: self.inner.cwd.clone(),
                agent_dir: self.inner.agent_dir.clone(),
            })
        };
        let resolved_agents_files = match &self.inner.agents_files_override {
            Some(override_fn) => override_fn(raw_agents_files),
            None => raw_agents_files,
        };

        let system_prompt_source = self
            .inner
            .system_prompt_source
            .clone()
            .or_else(|| self.discover_system_prompt_file());
        let base_system_prompt = resolve_prompt_input(system_prompt_source.as_deref(), "system prompt");
        let final_system_prompt = match &self.inner.system_prompt_override {
            Some(override_fn) => override_fn(base_system_prompt),
            None => base_system_prompt,
        };
        let system_prompt_source_path = system_prompt_source
            .filter(|source| exists(source))
            .map(|source| resolve_path(&source, None, PathInputOptions::default()));

        let append_sources = match &self.inner.append_system_prompt_source {
            Some(explicit) => explicit.clone(),
            None => self.discover_append_system_prompt_file().into_iter().collect(),
        };
        let base_append: Vec<String> = append_sources
            .iter()
            .filter_map(|s| resolve_prompt_input(Some(s), "append system prompt"))
            .collect();
        let final_append = match &self.inner.append_system_prompt_override {
            Some(override_fn) => override_fn(base_append),
            None => base_append,
        };
        let append_source_paths: Vec<String> = append_sources
            .into_iter()
            .filter(|source| exists(source))
            .map(|source| resolve_path(&source, None, PathInputOptions::default()))
            .collect();

        let mut guard = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        guard.resource_metadata_by_path = metadata_by_path;
        guard.extension_skill_source_infos.clear();
        guard.extension_prompt_source_infos.clear();
        guard.extension_theme_source_infos.clear();
        guard.extensions_result = final_extensions_result.clone();
        self.inner.extension_host.sync_core_extensions(&final_extensions_result);
        guard.last_skill_paths = skill_paths;
        guard.skills = skills_res.skills;
        guard.skill_diagnostics = skills_res.diagnostics;
        guard.last_prompt_paths = prompt_paths;
        guard.prompts = prompts_res.prompts;
        guard.prompt_diagnostics = prompts_res.diagnostics;
        guard.last_theme_paths = theme_paths;
        guard.themes = themes_res.themes;
        guard.theme_diagnostics = themes_res.diagnostics;
        guard.agents_files = resolved_agents_files;
        guard.system_prompt = final_system_prompt;
        guard.system_prompt_source_path = system_prompt_source_path;
        guard.append_system_prompt = final_append;
        guard.append_system_prompt_source_paths = append_source_paths;
        guard.loaded = true;
        Ok(())
    }

    async fn load_current_extension_set(&self, include_inline_factories: bool) -> Result<LoadExtensionsResult> {
        let resolved_paths = self.inner.package_manager.resolve(None).await?;
        let cli_extension_paths = self
            .inner
            .package_manager
            .resolve_extension_sources(&self.inner.additional_extension_paths, false, true)
            .await?;

        let enabled_extensions: Vec<String> = resolved_paths
            .extensions
            .iter()
            .filter(|r| r.enabled)
            .map(|r| r.path.clone())
            .collect();
        let cli_enabled_extensions: Vec<String> = cli_extension_paths
            .extensions
            .iter()
            .filter(|r| r.enabled)
            .map(|r| r.path.clone())
            .collect();

        let base_paths = if self.inner.no_extensions {
            cli_enabled_extensions
        } else {
            self.merge_paths(&cli_enabled_extensions, &enabled_extensions)
        };
        let extension_paths: Vec<String> = base_paths
            .into_iter()
            .filter(|p| !p.starts_with(BUILTIN_PATH_PREFIX))
            .collect();

        let mut metadata_by_path = IndexMap::new();
        for resource in resolved_paths
            .extensions
            .iter()
            .chain(cli_extension_paths.extensions.iter())
        {
            metadata_by_path
                .entry(resource.path.clone())
                .or_insert_with(|| resource.metadata.clone());
        }

        let package_warnings = collect_extension_package_warnings(&extension_paths, &metadata_by_path)?;
        let install_warnings = collect_package_install_warnings(&[
            resolved_paths.install_failures.as_deref(),
            cli_extension_paths.install_failures.as_deref(),
        ]);
        let mut extensions_result = self
            .inner
            .extension_host
            .load_extensions_cached(
                extension_paths.clone(),
                self.inner.cwd.clone(),
                self.inner.event_bus.clone(),
                None,
            )
            .await;
        let mut combined_warnings = install_warnings;
        combined_warnings.extend(package_warnings);
        merge_extension_warnings(&mut extensions_result, combined_warnings);
        if !include_inline_factories {
            return Ok(extensions_result);
        }

        let (inline_exts, inline_errors) = self.load_extension_factories(&extensions_result.runtime).await;
        extensions_result.extensions.extend(inline_exts);
        extensions_result.errors.extend(inline_errors);
        let mut replacement_warnings = Vec::new();
        extensions_result.extensions =
            omit_replaced_extensions(extensions_result.extensions, &mut replacement_warnings);
        merge_extension_warnings(&mut extensions_result, replacement_warnings);
        Ok(extensions_result)
    }

    fn resolve_extension_load_path(&self, path: &str) -> String {
        if is_synthetic_path(path) {
            path.to_owned()
        } else {
            resolve_path(
                path,
                Some(&self.inner.cwd),
                PathInputOptions {
                    normalize_unicode_spaces: Some(true),
                    ..Default::default()
                },
            )
        }
    }

    fn get_extension_path_key(&self, path: &str) -> String {
        get_resource_path_key(&self.resolve_extension_load_path(path))
    }

    async fn load_extension_paths(&self, paths: &[String], runtime: Option<ExtensionRuntime>) -> LoadExtensionsResult {
        let non_builtin: Vec<String> = paths
            .iter()
            .filter(|p| !p.starts_with(BUILTIN_PATH_PREFIX))
            .cloned()
            .collect();
        let mut result = self
            .inner
            .extension_host
            .load_extensions_cached(
                non_builtin,
                self.inner.cwd.clone(),
                self.inner.event_bus.clone(),
                runtime,
            )
            .await;

        for p in paths.iter().filter(|p| p.starts_with(BUILTIN_PATH_PREFIX)) {
            let builtin_name = &p[BUILTIN_PATH_PREFIX.len()..];
            if let Some(builtin) = self.inner.builtin_core_extensions.get(builtin_name) {
                match self
                    .inner
                    .extension_host
                    .load_core_extension_from_factory(
                        builtin.factory.clone(),
                        self.inner.cwd.clone(),
                        self.inner.event_bus.clone(),
                        p.clone(),
                        true,
                        builtin.replaceable,
                    )
                    .await
                {
                    Ok(mut extension) => {
                        extension.hidden = true;
                        extension.replaceable = builtin.replaceable;
                        result.extensions.push(extension);
                    }
                    Err(error) => {
                        result.errors.push(ExtensionLoadError {
                            path: p.clone(),
                            error: error.to_string(),
                        });
                    }
                }
                continue;
            }
            let Some(builtin) = self.inner.builtin_extensions.get(builtin_name) else {
                result.errors.push(ExtensionLoadError {
                    path: p.clone(),
                    error: format!("Unknown built-in extension: {p}"),
                });
                continue;
            };
            match self
                .inner
                .extension_host
                .load_extension_from_factory(
                    builtin.factory.clone(),
                    self.inner.cwd.clone(),
                    self.inner.event_bus.clone(),
                    result.runtime.clone(),
                    p.clone(),
                )
                .await
            {
                Ok(mut extension) => {
                    extension.hidden = true;
                    extension.replaceable = builtin.replaceable;
                    result.extensions.push(extension);
                }
                Err(error) => {
                    result.errors.push(ExtensionLoadError {
                        path: p.clone(),
                        error: error.to_string(),
                    });
                }
            }
        }
        result
    }

    async fn load_final_extension_set(
        &self,
        extension_paths: &[String],
        pre_trust_extensions: Option<LoadExtensionsResult>,
    ) -> LoadExtensionsResult {
        let preloaded = pre_trust_extensions
            .as_ref()
            .map(|r| r.extensions.as_slice())
            .unwrap_or(&[]);
        let mut preloaded_by_path: IndexMap<String, Extension> = IndexMap::new();
        for ext in preloaded {
            if !ext.path.starts_with("<inline:") {
                preloaded_by_path.insert(get_resource_path_key(&ext.resolved_path), ext.clone());
            }
        }
        let mut failed_preload_paths = IndexSet::new();
        if let Some(ref pre) = pre_trust_extensions {
            for error in &pre.errors {
                failed_preload_paths.insert(self.get_extension_path_key(&error.path));
            }
        }

        let remaining_paths: Vec<String> = extension_paths
            .iter()
            .filter(|p| {
                let path_key = self.get_extension_path_key(p);
                !preloaded_by_path.contains_key(&path_key) && !failed_preload_paths.contains(&path_key)
            })
            .cloned()
            .collect();

        let remaining_extensions = self
            .load_extension_paths(
                &remaining_paths,
                pre_trust_extensions.as_ref().map(|r| r.runtime.clone()),
            )
            .await;

        let mut loaded_by_path = preloaded_by_path;
        for ext in &remaining_extensions.extensions {
            loaded_by_path.insert(get_resource_path_key(&ext.resolved_path), ext.clone());
        }

        let (inline_exts, inline_errors) = if pre_trust_extensions.is_some() {
            let exts = preloaded
                .iter()
                .filter(|ext| ext.path.starts_with("<inline:"))
                .cloned()
                .collect();
            (exts, Vec::new())
        } else {
            self.load_extension_factories(&remaining_extensions.runtime).await
        };

        let mut ordered_extensions: Vec<Extension> = extension_paths
            .iter()
            .filter_map(|p| loaded_by_path.get(&self.get_extension_path_key(p)).cloned())
            .collect();
        ordered_extensions.extend(inline_exts);

        let mut replacement_warnings = Vec::new();
        let mut errors = Vec::new();
        if let Some(ref pre) = pre_trust_extensions {
            errors.extend(pre.errors.clone());
        }
        errors.extend(remaining_extensions.errors);
        errors.extend(inline_errors);

        let mut warnings = Vec::new();
        if let Some(ref pre) = pre_trust_extensions {
            warnings.extend(pre.warnings.clone());
        }
        warnings.extend(remaining_extensions.warnings);

        let mut extensions_result = LoadExtensionsResult {
            extensions: omit_replaced_extensions(ordered_extensions, &mut replacement_warnings),
            errors,
            warnings,
            runtime: remaining_extensions.runtime,
        };
        merge_extension_warnings(&mut extensions_result, replacement_warnings);
        self.add_extension_conflict_diagnostics(&mut extensions_result);
        extensions_result
    }

    fn add_extension_conflict_diagnostics(&self, extensions_result: &mut LoadExtensionsResult) {
        let conflicts = self.detect_extension_conflicts(&extensions_result.extensions);
        for (path, message) in conflicts {
            extensions_result
                .errors
                .push(ExtensionLoadError { path, error: message });
        }
    }

    fn map_skill_path(
        &self,
        resource: &ResolvedResource,
        metadata_by_path: &mut IndexMap<String, PathMetadata>,
    ) -> String {
        if resource.metadata.source != "auto" && resource.metadata.origin != SourceOrigin::Package {
            return resource.path.clone();
        }
        let Ok(stats) = stat(&resource.path) else {
            return resource.path.clone();
        };
        if !stats.is_directory() {
            return resource.path.clone();
        }
        let skill_file = join(&[&resource.path, "SKILL.md"]);
        if exists(&skill_file) {
            metadata_by_path
                .entry(skill_file.clone())
                .or_insert_with(|| resource.metadata.clone());
            return skill_file;
        }
        resource.path.clone()
    }

    fn normalize_extension_paths(&self, entries: &[ExtensionResourcePath]) -> Vec<ExtensionResourcePath> {
        entries
            .iter()
            .map(|entry| {
                let mut metadata = entry.metadata.clone();
                if let Some(ref base_dir) = metadata.base_dir {
                    metadata.base_dir = Some(self.resolve_resource_path(base_dir));
                }
                ExtensionResourcePath {
                    path: self.resolve_resource_path(&entry.path),
                    metadata,
                }
            })
            .collect()
    }

    fn compute_skills_from_paths(
        &self,
        skill_paths: &[String],
        extra_infos: &IndexMap<String, SourceInfo>,
        metadata_by_path: Option<&IndexMap<String, PathMetadata>>,
    ) -> SkillsResult {
        let base_result = if self.inner.no_skills && skill_paths.is_empty() {
            SkillsResult::default()
        } else {
            let loaded = load_skills(LoadSkillsOptions {
                cwd: self.inner.cwd.clone(),
                agent_dir: Some(self.inner.agent_dir.clone()),
                skill_paths: skill_paths.to_vec(),
                include_defaults: false,
            });
            SkillsResult {
                skills: loaded.skills,
                diagnostics: loaded.diagnostics,
            }
        };

        let mut resolved = match &self.inner.skills_override {
            Some(override_fn) => override_fn(base_result),
            None => base_result,
        };
        for skill in &mut resolved.skills {
            if let Some(info) = self.find_source_info_for_path(&skill.file_path, Some(extra_infos), metadata_by_path) {
                skill.source_info = info;
            } else if skill.source_info.path.is_empty() {
                skill.source_info = self.get_default_source_info_for_path(&skill.file_path);
            }
        }
        resolved
    }

    fn compute_prompts_from_paths(
        &self,
        prompt_paths: &[String],
        extra_infos: &IndexMap<String, SourceInfo>,
        metadata_by_path: Option<&IndexMap<String, PathMetadata>>,
    ) -> PromptsResult {
        let base_result = if self.inner.no_prompt_templates && prompt_paths.is_empty() {
            PromptsResult::default()
        } else {
            let loaded = self.inner.prompt_loader.load_prompt_templates(
                &self.inner.cwd,
                &self.inner.agent_dir,
                prompt_paths,
                false,
            );
            let (deduped, collision_diagnostics) = self.dedupe_prompts(loaded.templates);
            let mut diagnostics = loaded.diagnostics;
            diagnostics.extend(collision_diagnostics);
            PromptsResult {
                prompts: deduped,
                diagnostics,
            }
        };

        let mut resolved = match &self.inner.prompts_override {
            Some(override_fn) => override_fn(base_result),
            None => base_result,
        };
        for prompt in &mut resolved.prompts {
            if let Some(info) = self.find_source_info_for_path(&prompt.file_path, Some(extra_infos), metadata_by_path) {
                prompt.source_info = info;
            } else {
                prompt.source_info = self.get_default_source_info_for_path(&prompt.file_path);
            }
        }
        resolved
    }

    fn compute_themes_from_paths(
        &self,
        theme_paths: &[String],
        extra_infos: &IndexMap<String, SourceInfo>,
        metadata_by_path: Option<&IndexMap<String, PathMetadata>>,
    ) -> ThemesResult {
        let base_result = if self.inner.no_themes && theme_paths.is_empty() {
            ThemesResult::default()
        } else {
            let detected = detect_capabilities(Some(&|| false));
            let overrides = self.inner.settings_manager.get_terminal_capability_overrides();
            let merged_caps = TerminalCapabilities {
                images: overrides.images.unwrap_or(detected.images),
                true_color: overrides.true_color.unwrap_or(detected.true_color),
                hyperlinks: overrides.hyperlinks.unwrap_or(detected.hyperlinks),
            };
            let color_mode = get_terminal_color_mode(Some(merged_caps));
            let (loaded_themes, mut loaded_diagnostics) = self.load_themes(theme_paths, false, color_mode);
            let (deduped_themes, collision_diagnostics) = self.dedupe_themes(loaded_themes);
            loaded_diagnostics.extend(collision_diagnostics);
            ThemesResult {
                themes: deduped_themes,
                diagnostics: loaded_diagnostics,
            }
        };

        let mut resolved = match &self.inner.themes_override {
            Some(override_fn) => override_fn(base_result),
            None => base_result,
        };
        for theme in &mut resolved.themes {
            if let Some(ref source_path) = theme.source_path {
                if let Some(info) = self.find_source_info_for_path(source_path, Some(extra_infos), metadata_by_path) {
                    theme.source_info = Some(info);
                } else if theme.source_info.is_none() {
                    theme.source_info = Some(self.get_default_source_info_for_path(source_path));
                }
            }
        }
        resolved
    }

    fn apply_extension_source_info(
        &self,
        extensions: &mut [Extension],
        metadata_by_path: &IndexMap<String, PathMetadata>,
    ) {
        for ext in extensions {
            let source_info = self
                .find_source_info_for_path(&ext.path, None, Some(metadata_by_path))
                .unwrap_or_else(|| self.get_default_source_info_for_path(&ext.path));
            ext.source_info = source_info.clone();
            for cmd in ext.commands.values_mut() {
                cmd.source_info = source_info.clone();
            }
            for tool in ext.tools.values_mut() {
                tool.source_info = source_info.clone();
            }
        }
    }

    fn find_source_info_for_path(
        &self,
        resource_path: &str,
        extra_source_infos: Option<&IndexMap<String, SourceInfo>>,
        metadata_by_path: Option<&IndexMap<String, PathMetadata>>,
    ) -> Option<SourceInfo> {
        if resource_path.is_empty() {
            return None;
        }
        if is_synthetic_path(resource_path) {
            return Some(self.get_default_source_info_for_path(resource_path));
        }

        let normalized_resource_path = resolve(&[resource_path]);
        if let Some(extra) = extra_source_infos {
            for (source_path, source_info) in extra {
                if is_path_within(source_path, &normalized_resource_path) {
                    let mut cloned = source_info.clone();
                    cloned.path = resource_path.to_owned();
                    return Some(cloned);
                }
            }
        }

        if let Some(meta_map) = metadata_by_path {
            if let Some(exact) = meta_map
                .get(&normalized_resource_path)
                .or_else(|| meta_map.get(resource_path))
            {
                return Some(create_source_info(resource_path, &exact.to_source_metadata()));
            }
            for (source_path, metadata) in meta_map {
                if is_path_within(source_path, &normalized_resource_path) {
                    return Some(create_source_info(resource_path, &metadata.to_source_metadata()));
                }
            }
        }

        None
    }

    fn get_default_source_info_for_path(&self, file_path: &str) -> SourceInfo {
        if let Some(synthetic_source) = get_synthetic_path_source(file_path) {
            return SourceInfo {
                path: file_path.to_owned(),
                source: synthetic_source.to_owned(),
                scope: SourceScope::Temporary,
                origin: SourceOrigin::TopLevel,
                base_dir: None,
                changelog_path: None,
            };
        }

        let normalized_path = resolve(&[file_path]);
        let agent_roots = [
            join(&[&self.inner.agent_dir, "skills"]),
            join(&[&self.inner.agent_dir, "prompts"]),
            join(&[&self.inner.agent_dir, "themes"]),
            join(&[&self.inner.agent_dir, "extensions"]),
        ];
        let project_roots = [
            join(&[&self.inner.cwd, *CONFIG_DIR_NAME, "skills"]),
            join(&[&self.inner.cwd, *CONFIG_DIR_NAME, "prompts"]),
            join(&[&self.inner.cwd, *CONFIG_DIR_NAME, "themes"]),
            join(&[&self.inner.cwd, *CONFIG_DIR_NAME, "extensions"]),
        ];

        for root in &agent_roots {
            if self.is_under_path(&normalized_path, root) {
                return SourceInfo {
                    path: file_path.to_owned(),
                    source: "local".to_owned(),
                    scope: SourceScope::User,
                    origin: SourceOrigin::TopLevel,
                    base_dir: Some(root.clone()),
                    changelog_path: None,
                };
            }
        }

        for root in &project_roots {
            if self.is_under_path(&normalized_path, root) {
                return SourceInfo {
                    path: file_path.to_owned(),
                    source: "local".to_owned(),
                    scope: SourceScope::Project,
                    origin: SourceOrigin::TopLevel,
                    base_dir: Some(root.clone()),
                    changelog_path: None,
                };
            }
        }

        let base_dir = if stat(&normalized_path).map(|s| s.is_directory()).unwrap_or(false) {
            normalized_path
        } else {
            resolve(&[&normalized_path, ".."])
        };

        SourceInfo {
            path: file_path.to_owned(),
            source: "local".to_owned(),
            scope: SourceScope::Temporary,
            origin: SourceOrigin::TopLevel,
            base_dir: Some(base_dir),
            changelog_path: None,
        }
    }

    fn merge_paths(&self, primary: &[String], additional: &[String]) -> Vec<String> {
        let mut merged = Vec::new();
        let mut seen = IndexSet::new();

        for p in primary.iter().chain(additional.iter()) {
            let resolved = self.resolve_resource_path(p);
            let path_key = get_resource_path_key(&resolved);
            if !seen.insert(path_key) {
                continue;
            }
            merged.push(resolved);
        }

        merged
    }

    fn resolve_resource_path(&self, p: &str) -> String {
        if is_synthetic_path(p) {
            p.to_owned()
        } else {
            resolve_path(
                p,
                Some(&self.inner.cwd),
                PathInputOptions {
                    trim: Some(true),
                    ..Default::default()
                },
            )
        }
    }

    fn load_themes(
        &self,
        paths: &[String],
        include_defaults: bool,
        color_mode: TerminalColorMode,
    ) -> (Vec<Theme>, Vec<ResourceDiagnostic>) {
        let mut themes = Vec::new();
        let mut diagnostics = Vec::new();

        if include_defaults {
            let default_dirs = [
                join(&[&self.inner.agent_dir, "themes"]),
                join(&[&self.inner.cwd, *CONFIG_DIR_NAME, "themes"]),
            ];
            for dir in &default_dirs {
                self.load_themes_from_dir(dir, &mut themes, &mut diagnostics, color_mode);
            }
        }

        for p in paths {
            let resolved = self.resolve_resource_path(p);
            if !exists(&resolved) {
                diagnostics.push(ResourceDiagnostic {
                    r#type: ResourceDiagnosticType::Warning,
                    message: "theme path does not exist".to_owned(),
                    path: Some(resolved),
                    collision: None,
                });
                continue;
            }

            match stat(&resolved) {
                Ok(stats) => {
                    if stats.is_directory() {
                        self.load_themes_from_dir(&resolved, &mut themes, &mut diagnostics, color_mode);
                    } else if stats.is_file() && resolved.ends_with(".json") {
                        self.load_theme_from_file(&resolved, &mut themes, &mut diagnostics, color_mode);
                    } else {
                        diagnostics.push(ResourceDiagnostic {
                            r#type: ResourceDiagnosticType::Warning,
                            message: "theme path is not a json file".to_owned(),
                            path: Some(resolved),
                            collision: None,
                        });
                    }
                }
                Err(error) => {
                    diagnostics.push(ResourceDiagnostic {
                        r#type: ResourceDiagnosticType::Warning,
                        message: error.to_string(),
                        path: Some(resolved),
                        collision: None,
                    });
                }
            }
        }

        (themes, diagnostics)
    }

    fn load_themes_from_dir(
        &self,
        dir: &str,
        themes: &mut Vec<Theme>,
        diagnostics: &mut Vec<ResourceDiagnostic>,
        color_mode: TerminalColorMode,
    ) {
        if !exists(dir) {
            return;
        }
        match readdir_with_file_types(dir) {
            Ok(entries) => {
                for entry in entries {
                    let full = join(&[dir, &entry.name]);
                    let is_file = if entry.is_symbolic_link() {
                        match stat(&full) {
                            Ok(st) => st.is_file(),
                            Err(_) => continue,
                        }
                    } else {
                        entry.is_file()
                    };
                    if !is_file || !entry.name.ends_with(".json") {
                        continue;
                    }
                    self.load_theme_from_file(&full, themes, diagnostics, color_mode);
                }
            }
            Err(error) => {
                diagnostics.push(ResourceDiagnostic {
                    r#type: ResourceDiagnosticType::Warning,
                    message: error.to_string(),
                    path: Some(dir.to_owned()),
                    collision: None,
                });
            }
        }
    }

    fn load_theme_from_file(
        &self,
        file_path: &str,
        themes: &mut Vec<Theme>,
        diagnostics: &mut Vec<ResourceDiagnostic>,
        color_mode: TerminalColorMode,
    ) {
        match self.inner.theme_loader.load_theme_from_path(file_path, color_mode) {
            Ok(theme) => themes.push(theme),
            Err(error) => diagnostics.push(ResourceDiagnostic {
                r#type: ResourceDiagnosticType::Warning,
                message: error.to_string(),
                path: Some(file_path.to_owned()),
                collision: None,
            }),
        }
    }

    async fn load_extension_factories(&self, runtime: &ExtensionRuntime) -> (Vec<Extension>, Vec<ExtensionLoadError>) {
        let mut extensions = Vec::new();
        let mut errors = Vec::new();

        for (index, input) in self.inner.extension_factories.iter().enumerate() {
            match input {
                InlineExtension::Core(core_ext) => {
                    let (extension_path, factory, hidden, replaceable) = match core_ext {
                        crate::core::extensions::types::InlineExtension::Factory(f) => {
                            (format!("<inline:{}>", index + 1), f.clone(), false, false)
                        }
                        crate::core::extensions::types::InlineExtension::Named(named) => (
                            format!("<inline:{}>", named.name),
                            named.factory.clone(),
                            named.hidden,
                            named.replaceable,
                        ),
                    };
                    match self
                        .inner
                        .extension_host
                        .load_core_extension_from_factory(
                            factory,
                            self.inner.cwd.clone(),
                            self.inner.event_bus.clone(),
                            extension_path.clone(),
                            hidden,
                            replaceable,
                        )
                        .await
                    {
                        Ok(mut extension) => {
                            extension.hidden = hidden;
                            extension.replaceable = replaceable;
                            extensions.push(extension);
                        }
                        Err(error) => {
                            errors.push(ExtensionLoadError {
                                path: extension_path,
                                error: error.to_string(),
                            });
                        }
                    }
                    continue;
                }
                InlineExtension::Factory(_) | InlineExtension::Named(_) => {}
            }
            let (extension_path, factory, hidden, replaceable) = match input {
                InlineExtension::Factory(f) => (format!("<inline:{}>", index + 1), f.clone(), false, false),
                InlineExtension::Named(named) => (
                    format!("<inline:{}>", named.name),
                    named.factory.clone(),
                    named.hidden,
                    named.replaceable,
                ),
                InlineExtension::Core(_) => continue,
            };

            match self
                .inner
                .extension_host
                .load_extension_from_factory(
                    factory,
                    self.inner.cwd.clone(),
                    self.inner.event_bus.clone(),
                    runtime.clone(),
                    extension_path.clone(),
                )
                .await
            {
                Ok(mut extension) => {
                    extension.hidden = hidden;
                    extension.replaceable = replaceable;
                    extensions.push(extension);
                }
                Err(error) => {
                    errors.push(ExtensionLoadError {
                        path: extension_path,
                        error: error.to_string(),
                    });
                }
            }
        }

        (extensions, errors)
    }

    fn dedupe_prompts(&self, prompts: Vec<PromptTemplate>) -> (Vec<PromptTemplate>, Vec<ResourceDiagnostic>) {
        let mut seen: IndexMap<String, PromptTemplate> = IndexMap::new();
        let mut diagnostics = Vec::new();

        for prompt in prompts {
            if let Some(existing) = seen.get(&prompt.name) {
                diagnostics.push(ResourceDiagnostic {
                    r#type: ResourceDiagnosticType::Collision,
                    message: format!("name \"/{}\" collision", prompt.name),
                    path: Some(prompt.file_path.clone()),
                    collision: Some(ResourceCollision {
                        resource_type: ResourceType::Prompt,
                        name: prompt.name.clone(),
                        winner_path: existing.file_path.clone(),
                        loser_path: prompt.file_path.clone(),
                        winner_source: None,
                        loser_source: None,
                    }),
                });
            } else {
                seen.insert(prompt.name.clone(), prompt);
            }
        }

        (seen.into_values().collect(), diagnostics)
    }

    fn dedupe_themes(&self, themes: Vec<Theme>) -> (Vec<Theme>, Vec<ResourceDiagnostic>) {
        let mut seen: IndexMap<String, Theme> = IndexMap::new();
        let mut diagnostics = Vec::new();

        for theme in themes {
            let name = theme.name.as_deref().unwrap_or("unnamed").to_owned();
            if let Some(existing) = seen.get(&name) {
                diagnostics.push(ResourceDiagnostic {
                    r#type: ResourceDiagnosticType::Collision,
                    message: format!("name \"{name}\" collision"),
                    path: theme.source_path.clone(),
                    collision: Some(ResourceCollision {
                        resource_type: ResourceType::Theme,
                        name: name.clone(),
                        winner_path: existing.source_path.clone().unwrap_or_else(|| "<builtin>".to_owned()),
                        loser_path: theme.source_path.clone().unwrap_or_else(|| "<builtin>".to_owned()),
                        winner_source: None,
                        loser_source: None,
                    }),
                });
            } else {
                seen.insert(name, theme);
            }
        }

        (seen.into_values().collect(), diagnostics)
    }

    fn discover_system_prompt_file(&self) -> Option<String> {
        let project_path = join(&[&self.inner.cwd, *CONFIG_DIR_NAME, "SYSTEM.md"]);
        if self.inner.settings_manager.is_project_trusted() && exists(&project_path) {
            return Some(project_path);
        }

        let global_path = join(&[&self.inner.agent_dir, "SYSTEM.md"]);
        if exists(&global_path) {
            return Some(global_path);
        }

        None
    }

    fn discover_append_system_prompt_file(&self) -> Option<String> {
        let project_path = join(&[&self.inner.cwd, *CONFIG_DIR_NAME, "APPEND_SYSTEM.md"]);
        if self.inner.settings_manager.is_project_trusted() && exists(&project_path) {
            return Some(project_path);
        }

        let global_path = join(&[&self.inner.agent_dir, "APPEND_SYSTEM.md"]);
        if exists(&global_path) {
            return Some(global_path);
        }

        None
    }

    fn is_under_path(&self, target: &str, root: &str) -> bool {
        is_path_within(root, target)
    }

    fn detect_extension_conflicts(&self, extensions: &[Extension]) -> Vec<(String, String)> {
        let mut conflicts = Vec::new();
        let mut tool_owners: IndexMap<String, String> = IndexMap::new();
        let mut flag_owners: IndexMap<String, String> = IndexMap::new();

        for ext in extensions {
            for tool_name in ext.tools.keys() {
                if let Some(existing_owner) = tool_owners.get(tool_name) {
                    if existing_owner != &ext.path {
                        conflicts.push((
                            ext.path.clone(),
                            format!("Tool \"{tool_name}\" conflicts with {existing_owner}"),
                        ));
                    }
                } else {
                    tool_owners.insert(tool_name.clone(), ext.path.clone());
                }
            }

            for flag_name in ext.flags.keys() {
                if let Some(existing_owner) = flag_owners.get(flag_name) {
                    if existing_owner != &ext.path {
                        conflicts.push((
                            ext.path.clone(),
                            format!("Flag \"--{flag_name}\" conflicts with {existing_owner}"),
                        ));
                    }
                } else {
                    flag_owners.insert(flag_name.clone(), ext.path.clone());
                }
            }
        }

        conflicts
    }
}

impl ResourceLoader for DefaultResourceLoader {
    fn get_extensions(&self) -> LoadExtensionsResult {
        DefaultResourceLoader::get_extensions(self)
    }

    fn get_core_extensions(&self) -> crate::core::extensions::types::LoadExtensionsResult {
        self.inner.extension_host.loaded_extensions_result().unwrap_or_else(|| {
            crate::core::extensions::types::LoadExtensionsResult {
                extensions: Vec::new(),
                errors: Vec::new(),
                warnings: Vec::new(),
                runtime: crate::core::extensions::loader::create_extension_runtime(),
            }
        })
    }

    fn get_skills(&self) -> (Vec<Skill>, Vec<ResourceDiagnostic>) {
        let res = DefaultResourceLoader::get_skills(self);
        (res.skills, res.diagnostics)
    }

    fn get_prompts(&self) -> (Vec<PromptTemplate>, Vec<ResourceDiagnostic>) {
        let res = DefaultResourceLoader::get_prompts(self);
        (res.prompts, res.diagnostics)
    }

    fn get_themes(&self) -> (Vec<Theme>, Vec<ResourceDiagnostic>) {
        let res = DefaultResourceLoader::get_themes(self);
        (res.themes, res.diagnostics)
    }

    fn get_agents_files(&self) -> Vec<ContextFile> {
        DefaultResourceLoader::get_agents_files(self)
    }

    fn get_system_prompt(&self) -> Option<String> {
        DefaultResourceLoader::get_system_prompt(self)
    }

    fn get_system_prompt_source(&self) -> Option<PromptSource> {
        DefaultResourceLoader::get_system_prompt_source(self)
    }

    fn get_append_system_prompt(&self) -> Vec<String> {
        DefaultResourceLoader::get_append_system_prompt(self)
    }

    fn get_append_system_prompt_sources(&self) -> Vec<PromptSource> {
        DefaultResourceLoader::get_append_system_prompt_sources(self)
    }

    fn extend_resources(&self, paths: ResourceExtensionPaths) {
        DefaultResourceLoader::extend_resources(self, paths);
    }

    fn reload(&self, options: Option<ResourceLoaderReloadOptions>) -> BoxFuture<Result<()>> {
        let me = self.clone();
        Box::pin(async move { DefaultResourceLoader::reload(&me, options).await })
    }
}
