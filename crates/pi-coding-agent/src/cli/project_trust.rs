//! Port of packages/coding-agent/src/cli/project-trust.ts

use std::sync::Arc;

use async_trait::async_trait;
use pi_js::Unsubscribe;
use serde_json::Value;

use crate::cli::startup_ui::{show_startup_input, show_startup_selector};
use crate::core::extensions::types::{
    AutocompleteProviderFactory, CustomUiFactory, EditorFactory, ExtensionCustomOptions, ExtensionMode,
    ExtensionUIContext, ExtensionUIDialogOptions, ExtensionWidgetOptions, FooterFactory, HeaderFactory, NotifyType,
    ProjectTrustContext, SetThemeResult, TerminalInputHandler, Theme, ThemeInfo, WidgetFactory,
    WorkingIndicatorOptions,
};
use crate::core::project_trust::AppMode;
use crate::core::settings_manager::SettingsManager;
use crate::vendor::chalk;

#[derive(Clone)]
pub struct CreateProjectTrustContextOptions {
    pub cwd: String,
    pub mode: AppMode,
    pub settings_manager: SettingsManager,
    pub has_ui: bool,
}

struct CliProjectTrustUi {
    mode: AppMode,
    has_ui: bool,
    settings_manager: SettingsManager,
}

#[async_trait]
impl ExtensionUIContext for CliProjectTrustUi {
    async fn select(
        &self,
        title: &str,
        select_options: Vec<String>,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        if !self.has_ui || self.mode != AppMode::Interactive {
            return Ok(None);
        }
        let options: Vec<(String, String)> = select_options.iter().map(|o| (o.clone(), o.clone())).collect();
        show_startup_selector(&self.settings_manager, title, &options).await
    }

