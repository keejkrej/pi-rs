//! Port of packages/coding-agent/src/modes/interactive/components/user-message-selector.ts

use std::sync::{Arc, Mutex};

use pi_js::time::Timeout;
use pi_tui::{
    Component, Container, Spacer, Text, TuiMouseEvent, TuiMouseEventResult, get_keybindings, truncate_to_width,
};

use super::super::theme::theme::{ThemeColor, theme};
use super::dynamic_border::DynamicBorder;
use super::keybinding_hints::get_select_page_index;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserMessageItem {
    pub id: String,
    pub text: String,
    pub timestamp: Option<String>,
}

struct UserMessageListState {
    selected_index: usize,
    on_select: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    on_cancel: Option<Arc<dyn Fn() + Send + Sync>>,
}

struct UserMessageListInner {
    messages: Vec<UserMessageItem>,
    max_visible: usize,
    state: Mutex<UserMessageListState>,
}

#[derive(Clone)]
pub struct UserMessageList {
    inner: Arc<UserMessageListInner>,
}

impl UserMessageList {
    pub fn new(messages: Vec<UserMessageItem>, initial_selected_id: Option<&str>) -> Self {
        let initial_index = initial_selected_id.and_then(|id| messages.iter().position(|m| m.id == id));
        let selected_index = initial_index.unwrap_or_else(|| messages.len().saturating_sub(1));
        Self {
            inner: Arc::new(UserMessageListInner {
                messages,
                max_visible: 10,
                state: Mutex::new(UserMessageListState {
                    selected_index,
                    on_select: None,
                    on_cancel: None,
                }),
            }),
        }
    }

    pub fn set_on_select(&self, cb: Option<Arc<dyn Fn(&str) + Send + Sync>>) {
        self.inner.state.lock().unwrap().on_select = cb;
    }

    pub fn set_on_cancel(&self, cb: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.inner.state.lock().unwrap().on_cancel = cb;
    }

    pub fn invalidate(&self) {}

    pub fn render(&self, width: usize) -> Vec<String> {
        let t = theme();
        let mut lines = Vec::new();
        let len = self.inner.messages.len();
        if len == 0 {
            lines.push(t.fg(ThemeColor::Muted, "  No user messages found"));
            return lines;
        }

        let selected_index = self.inner.state.lock().unwrap().selected_index;
        let max_visible = self.inner.max_visible;
        let max_start = len.saturating_sub(max_visible);
        let start_index = selected_index.saturating_sub(max_visible / 2).min(max_start);
        let end_index = (start_index + max_visible).min(len);

        for i in start_index..end_index {
            let message = &self.inner.messages[i];
            let is_selected = i == selected_index;
            let normalized = message.text.replace('\n', " ");
            let normalized = normalized.trim();
            let cursor = if is_selected {
                t.fg(ThemeColor::Accent, "› ")
            } else {
                "  ".to_string()
            };
            let max_msg_width = (width as i64 - 2).max(0);
            let truncated = truncate_to_width(normalized, max_msg_width, Some("..."), None);
            let styled_msg = if is_selected { t.bold(&truncated) } else { truncated };
            lines.push(format!("{cursor}{styled_msg}"));

            let metadata = format!("  Message {} of {}", i + 1, len);
            lines.push(t.fg(ThemeColor::Muted, &metadata));
            lines.push(String::new());
        }

        if start_index > 0 || end_index < len {
            let scroll_info = format!("  ({}/{})", selected_index + 1, len);
            lines.push(t.fg(ThemeColor::Muted, &scroll_info));
        }

        lines
    }

    pub fn handle_input(&self, key_data: &str) {
        let kb = get_keybindings();
        let len = self.inner.messages.len();
        let selected_index = self.inner.state.lock().unwrap().selected_index;
        let page_index = get_select_page_index(&kb, key_data, selected_index, len, self.inner.max_visible);

        if kb.matches(key_data, "tui.select.up") {
            if len > 0 {
                let mut state = self.inner.state.lock().unwrap();
                state.selected_index = if state.selected_index == 0 {
                    len - 1
                } else {
                    state.selected_index - 1
                };
            }
        } else if kb.matches(key_data, "tui.select.down") {
            if len > 0 {
                let mut state = self.inner.state.lock().unwrap();
                state.selected_index = if state.selected_index == len - 1 {
                    0
                } else {
                    state.selected_index + 1
                };
            }
        } else if let Some(next_idx) = page_index {
            if len == 0 {
                return;
            }
            self.inner.state.lock().unwrap().selected_index = next_idx;
        } else if kb.matches(key_data, "tui.select.confirm") {
            let (selected_id, cb) = {
                let state = self.inner.state.lock().unwrap();
                (
                    self.inner.messages.get(state.selected_index).map(|m| m.id.clone()),
                    state.on_select.clone(),
                )
            };
            if let (Some(id), Some(cb)) = (selected_id, cb) {
                cb(&id);
            }
        } else if kb.matches(key_data, "tui.select.cancel") {
            let cb = self.inner.state.lock().unwrap().on_cancel.clone();
            if let Some(cb) = cb {
                cb();
            }
        }
    }
}

impl Component for UserMessageList {
    fn render(&self, width: usize) -> Vec<String> {
        UserMessageList::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        UserMessageList::handle_input(self, data)
    }

    fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        UserMessageList::invalidate(self)
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn captures_viewport_input(&self) -> bool {
        true
    }
}

struct UserMessageSelectorInner {
    container: Container,
    message_list: UserMessageList,
    auto_cancel_timer: Mutex<Option<Timeout>>,
}

impl Drop for UserMessageSelectorInner {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.auto_cancel_timer.lock()
            && let Some(t) = guard.take()
        {
            t.clear();
        }
    }
}

#[derive(Clone)]
pub struct UserMessageSelectorComponent {
    inner: Arc<UserMessageSelectorInner>,
}

impl UserMessageSelectorComponent {
    pub fn new(
        messages: Vec<UserMessageItem>,
        on_select: Arc<dyn Fn(&str) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
        initial_selected_id: Option<&str>,
    ) -> Self {
        let is_empty = messages.is_empty();
        let container = Container::new();
        let t = theme();

        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(Text::new(
            Some(&t.bold("Fork from Message")),
            Some(1),
            Some(0),
            None,
        )));
        container.add_child(Arc::new(Text::new(
            Some(&t.fg(
                ThemeColor::Muted,
                "Select a user message to copy the active path up to that point into a new session",
            )),
            Some(1),
            Some(0),
            None,
        )));
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(DynamicBorder::default()));
        container.add_child(Arc::new(Spacer::new(Some(1))));

        let message_list = UserMessageList::new(messages, initial_selected_id);
        message_list.set_on_select(Some(on_select));
        message_list.set_on_cancel(Some(Arc::clone(&on_cancel)));

        container.add_child(Arc::new(message_list.clone()));
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(DynamicBorder::default()));

        let timer = if is_empty {
            let cancel_c = Arc::clone(&on_cancel);
            Some(pi_js::time::set_timeout(100, move || cancel_c()))
        } else {
            None
        };

        Self {
            inner: Arc::new(UserMessageSelectorInner {
                container,
                message_list,
                auto_cancel_timer: Mutex::new(timer),
            }),
        }
    }

    pub fn get_message_list(&self) -> &UserMessageList {
        &self.inner.message_list
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.inner.container.render(width)
    }

    pub fn invalidate(&self) {
        self.inner.container.invalidate();
    }
}

impl Component for UserMessageSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        UserMessageSelectorComponent::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        self.inner.message_list.handle_input(data);
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        UserMessageSelectorComponent::invalidate(self)
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
