//! Port of packages/coding-agent/src/modes/interactive/components/login-dialog.ts

use std::sync::{Arc, Mutex};

use pi_ai::{AuthInfoLink, OAuthDeviceCodeInfo};
use pi_js::BoxFuture;
use pi_js::abort::{AbortController, AbortSignal};
use pi_js::seam::{Guard, Slot};
use pi_tui::{
    Component, Container, Focusable, Input, Spacer, TUI, Text, TruncatedText, TuiMouseEvent, TuiMouseEventResult,
    get_keybindings, hyperlink,
};
use tokio::sync::oneshot;

pub use crate::utils::web_url::{display_url, web_url};

use super::super::theme::theme::{ThemeColor, theme};
pub use super::auth_url::AuthUrlComponent;
use super::dynamic_border::DynamicBorder;
use super::keybinding_hints::key_hint;

type OpenBrowserFn = dyn Fn(&str) + Send + Sync;
type CopyToClipboardFn = dyn Fn(&str) -> BoxFuture<pi_js::Result<()>> + Send + Sync;

thread_local! {
    static OPEN_BROWSER_OVERRIDE: Slot<OpenBrowserFn> = Slot::new();
    static COPY_TO_CLIPBOARD_OVERRIDE: Slot<CopyToClipboardFn> = Slot::new();
}

/// Test seams for mocking clipboard and browser launchers in offline tests (`PORTING.md §12.3`).
pub mod testing {
    use super::*;

    pub fn override_open_browser(f: impl Fn(&str) + Send + Sync + 'static) -> Guard {
        OPEN_BROWSER_OVERRIDE.with(|s| s.set(Arc::new(f)))
    }

    pub fn override_copy_to_clipboard(
        f: impl Fn(&str) -> BoxFuture<pi_js::Result<()>> + Send + Sync + 'static,
    ) -> Guard {
        COPY_TO_CLIPBOARD_OVERRIDE.with(|s| s.set(Arc::new(f)))
    }
}

fn open_browser_wire(target: &str) {
    if let Some(f) = OPEN_BROWSER_OVERRIDE.with(|s| s.get()) {
        f(target);
        return;
    }
    crate::utils::open_browser::open_browser(target);
}

pub(crate) fn current_copy_to_clipboard_override() -> Option<Arc<CopyToClipboardFn>> {
    COPY_TO_CLIPBOARD_OVERRIDE.with(|s| s.get())
}

pub(crate) async fn copy_to_clipboard_with_override(
    text: &str,
    override_fn: Option<Arc<CopyToClipboardFn>>,
) -> pi_js::Result<()> {
    if let Some(f) = override_fn {
        return f(text).await;
    }
    crate::utils::clipboard::copy_to_clipboard(text).await
}

/// `text` as a terminal hyperlink to `url` when that is a web URL, else plain `text`.
fn web_link(text: &str, url: &str) -> String {
    match web_url(url) {
        Some(target) => hyperlink(text, &target),
        None => text.to_string(),
    }
}

/// Device-code payload for `LoginDialogComponent::show_device_code`, extending `pi_ai::OAuthDeviceCodeInfo`
/// with optional browser/clipboard confirmation flags (`open_browser_on_confirm`, `copy_code_on_confirm`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeviceCodeInfo {
    pub user_code: String,
    pub verification_uri: String,
    pub open_browser_on_confirm: Option<bool>,
    pub copy_code_on_confirm: Option<bool>,
    pub interval_seconds: Option<f64>,
    pub expires_in_seconds: Option<f64>,
}

impl From<OAuthDeviceCodeInfo> for DeviceCodeInfo {
    fn from(info: OAuthDeviceCodeInfo) -> Self {
        Self {
            user_code: info.user_code,
            verification_uri: info.verification_uri,
            open_browser_on_confirm: None,
            copy_code_on_confirm: None,
            interval_seconds: info.interval_seconds,
            expires_in_seconds: info.expires_in_seconds,
        }
    }
}

impl From<&OAuthDeviceCodeInfo> for DeviceCodeInfo {
    fn from(info: &OAuthDeviceCodeInfo) -> Self {
        Self {
            user_code: info.user_code.clone(),
            verification_uri: info.verification_uri.clone(),
            open_browser_on_confirm: None,
            copy_code_on_confirm: None,
            interval_seconds: info.interval_seconds,
            expires_in_seconds: info.expires_in_seconds,
        }
    }
}

