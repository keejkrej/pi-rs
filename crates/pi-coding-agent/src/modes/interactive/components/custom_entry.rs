//! Port of packages/coding-agent/src/modes/interactive/components/custom-entry.ts

use std::sync::{Arc, Mutex};

pub use crate::core::session_manager::CustomEntry;
use pi_tui::{Component, Container, Spacer, TuiMouseEvent, TuiMouseEventResult};
use serde::{Deserialize, Serialize};

use super::super::theme::theme::{Theme, theme};

// WIRE(@tui-w3): Replace local `EntryRenderOptions`/`EntryRenderer` with `crate::core::extensions::types::{EntryRenderOptions, EntryRenderer}` once @codemode merges.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryRenderOptions {
    pub expanded: bool,
}

pub type EntryRenderer =
    Arc<dyn Fn(CustomEntry, EntryRenderOptions, Theme) -> Option<Arc<dyn Component>> + Send + Sync>;

struct CustomEntryState {
    custom_component: Option<Arc<dyn Component>>,
    expanded: bool,
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
        let comp = Self {
            inner: Arc::new(CustomEntryInner {
                container: Container::new(),
                entry,
                renderer,
                state: Mutex::new(CustomEntryState {
                    custom_component: None,
                    expanded: false,
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

    fn rebuild(&self) {
        self.inner.container.clear();
        let expanded = {
            let mut state = self.inner.state.lock().unwrap();
            state.custom_component = None;
            state.expanded
        };

        let t = theme();
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
