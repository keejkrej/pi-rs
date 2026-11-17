//! Port of packages/coding-agent/src/modes/interactive/components/extension-input.ts

use std::sync::{Arc, Mutex};

use pi_tui::{
    Component, Container, Focusable, Input, Spacer, TUI, Text, TuiMouseEvent, TuiMouseEventResult, get_keybindings,
};

use super::super::theme::theme::{ThemeColor, theme};
use super::countdown_timer::CountdownTimer;
use super::dynamic_border::DynamicBorder;
use super::keybinding_hints::key_hint;

#[derive(Clone, Default)]
pub struct ExtensionInputOptions {
    pub tui: Option<Arc<dyn TUI + Send + Sync>>,
    pub timeout: Option<i64>,
    pub initial_value: Option<String>,
    pub description: Option<String>,
}

struct ExtensionInputState {
    focused: bool,
    countdown: Option<CountdownTimer>,
}

struct ExtensionInputInner {
    container: Container,
    input: Input,
    on_submit: Arc<dyn Fn(&str) + Send + Sync>,
    on_cancel: Arc<dyn Fn() + Send + Sync>,
    state: Mutex<ExtensionInputState>,
}

impl Drop for ExtensionInputInner {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.state.lock()
            && let Some(countdown) = guard.countdown.take()
        {
            countdown.dispose();
        }
    }
}

/// Simple text input component for extensions.
#[derive(Clone)]
pub struct ExtensionInputComponent {
    inner: Arc<ExtensionInputInner>,
}

impl ExtensionInputComponent {
    pub fn new(
        title: &str,
        _placeholder: Option<&str>,
        on_submit: Arc<dyn Fn(&str) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
        opts: Option<ExtensionInputOptions>,
    ) -> Self {
        let container = Container::new();
        container.add_child(Arc::new(DynamicBorder::default()));
        container.add_child(Arc::new(Spacer::new(Some(1))));

        let title_text = Text::new(Some(&theme().fg(ThemeColor::Accent, title)), Some(1), Some(0), None);
        container.add_child(Arc::new(title_text.clone()));
        if let Some(ref desc) = opts.as_ref().and_then(|o| o.description.clone()) {
            container.add_child(Arc::new(Spacer::new(Some(1))));
            container.add_child(Arc::new(Text::new(
                Some(&theme().fg(ThemeColor::Text, desc)),
                Some(1),
                Some(0),
                None,
            )));
        }
        container.add_child(Arc::new(Spacer::new(Some(1))));

        let countdown = if let Some(ref o) = opts
            && let Some(timeout) = o.timeout
            && timeout > 0
            && let Some(ref tui) = o.tui
        {
            let base_title = title.to_string();
            let title_c = title_text.clone();
            let cancel_c = Arc::clone(&on_cancel);
            Some(CountdownTimer::new(
                timeout,
                Some(Arc::clone(tui)),
                Arc::new(move |s| {
                    title_c.set_text(&theme().fg(ThemeColor::Accent, &format!("{base_title} ({s}s)")));
                }),
                Arc::new(move || cancel_c()),
            ))
        } else {
            None
        };

        let input = Input::new(None);
        if let Some(initial) = opts.as_ref().and_then(|o| o.initial_value.as_deref()) {
            input.set_value(initial);
        }
        container.add_child(Arc::new(input.clone()));
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(Text::new(
            Some(&format!(
                "{}  {}",
                key_hint(&"tui.select.confirm".to_string(), "submit"),
                key_hint(&"tui.select.cancel".to_string(), "cancel")
            )),
            Some(1),
            Some(0),
            None,
        )));
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(DynamicBorder::default()));

        Self {
            inner: Arc::new(ExtensionInputInner {
                container,
                input,
                on_submit,
                on_cancel,
                state: Mutex::new(ExtensionInputState {
                    focused: false,
                    countdown,
                }),
            }),
        }
    }

    pub fn focused(&self) -> bool {
        self.inner.state.lock().unwrap().focused
    }

    pub fn set_focused(&self, value: bool) {
        self.inner.state.lock().unwrap().focused = value;
        self.inner.input.set_focused(value);
    }

    pub fn handle_input(&self, key_data: &str) {
        let kb = get_keybindings();
        if kb.matches(key_data, "tui.select.confirm") || key_data == "\n" {
            let val = self.inner.input.get_value();
            (self.inner.on_submit)(&val);
        } else if kb.matches(key_data, "tui.select.cancel") {
            (self.inner.on_cancel)();
        } else {
            self.inner.input.handle_input(key_data);
        }
    }

    pub fn dispose(&self) {
        let cd = self.inner.state.lock().unwrap().countdown.take();
        if let Some(countdown) = cd {
            countdown.dispose();
        }
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.inner.container.render(width)
    }

    pub fn invalidate(&self) {
        self.inner.container.invalidate();
    }
}

impl Focusable for ExtensionInputComponent {
    fn focused(&self) -> bool {
        ExtensionInputComponent::focused(self)
    }

    fn set_focused(&self, focused: bool) {
        ExtensionInputComponent::set_focused(self, focused)
    }
}

impl Component for ExtensionInputComponent {
    fn render(&self, width: usize) -> Vec<String> {
        ExtensionInputComponent::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        ExtensionInputComponent::handle_input(self, data)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        ExtensionInputComponent::invalidate(self)
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