pub type LoginCompleteCallback = Arc<dyn Fn(bool, Option<&str>) + Send + Sync>;

#[derive(Clone)]
struct DeviceCodeAction {
    info: DeviceCodeInfo,
    open_browser_on_confirm: bool,
    copy_code_on_confirm: bool,
    hint: Container,
    hint_arc: Arc<dyn Component>,
}

struct LoginDialogState {
    focused: bool,
    auth_url: Option<AuthUrlComponent>,
    pending_input: Option<oneshot::Sender<pi_js::Result<String>>>,
    device_code_action: Option<DeviceCodeAction>,
    device_code_hint: Option<Arc<dyn Component>>,
}

struct LoginDialogInner {
    container: Container,
    content_container: Container,
    input: Input,
    input_component: Arc<dyn Component>,
    tui: Arc<dyn TUI + Send + Sync>,
    abort_controller: Arc<AbortController>,
    on_complete: LoginCompleteCallback,
    state: Mutex<LoginDialogState>,
}

/// Login dialog component - replaces editor during OAuth login flow.
#[derive(Clone)]
pub struct LoginDialogComponent {
    inner: Arc<LoginDialogInner>,
}

impl LoginDialogComponent {
    pub fn new(
        tui: Arc<dyn TUI + Send + Sync>,
        provider_id: &str,
        on_complete: LoginCompleteCallback,
        provider_name_override: Option<&str>,
        title_override: Option<&str>,
    ) -> Self {
        let provider_name = provider_name_override.filter(|s| !s.is_empty()).unwrap_or(provider_id);
        let default_title = format!("Login to {provider_name}");
        let title = title_override.unwrap_or(&default_title);

        let t = theme();
        let container = Container::new();
        container.add_child(Arc::new(DynamicBorder::default()));
        container.add_child(Arc::new(Text::new(
            Some(&t.fg(ThemeColor::Accent, &t.bold(title))),
            Some(1),
            Some(0),
            None,
        )));

        let content_container = Container::new();
        container.add_child(Arc::new(content_container.clone()));

        let input = Input::new(None);
        let input_component: Arc<dyn Component> = Arc::new(input.clone());

        container.add_child(Arc::new(DynamicBorder::default()));

        let comp = Self {
            inner: Arc::new(LoginDialogInner {
                container,
                content_container,
                input,
                input_component,
                tui,
                abort_controller: Arc::new(AbortController::new()),
                on_complete,
                state: Mutex::new(LoginDialogState {
                    focused: false,
                    auth_url: None,
                    pending_input: None,
                    device_code_action: None,
                    device_code_hint: None,
                }),
            }),
        };

        let weak_submit = Arc::downgrade(&comp.inner);
        comp.inner.input.set_on_submit(Some(Arc::new(move |_| {
            let Some(inner) = weak_submit.upgrade() else {
                return;
            };
            let dialog = Self { inner };
            let pending = {
                let mut state = dialog.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                state.pending_input.take()
            };
            if let Some(sender) = pending {
                let value = dialog.inner.input.get_value();
                dialog.replace_input_with_submitted_text(&value);
                let _ = sender.send(Ok(value));
            }
        })));

        let weak_escape = Arc::downgrade(&comp.inner);
        comp.inner.input.set_on_escape(Some(Arc::new(move || {
            if let Some(inner) = weak_escape.upgrade() {
                Self { inner }.cancel();
            }
        })));

        comp
    }