    async fn confirm(
        &self,
        title: &str,
        message: &str,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<bool> {
        if !self.has_ui || self.mode != AppMode::Interactive {
            return Ok(false);
        }
        let options = vec![("Yes".to_string(), true), ("No".to_string(), false)];
        Ok(
            show_startup_selector(&self.settings_manager, &format!("{title}\n{message}"), &options)
                .await?
                .unwrap_or(false),
        )
    }

    async fn input(
        &self,
        title: &str,
        placeholder: Option<&str>,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        if !self.has_ui || self.mode != AppMode::Interactive {
            return Ok(None);
        }
        show_startup_input(&self.settings_manager, title, placeholder).await
    }

    fn notify(&self, message: &str, notify_type: Option<NotifyType>) {
        if self.mode != AppMode::Interactive {
            let styled = match notify_type.unwrap_or(NotifyType::Info) {
                NotifyType::Error => chalk::red(message),
                NotifyType::Warning => chalk::yellow(message),
                NotifyType::Info => chalk::cyan(message),
            };
            pi_js::console::error(&styled);
        }
    }

    fn on_terminal_input(&self, _handler: TerminalInputHandler) -> Unsubscribe {
        Unsubscribe::noop()
    }

    fn set_status(&self, _key: &str, _text: Option<&str>) {}

    fn set_working_message(&self, _message: Option<&str>) {}

    fn set_working_visible(&self, _visible: bool) {}

    fn set_working_indicator(&self, _options: Option<WorkingIndicatorOptions>) {}

    fn set_hidden_thinking_label(&self, _label: Option<&str>) {}

    fn set_widget_lines(&self, _key: &str, _content: Option<Vec<String>>, _options: Option<ExtensionWidgetOptions>) {}

    fn set_widget_factory(
        &self,
        _key: &str,
        _factory: Option<WidgetFactory>,
        _options: Option<ExtensionWidgetOptions>,
    ) {
    }

    fn set_footer(&self, _factory: Option<FooterFactory>) {}

    fn set_header(&self, _factory: Option<HeaderFactory>) {}

    fn set_title(&self, _title: &str) {}

    async fn custom(
        &self,
        _factory: CustomUiFactory,
        _options: Option<ExtensionCustomOptions>,
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

    fn add_autocomplete_provider(&self, _factory: AutocompleteProviderFactory) {}

    fn set_editor_component(&self, _factory: Option<EditorFactory>) {}

    fn get_editor_component(&self) -> Option<EditorFactory> {
        None
    }

    fn theme(&self) -> Theme {
        Theme::default()
    }

    fn get_all_themes(&self) -> Vec<ThemeInfo> {
        Vec::new()
    }

    fn get_theme(&self, _name: &str) -> Option<Theme> {
        None
    }

    fn set_theme_by_name(&self, _name: &str) -> SetThemeResult {
        SetThemeResult {
            success: false,
            error: Some("Theme switching is not supported in project trust UI".to_string()),
        }
    }

    fn set_theme(&self, _theme: Theme) -> SetThemeResult {
        SetThemeResult {
            success: false,
            error: Some("Theme switching is not supported in project trust UI".to_string()),
        }
    }

    fn get_tools_expanded(&self) -> bool {
        false
    }

    fn set_tools_expanded(&self, _expanded: bool) {}
}

pub fn create_project_trust_context(options: CreateProjectTrustContextOptions) -> ProjectTrustContext {
    let ext_mode = match options.mode {
        AppMode::Interactive => ExtensionMode::Tui,
        AppMode::Print => ExtensionMode::Print,
        AppMode::Json => ExtensionMode::Json,
        AppMode::Rpc => ExtensionMode::Rpc,
    };
    ProjectTrustContext {
        cwd: options.cwd,
        mode: ext_mode,
        has_ui: options.has_ui,
        ui: Arc::new(CliProjectTrustUi {
            mode: options.mode,
            has_ui: options.has_ui,
            settings_manager: options.settings_manager,
        }),
    }
}

struct CliProjectTrustRuntimeUi {
    ui: Arc<dyn ExtensionUIContext>,
}

#[async_trait]
impl crate::core::project_trust::ProjectTrustUi for CliProjectTrustRuntimeUi {
    async fn select(&self, prompt: &str, options: &[String]) -> Option<String> {
        self.ui.select(prompt, options.to_vec(), None).await.ok().flatten()
    }
}

struct CliProjectTrustRuntimeContext {
    mode: AppMode,
    has_ui: bool,
    ui: Arc<CliProjectTrustRuntimeUi>,
}

impl crate::core::project_trust::ProjectTrustContext for CliProjectTrustRuntimeContext {
    fn has_ui(&self) -> bool {
        self.has_ui
    }

    fn ui(&self) -> &dyn crate::core::project_trust::ProjectTrustUi {
        self.ui.as_ref()
    }

    fn ui_arc(&self) -> Option<Arc<dyn crate::core::project_trust::ProjectTrustUi>> {
        Some(self.ui.clone())
    }

    fn mode(&self) -> AppMode {
        self.mode
    }
}

pub fn create_project_trust_runtime_context(
    options: CreateProjectTrustContextOptions,
) -> Arc<dyn crate::core::project_trust::ProjectTrustContext> {
    let inner = Arc::new(CliProjectTrustUi {
        mode: options.mode,
        has_ui: options.has_ui,
        settings_manager: options.settings_manager,
    });
    Arc::new(CliProjectTrustRuntimeContext {
        mode: options.mode,
        has_ui: options.has_ui,
        ui: Arc::new(CliProjectTrustRuntimeUi { ui: inner }),
    })
}

pub fn extension_project_trust_to_runtime_context(
    ctx: ProjectTrustContext,
) -> Arc<dyn crate::core::project_trust::ProjectTrustContext> {
    let mode = match ctx.mode {
        ExtensionMode::Tui => AppMode::Interactive,
        ExtensionMode::Print => AppMode::Print,
        ExtensionMode::Json => AppMode::Json,
        ExtensionMode::Rpc => AppMode::Rpc,
    };
    Arc::new(CliProjectTrustRuntimeContext {
        mode,
        has_ui: ctx.has_ui,
        ui: Arc::new(CliProjectTrustRuntimeUi { ui: ctx.ui }),
    })
}
