//! Port of packages/coding-agent/src/core/tools/tool-definition-wrapper.ts

use std::sync::Arc;

use pi_agent_core::types::{AgentTool, AgentToolResult};
use pi_js::abort::AbortSignal;
use serde_json::Value;

use crate::core::extensions::types::{ExtensionToolContext, ToolDefinition};

/// Creates the context for one tool call.
pub type ToolContextFactory = Arc<dyn Fn(&str, Option<AbortSignal>) -> ExtensionToolContext + Send + Sync>;

/// Wrap a ToolDefinition into an AgentTool for the core runtime.
pub fn wrap_tool_definition(definition: ToolDefinition, ctx_factory: Option<ToolContextFactory>) -> AgentTool {
    let def = definition.clone();
    let name_for_err = def.name.clone();
    AgentTool {
        name: definition.name,
        label: definition.label,
        description: definition.description,
        parameters: definition.parameters.into(),
        output_schema: definition.output_schema.map(|s| s.to_value()),
        constrained_sampling: match definition.constrained_sampling {
            Some(crate::core::extensions::types::ConstrainedSamplingOption::Config(cfg)) => Some(cfg),
            _ => None,
        },
        prepare_arguments: definition.prepare_arguments,
        execution_mode: definition.execution_mode,
        replay: None,
        execute: Arc::new(move |tool_call_id, params, signal, on_update| {
            let execute_fn = Arc::clone(&def.execute);
            let ctx_factory = ctx_factory.clone();
            let _name_for_err = name_for_err.clone();
            Box::pin(async move {
                let ctx = match ctx_factory {
                    Some(f) => f(&tool_call_id, signal.clone()),
                    None => standalone_tool_context(tool_call_id.clone(), signal.clone()),
                };
                let params_val: Value = params;
                execute_fn(tool_call_id, params_val, signal, on_update, ctx).await
            })
        }),
    }
}

/// Wrap multiple ToolDefinitions into AgentTools for the core runtime.
pub fn wrap_tool_definitions(
    definitions: Vec<ToolDefinition>,
    ctx_factory: Option<ToolContextFactory>,
) -> Vec<AgentTool> {
    definitions
        .into_iter()
        .map(|def| wrap_tool_definition(def, ctx_factory.clone()))
        .collect()
}

pub(crate) struct BuiltinToolDefSpec {
    pub name: &'static str,
    pub label: &'static str,
    pub description: String,
    pub prompt_snippet: Option<String>,
    pub prompt_guidelines: Option<Vec<String>>,
    pub parameters: pi_js::vendor::typebox::Schema,
    pub constrained_sampling: Option<crate::core::extensions::types::ConstrainedSamplingOption>,
    pub render_shell: Option<crate::core::extensions::types::RenderShell>,
    pub prepare_arguments: Option<pi_agent_core::types::AgentToolPrepareArgumentsFn>,
    pub output_schema: Option<pi_js::vendor::typebox::Schema>,
    pub execute: crate::core::extensions::types::ToolExecuteFn,
    pub render_call: Option<crate::core::extensions::types::RenderCallFn>,
    pub render_result: Option<crate::core::extensions::types::RenderResultFn>,
}

pub(crate) fn build_builtin_tool_definition(spec: BuiltinToolDefSpec) -> ToolDefinition {
    ToolDefinition {
        name: spec.name.to_string(),
        label: spec.label.to_string(),
        description: spec.description,
        prompt_snippet: spec.prompt_snippet,
        prompt_guidelines: spec.prompt_guidelines,
        parameters: spec.parameters,
        constrained_sampling: spec.constrained_sampling,
        render_shell: spec.render_shell,
        prepare_arguments: spec.prepare_arguments,
        output_schema: spec.output_schema,
        exposure: None,
        namespace: None,
        annotations: None,
        default_active: None,
        prepare_loadout: None,
        execution_mode: None,
        execute: spec.execute,
        render_call: spec.render_call,
        render_result: spec.render_result,
    }
}