    pub fn focused(&self) -> bool {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).focused
    }

    pub fn set_focused(&self, value: bool) {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).focused = value;
        self.inner.input.set_focused(value);
    }

    pub fn signal(&self) -> AbortSignal {
        self.inner.abort_controller.signal()
    }

    fn replace_input_with_submitted_text(&self, value: &str) {
        let updated = self
            .inner
            .content_container
            .children()
            .into_iter()
            .map(|child| {
                if Arc::ptr_eq(&child, &self.inner.input_component) {
                    Arc::new(Text::new(Some(&format!("> {value}")), Some(0), Some(0), None)) as Arc<dyn Component>
                } else {
                    child
                }
            })
            .collect();
        self.inner.content_container.set_children(updated);
    }

    fn clear_device_code_action(&self) {
        let hint = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.device_code_action = None;
            state.device_code_hint.take()
        };
        if let Some(hint_arc) = hint {
            self.inner.content_container.remove_child(&hint_arc);
        }
    }

    fn cancel(&self) {
        self.clear_device_code_action();
        self.inner.abort_controller.abort(None);
        let pending = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.pending_input.take()
        };
        if let Some(sender) = pending {
            let _ = sender.send(Err(pi_js::Error::msg("Login cancelled")));
        }
        (self.inner.on_complete)(false, Some("Login cancelled"));
    }

    /// Called by onAuth callback - show URL and optional instructions.
    pub fn show_auth(&self, url: &str, instructions: Option<&str>) {
        self.clear_device_code_action();
        self.inner.content_container.clear();
        self.inner.content_container.add_child(Arc::new(Spacer::new(Some(1))));

        let target = web_url(url);
        let shown = target
            .clone()
            .unwrap_or_else(|| pi_tui::utils::replace_control_characters(url));
        let auth_url = AuthUrlComponent::new(Arc::clone(&self.inner.tui), &shown);
        {
            self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).auth_url = Some(auth_url.clone());
        }
        self.inner.content_container.add_child(Arc::new(auth_url));

        if let Some(instr) = instructions.filter(|s| !s.is_empty()) {
            self.inner.content_container.add_child(Arc::new(Spacer::new(Some(1))));
            self.inner.content_container.add_child(Arc::new(Text::new(
                Some(&theme().fg(ThemeColor::Warning, instr)),
                Some(1),
                Some(0),
                None,
            )));
        }

        if let Some(ref target_url) = target {
            open_browser_wire(target_url);
        }
        self.inner.tui.request_render(None);
    }

    /// Called by onDeviceCode callback - show URL and user code.
    pub fn show_device_code(&self, info: impl Into<DeviceCodeInfo>) {
        let info = info.into();
        let open_browser_on_confirm = info.open_browser_on_confirm.unwrap_or(true);
        let copy_code_on_confirm = info.copy_code_on_confirm.unwrap_or(true);

        {
            self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).auth_url = None;
        }
        self.clear_device_code_action();
        self.inner.content_container.clear();
        self.inner.content_container.add_child(Arc::new(Spacer::new(Some(1))));

        let t = theme();
        let shown_url = display_url(&info.verification_uri);
        self.inner.content_container.add_child(Arc::new(Text::new(
            Some(&t.fg(ThemeColor::Accent, &web_link(&shown_url, &shown_url))),
            Some(1),
            Some(0),
            None,
        )));

        let click_hint = if pi_js::env::platform() == "darwin" {
            "Cmd+click to open"
        } else {
            "Ctrl+click to open"
        };
        self.inner.content_container.add_child(Arc::new(Text::new(
            Some(&t.fg(ThemeColor::Dim, &web_link(click_hint, &info.verification_uri))),
            Some(1),
            Some(0),
            None,
        )));
        self.inner.content_container.add_child(Arc::new(Spacer::new(Some(1))));
        self.inner.content_container.add_child(Arc::new(Text::new(
            Some(&t.fg(ThemeColor::Warning, &format!("Enter code: {}", info.user_code))),
            Some(1),
            Some(0),
            None,
        )));

        if !self.signal().aborted() && (open_browser_on_confirm || copy_code_on_confirm) {
            let action_label = if open_browser_on_confirm {
                if copy_code_on_confirm {
                    "to open browser and copy code"
                } else {
                    "to open browser"
                }
            } else {
                "to copy code"
            };
            // Reuse this single row for progress/result text, including in narrow terminals.
            let hint = Container::new();
            hint.add_child(Arc::new(TruncatedText::new(
                &format!("({})", key_hint(&"tui.select.confirm".to_string(), action_label)),
                Some(1),
                Some(0),
            )));
            let hint_arc: Arc<dyn Component> = Arc::new(hint.clone());
            self.inner.content_container.add_child(Arc::clone(&hint_arc));
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.device_code_hint = Some(Arc::clone(&hint_arc));
            state.device_code_action = Some(DeviceCodeAction {
                info,
                open_browser_on_confirm,
                copy_code_on_confirm,
                hint,
                hint_arc,
            });
        }
        self.inner.tui.request_render(None);
    }

    fn set_device_code_status(&self, hint: &Container, hint_arc: &Arc<dyn Component>, message: &str) {
        if self.signal().aborted() || !self.focused() {
            return;
        }
        let still_mounted = self
            .inner
            .content_container
            .children()
            .iter()
            .any(|child| Arc::ptr_eq(child, hint_arc));
        if !still_mounted {
            return;
        }
        hint.set_children(vec![Arc::new(TruncatedText::new(
            &theme().fg(ThemeColor::Dim, message),
            Some(1),
            Some(0),
        ))]);
        self.inner.tui.request_render(None);
    }

    fn activate_device_code(&self, action: DeviceCodeAction) {
        let status_msg = if action.copy_code_on_confirm {
            "Copying code..."
        } else {
            "Browser open requested"
        };
        self.set_device_code_status(&action.hint, &action.hint_arc, status_msg);

        if action.copy_code_on_confirm {
            // Do not delay polling or browser opening while clipboard tools are running.
            let copy_override = COPY_TO_CLIPBOARD_OVERRIDE.with(|s| s.get());
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let dialog = self.clone();
                let user_code = action.info.user_code.clone();
                let hint = action.hint.clone();
                let hint_arc = Arc::clone(&action.hint_arc);
                handle.spawn(async move {
                    match copy_to_clipboard_with_override(&user_code, copy_override).await {
                        Ok(()) => dialog.set_device_code_status(&hint, &hint_arc, "Code copied to clipboard"),
                        Err(_) => dialog.set_device_code_status(&hint, &hint_arc, "Copy code manually"),
                    }
                });
            }
        }

        if action.open_browser_on_confirm
            && let Some(target) = web_url(&action.info.verification_uri)
        {
            open_browser_wire(&target);
        }
    }

    /// Show input for manual code/URL entry (for callback server providers).
    pub fn show_manual_input(&self, prompt: &str) -> BoxFuture<pi_js::Result<String>> {
        self.clear_device_code_action();
        self.inner.input.set_value("");
        self.inner.content_container.add_child(Arc::new(Spacer::new(Some(1))));
        self.inner.content_container.add_child(Arc::new(Text::new(
            Some(&theme().fg(ThemeColor::Dim, prompt)),
            Some(1),
            Some(0),
            None,
        )));
        self.inner
            .content_container
            .add_child(Arc::clone(&self.inner.input_component));
        self.inner.content_container.add_child(Arc::new(Text::new(
            Some(&format!(
                "({})",
                key_hint(&"tui.select.cancel".to_string(), "to cancel")
            )),
            Some(1),
            Some(0),
            None,
        )));
        self.inner.tui.request_render(None);

        let (tx, rx) = oneshot::channel();
        {
            self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).pending_input = Some(tx);
        }
        Box::pin(async move {
            match rx.await {
                Ok(res) => res,
                Err(_) => Err(pi_js::Error::msg("Login cancelled")),
            }
        })
    }

    /// Called by onPrompt callback - show prompt and wait for input.
    /// Note: Does NOT clear content, appends to existing (preserves URL from showAuth).
    pub fn show_prompt(&self, message: &str, placeholder: Option<&str>) -> BoxFuture<pi_js::Result<String>> {
        self.clear_device_code_action();
        let t = theme();
        self.inner.content_container.add_child(Arc::new(Spacer::new(Some(1))));
        self.inner.content_container.add_child(Arc::new(Text::new(
            Some(&t.fg(ThemeColor::Text, message)),
            Some(1),
            Some(0),
            None,
        )));
        if let Some(ph) = placeholder.filter(|s| !s.is_empty()) {
            self.inner.content_container.add_child(Arc::new(Text::new(
                Some(&t.fg(ThemeColor::Dim, &format!("e.g., {ph}"))),
                Some(1),
                Some(0),
                None,
            )));
        }
        self.inner
            .content_container
            .add_child(Arc::clone(&self.inner.input_component));
        self.inner.content_container.add_child(Arc::new(Text::new(
            Some(&format!(
                "({} {})",
                key_hint(&"tui.select.cancel".to_string(), "to cancel,"),
                key_hint(&"tui.select.confirm".to_string(), "to submit"),
            )),
            Some(1),
            Some(0),
            None,
        )));

        self.inner.input.set_value("");
        self.inner.tui.request_render(None);

        let (tx, rx) = oneshot::channel();
        {
            self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).pending_input = Some(tx);
        }
        Box::pin(async move {
            match rx.await {
                Ok(res) => res,
                Err(_) => Err(pi_js::Error::msg("Login cancelled")),
            }
        })
    }

    /// Show informational text before another login step.
    pub fn show_details(&self, lines: &[String]) {
        {
            self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).auth_url = None;
        }
        self.clear_device_code_action();
        self.inner.content_container.clear();
        self.inner.content_container.add_child(Arc::new(Spacer::new(Some(1))));
        for line in lines {
            self.inner
                .content_container
                .add_child(Arc::new(Text::new(Some(line), Some(1), Some(0), None)));
        }
        self.inner.tui.request_render(None);
    }

    /// Show provider-owned information and links without starting an auth callback flow.
    pub fn show_info(&self, message: &str, links: Option<&[AuthInfoLink]>, show_close_hint: Option<bool>) {
        let t = theme();
        self.inner.content_container.add_child(Arc::new(Spacer::new(Some(1))));
        self.inner.content_container.add_child(Arc::new(Text::new(
            Some(&t.fg(ThemeColor::Text, message)),
            Some(1),
            Some(0),
            None,
        )));
        for link in links.unwrap_or(&[]) {
            let url = display_url(&link.url);
            let text = match link.label.as_deref() {
                Some(label) if !label.is_empty() => format!("{label}: {url}"),
                _ => url.clone(),
            };
            self.inner.content_container.add_child(Arc::new(Text::new(
                Some(&t.fg(ThemeColor::Accent, &web_link(&text, &url))),
                Some(1),
                Some(0),
                None,
            )));
        }
        if show_close_hint.unwrap_or(false) {
            self.inner.content_container.add_child(Arc::new(Spacer::new(Some(1))));
            self.inner.content_container.add_child(Arc::new(Text::new(
                Some(&format!("({})", key_hint(&"tui.select.cancel".to_string(), "to close"))),
                Some(1),
                Some(0),
                None,
            )));
        }
        self.inner.tui.request_render(None);
    }

    /// Show waiting message (for polling flows like GitHub Copilot).
    pub fn show_waiting(&self, message: &str) {
        self.inner.content_container.add_child(Arc::new(Spacer::new(Some(1))));
        self.inner.content_container.add_child(Arc::new(Text::new(
            Some(&theme().fg(ThemeColor::Dim, message)),
            Some(1),
            Some(0),
            None,
        )));
        self.inner.content_container.add_child(Arc::new(Text::new(
            Some(&format!(
                "({})",
                key_hint(&"tui.select.cancel".to_string(), "to cancel")
            )),
            Some(1),
            Some(0),
            None,
        )));
        self.inner.tui.request_render(None);
    }

    /// Called by onProgress callback.
    pub fn show_progress(&self, message: &str) {
        self.inner.content_container.add_child(Arc::new(Text::new(
            Some(&theme().fg(ThemeColor::Dim, message)),
            Some(1),
            Some(0),
            None,
        )));
        self.inner.tui.request_render(None);
    }

    pub fn handle_input(&self, data: &str) {
        let kb = get_keybindings();

        if kb.matches(data, "tui.select.cancel") {
            self.cancel();
            return;
        }

        let auth_url = {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .auth_url
                .clone()
        };
        if let Some(auth_url) = auth_url
            && kb.matches(data, "app.message.copy")
        {
            let copy_override = COPY_TO_CLIPBOARD_OVERRIDE.with(|s| s.get());
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    auth_url.copy_with_override(copy_override).await;
                });
            }
            return;
        }

        let is_aborted = self.signal().aborted();
        let action = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.device_code_action.is_some()
                && state.pending_input.is_none()
                && !is_aborted
                && state.focused
                && kb.matches(data, "tui.select.confirm")
            {
                // Consume once so a repeated Enter cannot launch multiple browsers or clipboard writes.
                state.device_code_action.take()
            } else {
                None
            }
        };
        if let Some(action) = action {
            self.activate_device_code(action);
            self.inner.tui.request_render(None);
            return;
        }

        // Pass to input
        self.inner.input.handle_input(data);
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.inner.container.render(width)
    }

    pub fn invalidate(&self) {
        self.inner.container.invalidate();
    }
}

