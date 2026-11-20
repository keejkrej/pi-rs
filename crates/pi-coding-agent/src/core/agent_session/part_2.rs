//! Port of packages/coding-agent/src/core/agent-session.ts#L1386-L2666

use base64::Engine as _;
use futures::future::BoxFuture;
use indexmap::{IndexMap, IndexSet};
use pi_agent_core::agent::{Agent, PromptInput};
use pi_agent_core::types::{AgentMessage, AgentState, AgentTool, QueueMode};
use pi_ai::types::{
    ImageContent, Model, ModelThinkingLevel, SystemMessage, TextContent, UserContentBlock, UserMessage,
    UserMessageContent,
};
use pi_ai::{
    AnyModel, clamp_thinking_level, get_current_system_message, get_current_tools, get_supported_thinking_levels,
    models_are_equal,
};
use serde_json::Value;

use super::BoundaryKind;
use crate::core::auth_guidance::{format_no_api_key_found_message, format_no_model_selected_message};
use crate::core::cache_warmer::CacheWarmingStatus;
use crate::core::defaults::DEFAULT_THINKING_LEVEL;
use crate::core::extensions::runner::{BeforeAgentStartTrigger, BoundaryBaseEvent};
use crate::core::extensions::types::{
    CustomMessageContent, CustomMessageDraft, ExtensionError, InputDeliveryBehavior, InputSource, ModelSelectEvent,
    ModelSelectSource, ThinkingLevelSelectEvent, ToolDefinition, ToolExposure, ToolInfo, ToolLoadoutBackend,
    ToolNamespace,
};
use crate::core::mcp_servers::is_mcp_tool_name;
use crate::core::messages::CustomMessage;
use crate::core::prompt_templates::{PromptTemplate, expand_prompt_template};
use crate::core::resource_loader::ResourceLoader;
use crate::core::sdk::ScopedModelEntry;
use crate::core::session_manager::SessionManager;
use crate::core::settings_manager::{CacheWarmingMode, QueueModeSetting, SettingsManager, apply_tool_modifiers};
use crate::core::system_prompt::{
    BuildSystemPromptOptions, ContextFile, NormalizedBuildSystemPromptOptions, build_system_prompt_from_normalized,
    build_system_prompt_sections, diff_system_prompt_sections, normalize_build_system_prompt_options,
};
use crate::core::virtual_models::{find_latest_response, is_virtual_model};
use crate::utils::frontmatter::strip_frontmatter;
use crate::utils::image_process::{ProcessImageOptions, ProcessImageResult, process_image};

use super::*;

/// Content input accepted by `AgentSession::send_user_message`.
#[derive(Clone, Debug)]
pub enum UserMessageInput {
    Text(String),
    Blocks(Vec<UserContentBlock>),
}