pub(crate) fn strict_prefer_sampling() -> Option<crate::core::extensions::types::ConstrainedSamplingOption> {
    Some(crate::core::extensions::types::ConstrainedSamplingOption::Config(
        pi_ai::types::ConstrainedSamplingConfig::JsonSchema {
            strict: pi_ai::types::JsonSchemaStrict::Prefer,
        },
    ))
}

pub(crate) fn resolve_effective_cwd(
    ctx: &crate::core::extensions::types::ExtensionToolContext,
    fallback_cwd: &str,
) -> String {
    ctx.cwd()
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| fallback_cwd.to_string())
}

struct NoopStandaloneUi;

#[async_trait::async_trait]
impl crate::core::extensions::types::ExtensionUIContext for NoopStandaloneUi {
    async fn select(
        &self,
        _title: &str,
        _options: Vec<String>,
        _opts: Option<crate::core::extensions::types::ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        Ok(None)
    }

    async fn confirm(
        &self,
        _title: &str,
        _message: &str,
        _opts: Option<crate::core::extensions::types::ExtensionUIDialogOptions>,
    ) -> pi_js::Result<bool> {
        Ok(false)
    }

    async fn input(
        &self,
        _title: &str,
        _placeholder: Option<&str>,
        _opts: Option<crate::core::extensions::types::ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        Ok(None)
    }

    fn notify(&self, _message: &str, _notify_type: Option<crate::core::extensions::types::NotifyType>) {}

    fn on_terminal_input(&self, _handler: crate::core::extensions::types::TerminalInputHandler) -> pi_js::Unsubscribe {
        pi_js::Unsubscribe::noop()
    }

    fn set_status(&self, _key: &str, _text: Option<&str>) {}

    fn set_working_message(&self, _message: Option<&str>) {}

    fn set_working_visible(&self, _visible: bool) {}

    fn set_working_indicator(&self, _options: Option<crate::core::extensions::types::WorkingIndicatorOptions>) {}

    fn set_hidden_thinking_label(&self, _label: Option<&str>) {}

    fn set_widget_lines(
        &self,
        _key: &str,
        _content: Option<Vec<String>>,
        _options: Option<crate::core::extensions::types::ExtensionWidgetOptions>,
    ) {
    }

    fn set_widget_factory(
        &self,
        _key: &str,
        _factory: Option<crate::core::extensions::types::WidgetFactory>,
        _options: Option<crate::core::extensions::types::ExtensionWidgetOptions>,
    ) {
    }

    fn set_footer(&self, _factory: Option<crate::core::extensions::types::FooterFactory>) {}

    fn set_header(&self, _factory: Option<crate::core::extensions::types::HeaderFactory>) {}

    fn set_title(&self, _title: &str) {}

    async fn custom(
        &self,
        _factory: crate::core::extensions::types::CustomUiFactory,
        _options: Option<crate::core::extensions::types::ExtensionCustomOptions>,
    ) -> pi_js::Result<Value> {
        Ok(Value::Null)
    }

    fn paste_to_editor(&self, _text: &str) {}

    fn set_editor_text(&self, _text: &str) {}

    fn get_editor_text(&self) -> String {
        String::new()
    }

    async fn editor(&self, _title: &str, _prefill: Option<&str>) -> pi_js::Result<Option<String>> {
        Ok(None)
    }

    fn add_autocomplete_provider(&self, _factory: crate::core::extensions::types::AutocompleteProviderFactory) {}

    fn set_editor_component(&self, _factory: Option<crate::core::extensions::types::EditorFactory>) {}

    fn get_editor_component(&self) -> Option<crate::core::extensions::types::EditorFactory> {
        None
    }

    fn theme(&self) -> crate::core::extensions::types::Theme {
        Default::default()
    }

    fn get_all_themes(&self) -> Vec<crate::core::extensions::types::ThemeInfo> {
        Vec::new()
    }

    fn get_theme(&self, _name: &str) -> Option<crate::core::extensions::types::Theme> {
        None
    }

