//! Port of packages/coding-agent/src/core/extensions/types.ts

use std::any::Any;
use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::{IndexMap, IndexSet};
use pi_agent_core::types::{
    AgentMessage, AgentTool, AgentToolCallOutcome, AgentToolPrepareArgumentsFn, AgentToolResult,
    AgentToolUpdateCallback, ThinkingLevel, ToolExecutionMode,
};
use pi_ai::types::{
    Api, AssistantMessageEvent, ClassifierApi, ConstrainedSamplingConfig, ImageApi, ImageContent, JsonValue, Message,
    Model, ProviderClassifier, ProviderHeaders, ProviderId, ProviderImages, SimpleStreamOptions, TextContent,
    ToolResultMessage, TranscriptContext, Usage,
};
use pi_ai::{AssistantMessageEventStream, OAuthCredentials, OAuthLoginCallbacks, Provider, RefreshModelsContext};
use pi_js::abort::AbortSignal;
use pi_js::vendor::typebox::Schema;
use pi_js::{BoxFuture, Unsubscribe};
use pi_tui::autocomplete::{AutocompleteItem, AutocompleteProvider};
use pi_tui::components::editor::EditorTheme;
use pi_tui::editor_component::EditorComponent;
use pi_tui::keys::KeyId;
use pi_tui::tui::{Component, OverlayHandle, OverlayOptions, TUI};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::tools::EditToolInput;
use crate::core::tools::bash::{BashOperations, BashToolDetails, BashToolInput};
use crate::core::tools::edit::EditToolDetails;
use crate::core::tools::find::{FindToolDetails, FindToolInput};
use crate::core::tools::grep::{GrepToolDetails, GrepToolInput};
use crate::core::tools::ls::{LsToolDetails, LsToolInput};
use crate::core::tools::powershell::{PowerShellToolDetails, PowerShellToolInput};
use crate::core::tools::read::{ReadToolDetails, ReadToolInput};
use crate::core::tools::write::WriteToolInput;

// ---------------------------------------------------------------------------
// Temporary WIRE(@mcp-w3) surface for Stage 3A-1 items owned by @mcp.
// Removed/replaced with `crate::core::*` + `crate::utils::*` once `swarm/mcp-w3`
// lands on `main`.
// ---------------------------------------------------------------------------

// WIRE(@mcp-w3): replace mcp_wire definitions with imports from crate::core::* / crate::utils::* once swarm/mcp-w3 merges to main.
pub mod mcp_wire {
    use super::*;