impl From<&str> for UserMessageInput {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<String> for UserMessageInput {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<Vec<UserContentBlock>> for UserMessageInput {
    fn from(value: Vec<UserContentBlock>) -> Self {
        Self::Blocks(value)
    }
}

/// Draft passed to `AgentSession::send_custom_message`.
#[derive(Clone, Debug)]
pub struct SendCustomMessageDraft {
    pub custom_type: String,
    pub content: CustomMessageContent,
    pub display: bool,
    pub details: Option<Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CustomMessageDelivery {
    Steer,
    FollowUp,
    NextTurn,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SendCustomMessageOptions {
    pub trigger_turn: Option<bool>,
    pub deliver_as: Option<CustomMessageDelivery>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SendUserMessageOptions {
    pub deliver_as: Option<StreamingBehavior>,
    pub expand_prompt_templates: Option<bool>,
}

struct SessionToolLoadout {
    declared: Vec<AgentTool>,
    callable: Vec<AgentTool>,
    registered: Vec<AgentTool>,
    exposures: IndexMap<String, ToolExposure>,
    namespaces: IndexMap<String, ToolNamespace>,
    prompt_guidelines: IndexMap<String, Vec<String>>,
}

impl ToolLoadoutBackend for SessionToolLoadout {
    fn declared(&self) -> &[AgentTool] {
        &self.declared
    }

    fn callable(&self) -> &[AgentTool] {
        &self.callable
    }

    fn registered(&self) -> &[AgentTool] {
        &self.registered
    }

    fn get_exposure(&self, name: &str) -> ToolExposure {
        self.exposures.get(name).copied().unwrap_or(ToolExposure::Direct)
    }

    fn get_namespace(&self, name: &str) -> Option<ToolNamespace> {
        self.namespaces.get(name).cloned()
    }

    fn get_prompt_guidelines(&self, name: &str) -> Vec<String> {
        self.prompt_guidelines.get(name).cloned().unwrap_or_default()
    }
}

/// Glob wildcard match where `*` matches zero or more characters.
pub(crate) fn tool_pattern_matches(pattern: &str, name: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == name;
    }
    let p = pattern.as_bytes();
    let s = name.as_bytes();
    let (mut pi, mut si) = (0usize, 0usize);
    let mut star_pi: Option<usize> = None;
    let mut match_si = 0usize;
    while si < s.len() {
        if pi < p.len() && p[pi] == s[si] {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star_pi = Some(pi);
            match_si = si;
            pi += 1;
        } else if let Some(sp) = star_pi {
            pi = sp + 1;
            match_si += 1;
            si = match_si;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

pub(crate) fn tool_name_matches_entries<'a, I>(entries: I, name: &str) -> bool
where
    I: IntoIterator<Item = &'a String>,
{
    entries.into_iter().any(|entry| tool_pattern_matches(entry, name))
}

impl AgentSession {
    // =========================================================================
    // Read-only State Access
    // =========================================================================

    /// Refresh the public finalized transcript from the canonical session projection.
    pub fn refresh_context(&self) {
        self._refresh_finalized_context();
    }

    /// Full agent state.
    pub fn state(&self) -> AgentState {
        self.inner.agent.state()
    }

    /// Current cache-warming state and the policy inputs that produced it.
    pub fn cache_warming_status(&self) -> Option<CacheWarmingStatus> {
        self.inner.cache_warmer.as_ref().map(|warmer| warmer.status())
    }

    /// Persist the cache-warming mode and immediately reconcile active warming.
    pub fn set_cache_warming_mode(&self, mode: CacheWarmingMode) {
        self.inner.settings_manager.set_cache_warming_mode(mode);
        if let Some(ref warmer) = self.inner.cache_warmer {
            warmer.on_mode_changed();
        }
    }

    /// Current model (may be `None` if not yet selected).
    pub fn model(&self) -> Option<Model> {
        let has_model = self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).has_model;
        if !has_model {
            return None;
        }
        Some(self.inner.agent.state().model)
    }

    /// Current thinking level.
    pub fn thinking_level(&self) -> ModelThinkingLevel {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .thinking_level
    }

    /// Under a virtual selection, the physical model and thinking level of the latest successful response.
    pub fn routed_model(&self) -> Option<RoutedModel> {
        let model = self.model()?;
        if !is_virtual_model(&model) {
            return None;
        }
        let messages = self.messages();
        let latest = find_latest_response(&messages)?;
        let physical = self
            .inner
            .model_runtime
            .get_physical_model(&latest.provider, &latest.model)?;
        Some(RoutedModel {
            model: physical,
            thinking_level: latest.thinking_level,
        })
    }

    /// Whether the session is currently processing an agent run or post-run continuation.
    pub fn is_streaming(&self) -> bool {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_agent_run_active
    }

    /// Whether the session has no active agent run, compaction, branch summary, retry, or queued continuation.
    pub fn is_idle(&self) -> bool {
        let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        !guard.is_agent_run_active
            && guard.compaction_abort_controller.is_none()
            && guard.auto_compaction_abort_controller.is_none()
            && guard.branch_summary_abort_controller.is_none()
    }

    /// Current effective system prompt, including changes not yet sent to the model.
    pub fn system_prompt(&self) -> String {
        let opts = {
            let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard
                .run_system_prompt_options
                .clone()
                .unwrap_or_else(|| guard.base_system_prompt_options.clone())
        };
        build_system_prompt_from_normalized(&opts).unwrap_or_default()
    }

    /// Current retry attempt (0 if not retrying).
    pub fn retry_attempt(&self) -> u32 {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).retry_attempt
    }

    /// Access the underlying session manager.
    pub fn session_manager(&self) -> &SessionManager {
        &self.inner.session_manager
    }

    /// Access the underlying settings manager.
    pub fn settings_manager(&self) -> &SettingsManager {
        &self.inner.settings_manager
    }

    /// Access the underlying agent handle.
    pub fn agent(&self) -> &Agent {
        &self.inner.agent
    }

    /// Get the names of currently active tools, which are the tools declared to the model.
    /// Tools with `codemode` or `deferred` exposure are callable from other tools without being active.
    pub fn get_active_tool_names(&self) -> Vec<String> {
        self.inner.agent.state().tools.into_iter().map(|t| t.name).collect()
    }

    /// Get the names of the tools that tools can call through `ctx.executeTool()`.
    pub fn get_callable_tool_names(&self) -> Vec<String> {
        self._get_callable_tools(None).into_iter().map(|t| t.name).collect()
    }

    /// Get all configured tools with name, description, parameter schema, prompt guidelines, and source metadata.
    pub fn get_all_tools(&self) -> Vec<ToolInfo> {
        let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .tool_definitions
            .values()
            .map(|entry| ToolInfo {
                name: entry.definition.name.clone(),
                description: entry.definition.description.clone(),
                parameters: entry.definition.parameters.clone(),
                prompt_guidelines: entry.definition.prompt_guidelines.clone(),
                reaches_tools: entry.definition.reaches_tools(),
                exposure: entry.definition.exposure.unwrap_or(ToolExposure::Direct),
                namespace: entry.definition.namespace.clone(),
                annotations: entry.definition.annotations.clone(),
                source_info: entry.source_info.clone(),
            })
            .collect()
    }

    pub fn get_tool_definition(&self, name: &str) -> Option<ToolDefinition> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tool_definitions
            .get(name)
            .map(|entry| entry.definition.clone())
    }

    /// Set active tools by name.
    /// Only tools in the registry can be enabled. Unknown and hidden tool names are ignored.
    /// Also rebuilds the system prompt to reflect the new tool set.
    /// Changes take effect on the next agent turn.
    pub fn set_active_tools_by_name(&self, tool_names: &[String]) {
        let previous = self.get_active_tool_names();
        self._set_active_tools(tool_names);
        let active: IndexSet<String> = self.get_active_tool_names().into_iter().collect();
        if previous.iter().any(|name| !active.contains(name)) {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pending_tool_names
                .clear();
        }
    }

    pub(crate) fn _set_active_tools(&self, tool_names: &[String]) {
        let tools = self._apply_tool_loadout(tool_names);
        let active_names: Vec<String> = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            for tool in &tools {
                guard.pending_tool_names.shift_remove(&tool.name);
            }
            tools.into_iter().map(|tool| tool.name).collect()
        };
        self._rebuild_system_prompt(&active_names);
    }

    pub(crate) fn _is_allowed_tool(&self, name: &str) -> bool {
        let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if guard
            .excluded_tool_names
            .as_ref()
            .is_some_and(|excluded| tool_name_matches_entries(excluded, name))
        {
            return false;
        }
        let Some(allowed) = guard.allowed_tool_names.as_ref() else {
            return true;
        };
        if tool_name_matches_entries(allowed, name) {
            return true;
        }
        !guard.allowlist_filters_mcp && is_mcp_tool_name(name)
    }

    pub(crate) fn _is_activatable_locked(guard: &AgentSessionState, name: &str) -> bool {
        let Some(allowed) = guard.allowed_tool_names.as_ref() else {
            return true;
        };
        if tool_name_matches_entries(allowed, name) || !is_mcp_tool_name(name) {
            return true;
        }
        let exposure = guard
            .tool_definitions
            .get(name)
            .and_then(|e| e.definition.exposure)
            .unwrap_or(ToolExposure::Direct);
        exposure != ToolExposure::Direct && guard.tool_registry.contains_key("tool_search")
    }

    pub(crate) fn _get_tool_exposure(&self, name: &str) -> ToolExposure {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tool_definitions
            .get(name)
            .and_then(|e| e.definition.exposure)
            .unwrap_or(ToolExposure::Direct)
    }

    /// Tools callable through `ctx.executeTool()`: the active `direct` tools and every registered
    /// `codemode` or `deferred` tool.
    pub(crate) fn _get_callable_tools(&self, active_override: Option<&IndexSet<String>>) -> Vec<AgentTool> {
        let active_owned;
        let active = match active_override {
            Some(set) => set,
            None => {
                active_owned = self.get_active_tool_names().into_iter().collect::<IndexSet<String>>();
                &active_owned
            }
        };
        let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .tool_registry
            .values()
            .filter(|tool| {
                let exposure = guard
                    .tool_definitions
                    .get(&tool.name)
                    .and_then(|e| e.definition.exposure)
                    .unwrap_or(ToolExposure::Direct);
                matches!(exposure, ToolExposure::Codemode | ToolExposure::Deferred)
                    || (exposure == ToolExposure::Direct && active.contains(&tool.name))
            })
            .cloned()
            .collect()
    }

    /// Set the agent's tools for the given active tool names and return them.
    pub(crate) fn _apply_tool_loadout(&self, tool_names: &[String]) -> Vec<AgentTool> {
        let requested: IndexSet<String> = tool_names.iter().cloned().collect();
        let kept: Vec<String> = self
            .get_active_tool_names()
            .into_iter()
            .filter(|name| requested.contains(name))
            .collect();
        let mut unique_names = IndexSet::new();
        for name in kept.into_iter().chain(requested) {
            unique_names.insert(name);
        }

        let (tools, hooks, registered, exposures, namespaces, prompt_guidelines, runner) = {
            let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let tools: Vec<AgentTool> = unique_names
                .iter()
                .filter_map(|name| {
                    let tool = guard.tool_registry.get(name)?;
                    let exposure = guard
                        .tool_definitions
                        .get(name)
                        .and_then(|e| e.definition.exposure)
                        .unwrap_or(ToolExposure::Direct);
                    if exposure == ToolExposure::Hidden || !Self::_is_activatable_locked(&guard, name) {
                        None
                    } else {
                        Some(tool.clone())
                    }
                })
                .collect();

            let hooks: Vec<ToolDefinitionEntry> = tools
                .iter()
                .filter_map(|tool| {
                    let entry = guard.tool_definitions.get(&tool.name)?;
                    entry.definition.prepare_loadout.as_ref().map(|_| entry.clone())
                })
                .collect();

            let registered: Vec<AgentTool> = guard.tool_registry.values().cloned().collect();
            let exposures: IndexMap<String, ToolExposure> = guard
                .tool_definitions
                .iter()
                .map(|(k, v)| (k.clone(), v.definition.exposure.unwrap_or(ToolExposure::Direct)))
                .collect();
            let namespaces: IndexMap<String, ToolNamespace> = guard
                .tool_definitions
                .iter()
                .filter_map(|(k, v)| v.definition.namespace.clone().map(|ns| (k.clone(), ns)))
                .collect();
            let prompt_guidelines: IndexMap<String, Vec<String>> = guard
                .tool_definitions
                .iter()
                .filter_map(|(k, v)| {
                    v.definition
                        .prompt_guidelines
                        .clone()
                        .map(|guidelines| (k.clone(), guidelines))
                })
                .collect();
            let runner = guard.extension_runner.clone();
            (
                tools,
                hooks,
                registered,
                exposures,
                namespaces,
                prompt_guidelines,
                runner,
            )
        };

        let mut hidden = IndexSet::new();
        let mut declared = tools.clone();
        if !hooks.is_empty() {
            let active_set: IndexSet<String> = tools.iter().map(|t| t.name.clone()).collect();
            let callable = self._get_callable_tools(Some(&active_set));
            let loadout = SessionToolLoadout {
                declared: tools.clone(),
                callable,
                registered,
                exposures,
                namespaces,
                prompt_guidelines,
            };
            let mut descriptions: IndexMap<String, String> = IndexMap::new();
            for entry in hooks {
                if let Some(prepare_loadout) = entry.definition.prepare_loadout {
                    match prepare_loadout(&loadout) {
                        Ok(Some(changes)) => {
                            if let Some(descs) = changes.descriptions {
                                for (k, v) in descs {
                                    descriptions.insert(k, v);
                                }
                            }
                            if let Some(hidden_names) = changes.hidden_declarations {
                                for name in hidden_names {
                                    hidden.insert(name);
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(err) => {
                            if let Some(ref runner) = runner {
                                runner.emit_error(ExtensionError {
                                    extension_path: entry.source_info.path.clone(),
                                    event: "prepare_loadout".to_string(),
                                    error: err.to_string(),
                                    stack: None,
                                });
                            }
                        }
                    }
                }
            }
            if !descriptions.is_empty() {
                for tool in &mut declared {
                    if let Some(desc) = descriptions.get(&tool.name) {
                        tool.description = desc.clone();
                    }
                }
            }
        }

        {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.hidden_declarations = hidden;
        }
        self.inner.agent.set_tools(declared.clone());
        declared
    }

    /// Whether compaction or branch summarization is currently running.
    pub fn is_compacting(&self) -> bool {
        let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.auto_compaction_abort_controller.is_some()
            || guard.compaction_abort_controller.is_some()
            || guard.branch_summary_abort_controller.is_some()
    }

    /// All messages including custom types like `BashExecutionMessage`.
    pub fn messages(&self) -> Vec<AgentMessage> {
        self.inner.agent.state().messages
    }

    /// Current steering mode.
    pub fn steering_mode(&self) -> QueueMode {
        self.inner.agent.steering_mode()
    }

    /// Current follow-up mode.
    pub fn follow_up_mode(&self) -> QueueMode {
        self.inner.agent.follow_up_mode()
    }

    /// Current session file path, or `None` if sessions are disabled.
    pub fn session_file(&self) -> Option<String> {
        self.inner.session_manager.get_session_file()
    }

    /// Current session ID.
    pub fn session_id(&self) -> String {
        self.inner.session_manager.get_session_id()
    }

    /// Current session display name, if set.
    pub fn session_name(&self) -> Option<String> {
        self.inner.session_manager.get_session_name()
    }

    /// Scoped models for cycling (from `--models` flag).
    pub fn scoped_models(&self) -> Vec<ScopedModelEntry> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .scoped_models
            .clone()
    }

    /// Update scoped models for cycling.
    pub fn set_scoped_models(&self, scoped_models: Vec<ScopedModelEntry>) {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).scoped_models = scoped_models;
    }

    /// File-based prompt templates.
    pub fn prompt_templates(&self) -> Vec<PromptTemplate> {
        self.inner.resource_loader.get_prompts().0
    }

    pub(crate) fn _normalize_prompt_snippet(&self, text: Option<&str>) -> Option<String> {
        let text = text?;
        let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let trimmed = pi_js::str16::trim(&collapsed).to_string();
        if trimmed.is_empty() { None } else { Some(trimmed) }
    }

    pub(crate) fn _normalize_prompt_guidelines(&self, guidelines: Option<&[String]>) -> Vec<String> {
        let Some(guidelines) = guidelines else {
            return Vec::new();
        };
        if guidelines.is_empty() {
            return Vec::new();
        }
        let mut unique = IndexSet::new();
        for guideline in guidelines {
            let normalized = pi_js::str16::trim(guideline).to_string();
            if !normalized.is_empty() {
                unique.insert(normalized);
            }
        }
        unique.into_iter().collect()
    }

    pub(crate) fn _rebuild_system_prompt(&self, tool_names: &[String]) {
        let loader_system_prompt = self.inner.resource_loader.get_system_prompt();
        let loader_append = self.inner.resource_loader.get_append_system_prompt();
        let append_system_prompt = if loader_append.is_empty() {
            String::new()
        } else {
            loader_append.join("\n\n")
        };
        let loaded_skills = self.inner.resource_loader.get_skills().0;
        let loaded_context_files: Vec<ContextFile> = self
            .inner
            .resource_loader
            .get_agents_files()
            .into_iter()
            .map(|cf| ContextFile {
                path: cf.path,
                content: cf.content,
            })
            .collect();

        let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let valid_tool_names: Vec<String> = tool_names
            .iter()
            .filter(|name| guard.tool_registry.contains_key(*name))
            .cloned()
            .collect();
        let mut tool_snippets = IndexMap::new();
        for name in guard.tool_registry.keys() {
            if let Some(snippet) = guard.tool_prompt_snippets.get(name) {
                tool_snippets.insert(name.clone(), snippet.clone());
            }
        }
        let hidden_tools: Vec<String> = guard.hidden_declarations.iter().cloned().collect();
        guard.base_system_prompt_options = normalize_build_system_prompt_options(&BuildSystemPromptOptions {
            cwd: guard.cwd.clone(),
            skills: Some(loaded_skills),
            context_files: Some(loaded_context_files),
            custom_prompt: loader_system_prompt,
            append_system_prompt: Some(append_system_prompt),
            selected_tools: Some(valid_tool_names),
            hidden_tools: Some(hidden_tools),
            tool_snippets: Some(tool_snippets),
            tool_guidelines: Some(guard.tool_prompt_guidelines.clone()),
            ..Default::default()
        });
    }

    /// Apply a prompt and tool loadout for the next request.
    pub(crate) fn _prepare_prompt_and_tool_loadout(
        &self,
        options: &mut NormalizedBuildSystemPromptOptions,
        messages_override: Option<&[AgentMessage]>,
    ) -> Option<SystemMessage> {
        let applied = self._apply_tool_loadout(&options.selected_tools);
        options.selected_tools = applied.into_iter().map(|t| t.name).collect();

        let hidden: Vec<String> = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .hidden_declarations
            .iter()
            .cloned()
            .collect();
        options.hidden_tools = hidden;

        let owned_messages;
        let messages = match messages_override {
            Some(msgs) => msgs,
            None => {
                owned_messages = self.inner.agent.state().messages;
                &owned_messages
            }
        };

        let llm_messages: Vec<pi_ai::types::Message> =
            messages.iter().filter_map(|m| m.as_message().cloned()).collect();
        let existing_sections = get_current_system_message(&llm_messages)
            .and_then(|s| s.sections)
            .unwrap_or_default();
        let next_sections = build_system_prompt_sections(&normalized_to_build_options(options)).unwrap_or_default();
        let sections = diff_system_prompt_sections(&existing_sections, &next_sections)?;
        Some(SystemMessage {
            content: pi_ai::types::SystemMessageContent::Text(String::new()),
            sections: Some(sections),
            tools_added: None,
            tools_removed: None,
            timestamp: pi_js::time::now_ms(),
        })
    }

    pub(crate) fn _install_hidden_declarations_projection(&self) {
        let previous = self.inner.agent.transform_context();
        let weak = Arc::downgrade(&self.inner);
        self.inner
            .agent
            .set_transform_context(Some(Arc::new(move |messages, signal| {
                let prev = previous.clone();
                let weak = weak.clone();
                Box::pin(async move {
                    let transformed = match prev {
                        Some(f) => f(messages, signal).await?,
                        None => messages,
                    };
                    let Some(inner) = weak.upgrade() else {
                        return Ok(transformed);
                    };
                    let hidden = inner
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .hidden_declarations
                        .clone();
                    if hidden.is_empty() {
                        return Ok(transformed);
                    }
                    Ok(transformed
                        .into_iter()
                        .map(|message| {
                            if let Some(mut sys) = message.as_system().cloned()
                                && (sys.tools_added.is_some() || sys.tools_removed.is_some())
                            {
                                let added: Vec<_> = sys
                                    .tools_added
                                    .take()
                                    .unwrap_or_default()
                                    .into_iter()
                                    .filter(|t| !hidden.contains(&t.name))
                                    .collect();
                                let removed: Vec<_> = sys
                                    .tools_removed
                                    .take()
                                    .unwrap_or_default()
                                    .into_iter()
                                    .filter(|t| !hidden.contains(&t.name))
                                    .collect();
                                sys.tools_added = if added.is_empty() { None } else { Some(added) };
                                sys.tools_removed = if removed.is_empty() { None } else { Some(removed) };
                                AgentMessage::from(sys)
                            } else {
                                message
                            }
                        })
                        .collect())
                })
            })));
    }

    pub(crate) fn _install_agent_forced_prompt_projection(&self) {
        let previous = self.inner.agent.transform_context();
        let weak = Arc::downgrade(&self.inner);
        self.inner
            .agent
            .set_transform_context(Some(Arc::new(move |messages, signal| {
                let prev = previous.clone();
                let weak = weak.clone();
                Box::pin(async move {
                    let transformed = match prev {
                        Some(f) => f(messages, signal).await?,
                        None => messages,
                    };
                    let Some(inner) = weak.upgrade() else {
                        return Ok(transformed);
                    };
                    let forced = inner
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .run_system_prompt_options
                        .as_ref()
                        .and_then(|opts| opts.force_system_prompt.clone());
                    let Some(forced) = forced else {
                        return Ok(transformed);
                    };
                    let llm_msgs: Vec<_> = transformed.iter().filter_map(|m| m.as_message().cloned()).collect();
                    let current = get_current_system_message(&llm_msgs);
                    let head = SystemMessage {
                        content: pi_ai::types::SystemMessageContent::Text(forced),
                        sections: None,
                        tools_added: current.as_ref().and_then(|c| c.tools_added.clone()),
                        tools_removed: None,
                        timestamp: current
                            .as_ref()
                            .map(|c| c.timestamp)
                            .unwrap_or_else(pi_js::time::now_ms),
                    };
                    let mut out = Vec::with_capacity(transformed.len() + 1);
                    out.push(AgentMessage::from(head));
                    out.extend(transformed.into_iter().filter(|m| m.role() != Some("system")));
                    Ok(out)
                })
            })));
    }

    pub(crate) fn _restore_tools_from_transcript(&self, modifiers: &[String]) {
        {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pending_tool_names
                .clear();
        }
        let branch = self.inner.session_manager.get_branch(None);
        let system_messages: Vec<pi_ai::types::Message> = branch
            .into_iter()
            .filter_map(|entry| match entry {
                SessionEntry::Message(m) if m.message.role() == Some("system") => m.message.as_message().cloned(),
                _ => None,
            })
            .collect();
        if system_messages.is_empty() {
            return;
        }
        let mut mentioned = IndexSet::new();
        for msg in &system_messages {
            if let pi_ai::types::Message::System(sm) = msg {
                for tool in sm.tools_added.iter().flatten() {
                    mentioned.insert(tool.name.clone());
                }
                for tool in sm.tools_removed.iter().flatten() {
                    mentioned.insert(tool.name.clone());
                }
            }
        }
        let registry_keys: Vec<String> = {
            let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard
                .tool_registry
                .keys()
                .filter(|name| !mentioned.contains(*name) && !guard.base_tool_definitions.contains_key(*name))
                .cloned()
                .collect()
        };
        let new_tools: Vec<String> = registry_keys
            .into_iter()
            .filter(|name| self._is_activated_on_registration(name))
            .collect();
        let mut combined: Vec<String> = get_current_tools(&system_messages)
            .into_iter()
            .map(|t| t.name)
            .collect();
        combined.extend(new_tools);
        let names = apply_tool_modifiers(&combined, modifiers);
        let allowed_names: IndexSet<String> = names
            .iter()
            .filter(|name| self._is_allowed_tool(name))
            .cloned()
            .collect();
        {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pending_tool_names = allowed_names;
        }
        self._set_active_tools(&names);
    }

    // =========================================================================
    // Prompting
    // =========================================================================

    pub(crate) fn _start_run(
        &self,
        trigger: BeforeAgentStartTrigger,
        on_started: Option<Box<dyn FnOnce() + Send>>,
    ) -> BoxFuture<'_, pi_js::Result<()>> {
        Box::pin(async move {
            {
                let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                guard.is_agent_run_active = true;
                guard.agent_run_abort_requested = false;
            }
            let messages = match self._prepare_run(trigger).await {
                Ok(msgs) => msgs,
                Err(err) => {
                    {
                        self.inner
                            .state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .is_agent_run_active = false;
                    }
                    self._flush_pending_bash_messages();
                    self._flush_pending_custom_messages();
                    self._resolve_idle_wait_if_idle();
                    return Err(err);
                }
            };
            self._flush_pending_bash_messages();
            self._flush_pending_custom_messages();
            if let Some(cb) = on_started {
                cb();
            }

            {
                let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                guard.failed_response = None;
                guard.pending_tool_names.clear();
                guard.message_end_replacements.clear();
            }
            self._record_selection();

            let result: pi_js::Result<()> = async {
                self.inner.agent.prompt(PromptInput::Messages(messages)).await?;
                loop {
                    let abort_req = self
                        .inner
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .agent_run_abort_requested;
                    if abort_req {
                        break;
                    }
                    if self._handle_post_agent_run().await {
                        let abort_req = self
                            .inner
                            .state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .agent_run_abort_requested;
                        if abort_req {
                            break;
                        }
                        self.inner.agent.continue_run().await?;
                        continue;
                    }
                    let abort_req = self
                        .inner
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .agent_run_abort_requested;
                    if abort_req || !self._run_before_settle_boundary().await {
                        break;
                    }
                    let abort_req = self
                        .inner
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .agent_run_abort_requested;
                    if abort_req {
                        break;
                    }
                    self.inner.agent.continue_run().await?;
                }
                Ok(())
            }
            .await;

            let abort_req = {
                let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                guard.failed_response = None;
                guard.run_system_prompt_options = None;
                guard.agent_run_abort_requested
            };
            if abort_req {
                self._finish_cancelled_retry();
            }
            self._flush_pending_bash_messages();
            self._flush_pending_custom_messages();
            self._emit_agent_settled().await;
            result
        })
    }

    async fn _prepare_run(&self, trigger: BeforeAgentStartTrigger) -> pi_js::Result<Vec<AgentMessage>> {
        if let Some(last_assistant) = self._find_last_assistant_message() {
            self._check_compaction(&last_assistant, false).await;
        }

        let (runner, base_opts) = {
            let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            (guard.extension_runner.clone(), guard.base_system_prompt_options.clone())
        };
        let selected_tools_before = base_opts.selected_tools.clone();
        let mut next_system_opts = base_opts.clone();
        let mut extra_custom_messages: Vec<CustomMessageDraft> = Vec::new();

        if let Some(ref runner) = runner {
            let build_opts = BuildSystemPromptOptions {
                custom_prompt: base_opts.custom_prompt.clone(),
                force_system_prompt: base_opts.force_system_prompt.clone(),
                selected_tools: Some(base_opts.selected_tools.clone()),
                hidden_tools: Some(base_opts.hidden_tools.clone()),
                tool_snippets: Some(base_opts.tool_snippets.clone()),
                tool_guidelines: Some(base_opts.tool_guidelines.clone()),
                prompt_guidelines: Some(base_opts.prompt_guidelines.clone()),
                append_system_prompt: Some(base_opts.append_system_prompt.clone()),
                sections: Some(base_opts.sections.clone()),
                cwd: base_opts.cwd.clone(),
                context_files: Some(base_opts.context_files.clone()),
                skills: Some(base_opts.skills.clone()),
            };
            let before_res = runner
                .emit_before_agent_start_trigger(trigger.clone(), build_opts)
                .await;
            next_system_opts = before_res.system_prompt_options;
            extra_custom_messages = before_res.messages;
        }

        let handler_edited_tools = next_system_opts.selected_tools != selected_tools_before;
        if !handler_edited_tools {
            next_system_opts.selected_tools = self.get_active_tool_names();
        }

        let mut messages: Vec<AgentMessage> = Vec::new();
        match trigger {
            BeforeAgentStartTrigger::Custom { message } => {
                messages.push(AgentMessage::from(message));
            }
            BeforeAgentStartTrigger::User { prompt, images } => {
                let (normalized_images, hints) = self._normalize_prompt_images(images).await;
                let user_text = if hints.is_empty() {
                    prompt
                } else {
                    format!("{prompt}\n\n{}", hints.join("\n"))
                };
                let mut user_content = Vec::with_capacity(1 + normalized_images.len());
                user_content.push(UserContentBlock::Text(TextContent::new(user_text)));
                for img in normalized_images {
                    user_content.push(UserContentBlock::Image(img));
                }
                messages.push(AgentMessage::from(UserMessage {
                    content: UserMessageContent::Blocks(user_content),
                    timestamp: pi_js::time::now_ms(),
                }));
            }
        }

        let next_turn = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut guard.pending_next_turn_messages)
        };
        messages.extend(next_turn);

