//! Port of packages/coding-agent/src/core/settings-manager.ts

use std::sync::{Arc, Mutex, PoisonError};

use indexmap::{IndexMap, IndexSet};
use pi_ai::{DEFAULT_MAX_AGENT_RETRY_DELAY_MS, ModelThinkingLevel, Transport};
use pi_js::{Error, Result};
use pi_tui::terminal_image::{ImageProtocol, TerminalCapabilityOverrides};
use pi_tui::{ScrollViewScrollbar, WheelScrollLines};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::config::{CONFIG_DIR_NAME, get_agent_dir};
use crate::core::http_dispatcher::{DEFAULT_HTTP_IDLE_TIMEOUT_MS, parse_http_idle_timeout_ms};
use crate::utils::paths::{PathInputOptions, normalize_path, resolve_path};
use crate::utils::text::strip_bom;
use crate::vendor::proper_lockfile::{LockOptions, lock_sync};

pub use pi_tui::TuiMode;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionModelOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserve_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_recent_tokens: Option<i64>,
}

const DEFAULT_COMPACTION_RESERVE_TOKENS: i64 = 16_384;
const DEFAULT_COMPACTION_KEEP_RECENT_TOKENS: i64 = 20_000;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserve_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_recent_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_overrides: Option<IndexMap<String, Value>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummarySettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserve_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_prompt: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRetrySettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retry_delay_ms: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetrySettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_delay_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_agent_delay_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderRetrySettings>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FullscreenExitOutput {
    #[serde(rename = "transcript")]
    Transcript,
    #[serde(rename = "resume-hint")]
    ResumeHint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TerminalImagesSetting {
    Protocol(ImageProtocol),
    Mode(TerminalImagesAuto),
    Bool(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TerminalImagesAuto {
    #[serde(rename = "auto")]
    Auto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TerminalBoolOrAuto {
    Bool(bool),
    Auto(TerminalImagesAuto),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_images: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_width_cells: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clear_on_shrink: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_terminal_progress: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyperlinks: Option<TerminalBoolOrAuto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<TerminalImagesSetting>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub true_color: Option<TerminalBoolOrAuto>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_resize: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_images: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingBudgetsSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimal: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub medium: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MermaidRenderingMode {
    Off,
    Final,
    Streaming,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheWarmingMode {
    Off,
    Streaming,
    Idle,
}

pub const CACHE_WARMING_MODES: &[CacheWarmingMode] = &[
    CacheWarmingMode::Off,
    CacheWarmingMode::Streaming,
    CacheWarmingMode::Idle,
];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_block_indent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mermaid: Option<MermaidRenderingMode>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WarningSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic_extra_usage: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodemodeMode {
    On,
    Only,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<CodemodeMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_budget: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DefaultProjectTrust {
    Ask,
    Always,
    Never,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum QuietStartup {
    Bool(bool),
    Header(QuietStartupHeader),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum QuietStartupHeader {
    #[serde(rename = "header")]
    Header,
}

pub type TransportSetting = Transport;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageFilterSource {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autoload: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompts: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub themes: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PackageSource {
    String(String),
    Object(PackageFilterSource),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum QueueModeSetting {
    #[serde(rename = "all")]
    All,
    #[serde(rename = "one-at-a-time")]
    OneAtATime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DoubleEscapeAction {
    Fork,
    Tree,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TreeFilterMode {
    #[serde(rename = "default")]
    Default,
    #[serde(rename = "no-tools")]
    NoTools,
    #[serde(rename = "user-only")]
    UserOnly,
    #[serde(rename = "labeled-only")]
    LabeledOnly,
    #[serde(rename = "all")]
    All,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_changelog_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_thinking_level: Option<ModelThinkingLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_thinking_levels: Option<IndexMap<String, ModelThinkingLevel>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<TransportSetting>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steering_mode: Option<QueueModeSetting>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up_mode: Option<QueueModeSetting>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactionSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_summary: Option<BranchSummarySettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetrySettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hide_thinking_block: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_cache_miss_notices: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_editor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet_startup: Option<QuietStartup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_project_trust: Option<DefaultProjectTrust>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_command_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub npm_command: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collapse_changelog: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_install_telemetry: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_analytics: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracking_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packages: Option<Vec<PackageSource>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompts: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub themes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_skill_commands: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<TerminalSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<ImageSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_models: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub double_escape_action: Option<DoubleEscapeAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_filter_mode: Option<TreeFilterMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_budgets: Option<ThinkingBudgetsSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor_padding_x: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_pad: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autocomplete_max_visible: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_hardware_cursor: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markdown: Option<MarkdownSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warnings: Option<WarningSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codemode: Option<CodemodeSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_proxy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_idle_timeout_ms: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_warming: Option<CacheWarmingMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub websocket_connect_timeout_ms: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tui_mode: Option<TuiMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fullscreen_exit_output: Option<FullscreenExitOutput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fullscreen_scrollbar: Option<ScrollViewScrollbar>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fullscreen_copy_on_select: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fullscreen_wheel_scroll_lines: Option<WheelScrollLines>,
    #[serde(flatten, default, skip_serializing_if = "IndexMap::is_empty")]
    pub extra: IndexMap<String, Value>,
}

pub const DEFAULT_TOOL_NAMES: &[&str] = &["read", "bash", "edit", "write"];

fn is_tool_modifier(entry: &str) -> bool {
    entry.starts_with('+') || entry.starts_with('-')
}

fn merge_default_tools(base: Option<&Vec<String>>, overrides: Option<&Vec<String>>) -> Option<Vec<String>> {
    let Some(ovr) = overrides else {
        return base.cloned();
    };
    let Some(base_list) = base else {
        return Some(ovr.clone());
    };
    if !ovr.iter().all(|entry| is_tool_modifier(entry)) {
        return Some(ovr.clone());
    }
    let mut merged = base_list.clone();
    merged.extend(ovr.iter().cloned());
    Some(merged)
}

fn resolve_default_tools(entries: &[String]) -> Vec<String> {
    let plain: Vec<String> = entries.iter().filter(|e| !is_tool_modifier(e)).cloned().collect();
    let mut tools = if !plain.is_empty() || entries.is_empty() {
        plain
    } else {
        DEFAULT_TOOL_NAMES.iter().map(|&s| s.to_owned()).collect()
    };
    for entry in entries {
        if !is_tool_modifier(entry) {
            continue;
        }
        let name = &entry[1..];
        let pos = tools.iter().position(|t| t == name);
        if entry.starts_with('+') && pos.is_none() && !name.is_empty() {
            tools.push(name.to_owned());
        } else if entry.starts_with('-')
            && let Some(idx) = pos
        {
            tools.remove(idx);
        }
    }
    tools
}

fn deep_merge_values(base: &Map<String, Value>, overrides: &Map<String, Value>) -> Map<String, Value> {
    let mut result = base.clone();
    for (key, override_val) in overrides {
        if override_val.is_null() && !overrides.contains_key(key) {
            continue;
        }
        match (base.get(key), override_val) {
            (Some(Value::Object(base_obj)), Value::Object(over_obj)) => {
                result.insert(key.clone(), Value::Object(deep_merge_values(base_obj, over_obj)));
            }
            _ => {
                result.insert(key.clone(), override_val.clone());
            }
        }
    }
    result
}

fn deep_merge_settings(base: &Settings, overrides: &Settings) -> Settings {
    let base_map = match serde_json::to_value(base) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    let over_map = match serde_json::to_value(overrides) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    let merged_map = deep_merge_values(&base_map, &over_map);
    let mut merged: Settings = serde_json::from_value(Value::Object(merged_map)).unwrap_or_default();
    merged.default_tools = merge_default_tools(base.default_tools.as_ref(), overrides.default_tools.as_ref());
    merged
}

fn parse_timeout_setting(value: Option<&Value>, setting_name: &str) -> Result<Option<u64>> {
    let Some(val) = value else {
        return Ok(None);
    };
    if let Some(ms) = parse_http_idle_timeout_ms(val) {
        return Ok(Some(ms));
    }
    Err(Error::msg(format!("Invalid {setting_name} setting: {val}")))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SettingsScope {
    Global,
    Project,
}

#[derive(Clone, Debug, Default)]
pub struct SettingsManagerCreateOptions {
    pub project_trusted: Option<bool>,
}

pub trait SettingsStorage: Send + Sync {
    fn with_lock_dyn(
        &self,
        scope: SettingsScope,
        f: &mut dyn FnMut(Option<&str>) -> Result<Option<String>>,
    ) -> Result<()>;
}

#[derive(Clone, Debug)]
pub struct SettingsError {
    pub scope: SettingsScope,
    pub path: Option<String>,
    pub error: String,
}

#[derive(Clone, Debug, Default)]
struct SettingsPaths {
    global: Option<String>,
    project: Option<String>,
}

impl SettingsPaths {
    fn get(&self, scope: SettingsScope) -> Option<&str> {
        match scope {
            SettingsScope::Global => self.global.as_deref(),
            SettingsScope::Project => self.project.as_deref(),
        }
    }
}

fn to_settings_error(scope: SettingsScope, error: &str, path: Option<&str>) -> SettingsError {
    SettingsError {
        scope,
        path: path.map(str::to_owned),
        error: error.to_owned(),
    }
}

pub struct FileSettingsStorage {
    global_settings_path: String,
    project_settings_path: String,
}

impl FileSettingsStorage {
    pub fn new(cwd: &str, agent_dir: &str) -> Self {
        let resolved_cwd = resolve_path(cwd, None, PathInputOptions::default());
        let resolved_agent_dir = resolve_path(agent_dir, None, PathInputOptions::default());
        Self {
            global_settings_path: pi_js::path::join(&[&resolved_agent_dir, "settings.json"]),
            project_settings_path: pi_js::path::join(&[&resolved_cwd, *CONFIG_DIR_NAME, "settings.json"]),
        }
    }

    fn acquire_lock_sync_with_retry(&self, path: &str) -> Result<crate::vendor::proper_lockfile::LockRelease> {
        let max_attempts = 10u32;
        let delay_ms = 20i64;
        for attempt in 1u32.. {
            let opts = LockOptions {
                realpath: Some(false),
                ..Default::default()
            };
            match lock_sync(path, opts) {
                Ok(release) => return Ok(release),
                Err(error) => {
                    if error.code() != Some("ELOCKED") || attempt == max_attempts {
                        return Err(error);
                    }
                    let start = pi_js::time::now_ms();
                    while pi_js::time::now_ms() - start < delay_ms {
                        std::hint::spin_loop();
                    }
                }
            }
        }
        Err(Error::msg("Failed to acquire settings lock"))
    }

    pub fn with_lock<F>(&self, scope: SettingsScope, mut f: F) -> Result<()>
    where
        F: FnMut(Option<&str>) -> Result<Option<String>>,
    {
        self.with_lock_dyn(scope, &mut f)
    }
}

impl SettingsStorage for FileSettingsStorage {
    fn with_lock_dyn(
        &self,
        scope: SettingsScope,
        f: &mut dyn FnMut(Option<&str>) -> Result<Option<String>>,
    ) -> Result<()> {
        let path = match scope {
            SettingsScope::Global => &self.global_settings_path,
            SettingsScope::Project => &self.project_settings_path,
        };
        let dir = pi_js::path::dirname(path);

        let file_exists = pi_js::fs::exists(path);
        let mut release = if file_exists {
            Some(self.acquire_lock_sync_with_retry(path)?)
        } else {
            None
        };

        let result = (|| {
            let current = if file_exists {
                Some(pi_js::fs::read_to_string(path)?)
            } else {
                None
            };
            if let Some(next) = f(current.as_deref())? {
                if !pi_js::fs::exists(&dir) {
                    pi_js::fs::mkdir(&dir, true, None)?;
                }
                if release.is_none() {
                    release = Some(self.acquire_lock_sync_with_retry(path)?);
                }
                pi_js::fs::write_file(path, next, None)?;
            }
            Ok(())
        })();

        if let Some(rel) = release {
            let rel_res = rel.release_sync();
            if result.is_ok() {
                rel_res?;
            }
        }
        result
    }
}

pub struct InMemorySettingsStorage {
    global: Mutex<Option<String>>,
    project: Mutex<Option<String>>,
}

impl Default for InMemorySettingsStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemorySettingsStorage {
    pub fn new() -> Self {
        Self {
            global: Mutex::new(None),
            project: Mutex::new(None),
        }
    }

    pub fn with_lock<F>(&self, scope: SettingsScope, mut f: F) -> Result<()>
    where
        F: FnMut(Option<&str>) -> Result<Option<String>>,
    {
        self.with_lock_dyn(scope, &mut f)
    }
}

impl SettingsStorage for InMemorySettingsStorage {
    fn with_lock_dyn(
        &self,
        scope: SettingsScope,
        f: &mut dyn FnMut(Option<&str>) -> Result<Option<String>>,
    ) -> Result<()> {
        let slot = match scope {
            SettingsScope::Global => &self.global,
            SettingsScope::Project => &self.project,
        };
        let mut guard = slot.lock().unwrap_or_else(PoisonError::into_inner);
        let next = f(guard.as_deref())?;
        if let Some(val) = next {
            *guard = Some(val);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct ModelRef {
    pub provider: String,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedCompactionSettings {
    pub enabled: bool,
    pub reserve_tokens: i64,
    pub keep_recent_tokens: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedBranchSummarySettings {
    pub reserve_tokens: i64,
    pub skip_prompt: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedRetrySettings {
    pub enabled: bool,
    pub max_retries: i64,
    pub base_delay_ms: i64,
    pub max_agent_delay_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedProviderRetrySettings {
    pub timeout_ms: Option<i64>,
    pub max_retries: Option<i64>,
    pub max_retry_delay_ms: i64,
}

struct SettingsManagerState {
    global_settings: Settings,
    project_settings: Settings,
    settings: Settings,
    project_trusted: bool,
    modified_fields: IndexSet<String>,
    modified_nested_fields: IndexMap<String, IndexSet<String>>,
    modified_project_fields: IndexSet<String>,
    modified_project_nested_fields: IndexMap<String, IndexSet<String>>,
    global_settings_load_error: Option<String>,
    project_settings_load_error: Option<String>,
    errors: Vec<SettingsError>,
}

#[derive(Clone)]
pub struct SettingsManager {
    storage: Arc<dyn SettingsStorage>,
    state: Arc<Mutex<SettingsManagerState>>,
    settings_paths: SettingsPaths,
    write_lock: Arc<tokio::sync::Mutex<()>>,
    pending_writes: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

impl SettingsManager {
    fn new_internal(
        storage: Arc<dyn SettingsStorage>,
        initial_global: Settings,
        initial_project: Settings,
        global_load_error: Option<String>,
        project_load_error: Option<String>,
        initial_errors: Vec<SettingsError>,
        project_trusted: bool,
        settings_paths: SettingsPaths,
    ) -> Self {
        let merged = deep_merge_settings(&initial_global, &initial_project);
        Self {
            storage,
            state: Arc::new(Mutex::new(SettingsManagerState {
                global_settings: initial_global,
                project_settings: initial_project,
                settings: merged,
                project_trusted,
                modified_fields: IndexSet::new(),
                modified_nested_fields: IndexMap::new(),
                modified_project_fields: IndexSet::new(),
                modified_project_nested_fields: IndexMap::new(),
                global_settings_load_error: global_load_error,
                project_settings_load_error: project_load_error,
                errors: initial_errors,
            })),
            settings_paths,
            write_lock: Arc::new(tokio::sync::Mutex::new(())),
            pending_writes: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn create(cwd: &str, agent_dir: Option<&str>, options: Option<SettingsManagerCreateOptions>) -> Self {
        let resolved_cwd = resolve_path(cwd, None, PathInputOptions::default());
        let dir_owned;
        let resolved_agent_dir = match agent_dir {
            Some(d) => resolve_path(d, None, PathInputOptions::default()),
            None => {
                dir_owned = get_agent_dir();
                resolve_path(&dir_owned, None, PathInputOptions::default())
            }
        };
        let storage = Arc::new(FileSettingsStorage::new(&resolved_cwd, &resolved_agent_dir));
        let paths = SettingsPaths {
            global: Some(pi_js::path::join(&[&resolved_agent_dir, "settings.json"])),
            project: Some(pi_js::path::join(&[&resolved_cwd, *CONFIG_DIR_NAME, "settings.json"])),
        };
        Self::from_storage_with_paths(storage, options.unwrap_or_default(), paths)
    }

    pub fn from_storage(storage: Arc<dyn SettingsStorage>, options: Option<SettingsManagerCreateOptions>) -> Self {
        Self::from_storage_with_paths(storage, options.unwrap_or_default(), SettingsPaths::default())
    }

    fn from_storage_with_paths(
        storage: Arc<dyn SettingsStorage>,
        options: SettingsManagerCreateOptions,
        settings_paths: SettingsPaths,
    ) -> Self {
        let project_trusted = options.project_trusted.unwrap_or(true);
        let (global_settings, global_error) = Self::try_load_from_storage(&*storage, SettingsScope::Global, true);
        let (project_settings, project_error) =
            Self::try_load_from_storage(&*storage, SettingsScope::Project, project_trusted);
        let mut initial_errors = Vec::new();
        if let Some(ref err) = global_error {
            initial_errors.push(to_settings_error(
                SettingsScope::Global,
                err,
                settings_paths.get(SettingsScope::Global),
            ));
        }
        if let Some(ref err) = project_error {
            initial_errors.push(to_settings_error(
                SettingsScope::Project,
                err,
                settings_paths.get(SettingsScope::Project),
            ));
        }
        Self::new_internal(
            storage,
            global_settings,
            project_settings,
            global_error,
            project_error,
            initial_errors,
            project_trusted,
            settings_paths,
        )
    }

    pub fn in_memory(settings: Option<Settings>, options: Option<SettingsManagerCreateOptions>) -> Self {
        let storage = Arc::new(InMemorySettingsStorage::new());
        let initial = settings.unwrap_or_default();
        if let Ok(Value::Object(map)) = serde_json::to_value(&initial) {
            let migrated = Self::migrate_settings_map(map);
            if let Ok(json_str) = pi_js::json::try_stringify_pretty(&migrated, "  ") {
                let _ = storage.with_lock(SettingsScope::Global, |_| Ok(Some(json_str.clone())));
            }
        }
        Self::from_storage(storage, options)
    }

    fn load_from_storage(
        storage: &dyn SettingsStorage,
        scope: SettingsScope,
        project_trusted: bool,
    ) -> Result<Settings> {
        if scope == SettingsScope::Project && !project_trusted {
            return Ok(Settings::default());
        }
        let mut content: Option<String> = None;
        storage.with_lock_dyn(scope, &mut |current| {
            content = current.map(str::to_owned);
            Ok(None)
        })?;
        let Some(raw) = content else {
            return Ok(Settings::default());
        };
        if raw.is_empty() {
            return Ok(Settings::default());
        }
        let parsed = pi_js::json::parse(strip_bom(&raw))?;
        let Some(map) = parsed.as_object() else {
            return Ok(Settings::default());
        };
        Self::migrate_settings_map_to_settings(map.clone())
    }

    fn try_load_from_storage(
        storage: &dyn SettingsStorage,
        scope: SettingsScope,
        project_trusted: bool,
    ) -> (Settings, Option<String>) {
        match Self::load_from_storage(storage, scope, project_trusted) {
            Ok(settings) => (settings, None),
            Err(err) => (Settings::default(), Some(err.message().to_owned())),
        }
    }

    pub fn migrate_settings_map(mut settings: Map<String, Value>) -> Map<String, Value> {
        if let Some(queue_mode) = settings.get("queueMode").cloned()
            && !settings.contains_key("steeringMode")
        {
            settings.insert("steeringMode".to_owned(), queue_mode);
            settings.remove("queueMode");
        }

        if !settings.contains_key("transport")
            && let Some(Value::Bool(websockets)) = settings.get("websockets")
        {
            settings.insert(
                "transport".to_owned(),
                Value::String(if *websockets { "websocket" } else { "sse" }.to_owned()),
            );
            settings.remove("websockets");
        }

        if let Some(Value::Object(skills_obj)) = settings.get("skills").cloned() {
            if let Some(enable_cmd) = skills_obj.get("enableSkillCommands").cloned()
                && !settings.contains_key("enableSkillCommands")
            {
                settings.insert("enableSkillCommands".to_owned(), enable_cmd);
            }
            match skills_obj.get("customDirectories") {
                Some(Value::Array(dirs)) if !dirs.is_empty() => {
                    settings.insert("skills".to_owned(), Value::Array(dirs.clone()));
                }
                _ => {
                    settings.remove("skills");
                }
            }
        }

        if let Some(Value::Object(mut retry_obj)) = settings.get("retry").cloned() {
            let max_delay_ms = retry_obj.get("maxDelayMs").and_then(Value::as_f64);
            let provider_obj = match retry_obj.get("provider") {
                Some(Value::Object(m)) => Some(m.clone()),
                _ => None,
            };
            let missing_provider_delay = provider_obj
                .as_ref()
                .and_then(|p| p.get("maxRetryDelayMs"))
                .is_none_or(Value::is_null);
            if let Some(delay) = max_delay_ms
                && missing_provider_delay
            {
                let mut next_provider = provider_obj.unwrap_or_default();
                if let Some(num) = serde_json::Number::from_f64(delay) {
                    next_provider.insert("maxRetryDelayMs".to_owned(), Value::Number(num));
                }
                retry_obj.insert("provider".to_owned(), Value::Object(next_provider));
            }
            retry_obj.remove("maxDelayMs");
            settings.insert("retry".to_owned(), Value::Object(retry_obj));
        }

        settings
    }

    fn migrate_settings_map_to_settings(map: Map<String, Value>) -> Result<Settings> {
        let migrated = Self::migrate_settings_map(map);
        serde_json::from_value(Value::Object(migrated)).map_err(|err| Error::msg(err.to_string()))
    }

    pub fn get_settings(&self) -> Settings {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .settings
            .clone()
    }

    pub fn get_global_settings(&self) -> Settings {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .global_settings
            .clone()
    }

    pub fn get_project_settings(&self) -> Settings {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .project_settings
            .clone()
    }

    pub fn is_project_trusted(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .project_trusted
    }

    pub fn set_project_trusted(&self, trusted: bool) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if st.project_trusted == trusted {
            return;
        }
        st.project_trusted = trusted;
        st.modified_project_fields.clear();
        st.modified_project_nested_fields.clear();

        if !trusted {
            st.project_settings = Settings::default();
            st.project_settings_load_error = None;
            st.settings = deep_merge_settings(&st.global_settings, &st.project_settings);
            return;
        }

        let (project_settings, project_err) =
            Self::try_load_from_storage(&*self.storage, SettingsScope::Project, trusted);
        st.project_settings = project_settings;
        st.project_settings_load_error = project_err.clone();
        if let Some(ref err) = project_err {
            st.errors.push(to_settings_error(
                SettingsScope::Project,
                err,
                self.settings_paths.get(SettingsScope::Project),
            ));
        }
        st.settings = deep_merge_settings(&st.global_settings, &st.project_settings);
    }

    pub async fn reload(&self) {
        self.flush().await;
        let trusted = self.is_project_trusted();
        let global_load = Self::try_load_from_storage(&*self.storage, SettingsScope::Global, true);
        let project_load = Self::try_load_from_storage(&*self.storage, SettingsScope::Project, trusted);

        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(ref err) = global_load.1 {
            st.global_settings_load_error = Some(err.clone());
            st.errors.push(to_settings_error(
                SettingsScope::Global,
                err,
                self.settings_paths.get(SettingsScope::Global),
            ));
        } else {
            st.global_settings = global_load.0;
            st.global_settings_load_error = None;
        }

        st.modified_fields.clear();
        st.modified_nested_fields.clear();
        st.modified_project_fields.clear();
        st.modified_project_nested_fields.clear();

        if let Some(ref err) = project_load.1 {
            st.project_settings_load_error = Some(err.clone());
            st.errors.push(to_settings_error(
                SettingsScope::Project,
                err,
                self.settings_paths.get(SettingsScope::Project),
            ));
        } else {
            st.project_settings = project_load.0;
            st.project_settings_load_error = None;
        }

        st.settings = deep_merge_settings(&st.global_settings, &st.project_settings);
    }

    pub fn apply_overrides(&self, overrides: &Settings) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.settings = deep_merge_settings(&st.settings, overrides);
    }

    fn mark_modified_locked(st: &mut SettingsManagerState, field: &str, nested_key: Option<&str>) {
        st.modified_fields.insert(field.to_owned());
        if let Some(nk) = nested_key {
            st.modified_nested_fields
                .entry(field.to_owned())
                .or_default()
                .insert(nk.to_owned());
        }
    }

    fn mark_project_modified_locked(st: &mut SettingsManagerState, field: &str, nested_key: Option<&str>) {
        st.modified_project_fields.insert(field.to_owned());
        if let Some(nk) = nested_key {
            st.modified_project_nested_fields
                .entry(field.to_owned())
                .or_default()
                .insert(nk.to_owned());
        }
    }

    fn persist_scoped_settings(
        storage: &dyn SettingsStorage,
        scope: SettingsScope,
        snapshot_settings: &Settings,
        modified_fields: &IndexSet<String>,
        modified_nested_fields: &IndexMap<String, IndexSet<String>>,
    ) -> Result<()> {
        let snapshot_map = match serde_json::to_value(snapshot_settings) {
            Ok(Value::Object(m)) => m,
            _ => Map::new(),
        };
        storage.with_lock_dyn(scope, &mut |current| {
            let current_file_map = if let Some(raw) = current
                && !raw.is_empty()
            {
                let parsed = pi_js::json::parse(strip_bom(raw))?;
                let obj = parsed.as_object().cloned().unwrap_or_default();
                Self::migrate_settings_map(obj)
            } else {
                Map::new()
            };

            let mut merged_map = current_file_map.clone();
            for field in modified_fields {
                if let Some(nested_modified) = modified_nested_fields.get(field)
                    && let Some(Value::Object(in_memory_nested)) = snapshot_map.get(field)
                {
                    let mut merged_nested = match current_file_map.get(field) {
                        Some(Value::Object(m)) => m.clone(),
                        _ => Map::new(),
                    };
                    for nk in nested_modified {
                        if let Some(nv) = in_memory_nested.get(nk) {
                            merged_nested.insert(nk.clone(), nv.clone());
                        } else {
                            merged_nested.remove(nk);
                        }
                    }
                    merged_map.insert(field.clone(), Value::Object(merged_nested));
                } else if let Some(val) = snapshot_map.get(field) {
                    merged_map.insert(field.clone(), val.clone());
                } else {
                    merged_map.remove(field);
                }
            }

            Ok(Some(pi_js::json::try_stringify_pretty(&merged_map, "  ")?))
        })
    }

    fn enqueue_write(
        &self,
        scope: SettingsScope,
        snapshot: Settings,
        modified_fields: IndexSet<String>,
        modified_nested_fields: IndexMap<String, IndexSet<String>>,
    ) {
        let storage = self.storage.clone();
        let state = self.state.clone();
        let paths = self.settings_paths.clone();
        let write_lock = self.write_lock.clone();

        let execute = move || {
            let res: Result<()> = (|| {
                if scope == SettingsScope::Project {
                    let trusted = state.lock().unwrap_or_else(PoisonError::into_inner).project_trusted;
                    if !trusted {
                        return Err(Error::msg("Project is not trusted; refusing to write project settings"));
                    }
                }
                Self::persist_scoped_settings(&*storage, scope, &snapshot, &modified_fields, &modified_nested_fields)?;
                let mut st = state.lock().unwrap_or_else(PoisonError::into_inner);
                if scope == SettingsScope::Global {
                    st.modified_fields.clear();
                    st.modified_nested_fields.clear();
                } else {
                    st.modified_project_fields.clear();
                    st.modified_project_nested_fields.clear();
                }
                Ok(())
            })();

            if let Err(err) = res {
                let mut st = state.lock().unwrap_or_else(PoisonError::into_inner);
                st.errors
                    .push(to_settings_error(scope, &err.message(), paths.get(scope)));
            }
        };

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let jh = handle.spawn(async move {
                let _guard = write_lock.lock().await;
                execute();
            });
            let mut pending = self.pending_writes.lock().unwrap_or_else(PoisonError::into_inner);
            pending.retain(|h| !h.is_finished());
            pending.push(jh);
        } else {
            execute();
        }
    }

    fn save_locked(&self, st: &mut SettingsManagerState) {
        st.settings = deep_merge_settings(&st.global_settings, &st.project_settings);
        if st.global_settings_load_error.is_some() {
            return;
        }
        let snapshot = st.global_settings.clone();
        let modified = st.modified_fields.clone();
        let nested = st.modified_nested_fields.clone();
        self.enqueue_write(SettingsScope::Global, snapshot, modified, nested);
    }

    fn save_project_locked(&self, st: &mut SettingsManagerState, settings: Settings) -> Result<()> {
        if !st.project_trusted {
            return Err(Error::msg("Project is not trusted; refusing to write project settings"));
        }
        st.project_settings = settings;
        st.settings = deep_merge_settings(&st.global_settings, &st.project_settings);
        if st.project_settings_load_error.is_some() {
            return Ok(());
        }
        let snapshot = st.project_settings.clone();
        let modified = st.modified_project_fields.clone();
        let nested = st.modified_project_nested_fields.clone();
        self.enqueue_write(SettingsScope::Project, snapshot, modified, nested);
        Ok(())
    }

    fn update_project_settings(&self, field: &str, update: impl FnOnce(&mut Settings)) -> Result<()> {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !st.project_trusted {
            return Err(Error::msg("Project is not trusted; refusing to write project settings"));
        }
        let mut project_settings = st.project_settings.clone();
        update(&mut project_settings);
        Self::mark_project_modified_locked(&mut st, field, None);
        self.save_project_locked(&mut st, project_settings)
    }

    pub async fn flush(&self) {
        loop {
            let handles = {
                let mut pending = self.pending_writes.lock().unwrap_or_else(PoisonError::into_inner);
                std::mem::take(&mut *pending)
            };
            if handles.is_empty() {
                break;
            }
            for h in handles {
                let _ = h.await;
            }
        }
    }

    pub fn drain_errors(&self) -> Vec<SettingsError> {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        std::mem::take(&mut st.errors)
    }

    pub fn get_last_changelog_version(&self) -> Option<String> {
        self.get_settings().last_changelog_version
    }

    pub fn set_last_changelog_version(&self, version: &str) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.last_changelog_version = Some(version.to_owned());
        Self::mark_modified_locked(&mut st, "lastChangelogVersion", None);
        self.save_locked(&mut st);
    }

    pub fn get_session_dir(&self) -> Option<String> {
        self.get_settings()
            .session_dir
            .map(|d| normalize_path(&d, Default::default()))
    }

    pub fn get_default_provider(&self) -> Option<String> {
        self.get_settings().default_provider
    }

    pub fn get_default_model(&self) -> Option<String> {
        self.get_settings().default_model
    }

    pub fn set_default_provider(&self, provider: &str) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.default_provider = Some(provider.to_owned());
        Self::mark_modified_locked(&mut st, "defaultProvider", None);
        self.save_locked(&mut st);
    }

    pub fn set_default_model(&self, model_id: &str) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.default_model = Some(model_id.to_owned());
        Self::mark_modified_locked(&mut st, "defaultModel", None);
        self.save_locked(&mut st);
    }

    pub fn set_default_model_and_provider(&self, provider: &str, model_id: &str) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.default_provider = Some(provider.to_owned());
        st.global_settings.default_model = Some(model_id.to_owned());
        Self::mark_modified_locked(&mut st, "defaultProvider", None);
        Self::mark_modified_locked(&mut st, "defaultModel", None);
        self.save_locked(&mut st);
    }

    pub fn get_steering_mode(&self) -> QueueModeSetting {
        self.get_settings()
            .steering_mode
            .unwrap_or(QueueModeSetting::OneAtATime)
    }

    pub fn set_steering_mode(&self, mode: QueueModeSetting) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.steering_mode = Some(mode);
        Self::mark_modified_locked(&mut st, "steeringMode", None);
        self.save_locked(&mut st);
    }

    pub fn get_follow_up_mode(&self) -> QueueModeSetting {
        self.get_settings()
            .follow_up_mode
            .unwrap_or(QueueModeSetting::OneAtATime)
    }

    pub fn set_follow_up_mode(&self, mode: QueueModeSetting) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.follow_up_mode = Some(mode);
        Self::mark_modified_locked(&mut st, "followUpMode", None);
        self.save_locked(&mut st);
    }

    pub fn get_theme_setting(&self) -> Option<String> {
        self.get_settings().theme
    }

    pub fn get_theme(&self) -> Option<String> {
        let theme = self.get_theme_setting()?;
        if theme.contains('/') { None } else { Some(theme) }
    }

    pub fn set_theme(&self, theme: &str) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.theme = Some(theme.to_owned());
        Self::mark_modified_locked(&mut st, "theme", None);
        self.save_locked(&mut st);
    }

    pub fn get_default_thinking_level(&self) -> Option<ModelThinkingLevel> {
        self.get_settings().default_thinking_level
    }

    pub fn set_default_thinking_level(&self, level: ModelThinkingLevel) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.default_thinking_level = Some(level);
        Self::mark_modified_locked(&mut st, "defaultThinkingLevel", None);
        self.save_locked(&mut st);
    }

    pub fn get_model_thinking_level(&self, provider: &str, model_id: &str) -> Option<ModelThinkingLevel> {
        let key = format!("{provider}/{model_id}");
        self.get_settings()
            .model_thinking_levels
            .and_then(|map| map.get(&key).copied())
    }

    pub fn get_all_model_thinking_levels(&self) -> IndexMap<String, ModelThinkingLevel> {
        self.get_settings().model_thinking_levels.unwrap_or_default()
    }

    pub fn set_model_thinking_level(&self, provider: &str, model_id: &str, level: ModelThinkingLevel) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let levels = st
            .global_settings
            .model_thinking_levels
            .get_or_insert_with(IndexMap::new);
        levels.insert(format!("{provider}/{model_id}"), level);
        Self::mark_modified_locked(&mut st, "modelThinkingLevels", None);
        self.save_locked(&mut st);
    }

    pub fn remove_model_thinking_level(&self, provider: &str, model_id: &str) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(ref mut levels) = st.global_settings.model_thinking_levels else {
            return;
        };
        levels.shift_remove(&format!("{provider}/{model_id}"));
        if levels.is_empty() {
            st.global_settings.model_thinking_levels = None;
        }
        Self::mark_modified_locked(&mut st, "modelThinkingLevels", None);
        self.save_locked(&mut st);
    }

    pub fn get_transport(&self) -> TransportSetting {
        self.get_settings().transport.unwrap_or(Transport::Auto)
    }

    pub fn set_transport(&self, transport: TransportSetting) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.transport = Some(transport);
        Self::mark_modified_locked(&mut st, "transport", None);
        self.save_locked(&mut st);
    }

    pub fn get_compaction_enabled(&self) -> bool {
        self.get_settings().compaction.and_then(|c| c.enabled).unwrap_or(true)
    }

    pub fn set_compaction_enabled(&self, enabled: bool) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let comp = st
            .global_settings
            .compaction
            .get_or_insert_with(CompactionSettings::default);
        comp.enabled = Some(enabled);
        Self::mark_modified_locked(&mut st, "compaction", Some("enabled"));
        self.save_locked(&mut st);
    }

    fn get_compaction_token_setting(&self, field: &str, model: Option<&ModelRef>) -> Result<i64> {
        let settings = self.get_settings();
        let compaction = settings.compaction.as_ref();
        let (ordinary, default_val) = if field == "reserveTokens" {
            (
                compaction.and_then(|c| c.reserve_tokens),
                DEFAULT_COMPACTION_RESERVE_TOKENS,
            )
        } else {
            (
                compaction.and_then(|c| c.keep_recent_tokens),
                DEFAULT_COMPACTION_KEEP_RECENT_TOKENS,
            )
        };
        if let Some(val) = ordinary
            && val < 0
        {
            return Err(Error::msg(format!(
                "Invalid compaction.{field} setting: {val}. Expected a non-negative safe integer."
            )));
        }

        let mut override_val: Option<i64> = None;
        if let Some(m) = model {
            let model_key = format!("{}/{}", m.provider, m.id);
            if let Some(entry_val) = compaction
                .and_then(|c| c.model_overrides.as_ref())
                .and_then(|ovr| ovr.get(&model_key))
            {
                let Some(entry_obj) = entry_val.as_object() else {
                    return Err(Error::msg(format!(
                        "Invalid compaction.modelOverrides[\"{model_key}\"] setting: {entry_val}. Expected an object."
                    )));
                };
                if let Some(raw_override) = entry_obj.get(field)
                    && !raw_override.is_null()
                {
                    let Some(num) = raw_override
                        .as_f64()
                        .filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0 && *n <= 9_007_199_254_740_991.0)
                    else {
                        return Err(Error::msg(format!(
                            "Invalid compaction.modelOverrides[\"{model_key}\"].{field} setting: {raw_override}. Expected a non-negative safe integer."
                        )));
                    };
                    override_val = Some(num as i64);
                }
            }
        }

        Ok(override_val.or(ordinary).unwrap_or(default_val))
    }

    pub fn get_compaction_reserve_tokens(&self, model: Option<&ModelRef>) -> Result<i64> {
        self.get_compaction_token_setting("reserveTokens", model)
    }

    pub fn get_compaction_keep_recent_tokens(&self, model: Option<&ModelRef>) -> Result<i64> {
        self.get_compaction_token_setting("keepRecentTokens", model)
    }

    pub fn get_compaction_settings(&self, model: Option<&ModelRef>) -> Result<ResolvedCompactionSettings> {
        Ok(ResolvedCompactionSettings {
            enabled: self.get_compaction_enabled(),
            reserve_tokens: self.get_compaction_reserve_tokens(model)?,
            keep_recent_tokens: self.get_compaction_keep_recent_tokens(model)?,
        })
    }

    pub fn get_branch_summary_settings(&self) -> ResolvedBranchSummarySettings {
        let bs = self.get_settings().branch_summary.unwrap_or_default();
        ResolvedBranchSummarySettings {
            reserve_tokens: bs.reserve_tokens.unwrap_or(16_384),
            skip_prompt: bs.skip_prompt.unwrap_or(false),
        }
    }

    pub fn get_branch_summary_skip_prompt(&self) -> bool {
        self.get_settings()
            .branch_summary
            .and_then(|b| b.skip_prompt)
            .unwrap_or(false)
    }

    pub fn get_retry_enabled(&self) -> bool {
        self.get_settings().retry.and_then(|r| r.enabled).unwrap_or(true)
    }

    pub fn set_retry_enabled(&self, enabled: bool) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let retry = st.global_settings.retry.get_or_insert_with(RetrySettings::default);
        retry.enabled = Some(enabled);
        Self::mark_modified_locked(&mut st, "retry", Some("enabled"));
        self.save_locked(&mut st);
    }

    pub fn get_retry_settings(&self) -> ResolvedRetrySettings {
        let settings = self.get_settings();
        let retry = settings.retry.unwrap_or_default();
        ResolvedRetrySettings {
            enabled: retry.enabled.unwrap_or(true),
            max_retries: retry.max_retries.unwrap_or(3),
            base_delay_ms: retry.base_delay_ms.unwrap_or(2000),
            max_agent_delay_ms: retry
                .max_agent_delay_ms
                .unwrap_or(DEFAULT_MAX_AGENT_RETRY_DELAY_MS as i64),
        }
    }

    pub fn get_http_idle_timeout_ms(&self) -> Result<u64> {
        Ok(
            parse_timeout_setting(self.get_settings().http_idle_timeout_ms.as_ref(), "httpIdleTimeoutMs")?
                .unwrap_or(DEFAULT_HTTP_IDLE_TIMEOUT_MS),
        )
    }

    pub fn set_http_idle_timeout_ms(&self, timeout_ms: f64) -> Result<()> {
        if !timeout_ms.is_finite() || timeout_ms < 0.0 {
            return Err(Error::msg(format!("Invalid httpIdleTimeoutMs setting: {timeout_ms}")));
        }
        let val = timeout_ms.floor() as u64;
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.http_idle_timeout_ms = Some(Value::Number(serde_json::Number::from(val)));
        Self::mark_modified_locked(&mut st, "httpIdleTimeoutMs", None);
        self.save_locked(&mut st);
        Ok(())
    }

    pub fn get_cache_warming_mode(&self) -> CacheWarmingMode {
        self.get_global_settings()
            .cache_warming
            .unwrap_or(CacheWarmingMode::Streaming)
    }

    pub fn set_cache_warming_mode(&self, mode: CacheWarmingMode) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.cache_warming = Some(mode);
        Self::mark_modified_locked(&mut st, "cacheWarming", None);
        self.save_locked(&mut st);
    }

    pub fn get_provider_retry_settings(&self) -> ResolvedProviderRetrySettings {
        let prov = self.get_settings().retry.and_then(|r| r.provider).unwrap_or_default();
        ResolvedProviderRetrySettings {
            timeout_ms: prov.timeout_ms,
            max_retries: prov.max_retries,
            max_retry_delay_ms: prov.max_retry_delay_ms.unwrap_or(60_000),
        }
    }

    pub fn get_web_socket_connect_timeout_ms(&self) -> Result<Option<u64>> {
        parse_timeout_setting(
            self.get_settings().websocket_connect_timeout_ms.as_ref(),
            "websocketConnectTimeoutMs",
        )
    }

    pub fn get_hide_thinking_block(&self) -> bool {
        self.get_settings().hide_thinking_block.unwrap_or(false)
    }

    pub fn get_show_cache_miss_notices(&self) -> bool {
        self.get_settings().show_cache_miss_notices.unwrap_or(false)
    }

    pub fn get_external_editor_command(&self) -> String {
        if let Some(configured) = self.get_settings().external_editor
            && !pi_js::str16::trim(&configured).is_empty()
        {
            return configured;
        }
        if let Some(visual) = pi_js::env::var("VISUAL").filter(|s| !s.is_empty()) {
            return visual;
        }
        if let Some(editor) = pi_js::env::var("EDITOR").filter(|s| !s.is_empty()) {
            return editor;
        }
        if pi_js::env::platform() == "win32" {
            "notepad".to_owned()
        } else {
            "nano".to_owned()
        }
    }

    pub fn set_hide_thinking_block(&self, hide: bool) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.hide_thinking_block = Some(hide);
        Self::mark_modified_locked(&mut st, "hideThinkingBlock", None);
        self.save_locked(&mut st);
    }

    pub fn set_show_cache_miss_notices(&self, show: bool) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.show_cache_miss_notices = Some(show);
        Self::mark_modified_locked(&mut st, "showCacheMissNotices", None);
        self.save_locked(&mut st);
    }

    pub fn get_shell_path(&self) -> Option<String> {
        self.get_settings()
            .shell_path
            .map(|p| normalize_path(&p, Default::default()))
    }

    pub fn set_shell_path(&self, path: Option<String>) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.shell_path = path;
        Self::mark_modified_locked(&mut st, "shellPath", None);
        self.save_locked(&mut st);
    }

    pub fn get_quiet_startup(&self) -> QuietStartup {
        self.get_settings().quiet_startup.unwrap_or(QuietStartup::Bool(false))
    }

    pub fn set_quiet_startup(&self, quiet: QuietStartup) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.quiet_startup = Some(quiet);
        Self::mark_modified_locked(&mut st, "quietStartup", None);
        self.save_locked(&mut st);
    }

    pub fn get_default_project_trust(&self) -> DefaultProjectTrust {
        self.get_global_settings()
            .default_project_trust
            .unwrap_or(DefaultProjectTrust::Ask)
    }

    pub fn set_default_project_trust(&self, default_project_trust: DefaultProjectTrust) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.default_project_trust = Some(default_project_trust);
        Self::mark_modified_locked(&mut st, "defaultProjectTrust", None);
        self.save_locked(&mut st);
    }

    pub fn get_shell_command_prefix(&self) -> Option<String> {
        self.get_settings().shell_command_prefix
    }

    pub fn set_shell_command_prefix(&self, prefix: Option<String>) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.shell_command_prefix = prefix;
        Self::mark_modified_locked(&mut st, "shellCommandPrefix", None);
        self.save_locked(&mut st);
    }

    pub fn get_npm_command(&self) -> Option<Vec<String>> {
        self.get_settings().npm_command
    }

    pub fn set_npm_command(&self, command: Option<Vec<String>>) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.npm_command = command;
        Self::mark_modified_locked(&mut st, "npmCommand", None);
        self.save_locked(&mut st);
    }

    pub fn get_collapse_changelog(&self) -> bool {
        self.get_settings().collapse_changelog.unwrap_or(false)
    }

    pub fn set_collapse_changelog(&self, collapse: bool) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.collapse_changelog = Some(collapse);
        Self::mark_modified_locked(&mut st, "collapseChangelog", None);
        self.save_locked(&mut st);
    }

    pub fn get_enable_install_telemetry(&self) -> bool {
        self.get_settings().enable_install_telemetry.unwrap_or(true)
    }

    pub fn set_enable_install_telemetry(&self, enabled: bool) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.enable_install_telemetry = Some(enabled);
        Self::mark_modified_locked(&mut st, "enableInstallTelemetry", None);
        self.save_locked(&mut st);
    }

    pub fn get_enable_analytics(&self) -> bool {
        self.get_settings().enable_analytics.unwrap_or(false)
    }

    pub fn get_tracking_id(&self) -> Option<String> {
        self.get_settings().tracking_id
    }

    pub fn set_enable_analytics(&self, enabled: bool) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.enable_analytics = Some(enabled);
        Self::mark_modified_locked(&mut st, "enableAnalytics", None);
        if enabled && st.global_settings.tracking_id.is_none() {
            st.global_settings.tracking_id = Some(pi_js::crypto::random_uuid());
            Self::mark_modified_locked(&mut st, "trackingId", None);
        }
        self.save_locked(&mut st);
    }

    pub fn get_or_create_device_id(&self) -> String {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(ref existing) = st.global_settings.device_id {
            return existing.clone();
        }
        let id = pi_js::crypto::random_uuid();
        st.global_settings.device_id = Some(id.clone());
        Self::mark_modified_locked(&mut st, "deviceId", None);
        self.save_locked(&mut st);
        id
    }

    pub fn get_packages(&self) -> Vec<PackageSource> {
        self.get_settings().packages.unwrap_or_default()
    }

    pub fn set_packages(&self, packages: Vec<PackageSource>) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.packages = Some(packages);
        Self::mark_modified_locked(&mut st, "packages", None);
        self.save_locked(&mut st);
    }

    pub fn set_project_packages(&self, packages: Vec<PackageSource>) -> Result<()> {
        self.update_project_settings("packages", |s| s.packages = Some(packages))
    }

    pub fn get_extension_paths(&self) -> Vec<String> {
        self.get_settings().extensions.unwrap_or_default()
    }

    pub fn set_extension_paths(&self, paths: Vec<String>) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.extensions = Some(paths);
        Self::mark_modified_locked(&mut st, "extensions", None);
        self.save_locked(&mut st);
    }

    pub fn set_project_extension_paths(&self, paths: Vec<String>) -> Result<()> {
        self.update_project_settings("extensions", |s| s.extensions = Some(paths))
    }

    pub fn get_skill_paths(&self) -> Vec<String> {
        self.get_settings().skills.unwrap_or_default()
    }

    pub fn set_skill_paths(&self, paths: Vec<String>) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.skills = Some(paths);
        Self::mark_modified_locked(&mut st, "skills", None);
        self.save_locked(&mut st);
    }

    pub fn set_project_skill_paths(&self, paths: Vec<String>) -> Result<()> {
        self.update_project_settings("skills", |s| s.skills = Some(paths))
    }

    pub fn get_prompt_template_paths(&self) -> Vec<String> {
        self.get_settings().prompts.unwrap_or_default()
    }

    pub fn set_prompt_template_paths(&self, paths: Vec<String>) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.prompts = Some(paths);
        Self::mark_modified_locked(&mut st, "prompts", None);
        self.save_locked(&mut st);
    }

    pub fn set_project_prompt_template_paths(&self, paths: Vec<String>) -> Result<()> {
        self.update_project_settings("prompts", |s| s.prompts = Some(paths))
    }

    pub fn get_theme_paths(&self) -> Vec<String> {
        self.get_settings().themes.unwrap_or_default()
    }

    pub fn set_theme_paths(&self, paths: Vec<String>) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.themes = Some(paths);
        Self::mark_modified_locked(&mut st, "themes", None);
        self.save_locked(&mut st);
    }

    pub fn set_project_theme_paths(&self, paths: Vec<String>) -> Result<()> {
        self.update_project_settings("themes", |s| s.themes = Some(paths))
    }

    pub fn get_enable_skill_commands(&self) -> bool {
        self.get_settings().enable_skill_commands.unwrap_or(true)
    }

    pub fn set_enable_skill_commands(&self, enabled: bool) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.enable_skill_commands = Some(enabled);
        Self::mark_modified_locked(&mut st, "enableSkillCommands", None);
        self.save_locked(&mut st);
    }

    pub fn get_thinking_budgets(&self) -> Option<ThinkingBudgetsSettings> {
        self.get_settings().thinking_budgets
    }

    pub fn get_terminal_capability_overrides(&self) -> TerminalCapabilityOverrides {
        let terminal = self.get_settings().terminal.unwrap_or_default();
        let images = match terminal.images {
            Some(TerminalImagesSetting::Protocol(proto)) => Some(Some(proto)),
            Some(TerminalImagesSetting::Bool(false)) => Some(None),
            _ => None,
        };
        let true_color = match terminal.true_color {
            Some(TerminalBoolOrAuto::Bool(b)) => Some(b),
            _ => None,
        };
        let hyperlinks = match terminal.hyperlinks {
            Some(TerminalBoolOrAuto::Bool(b)) => Some(b),
            _ => None,
        };
        TerminalCapabilityOverrides {
            images,
            true_color,
            hyperlinks,
        }
    }

    pub fn get_show_images(&self) -> bool {
        self.get_settings().terminal.and_then(|t| t.show_images).unwrap_or(true)
    }

    pub fn set_show_images(&self, show: bool) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let term = st
            .global_settings
            .terminal
            .get_or_insert_with(TerminalSettings::default);
        term.show_images = Some(show);
        Self::mark_modified_locked(&mut st, "terminal", Some("showImages"));
        self.save_locked(&mut st);
    }

    pub fn get_image_width_cells(&self) -> i64 {
        let width = self
            .get_settings()
            .terminal
            .and_then(|t| t.image_width_cells)
            .and_then(|v| v.as_f64())
            .filter(|n| n.is_finite());
        match width {
            Some(w) => (w.floor() as i64).max(1),
            None => 60,
        }
    }

    pub fn set_image_width_cells(&self, width: f64) {
        let clamped = (width.floor() as i64).max(1);
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let term = st
            .global_settings
            .terminal
            .get_or_insert_with(TerminalSettings::default);
        term.image_width_cells = Some(Value::Number(serde_json::Number::from(clamped)));
        Self::mark_modified_locked(&mut st, "terminal", Some("imageWidthCells"));
        self.save_locked(&mut st);
    }

    pub fn get_clear_on_shrink(&self) -> bool {
        if let Some(clear) = self.get_settings().terminal.and_then(|t| t.clear_on_shrink) {
            return clear;
        }
        pi_js::env::var("PI_CLEAR_ON_SHRINK").is_some_and(|v| v == "1")
    }

    pub fn set_clear_on_shrink(&self, enabled: bool) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let term = st
            .global_settings
            .terminal
            .get_or_insert_with(TerminalSettings::default);
        term.clear_on_shrink = Some(enabled);
        Self::mark_modified_locked(&mut st, "terminal", Some("clearOnShrink"));
        self.save_locked(&mut st);
    }

    pub fn get_show_terminal_progress(&self) -> bool {
        self.get_settings()
            .terminal
            .and_then(|t| t.show_terminal_progress)
            .unwrap_or(false)
    }

    pub fn set_show_terminal_progress(&self, enabled: bool) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let term = st
            .global_settings
            .terminal
            .get_or_insert_with(TerminalSettings::default);
        term.show_terminal_progress = Some(enabled);
        Self::mark_modified_locked(&mut st, "terminal", Some("showTerminalProgress"));
        self.save_locked(&mut st);
    }

    pub fn get_tui_mode(&self) -> TuiMode {
        self.get_settings().tui_mode.unwrap_or(TuiMode::Fullscreen)
    }

    pub fn set_tui_mode(&self, mode: TuiMode) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.tui_mode = Some(mode);
        Self::mark_modified_locked(&mut st, "tuiMode", None);
        self.save_locked(&mut st);
    }

    pub fn get_fullscreen_exit_output(&self) -> FullscreenExitOutput {
        self.get_settings()
            .fullscreen_exit_output
            .unwrap_or(FullscreenExitOutput::Transcript)
    }

    pub fn set_fullscreen_exit_output(&self, output: FullscreenExitOutput) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.fullscreen_exit_output = Some(output);
        Self::mark_modified_locked(&mut st, "fullscreenExitOutput", None);
        self.save_locked(&mut st);
    }

    pub fn get_fullscreen_scrollbar(&self) -> ScrollViewScrollbar {
        self.get_settings()
            .fullscreen_scrollbar
            .unwrap_or(ScrollViewScrollbar::Auto)
    }

    pub fn set_fullscreen_scrollbar(&self, mode: ScrollViewScrollbar) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.fullscreen_scrollbar = Some(mode);
        Self::mark_modified_locked(&mut st, "fullscreenScrollbar", None);
        self.save_locked(&mut st);
    }

    pub fn get_fullscreen_copy_on_select(&self) -> bool {
        self.get_settings().fullscreen_copy_on_select.unwrap_or(true)
    }

    pub fn set_fullscreen_copy_on_select(&self, enabled: bool) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.fullscreen_copy_on_select = Some(enabled);
        Self::mark_modified_locked(&mut st, "fullscreenCopyOnSelect", None);
        self.save_locked(&mut st);
    }

    pub fn get_fullscreen_wheel_scroll_lines(&self) -> WheelScrollLines {
        match self.get_settings().fullscreen_wheel_scroll_lines {
            Some(WheelScrollLines::Lines(n)) if n.is_finite() => WheelScrollLines::Lines(n.floor().clamp(1.0, 100.0)),
            _ => WheelScrollLines::AUTO,
        }
    }

    pub fn set_fullscreen_wheel_scroll_lines(&self, lines: WheelScrollLines) {
        let normalized = match lines {
            WheelScrollLines::Lines(n) => WheelScrollLines::Lines(n.floor().clamp(1.0, 100.0)),
            WheelScrollLines::Auto(a) => WheelScrollLines::Auto(a),
        };
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.fullscreen_wheel_scroll_lines = Some(normalized);
        Self::mark_modified_locked(&mut st, "fullscreenWheelScrollLines", None);
        self.save_locked(&mut st);
    }

    pub fn get_image_auto_resize(&self) -> bool {
        self.get_settings().images.and_then(|i| i.auto_resize).unwrap_or(true)
    }

    pub fn set_image_auto_resize(&self, enabled: bool) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let img = st.global_settings.images.get_or_insert_with(ImageSettings::default);
        img.auto_resize = Some(enabled);
        Self::mark_modified_locked(&mut st, "images", Some("autoResize"));
        self.save_locked(&mut st);
    }

    pub fn get_block_images(&self) -> bool {
        self.get_settings().images.and_then(|i| i.block_images).unwrap_or(false)
    }

    pub fn set_block_images(&self, blocked: bool) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let img = st.global_settings.images.get_or_insert_with(ImageSettings::default);
        img.block_images = Some(blocked);
        Self::mark_modified_locked(&mut st, "images", Some("blockImages"));
        self.save_locked(&mut st);
    }

    pub fn get_enabled_models(&self) -> Option<Vec<String>> {
        self.get_settings().enabled_models
    }

    pub fn get_default_tools(&self) -> Option<Vec<String>> {
        self.get_settings()
            .default_tools
            .map(|tools| resolve_default_tools(&tools))
    }

    pub fn set_enabled_models(&self, patterns: Option<Vec<String>>) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.enabled_models = patterns;
        Self::mark_modified_locked(&mut st, "enabledModels", None);
        self.save_locked(&mut st);
    }

    pub fn get_double_escape_action(&self) -> DoubleEscapeAction {
        self.get_settings()
            .double_escape_action
            .unwrap_or(DoubleEscapeAction::Tree)
    }

    pub fn set_double_escape_action(&self, action: DoubleEscapeAction) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.double_escape_action = Some(action);
        Self::mark_modified_locked(&mut st, "doubleEscapeAction", None);
        self.save_locked(&mut st);
    }

    pub fn get_tree_filter_mode(&self) -> TreeFilterMode {
        self.get_settings().tree_filter_mode.unwrap_or(TreeFilterMode::Default)
    }

    pub fn set_tree_filter_mode(&self, mode: TreeFilterMode) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.tree_filter_mode = Some(mode);
        Self::mark_modified_locked(&mut st, "treeFilterMode", None);
        self.save_locked(&mut st);
    }

    pub fn get_show_hardware_cursor(&self) -> bool {
        if let Some(show) = self.get_settings().show_hardware_cursor {
            return show;
        }
        pi_js::env::var("PI_HARDWARE_CURSOR").is_some_and(|v| v == "1")
    }

    pub fn set_show_hardware_cursor(&self, enabled: bool) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.show_hardware_cursor = Some(enabled);
        Self::mark_modified_locked(&mut st, "showHardwareCursor", None);
        self.save_locked(&mut st);
    }

    pub fn get_editor_padding_x(&self) -> i64 {
        self.get_settings().editor_padding_x.map(|p| p.clamp(0, 3)).unwrap_or(0)
    }

    pub fn set_editor_padding_x(&self, padding: i64) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.editor_padding_x = Some(padding.clamp(0, 3));
        Self::mark_modified_locked(&mut st, "editorPaddingX", None);
        self.save_locked(&mut st);
    }

    pub fn get_output_pad(&self) -> u8 {
        if self.get_settings().output_pad == Some(0) {
            0
        } else {
            1
        }
    }

    pub fn set_output_pad(&self, padding: u8) {
        let pad = if padding == 0 { 0 } else { 1 };
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.output_pad = Some(pad);
        Self::mark_modified_locked(&mut st, "outputPad", None);
        self.save_locked(&mut st);
    }

    pub fn get_autocomplete_max_visible(&self) -> i64 {
        self.get_settings()
            .autocomplete_max_visible
            .map(|v| v.clamp(3, 20))
            .unwrap_or(5)
    }

    pub fn set_autocomplete_max_visible(&self, max_visible: i64) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.autocomplete_max_visible = Some(max_visible.clamp(3, 20));
        Self::mark_modified_locked(&mut st, "autocompleteMaxVisible", None);
        self.save_locked(&mut st);
    }

    pub fn get_code_block_indent(&self) -> String {
        self.get_settings()
            .markdown
            .and_then(|m| m.code_block_indent)
            .unwrap_or_else(|| "  ".to_owned())
    }

    pub fn get_mermaid_rendering_mode(&self) -> MermaidRenderingMode {
        self.get_settings()
            .markdown
            .and_then(|m| m.mermaid)
            .unwrap_or(MermaidRenderingMode::Streaming)
    }

    pub fn set_mermaid_rendering_mode(&self, mode: MermaidRenderingMode) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let md = st
            .global_settings
            .markdown
            .get_or_insert_with(MarkdownSettings::default);
        md.mermaid = Some(mode);
        Self::mark_modified_locked(&mut st, "markdown", Some("mermaid"));
        self.save_locked(&mut st);
    }

    pub fn get_warnings(&self) -> WarningSettings {
        self.get_settings().warnings.unwrap_or_default()
    }

    pub fn set_warnings(&self, warnings: WarningSettings) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.global_settings.warnings = Some(warnings);
        Self::mark_modified_locked(&mut st, "warnings", None);
        self.save_locked(&mut st);
    }
}
