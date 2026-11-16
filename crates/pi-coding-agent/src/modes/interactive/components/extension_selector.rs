//! Port of packages/coding-agent/src/modes/interactive/components/extension-selector.ts

use std::sync::{Arc, Mutex};

use pi_tui::{Component, Container, Spacer, TUI, Text, TuiMouseEvent, TuiMouseEventResult, get_keybindings};

use super::super::theme::theme::{ThemeColor, theme};
use super::countdown_timer::CountdownTimer;
use super::dynamic_border::DynamicBorder;
use super::keybinding_hints::{get_select_page_index, key_hint, raw_key_hint};

#[derive(Clone, Default)]
pub struct ExtensionSelectorOptions {
    pub tui: Option<Arc<dyn TUI + Send + Sync>>,
    pub timeout: Option<i64>,
    pub on_toggle_tools_expanded: Option<Arc<dyn Fn() + Send + Sync>>,
    pub description: Option<String>,
}

struct ExtensionSelectorState {
    selected_index: usize,
    countdown: Option<CountdownTimer>,
}

struct ExtensionSelectorInner {
    container: Container,
    list_container: Container,
    options: Vec<String>,
    on_select: Arc<dyn Fn(&str) + Send + Sync>,
    on_cancel: Arc<dyn Fn() + Send + Sync>,
    on_toggle_tools_expanded: Option<Arc<dyn Fn() + Send + Sync>>,
    state: Mutex<ExtensionSelectorState>,
}

impl Drop for ExtensionSelectorInner {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.state.lock()
            && let Some(countdown) = guard.countdown.take()
        {
            countdown.dispose();
        }
    }
}

/// Generic selector component for extensions.
#[derive(Clone)]
pub struct ExtensionSelectorComponent {
    inner: Arc<ExtensionSelectorInner>,
}

impl ExtensionSelectorComponent {
    pub fn new(
        title: &str,
        options: Vec<String>,
        on_select: Arc<dyn Fn(&str) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
        opts: Option<ExtensionSelectorOptions>,
    ) -> Self {
        let container = Container::new();
        container.add_child(Arc::new(DynamicBorder::default()));
        container.add_child(Arc::new(Spacer::new(Some(1))));

        let t = theme();
        let title_text = Text::new(Some(&t.fg(ThemeColor::Accent, &t.bold(title))), Some(1), Some(0), None);
        container.add_child(Arc::new(title_text.clone()));
        if let Some(ref desc) = opts.as_ref().and_then(|o| o.description.clone()) {
            container.add_child(Arc::new(Spacer::new(Some(1))));
            container.add_child(Arc::new(Text::new(
                Some(&t.fg(ThemeColor::Text, desc)),
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
                    let t = theme();
                    title_c.set_text(&t.fg(ThemeColor::Accent, &t.bold(&format!("{base_title} ({s}s)"))));
                }),
                Arc::new(move || cancel_c()),
            ))
        } else {
            None
        };

        let list_container = Container::new();
        container.add_child(Arc::new(list_container.clone()));
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(Text::new(
            Some(&format!(
                "{}  {}  {}",
                raw_key_hint("↑↓", "navigate"),
                key_hint(&"tui.select.confirm".to_string(), "select"),
                key_hint(&"tui.select.cancel".to_string(), "cancel"),
            )),
            Some(1),
            Some(0),
            None,
        )));
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(DynamicBorder::default()));

        let comp = Self {
            inner: Arc::new(ExtensionSelectorInner {
                container,
                list_container,
                options,
                on_select,
                on_cancel,
                on_toggle_tools_expanded: opts.and_then(|o| o.on_toggle_tools_expanded),
                state: Mutex::new(ExtensionSelectorState {
                    selected_index: 0,
                    countdown,
                }),
            }),
        };
        comp.update_list();
        comp
    }

    fn update_list(&self) {
        let selected_index = self.inner.state.lock().unwrap().selected_index;
        self.inner.list_container.clear();

        let len = self.inner.options.len();
        let max_visible = 10usize;
        let max_start = len.saturating_sub(max_visible);
        let start_index = selected_index.saturating_sub(max_visible / 2).min(max_start);
        let end_index = (start_index + max_visible).min(len);
        let t = theme();

        for i in start_index..end_index {
            let is_selected = i == selected_index;
            let opt = &self.inner.options[i];
            let text = if is_selected {
                format!("{}{}", t.fg(ThemeColor::Accent, "→ "), t.fg(ThemeColor::Accent, opt))
            } else {
                format!("  {}", t.fg(ThemeColor::Text, opt))
            };
            self.inner
                .list_container
                .add_child(Arc::new(Text::new(Some(&text), Some(1), Some(0), None)));
        }

        if start_index > 0 || end_index < len {
            let scroll_info = t.fg(ThemeColor::Muted, &format!("  ({}/{})", selected_index + 1, len));
            self.inner
                .list_container
                .add_child(Arc::new(Text::new(Some(&scroll_info), Some(0), Some(0), None)));
        }
    }

    pub fn handle_input(&self, key_data: &str) {
        let kb = get_keybindings();
        let len = self.inner.options.len();
        if kb.matches(key_data, "app.tools.expand") {
            if let Some(ref cb) = self.inner.on_toggle_tools_expanded {
                cb();
            }
        } else if kb.matches(key_data, "tui.select.up") || key_data == "k" {
            {
                let mut state = self.inner.state.lock().unwrap();
                state.selected_index = state.selected_index.saturating_sub(1);
            }
            self.update_list();
        } else if kb.matches(key_data, "tui.select.down") || key_data == "j" {
            {
                let mut state = self.inner.state.lock().unwrap();
                if len > 0 {
                    state.selected_index = (state.selected_index + 1).min(len - 1);
                }
            }
            self.update_list();
        } else if let Some(page_index) = {
            let selected_index = self.inner.state.lock().unwrap().selected_index;
            get_select_page_index(&kb, key_data, selected_index, len, 10)
        } {
            self.inner.state.lock().unwrap().selected_index = page_index;
            self.update_list();
        } else if kb.matches(key_data, "tui.select.confirm") || key_data == "\n" {
            let idx = self.inner.state.lock().unwrap().selected_index;
            if let Some(selected) = self.inner.options.get(idx) {
                (self.inner.on_select)(selected);
            }
        } else if kb.matches(key_data, "tui.select.cancel") {
            (self.inner.on_cancel)();
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

impl Component for ExtensionSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        ExtensionSelectorComponent::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        ExtensionSelectorComponent::handle_input(self, data)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        ExtensionSelectorComponent::invalidate(self)
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.inner.container.children())
    }
}