        for msg in extra_custom_messages {
            messages.push(AgentMessage::from(CustomMessage {
                role: "custom".to_string(),
                custom_type: msg.custom_type,
                content: msg.content,
                display: msg.display,
                details: msg.details,
                timestamp: pi_js::time::now_ms(),
            }));
        }

        let current_agent_msgs = self.inner.agent.state().messages;
        let update_message = self._prepare_prompt_and_tool_loadout(&mut next_system_opts, Some(&current_agent_msgs));
        {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.run_system_prompt_options = Some(next_system_opts);
        }
        if let Some(upd) = update_message {
            messages.insert(0, AgentMessage::from(upd));
        }

        Ok(messages)
    }

    async fn _handle_post_agent_run(&self) -> bool {
        self._await_pending_extension_sends().await;
        let (message, abort_req) = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let message = guard.last_assistant_message.take();
            (message, guard.agent_run_abort_requested)
        };
        if abort_req {
            self._finish_cancelled_retry();
            return false;
        }
        let Some(message) = message else {
            return self.inner.agent.has_queued_messages();
        };

        if self._is_retryable_error(&message) && self._prepare_retry(&message).await {
            let abort_req = {
                let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                guard.failed_response = Some(message);
                guard.agent_run_abort_requested
            };
            if abort_req {
                self._finish_cancelled_retry();
            }
            return !abort_req;
        }
        if self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .agent_run_abort_requested
        {
            self._finish_cancelled_retry();
            return false;
        }

