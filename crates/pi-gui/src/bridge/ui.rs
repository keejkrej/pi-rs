//! Extension UI dialog & provider auth interaction handlers for `GuiBridge`.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::{AuthEvent, AuthInteraction, AuthPrompt, AuthPromptKind};
use pi_coding_agent::core::extensions::types::{
    AutocompleteProviderFactory, CustomUiFactory, EditorFactory, ExtensionCustomOptions, ExtensionUIContext,
    ExtensionUIDialogOptions, ExtensionWidgetOptions, FooterFactory, HeaderFactory, NotifyType, SetThemeResult,
    TerminalInputHandler, Theme as ExtTheme, ThemeInfo, WidgetFactory, WorkingIndicatorOptions,
};
use pi_js::Unsubscribe;
use serde_json::Value;
use tokio::sync::oneshot;

use super::{BridgeEmitter, BridgeEvent};
use crate::state::{ExtensionDialogView, ToastTone};

type PendingMap = Arc<Mutex<IndexMap<String, oneshot::Sender<Option<String>>>>>;

#[derive(Clone)]
pub(crate) struct DialogBroker {
    pub(crate) emitter: BridgeEmitter,
    pending: PendingMap,
    next_id: Arc<AtomicU64>,
}

impl DialogBroker {
    pub(crate) fn new(emitter: BridgeEmitter) -> Self {
        Self {
            emitter,
            pending: Arc::new(Mutex::new(IndexMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub(crate) async fn ask(
        &self,
        title: String,
        message: Option<String>,
        options: Vec<String>,
        allow_input: bool,
        timeout_ms: Option<u64>,
    ) -> Option<String> {
        let id = format!("dlg-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        if let Ok(mut guard) = self.pending.lock() {
            guard.insert(id.clone(), tx);
        }
        self.emitter
            .emit(BridgeEvent::ExtensionDialogRequested(ExtensionDialogView {
                request_id: id.clone(),
                title,
                message,
                options,
                allow_input,
            }));
        let res = match timeout_ms {
            Some(ms) => tokio::time::timeout(std::time::Duration::from_millis(ms), rx)
                .await
                .ok()
                .and_then(Result::ok)
                .flatten(),
            None => rx.await.ok().flatten(),
        };
        if let Ok(mut guard) = self.pending.lock() {
            guard.shift_remove(&id);
        }
        res
    }

    pub(crate) fn resolve(&self, request_id: &str, value: Option<String>) {
        if let Some(tx) = self.pending.lock().ok().and_then(|mut g| g.shift_remove(request_id)) {
            let _ = tx.send(value);
        }
    }
}

pub(crate) struct GuiExtensionUiContext {
    dialogs: DialogBroker,
    editor_text: Mutex<String>,
    tools_expanded: AtomicBool,
}

impl GuiExtensionUiContext {
    pub(crate) fn new(dialogs: DialogBroker) -> Self {
        Self {
            dialogs,
            editor_text: Mutex::new(String::new()),
            tools_expanded: AtomicBool::new(false),
        }
    }
}

impl ExtensionUIContext for GuiExtensionUiContext {
    fn select<'l0, 'l1, 'at>(
        &'l0 self,
        title: &'l1 str,
        options: Vec<String>,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> Pin<Box<dyn Future<Output = pi_js::Result<Option<String>>> + Send + 'at>>
    where
        'l0: 'at,
        'l1: 'at,
        Self: 'at,
    {
        let (title, timeout) = (title.to_string(), opts.and_then(|o| o.timeout));
        Box::pin(async move { Ok(self.dialogs.ask(title, None, options, false, timeout).await) })
    }

    fn confirm<'l0, 'l1, 'l2, 'at>(
        &'l0 self,
        title: &'l1 str,
        message: &'l2 str,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> Pin<Box<dyn Future<Output = pi_js::Result<bool>> + Send + 'at>>
    where
        'l0: 'at,
        'l1: 'at,
        'l2: 'at,
        Self: 'at,
    {
        let (title, msg, timeout) = (title.to_string(), message.to_string(), opts.and_then(|o| o.timeout));
        Box::pin(async move {
            let choices = vec!["Confirm".to_string(), "Cancel".to_string()];
            let ans = self.dialogs.ask(title, Some(msg), choices, false, timeout).await;
            Ok(ans.is_some_and(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "confirm" | "yes" | "true" | "ok" | "1"
                )
            }))
        })
    }

    fn input<'l0, 'l1, 'l2, 'at>(
        &'l0 self,
        title: &'l1 str,
        placeholder: Option<&'l2 str>,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> Pin<Box<dyn Future<Output = pi_js::Result<Option<String>>> + Send + 'at>>
    where
        'l0: 'at,
        'l1: 'at,
        'l2: 'at,
        Self: 'at,
    {
        let (title, msg, timeout) = (
            title.to_string(),
            placeholder.map(ToOwned::to_owned),
            opts.and_then(|o| o.timeout),
        );
        Box::pin(async move { Ok(self.dialogs.ask(title, msg, Vec::new(), true, timeout).await) })
    }

    fn notify(&self, message: &str, notify_type: Option<NotifyType>) {
        let tone = match notify_type {
            Some(NotifyType::Warning) => ToastTone::Warning,
            Some(NotifyType::Error) => ToastTone::Error,
            Some(NotifyType::Info) | None => ToastTone::Info,
        };
        self.dialogs.emitter.toast(message, tone);
    }

    fn on_terminal_input(&self, _handler: TerminalInputHandler) -> Unsubscribe {
        Unsubscribe::noop()
    }
    fn set_status(&self, _key: &str, _text: Option<&str>) {}
    fn set_working_message(&self, _message: Option<&str>) {}
    fn set_working_visible(&self, _visible: bool) {}
    fn set_working_indicator(&self, _options: Option<WorkingIndicatorOptions>) {}
    fn set_hidden_thinking_label(&self, _label: Option<&str>) {}
    fn set_widget_lines(&self, _key: &str, _content: Option<Vec<String>>, _opts: Option<ExtensionWidgetOptions>) {}
    fn set_widget_factory(&self, _key: &str, _factory: Option<WidgetFactory>, _opts: Option<ExtensionWidgetOptions>) {}
    fn set_footer(&self, _factory: Option<FooterFactory>) {}
    fn set_header(&self, _factory: Option<HeaderFactory>) {}
    fn set_title(&self, _title: &str) {}

    fn custom<'l0, 'at>(
        &'l0 self,
        _factory: CustomUiFactory,
        _options: Option<ExtensionCustomOptions>,
    ) -> Pin<Box<dyn Future<Output = pi_js::Result<Value>> + Send + 'at>>
    where
        'l0: 'at,
        Self: 'at,
    {
        Box::pin(async { Ok(Value::Null) })
    }

    fn paste_to_editor(&self, text: &str) {
        if let Ok(mut g) = self.editor_text.lock() {
            g.push_str(text);
        }
    }
    fn set_editor_text(&self, text: &str) {
        if let Ok(mut g) = self.editor_text.lock() {
            *g = text.to_string();
        }
    }
    fn get_editor_text(&self) -> String {
        self.editor_text.lock().map(|g| g.clone()).unwrap_or_default()
    }

    fn editor<'l0, 'l1, 'l2, 'at>(
        &'l0 self,
        title: &'l1 str,
        prefill: Option<&'l2 str>,
    ) -> Pin<Box<dyn Future<Output = pi_js::Result<Option<String>>> + Send + 'at>>
    where
        'l0: 'at,
        'l1: 'at,
        'l2: 'at,
        Self: 'at,
    {
        let (title, prefill) = (title.to_string(), prefill.map(ToOwned::to_owned));
        Box::pin(async move { Ok(self.dialogs.ask(title, prefill, Vec::new(), true, None).await) })
    }

    fn add_autocomplete_provider(&self, _factory: AutocompleteProviderFactory) {}
    fn set_editor_component(&self, _factory: Option<EditorFactory>) {}
    fn get_editor_component(&self) -> Option<EditorFactory> {
        None
    }
    fn theme(&self) -> ExtTheme {
        ExtTheme::default()
    }
    fn get_all_themes(&self) -> Vec<ThemeInfo> {
        Vec::new()
    }
    fn get_theme(&self, _name: &str) -> Option<ExtTheme> {
        None
    }
    fn set_theme_by_name(&self, _name: &str) -> SetThemeResult {
        SetThemeResult {
            success: false,
            error: Some("Theme controlled by Rune GUI".into()),
        }
    }
    fn set_theme(&self, _theme: ExtTheme) -> SetThemeResult {
        SetThemeResult {
            success: true,
            error: None,
        }
    }
    fn get_tools_expanded(&self) -> bool {
        self.tools_expanded.load(Ordering::Relaxed)
    }
    fn set_tools_expanded(&self, expanded: bool) {
        self.tools_expanded.store(expanded, Ordering::Relaxed);
    }
}

pub(crate) struct GuiAuthInteraction {
    pub(crate) provider: String,
    pub(crate) dialogs: DialogBroker,
}

impl AuthInteraction for GuiAuthInteraction {
    fn prompt<'l0, 'at>(
        &'l0 self,
        prompt: AuthPrompt,
    ) -> Pin<Box<dyn Future<Output = pi_js::Result<String>> + Send + 'at>>
    where
        'l0: 'at,
        Self: 'at,
    {
        let title = format!("Authenticate: {}", self.provider);
        Box::pin(async move {
            let (msg, opts, allow_input) = match prompt.kind {
                AuthPromptKind::Text { message, placeholder }
                | AuthPromptKind::Secret { message, placeholder }
                | AuthPromptKind::ManualCode { message, placeholder } => {
                    let full = placeholder.map_or(message.clone(), |p| format!("{message}\n({p})"));
                    (Some(full), Vec::new(), true)
                }
                AuthPromptKind::Select { message, options } => {
                    (Some(message), options.into_iter().map(|o| o.label).collect(), false)
                }
            };
            self.dialogs
                .ask(title, msg, opts, allow_input, None)
                .await
                .ok_or_else(|| pi_js::Error::msg("Authentication cancelled"))
        })
    }

    fn notify(&self, event: AuthEvent) {
        let msg = match event {
            AuthEvent::Info { message, .. } | AuthEvent::Progress { message } => message,
            AuthEvent::AuthUrl { url, instructions } => {
                instructions.map_or_else(|| format!("Open URL: {url}"), |i| format!("{i}: {url}"))
            }
            AuthEvent::DeviceCode {
                user_code,
                verification_uri,
                ..
            } => format!("Enter {user_code} at {verification_uri}"),
        };
        self.dialogs.emitter.toast(msg, ToastTone::Info);
    }
}
