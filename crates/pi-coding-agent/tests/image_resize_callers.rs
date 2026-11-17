//! Port of packages/coding-agent/test/image-resize-callers.test.ts#L1-45, #L56-85 (unit coding-agent-tools-file-edit)

// @generated-mods begin (scaffold-owned, do not edit)
#[path = "image_resize_callers/part_2.rs"]
mod part_2;
// @generated-mods end

use std::sync::{Arc, Mutex};

use pi_ai::types::{Model, UserContentBlock};
use pi_coding_agent::core::extensions::types::{
    BuildSystemPromptOptions, CancelledResult, CompactOptions, ContextUsage, CustomMessageContent, CustomMessageDraft,
    ExecuteToolOptions, ExtensionContext, ExtensionContextBackend, ExtensionMode, ExtensionToolContext,
    ExtensionUIContext, ForkOptions, ModelRegistry, NavigateTreeOptions, NewSessionOptions, ReadonlySessionManager,
    ScopedModel, SendMessageOptions, SendUserMessageOptions, SwitchSessionOptions,
};
use pi_coding_agent::core::tools::read::{
    ResizeImageOptions, create_read_tool, create_read_tool_definition, testing as read_testing,
};
use serde_json::json;
use serial_test::serial;
use tempfile::TempDir;

const TINY_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";

#[tokio::test]
#[serial]
async fn read_tool_returns_text_only_when_auto_resize_returns_none() {
    let _guard = read_testing::override_resize_image(Arc::new(|_img, _opts| Box::pin(async { Ok(None) })));

    let dir = TempDir::new().unwrap();
    let image_path = dir.path().join("test.png");
    std::fs::write(&image_path, pi_js::b64::decode(TINY_PNG_BASE64)).unwrap();

    let cwd = dir.path().to_string_lossy().into_owned();
    let tool = create_read_tool(&cwd, None);
    let result = (tool.execute)(
        "test-read-image".into(),
        json!({ "path": image_path.to_string_lossy() }),
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.content.len(), 1);
    match &result.content[0] {
        UserContentBlock::Text(t) => assert!(t.text.contains("Image omitted"), "{}", t.text),
        other => panic!("expected text block, got {other:?}"),
    }
}

struct ModelOverrideBackend {
    cwd: String,
    model: Option<Model>,
}

struct EmptyUi;

#[async_trait::async_trait]
impl ExtensionUIContext for EmptyUi {
    async fn select(
        &self,
        _: &str,
        _: Vec<String>,
        _: Option<pi_coding_agent::core::extensions::types::ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        Ok(None)
    }
    async fn confirm(
        &self,
        _: &str,
        _: &str,
        _: Option<pi_coding_agent::core::extensions::types::ExtensionUIDialogOptions>,
    ) -> pi_js::Result<bool> {
        Ok(false)
    }
    async fn input(
        &self,
        _: &str,
        _: Option<&str>,
        _: Option<pi_coding_agent::core::extensions::types::ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        Ok(None)
    }
    fn notify(&self, _: &str, _: Option<pi_coding_agent::core::extensions::types::NotifyType>) {}
    fn on_terminal_input(
        &self,
        _: pi_coding_agent::core::extensions::types::TerminalInputHandler,
    ) -> pi_js::Unsubscribe {
        pi_js::Unsubscribe::noop()
    }
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_working_message(&self, _: Option<&str>) {}
    fn set_working_visible(&self, _: bool) {}
    fn set_working_indicator(&self, _: Option<pi_coding_agent::core::extensions::types::WorkingIndicatorOptions>) {}
    fn set_hidden_thinking_label(&self, _: Option<&str>) {}
    fn set_widget_lines(
        &self,
        _: &str,
        _: Option<Vec<String>>,
        _: Option<pi_coding_agent::core::extensions::types::ExtensionWidgetOptions>,
    ) {
    }
    fn set_widget_factory(
        &self,
        _: &str,
        _: Option<pi_coding_agent::core::extensions::types::WidgetFactory>,
        _: Option<pi_coding_agent::core::extensions::types::ExtensionWidgetOptions>,
    ) {
    }
    fn set_footer(&self, _: Option<pi_coding_agent::core::extensions::types::FooterFactory>) {}
    fn set_header(&self, _: Option<pi_coding_agent::core::extensions::types::HeaderFactory>) {}
    fn set_title(&self, _: &str) {}
    async fn custom(
        &self,
        _: pi_coding_agent::core::extensions::types::CustomUiFactory,
        _: Option<pi_coding_agent::core::extensions::types::ExtensionCustomOptions>,
    ) -> pi_js::Result<serde_json::Value> {
        Ok(serde_json::Value::Null)
    }
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    async fn editor(&self, _: &str, _: Option<&str>) -> pi_js::Result<Option<String>> {
        Ok(None)
    }
    fn add_autocomplete_provider(&self, _: pi_coding_agent::core::extensions::types::AutocompleteProviderFactory) {}
    fn set_editor_component(&self, _: Option<pi_coding_agent::core::extensions::types::EditorFactory>) {}
    fn get_editor_component(&self) -> Option<pi_coding_agent::core::extensions::types::EditorFactory> {
        None
    }
    fn theme(&self) -> pi_coding_agent::core::extensions::types::Theme {
        Default::default()
    }
    fn get_all_themes(&self) -> Vec<pi_coding_agent::core::extensions::types::ThemeInfo> {
        Vec::new()
    }
    fn get_theme(&self, _: &str) -> Option<pi_coding_agent::core::extensions::types::Theme> {
        None
    }
    fn set_theme_by_name(&self, _: &str) -> pi_coding_agent::core::extensions::types::SetThemeResult {
        pi_coding_agent::core::extensions::types::SetThemeResult {
            success: false,
            error: None,
        }
    }
    fn set_theme(
        &self,
        _: pi_coding_agent::core::extensions::types::Theme,
    ) -> pi_coding_agent::core::extensions::types::SetThemeResult {
        pi_coding_agent::core::extensions::types::SetThemeResult {
            success: false,
            error: None,
        }
    }
    fn get_tools_expanded(&self) -> bool {
        false
    }
    fn set_tools_expanded(&self, _: bool) {}
}