    fn set_theme_by_name(&self, _name: &str) -> crate::core::extensions::types::SetThemeResult {
        crate::core::extensions::types::SetThemeResult {
            success: false,
            error: Some("UI not available".to_string()),
        }
    }

    fn set_theme(
        &self,
        _theme: crate::core::extensions::types::Theme,
    ) -> crate::core::extensions::types::SetThemeResult {
        crate::core::extensions::types::SetThemeResult {
            success: false,
            error: Some("UI not available".to_string()),
        }
    }

    fn get_tools_expanded(&self) -> bool {
        false
    }

    fn set_tools_expanded(&self, _expanded: bool) {}
}

struct NoopStandaloneBackend;

impl crate::core::extensions::types::ExtensionContextBackend for NoopStandaloneBackend {
    fn assert_active(&self) -> pi_js::Result<()> {
        Ok(())
    }
    fn ui(&self) -> Arc<dyn crate::core::extensions::types::ExtensionUIContext> {
        Arc::new(NoopStandaloneUi)
    }
    fn mode(&self) -> crate::core::extensions::types::ExtensionMode {
        crate::core::extensions::types::ExtensionMode::Print
    }
    fn has_ui(&self) -> bool {
        false
    }
    fn has_session_environment(&self) -> bool {
        false
    }
    fn cwd(&self) -> String {
        String::new()
    }
    fn session_manager(&self) -> crate::core::session_manager::ReadonlySessionManager {
        Default::default()
    }
    fn model_registry(&self) -> crate::core::model_registry::ModelRegistry {
        Default::default()
    }
    fn model(&self) -> Option<pi_ai::types::Model> {
        None
    }
    fn scoped_models(&self) -> Vec<crate::core::model_resolver::ScopedModel> {
        Vec::new()
    }
    fn thinking_level(&self) -> Option<pi_agent_core::types::ThinkingLevel> {
        None
    }
    fn is_idle(&self) -> bool {
        true
    }
    fn is_project_trusted(&self) -> bool {
        false
    }
    fn signal(&self) -> Option<pi_js::abort::AbortSignal> {
        None
    }
    fn abort(&self) {}
    fn has_pending_messages(&self) -> bool {
        false
    }
    fn shutdown(&self) {}
    fn get_context_usage(&self) -> Option<crate::core::extensions::types::ContextUsage> {
        None
    }
    fn compact(&self, _options: Option<crate::core::extensions::types::CompactOptions>) {}
    fn get_system_prompt(&self) -> String {
        String::new()
    }
    fn get_system_prompt_options(&self) -> crate::core::extensions::types::BuildSystemPromptOptions {
        Default::default()
    }
    fn wait_for_idle(&self) -> pi_js::BoxFuture<pi_js::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn new_session(
        &self,
        _options: Option<crate::core::extensions::types::NewSessionOptions>,
    ) -> pi_js::BoxFuture<pi_js::Result<crate::core::extensions::types::CancelledResult>> {
        Box::pin(async { Ok(crate::core::extensions::types::CancelledResult { cancelled: false }) })
    }
    fn fork(
        &self,
        _entry_id: &str,
        _options: Option<crate::core::extensions::types::ForkOptions>,
    ) -> pi_js::BoxFuture<pi_js::Result<crate::core::extensions::types::CancelledResult>> {
        Box::pin(async { Ok(crate::core::extensions::types::CancelledResult { cancelled: false }) })
    }
    fn navigate_tree(
        &self,
        _target_id: &str,
        _options: Option<crate::core::extensions::types::NavigateTreeOptions>,
    ) -> pi_js::BoxFuture<pi_js::Result<crate::core::extensions::types::CancelledResult>> {
        Box::pin(async { Ok(crate::core::extensions::types::CancelledResult { cancelled: false }) })
    }
    fn switch_session(
        &self,
        _session_path: &str,
        _options: Option<crate::core::extensions::types::SwitchSessionOptions>,
    ) -> pi_js::BoxFuture<pi_js::Result<crate::core::extensions::types::CancelledResult>> {
        Box::pin(async { Ok(crate::core::extensions::types::CancelledResult { cancelled: false }) })
    }
    fn reload(&self) -> pi_js::BoxFuture<pi_js::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn callable_tools(&self) -> Vec<AgentTool> {
        Vec::new()
    }
    fn execute_tool(
        &self,
        _caller_id: &str,
        _name: &str,
        _args: Value,
        _options: crate::core::extensions::types::ExecuteToolOptions,
    ) -> pi_js::BoxFuture<pi_agent_core::types::AgentToolCallOutcome> {
        Box::pin(async {
            pi_agent_core::types::AgentToolCallOutcome {
                tool_call: pi_agent_core::types::AgentToolCall {
                    id: String::new(),
                    name: String::new(),
                    arguments: Default::default(),
                },
                result: Default::default(),
                is_error: true,
                duration_ms: None,
            }
        })
    }
    fn send_message(
        &self,
        _message: crate::core::extensions::types::CustomMessageDraft,
        _options: Option<crate::core::extensions::types::SendMessageOptions>,
    ) {
    }
    fn send_user_message(
        &self,
        _content: crate::core::messages::CustomMessageContent,
        _options: Option<crate::core::extensions::types::SendUserMessageOptions>,
    ) {
    }
}