impl Focusable for LoginDialogComponent {
    fn focused(&self) -> bool {
        LoginDialogComponent::focused(self)
    }

    fn set_focused(&self, focused: bool) {
        LoginDialogComponent::set_focused(self, focused)
    }
}

impl Component for LoginDialogComponent {
    fn render(&self, width: usize) -> Vec<String> {
        LoginDialogComponent::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        LoginDialogComponent::handle_input(self, data)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        LoginDialogComponent::invalidate(self)
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn as_focusable(&self) -> Option<&dyn Focusable> {
        Some(self)
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.inner.container.children())
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
    use pi_tui::{KeybindingsManager, TUI_KEYBINDINGS, TuiMainScreen, set_keybindings, strip_terminal_sequences};
    use serial_test::serial;

    use super::*;
    use crate::modes::interactive::theme::theme::init_theme;

    struct NoopTerminal;

    #[async_trait]
    impl Terminal for NoopTerminal {
        fn start(&self, _on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {}
        fn stop(&self) {}
        async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
        fn write(&self, _data: &str) {}
        fn columns(&self) -> i64 {
            120
        }
        fn rows(&self) -> i64 {
            24
        }
        fn kitty_protocol_active(&self) -> bool {
            false
        }
        fn move_by(&self, _lines: i64) {}
        fn hide_cursor(&self) {}
        fn show_cursor(&self) {}
        fn clear_line(&self) {}
        fn clear_from_cursor(&self) {}
        fn clear_screen(&self) {}
        fn set_title(&self, _title: &str) {}
        fn set_progress(&self, _active: bool) {}
    }

    struct Guard {
        _open_guard: pi_js::seam::Guard,
        _copy_guard: pi_js::seam::Guard,
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            init_theme(Some("dark"), None);
            set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
        }
    }

    fn setup_keybindings() {
        set_keybindings(KeybindingsManager::new(
            crate::core::keybindings::KEYBINDINGS.clone(),
            None,
        ));
    }

    fn create_dialog(
        opened: Arc<Mutex<Vec<String>>>,
        copied: Arc<Mutex<Vec<String>>>,
        copy_err: Option<&'static str>,
    ) -> (LoginDialogComponent, Arc<Mutex<Vec<(bool, Option<String>)>>>, Guard) {
        init_theme(Some("dark"), None);
        setup_keybindings();

        let opened_c = Arc::clone(&opened);
        let open_guard = testing::override_open_browser(move |url| {
            opened_c.lock().unwrap_or_else(|e| e.into_inner()).push(url.to_string());
        });

        let copied_c = Arc::clone(&copied);
        let copy_guard = testing::override_copy_to_clipboard(move |text| {
            copied_c
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(text.to_string());
            let err = copy_err;
            Box::pin(async move {
                match err {
                    Some(msg) => Err(pi_js::Error::msg(msg)),
                    None => Ok(()),
                }
            })
        });

        let tui: Arc<dyn TUI + Send + Sync> = Arc::new(TuiMainScreen::new(Arc::new(NoopTerminal), None, None));
        let completed: Arc<Mutex<Vec<(bool, Option<String>)>>> = Arc::new(Mutex::new(Vec::new()));
        let completed_c = Arc::clone(&completed);
        let dialog = LoginDialogComponent::new(
            tui,
            "custom-provider",
            Arc::new(move |ok, msg| {
                completed_c
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((ok, msg.map(ToString::to_string)));
            }),
            None,
            None,
        );
        dialog.set_focused(true);
        (
            dialog,
            completed,
            Guard {
                _open_guard: open_guard,
                _copy_guard: copy_guard,
            },
        )
    }

    fn render_plain(dialog: &LoginDialogComponent) -> String {
        strip_terminal_sequences(&dialog.render(120).join("\n"))
    }

    #[tokio::test]
    #[serial]
    async fn device_code_requires_confirmation_and_consumes_once() {
        let opened = Arc::new(Mutex::new(Vec::new()));
        let copied = Arc::new(Mutex::new(Vec::new()));
        let (dialog, _, _guard) = create_dialog(Arc::clone(&opened), Arc::clone(&copied), None);

        let domain = dialog.show_prompt("GitHub Enterprise URL/domain (blank for github.com)", None);
        dialog.handle_input("\r");
        let resolved = match domain.await {
            Ok(v) => v,
            Err(e) => panic!("expected Ok, got {e}"),
        };
        assert_eq!(resolved, "");

        dialog.show_device_code(OAuthDeviceCodeInfo {
            user_code: "ABCD-EFGH".to_string(),
            verification_uri: "https://example.com/device".to_string(),
            open_browser_on_confirm: None,
            copy_code_on_confirm: None,
            interval_seconds: None,
            expires_in_seconds: None,
        });
        dialog.show_waiting("Waiting for authentication...");
        assert!(render_plain(&dialog).contains("to open browser and copy code"));
        assert!(opened.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
        assert!(copied.lock().unwrap_or_else(|e| e.into_inner()).is_empty());

        dialog.handle_input("\r");
        tokio::task::yield_now().await;
        dialog.handle_input("\r");
        assert_eq!(
            *opened.lock().unwrap_or_else(|e| e.into_inner()),
            vec!["https://example.com/device"]
        );
        assert_eq!(*copied.lock().unwrap_or_else(|e| e.into_inner()), vec!["ABCD-EFGH"]);
        let text = render_plain(&dialog);
        assert!(text.contains("Code copied to clipboard"), "text={text}");
        assert!(text.contains("Enter code: ABCD-EFGH"));
        assert!(text.contains("Waiting for authentication..."));
        assert!(!text.contains("to open browser and copy code"));
    }

    #[tokio::test]
    #[serial]
    async fn honors_device_code_confirm_flags_and_failures() {
        for (open_flag, copy_flag) in [(false, false), (true, false), (false, true)] {
            let opened = Arc::new(Mutex::new(Vec::new()));
            let copied = Arc::new(Mutex::new(Vec::new()));
            let (dialog, _, _guard) = create_dialog(Arc::clone(&opened), Arc::clone(&copied), None);
            dialog.show_device_code(DeviceCodeInfo {
                user_code: "ABCD-EFGH".to_string(),
                verification_uri: "https://example.com/device".to_string(),
                open_browser_on_confirm: Some(open_flag),
                copy_code_on_confirm: Some(copy_flag),
                interval_seconds: None,
                expires_in_seconds: None,
            });
            dialog.handle_input("\r");
            tokio::task::yield_now().await;
            assert_eq!(
                opened.lock().unwrap_or_else(|e| e.into_inner()).len(),
                usize::from(open_flag)
            );
            assert_eq!(
                copied.lock().unwrap_or_else(|e| e.into_inner()).len(),
                usize::from(copy_flag)
            );
            assert!(!dialog.signal().aborted());
        }

        let opened = Arc::new(Mutex::new(Vec::new()));
        let copied = Arc::new(Mutex::new(Vec::new()));
        let (dialog, completed, _guard) =
            create_dialog(Arc::clone(&opened), Arc::clone(&copied), Some("Clipboard unavailable"));
        dialog.show_device_code(OAuthDeviceCodeInfo {
            user_code: "ABCD-EFGH".to_string(),
            verification_uri: "https://example.com/device".to_string(),
            open_browser_on_confirm: None,
            copy_code_on_confirm: None,
            interval_seconds: None,
            expires_in_seconds: None,
        });
        dialog.show_waiting("Waiting for authentication...");
        dialog.handle_input("\r");
        tokio::task::yield_now().await;
        let text = render_plain(&dialog);
        assert!(text.contains("Copy code manually"), "text={text}");
        assert!(text.contains("Enter code: ABCD-EFGH"));
        assert!(!dialog.signal().aborted());
        assert!(completed.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
    }

    #[tokio::test]
    #[serial]
    async fn clears_actions_between_modes_and_on_cancel() {
        let opened = Arc::new(Mutex::new(Vec::new()));
        let copied = Arc::new(Mutex::new(Vec::new()));
        let (dialog, completed, _guard) = create_dialog(Arc::clone(&opened), Arc::clone(&copied), None);

        dialog.show_auth("https://example.com/authorize", None);
        opened.lock().unwrap_or_else(|e| e.into_inner()).clear();
        dialog.show_device_code(OAuthDeviceCodeInfo {
            user_code: "ABCD-EFGH".to_string(),
            verification_uri: "https://example.com/device".to_string(),
            open_browser_on_confirm: None,
            copy_code_on_confirm: None,
            interval_seconds: None,
            expires_in_seconds: None,
        });
        dialog.handle_input("\x18");
        tokio::task::yield_now().await;
        assert!(copied.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
        dialog.handle_input("\r");
        tokio::task::yield_now().await;
        assert_eq!(*copied.lock().unwrap_or_else(|e| e.into_inner()), vec!["ABCD-EFGH"]);

        copied.lock().unwrap_or_else(|e| e.into_inner()).clear();
        dialog.show_auth("https://example.com/authorize", None);
        dialog.handle_input("\r");
        assert!(copied.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
        dialog.handle_input("\x18");
        tokio::task::yield_now().await;
        assert_eq!(
            *copied.lock().unwrap_or_else(|e| e.into_inner()),
            vec!["https://example.com/authorize"]
        );

        copied.lock().unwrap_or_else(|e| e.into_inner()).clear();
        opened.lock().unwrap_or_else(|e| e.into_inner()).clear();
        dialog.show_details(&["Choose a login method".to_string()]);
        dialog.handle_input("\x18");
        dialog.handle_input("\r");
        tokio::task::yield_now().await;
        assert!(copied.lock().unwrap_or_else(|e| e.into_inner()).is_empty());

        dialog.show_device_code(OAuthDeviceCodeInfo {
            user_code: "ABCD-EFGH".to_string(),
            verification_uri: "https://example.com/device".to_string(),
            open_browser_on_confirm: None,
            copy_code_on_confirm: None,
            interval_seconds: None,
            expires_in_seconds: None,
        });
        dialog.handle_input("\x1b");
        dialog.handle_input("\r");
        assert!(dialog.signal().aborted());
        assert_eq!(
            *completed.lock().unwrap_or_else(|e| e.into_inner()),
            vec![(false, Some("Login cancelled".to_string()))]
        );
        assert!(opened.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
        assert!(copied.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
    }

    #[test]
    #[serial]
    fn sanitizes_urls_and_only_opens_http_https() {
        let opened = Arc::new(Mutex::new(Vec::new()));
        let copied = Arc::new(Mutex::new(Vec::new()));
        let (dialog, _, _guard) = create_dialog(Arc::clone(&opened), Arc::clone(&copied), None);

        dialog.show_auth("file:///Users/victim/Downloads/payload.app", None);
        assert!(opened.lock().unwrap_or_else(|e| e.into_inner()).is_empty());

        dialog.show_auth("https://example.com/authorize", None);
        assert_eq!(
            *opened.lock().unwrap_or_else(|e| e.into_inner()),
            vec!["https://example.com/authorize"]
        );

        opened.lock().unwrap_or_else(|e| e.into_inner()).clear();
        dialog.show_device_code(OAuthDeviceCodeInfo {
            user_code: "ABCD-EFGH".to_string(),
            verification_uri: "https://example.com/device\x07\x1b]52;c;Zm9v\x07".to_string(),
            open_browser_on_confirm: None,
            copy_code_on_confirm: None,
            interval_seconds: None,
            expires_in_seconds: None,
        });
        dialog.show_info(
            "Docs",
            Some(&[AuthInfoLink {
                label: Some("Help".to_string()),
                url: "https://example.com/help\x07\x1b]52;c;Zm9v\x07".to_string(),
            }]),
            None,
        );
        let raw = dialog.render(120).join("\n");
        let plain = render_plain(&dialog);
        assert!(!raw.contains("\x1b]52"));
        assert!(
            plain.contains("https://example.com/device%07%1B]52;c;Zm9v"),
            "plain={plain}"
        );
        assert!(
            plain.contains("Help: https://example.com/help%07%1B]52;c;Zm9v"),
            "plain={plain}"
        );

        let injected = "x\x07\x1b]52;c;Zm9v\x07";
        dialog.show_auth(&format!("file:///tmp/{injected}"), None);
        assert!(!dialog.render(120).join("\n").contains("\x1b]52"));

        dialog.show_device_code(OAuthDeviceCodeInfo {
            user_code: "ABCD-EFGH".to_string(),
            verification_uri: injected.to_string(),
            open_browser_on_confirm: None,
            copy_code_on_confirm: None,
            interval_seconds: None,
            expires_in_seconds: None,
        });
        assert!(!dialog.render(120).join("\n").contains("\x1b]52"));
        assert!(render_plain(&dialog).contains("x ]52;c;Zm9v"));

        dialog.show_info(
            "Docs",
            Some(&[AuthInfoLink {
                label: Some("Help".to_string()),
                url: injected.to_string(),
            }]),
            None,
        );
        assert!(!dialog.render(120).join("\n").contains("\x1b]52"));
        assert!(render_plain(&dialog).contains("Help: x ]52;c;Zm9v"));
        assert!(opened.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
    }
}