        let failed_retry_attempt = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if message.stop_reason == pi_ai::StopReason::Error && guard.retry_attempt > 0 {
                let attempt = guard.retry_attempt;
                guard.retry_attempt = 0;
                Some(attempt)
            } else {
                None
            }
        };
        if let Some(attempt) = failed_retry_attempt {
            self._emit(AgentSessionEvent::AutoRetryEnd {
                success: false,
                attempt,
                final_error: message.error_message.clone(),
            });
        }

        if self._check_compaction(&message, true).await {
            return !self
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .agent_run_abort_requested;
        }

        !self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .agent_run_abort_requested
            && self.inner.agent.has_queued_messages()
    }

    async fn _run_before_settle_boundary(&self) -> bool {
        let (runner, outcome) = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let Some(runner) = guard.extension_runner.clone() else {
                return self.inner.agent.has_queued_messages();
            };
            if !runner.has_handlers("agent_before_settle") {
                return self.inner.agent.has_queued_messages();
            }
            guard.is_before_settle = true;
            guard.abort_during_before_settle = false;
            (runner, guard.last_activity_outcome)
        };

        let weak = Arc::downgrade(&self.inner);
        let result = runner
            .emit_boundary(
                BoundaryBaseEvent::AgentBeforeSettle { outcome },
                move |entries: &[crate::core::extensions::types::SessionBoundaryDraft]| {
                    let entries = entries.to_vec();
                    let weak = weak.clone();
                    Box::pin(async move {
                        Ok(weak
                            .upgrade()
                            .map(|inner| {
                                AgentSession { inner }
                                    ._build_boundary_context(&entries, BoundaryKind::AgentBeforeSettle)
                            })
                            .unwrap_or_default())
                    })
                },
            )
            .await;

        self._await_pending_extension_sends().await;
        self._commit_boundary_drafts(&result.entries);
        self._flush_pending_custom_messages();
        let final_context = self._build_boundary_context(&[], BoundaryKind::AgentBeforeSettle);

        let abort_during = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.is_before_settle = false;
            guard.abort_during_before_settle
        };
        if abort_during {
            return false;
        }

        let should_continue = result.r#continue || self.inner.agent.has_queued_messages();
        if should_continue && !final_context.can_continue {
            if result.r#continue {
                self._report_invalid_boundary_continuation("agent_before_settle");
            }
            return false;
        }
        should_continue
    }

    async fn _run_input_handlers(
        &self,
        text: &str,
        images: Option<Vec<ImageContent>>,
        source: InputSource,
        streaming_behavior: Option<StreamingBehavior>,
    ) -> Option<(String, Option<Vec<ImageContent>>)> {
        let runner = {
            let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.extension_runner.clone()
        };
        let Some(runner) = runner else {
            return Some((text.to_string(), images));
        };
        if !runner.has_handlers("input") {
            return Some((text.to_string(), images));
        }

        let delivery = streaming_behavior.map(|b| match b {
            StreamingBehavior::Steer => InputDeliveryBehavior::Steer,
            StreamingBehavior::FollowUp => InputDeliveryBehavior::FollowUp,
        });
        match runner.emit_input(text, images.clone(), source, delivery).await {
            crate::core::extensions::types::InputEventResult::Handled => None,
            crate::core::extensions::types::InputEventResult::Transform {
                text: next_text,
                images: next_images,
            } => Some((next_text, next_images.or(images))),
            crate::core::extensions::types::InputEventResult::Continue => Some((text.to_string(), images)),
        }
    }

    async fn _normalize_prompt_images(&self, images: Option<Vec<ImageContent>>) -> (Vec<ImageContent>, Vec<String>) {
        let Some(images) = images else {
            return (Vec::new(), Vec::new());
        };
        let auto_resize = self.inner.settings_manager.get_image_auto_resize();
        let resize_options = self
            ._limits_model()
            .and_then(|m| m.input_limits)
            .and_then(|l| l.images)
            .and_then(|img| img.resize)
            .map(|r| crate::utils::image_resize::ImageResizeOptions {
                max_width: r.max_width.and_then(|v| u32::try_from(v).ok()),
                max_height: r.max_height.and_then(|v| u32::try_from(v).ok()),
                max_bytes: r.max_bytes.and_then(|v| usize::try_from(v).ok()),
                jpeg_quality: r.jpeg_quality.map(|v| v.clamp(0.0, 100.0) as u8),
            });
        let mut normalized_images = Vec::new();
        let mut hints = Vec::new();
        for image in images {
            let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&image.data) else {
                hints.push("[Image omitted: invalid base64 payload.]".to_string());
                continue;
            };
            match process_image(
                &bytes,
                &image.mime_type,
                Some(ProcessImageOptions {
                    auto_resize_images: Some(auto_resize),
                    resize_options: resize_options.clone(),
                }),
            )
            .await
            {
                ProcessImageResult::Ok {
                    data,
                    mime_type,
                    hints: img_hints,
                } => {
                    normalized_images.push(ImageContent::new(data, mime_type));
                    hints.extend(img_hints);
                }
                ProcessImageResult::Err { message } => {
                    hints.push(message);
                }
            }
        }
        (normalized_images, hints)
    }

    /// Send a prompt to the agent.
    pub fn prompt<'a>(&'a self, text: &'a str, options: Option<PromptOptions>) -> BoxFuture<'a, pi_js::Result<()>> {
        Box::pin(async move {
            {
                let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                if guard.is_emitting_agent_settled {
                    let weak = Arc::downgrade(&self.inner);
                    let text_owned = text.to_string();
                    let options_owned = options;
                    guard.deferred_settled_actions.push(Box::new(move || {
                        Box::pin(async move {
                            if let Some(inner) = weak.upgrade() {
                                let _ = AgentSession { inner }.prompt(&text_owned, options_owned).await;
                            }
                        })
                    }));
                    return Ok(());
                }
            }

            let options = options.unwrap_or_default();
            let expand_prompt_templates = options.expand_prompt_templates.unwrap_or(true);
            let preflight_result = options.preflight_result.clone();

            if expand_prompt_templates && text.starts_with('/') {
                if self._try_execute_extension_command(text).await {
                    if let Some(ref cb) = preflight_result {
                        cb(PromptPreflightOutcome::Handled);
                    }
                    return Ok(());
                }
            }

            let is_compacting_or_summary = {
                let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                guard.compaction_abort_controller.is_some() || guard.branch_summary_abort_controller.is_some()
            };
            if is_compacting_or_summary {
                return Err(pi_js::Error::msg(
                    "Cannot submit a prompt while compaction is in progress. Wait for compaction to finish and retry.",
                ));
            }

            struct PromptPreflightGuard {
                inner: Arc<AgentSessionInner>,
            }
            impl Drop for PromptPreflightGuard {
                fn drop(&mut self) {
                    let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                    guard.prompt_preflight_count = guard.prompt_preflight_count.saturating_sub(1);
                }
            }
            {
                let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                guard.prompt_preflight_count += 1;
            }
            let preflight_guard = PromptPreflightGuard {
                inner: Arc::clone(&self.inner),
            };
            let trigger = self._prepare_prompt(text, &options, preflight_result.as_ref()).await?;
            drop(preflight_guard);

            if let Some(trigger) = trigger {
                let cb_opt = preflight_result;
                let on_started: Option<Box<dyn FnOnce() + Send>> = cb_opt.map(|cb| {
                    Box::new(move || {
                        cb(PromptPreflightOutcome::Started);
                    }) as Box<dyn FnOnce() + Send>
                });
                self._start_run(trigger, on_started).await?;
            }
            Ok(())
        })
    }

    async fn _prepare_prompt(
        &self,
        text: &str,
        options: &PromptOptions,
        preflight_result: Option<&PromptPreflightCallback>,
    ) -> pi_js::Result<Option<BeforeAgentStartTrigger>> {
        let expand_prompt_templates = options.expand_prompt_templates.unwrap_or(true);
        let Some((current_text, current_images)) = self
            ._run_input_handlers(
                text,
                options.images.clone(),
                options.source.unwrap_or(InputSource::Interactive),
                if self.is_streaming() {
                    options.streaming_behavior
                } else {
                    None
                },
            )
            .await
        else {
            if let Some(cb) = preflight_result {
                cb(PromptPreflightOutcome::Handled);
            }
            return Ok(None);
        };

        let mut expanded_text = current_text;
        if expand_prompt_templates {
            expanded_text = self._expand_skill_command(&expanded_text);
            let templates = self.prompt_templates();
            expanded_text = expand_prompt_template(&expanded_text, &templates);
        }

        if self.is_streaming() {
            let Some(behavior) = options.streaming_behavior else {
                return Err(pi_js::Error::msg(
                    "Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.",
                ));
            };
            match behavior {
                StreamingBehavior::FollowUp => {
                    self._queue_follow_up(&expanded_text, current_images);
                }
                StreamingBehavior::Steer => {
                    self._queue_steer(&expanded_text, current_images);
                }
            }
            if let Some(cb) = preflight_result {
                cb(PromptPreflightOutcome::Queued);
            }
            return Ok(None);
        }

        let Some(model) = self.model() else {
            return Err(pi_js::Error::msg(format_no_model_selected_message()));
        };

        let has_configured_auth = self.inner.model_runtime.has_configured_auth(&model.provider)
            || self
                .inner
                .model_runtime
                .check_auth(&model.provider, None)
                .await
                .ok()
                .flatten()
                .is_some();
        if !has_configured_auth {
            if self.inner.model_runtime.is_using_oauth(&model.provider) {
                return Err(pi_js::Error::msg(format!(
                    "Authentication failed for \"{}\". Credentials may have expired or network is unavailable. Run '/login {}' to re-authenticate.",
                    model.provider, model.provider
                )));
            }
            return Err(pi_js::Error::msg(format_no_api_key_found_message(&model.provider)));
        }

        Ok(Some(BeforeAgentStartTrigger::User {
            prompt: expanded_text,
            images: current_images,
        }))
    }

    async fn _try_execute_extension_command(&self, text: &str) -> bool {
        let rest = &text[1..];
        let (command_name, args) = match rest.find(' ') {
            Some(idx) => (&rest[..idx], &rest[idx + 1..]),
            None => (rest, ""),
        };

        let runner = {
            let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.extension_runner.clone()
        };
        let Some(runner) = runner else {
            return false;
        };
        let Some(command) = runner.get_command(command_name) else {
            return false;
        };
        let ctx = runner.create_command_context();
        if let Err(err) = (command.command.handler)(args.to_string(), ctx).await {
            runner.emit_error(ExtensionError {
                extension_path: format!("command:{command_name}"),
                event: "command".to_string(),
                error: err.to_string(),
                stack: None,
            });
        }
        true
    }

    fn _expand_skill_command(&self, text: &str) -> String {
        let Some(after_prefix) = text.strip_prefix("/skill:") else {
            return text.to_string();
        };
        let (skill_name, args) = match after_prefix.find(' ') {
            Some(idx) => (&after_prefix[..idx], pi_js::str16::trim(&after_prefix[idx + 1..])),
            None => (after_prefix, ""),
        };

        let skills = self.inner.resource_loader.get_skills().0;
        let Some(skill) = skills.into_iter().find(|s| s.name == skill_name) else {
            return text.to_string();
        };

        match std::fs::read_to_string(&skill.file_path) {
            Ok(content) => {
                let body = strip_frontmatter(&content);
                let body_trimmed = pi_js::str16::trim(&body);
                let skill_block = format!(
                    "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{body_trimmed}\n</skill>",
                    skill.name, skill.file_path, skill.base_dir
                );
                if args.is_empty() {
                    skill_block
                } else {
                    format!("{skill_block}\n\n{args}")
                }
            }
            Err(err) => {
                let runner = self
                    .inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .extension_runner
                    .clone();
                if let Some(runner) = runner {
                    runner.emit_error(ExtensionError {
                        extension_path: skill.file_path.clone(),
                        event: "skill_expansion".to_string(),
                        error: err.to_string(),
                        stack: None,
                    });
                }
                text.to_string()
            }
        }
    }

    async fn _queue_user_input(
        &self,
        text: &str,
        images: Option<Vec<ImageContent>>,
        behavior: StreamingBehavior,
        source: InputSource,
    ) -> pi_js::Result<QueuedInputDisposition> {
        if text.starts_with('/') {
            self._throw_if_extension_command(text)?;
        }

        let Some((processed_text, processed_images)) = self
            ._run_input_handlers(
                text,
                images,
                source,
                if self.is_streaming() { Some(behavior) } else { None },
            )
            .await
        else {
            return Ok(QueuedInputDisposition::Handled);
        };

        let mut expanded = self._expand_skill_command(&processed_text);
        let templates = self.prompt_templates();
        expanded = expand_prompt_template(&expanded, &templates);

        match behavior {
            StreamingBehavior::Steer => self._queue_steer(&expanded, processed_images),
            StreamingBehavior::FollowUp => self._queue_follow_up(&expanded, processed_images),
        }
        Ok(QueuedInputDisposition::Queued)
    }

    /// Queue a steering message while the agent is running.
    pub async fn steer(
        &self,
        text: &str,
        images: Option<Vec<ImageContent>>,
        options: Option<QueueInputOptions>,
    ) -> pi_js::Result<QueuedInputDisposition> {
        self._queue_user_input(
            text,
            images,
            StreamingBehavior::Steer,
            options.and_then(|o| o.source).unwrap_or(InputSource::Interactive),
        )
        .await
    }

    /// Queue a follow-up message to be processed after the agent finishes.
    pub async fn follow_up(
        &self,
        text: &str,
        images: Option<Vec<ImageContent>>,
        options: Option<QueueInputOptions>,
    ) -> pi_js::Result<QueuedInputDisposition> {
        self._queue_user_input(
            text,
            images,
            StreamingBehavior::FollowUp,
            options.and_then(|o| o.source).unwrap_or(InputSource::Interactive),
        )
        .await
    }

    fn _queue_steer(&self, text: &str, images: Option<Vec<ImageContent>>) {
        let has_images = images.as_ref().is_some_and(|v| !v.is_empty());
        let mut content = vec![UserContentBlock::Text(TextContent::new(text))];
        if let Some(imgs) = images {
            content.extend(imgs.into_iter().map(UserContentBlock::Image));
        }
        let message = AgentMessage::from(UserMessage {
            content: UserMessageContent::Blocks(content),
            timestamp: pi_js::time::now_ms(),
        });
        {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.steering_messages.push(text.to_string());
            if has_images {
                guard.queued_messages_with_images.push(message.clone());
            }
        }
        self._emit_queue_update();
        self.inner.agent.steer(message);
    }

    fn _queue_follow_up(&self, text: &str, images: Option<Vec<ImageContent>>) {
        let has_images = images.as_ref().is_some_and(|v| !v.is_empty());
        let mut content = vec![UserContentBlock::Text(TextContent::new(text))];
        if let Some(imgs) = images {
            content.extend(imgs.into_iter().map(UserContentBlock::Image));
        }
        let message = AgentMessage::from(UserMessage {
            content: UserMessageContent::Blocks(content),
            timestamp: pi_js::time::now_ms(),
        });
        {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.follow_up_messages.push(text.to_string());
            if has_images {
                guard.queued_messages_with_images.push(message.clone());
            }
        }
        self._emit_queue_update();
        self.inner.agent.follow_up(message);
    }

    pub(crate) async fn _normalize_queued_message_images(&self, msg: &AgentMessage) -> Option<AgentMessage> {
        let user = msg.as_user()?;
        let UserMessageContent::Blocks(ref blocks) = user.content else {
            return None;
        };
        let Some(UserContentBlock::Text(first)) = blocks.first() else {
            return None;
        };
        let rest = &blocks[1..];
        let images: Vec<ImageContent> = rest
            .iter()
            .filter_map(|block| match block {
                UserContentBlock::Image(img) => Some(img.clone()),
                _ => None,
            })
            .collect();
        let (normalized_images, hints) = self._normalize_prompt_images(Some(images)).await;
        let text = if hints.is_empty() {
            first.text.clone()
        } else {
            format!("{}\n\n{}", first.text, hints.join("\n"))
        };
        let mut next_content = Vec::with_capacity(1 + rest.len() + normalized_images.len());
        next_content.push(UserContentBlock::Text(TextContent::new(text)));
        for block in rest {
            if !matches!(block, UserContentBlock::Image(_)) {
                next_content.push(block.clone());
            }
        }
        for img in normalized_images {
            next_content.push(UserContentBlock::Image(img));
        }
        Some(AgentMessage::from(UserMessage {
            content: UserMessageContent::Blocks(next_content),
            timestamp: user.timestamp,
        }))
    }

    fn _throw_if_extension_command(&self, text: &str) -> pi_js::Result<()> {
        let rest = &text[1..];
        let command_name = match rest.find(' ') {
            Some(idx) => &rest[..idx],
            None => rest,
        };
        let runner = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extension_runner
            .clone();
        if let Some(runner) = runner
            && runner.get_command(command_name).is_some()
        {
            return Err(pi_js::Error::msg(format!(
                "Extension command \"/{command_name}\" cannot be queued. Use prompt() or execute the command when not streaming."
            )));
        }
        Ok(())
    }

    /// Synchronously performs custom-message queuing/appending when no new run is needed, returning
    /// the message to prompt when a new agent turn should start asynchronously.
    pub(crate) fn _try_send_custom_message_sync(
        &self,
        message: SendCustomMessageDraft,
        options: Option<SendCustomMessageOptions>,
    ) -> Option<CustomMessage> {
        let opts = options.unwrap_or_default();
        let app_message = CustomMessage {
            role: "custom".to_string(),
            custom_type: message.custom_type,
            content: message.content,
            display: message.display,
            details: message.details,
            timestamp: pi_js::time::now_ms(),
        };

        if opts.deliver_as == Some(CustomMessageDelivery::NextTurn) {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pending_next_turn_messages
                .push(AgentMessage::from(app_message));
        } else if self.is_streaming() && opts.trigger_turn != Some(false) {
            let agent_msg = AgentMessage::from(app_message);
            if opts.deliver_as == Some(CustomMessageDelivery::FollowUp) {
                self.inner.agent.follow_up(agent_msg);
            } else {
                self.inner.agent.steer(agent_msg);
            }
        } else if opts.trigger_turn == Some(true) {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if guard.is_emitting_agent_settled {
                let weak = Arc::downgrade(&self.inner);
                guard.deferred_settled_actions.push(Box::new(move || {
                    Box::pin(async move {
                        if let Some(inner) = weak.upgrade() {
                            let _ = AgentSession { inner }
                                .send_custom_message(
                                    SendCustomMessageDraft {
                                        custom_type: app_message.custom_type,
                                        content: app_message.content,
                                        display: app_message.display,
                                        details: app_message.details,
                                    },
                                    Some(opts),
                                )
                                .await;
                        }
                    })
                }));
                return None;
            }
            return Some(app_message);
        } else if self.is_streaming() {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pending_custom_messages
                .push(AgentMessage::from(app_message));
        } else {
            self._append_custom_message(app_message);
        }
        None
    }

    /// Send a custom message to the session. Creates a `CustomMessageEntry`.
    pub async fn send_custom_message(
        &self,
        message: SendCustomMessageDraft,
        options: Option<SendCustomMessageOptions>,
    ) -> pi_js::Result<()> {
        if let Some(agent_msg) = self._try_send_custom_message_sync(message, options) {
            self._start_run(BeforeAgentStartTrigger::Custom { message: agent_msg }, None)
                .await?;
        }
        Ok(())
    }

    fn _append_custom_message(&self, app_message: CustomMessage) {
        let _ = self.inner.session_manager.append_custom_message_entry(
            &app_message.custom_type,
            app_message.content.clone(),
            app_message.display,
            app_message.details.clone(),
        );
        self._refresh_finalized_context();
        let agent_msg = AgentMessage::from(app_message);
        self._emit(AgentSessionEvent::MessageStart {
            message: agent_msg.clone(),
        });
        self._emit(AgentSessionEvent::MessageEnd { message: agent_msg });
    }

    pub(crate) fn _flush_pending_custom_messages(&self) {
        let pending = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if guard.pending_custom_messages.is_empty() {
                return;
            }
            std::mem::take(&mut guard.pending_custom_messages)
        };
        for msg in pending {
            if let Some(val) = msg.as_custom()
                && let Ok(cm) = pi_js::json::parse_as::<CustomMessage>(&pi_js::json::stringify(val))
            {
                self._append_custom_message(cm);
            }
        }
    }

    fn _split_user_message_input(content: UserMessageInput) -> (String, Option<Vec<ImageContent>>) {
        match content {
            UserMessageInput::Text(s) => (s, None),
            UserMessageInput::Blocks(blocks) => {
                let mut text_parts = Vec::new();
                let mut imgs = Vec::new();
                for block in blocks {
                    match block {
                        UserContentBlock::Text(t) => text_parts.push(t.text),
                        UserContentBlock::Image(img) => imgs.push(img),
                    }
                }
                let images = if imgs.is_empty() { None } else { Some(imgs) };
                (text_parts.join("\n"), images)
            }
        }
    }

    /// Synchronously queues extension user messages when safe to do without awaiting hooks, returning
    /// `(track_during_run, future)` when async execution is required.
    pub(crate) fn _try_send_user_message_sync(
        &self,
        content: UserMessageInput,
        options: Option<SendUserMessageOptions>,
    ) -> pi_js::Result<Option<(bool, BoxFuture<'static, pi_js::Result<()>>)>> {
        let (text, images) = Self::_split_user_message_input(content);
        let opts = options.unwrap_or_default();
        let expand_prompt_templates = opts.expand_prompt_templates.unwrap_or(false);

        {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if guard.is_emitting_agent_settled {
                let weak = Arc::downgrade(&self.inner);
                let text_owned = text;
                let images_owned = images;
                let deliver_as = opts.deliver_as;
                guard.deferred_settled_actions.push(Box::new(move || {
                    Box::pin(async move {
                        if let Some(inner) = weak.upgrade() {
                            let _ = AgentSession { inner }
                                .prompt(
                                    &text_owned,
                                    Some(PromptOptions {
                                        expand_prompt_templates: Some(expand_prompt_templates),
                                        streaming_behavior: deliver_as,
                                        images: images_owned,
                                        source: Some(InputSource::Extension),
                                        preflight_result: None,
                                    }),
                                )
                                .await;
                        }
                    })
                }));
                return Ok(None);
            }
        }

        let is_streaming = self.is_streaming();
        if is_streaming {
            let runner = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extension_runner
                .clone();
            let is_ext_cmd = expand_prompt_templates && text.starts_with('/') && {
                let rest = &text[1..];
                let cmd_name = rest.split(' ').next().unwrap_or(rest);
                runner.as_ref().is_some_and(|r| r.get_command(cmd_name).is_some())
            };
            let has_input_handlers = runner.as_ref().is_some_and(|r| r.has_handlers("input"));
            if !is_ext_cmd && !has_input_handlers {
                let mut expanded_text = text;
                if expand_prompt_templates {
                    expanded_text = self._expand_skill_command(&expanded_text);
                    let templates = self.prompt_templates();
                    expanded_text = expand_prompt_template(&expanded_text, &templates);
                }
                let Some(behavior) = opts.deliver_as else {
                    return Err(pi_js::Error::msg(
                        "Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.",
                    ));
                };
                match behavior {
                    StreamingBehavior::FollowUp => self._queue_follow_up(&expanded_text, images),
                    StreamingBehavior::Steer => self._queue_steer(&expanded_text, images),
                }
                return Ok(None);
            }
        }

        let session = self.clone();
        Ok(Some((
            is_streaming,
            Box::pin(async move {
                session
                    .prompt(
                        &text,
                        Some(PromptOptions {
                            expand_prompt_templates: Some(expand_prompt_templates),
                            streaming_behavior: opts.deliver_as,
                            images,
                            source: Some(InputSource::Extension),
                            preflight_result: None,
                        }),
                    )
                    .await
            }),
        )))
    }

    /// Send a user message to the agent. Always triggers a turn.
    pub async fn send_user_message(
        &self,
        content: impl Into<UserMessageInput>,
        options: Option<SendUserMessageOptions>,
    ) -> pi_js::Result<()> {
        let (text, images) = Self::_split_user_message_input(content.into());
        let opts = options.unwrap_or_default();
        self.prompt(
            &text,
            Some(PromptOptions {
                expand_prompt_templates: Some(opts.expand_prompt_templates.unwrap_or(false)),
                streaming_behavior: opts.deliver_as,
                images,
                source: Some(InputSource::Extension),
                preflight_result: None,
            }),
        )
        .await
    }

    /// Clear all queued messages and return them.
    pub fn clear_queue(&self) -> ClearQueueResult {
        let (steering, follow_up) = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                std::mem::take(&mut guard.steering_messages),
                std::mem::take(&mut guard.follow_up_messages),
            )
        };
        self.inner.agent.clear_all_queues();
        self._emit_queue_update();
        ClearQueueResult { steering, follow_up }
    }

    /// Number of pending messages (includes both steering and follow-up).
    pub fn pending_message_count(&self) -> usize {
        let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.steering_messages.len() + guard.follow_up_messages.len()
    }

    /// Get pending steering messages.
    pub fn get_steering_messages(&self) -> Vec<String> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .steering_messages
            .clone()
    }

    /// Get pending follow-up messages.
    pub fn get_follow_up_messages(&self) -> Vec<String> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .follow_up_messages
            .clone()
    }

    pub fn resource_loader(&self) -> &Arc<dyn ResourceLoader> {
        &self.inner.resource_loader
    }

    /// Synchronously mark abort requested and signal active operations to abort without waiting for idle.
    pub fn request_abort(&self) {
        {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if guard.is_agent_run_active {
                guard.agent_run_abort_requested = true;
            }
            if guard.is_before_settle {
                guard.abort_during_before_settle = true;
            }
        }
        self.abort_retry();
        self.abort_compaction();
        self.abort_branch_summary();
        self.inner.agent.abort();
    }

    /// Abort current operation and wait for agent to become idle.
    pub async fn abort(&self) {
        self.request_abort();
        self.wait_for_idle().await;
    }

    pub async fn wait_for_idle(&self) {
        let rx = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let idle = !guard.is_agent_run_active
                && guard.compaction_abort_controller.is_none()
                && guard.auto_compaction_abort_controller.is_none()
                && guard.branch_summary_abort_controller.is_none();
            if idle {
                return;
            }
            let (tx, rx) = tokio::sync::oneshot::channel();
            guard.idle_waiters.push(tx);
            rx
        };
        let _ = rx.await;
    }

    // =========================================================================
    // Model Management
    // =========================================================================

    async fn _emit_model_select(&self, next_model: &Model, previous_model: Option<&Model>, source: ModelSelectSource) {
        let prev_any = previous_model.map(|m| AnyModel::Chat(m.clone()));
        let next_any = AnyModel::Chat(next_model.clone());
        if models_are_equal(prev_any.as_ref(), Some(&next_any)) {
            return;
        }
        let runner = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extension_runner
            .clone();
        if let Some(runner) = runner {
            let _ = runner
                .emit(ModelSelectEvent {
                    model: next_model.clone(),
                    previous_model: previous_model.cloned(),
                    source,
                })
                .await;
        }
    }

    /// Set model directly.
    /// Validates that auth is configured and saves to the session transcript.
    pub async fn set_model(&self, model: Model, options: Option<ModelMutationOptions>) -> pi_js::Result<()> {
        if self
            .inner
            .model_runtime
            .check_auth(&model.provider, None)
            .await
            .ok()
            .flatten()
            .is_none()
        {
            return Err(pi_js::Error::msg(format!(
                "No API key for {}/{}",
                model.provider, model.id
            )));
        }

        let previous_model = self.model();
        let thinking_level = self._get_thinking_level_for_model_switch(Some(&model), None);
        {
            self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).has_model = true;
        }
        self.inner.agent.set_model(model.clone());
        let _ = self
            .inner
            .session_manager
            .append_model_change(&model.provider, &model.id);

        if options.is_some_and(|o| o.persist == Some(true)) {
            self.inner
                .settings_manager
                .set_default_model_and_provider(&model.provider, &model.id);
            self._add_persisted_default_to_non_empty_scope(&model);
        }

        self.set_thinking_level(thinking_level, None);
        self._emit_model_select(&model, previous_model.as_ref(), ModelSelectSource::Set)
            .await;
        Ok(())
    }

    fn _add_persisted_default_to_non_empty_scope(&self, model: &Model) {
        {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if guard.scoped_models.is_empty() {
                return;
            }
            let model_any = AnyModel::Chat(model.clone());
            if guard
                .scoped_models
                .iter()
                .any(|scoped| models_are_equal(Some(&AnyModel::Chat(scoped.model.clone())), Some(&model_any)))
            {
                return;
            }
            guard.scoped_models.push(ScopedModelEntry {
                model: model.clone(),
                thinking_level: None,
            });
        }

        let Some(mut enabled_models) = self.inner.settings_manager.get_enabled_models() else {
            return;
        };
        if enabled_models.is_empty() {
            return;
        }
        let model_reference = format!("{}/{}", model.provider, model.id);
        if enabled_models
            .iter()
            .any(|pattern| pattern.eq_ignore_ascii_case(&model_reference))
        {
            return;
        }
        enabled_models.push(model_reference);
        self.inner.settings_manager.set_enabled_models(Some(enabled_models));
    }

    /// Cycle to next/previous model.
    pub async fn cycle_model(
        &self,
        direction: Option<CycleDirection>,
        options: Option<ModelMutationOptions>,
    ) -> pi_js::Result<Option<ModelCycleResult>> {
        let dir = direction.unwrap_or(CycleDirection::Forward);
        let opts = options.unwrap_or_default();
        let has_scoped = !self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .scoped_models
            .is_empty();
        if has_scoped {
            self._cycle_scoped_model(dir, opts).await
        } else {
            self._cycle_available_model(dir, opts).await
        }
    }

    fn get_next_cycle_index(current_index: Option<usize>, length: usize, direction: CycleDirection) -> Option<usize> {
        if length == 0 {
            return None;
        }
        let Some(idx) = current_index else {
            return Some(match direction {
                CycleDirection::Forward => 0,
                CycleDirection::Backward => length - 1,
            });
        };
        if length == 1 {
            return None;
        }
        Some(match direction {
            CycleDirection::Forward => (idx + 1) % length,
            CycleDirection::Backward => (idx + length - 1) % length,
        })
    }

    async fn _cycle_scoped_model(
        &self,
        direction: CycleDirection,
        options: ModelMutationOptions,
    ) -> pi_js::Result<Option<ModelCycleResult>> {
        let available_ids: IndexSet<(String, String)> = self
            .inner
            .model_runtime
            .get_available_snapshot()
            .into_iter()
            .map(|m| (m.provider, m.id))
            .collect();
        let scoped_models: Vec<ScopedModelEntry> = self
            .scoped_models()
            .into_iter()
            .filter(|s| available_ids.contains(&(s.model.provider.clone(), s.model.id.clone())))
            .collect();

        let current_model = self.model();
        let current_any = current_model.as_ref().map(|m| AnyModel::Chat(m.clone()));
        let current_index = scoped_models
            .iter()
            .position(|sm| models_are_equal(Some(&AnyModel::Chat(sm.model.clone())), current_any.as_ref()));
        let Some(next_index) = Self::get_next_cycle_index(current_index, scoped_models.len(), direction) else {
            return Ok(None);
        };
        let next = &scoped_models[next_index];
        let thinking_level = self._get_thinking_level_for_model_switch(Some(&next.model), next.thinking_level);

        {
            self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).has_model = true;
        }
        self.inner.agent.set_model(next.model.clone());
        let _ = self
            .inner
            .session_manager
            .append_model_change(&next.model.provider, &next.model.id);
        if options.persist == Some(true) {
            self.inner
                .settings_manager
                .set_default_model_and_provider(&next.model.provider, &next.model.id);
            self._add_persisted_default_to_non_empty_scope(&next.model);
        }

        self.set_thinking_level(thinking_level, None);
        self._emit_model_select(&next.model, current_model.as_ref(), ModelSelectSource::Cycle)
            .await;

        Ok(Some(ModelCycleResult {
            model: next.model.clone(),
            thinking_level: self.thinking_level(),
            is_scoped: true,
        }))
    }

    async fn _cycle_available_model(
        &self,
        direction: CycleDirection,
        options: ModelMutationOptions,
    ) -> pi_js::Result<Option<ModelCycleResult>> {
        let available_models = self.inner.model_runtime.get_available_snapshot();

        let current_model = self.model();
        let current_any = current_model.as_ref().map(|m| AnyModel::Chat(m.clone()));
        let current_index = available_models
            .iter()
            .position(|m| models_are_equal(Some(&AnyModel::Chat(m.clone())), current_any.as_ref()));
        let Some(next_index) = Self::get_next_cycle_index(current_index, available_models.len(), direction) else {
            return Ok(None);
        };
        let next_model = available_models[next_index].clone();

        let thinking_level = self._get_thinking_level_for_model_switch(Some(&next_model), None);
        {
            self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).has_model = true;
        }
        self.inner.agent.set_model(next_model.clone());
        let _ = self
            .inner
            .session_manager
            .append_model_change(&next_model.provider, &next_model.id);
        if options.persist == Some(true) {
            self.inner
                .settings_manager
                .set_default_model_and_provider(&next_model.provider, &next_model.id);
            self._add_persisted_default_to_non_empty_scope(&next_model);
        }

        self.set_thinking_level(thinking_level, None);
        self._emit_model_select(&next_model, current_model.as_ref(), ModelSelectSource::Cycle)
            .await;

        Ok(Some(ModelCycleResult {
            model: next_model,
            thinking_level: self.thinking_level(),
            is_scoped: false,
        }))
    }

    // =========================================================================
    // Thinking Level Management
    // =========================================================================

    /// Set thinking level. Clamps to model capabilities and persists changes.
    pub fn set_thinking_level(&self, level: ModelThinkingLevel, options: Option<ModelMutationOptions>) {
        let available_levels = self.get_available_thinking_levels();
        let effective_level = if available_levels.contains(&level) {
            level
        } else {
            self._clamp_thinking_level(level, &available_levels)
        };

        let (previous_level, is_changing, runner) = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let prev = guard.thinking_level;
            let changing = effective_level != prev;
            guard.thinking_level = effective_level;
            (prev, changing, guard.extension_runner.clone())
        };
        self.inner
            .agent
            .set_thinking_level(to_agent_thinking_level(effective_level));

        if options.is_some_and(|o| o.persist == Some(true)) {
            self.inner.settings_manager.set_default_thinking_level(level);
        }

        if is_changing {
            let _ = self
                .inner
                .session_manager
                .append_thinking_level_change(effective_level.as_str());
            self._emit(AgentSessionEvent::ThinkingLevelChanged { level: effective_level });
            if let Some(runner) = runner {
                let ev = ThinkingLevelSelectEvent {
                    level: to_agent_thinking_level(effective_level),
                    previous_level: to_agent_thinking_level(previous_level),
                };
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(async move {
                        let _ = runner.emit(ev).await;
                    });
                }
            }
        }
    }

    /// Cycle to next thinking level. Returns `None` if model doesn't support thinking.
    pub fn cycle_thinking_level(&self, options: Option<ModelMutationOptions>) -> Option<ModelThinkingLevel> {
        if !self.supports_thinking() {
            return None;
        }
        let levels = self.get_available_thinking_levels();
        if levels.is_empty() {
            return None;
        }
        let current = self.thinking_level();
        let current_idx = levels.iter().position(|&l| l == current).unwrap_or(0);
        let next_idx = (current_idx + 1) % levels.len();
        let next_level = levels[next_idx];
        self.set_thinking_level(next_level, options);
        Some(next_level)
    }

    /// Get available thinking levels for current model.
    pub fn get_available_thinking_levels(&self) -> Vec<ModelThinkingLevel> {
        match self.model() {
            None => crate::core::defaults::THINKING_LEVEL_OPTIONS.to_vec(),
            Some(ref model) => get_supported_thinking_levels(model),
        }
    }

    /// Check if current model supports thinking/reasoning.
    pub fn supports_thinking(&self) -> bool {
        self.model().is_some_and(|m| m.reasoning)
    }

    fn _get_thinking_level_for_model_switch(
        &self,
        target_model: Option<&Model>,
        explicit_level: Option<ModelThinkingLevel>,
    ) -> ModelThinkingLevel {
        if let Some(level) = explicit_level {
            return level;
        }
        if let Some(target) = target_model
            && let Some(per_model) = self
                .inner
                .settings_manager
                .get_model_thinking_level(&target.provider, &target.id)
        {
            return per_model;
        }
        self.inner
            .settings_manager
            .get_default_thinking_level()
            .unwrap_or_else(|| {
                let current = self.thinking_level();
                let _ = DEFAULT_THINKING_LEVEL;
                current
            })
    }

    pub(super) fn _clamp_thinking_level(
        &self,
        level: ModelThinkingLevel,
        _available_levels: &[ModelThinkingLevel],
    ) -> ModelThinkingLevel {
        match self.model() {
            Some(ref model) => clamp_thinking_level(model, level),
            None => ModelThinkingLevel::Off,
        }
    }

    // =========================================================================
    // Queue Mode Management
    // =========================================================================

    pub(crate) fn sync_queue_modes_from_settings(&self) {
        let to_queue_mode = |m: QueueModeSetting| match m {
            QueueModeSetting::All => QueueMode::All,
            QueueModeSetting::OneAtATime => QueueMode::OneAtATime,
        };
        self.inner
            .agent
            .set_steering_mode(to_queue_mode(self.inner.settings_manager.get_steering_mode()));
        self.inner
            .agent
            .set_follow_up_mode(to_queue_mode(self.inner.settings_manager.get_follow_up_mode()));
    }

    /// Set steering message mode. Saves to settings.
    pub fn set_steering_mode(&self, mode: QueueMode) {
        self.inner.agent.set_steering_mode(mode);
        let setting = match mode {
            QueueMode::All => QueueModeSetting::All,
            QueueMode::OneAtATime => QueueModeSetting::OneAtATime,
        };
        self.inner.settings_manager.set_steering_mode(setting);
    }

    /// Set follow-up message mode. Saves to settings.
    pub fn set_follow_up_mode(&self, mode: QueueMode) {
        self.inner.agent.set_follow_up_mode(mode);
        let setting = match mode {
            QueueMode::All => QueueModeSetting::All,
            QueueMode::OneAtATime => QueueModeSetting::OneAtATime,
        };
        self.inner.settings_manager.set_follow_up_mode(setting);
    }
}