pub(crate) fn standalone_tool_context(
    tool_call_id: String,
    default_signal: Option<pi_js::abort::AbortSignal>,
) -> crate::core::extensions::types::ExtensionToolContext {
    crate::core::extensions::types::ExtensionToolContext {
        base: crate::core::extensions::types::ExtensionContext::from_backend(Arc::new(NoopStandaloneBackend)),
        tool_call_id,
        default_signal,
    }
}

pub(crate) fn extract_posix_error_code(err: &pi_js::Error) -> Option<&'static str> {
    let msg = err.to_string();
    for code in [
        "ENOENT", "EACCES", "EPERM", "EISDIR", "ENOTDIR", "EEXIST", "EROFS", "EMFILE",
    ] {
        if msg.contains(code) {
            return Some(code);
        }
    }
    if msg.contains("os error 2)") || msg.contains("No such file or directory") {
        return Some("ENOENT");
    }
    if msg.contains("os error 13)") || msg.contains("Permission denied") {
        return Some("EACCES");
    }
    if msg.contains("os error 21)") || msg.contains("Is a directory") {
        return Some("EISDIR");
    }
    if msg.contains("os error 1)") || msg.contains("Operation not permitted") {
        return Some("EPERM");
    }
    None
}

pub(crate) fn format_edit_access_error(display_path: &str, err: &pi_js::Error) -> String {
    if let Some(code) = extract_posix_error_code(err) {
        format!("Could not edit file: {display_path}. Error code: {code}.")
    } else {
        let msg = err.to_string();
        let trimmed = msg.trim_end_matches('.');
        format!("Could not edit file: {display_path}. Error: {trimmed}.")
    }
}

/// Synthesize a minimal ToolDefinition from an AgentTool.
pub fn create_tool_definition_from_agent_tool(tool: AgentTool) -> ToolDefinition {
    let exec = Arc::clone(&tool.execute);
    ToolDefinition {
        name: tool.name,
        label: tool.label,
        description: tool.description,
        prompt_snippet: None,
        prompt_guidelines: None,
        parameters: tool.parameters.into(),
        constrained_sampling: tool
            .constrained_sampling
            .map(crate::core::extensions::types::ConstrainedSamplingOption::Config),
        render_shell: None,
        prepare_arguments: tool.prepare_arguments,
        output_schema: tool.output_schema.map(pi_js::vendor::typebox::t::unsafe_),
        exposure: None,
        namespace: None,
        annotations: None,
        default_active: None,
        prepare_loadout: None,
        execution_mode: tool.execution_mode,
        execute: Arc::new(move |tool_call_id, params, signal, on_update, _ctx| {
            let exec = Arc::clone(&exec);
            Box::pin(async move {
                let res: AgentToolResult = exec(tool_call_id, params, signal, on_update).await?;
                Ok(res)
            })
        }),
        render_call: None,
        render_result: None,
    }
}
