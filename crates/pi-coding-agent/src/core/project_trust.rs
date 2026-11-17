//! Port of packages/coding-agent/src/core/project-trust.ts

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::config::{APP_NAME, CONFIG_DIR_NAME};
use crate::core::extensions::runner::{emit_project_trust_event, noop_ui_context};
use crate::core::extensions::types::{
    ExtensionMode, ExtensionUIContext, ExtensionUIDialogOptions, LoadExtensionsResult, ProjectTrustEventDecision,
};
use crate::core::settings_manager::DefaultProjectTrust;
use crate::core::trust_manager::{
    ProjectTrustOption, ProjectTrustStore, get_project_trust_options, has_trust_requiring_project_resources,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppMode {
    Interactive,
    Print,
    Json,
    Rpc,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTrustEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub cwd: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectTrustHandlerDecision {
    Yes,
    No,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTrustEventResult {
    pub trusted: ProjectTrustHandlerDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remember: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectTrustExtensionError {
    pub extension_path: String,
    pub error: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectTrustEmitResult {
    pub result: Option<ProjectTrustEventResult>,
    pub errors: Vec<ProjectTrustExtensionError>,
}

#[async_trait::async_trait]
pub trait ProjectTrustUi: Send + Sync {
    async fn select(&self, prompt: &str, options: &[String]) -> Option<String>;
}

#[async_trait::async_trait]
pub trait ProjectTrustContext: Send + Sync {
    fn has_ui(&self) -> bool;
    fn ui(&self) -> &dyn ProjectTrustUi;
    fn ui_arc(&self) -> Option<Arc<dyn ProjectTrustUi>> {
        None
    }
    fn mode(&self) -> AppMode;
}

#[async_trait::async_trait]
pub trait ProjectTrustExtensionHost: Send + Sync {
    async fn emit_project_trust(
        &self,
        event: ProjectTrustEvent,
        ctx: &dyn ProjectTrustContext,
    ) -> ProjectTrustEmitResult;
}

impl From<AppMode> for ExtensionMode {
    fn from(mode: AppMode) -> Self {
        match mode {
            AppMode::Interactive => Self::Tui,
            AppMode::Print => Self::Print,
            AppMode::Json => Self::Json,
            AppMode::Rpc => Self::Rpc,
        }
    }
}

struct ProjectTrustUiAdapter {
    inner: Arc<dyn ProjectTrustUi>,
    fallback: Arc<dyn ExtensionUIContext>,
}

#[async_trait::async_trait]
impl ExtensionUIContext for ProjectTrustUiAdapter {
    async fn select(
        &self,
        title: &str,
        options: Vec<String>,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        Ok(self.inner.select(title, &options).await)
    }

    async fn confirm(&self, title: &str, message: &str, opts: Option<ExtensionUIDialogOptions>) -> pi_js::Result<bool> {
        self.fallback.confirm(title, message, opts).await
    }

    async fn input(
        &self,
        title: &str,
        placeholder: Option<&str>,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        self.fallback.input(title, placeholder, opts).await
    }

    fn notify(&self, message: &str, notify_type: Option<crate::core::extensions::types::NotifyType>) {
        self.fallback.notify(message, notify_type);
    }

    fn on_terminal_input(&self, handler: crate::core::extensions::types::TerminalInputHandler) -> pi_js::Unsubscribe {
        self.fallback.on_terminal_input(handler)
    }

    fn set_status(&self, key: &str, text: Option<&str>) {
        self.fallback.set_status(key, text);
    }

    fn set_working_message(&self, message: Option<&str>) {
        self.fallback.set_working_message(message);
    }

    fn set_working_visible(&self, visible: bool) {
        self.fallback.set_working_visible(visible);
    }

    fn set_working_indicator(&self, options: Option<crate::core::extensions::types::WorkingIndicatorOptions>) {
        self.fallback.set_working_indicator(options);
    }

    fn set_hidden_thinking_label(&self, label: Option<&str>) {
        self.fallback.set_hidden_thinking_label(label);
    }

    fn set_widget_lines(
        &self,
        key: &str,
        content: Option<Vec<String>>,
        options: Option<crate::core::extensions::types::ExtensionWidgetOptions>,
    ) {
        self.fallback.set_widget_lines(key, content, options);
    }

    fn set_widget_factory(
        &self,
        key: &str,
        factory: Option<crate::core::extensions::types::WidgetFactory>,
        options: Option<crate::core::extensions::types::ExtensionWidgetOptions>,
    ) {
        self.fallback.set_widget_factory(key, factory, options);
    }

    fn set_footer(&self, factory: Option<crate::core::extensions::types::FooterFactory>) {
        self.fallback.set_footer(factory);
    }

    fn set_header(&self, factory: Option<crate::core::extensions::types::HeaderFactory>) {
        self.fallback.set_header(factory);
    }

    fn set_title(&self, title: &str) {
        self.fallback.set_title(title);
    }

    async fn custom(
        &self,
        factory: crate::core::extensions::types::CustomUiFactory,
        options: Option<crate::core::extensions::types::ExtensionCustomOptions>,
    ) -> pi_js::Result<serde_json::Value> {
        self.fallback.custom(factory, options).await
    }

    fn paste_to_editor(&self, text: &str) {
        self.fallback.paste_to_editor(text);
    }

    fn set_editor_text(&self, text: &str) {
        self.fallback.set_editor_text(text);
    }

    fn get_editor_text(&self) -> String {
        self.fallback.get_editor_text()
    }

    async fn editor(&self, title: &str, prefill: Option<&str>) -> pi_js::Result<Option<String>> {
        self.fallback.editor(title, prefill).await
    }

    fn add_autocomplete_provider(&self, factory: crate::core::extensions::types::AutocompleteProviderFactory) {
        self.fallback.add_autocomplete_provider(factory);
    }

    fn set_editor_component(&self, factory: Option<crate::core::extensions::types::EditorFactory>) {
        self.fallback.set_editor_component(factory);
    }

    fn get_editor_component(&self) -> Option<crate::core::extensions::types::EditorFactory> {
        self.fallback.get_editor_component()
    }

    fn theme(&self) -> crate::core::extensions::types::Theme {
        self.fallback.theme()
    }

    fn get_all_themes(&self) -> Vec<crate::core::extensions::types::ThemeInfo> {
        self.fallback.get_all_themes()
    }

    fn get_theme(&self, name: &str) -> Option<crate::core::extensions::types::Theme> {
        self.fallback.get_theme(name)
    }

    fn set_theme_by_name(&self, name: &str) -> crate::core::extensions::types::SetThemeResult {
        self.fallback.set_theme_by_name(name)
    }

    fn set_theme(
        &self,
        theme: crate::core::extensions::types::Theme,
    ) -> crate::core::extensions::types::SetThemeResult {
        self.fallback.set_theme(theme)
    }

    fn get_tools_expanded(&self) -> bool {
        self.fallback.get_tools_expanded()
    }

    fn set_tools_expanded(&self, expanded: bool) {
        self.fallback.set_tools_expanded(expanded);
    }
}

/// [`ProjectTrustExtensionHost`] backed by [`crate::core::extensions::runner::emit_project_trust_event`].
#[derive(Clone)]
pub struct RunnerProjectTrustExtensionHost {
    extensions: LoadExtensionsResult,
}

impl RunnerProjectTrustExtensionHost {
    pub fn new(extensions: LoadExtensionsResult) -> Self {
        Self { extensions }
    }
}

#[async_trait::async_trait]
impl ProjectTrustExtensionHost for RunnerProjectTrustExtensionHost {
    async fn emit_project_trust(
        &self,
        event: ProjectTrustEvent,
        ctx: &dyn ProjectTrustContext,
    ) -> ProjectTrustEmitResult {
        let ui: Arc<dyn ExtensionUIContext> = match ctx.ui_arc() {
            Some(ui) => Arc::new(ProjectTrustUiAdapter {
                inner: ui,
                fallback: noop_ui_context(),
            }),
            None => noop_ui_context(),
        };
        let ext_ctx = crate::core::extensions::types::ProjectTrustContext {
            cwd: event.cwd.clone(),
            mode: ctx.mode().into(),
            has_ui: ctx.has_ui(),
            ui,
        };
        let ext_event = crate::core::extensions::types::ProjectTrustEvent { cwd: event.cwd };
        let output = emit_project_trust_event(&self.extensions, ext_event, ext_ctx).await;
        let result = output.result.and_then(|res| {
            let trusted = match res.trusted {
                ProjectTrustEventDecision::Yes => ProjectTrustHandlerDecision::Yes,
                ProjectTrustEventDecision::No => ProjectTrustHandlerDecision::No,
                ProjectTrustEventDecision::Undecided => return None,
            };
            Some(ProjectTrustEventResult {
                trusted,
                remember: res.remember,
            })
        });
        let errors = output
            .errors
            .into_iter()
            .map(|e| ProjectTrustExtensionError {
                extension_path: e.extension_path,
                error: e.error,
            })
            .collect();
        ProjectTrustEmitResult { result, errors }
    }
}

pub struct ResolveProjectTrustedOptions<'a> {
    pub cwd: &'a str,
    pub trust_store: &'a ProjectTrustStore,
    pub trust_override: Option<bool>,
    pub default_project_trust: Option<DefaultProjectTrust>,
    pub extension_host: Option<&'a dyn ProjectTrustExtensionHost>,
    pub project_trust_context: &'a dyn ProjectTrustContext,
    pub on_extension_error: Option<&'a (dyn Fn(&str) + Send + Sync)>,
}

fn format_project_trust_prompt(cwd: &str) -> String {
    let app_name = *APP_NAME;
    let config_dir = *CONFIG_DIR_NAME;
    format!(
        "Trust project folder?\n{cwd}\n\nThis allows {app_name} to load {config_dir} settings and resources, install missing project packages, and execute project extensions."
    )
}

async fn select_project_trust_option(cwd: &str, ctx: &dyn ProjectTrustContext) -> Option<ProjectTrustOption> {
    let options = get_project_trust_options(cwd, true);
    let labels: Vec<String> = options.iter().map(|opt| opt.label.clone()).collect();
    let selected = ctx.ui().select(&format_project_trust_prompt(cwd), &labels).await?;
    options.into_iter().find(|opt| opt.label == selected)
}

fn save_project_trust_prompt_result(trust_store: &ProjectTrustStore, result: &ProjectTrustOption) {
    if !result.updates.is_empty() {
        let _ = trust_store.set_many(&result.updates);
    }
}

pub async fn resolve_project_trusted(options: ResolveProjectTrustedOptions<'_>) -> bool {
    if let Some(ovr) = options.trust_override {
        return ovr;
    }
    if !has_trust_requiring_project_resources(options.cwd) {
        return true;
    }

    if let Some(host) = options.extension_host {
        let emitted = host
            .emit_project_trust(
                ProjectTrustEvent {
                    event_type: "project_trust".to_owned(),
                    cwd: options.cwd.to_owned(),
                },
                options.project_trust_context,
            )
            .await;
        for err in emitted.errors {
            if let Some(on_err) = options.on_extension_error {
                on_err(&format!(
                    "Extension \"{}\" project_trust error: {}",
                    err.extension_path, err.error
                ));
            }
        }
        if let Some(result) = emitted.result {
            let trusted = result.trusted == ProjectTrustHandlerDecision::Yes;
            if result.remember == Some(true) {
                let _ = options.trust_store.set(options.cwd, Some(trusted));
            }
            return trusted;
        }
    }

    if let Ok(Some(decision)) = options.trust_store.get(options.cwd) {
        return decision;
    }

    match options.default_project_trust.unwrap_or(DefaultProjectTrust::Ask) {
        DefaultProjectTrust::Always => return true,
        DefaultProjectTrust::Never => return false,
        DefaultProjectTrust::Ask => {}
    }

    if !options.project_trust_context.has_ui() {
        return false;
    }

    if let Some(selected) = select_project_trust_option(options.cwd, options.project_trust_context).await {
        save_project_trust_prompt_result(options.trust_store, &selected);
        return selected.trusted;
    }
    false
}