impl ExtensionContextBackend for ModelOverrideBackend {
    fn assert_active(&self) -> pi_js::Result<()> {
        Ok(())
    }
    fn ui(&self) -> Arc<dyn ExtensionUIContext> {
        Arc::new(EmptyUi)
    }
    fn mode(&self) -> ExtensionMode {
        ExtensionMode::Print
    }
    fn has_ui(&self) -> bool {
        false
    }
    fn cwd(&self) -> String {
        self.cwd.clone()
    }
    fn session_manager(&self) -> ReadonlySessionManager {
        Default::default()
    }
    fn model_registry(&self) -> ModelRegistry {
        Default::default()
    }
    fn model(&self) -> Option<Model> {
        self.model.clone()
    }
    fn scoped_models(&self) -> Vec<ScopedModel> {
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
    fn get_context_usage(&self) -> Option<ContextUsage> {
        None
    }
    fn compact(&self, _: Option<CompactOptions>) {}
    fn get_system_prompt(&self) -> String {
        String::new()
    }
    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions {
        Default::default()
    }
    fn wait_for_idle(&self) -> pi_js::BoxFuture<pi_js::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn new_session(&self, _: Option<NewSessionOptions>) -> pi_js::BoxFuture<pi_js::Result<CancelledResult>> {
        Box::pin(async { Ok(CancelledResult { cancelled: false }) })
    }
    fn fork(&self, _: &str, _: Option<ForkOptions>) -> pi_js::BoxFuture<pi_js::Result<CancelledResult>> {
        Box::pin(async { Ok(CancelledResult { cancelled: false }) })
    }
    fn navigate_tree(
        &self,
        _: &str,
        _: Option<NavigateTreeOptions>,
    ) -> pi_js::BoxFuture<pi_js::Result<CancelledResult>> {
        Box::pin(async { Ok(CancelledResult { cancelled: false }) })
    }
    fn switch_session(
        &self,
        _: &str,
        _: Option<SwitchSessionOptions>,
    ) -> pi_js::BoxFuture<pi_js::Result<CancelledResult>> {
        Box::pin(async { Ok(CancelledResult { cancelled: false }) })
    }
    fn reload(&self) -> pi_js::BoxFuture<pi_js::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn callable_tools(&self) -> Vec<pi_agent_core::types::AgentTool> {
        Vec::new()
    }
    fn execute_tool(
        &self,
        _: &str,
        _: &str,
        _: serde_json::Value,
        _: ExecuteToolOptions,
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
            }
        })
    }
    fn send_message(&self, _: CustomMessageDraft, _: Option<SendMessageOptions>) {}
    fn send_user_message(&self, _: CustomMessageContent, _: Option<SendUserMessageOptions>) {}
}

#[tokio::test]
#[serial]
async fn passes_current_model_resize_profile_to_read_tool() {
    let seen = Arc::new(Mutex::new(None::<(String, ResizeImageOptions)>));
    let seen_cb = Arc::clone(&seen);
    let _guard = read_testing::override_resize_image(Arc::new(move |img, opts| {
        *seen_cb.lock().unwrap() = Some((img.mime_type, opts));
        Box::pin(async { Ok(None) })
    }));

    let dir = TempDir::new().unwrap();
    let image_path = dir.path().join("test.png");
    std::fs::write(&image_path, pi_js::b64::decode(TINY_PNG_BASE64)).unwrap();

    let model: Model = serde_json::from_value(json!({
        "id": "vision-model",
        "name": "Vision model",
        "api": "openai-responses",
        "provider": "openai",
        "baseUrl": "https://example.com",
        "reasoning": false,
        "input": ["text", "image"],
        "inputLimits": {
            "images": {
                "resize": {
                    "maxWidth": 1234,
                    "maxHeight": 1000,
                    "maxBytes": 500000,
                    "jpegQuality": 70
                }
            }
        },
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 1000,
        "maxTokens": 100
    }))
    .unwrap();

    let cwd = dir.path().to_string_lossy().into_owned();
    let ctx = ExtensionToolContext {
        base: ExtensionContext::from_backend(Arc::new(ModelOverrideBackend {
            cwd: cwd.clone(),
            model: Some(model),
        })),
        tool_call_id: "test-read-model-profile".to_string(),
        default_signal: None,
    };

    let def = create_read_tool_definition(&cwd, None);
    let _ = (def.execute)(
        "test-read-model-profile".into(),
        json!({ "path": image_path.to_string_lossy() }),
        None,
        None,
        ctx,
    )
    .await
    .unwrap();

    let captured = seen.lock().unwrap().clone().unwrap();
    assert_eq!(captured.0, "image/png");
    assert_eq!(
        captured.1,
        ResizeImageOptions {
            max_width: Some(1234),
            max_height: Some(1000),
            max_bytes: Some(500_000),
            jpeg_quality: Some(70),
        }
    );
}
