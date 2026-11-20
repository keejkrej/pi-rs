//! Port of packages/coding-agent/src/modes/interactive/components/custom-entry.ts

use std::sync::{Arc, Mutex};

pub use crate::core::extensions::types::{EntryRenderOptions, EntryRenderer};
pub use crate::core::session_manager::CustomEntry;
use pi_tui::{Component, Container, Spacer, TuiMouseEvent, TuiMouseEventResult};

use super::super::theme::theme::theme;

struct CustomEntryState {
    custom_component: Option<Arc<dyn Component>>,
    expanded: bool,
    output_pad: i64,
}

struct CustomEntryInner {
    container: Container,
    entry: CustomEntry,
    renderer: EntryRenderer,
    state: Mutex<CustomEntryState>,
}

/// Component that renders a custom session entry from extensions.
/// The host owns transcript spacing; renderer output should provide only its content.
#[derive(Clone)]
pub struct CustomEntryComponent {
    inner: Arc<CustomEntryInner>,
}

impl CustomEntryComponent {
    pub fn new(entry: CustomEntry, renderer: EntryRenderer) -> Self {
        Self::new_with_output_pad(entry, renderer, None)
    }

    pub fn new_with_output_pad(entry: CustomEntry, renderer: EntryRenderer, output_pad: Option<i64>) -> Self {
        let comp = Self {
            inner: Arc::new(CustomEntryInner {
                container: Container::new(),
                entry,
                renderer,
                state: Mutex::new(CustomEntryState {
                    custom_component: None,
                    expanded: false,
                    output_pad: output_pad.unwrap_or(1),
                }),
            }),
        };
        comp.rebuild();
        comp
    }

    pub fn has_content(&self) -> bool {
        self.inner.state.lock().unwrap().custom_component.is_some()
    }

    pub fn set_expanded(&self, expanded: bool) {
        let changed = {
            let mut state = self.inner.state.lock().unwrap();
            if state.expanded != expanded {
                state.expanded = expanded;
                true
            } else {
                false
            }
        };
        if changed {
            self.rebuild();
        }
    }

    pub fn set_output_pad(&self, output_pad: i64) {
        let changed = {
            let mut state = self.inner.state.lock().unwrap();
            if state.output_pad != output_pad {
                state.output_pad = output_pad;
                true
            } else {
                false
            }
        };
        if changed {
            self.rebuild();
        }
    }

    fn rebuild(&self) {
        self.inner.container.clear();
        let expanded = {
            let mut state = self.inner.state.lock().unwrap();
            state.custom_component = None;
            state.expanded
        };

        let t = crate::core::extensions::types::Theme::new(Arc::new(theme()));
        let Some(component) = (self.inner.renderer)(self.inner.entry.clone(), EntryRenderOptions { expanded }, t)
        else {
            return;
        };

        self.inner.state.lock().unwrap().custom_component = Some(Arc::clone(&component));
        self.inner.container.add_child(Arc::new(Spacer::new(Some(1))));
        self.inner.container.add_child(component);
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.inner.container.render(width)
    }

    pub fn invalidate(&self) {
        self.inner.container.invalidate();
        self.rebuild();
    }
}

impl Component for CustomEntryComponent {
    fn render(&self, width: usize) -> Vec<String> {
        CustomEntryComponent::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        CustomEntryComponent::invalidate(self)
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.inner.container.children())
    }
}