    pub const BUILTIN_PATH_PREFIX: &str = "builtin:";

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    pub enum SourceScope {
        User,
        Project,
        Temporary,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub enum SourceOrigin {
        Package,
        TopLevel,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct SourceInfo {
        pub path: String,
        pub source: String,
        pub scope: SourceScope,
        pub origin: SourceOrigin,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub base_dir: Option<String>,
    }

    pub fn get_synthetic_path_source(path: &str) -> Option<String> {
        if path.starts_with(BUILTIN_PATH_PREFIX) {
            return Some("builtin".to_string());
        }
        if path.starts_with('<') && path.ends_with('>') {
            let inner = &path[1..path.len() - 1];
            let prefix = inner.split(':').next().unwrap_or("");
            return Some(if prefix.is_empty() {
                "temporary".to_string()
            } else {
                prefix.to_string()
            });
        }
        None
    }

    pub fn is_synthetic_path(path: &str) -> bool {
        path.starts_with(BUILTIN_PATH_PREFIX) || path.starts_with('<')
    }

    pub fn create_synthetic_source_info(
        path: impl Into<String>,
        source: impl Into<String>,
        scope: Option<SourceScope>,
        origin: Option<SourceOrigin>,
        base_dir: Option<String>,
    ) -> SourceInfo {
        SourceInfo {
            path: path.into(),
            source: source.into(),
            scope: scope.unwrap_or(SourceScope::Temporary),
            origin: origin.unwrap_or(SourceOrigin::TopLevel),
            base_dir,
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    pub enum SlashCommandSource {
        Extension,
        Prompt,
        Skill,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct SlashCommandInfo {
        pub name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
        pub source: SlashCommandSource,
        pub source_info: SourceInfo,
    }

    #[derive(Clone, Debug, Default)]
    pub struct ExecOptions {
        pub signal: Option<AbortSignal>,
        pub timeout: Option<u64>,
        pub cwd: Option<String>,
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ExecResult {
        pub stdout: String,
        pub stderr: String,
        pub code: i64,
        pub killed: bool,
    }

    pub async fn exec_command_wire(
        command: &str,
        args: &[String],
        cwd: &str,
        options: Option<ExecOptions>,
    ) -> pi_js::Result<ExecResult> {
        let opts = options.unwrap_or_default();
        let effective_cwd = opts.cwd.as_deref().unwrap_or(cwd);
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args)
            .current_dir(effective_cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let child = match cmd.spawn() {
            Ok(child) => child,
            Err(err) => {
                return Ok(ExecResult {
                    stdout: String::new(),
                    stderr: err.to_string(),
                    code: 1,
                    killed: false,
                });
            }
        };

        let fut = child.wait_with_output();
        if let Some(sig) = &opts.signal {
            if sig.aborted() {
                return Ok(ExecResult {
                    stdout: String::new(),
                    stderr: String::new(),
                    code: 1,
                    killed: true,
                });
            }
        }

        let res = match opts.timeout {
            Some(ms) if ms > 0 => tokio::time::timeout(std::time::Duration::from_millis(ms), fut)
                .await
                .map_err(|_| ()),
            _ => Ok(fut.await),
        };

        match res {
            Ok(Ok(output)) => Ok(ExecResult {
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                code: i64::from(output.status.code().unwrap_or(1)),
                killed: false,
            }),
            Ok(Err(err)) => Ok(ExecResult {
                stdout: String::new(),
                stderr: err.to_string(),
                code: 1,
                killed: false,
            }),
            Err(()) => Ok(ExecResult {
                stdout: String::new(),
                stderr: String::new(),
                code: 1,
                killed: true,
            }),
        }
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct BashResult {
        pub output: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub exit_code: Option<i64>,
        pub cancelled: bool,
        pub truncated: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub full_output_path: Option<String>,
    }

    pub type EventBusHandler = Arc<dyn Fn(Value) + Send + Sync>;

    pub trait EventBus: Send + Sync {
        fn emit(&self, channel: &str, data: Value);
        fn on(&self, channel: &str, handler: EventBusHandler) -> Unsubscribe;
    }

    pub trait EventBusController: EventBus {
        fn clear(&self);
    }

    #[derive(Default)]
    struct DefaultEventBusInner {
        next_id: u64,
        listeners: IndexMap<String, Vec<(u64, EventBusHandler)>>,
    }

    #[derive(Clone, Default)]
    pub struct DefaultEventBus {
        inner: Arc<Mutex<DefaultEventBusInner>>,
    }

    impl EventBus for DefaultEventBus {
        fn emit(&self, channel: &str, data: Value) {
            let handlers: Vec<EventBusHandler> = {
                let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                guard
                    .listeners
                    .get(channel)
                    .map(|list| list.iter().map(|(_, h)| Arc::clone(h)).collect())
                    .unwrap_or_default()
            };
            for handler in handlers {
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(data.clone())));
                if res.is_err() {
                    // Errors isolated per subscriber like EventEmitter in TS.
                }
            }
        }

        fn on(&self, channel: &str, handler: EventBusHandler) -> Unsubscribe {
            let id = {
                let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                guard.next_id += 1;
                let id = guard.next_id;
                guard
                    .listeners
                    .entry(channel.to_string())
                    .or_default()
                    .push((id, handler));
                id
            };
            let inner = Arc::clone(&self.inner);
            let channel_name = channel.to_string();
            Unsubscribe::new(move || {
                let mut guard = inner.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(list) = guard.listeners.get_mut(&channel_name) {
                    list.retain(|(entry_id, _)| *entry_id != id);
                    if list.is_empty() {
                        guard.listeners.shift_remove(&channel_name);
                    }
                }
            })
        }
    }

    impl EventBusController for DefaultEventBus {
        fn clear(&self) {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            guard.listeners.clear();
        }
    }

    pub fn create_event_bus() -> Arc<dyn EventBusController> {
        Arc::new(DefaultEventBus::default())
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    pub enum McpExposure {
        Codemode,
        Deferred,
        Direct,
        Hidden,
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct McpOAuthConfig {
        #[serde(skip_serializing_if = "Option::is_none")]
        pub client_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub client_secret: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub callback_port: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub callback_url: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub scope: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub client_name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub auth_server_metadata_url: Option<String>,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct McpStdioServerConfig {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub exposure: Option<McpExposure>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tool_exposure: Option<IndexMap<String, McpExposure>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub enabled: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub timeout: Option<i64>,
        #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
        pub r#type: Option<String>,
        pub command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub args: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub env: Option<IndexMap<String, String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub cwd: Option<String>,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct McpHttpServerConfig {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub exposure: Option<McpExposure>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tool_exposure: Option<IndexMap<String, McpExposure>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub enabled: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub timeout: Option<i64>,
        #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
        pub r#type: Option<String>,
        pub url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub headers: Option<IndexMap<String, String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub oauth: Option<McpOAuthConfig>,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(untagged)]
    pub enum McpServerConfig {
        Stdio(McpStdioServerConfig),
        Http(McpHttpServerConfig),
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct RegisteredMcpServer {
        pub name: String,
        pub config: McpServerConfig,
        pub extension_path: String,
    }

    pub fn mcp_namespace(server: &str) -> String {
        let mut out = String::from("mcp__");
        for ch in server.chars() {
            if ch == '-' {
                out.push('_');
            } else {
                out.push(ch);
            }
        }
        out
    }

    pub fn validate_mcp_server_config(_name: &str, raw: &Value) -> std::result::Result<McpServerConfig, String> {
        let mut val = raw.clone();
        if let Some(obj) = val.as_object_mut() {
            if let Some(exp) = obj.get("exposure").and_then(|v| v.as_str()) {
                if exp == "codemode-deferred" {
                    obj.insert("exposure".to_string(), Value::String("codemode".to_string()));
                }
            }
        }
        serde_json::from_value::<McpServerConfig>(val).map_err(|e| format!("Invalid MCP server config: {e}"))
    }

    #[derive(Default)]
    struct McpServerRegistryInner {
        servers: IndexMap<String, RegisteredMcpServer>,
        listener: Option<Arc<dyn Fn() + Send + Sync>>,
    }

    #[derive(Clone, Default)]
    pub struct McpServerRegistry {
        inner: Arc<Mutex<McpServerRegistryInner>>,
    }

    impl McpServerRegistry {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn set_change_listener(&self, listener: Option<Arc<dyn Fn() + Send + Sync>>) {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            guard.listener = listener;
        }

        pub fn get(&self, name: &str) -> Option<RegisteredMcpServer> {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            guard.servers.get(name).cloned()
        }

        pub fn list(&self) -> Vec<RegisteredMcpServer> {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            guard.servers.values().cloned().collect()
        }

        pub fn register(&self, server: RegisteredMcpServer) {
            let listener = {
                let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                guard.servers.insert(server.name.clone(), server);
                guard.listener.clone()
            };
            if let Some(cb) = listener {
                cb();
            }
        }

        pub fn unregister(&self, name: &str, extension_path: &str) -> bool {
            let (removed, listener) = {
                let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                let matches = guard
                    .servers
                    .get(name)
                    .is_some_and(|s| s.extension_path == extension_path);
                let removed = if matches {
                    guard.servers.shift_remove(name).is_some()
                } else {
                    false
                };
                (removed, if removed { guard.listener.clone() } else { None })
            };
            if removed {
                if let Some(cb) = listener {
                    cb();
                }
            }
            removed
        }
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(untagged)]
    pub enum CustomMessageContent {
        Text(String),
        Blocks(Vec<ContentBlock>),
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "type")]
    pub enum ContentBlock {
        #[serde(rename = "text")]
        Text(TextContent),
        #[serde(rename = "image")]
        Image(ImageContent),
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CustomMessage {
        pub role: String,
        pub custom_type: String,
        pub content: CustomMessageContent,
        pub display: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub details: Option<Value>,
        pub timestamp: i64,
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CustomMessageDraft {
        pub custom_type: String,
        pub content: CustomMessageContent,
        pub display: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub details: Option<Value>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    pub enum CodemodeMode {
        On,
        Only,
    }

    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CodemodeSettings {
        #[serde(skip_serializing_if = "Option::is_none")]
        pub mode: Option<CodemodeMode>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub inline_budget: Option<i64>,
    }

    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CompactionSettings {
        pub enabled: bool,
        pub reserve_tokens: i64,
        pub keep_recent_tokens: i64,
    }

    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Settings {
        #[serde(skip_serializing_if = "Option::is_none")]
        pub default_provider: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub default_model: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub default_thinking_level: Option<ThinkingLevel>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub theme: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub codemode: Option<CodemodeSettings>,
        #[serde(flatten)]
        pub extra: IndexMap<String, Value>,
    }

    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct BuildSystemPromptOptions {
        #[serde(skip_serializing_if = "Option::is_none")]
        pub custom_prompt: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub force_system_prompt: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub selected_tools: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tool_snippets: Option<IndexMap<String, String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tool_guidelines: Option<IndexMap<String, Vec<String>>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub prompt_guidelines: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub append_system_prompt: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub sections: Option<IndexMap<String, String>>,
        pub cwd: String,
    }

    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct NormalizedBuildSystemPromptOptions {
        #[serde(skip_serializing_if = "Option::is_none")]
        pub custom_prompt: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub force_system_prompt: Option<String>,
        pub selected_tools: Vec<String>,
        pub tool_snippets: IndexMap<String, String>,
        pub tool_guidelines: IndexMap<String, Vec<String>>,
        pub prompt_guidelines: Vec<String>,
        pub append_system_prompt: String,
        pub sections: IndexMap<String, String>,
        pub cwd: String,
    }

    pub fn normalize_build_system_prompt_options(
        input: BuildSystemPromptOptions,
    ) -> NormalizedBuildSystemPromptOptions {
        NormalizedBuildSystemPromptOptions {
            custom_prompt: input.custom_prompt,
            force_system_prompt: input.force_system_prompt,
            selected_tools: input.selected_tools.unwrap_or_else(|| {
                vec![
                    "read".to_string(),
                    "bash".to_string(),
                    "edit".to_string(),
                    "write".to_string(),
                ]
            }),
            tool_snippets: input.tool_snippets.unwrap_or_default(),
            tool_guidelines: input.tool_guidelines.unwrap_or_default(),
            prompt_guidelines: input.prompt_guidelines.unwrap_or_default(),
            append_system_prompt: input.append_system_prompt.unwrap_or_default(),
            sections: input.sections.unwrap_or_default(),
            cwd: input.cwd,
        }
    }

    pub fn build_system_prompt(options: &NormalizedBuildSystemPromptOptions) -> String {
        if let Some(forced) = &options.force_system_prompt {
            return forced.clone();
        }
        if let Some(custom) = &options.custom_prompt {
            return custom.clone();
        }
        format!("Current working directory: {}", options.cwd)
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    pub enum CacheWarmingAction {
        Warm,
        Stop,
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CacheWarmingDecisionEvent {
        pub action: CacheWarmingAction,
        #[serde(flatten)]
        pub extra: IndexMap<String, Value>,
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CacheWarmingDecisionEventResult {
        #[serde(skip_serializing_if = "Option::is_none")]
        pub action: Option<CacheWarmingAction>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    pub enum ResourceCollisionType {
        Extension,
        Skill,
        Prompt,
        Theme,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ResourceCollision {
        pub resource_type: ResourceCollisionType,
        pub name: String,
        pub winner_path: String,
        pub loser_path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub winner_source: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub loser_source: Option<String>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    pub enum ResourceDiagnosticType {
        Warning,
        Error,
        Collision,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ResourceDiagnostic {
        #[serde(rename = "type")]
        pub r#type: ResourceDiagnosticType,
        pub message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub path: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub collision: Option<ResourceCollision>,
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CompactionEntry {
        pub id: String,
        pub parent_id: Option<String>,
        pub timestamp: String,
        pub summary: String,
        pub first_kept_entry_id: String,
        pub tokens_before: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub details: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub usage: Option<Usage>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub from_hook: Option<bool>,
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct BranchSummaryEntry {
        pub id: String,
        pub parent_id: Option<String>,
        pub timestamp: String,
        pub from_id: String,
        pub summary: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub details: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub usage: Option<Usage>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub from_hook: Option<bool>,
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CustomEntry {
        pub id: String,
        pub parent_id: Option<String>,
        pub timestamp: String,
        pub custom_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub data: Option<Value>,
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ContextEditReplacement {
        pub content: Value,
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ContextEditEntry {
        pub id: String,
        pub parent_id: Option<String>,
        pub timestamp: String,
        pub target_id: String,
        pub replacement: Option<ContextEditReplacement>,
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct SessionEntry {
        #[serde(rename = "type")]
        pub r#type: String,
        pub id: String,
        pub parent_id: Option<String>,
        pub timestamp: String,
        #[serde(flatten)]
        pub fields: IndexMap<String, Value>,
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ProjectedSessionEntry {
        pub source_entry: SessionEntry,
        pub messages: Vec<AgentMessage>,
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct FileOperations {
        pub read: IndexSet<String>,
        pub written: IndexSet<String>,
        pub edited: IndexSet<String>,
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CompactionPreparation {
        pub first_kept_entry_id: String,
        pub messages_to_summarize: Vec<AgentMessage>,
        pub turn_prefix_messages: Vec<AgentMessage>,
        pub is_split_turn: bool,
        pub tokens_before: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub previous_summary: Option<String>,
        pub file_ops: FileOperations,
        pub settings: CompactionSettings,
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CompactionResult {
        pub summary: String,
        pub first_kept_entry_id: String,
        pub tokens_before: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub estimated_tokens_after: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub usage: Option<Usage>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub details: Option<Value>,
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ScopedModel {
        pub model: Model,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub thinking_level: Option<ThinkingLevel>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    pub enum ModelRouteReason {
        User,
        Continuation,
        Retry,
        Direct,
    }

    #[derive(Clone, Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ModelRouteRequest {
        pub model: Model,
        pub thinking_level: pi_ai::types::ModelThinkingLevel,
        pub reason: ModelRouteReason,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub state: Option<Value>,
        pub messages: Vec<Message>,
        #[serde(skip)]
        pub signal: Option<AbortSignal>,
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ModelRoute {
        pub model: Model,
        pub thinking_level: pi_ai::types::ModelThinkingLevel,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub state: Option<Value>,
    }

    #[derive(Clone)]
    pub struct VirtualModelDefinition {
        pub provider: String,
        pub id: String,
        pub name: String,
        pub thinking_levels: Option<Vec<pi_ai::types::ModelThinkingLevel>>,
        pub route: Arc<dyn Fn(ModelRouteRequest) -> BoxFuture<pi_js::Result<ModelRoute>> + Send + Sync>,
    }

    pub trait SessionManagerBackend: Send + Sync {
        fn get_cwd(&self) -> String;
        fn get_session_dir(&self) -> String;
        fn get_session_id(&self) -> String;
        fn get_session_file(&self) -> Option<String>;
        fn get_leaf_id(&self) -> Option<String>;
        fn get_entry(&self, id: &str) -> Option<SessionEntry>;
        fn get_label(&self, id: &str) -> Option<String>;
        fn get_branch(&self, from_id: Option<&str>) -> Vec<SessionEntry>;
        fn get_entries(&self) -> Vec<SessionEntry>;
        fn get_session_name(&self) -> Option<String>;
    }

    #[derive(Clone)]
    pub struct SessionManager(pub Arc<dyn SessionManagerBackend>);

    impl Default for SessionManager {
        fn default() -> Self {
            Self(Arc::new(NoopSessionManager))
        }
    }

    impl SessionManager {
        pub fn get_cwd(&self) -> String {
            self.0.get_cwd()
        }
        pub fn get_session_dir(&self) -> String {
            self.0.get_session_dir()
        }
        pub fn get_session_id(&self) -> String {
            self.0.get_session_id()
        }
        pub fn get_session_file(&self) -> Option<String> {
            self.0.get_session_file()
        }
        pub fn get_leaf_id(&self) -> Option<String> {
            self.0.get_leaf_id()
        }
        pub fn get_entry(&self, id: &str) -> Option<SessionEntry> {
            self.0.get_entry(id)
        }
        pub fn get_label(&self, id: &str) -> Option<String> {
            self.0.get_label(id)
        }
        pub fn get_branch(&self, from_id: Option<&str>) -> Vec<SessionEntry> {
            self.0.get_branch(from_id)
        }
        pub fn get_entries(&self) -> Vec<SessionEntry> {
            self.0.get_entries()
        }
        pub fn get_session_name(&self) -> Option<String> {
            self.0.get_session_name()
        }
    }

    pub type ReadonlySessionManager = SessionManager;

    struct NoopSessionManager;
    impl SessionManagerBackend for NoopSessionManager {
        fn get_cwd(&self) -> String {
            String::new()
        }
        fn get_session_dir(&self) -> String {
            String::new()
        }
        fn get_session_id(&self) -> String {
            String::new()
        }
        fn get_session_file(&self) -> Option<String> {
            None
        }
        fn get_leaf_id(&self) -> Option<String> {
            None
        }
        fn get_entry(&self, _id: &str) -> Option<SessionEntry> {
            None
        }
        fn get_label(&self, _id: &str) -> Option<String> {
            None
        }
        fn get_branch(&self, _from_id: Option<&str>) -> Vec<SessionEntry> {
            Vec::new()
        }
        fn get_entries(&self) -> Vec<SessionEntry> {
            Vec::new()
        }
        fn get_session_name(&self) -> Option<String> {
            None
        }
    }

    pub trait ModelRegistryBackend: Send + Sync {
        fn register_provider(&self, name: &str, config: ProviderConfig);
        fn register_native_provider(&self, provider: Arc<dyn Provider>);
        fn unregister_provider(&self, name: &str);
        fn register_virtual_model(&self, definition: VirtualModelDefinition);
        fn unregister_virtual_model(&self, provider: &str, id: &str);
        fn get_available_models(&self) -> Vec<Model>;
        fn get_api_key(&self, model: &Model) -> BoxFuture<pi_js::Result<Option<String>>>;
    }

    #[derive(Clone)]
    pub struct ModelRegistry(pub Arc<dyn ModelRegistryBackend>);

    impl Default for ModelRegistry {
        fn default() -> Self {
            Self(Arc::new(NoopModelRegistry))
        }
    }

    struct NoopModelRegistry;
    impl ModelRegistryBackend for NoopModelRegistry {
        fn register_provider(&self, _name: &str, _config: ProviderConfig) {}
        fn register_native_provider(&self, _provider: Arc<dyn Provider>) {}
        fn unregister_provider(&self, _name: &str) {}
        fn register_virtual_model(&self, _definition: VirtualModelDefinition) {}
        fn unregister_virtual_model(&self, _provider: &str, _id: &str) {}
        fn get_available_models(&self) -> Vec<Model> {
            Vec::new()
        }
        fn get_api_key(&self, _model: &Model) -> BoxFuture<pi_js::Result<Option<String>>> {
            Box::pin(async { Ok(None) })
        }
    }

    pub trait ReadonlyFooterDataProvider: Send + Sync {
        fn get_git_branch(&self) -> Option<String>;
        fn get_extension_statuses(&self) -> IndexMap<String, String>;
    }

    pub type KeybindingsManager = pi_tui::keybindings::KeybindingsManager;
    pub type AppKeybinding = String;

    pub fn resolve_path_wire(input: &str, base_dir: &str) -> String {
        let trimmed = input.strip_prefix('@').unwrap_or(input);
        let expanded = if trimmed == "~" {
            pi_js::env::home_dir()
        } else if let Some(rest) = trimmed.strip_prefix("~/").or_else(|| trimmed.strip_prefix("~\\")) {
            pi_js::path::join(&[&pi_js::env::home_dir(), rest])
        } else {
            trimmed.to_string()
        };
        if pi_js::path::is_absolute(&expanded) {
            pi_js::path::normalize(&expanded)
        } else {
            pi_js::path::resolve(&[base_dir, &expanded])
        }
    }

    pub fn strip_ansi_wire(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                match chars.peek().copied() {
                    Some('[') => {
                        chars.next();
                        while let Some(n) = chars.next() {
                            if ('\x40'..='\x7e').contains(&n) {
                                break;
                            }
                        }
                    }
                    Some(']') => {
                        chars.next();
                        while let Some(n) = chars.next() {
                            if n == '\x07' {
                                break;
                            }
                            if n == '\x1b' && chars.peek() == Some(&'\\') {
                                chars.next();
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    pub fn sanitize_binary_output_wire(s: &str) -> String {
        s.chars()
            .filter(|&c| {
                if c == '\t' || c == '\n' || c == '\r' {
                    return true;
                }
                let code = c as u32;
                !(code <= 0x1f || (0x7f..=0x9f).contains(&code) || (0xfff9..=0xfffb).contains(&code))
            })
            .collect()
    }

    pub fn get_agent_dir_wire() -> String {
        if let Some(dir) = pi_js::env::var("PI_CODING_AGENT_DIR") {
            return resolve_path_wire(&dir, &pi_js::env::cwd());
        }
        pi_js::path::join(&[&pi_js::env::home_dir(), ".pi", "agent"])
    }

    pub const CONFIG_DIR_NAME_WIRE: &str = ".pi";

    #[derive(Clone, Debug, Default, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct PiManifest {
        #[serde(default)]
        pub extensions: Vec<String>,
    }

    pub fn read_pi_manifest_wire(package_json_path: &str) -> Option<PiManifest> {
        let content = pi_js::fs::read_to_string(package_json_path).ok()?;
        let val = pi_js::json::parse(content.strip_prefix('\u{feff}').unwrap_or(&content)).ok()?;
        let pi_val = val.get("pi")?;
        serde_json::from_value::<PiManifest>(pi_val.clone()).ok()
    }
}

pub use mcp_wire::{
    AppKeybinding, BashResult, BuildSystemPromptOptions, CacheWarmingAction, CacheWarmingDecisionEvent,
    CacheWarmingDecisionEventResult, ExecOptions, ExecResult, KeybindingsManager, NormalizedBuildSystemPromptOptions,
    SlashCommandInfo, SlashCommandSource, SourceInfo,
};

// ---------------------------------------------------------------------------
// Stage 3B Theme adapter (// WIRE(@w3-b))
// ---------------------------------------------------------------------------

/// Render theme surface used by tool renderers and extension UI.
///
/// // WIRE(@w3-b): implement for `crate::modes::interactive::theme::Theme` in Stage 3B-2.
pub trait ExtensionTheme: Send + Sync {
    fn fg(&self, color: &str, text: &str) -> String;
    fn bg(&self, bg: &str, text: &str) -> String;
    fn bold(&self, text: &str) -> String;
    fn italic(&self, text: &str) -> String;
    fn underline(&self, text: &str) -> String;
    fn inverse(&self, text: &str) -> String;
    fn strikethrough(&self, text: &str) -> String;
    fn get_fg_ansi(&self, color: &str) -> String;
    fn get_bg_ansi(&self, bg: &str) -> String;
}

#[derive(Default)]
struct PlainTheme;

impl ExtensionTheme for PlainTheme {
    fn fg(&self, _color: &str, text: &str) -> String {
        text.to_string()
    }
    fn bg(&self, _bg: &str, text: &str) -> String {
        text.to_string()
    }
    fn bold(&self, text: &str) -> String {
        text.to_string()
    }
    fn italic(&self, text: &str) -> String {
        text.to_string()
    }
    fn underline(&self, text: &str) -> String {
        text.to_string()
    }
    fn inverse(&self, text: &str) -> String {
        text.to_string()
    }
    fn strikethrough(&self, text: &str) -> String {
        text.to_string()
    }
    fn get_fg_ansi(&self, _color: &str) -> String {
        String::new()
    }
    fn get_bg_ansi(&self, _bg: &str) -> String {
        String::new()
    }
}

/// Handle for the active UI theme passed to extension/tool renderers.
#[derive(Clone)]
pub struct Theme(pub Arc<dyn ExtensionTheme>);

impl Default for Theme {
    fn default() -> Self {
        Self(Arc::new(PlainTheme))
    }
}

impl fmt::Debug for Theme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Theme").finish()
    }
}

impl Theme {
    pub fn new(inner: Arc<dyn ExtensionTheme>) -> Self {
        Self(inner)
    }
    pub fn fg(&self, color: &str, text: &str) -> String {
        self.0.fg(color, text)
    }
    pub fn bg(&self, bg: &str, text: &str) -> String {
        self.0.bg(bg, text)
    }
    pub fn bold(&self, text: &str) -> String {
        self.0.bold(text)
    }
    pub fn italic(&self, text: &str) -> String {
        self.0.italic(text)
    }
    pub fn underline(&self, text: &str) -> String {
        self.0.underline(text)
    }
    pub fn inverse(&self, text: &str) -> String {
        self.0.inverse(text)
    }
    pub fn strikethrough(&self, text: &str) -> String {
        self.0.strikethrough(text)
    }
    pub fn get_fg_ansi(&self, color: &str) -> String {
        self.0.get_fg_ansi(color)
    }
    pub fn get_bg_ansi(&self, bg: &str) -> String {
        self.0.get_bg_ansi(bg)
    }
}

// ============================================================================
// UI Context
// ============================================================================

/// Options for extension UI dialogs.
#[derive(Clone, Debug, Default)]
pub struct ExtensionUIDialogOptions {
    /// AbortSignal to programmatically dismiss the dialog.
    pub signal: Option<AbortSignal>,
    /// Timeout in milliseconds. Dialog auto-dismisses with live countdown display.
    pub timeout: Option<u64>,
}

pub type ExtensionUiDialogOptions = ExtensionUIDialogOptions;

/// Placement for extension widgets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WidgetPlacement {
    #[default]
    AboveEditor,
    BelowEditor,
}

/// Options for extension widgets.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionWidgetOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placement: Option<WidgetPlacement>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalInputHandlerResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consume: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

/// Raw terminal input listener for extensions.
pub type TerminalInputHandler = Arc<dyn Fn(&str) -> Option<TerminalInputHandlerResult> + Send + Sync>;

/// Working indicator configuration for the interactive streaming loader.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkingIndicatorOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frames: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<i64>,
}

/// Wrap the current autocomplete provider with additional behavior.
pub type AutocompleteProviderFactory =
    Arc<dyn Fn(Arc<dyn AutocompleteProvider>) -> Arc<dyn AutocompleteProvider> + Send + Sync>;

pub type EditorFactory =
    Arc<dyn Fn(Arc<dyn TUI>, EditorTheme, KeybindingsManager) -> Arc<dyn EditorComponent> + Send + Sync>;

pub type WidgetFactory = Arc<dyn Fn(Arc<dyn TUI>, Theme) -> Arc<dyn Component> + Send + Sync>;
pub type FooterFactory =
    Arc<dyn Fn(Arc<dyn TUI>, Theme, Arc<dyn mcp_wire::ReadonlyFooterDataProvider>) -> Arc<dyn Component> + Send + Sync>;
pub type HeaderFactory = Arc<dyn Fn(Arc<dyn TUI>, Theme) -> Arc<dyn Component> + Send + Sync>;

pub type DoneFn = Arc<dyn Fn(Value) + Send + Sync>;
pub type CustomUiFactory = Box<
    dyn FnOnce(Arc<dyn TUI>, Theme, KeybindingsManager, DoneFn) -> BoxFuture<pi_js::Result<Arc<dyn Component>>> + Send,
>;

pub type OverlayOptionsFn = Arc<dyn Fn() -> OverlayOptions + Send + Sync>;

#[derive(Clone, Default)]
pub struct ExtensionCustomOptions {
    pub overlay: Option<bool>,
    pub overlay_options: Option<OverlayOptionsSource>,
    pub on_handle: Option<Arc<dyn Fn(OverlayHandle) + Send + Sync>>,
}

#[derive(Clone)]
pub enum OverlayOptionsSource {
    Static(OverlayOptions),
    Dynamic(OverlayOptionsFn),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotifyType {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetThemeResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// UI context for extensions to request interactive UI.
/// Each mode (interactive, RPC, print) provides its own implementation.
#[async_trait]
pub trait ExtensionUIContext: Send + Sync {
    async fn select(
        &self,
        title: &str,
        options: Vec<String>,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>>;

    async fn confirm(&self, title: &str, message: &str, opts: Option<ExtensionUIDialogOptions>) -> pi_js::Result<bool>;

    async fn input(
        &self,
        title: &str,
        placeholder: Option<&str>,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>>;

    fn notify(&self, message: &str, notify_type: Option<NotifyType>);

    fn on_terminal_input(&self, handler: TerminalInputHandler) -> Unsubscribe;

    fn set_status(&self, key: &str, text: Option<&str>);

    fn set_working_message(&self, message: Option<&str>);

    fn set_working_visible(&self, visible: bool);

    fn set_working_indicator(&self, options: Option<WorkingIndicatorOptions>);

    fn set_hidden_thinking_label(&self, label: Option<&str>);

    fn set_widget_lines(&self, key: &str, content: Option<Vec<String>>, options: Option<ExtensionWidgetOptions>);

    fn set_widget_factory(&self, key: &str, factory: Option<WidgetFactory>, options: Option<ExtensionWidgetOptions>);

    fn set_footer(&self, factory: Option<FooterFactory>);

    fn set_header(&self, factory: Option<HeaderFactory>);

    fn set_title(&self, title: &str);

    async fn custom(&self, factory: CustomUiFactory, options: Option<ExtensionCustomOptions>) -> pi_js::Result<Value>;

    fn paste_to_editor(&self, text: &str);

    fn set_editor_text(&self, text: &str);

    fn get_editor_text(&self) -> String;

    async fn editor(&self, title: &str, prefill: Option<&str>) -> pi_js::Result<Option<String>>;

    fn add_autocomplete_provider(&self, factory: AutocompleteProviderFactory);

    fn set_editor_component(&self, factory: Option<EditorFactory>);

    fn get_editor_component(&self) -> Option<EditorFactory>;

    fn theme(&self) -> Theme;

    fn get_all_themes(&self) -> Vec<ThemeInfo>;

    fn get_theme(&self, name: &str) -> Option<Theme>;

    fn set_theme_by_name(&self, name: &str) -> SetThemeResult;

    fn set_theme(&self, theme: Theme) -> SetThemeResult;

    fn get_tools_expanded(&self) -> bool;

    fn set_tools_expanded(&self, expanded: bool);
}

pub use ExtensionUIContext as ExtensionUiContext;

// ============================================================================
// Extension Context
// ============================================================================

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
    pub tokens: Option<i64>,
    pub context_window: i64,
    pub percent: Option<f64>,
}

#[derive(Clone, Default)]
pub struct CompactOptions {
    pub custom_instructions: Option<String>,
    pub on_complete: Option<Arc<dyn Fn(mcp_wire::CompactionResult) + Send + Sync>>,
    pub on_error: Option<Arc<dyn Fn(pi_js::Error) + Send + Sync>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtensionMode {
    Tui,
    Rpc,
    Json,
    Print,
}

#[derive(Clone, Default)]
pub struct ExecuteToolOptions {
    pub signal: Option<AbortSignal>,
    pub on_update: Option<AgentToolUpdateCallback>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelledResult {
    pub cancelled: bool,
}

pub struct NewSessionOptions {
    pub parent_session: Option<String>,
    pub setup: Option<Box<dyn FnOnce(mcp_wire::SessionManager) -> BoxFuture<pi_js::Result<()>> + Send>>,
    pub with_session: Option<Box<dyn FnOnce(ReplacedSessionContext) -> BoxFuture<pi_js::Result<()>> + Send>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForkPosition {
    Before,
    At,
}

pub struct ForkOptions {
    pub position: Option<ForkPosition>,
    pub with_session: Option<Box<dyn FnOnce(ReplacedSessionContext) -> BoxFuture<pi_js::Result<()>> + Send>>,
}

#[derive(Clone, Debug, Default)]
pub struct NavigateTreeOptions {
    pub summarize: Option<bool>,
    pub custom_instructions: Option<String>,
    pub replace_instructions: Option<bool>,
    pub label: Option<String>,
}

pub struct SwitchSessionOptions {
    pub with_session: Option<Box<dyn FnOnce(ReplacedSessionContext) -> BoxFuture<pi_js::Result<()>> + Send>>,
}

/// Backend backing [`ExtensionContext`], [`ExtensionToolContext`], [`ExtensionCommandContext`].
pub trait ExtensionContextBackend: Send + Sync {
    fn assert_active(&self) -> pi_js::Result<()>;
    fn ui(&self) -> Arc<dyn ExtensionUIContext>;
    fn mode(&self) -> ExtensionMode;
    fn has_ui(&self) -> bool;
    fn cwd(&self) -> String;
    fn session_manager(&self) -> mcp_wire::ReadonlySessionManager;
    fn model_registry(&self) -> mcp_wire::ModelRegistry;
    fn model(&self) -> Option<Model>;
    fn scoped_models(&self) -> Vec<mcp_wire::ScopedModel>;
    fn thinking_level(&self) -> Option<ThinkingLevel>;
    fn is_idle(&self) -> bool;
    fn is_project_trusted(&self) -> bool;
    fn signal(&self) -> Option<AbortSignal>;
    fn abort(&self);
    fn has_pending_messages(&self) -> bool;
    fn shutdown(&self);
    fn get_context_usage(&self) -> Option<ContextUsage>;
    fn compact(&self, options: Option<CompactOptions>);
    fn get_system_prompt(&self) -> String;
    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions;
    fn wait_for_idle(&self) -> BoxFuture<pi_js::Result<()>>;
    fn new_session(&self, options: Option<NewSessionOptions>) -> BoxFuture<pi_js::Result<CancelledResult>>;
    fn fork(&self, entry_id: &str, options: Option<ForkOptions>) -> BoxFuture<pi_js::Result<CancelledResult>>;
    fn navigate_tree(
        &self,
        target_id: &str,
        options: Option<NavigateTreeOptions>,
    ) -> BoxFuture<pi_js::Result<CancelledResult>>;
    fn switch_session(
        &self,
        session_path: &str,
        options: Option<SwitchSessionOptions>,
    ) -> BoxFuture<pi_js::Result<CancelledResult>>;
    fn reload(&self) -> BoxFuture<pi_js::Result<()>>;
    fn callable_tools(&self) -> Vec<AgentTool>;
    fn execute_tool(
        &self,
        caller_id: &str,
        name: &str,
        args: Value,
        options: ExecuteToolOptions,
    ) -> BoxFuture<AgentToolCallOutcome>;
    fn send_message(&self, message: mcp_wire::CustomMessageDraft, options: Option<SendMessageOptions>);
    fn send_user_message(&self, content: mcp_wire::CustomMessageContent, options: Option<SendUserMessageOptions>);
}

#[derive(Clone)]
pub struct ExtensionContext {
    pub(crate) backend: Arc<dyn ExtensionContextBackend>,
    pub(crate) system_prompt_override: Option<Arc<dyn Fn() -> String + Send + Sync>>,
}

impl ExtensionContext {
    pub fn from_backend(backend: Arc<dyn ExtensionContextBackend>) -> Self {
        Self {
            backend,
            system_prompt_override: None,
        }
    }

    pub fn with_system_prompt_override(mut self, f: Arc<dyn Fn() -> String + Send + Sync>) -> Self {
        self.system_prompt_override = Some(f);
        self
    }

    pub fn ui(&self) -> pi_js::Result<Arc<dyn ExtensionUIContext>> {
        self.backend.assert_active()?;
        Ok(self.backend.ui())
    }

    pub fn mode(&self) -> pi_js::Result<ExtensionMode> {
        self.backend.assert_active()?;
        Ok(self.backend.mode())
    }

    pub fn has_ui(&self) -> pi_js::Result<bool> {
        self.backend.assert_active()?;
        Ok(self.backend.has_ui())
    }

    pub fn cwd(&self) -> pi_js::Result<String> {
        self.backend.assert_active()?;
        Ok(self.backend.cwd())
    }

    pub fn session_manager(&self) -> pi_js::Result<mcp_wire::ReadonlySessionManager> {
        self.backend.assert_active()?;
        Ok(self.backend.session_manager())
    }

    pub fn model_registry(&self) -> pi_js::Result<mcp_wire::ModelRegistry> {
        self.backend.assert_active()?;
        Ok(self.backend.model_registry())
    }

    pub fn model(&self) -> pi_js::Result<Option<Model>> {
        self.backend.assert_active()?;
        Ok(self.backend.model())
    }

    pub fn scoped_models(&self) -> pi_js::Result<Vec<mcp_wire::ScopedModel>> {
        self.backend.assert_active()?;
        Ok(self.backend.scoped_models())
    }

    pub fn thinking_level(&self) -> pi_js::Result<Option<ThinkingLevel>> {
        self.backend.assert_active()?;
        Ok(self.backend.thinking_level())
    }

    pub fn is_idle(&self) -> pi_js::Result<bool> {
        self.backend.assert_active()?;
        Ok(self.backend.is_idle())
    }

    pub fn is_project_trusted(&self) -> pi_js::Result<bool> {
        self.backend.assert_active()?;
        Ok(self.backend.is_project_trusted())
    }

    pub fn signal(&self) -> pi_js::Result<Option<AbortSignal>> {
        self.backend.assert_active()?;
        Ok(self.backend.signal())
    }

    pub fn abort(&self) -> pi_js::Result<()> {
        self.backend.assert_active()?;
        self.backend.abort();
        Ok(())
    }

    pub fn has_pending_messages(&self) -> pi_js::Result<bool> {
        self.backend.assert_active()?;
        Ok(self.backend.has_pending_messages())
    }

    pub fn shutdown(&self) -> pi_js::Result<()> {
        self.backend.assert_active()?;
        self.backend.shutdown();
        Ok(())
    }

    pub fn get_context_usage(&self) -> pi_js::Result<Option<ContextUsage>> {
        self.backend.assert_active()?;
        Ok(self.backend.get_context_usage())
    }

    pub fn compact(&self, options: Option<CompactOptions>) -> pi_js::Result<()> {
        self.backend.assert_active()?;
        self.backend.compact(options);
        Ok(())
    }

    pub fn get_system_prompt(&self) -> pi_js::Result<String> {
        self.backend.assert_active()?;
        if let Some(f) = &self.system_prompt_override {
            Ok(f())
        } else {
            Ok(self.backend.get_system_prompt())
        }
    }
}

#[derive(Clone)]
pub struct ExtensionToolContext {
    pub base: ExtensionContext,
    pub tool_call_id: String,
    pub default_signal: Option<AbortSignal>,
}

impl ExtensionToolContext {
    pub fn as_context(&self) -> &ExtensionContext {
        &self.base
    }

    pub fn cwd(&self) -> pi_js::Result<String> {
        self.base.cwd()
    }

    pub fn model(&self) -> pi_js::Result<Option<Model>> {
        self.base.model()
    }

    pub fn session_manager(&self) -> pi_js::Result<mcp_wire::ReadonlySessionManager> {
        self.base.session_manager()
    }

    pub fn model_registry(&self) -> pi_js::Result<mcp_wire::ModelRegistry> {
        self.base.model_registry()
    }

    pub fn thinking_level(&self) -> pi_js::Result<Option<ThinkingLevel>> {
        self.base.thinking_level()
    }

    pub fn ui(&self) -> pi_js::Result<Arc<dyn ExtensionUIContext>> {
        self.base.ui()
    }

    pub fn tools(&self) -> pi_js::Result<Vec<AgentTool>> {
        self.base.backend.assert_active()?;
        Ok(self.base.backend.callable_tools())
    }

    pub async fn execute_tool(
        &self,
        name: &str,
        args: Value,
        options: Option<ExecuteToolOptions>,
    ) -> pi_js::Result<AgentToolCallOutcome> {
        self.base.backend.assert_active()?;
        let mut opts = options.unwrap_or_default();
        if opts.signal.is_none() {
            opts.signal = self.default_signal.clone();
        }
        Ok(self
            .base
            .backend
            .execute_tool(&self.tool_call_id, name, args, opts)
            .await)
    }
}

#[derive(Clone)]
pub struct ExtensionCommandContext {
    pub base: ExtensionContext,
}

impl ExtensionCommandContext {
    pub fn as_context(&self) -> &ExtensionContext {
        &self.base
    }

    pub fn get_system_prompt_options(&self) -> pi_js::Result<BuildSystemPromptOptions> {
        self.base.backend.assert_active()?;
        Ok(self.base.backend.get_system_prompt_options())
    }

    pub async fn wait_for_idle(&self) -> pi_js::Result<()> {
        self.base.backend.assert_active()?;
        self.base.backend.wait_for_idle().await
    }

    pub async fn new_session(&self, options: Option<NewSessionOptions>) -> pi_js::Result<CancelledResult> {
        self.base.backend.assert_active()?;
        self.base.backend.new_session(options).await
    }

    pub async fn fork(&self, entry_id: &str, options: Option<ForkOptions>) -> pi_js::Result<CancelledResult> {
        self.base.backend.assert_active()?;
        self.base.backend.fork(entry_id, options).await
    }

    pub async fn navigate_tree(
        &self,
        target_id: &str,
        options: Option<NavigateTreeOptions>,
    ) -> pi_js::Result<CancelledResult> {
        self.base.backend.assert_active()?;
        self.base.backend.navigate_tree(target_id, options).await
    }

    pub async fn switch_session(
        &self,
        session_path: &str,
        options: Option<SwitchSessionOptions>,
    ) -> pi_js::Result<CancelledResult> {
        self.base.backend.assert_active()?;
        self.base.backend.switch_session(session_path, options).await
    }

    pub async fn reload(&self) -> pi_js::Result<()> {
        self.base.backend.assert_active()?;
        self.base.backend.reload().await
    }
}

#[derive(Clone)]
pub struct ReplacedSessionContext {
    pub base: ExtensionCommandContext,
}

impl ReplacedSessionContext {
    pub fn as_command_context(&self) -> &ExtensionCommandContext {
        &self.base
    }

    pub async fn send_message(
        &self,
        message: mcp_wire::CustomMessageDraft,
        options: Option<SendMessageOptions>,
    ) -> pi_js::Result<()> {
        self.base.base.backend.assert_active()?;
        self.base.base.backend.send_message(message, options);
        Ok(())
    }

    pub async fn send_user_message(
        &self,
        content: mcp_wire::CustomMessageContent,
        options: Option<SendUserMessageOptions>,
    ) -> pi_js::Result<()> {
        self.base.base.backend.assert_active()?;
        self.base.base.backend.send_user_message(content, options);
        Ok(())
    }
}

// ============================================================================
// Tool Types
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolRenderResultOptions {
    pub expanded: bool,
    pub is_partial: bool,
}

#[derive(Clone)]
pub struct ToolRenderContext {
    pub args: Value,
    pub tool_call_id: String,
    pub invalidate: Arc<dyn Fn() + Send + Sync>,
    pub last_component: Option<Arc<dyn Component>>,
    pub state: Arc<Mutex<Option<Box<dyn Any + Send>>>>,
    pub cwd: String,
    pub execution_started: bool,
    pub args_complete: bool,
    pub is_partial: bool,
    pub expanded: bool,
    pub show_images: bool,
    pub is_error: bool,
}

impl Default for ToolRenderContext {
    fn default() -> Self {
        Self {
            args: Value::Null,
            tool_call_id: String::new(),
            invalidate: Arc::new(|| {}),
            last_component: None,
            state: Arc::new(Mutex::new(None)),
            cwd: String::new(),
            execution_started: false,
            args_complete: true,
            is_partial: false,
            expanded: false,
            show_images: true,
            is_error: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolExposure {
    #[default]
    Direct,
    ModelOnly,
    Codemode,
    Deferred,
    Hidden,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAnnotations {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_world_hint: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolNamespace {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

pub trait ToolLoadoutBackend: Send + Sync {
    fn declared(&self) -> &[AgentTool];
    fn callable(&self) -> &[AgentTool];
    fn registered(&self) -> &[AgentTool];
    fn get_exposure(&self, name: &str) -> ToolExposure;
    fn get_namespace(&self, name: &str) -> Option<ToolNamespace>;
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolLoadoutChanges {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub descriptions: Option<IndexMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden_declarations: Option<Vec<String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderShell {
    Default,
    Self_,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConstrainedSamplingOption {
    Disabled(bool),
    Config(ConstrainedSamplingConfig),
}

pub type PrepareLoadoutFn =
    Arc<dyn Fn(&dyn ToolLoadoutBackend) -> pi_js::Result<Option<ToolLoadoutChanges>> + Send + Sync>;

pub type ToolExecuteFn = Arc<
    dyn Fn(
            String,
            Value,
            Option<AbortSignal>,
            Option<AgentToolUpdateCallback>,
            ExtensionToolContext,
        ) -> BoxFuture<pi_js::Result<AgentToolResult>>
        + Send
        + Sync,
>;

pub type RenderCallFn = Arc<dyn Fn(Value, Theme, ToolRenderContext) -> Arc<dyn Component> + Send + Sync>;
pub type RenderResultFn =
    Arc<dyn Fn(AgentToolResult, ToolRenderResultOptions, Theme, ToolRenderContext) -> Arc<dyn Component> + Send + Sync>;

#[derive(Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub label: String,
    pub description: String,
    pub prompt_snippet: Option<String>,
    pub prompt_guidelines: Option<Vec<String>>,
    pub parameters: Schema,
    pub constrained_sampling: Option<ConstrainedSamplingOption>,
    pub render_shell: Option<RenderShell>,
    pub prepare_arguments: Option<AgentToolPrepareArgumentsFn>,
    pub output_schema: Option<Schema>,
    pub exposure: Option<ToolExposure>,
    pub namespace: Option<ToolNamespace>,
    pub annotations: Option<ToolAnnotations>,
    pub default_active: Option<bool>,
    pub prepare_loadout: Option<PrepareLoadoutFn>,
    pub execution_mode: Option<ToolExecutionMode>,
    pub execute: ToolExecuteFn,
    pub render_call: Option<RenderCallFn>,
    pub render_result: Option<RenderResultFn>,
}

pub fn define_tool(tool: ToolDefinition) -> ToolDefinition {
    tool
}

// ============================================================================
// Event Trait & Startup/Resource/Session/Agent/Tool Events
// ============================================================================

pub trait ExtensionEvent: Clone + Send + Sync + 'static {
    const TYPE: &'static str;
    type Output: Send + 'static;
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTrustEvent {
    pub cwd: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectTrustEventDecision {
    Yes,
    No,
    Undecided,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTrustEventResult {
    pub trusted: ProjectTrustEventDecision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remember: Option<bool>,
}

#[derive(Clone)]
pub struct ProjectTrustContext {
    pub cwd: String,
    pub mode: ExtensionMode,
    pub has_ui: bool,
    pub ui: Arc<dyn ExtensionUIContext>,
}

pub type ProjectTrustHandler = Arc<
    dyn Fn(ProjectTrustEvent, ProjectTrustContext) -> BoxFuture<pi_js::Result<ProjectTrustEventResult>> + Send + Sync,
>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourcesDiscoverReason {
    Startup,
    Reload,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesDiscoverEvent {
    pub cwd: String,
    pub reason: ResourcesDiscoverReason,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesDiscoverResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill_paths: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_paths: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme_paths: Option<Vec<String>>,
}

impl ExtensionEvent for ResourcesDiscoverEvent {
    const TYPE: &'static str = "resources_discover";
    type Output = ResourcesDiscoverResult;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServersChangeEvent {
    pub servers: Vec<mcp_wire::RegisteredMcpServer>,
}

impl ExtensionEvent for McpServersChangeEvent {
    const TYPE: &'static str = "mcp_servers_change";
    type Output = ();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStartReason {
    Startup,
    Reload,
    New,
    Resume,
    Fork,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStartEvent {
    pub reason: SessionStartReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_session_file: Option<String>,
}

impl ExtensionEvent for SessionStartEvent {
    const TYPE: &'static str = "session_start";
    type Output = ();
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfoChangedEvent {
    pub name: Option<String>,
}

impl ExtensionEvent for SessionInfoChangedEvent {
    const TYPE: &'static str = "session_info_changed";
    type Output = ();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionBeforeSwitchReason {
    New,
    Resume,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeforeSwitchEvent {
    pub reason: SessionBeforeSwitchReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_session_file: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeforeSwitchResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel: Option<bool>,
}

impl ExtensionEvent for SessionBeforeSwitchEvent {
    const TYPE: &'static str = "session_before_switch";
    type Output = SessionBeforeSwitchResult;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeforeForkEvent {
    pub entry_id: String,
    pub position: ForkPosition,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeforeForkResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_conversation_restore: Option<bool>,
}

impl ExtensionEvent for SessionBeforeForkEvent {
    const TYPE: &'static str = "session_before_fork";
    type Output = SessionBeforeForkResult;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompactionReason {
    Manual,
    Threshold,
    Overflow,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeforeCompactEvent {
    pub preparation: mcp_wire::CompactionPreparation,
    pub branch_entries: Vec<mcp_wire::SessionEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<String>,
    pub reason: CompactionReason,
    pub will_retry: bool,
    #[serde(skip)]
    pub signal: Option<AbortSignal>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeforeCompactResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction: Option<mcp_wire::CompactionResult>,
}

impl ExtensionEvent for SessionBeforeCompactEvent {
    const TYPE: &'static str = "session_before_compact";
    type Output = SessionBeforeCompactResult;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCompactEvent {
    pub compaction_entry: mcp_wire::CompactionEntry,
    pub from_extension: bool,
    pub reason: CompactionReason,
    pub will_retry: bool,
}

impl ExtensionEvent for SessionCompactEvent {
    const TYPE: &'static str = "session_compact";
    type Output = ();
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCompactFailedEvent {
    pub reason: CompactionReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub aborted: bool,
    pub will_retry: bool,
    pub from_extension: bool,
}

impl ExtensionEvent for SessionCompactFailedEvent {
    const TYPE: &'static str = "session_compact_failed";
    type Output = ();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionShutdownReason {
    Quit,
    Reload,
    New,
    Resume,
    Fork,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionShutdownEvent {
    pub reason: SessionShutdownReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_session_file: Option<String>,
}

impl ExtensionEvent for SessionShutdownEvent {
    const TYPE: &'static str = "session_shutdown";
    type Output = ();
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreePreparation {
    pub target_id: String,
    pub old_leaf_id: Option<String>,
    pub common_ancestor_id: Option<String>,
    pub entries_to_summarize: Vec<mcp_wire::SessionEntry>,
    pub user_wants_summary: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replace_instructions: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeforeTreeEvent {
    pub preparation: TreePreparation,
    #[serde(skip)]
    pub signal: Option<AbortSignal>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeSummaryOverride {
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeforeTreeResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<TreeSummaryOverride>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replace_instructions: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl ExtensionEvent for SessionBeforeTreeEvent {
    const TYPE: &'static str = "session_before_tree";
    type Output = SessionBeforeTreeResult;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTreeEvent {
    pub new_leaf_id: Option<String>,
    pub old_leaf_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_entry: Option<mcp_wire::BranchSummaryEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_extension: Option<bool>,
}

impl ExtensionEvent for SessionTreeEvent {
    const TYPE: &'static str = "session_tree";
    type Output = ();
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEvent {
    pub messages: Vec<AgentMessage>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEventResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<AgentMessage>>,
}

impl ExtensionEvent for ContextEvent {
    const TYPE: &'static str = "context";
    type Output = ContextEventResult;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextWithSystemEvent {
    pub messages: Vec<AgentMessage>,
}

impl ExtensionEvent for ContextWithSystemEvent {
    const TYPE: &'static str = "context_with_system";
    type Output = ContextEventResult;
}

impl ExtensionEvent for CacheWarmingDecisionEvent {
    const TYPE: &'static str = "cache_warming_decision";
    type Output = CacheWarmingDecisionEventResult;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeProviderRequestEvent {
    pub payload: Value,
}

pub type BeforeProviderRequestEventResult = Value;

impl ExtensionEvent for BeforeProviderRequestEvent {
    const TYPE: &'static str = "before_provider_request";
    type Output = BeforeProviderRequestEventResult;
}

#[derive(Clone, Debug)]
pub struct BeforeProviderHeadersEvent {
    pub headers: Arc<Mutex<ProviderHeaders>>,
}

impl ExtensionEvent for BeforeProviderHeadersEvent {
    const TYPE: &'static str = "before_provider_headers";
    type Output = ();
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AfterProviderResponseEvent {
    pub status: i64,
    pub headers: IndexMap<String, String>,
}

impl ExtensionEvent for AfterProviderResponseEvent {
    const TYPE: &'static str = "after_provider_response";
    type Output = ();
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStreamEvent {
    pub provider: ProviderId,
    pub api: Api,
    pub model: String,
    pub data: Value,
}

impl ExtensionEvent for ProviderStreamEvent {
    const TYPE: &'static str = "provider_stream_event";
    type Output = ();
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeAgentStartEvent {
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageContent>>,
    pub system_prompt: String,
    pub system_prompt_options: Arc<Mutex<NormalizedBuildSystemPromptOptions>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeAgentStartEventResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<mcp_wire::CustomMessageDraft>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
}

impl ExtensionEvent for BeforeAgentStartEvent {
    const TYPE: &'static str = "before_agent_start";
    type Output = BeforeAgentStartEventResult;
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStartEvent {}

impl ExtensionEvent for AgentStartEvent {
    const TYPE: &'static str = "agent_start";
    type Output = ();
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentEndEvent {
    pub messages: Vec<AgentMessage>,
}

impl ExtensionEvent for AgentEndEvent {
    const TYPE: &'static str = "agent_end";
    type Output = ();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentActivityOutcome {
    Completed,
    Aborted,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SessionBoundaryDraft {
    #[serde(rename = "custom", rename_all = "camelCase")]
    Custom {
        custom_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<Value>,
    },
    #[serde(rename = "custom_message", rename_all = "camelCase")]
    CustomMessage {
        custom_type: String,
        content: mcp_wire::CustomMessageContent,
        display: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
    },
    #[serde(rename = "context_edit", rename_all = "camelCase")]
    ContextEdit {
        target_id: String,
        replacement: Option<mcp_wire::ContextEditReplacement>,
    },
    #[serde(rename = "compaction", rename_all = "camelCase")]
    Compaction {
        summary: String,
        first_kept_entry_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundaryContextPreview {
    pub context_entries: Vec<mcp_wire::ProjectedSessionEntry>,
    pub context_messages: Vec<AgentMessage>,
    pub llm_messages: Vec<Message>,
    pub pending_messages: Vec<AgentMessage>,
    pub can_continue: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundaryResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<SessionBoundaryDraft>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#continue: Option<bool>,
}

pub type TurnEndEventResult = BoundaryResult;
pub type AgentBeforeSettleEventResult = BoundaryResult;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentBeforeSettleEvent {
    pub entries: Vec<SessionBoundaryDraft>,
    pub r#continue: bool,
    pub context: BoundaryContextPreview,
    pub outcome: AgentActivityOutcome,
}

impl ExtensionEvent for AgentBeforeSettleEvent {
    const TYPE: &'static str = "agent_before_settle";
    type Output = AgentBeforeSettleEventResult;
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSettledEvent {}

impl ExtensionEvent for AgentSettledEvent {
    const TYPE: &'static str = "agent_settled";
    type Output = ();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UIPromptKind {
    Select,
    Confirm,
    Input,
    Editor,
    Custom,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UIPromptStartEvent {
    pub reason: String,
    pub kind: UIPromptKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl ExtensionEvent for UIPromptStartEvent {
    const TYPE: &'static str = "ui_prompt_start";
    type Output = ();
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UIPromptEndEvent {
    pub reason: String,
    pub kind: UIPromptKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl ExtensionEvent for UIPromptEndEvent {
    const TYPE: &'static str = "ui_prompt_end";
    type Output = ();
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartEvent {
    pub turn_index: i64,
    pub timestamp: i64,
}

impl ExtensionEvent for TurnStartEvent {
    const TYPE: &'static str = "turn_start";
    type Output = ();
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnEndEvent {
    pub entries: Vec<SessionBoundaryDraft>,
    pub r#continue: bool,
    pub context: BoundaryContextPreview,
    pub outcome: AgentActivityOutcome,
    pub turn_index: i64,
    pub message: AgentMessage,
    pub tool_results: Vec<ToolResultMessage>,
    pub message_entry_id: String,
    pub tool_result_entry_ids: Vec<String>,
}

impl ExtensionEvent for TurnEndEvent {
    const TYPE: &'static str = "turn_end";
    type Output = TurnEndEventResult;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageStartEvent {
    pub message: AgentMessage,
}

impl ExtensionEvent for MessageStartEvent {
    const TYPE: &'static str = "message_start";
    type Output = ();
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageUpdateEvent {
    pub message: AgentMessage,
    pub assistant_message_event: AssistantMessageEvent,
}

impl ExtensionEvent for MessageUpdateEvent {
    const TYPE: &'static str = "message_update";
    type Output = ();
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageEndEvent {
    pub message: AgentMessage,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageEndEventResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<AgentMessage>,
}

impl ExtensionEvent for MessageEndEvent {
    const TYPE: &'static str = "message_end";
    type Output = MessageEndEventResult;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionStartEvent {
    pub tool_call_id: String,
    pub tool_name: String,
    pub args: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
}

impl ExtensionEvent for ToolExecutionStartEvent {
    const TYPE: &'static str = "tool_execution_start";
    type Output = ();
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionUpdateEvent {
    pub tool_call_id: String,
    pub tool_name: String,
    pub args: Value,
    pub partial_result: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
}

impl ExtensionEvent for ToolExecutionUpdateEvent {
    const TYPE: &'static str = "tool_execution_update";
    type Output = ();
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionEndEvent {
    pub tool_call_id: String,
    pub tool_name: String,
    pub result: Value,
    pub is_error: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
}

impl ExtensionEvent for ToolExecutionEndEvent {
    const TYPE: &'static str = "tool_execution_end";
    type Output = ();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelSelectSource {
    Set,
    Cycle,
    Restore,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelectEvent {
    pub model: Model,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_model: Option<Model>,
    pub source: ModelSelectSource,
}

impl ExtensionEvent for ModelSelectEvent {
    const TYPE: &'static str = "model_select";
    type Output = ();
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingLevelSelectEvent {
    pub level: ThinkingLevel,
    pub previous_level: ThinkingLevel,
}

impl ExtensionEvent for ThinkingLevelSelectEvent {
    const TYPE: &'static str = "thinking_level_select";
    type Output = ();
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserBashEvent {
    pub command: String,
    pub exclude_from_context: bool,
    pub cwd: String,
}

#[derive(Clone)]
pub enum UserBashEventResult {
    Operations { operations: Arc<dyn BashOperations> },
    Result { result: mcp_wire::BashResult },
}

impl ExtensionEvent for UserBashEvent {
    const TYPE: &'static str = "user_bash";
    type Output = UserBashEventResult;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputSource {
    Interactive,
    Rpc,
    Extension,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputEvent {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageContent>>,
    pub source: InputSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub streaming_behavior: Option<InputDeliveryBehavior>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InputDeliveryBehavior {
    Steer,
    FollowUp,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action")]
pub enum InputEventResult {
    #[serde(rename = "continue")]
    Continue,
    #[serde(rename = "transform")]
    Transform {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        images: Option<Vec<ImageContent>>,
    },
    #[serde(rename = "handled")]
    Handled,
}

impl ExtensionEvent for InputEvent {
    const TYPE: &'static str = "input";
    type Output = InputEventResult;
}

// ---------------------------------------------------------------------------
// ToolCallEvent & ToolResultEvent
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashToolCallEvent {
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    pub input: BashToolInput,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PowerShellToolCallEvent {
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    pub input: PowerShellToolInput,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadToolCallEvent {
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    pub input: ReadToolInput,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditToolCallEvent {
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    pub input: EditToolInput,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteToolCallEvent {
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    pub input: WriteToolInput,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepToolCallEvent {
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    pub input: GrepToolInput,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindToolCallEvent {
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    pub input: FindToolInput,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LsToolCallEvent {
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    pub input: LsToolInput,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomToolCallEvent {
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    pub tool_name: String,
    pub input: IndexMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ToolCallEvent {
    Bash(BashToolCallEvent),
    PowerShell(PowerShellToolCallEvent),
    Read(ReadToolCallEvent),
    Edit(EditToolCallEvent),
    Write(WriteToolCallEvent),
    Grep(GrepToolCallEvent),
    Find(FindToolCallEvent),
    Ls(LsToolCallEvent),
    Custom(CustomToolCallEvent),
}

impl ToolCallEvent {
    pub fn tool_name(&self) -> &str {
        match self {
            Self::Bash(_) => "bash",
            Self::PowerShell(_) => "powershell",
            Self::Read(_) => "read",
            Self::Edit(_) => "edit",
            Self::Write(_) => "write",
            Self::Grep(_) => "grep",
            Self::Find(_) => "find",
            Self::Ls(_) => "ls",
            Self::Custom(e) => &e.tool_name,
        }
    }

    pub fn tool_call_id(&self) -> &str {
        match self {
            Self::Bash(e) => &e.tool_call_id,
            Self::PowerShell(e) => &e.tool_call_id,
            Self::Read(e) => &e.tool_call_id,
            Self::Edit(e) => &e.tool_call_id,
            Self::Write(e) => &e.tool_call_id,
            Self::Grep(e) => &e.tool_call_id,
            Self::Find(e) => &e.tool_call_id,
            Self::Ls(e) => &e.tool_call_id,
            Self::Custom(e) => &e.tool_call_id,
        }
    }

    pub fn parent_tool_call_id(&self) -> Option<&str> {
        match self {
            Self::Bash(e) => e.parent_tool_call_id.as_deref(),
            Self::PowerShell(e) => e.parent_tool_call_id.as_deref(),
            Self::Read(e) => e.parent_tool_call_id.as_deref(),
            Self::Edit(e) => e.parent_tool_call_id.as_deref(),
            Self::Write(e) => e.parent_tool_call_id.as_deref(),
            Self::Grep(e) => e.parent_tool_call_id.as_deref(),
            Self::Find(e) => e.parent_tool_call_id.as_deref(),
            Self::Ls(e) => e.parent_tool_call_id.as_deref(),
            Self::Custom(e) => e.parent_tool_call_id.as_deref(),
        }
    }

    pub fn input_value(&self) -> Value {
        match self {
            Self::Bash(e) => serde_json::to_value(&e.input).unwrap_or(Value::Null),
            Self::PowerShell(e) => serde_json::to_value(&e.input).unwrap_or(Value::Null),
            Self::Read(e) => serde_json::to_value(&e.input).unwrap_or(Value::Null),
            Self::Edit(e) => serde_json::to_value(&e.input).unwrap_or(Value::Null),
            Self::Write(e) => serde_json::to_value(&e.input).unwrap_or(Value::Null),
            Self::Grep(e) => serde_json::to_value(&e.input).unwrap_or(Value::Null),
            Self::Find(e) => serde_json::to_value(&e.input).unwrap_or(Value::Null),
            Self::Ls(e) => serde_json::to_value(&e.input).unwrap_or(Value::Null),
            Self::Custom(e) => serde_json::to_value(&e.input).unwrap_or(Value::Null),
        }
    }

    pub fn from_tool_name_and_input(
        tool_call_id: String,
        parent_tool_call_id: Option<String>,
        tool_name: &str,
        input: Value,
    ) -> Self {
        match tool_name {
            "bash" => {
                if let Ok(parsed) = serde_json::from_value::<BashToolInput>(input.clone()) {
                    return Self::Bash(BashToolCallEvent {
                        tool_call_id,
                        parent_tool_call_id,
                        input: parsed,
                    });
                }
            }
            "powershell" => {
                if let Ok(parsed) = serde_json::from_value::<PowerShellToolInput>(input.clone()) {
                    return Self::PowerShell(PowerShellToolCallEvent {
                        tool_call_id,
                        parent_tool_call_id,
                        input: parsed,
                    });
                }
            }
            "read" => {
                if let Ok(parsed) = serde_json::from_value::<ReadToolInput>(input.clone()) {
                    return Self::Read(ReadToolCallEvent {
                        tool_call_id,
                        parent_tool_call_id,
                        input: parsed,
                    });
                }
            }
            "edit" => {
                if let Ok(parsed) = serde_json::from_value::<EditToolInput>(input.clone()) {
                    return Self::Edit(EditToolCallEvent {
                        tool_call_id,
                        parent_tool_call_id,
                        input: parsed,
                    });
                }
            }
            "write" => {
                if let Ok(parsed) = serde_json::from_value::<WriteToolInput>(input.clone()) {
                    return Self::Write(WriteToolCallEvent {
                        tool_call_id,
                        parent_tool_call_id,
                        input: parsed,
                    });
                }
            }
            "grep" => {
                if let Ok(parsed) = serde_json::from_value::<GrepToolInput>(input.clone()) {
                    return Self::Grep(GrepToolCallEvent {
                        tool_call_id,
                        parent_tool_call_id,
                        input: parsed,
                    });
                }
            }
            "find" => {
                if let Ok(parsed) = serde_json::from_value::<FindToolInput>(input.clone()) {
                    return Self::Find(FindToolCallEvent {
                        tool_call_id,
                        parent_tool_call_id,
                        input: parsed,
                    });
                }
            }
            "ls" => {
                if let Ok(parsed) = serde_json::from_value::<LsToolInput>(input.clone()) {
                    return Self::Ls(LsToolCallEvent {
                        tool_call_id,
                        parent_tool_call_id,
                        input: parsed,
                    });
                }
            }
            _ => {}
        }
        let map = serde_json::from_value::<IndexMap<String, Value>>(input).unwrap_or_default();
        Self::Custom(CustomToolCallEvent {
            tool_call_id,
            parent_tool_call_id,
            tool_name: tool_name.to_string(),
            input: map,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallEventResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

impl ExtensionEvent for ToolCallEvent {
    const TYPE: &'static str = "tool_call";
    type Output = ToolCallEventResult;
}

pub fn is_tool_call_event_type(tool_name: &str, event: &ToolCallEvent) -> bool {
    event.tool_name() == tool_name
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolResultContentBlock {
    #[serde(rename = "text")]
    Text(TextContent),
    #[serde(rename = "image")]
    Image(ImageContent),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashToolResultEvent {
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    pub input: IndexMap<String, Value>,
    pub content: Vec<ToolResultContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<JsonValue>,
    pub is_error: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub details: Option<BashToolDetails>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PowerShellToolResultEvent {
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    pub input: IndexMap<String, Value>,
    pub content: Vec<ToolResultContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<JsonValue>,
    pub is_error: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub details: Option<PowerShellToolDetails>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadToolResultEvent {
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    pub input: IndexMap<String, Value>,
    pub content: Vec<ToolResultContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<JsonValue>,
    pub is_error: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub details: Option<ReadToolDetails>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditToolResultEvent {
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    pub input: IndexMap<String, Value>,
    pub content: Vec<ToolResultContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<JsonValue>,
    pub is_error: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub details: Option<EditToolDetails>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteToolResultEvent {
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    pub input: IndexMap<String, Value>,
    pub content: Vec<ToolResultContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<JsonValue>,
    pub is_error: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepToolResultEvent {
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    pub input: IndexMap<String, Value>,
    pub content: Vec<ToolResultContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<JsonValue>,
    pub is_error: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub details: Option<GrepToolDetails>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindToolResultEvent {
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    pub input: IndexMap<String, Value>,
    pub content: Vec<ToolResultContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<JsonValue>,
    pub is_error: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub details: Option<FindToolDetails>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LsToolResultEvent {
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    pub input: IndexMap<String, Value>,
    pub content: Vec<ToolResultContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<JsonValue>,
    pub is_error: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub details: Option<LsToolDetails>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomToolResultEvent {
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    pub tool_name: String,
    pub input: IndexMap<String, Value>,
    pub content: Vec<ToolResultContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<JsonValue>,
    pub is_error: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ToolResultEvent {
    Bash(BashToolResultEvent),
    PowerShell(PowerShellToolResultEvent),
    Read(ReadToolResultEvent),
    Edit(EditToolResultEvent),
    Write(WriteToolResultEvent),
    Grep(GrepToolResultEvent),
    Find(FindToolResultEvent),
    Ls(LsToolResultEvent),
    Custom(CustomToolResultEvent),
}

impl ToolResultEvent {
    pub fn tool_name(&self) -> &str {
        match self {
            Self::Bash(_) => "bash",
            Self::PowerShell(_) => "powershell",
            Self::Read(_) => "read",
            Self::Edit(_) => "edit",
            Self::Write(_) => "write",
            Self::Grep(_) => "grep",
            Self::Find(_) => "find",
            Self::Ls(_) => "ls",
            Self::Custom(e) => &e.tool_name,
        }
    }

    pub fn is_bash(&self) -> bool {
        matches!(self, Self::Bash(_))
    }
    pub fn is_powershell(&self) -> bool {
        matches!(self, Self::PowerShell(_))
    }
    pub fn is_read(&self) -> bool {
        matches!(self, Self::Read(_))
    }
    pub fn is_edit(&self) -> bool {
        matches!(self, Self::Edit(_))
    }
    pub fn is_write(&self) -> bool {
        matches!(self, Self::Write(_))
    }
    pub fn is_grep(&self) -> bool {
        matches!(self, Self::Grep(_))
    }
    pub fn is_find(&self) -> bool {
        matches!(self, Self::Find(_))
    }
    pub fn is_ls(&self) -> bool {
        matches!(self, Self::Ls(_))
    }
}

pub fn is_bash_tool_result(e: &ToolResultEvent) -> bool {
    e.is_bash()
}
pub fn is_power_shell_tool_result(e: &ToolResultEvent) -> bool {
    e.is_powershell()
}
pub fn is_read_tool_result(e: &ToolResultEvent) -> bool {
    e.is_read()
}
pub fn is_edit_tool_result(e: &ToolResultEvent) -> bool {
    e.is_edit()
}
pub fn is_write_tool_result(e: &ToolResultEvent) -> bool {
    e.is_write()
}
pub fn is_grep_tool_result(e: &ToolResultEvent) -> bool {
    e.is_grep()
}
pub fn is_find_tool_result(e: &ToolResultEvent) -> bool {
    e.is_find()
}
pub fn is_ls_tool_result(e: &ToolResultEvent) -> bool {
    e.is_ls()
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultEventResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ToolResultContentBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

impl ExtensionEvent for ToolResultEvent {
    const TYPE: &'static str = "tool_result";
    type Output = ToolResultEventResult;
}

// ============================================================================
// Message and Entry Rendering
// ============================================================================

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageRenderOptions {
    pub expanded: bool,
    pub output_pad: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MarkdownMessageType {
    User,
    Assistant,
    AssistantThinking,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownTransformContext {
    pub message_type: MarkdownMessageType,
    pub is_streaming: bool,
    pub available_width: i64,
}

pub type MarkdownTransformer = Arc<dyn Fn(&str, MarkdownTransformContext) -> String + Send + Sync>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryRenderOptions {
    pub expanded: bool,
}

pub type MessageRenderer =
    Arc<dyn Fn(mcp_wire::CustomMessage, MessageRenderOptions, Theme) -> Option<Arc<dyn Component>> + Send + Sync>;

pub type EntryRenderer =
    Arc<dyn Fn(mcp_wire::CustomEntry, EntryRenderOptions, Theme) -> Option<Arc<dyn Component>> + Send + Sync>;

// ============================================================================
// Command, Shortcut, Flag Registration
// ============================================================================

pub type CommandCompletionsFn = Arc<dyn Fn(&str) -> BoxFuture<Option<Vec<AutocompleteItem>>> + Send + Sync>;
pub type CommandHandlerFn = Arc<dyn Fn(String, ExtensionCommandContext) -> BoxFuture<pi_js::Result<()>> + Send + Sync>;

#[derive(Clone)]
pub struct RegisteredCommandOptions {
    pub description: Option<String>,
    pub get_argument_completions: Option<CommandCompletionsFn>,
    pub handler: CommandHandlerFn,
}

#[derive(Clone)]
pub struct RegisteredCommand {
    pub name: String,
    pub source_info: SourceInfo,
    pub description: Option<String>,
    pub get_argument_completions: Option<CommandCompletionsFn>,
    pub handler: CommandHandlerFn,
}

#[derive(Clone)]
pub struct ResolvedCommand {
    pub command: RegisteredCommand,
    pub invocation_name: String,
}

pub type ShortcutHandlerFn = Arc<dyn Fn(ExtensionContext) -> BoxFuture<pi_js::Result<()>> + Send + Sync>;

#[derive(Clone)]
pub struct ShortcutOptions {
    pub description: Option<String>,
    pub handler: ShortcutHandlerFn,
}

#[derive(Clone)]
pub struct ExtensionShortcut {
    pub shortcut: KeyId,
    pub description: Option<String>,
    pub handler: ShortcutHandlerFn,
    pub extension_path: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FlagType {
    Boolean,
    String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FlagValue {
    Bool(bool),
    String(String),
}

#[derive(Clone, Debug, Default)]
pub struct FlagOptions {
    pub description: Option<String>,
    pub r#type: Option<FlagType>,
    pub default: Option<FlagValue>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionFlag {
    pub name: String,
    pub description: Option<String>,
    pub r#type: FlagType,
    pub default: Option<FlagValue>,
    pub extension_path: String,
}

// ============================================================================
// Message / Actions options
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeliverAs {
    Steer,
    FollowUp,
    NextTurn,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_turn: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deliver_as: Option<DeliverAs>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendUserMessageOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deliver_as: Option<InputDeliveryBehavior>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expand_prompt_templates: Option<bool>,
}

// ============================================================================
// Provider Registration Types
// ============================================================================

#[derive(Clone)]
pub struct ExtensionVirtualModel {
    pub provider: String,
    pub id: String,
    pub name: String,
    pub thinking_levels: Option<Vec<pi_ai::types::ModelThinkingLevel>>,
    pub route: Arc<
        dyn Fn(mcp_wire::ModelRouteRequest, ExtensionContext) -> BoxFuture<pi_js::Result<mcp_wire::ModelRoute>>
            + Send
            + Sync,
    >,
}

pub type CustomStreamSimpleFn =
    Arc<dyn Fn(Model, TranscriptContext, Option<SimpleStreamOptions>) -> AssistantMessageEventStream + Send + Sync>;

#[derive(Clone)]
pub struct ProviderOAuthConfig {
    pub name: String,
    pub is_subscription: Option<bool>,
    pub uses_callback_server: Option<bool>,
    pub login: Arc<dyn Fn(Arc<dyn OAuthLoginCallbacks>) -> BoxFuture<pi_js::Result<OAuthCredentials>> + Send + Sync>,
    pub refresh_token:
        Arc<dyn Fn(OAuthCredentials, AbortSignal) -> BoxFuture<pi_js::Result<OAuthCredentials>> + Send + Sync>,
    pub get_api_key: Arc<dyn Fn(&OAuthCredentials) -> String + Send + Sync>,
    pub modify_models: Option<Arc<dyn Fn(Vec<Model>, &OAuthCredentials) -> Vec<Model> + Send + Sync>>,
}

#[derive(Clone, Default)]
pub struct ProviderConfig {
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub api: Option<Api>,
    pub stream_simple: Option<CustomStreamSimpleFn>,
    pub images: Option<IndexMap<ImageApi, ProviderImages>>,
    pub classifiers: Option<IndexMap<ClassifierApi, ProviderClassifier>>,
    pub headers: Option<IndexMap<String, String>>,
    pub auth_header: Option<bool>,
    pub models: Option<Vec<ProviderModelConfig>>,
    pub refresh_models:
        Option<Arc<dyn Fn(RefreshModelsContext) -> BoxFuture<pi_js::Result<Vec<ProviderModelConfig>>> + Send + Sync>>,
    pub oauth: Option<ProviderOAuthConfig>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderChatModelConfig {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api: Option<Api>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub input: Vec<pi_ai::types::ModelInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_limits: Option<pi_ai::types::ModelInputLimits>,
    pub cost: pi_ai::types::ModelCost,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
    pub reasoning: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<pi_ai::types::ThinkingLevelMap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache: Option<pi_ai::types::ModelPromptCache>,
    pub context_window: i64,
    pub max_tokens: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling_params: Option<IndexMap<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<pi_ai::types::ModelCompat>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderImageModelConfig {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api: Option<ImageApi>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub input: Vec<pi_ai::types::ModelInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_limits: Option<pi_ai::types::ModelInputLimits>,
    pub cost: pi_ai::types::ModelCost,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
    pub output: Vec<pi_ai::types::ModelInput>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderClassifierModelConfig {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api: Option<ClassifierApi>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub input: Vec<pi_ai::types::ModelInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_limits: Option<pi_ai::types::ModelInputLimits>,
    pub cost: pi_ai::types::ModelCost,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
    pub context_window: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ProviderModelConfig {
    Chat(ProviderChatModelConfig),
    Image(ProviderImageModelConfig),
    Classifier(ProviderClassifierModelConfig),
}

// ============================================================================
// Factory / InlineExtension (PORTING.md §10.1)
// ============================================================================

#[async_trait]
pub trait Extension: Send + Sync + 'static {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()>;

    fn host_keepalive(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }
}

#[async_trait]
impl<F, Fut> Extension for F
where
    F: Fn(ExtensionApi) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = pi_js::Result<()>> + Send + 'static,
{
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        (self)(pi).await
    }
}

pub type ExtensionFactory = Arc<dyn Extension>;

#[derive(Clone)]
pub struct NamedInlineExtension {
    pub name: String,
    pub factory: ExtensionFactory,
    pub hidden: bool,
    pub replaceable: bool,
    pub builtin: bool,
}

#[derive(Clone)]
pub enum InlineExtension {
    Factory(ExtensionFactory),
    Named(NamedInlineExtension),
}

// ============================================================================
// Runtime & ExtensionApi (PORTING.md §10.2 & §10.3)
// ============================================================================

#[derive(Clone)]
pub struct RegisteredTool {
    pub definition: ToolDefinition,
    pub source_info: SourceInfo,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub parameters: Schema,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_guidelines: Option<Vec<String>>,
    pub exposure: ToolExposure,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<ToolNamespace>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
    pub source_info: SourceInfo,
}

/// Unified stored handler invoked by `ExtensionRunner`.
#[derive(Clone)]
pub enum RegisteredEventHandler {
    Raw(Arc<dyn Fn(Value, ExtensionContext) -> BoxFuture<pi_js::Result<Option<Value>>> + Send + Sync>),
    Boxed(
        Arc<
            dyn Fn(
                    Arc<dyn Any + Send + Sync>,
                    ExtensionContext,
                ) -> BoxFuture<pi_js::Result<Option<Box<dyn Any + Send>>>>
                + Send
                + Sync,
        >,
    ),
}

/// Loaded extension record with all registered items.
#[derive(Clone)]
pub struct LoadedExtension {
    pub path: String,
    pub resolved_path: String,
    pub hidden: bool,
    pub replaceable: bool,
    pub source_info: SourceInfo,
    pub handlers: Arc<Mutex<IndexMap<String, Vec<(u64, RegisteredEventHandler)>>>>,
    pub project_trust_handlers: Arc<Mutex<Vec<(u64, ProjectTrustHandler)>>>,
    pub tools: Arc<Mutex<IndexMap<String, RegisteredTool>>>,
    pub message_renderers: Arc<Mutex<IndexMap<String, MessageRenderer>>>,
    pub markdown_transformer: Arc<Mutex<Option<MarkdownTransformer>>>,
    pub entry_renderers: Arc<Mutex<IndexMap<String, EntryRenderer>>>,
    pub commands: Arc<Mutex<IndexMap<String, RegisteredCommand>>>,
    pub flags: Arc<Mutex<IndexMap<String, ExtensionFlag>>>,
    pub shortcuts: Arc<Mutex<IndexMap<KeyId, ExtensionShortcut>>>,
    /// Opaque runtime handle kept alive as long as the loaded extension is active (`None` on `ExtensionApi`).
    pub host_keepalive: Arc<Mutex<Option<Arc<dyn Any + Send + Sync>>>>,
}

impl LoadedExtension {
    pub fn new(path: impl Into<String>, resolved_path: impl Into<String>, source_info: SourceInfo) -> Self {
        Self {
            path: path.into(),
            resolved_path: resolved_path.into(),
            hidden: false,
            replaceable: false,
            source_info,
            handlers: Arc::new(Mutex::new(IndexMap::new())),
            project_trust_handlers: Arc::new(Mutex::new(Vec::new())),
            tools: Arc::new(Mutex::new(IndexMap::new())),
            message_renderers: Arc::new(Mutex::new(IndexMap::new())),
            markdown_transformer: Arc::new(Mutex::new(None)),
            entry_renderers: Arc::new(Mutex::new(IndexMap::new())),
            commands: Arc::new(Mutex::new(IndexMap::new())),
            flags: Arc::new(Mutex::new(IndexMap::new())),
            shortcuts: Arc::new(Mutex::new(IndexMap::new())),
            host_keepalive: Arc::new(Mutex::new(None)),
        }
    }
}

#[derive(Clone)]
pub struct PendingProviderRegistration {
    pub name: String,
    pub config: ProviderConfig,
    pub extension_path: String,
}

#[derive(Clone)]
pub struct PendingNativeProviderRegistration {
    pub provider: Arc<dyn Provider>,
    pub extension_path: String,
}

#[derive(Clone)]
pub struct PendingVirtualModelRegistration {
    pub definition: mcp_wire::VirtualModelDefinition,
    pub extension_path: String,
}

pub type SendMessageHandler = Arc<dyn Fn(mcp_wire::CustomMessageDraft, Option<SendMessageOptions>) + Send + Sync>;
pub type SendUserMessageHandler =
    Arc<dyn Fn(mcp_wire::CustomMessageContent, Option<SendUserMessageOptions>) + Send + Sync>;
pub type AppendEntryHandler = Arc<dyn Fn(&str, Option<Value>) + Send + Sync>;
pub type SetSessionNameHandler = Arc<dyn Fn(&str) + Send + Sync>;
pub type GetSessionNameHandler = Arc<dyn Fn() -> Option<String> + Send + Sync>;
pub type SetLabelHandler = Arc<dyn Fn(&str, Option<&str>) + Send + Sync>;
pub type GetActiveToolsHandler = Arc<dyn Fn() -> Vec<String> + Send + Sync>;
pub type GetAllToolsHandler = Arc<dyn Fn() -> Vec<ToolInfo> + Send + Sync>;
pub type GetSettingsHandler = Arc<dyn Fn() -> mcp_wire::Settings + Send + Sync>;
pub type SetActiveToolsHandler = Arc<dyn Fn(Vec<String>) + Send + Sync>;
pub type RefreshToolsHandler = Arc<dyn Fn() + Send + Sync>;
pub type GetCommandsHandler = Arc<dyn Fn() -> Vec<SlashCommandInfo> + Send + Sync>;
pub type SetModelHandler = Arc<dyn Fn(Model) -> BoxFuture<pi_js::Result<bool>> + Send + Sync>;
pub type GetThinkingLevelHandler = Arc<dyn Fn() -> ThinkingLevel + Send + Sync>;
pub type SetThinkingLevelHandler = Arc<dyn Fn(ThinkingLevel) + Send + Sync>;

#[derive(Clone)]
pub struct ExtensionActions {
    pub send_message: SendMessageHandler,
    pub send_user_message: SendUserMessageHandler,
    pub append_entry: AppendEntryHandler,
    pub set_session_name: SetSessionNameHandler,
    pub get_session_name: GetSessionNameHandler,
    pub set_label: SetLabelHandler,
    pub get_active_tools: GetActiveToolsHandler,
    pub get_all_tools: GetAllToolsHandler,
    pub get_settings: GetSettingsHandler,
    pub set_active_tools: SetActiveToolsHandler,
    pub refresh_tools: RefreshToolsHandler,
    pub get_commands: GetCommandsHandler,
    pub set_model: SetModelHandler,
    pub get_thinking_level: GetThinkingLevelHandler,
    pub set_thinking_level: SetThinkingLevelHandler,
}

pub struct ExtensionContextActions {
    pub get_model: Arc<dyn Fn() -> Option<Model> + Send + Sync>,
    pub get_scoped_models: Arc<dyn Fn() -> Vec<mcp_wire::ScopedModel> + Send + Sync>,
    pub is_idle: Arc<dyn Fn() -> bool + Send + Sync>,
    pub is_project_trusted: Arc<dyn Fn() -> bool + Send + Sync>,
    pub get_signal: Arc<dyn Fn() -> Option<AbortSignal> + Send + Sync>,
    pub abort: Arc<dyn Fn() + Send + Sync>,
    pub has_pending_messages: Arc<dyn Fn() -> bool + Send + Sync>,
    pub shutdown: Arc<dyn Fn() + Send + Sync>,
    pub get_context_usage: Arc<dyn Fn() -> Option<ContextUsage> + Send + Sync>,
    pub compact: Arc<dyn Fn(Option<CompactOptions>) + Send + Sync>,
    pub get_system_prompt: Arc<dyn Fn() -> String + Send + Sync>,
    pub get_system_prompt_options: Option<Arc<dyn Fn() -> BuildSystemPromptOptions + Send + Sync>>,
    pub execute_tool:
        Option<Arc<dyn Fn(&str, &str, Value, ExecuteToolOptions) -> BoxFuture<AgentToolCallOutcome> + Send + Sync>>,
    pub get_callable_tools: Option<Arc<dyn Fn() -> Vec<AgentTool> + Send + Sync>>,
}

pub struct ExtensionCommandContextActions {
    pub wait_for_idle: Arc<dyn Fn() -> BoxFuture<pi_js::Result<()>> + Send + Sync>,
    pub new_session: Arc<dyn Fn(Option<NewSessionOptions>) -> BoxFuture<pi_js::Result<CancelledResult>> + Send + Sync>,
    pub fork: Arc<dyn Fn(&str, Option<ForkOptions>) -> BoxFuture<pi_js::Result<CancelledResult>> + Send + Sync>,
    pub navigate_tree:
        Arc<dyn Fn(&str, Option<NavigateTreeOptions>) -> BoxFuture<pi_js::Result<CancelledResult>> + Send + Sync>,
    pub switch_session:
        Arc<dyn Fn(&str, Option<SwitchSessionOptions>) -> BoxFuture<pi_js::Result<CancelledResult>> + Send + Sync>,
    pub reload: Arc<dyn Fn() -> BoxFuture<pi_js::Result<()>> + Send + Sync>,
}

pub struct ExtensionRuntimeInner {
    pub flag_values: Mutex<IndexMap<String, FlagValue>>,
    pub pending_provider_registrations: Mutex<Vec<PendingProviderRegistration>>,
    pub pending_native_provider_registrations: Mutex<Vec<PendingNativeProviderRegistration>>,
    pub pending_virtual_model_registrations: Mutex<Vec<PendingVirtualModelRegistration>>,
    pub mcp_servers: mcp_wire::McpServerRegistry,
    pub actions: Mutex<ExtensionActions>,
    pub stale_message: Mutex<Option<String>>,
    pub generation: std::sync::atomic::AtomicU64,
    pub event_bus_unsubscribers: Mutex<Vec<Unsubscribe>>,
    pub create_context_fn: Mutex<Arc<dyn Fn() -> pi_js::Result<ExtensionContext> + Send + Sync>>,
    pub register_provider_fn: Mutex<Arc<dyn Fn(&str, ProviderConfig, &str) + Send + Sync>>,
    pub register_native_provider_fn: Mutex<Arc<dyn Fn(Arc<dyn Provider>, &str) + Send + Sync>>,
    pub unregister_provider_fn: Mutex<Arc<dyn Fn(&str, &str) + Send + Sync>>,
    pub register_virtual_model_fn: Mutex<Arc<dyn Fn(mcp_wire::VirtualModelDefinition, &str) + Send + Sync>>,
    pub unregister_virtual_model_fn: Mutex<Arc<dyn Fn(&str, &str) + Send + Sync>>,
}

#[derive(Clone)]
pub struct ExtensionRuntime {
    pub inner: Arc<ExtensionRuntimeInner>,
}

impl ExtensionRuntime {
    pub fn assert_active(&self) -> pi_js::Result<()> {
        let guard = self.inner.stale_message.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(msg) = &*guard {
            return Err(pi_js::Error::msg(msg.clone()));
        }
        Ok(())
    }

    pub fn invalidate(&self, message: Option<&str>) {
        let msg = message
            .unwrap_or("This extension ctx is stale after session replacement or reload. Do not use a captured pi or command ctx after ctx.newSession(), ctx.fork(), ctx.switchSession(), or ctx.reload(). For newSession, fork, and switchSession, move post-replacement work into withSession and use the ctx passed to withSession. For reload, do not use the old ctx after await ctx.reload().")
            .to_string();
        {
            let mut guard = self.inner.stale_message.lock().unwrap_or_else(|e| e.into_inner());
            if guard.is_some() {
                return;
            }
            *guard = Some(msg);
            self.inner.generation.fetch_add(1, std::sync::atomic::Ordering::Release);
        }
        let unsubs = {
            let mut guard = self
                .inner
                .event_bus_unsubscribers
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *guard)
        };
        for unsub in unsubs {
            unsub.call();
        }
    }

    pub fn track_event_bus_subscription(&self, unsub: Unsubscribe) -> Unsubscribe {
        let active = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let active_clone = Arc::clone(&active);
        let unsub_slot = Arc::new(Mutex::new(Some(unsub)));
        let unsub_slot_clone = Arc::clone(&unsub_slot);
        let runtime_unsub = Unsubscribe::new(move || {
            if active_clone.swap(false, std::sync::atomic::Ordering::AcqRel) {
                if let Some(u) = unsub_slot_clone.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    u.call();
                }
            }
        });
        let active_outer = Arc::clone(&active);
        let unsub_slot_outer = Arc::clone(&unsub_slot);
        {
            let mut guard = self
                .inner
                .event_bus_unsubscribers
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            guard.push(runtime_unsub);
        }
        Unsubscribe::new(move || {
            if active_outer.swap(false, std::sync::atomic::Ordering::AcqRel) {
                if let Some(u) = unsub_slot_outer.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    u.call();
                }
            }
        })
    }

    pub fn generation(&self) -> u64 {
        self.inner.generation.load(std::sync::atomic::Ordering::Acquire)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ExtensionLoadState {
    Loading,
    Active,
    Failed,
}

pub struct ExtensionApiInner {
    pub extension: LoadedExtension,
    pub runtime: ExtensionRuntime,
    pub cwd: String,
    pub event_bus: Arc<dyn mcp_wire::EventBus>,
    pub load_state: Mutex<ExtensionLoadState>,
    pub next_handler_id: std::sync::atomic::AtomicU64,
    pub pending_flag_values: Mutex<IndexMap<String, FlagValue>>,
    pub pending_runtime_changes: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
    pub loading_unsubscribers: Mutex<Vec<Unsubscribe>>,
}

/// Handle passed to extension factories (`PORTING.md` §10.2).
#[derive(Clone)]
pub struct ExtensionApi {
    pub inner: Arc<ExtensionApiInner>,
}

pub use ExtensionApi as ExtensionAPI;

impl ExtensionApi {
    pub fn assert_active(&self) -> pi_js::Result<()> {
        let state = *self.inner.load_state.lock().unwrap_or_else(|e| e.into_inner());
        if state == ExtensionLoadState::Failed {
            return Err(pi_js::Error::msg(format!(
                "Extension \"{}\" failed to load and its API is no longer active.",
                self.inner.extension.path
            )));
        }
        self.inner.runtime.assert_active()
    }

    pub(crate) fn apply_runtime_change(&self, change: impl FnOnce() + Send + 'static) {
        let state = *self.inner.load_state.lock().unwrap_or_else(|e| e.into_inner());
        if state == ExtensionLoadState::Loading {
            self.inner
                .pending_runtime_changes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(Box::new(change));
        } else {
            change();
        }
    }

    pub fn commit_loading(&self) -> pi_js::Result<()> {
        {
            let state = *self.inner.load_state.lock().unwrap_or_else(|e| e.into_inner());
            if state != ExtensionLoadState::Loading {
                return Ok(());
            }
        }
        self.inner.runtime.assert_active()?;
        let flags = std::mem::take(&mut *self.inner.pending_flag_values.lock().unwrap_or_else(|e| e.into_inner()));
        {
            let mut runtime_flags = self
                .inner
                .runtime
                .inner
                .flag_values
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for (name, val) in flags {
                if !runtime_flags.contains_key(&name) {
                    runtime_flags.insert(name, val);
                }
            }
        }
        let changes = std::mem::take(
            &mut *self
                .inner
                .pending_runtime_changes
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        for change in changes {
            change();
        }
        *self.inner.load_state.lock().unwrap_or_else(|e| e.into_inner()) = ExtensionLoadState::Active;
        self.inner
            .loading_unsubscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        Ok(())
    }

    pub fn discard_loading(&self) {
        {
            let mut state = self.inner.load_state.lock().unwrap_or_else(|e| e.into_inner());
            if *state != ExtensionLoadState::Loading {
                return;
            }
            *state = ExtensionLoadState::Failed;
        }
        self.inner
            .pending_flag_values
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.inner
            .pending_runtime_changes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        let unsubs = std::mem::take(
            &mut *self
                .inner
                .loading_unsubscribers
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        for u in unsubs {
            u.call();
        }
    }

    /// Subscribe to a typed extension event (`PORTING.md` §10.3).
    pub fn on<E: ExtensionEvent>(
        &self,
        handler: impl Fn(E, ExtensionContext) -> BoxFuture<pi_js::Result<Option<E::Output>>> + Send + Sync + 'static,
    ) -> Unsubscribe {
        if self.assert_active().is_err() {
            return Unsubscribe::noop();
        }
        let id = self
            .inner
            .next_handler_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let boxed_handler = Arc::new(handler);
        let registered = RegisteredEventHandler::Boxed(Arc::new(move |ev_any, ctx| {
            let h = Arc::clone(&boxed_handler);
            match ev_any.downcast::<E>() {
                Ok(ev) => {
                    let ev_val = (*ev).clone();
                    Box::pin(async move {
                        let res = h(ev_val, ctx).await?;
                        Ok(res.map(|v| Box::new(v) as Box<dyn Any + Send>))
                    })
                }
                Err(_) => Box::pin(async {
                    Err(pi_js::Error::msg(format!(
                        "Extension event type mismatch for {}",
                        E::TYPE
                    )))
                }),
            }
        }));
        {
            let mut map = self.inner.extension.handlers.lock().unwrap_or_else(|e| e.into_inner());
            map.entry(E::TYPE.to_string()).or_default().push((id, registered));
        }
        let handlers_arc = Arc::clone(&self.inner.extension.handlers);
        let event_name = E::TYPE.to_string();
        Unsubscribe::new(move || {
            let mut map = handlers_arc.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(list) = map.get_mut(&event_name) {
                list.retain(|(entry_id, _)| *entry_id != id);
                if list.is_empty() {
                    map.shift_remove(&event_name);
                }
            }
        })
    }

    /// Dynamic/JSON event registration used by the QuickJS plugin bridge.
    pub fn on_raw(
        &self,
        event_name: &str,
        handler: Arc<dyn Fn(Value, ExtensionContext) -> BoxFuture<pi_js::Result<Option<Value>>> + Send + Sync>,
    ) -> pi_js::Result<Unsubscribe> {
        self.assert_active()?;
        let id = self
            .inner
            .next_handler_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        {
            let mut map = self.inner.extension.handlers.lock().unwrap_or_else(|e| e.into_inner());
            map.entry(event_name.to_string())
                .or_default()
                .push((id, RegisteredEventHandler::Raw(handler)));
        }
        let handlers_arc = Arc::clone(&self.inner.extension.handlers);
        let event_key = event_name.to_string();
        Ok(Unsubscribe::new(move || {
            let mut map = handlers_arc.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(list) = map.get_mut(&event_key) {
                list.retain(|(entry_id, _)| *entry_id != id);
                if list.is_empty() {
                    map.shift_remove(&event_key);
                }
            }
        }))
    }

    pub fn on_project_trust(&self, handler: ProjectTrustHandler) -> Unsubscribe {
        if self.assert_active().is_err() {
            return Unsubscribe::noop();
        }
        let id = self
            .inner
            .next_handler_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner
            .extension
            .project_trust_handlers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((id, handler));
        let handlers_arc = Arc::clone(&self.inner.extension.project_trust_handlers);
        Unsubscribe::new(move || {
            let mut list = handlers_arc.lock().unwrap_or_else(|e| e.into_inner());
            list.retain(|(entry_id, _)| *entry_id != id);
        })
    }

    pub fn register_tool(&self, tool: ToolDefinition) -> pi_js::Result<()> {
        self.assert_active()?;
        if !tool.parameters.to_value().is_object() {
            return Err(pi_js::Error::msg(format!(
                "Tool \"{}\" registered by extension \"{}\" must define an object parameter schema.",
                tool.name, self.inner.extension.path
            )));
        }
        self.inner
            .extension
            .tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                tool.name.clone(),
                RegisteredTool {
                    definition: tool,
                    source_info: self.inner.extension.source_info.clone(),
                },
            );
        let refresh = self
            .inner
            .runtime
            .inner
            .actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .refresh_tools
            .clone();
        refresh();
        Ok(())
    }

    pub fn register_command(&self, name: &str, options: RegisteredCommandOptions) -> pi_js::Result<()> {
        self.assert_active()?;
        if name.is_empty() {
            return Err(pi_js::Error::msg(format!(
                "Command registered by extension \"{}\" must have a non-empty string name. Use pi.registerCommand(\"name\", {{ description, handler }}).",
                self.inner.extension.path
            )));
        }
        self.inner
            .extension
            .commands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                name.to_string(),
                RegisteredCommand {
                    name: name.to_string(),
                    source_info: self.inner.extension.source_info.clone(),
                    description: options.description,
                    get_argument_completions: options.get_argument_completions,
                    handler: options.handler,
                },
            );
        Ok(())
    }

    pub fn register_shortcut(&self, shortcut: KeyId, options: ShortcutOptions) -> pi_js::Result<()> {
        self.assert_active()?;
        self.inner
            .extension
            .shortcuts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                shortcut.clone(),
                ExtensionShortcut {
                    shortcut,
                    description: options.description,
                    handler: options.handler,
                    extension_path: self.inner.extension.path.clone(),
                },
            );
        Ok(())
    }

    pub fn register_flag(&self, name: &str, options: FlagOptions) -> pi_js::Result<()> {
        self.assert_active()?;
        let flag_type = options.r#type.unwrap_or(FlagType::Boolean);
        if let Some(ref def) = options.default {
            let matches = matches!(
                (flag_type, def),
                (FlagType::Boolean, FlagValue::Bool(_)) | (FlagType::String, FlagValue::String(_))
            );
            if !matches {
                let got = match def {
                    FlagValue::Bool(_) => "boolean",
                    FlagValue::String(_) => "string",
                };
                let expected = match flag_type {
                    FlagType::Boolean => "boolean",
                    FlagType::String => "string",
                };
                return Err(pi_js::Error::msg(format!(
                    "Invalid default for flag \"{name}\": expected {expected}, got {got}"
                )));
            }
        }
        self.inner
            .extension
            .flags
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                name.to_string(),
                ExtensionFlag {
                    name: name.to_string(),
                    description: options.description,
                    r#type: flag_type,
                    default: options.default.clone(),
                    extension_path: self.inner.extension.path.clone(),
                },
            );
        if let Some(def) = options.default {
            let already_in_runtime = self
                .inner
                .runtime
                .inner
                .flag_values
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(name);
            if !already_in_runtime {
                let state = *self.inner.load_state.lock().unwrap_or_else(|e| e.into_inner());
                if state == ExtensionLoadState::Loading {
                    let mut pending = self.inner.pending_flag_values.lock().unwrap_or_else(|e| e.into_inner());
                    if !pending.contains_key(name) {
                        pending.insert(name.to_string(), def);
                    }
                } else {
                    self.inner
                        .runtime
                        .inner
                        .flag_values
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(name.to_string(), def);
                }
            }
        }
        Ok(())
    }

    pub fn register_message_renderer(&self, custom_type: &str, renderer: MessageRenderer) -> pi_js::Result<()> {
        self.assert_active()?;
        self.inner
            .extension
            .message_renderers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(custom_type.to_string(), renderer);
        Ok(())
    }

    pub fn register_markdown_transformer(&self, transformer: MarkdownTransformer) -> pi_js::Result<()> {
        self.assert_active()?;
        *self
            .inner
            .extension
            .markdown_transformer
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(transformer);
        Ok(())
    }

    pub fn register_entry_renderer(&self, custom_type: &str, renderer: EntryRenderer) -> pi_js::Result<()> {
        self.assert_active()?;
        self.inner
            .extension
            .entry_renderers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(custom_type.to_string(), renderer);
        Ok(())
    }

    pub fn get_flag(&self, name: &str) -> pi_js::Result<Option<FlagValue>> {
        self.assert_active()?;
        let registered = self
            .inner
            .extension
            .flags
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(name);
        if !registered {
            return Ok(None);
        }
        if let Some(val) = self
            .inner
            .runtime
            .inner
            .flag_values
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
        {
            return Ok(Some(val));
        }
        Ok(self
            .inner
            .pending_flag_values
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned())
    }

    pub fn send_message(
        &self,
        message: mcp_wire::CustomMessageDraft,
        options: Option<SendMessageOptions>,
    ) -> pi_js::Result<()> {
        self.assert_active()?;
        let handler = self
            .inner
            .runtime
            .inner
            .actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send_message
            .clone();
        handler(message, options);
        Ok(())
    }

    pub fn send_user_message(
        &self,
        content: mcp_wire::CustomMessageContent,
        options: Option<SendUserMessageOptions>,
    ) -> pi_js::Result<()> {
        self.assert_active()?;
        let handler = self
            .inner
            .runtime
            .inner
            .actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send_user_message
            .clone();
        handler(content, options);
        Ok(())
    }

    pub fn append_entry(&self, custom_type: &str, data: Option<Value>) -> pi_js::Result<()> {
        self.assert_active()?;
        let handler = self
            .inner
            .runtime
            .inner
            .actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .append_entry
            .clone();
        handler(custom_type, data);
        Ok(())
    }

    pub fn set_session_name(&self, name: &str) -> pi_js::Result<()> {
        self.assert_active()?;
        let handler = self
            .inner
            .runtime
            .inner
            .actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_session_name
            .clone();
        handler(name);
        Ok(())
    }

    pub fn get_session_name(&self) -> pi_js::Result<Option<String>> {
        self.assert_active()?;
        let handler = self
            .inner
            .runtime
            .inner
            .actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_session_name
            .clone();
        Ok(handler())
    }

    pub fn set_label(&self, entry_id: &str, label: Option<&str>) -> pi_js::Result<()> {
        self.assert_active()?;
        let handler = self
            .inner
            .runtime
            .inner
            .actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_label
            .clone();
        handler(entry_id, label);
        Ok(())
    }

    pub async fn exec(
        &self,
        command: &str,
        args: &[String],
        options: Option<ExecOptions>,
    ) -> pi_js::Result<ExecResult> {
        self.assert_active()?;
        let target_cwd = options
            .as_ref()
            .and_then(|o| o.cwd.as_deref())
            .unwrap_or(&self.inner.cwd)
            .to_string();
        mcp_wire::exec_command_wire(command, args, &target_cwd, options).await
    }

    pub fn get_active_tools(&self) -> pi_js::Result<Vec<String>> {
        self.assert_active()?;
        let handler = self
            .inner
            .runtime
            .inner
            .actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_active_tools
            .clone();
        Ok(handler())
    }

    pub fn get_all_tools(&self) -> pi_js::Result<Vec<ToolInfo>> {
        self.assert_active()?;
        let handler = self
            .inner
            .runtime
            .inner
            .actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_all_tools
            .clone();
        Ok(handler())
    }

    pub fn get_settings(&self) -> pi_js::Result<mcp_wire::Settings> {
        self.assert_active()?;
        let handler = self
            .inner
            .runtime
            .inner
            .actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_settings
            .clone();
        Ok(handler())
    }

    pub fn set_active_tools(&self, tool_names: Vec<String>) -> pi_js::Result<()> {
        self.assert_active()?;
        let handler = self
            .inner
            .runtime
            .inner
            .actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_active_tools
            .clone();
        handler(tool_names);
        Ok(())
    }

    pub fn get_commands(&self) -> pi_js::Result<Vec<SlashCommandInfo>> {
        self.assert_active()?;
        let handler = self
            .inner
            .runtime
            .inner
            .actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_commands
            .clone();
        Ok(handler())
    }

    pub async fn set_model(&self, model: Model) -> pi_js::Result<bool> {
        self.assert_active()?;
        let handler = self
            .inner
            .runtime
            .inner
            .actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_model
            .clone();
        handler(model).await
    }

    pub fn get_thinking_level(&self) -> pi_js::Result<ThinkingLevel> {
        self.assert_active()?;
        let handler = self
            .inner
            .runtime
            .inner
            .actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_thinking_level
            .clone();
        Ok(handler())
    }

    pub fn set_thinking_level(&self, level: ThinkingLevel) -> pi_js::Result<()> {
        self.assert_active()?;
        let handler = self
            .inner
            .runtime
            .inner
            .actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_thinking_level
            .clone();
        handler(level);
        Ok(())
    }

    pub fn register_provider(&self, name: &str, config: ProviderConfig) -> pi_js::Result<()> {
        self.assert_active()?;
        let runtime = self.inner.runtime.clone();
        let ext_path = self.inner.extension.path.clone();
        let provider_name = name.to_string();
        self.apply_runtime_change(move || {
            let cb = runtime
                .inner
                .register_provider_fn
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            cb(&provider_name, config, &ext_path);
        });
        Ok(())
    }

    pub fn register_native_provider(&self, provider: Arc<dyn Provider>) -> pi_js::Result<()> {
        self.assert_active()?;
        let runtime = self.inner.runtime.clone();
        let ext_path = self.inner.extension.path.clone();
        self.apply_runtime_change(move || {
            let cb = runtime
                .inner
                .register_native_provider_fn
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            cb(provider, &ext_path);
        });
        Ok(())
    }

    pub fn unregister_provider(&self, name: &str) -> pi_js::Result<()> {
        self.assert_active()?;
        let runtime = self.inner.runtime.clone();
        let ext_path = self.inner.extension.path.clone();
        let provider_name = name.to_string();
        self.apply_runtime_change(move || {
            let cb = runtime
                .inner
                .unregister_provider_fn
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            cb(&provider_name, &ext_path);
        });
        Ok(())
    }

    pub fn register_mcp_server(&self, name: &str, config_raw: Value) -> pi_js::Result<()> {
        self.assert_active()?;
        let validated = mcp_wire::validate_mcp_server_config(name, &config_raw).map_err(|msg| {
            pi_js::Error::msg(format!(
                "Invalid MCP server registered by extension \"{}\": {msg}",
                self.inner.extension.path
            ))
        })?;
        if let Some(existing) = self.inner.runtime.inner.mcp_servers.get(name) {
            if existing.extension_path != self.inner.extension.path {
                return Err(pi_js::Error::msg(format!(
                    "MCP server \"{name}\" is already registered by extension \"{}\"",
                    existing.extension_path
                )));
            }
        }
        let target_ns = mcp_wire::mcp_namespace(name);
        for existing in self.inner.runtime.inner.mcp_servers.list() {
            if existing.name != name && mcp_wire::mcp_namespace(&existing.name) == target_ns {
                return Err(pi_js::Error::msg(format!(
                    "MCP server \"{name}\" conflicts with registered server \"{}\"",
                    existing.name
                )));
            }
        }
        let registry = self.inner.runtime.inner.mcp_servers.clone();
        let server = mcp_wire::RegisteredMcpServer {
            name: name.to_string(),
            config: validated,
            extension_path: self.inner.extension.path.clone(),
        };
        self.apply_runtime_change(move || {
            registry.register(server);
        });
        Ok(())
    }

    pub fn unregister_mcp_server(&self, name: &str) -> pi_js::Result<()> {
        self.assert_active()?;
        let registry = self.inner.runtime.inner.mcp_servers.clone();
        let name = name.to_string();
        let ext_path = self.inner.extension.path.clone();
        self.apply_runtime_change(move || {
            registry.unregister(&name, &ext_path);
        });
        Ok(())
    }

    pub fn get_mcp_servers(&self) -> pi_js::Result<Vec<mcp_wire::RegisteredMcpServer>> {
        self.assert_active()?;
        Ok(self.inner.runtime.inner.mcp_servers.list())
    }

    pub fn register_virtual_model(&self, model: ExtensionVirtualModel) -> pi_js::Result<()> {
        self.assert_active()?;
        let runtime = self.inner.runtime.clone();
        let ext_path = self.inner.extension.path.clone();
        let route_cb = model.route;
        let runtime_for_route = runtime.clone();
        let def = mcp_wire::VirtualModelDefinition {
            provider: model.provider,
            id: model.id,
            name: model.name,
            thinking_levels: model.thinking_levels,
            route: Arc::new(move |req| {
                let route_cb = Arc::clone(&route_cb);
                let create_ctx = runtime_for_route
                    .inner
                    .create_context_fn
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                Box::pin(async move {
                    let ctx = create_ctx()?;
                    route_cb(req, ctx).await
                })
            }),
        };
        self.apply_runtime_change(move || {
            let cb = runtime
                .inner
                .register_virtual_model_fn
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            cb(def, &ext_path);
        });
        Ok(())
    }

    pub fn unregister_virtual_model(&self, provider: &str, id: &str) -> pi_js::Result<()> {
        self.assert_active()?;
        let runtime = self.inner.runtime.clone();
        let provider = provider.to_string();
        let id = id.to_string();
        self.apply_runtime_change(move || {
            let cb = runtime
                .inner
                .unregister_virtual_model_fn
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            cb(&provider, &id);
        });
        Ok(())
    }

    pub fn events(&self) -> Arc<dyn mcp_wire::EventBus> {
        let state = *self.inner.load_state.lock().unwrap_or_else(|e| e.into_inner());
        Arc::new(TrackedEventBus {
            api: self.clone(),
            loading_only: state == ExtensionLoadState::Loading,
        })
    }
}

struct TrackedEventBus {
    api: ExtensionApi,
    loading_only: bool,
}

impl mcp_wire::EventBus for TrackedEventBus {
    fn emit(&self, channel: &str, data: Value) {
        if self.api.assert_active().is_ok() {
            self.api.inner.event_bus.emit(channel, data);
        }
    }

    fn on(&self, channel: &str, handler: mcp_wire::EventBusHandler) -> Unsubscribe {
        if self.api.assert_active().is_err() {
            return Unsubscribe::noop();
        }
        let raw_unsub = self.api.inner.event_bus.on(channel, handler);
        let tracked = self.api.inner.runtime.track_event_bus_subscription(raw_unsub);
        if self.loading_only {
            let active = Arc::new(std::sync::atomic::AtomicBool::new(true));
            let active_a = Arc::clone(&active);
            let slot = Arc::new(Mutex::new(Some(tracked)));
            let slot_a = Arc::clone(&slot);
            self.api
                .inner
                .loading_unsubscribers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(Unsubscribe::new(move || {
                    if active_a.swap(false, std::sync::atomic::Ordering::AcqRel) {
                        if let Some(u) = slot_a.lock().unwrap_or_else(|e| e.into_inner()).take() {
                            u.call();
                        }
                    }
                }));
            let active_b = Arc::clone(&active);
            let slot_b = Arc::clone(&slot);
            Unsubscribe::new(move || {
                if active_b.swap(false, std::sync::atomic::Ordering::AcqRel) {
                    if let Some(u) = slot_b.lock().unwrap_or_else(|e| e.into_inner()).take() {
                        u.call();
                    }
                }
            })
        } else {
            tracked
        }
    }
}

// ============================================================================
// Load result & ExtensionError
// ============================================================================

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadExtensionErrorEntry {
    pub path: String,
    pub error: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadExtensionWarningEntry {
    pub path: String,
    pub warning: String,
}

#[derive(Clone)]
pub struct LoadExtensionsResult {
    pub extensions: Vec<LoadedExtension>,
    pub errors: Vec<LoadExtensionErrorEntry>,
    pub warnings: Vec<LoadExtensionWarningEntry>,
    pub runtime: ExtensionRuntime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionError {
    pub extension_path: String,
    pub event: String,
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
}
