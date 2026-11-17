//! Port of packages/coding-agent/src/modes/interactive/components/trust-selector.ts

use std::sync::{Arc, Mutex};

use pi_js::path::{dirname, resolve};
use pi_tui::{Component, Container, Spacer, Text, TuiMouseEvent, TuiMouseEventResult, get_keybindings};
use serde::{Deserialize, Serialize};

use super::super::theme::theme::{ThemeColor, theme};
use super::dynamic_border::DynamicBorder;
use super::keybinding_hints::{key_hint, raw_key_hint};

// WIRE(@tui-w3): Replace local mirror types and `get_project_trust_options_local` with `crate::core::trust_manager::{ProjectTrustOption, ProjectTrustStoreEntry, ProjectTrustUpdate, get_project_trust_options}` once `mcp-w3` merges onto main.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTrustStoreEntry {
    pub path: String,
    pub decision: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTrustUpdate {
    pub path: String,
    pub decision: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTrustOption {
    pub label: String,
    pub trusted: bool,
    pub updates: Vec<ProjectTrustUpdate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_path: Option<String>,
}

fn normalize_cwd_local(cwd: &str) -> String {
    let resolved = resolve(&[cwd]);
    std::fs::canonicalize(&resolved)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or(resolved)
}

fn get_project_trust_parent_path_local(cwd: &str) -> Option<String> {
    let trust_path = normalize_cwd_local(cwd);
    let parent_dir = dirname(&trust_path);
    if parent_dir == trust_path {
        None
    } else {
        Some(parent_dir)
    }
}

fn get_project_trust_options_local(cwd: &str) -> Vec<ProjectTrustOption> {
    let trust_path = normalize_cwd_local(cwd);
    let mut trust_options = vec![ProjectTrustOption {
        label: "Trust".to_string(),
        trusted: true,
        updates: vec![ProjectTrustUpdate {
            path: trust_path.clone(),
            decision: Some(true),
        }],
        saved_path: Some(trust_path.clone()),
    }];

    if let Some(parent_path) = get_project_trust_parent_path_local(cwd) {
        trust_options.push(ProjectTrustOption {
            label: format!("Trust parent folder ({parent_path})"),
            trusted: true,
            updates: vec![
                ProjectTrustUpdate {
                    path: parent_path.clone(),
                    decision: Some(true),
                },
                ProjectTrustUpdate {
                    path: trust_path.clone(),
                    decision: None,
                },
            ],
            saved_path: Some(parent_path),
        });
    }

    trust_options.push(ProjectTrustOption {
        label: "Do not trust".to_string(),
        trusted: false,
        updates: vec![ProjectTrustUpdate {
            path: trust_path.clone(),
            decision: Some(false),
        }],
        saved_path: Some(trust_path),
    });

    trust_options
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustSelection {
    pub trusted: bool,
    pub updates: Vec<ProjectTrustUpdate>,
}

pub struct TrustSelectorOptions {
    pub cwd: String,
    pub saved_decision: Option<ProjectTrustStoreEntry>,
    pub project_trusted: bool,
    pub on_select: Arc<dyn Fn(TrustSelection) + Send + Sync>,
    pub on_cancel: Arc<dyn Fn() + Send + Sync>,
}

fn format_decision(trust_path: Option<&str>, decision: Option<&ProjectTrustStoreEntry>) -> String {
    let Some(decision) = decision else {
        return "none".to_string();
    };
    let label = if decision.decision { "trusted" } else { "untrusted" };
    if let Some(tp) = trust_path
        && decision.path != tp
    {
        return format!("{label} (inherited from {})", decision.path);
    }
    format!("{label} ({})", decision.path)
}

struct TrustSelectorState {
    selected_index: usize,
}

struct TrustSelectorInner {
    container: Container,
    list_container: Container,
    trust_options: Vec<ProjectTrustOption>,
    saved_decision: Option<ProjectTrustStoreEntry>,
    on_select: Arc<dyn Fn(TrustSelection) + Send + Sync>,
    on_cancel: Arc<dyn Fn() + Send + Sync>,
    state: Mutex<TrustSelectorState>,
}

#[derive(Clone)]
pub struct TrustSelectorComponent {
    inner: Arc<TrustSelectorInner>,
}

impl TrustSelectorComponent {
    pub fn new(options: TrustSelectorOptions) -> Self {
        let trust_options = get_project_trust_options_local(&options.cwd);
        let is_saved = |opt: &ProjectTrustOption| -> bool {
            let Some(ref saved_path) = opt.saved_path else {
                return false;
            };
            options
                .saved_decision
                .as_ref()
                .is_some_and(|sd| sd.decision == opt.trusted && sd.path == *saved_path)
        };
        let initial_selected = trust_options.iter().position(is_saved).unwrap_or(0);

        let t = theme();
        let container = Container::new();
        container.add_child(Arc::new(DynamicBorder::default()));
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(Text::new(
            Some(&t.fg(ThemeColor::Accent, &t.bold("Project trust"))),
            Some(1),
            Some(0),
            None,
        )));
        container.add_child(Arc::new(Text::new(
            Some(&t.fg(ThemeColor::Muted, &options.cwd)),
            Some(1),
            Some(0),
            None,
        )));
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(Text::new(
            Some(&t.fg(
                ThemeColor::Muted,
                &format!(
                    "Saved decision: {}",
                    format_decision(
                        trust_options.first().and_then(|o| o.saved_path.as_deref()),
                        options.saved_decision.as_ref()
                    )
                ),
            )),
            Some(1),
            Some(0),
            None,
        )));
        container.add_child(Arc::new(Text::new(
            Some(&t.fg(
                ThemeColor::Muted,
                &format!(
                    "Current session: {}",
                    if options.project_trusted {
                        "trusted"
                    } else {
                        "untrusted"
                    }
                ),
            )),
            Some(1),
            Some(0),
            None,
        )));
        container.add_child(Arc::new(Spacer::new(Some(1))));

        let list_container = Container::new();
        container.add_child(Arc::new(list_container.clone()));
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(Text::new(
            Some(&format!(
                "{}  {}  {}",
                raw_key_hint("↑↓", "navigate"),
                key_hint(&"tui.select.confirm".to_string(), "save"),
                key_hint(&"tui.select.cancel".to_string(), "cancel"),
            )),
            Some(1),
            Some(0),
            None,
        )));
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(DynamicBorder::default()));

        let comp = Self {
            inner: Arc::new(TrustSelectorInner {
                container,
                list_container,
                trust_options,
                saved_decision: options.saved_decision,
                on_select: options.on_select,
                on_cancel: options.on_cancel,
                state: Mutex::new(TrustSelectorState {
                    selected_index: initial_selected,
                }),
            }),
        };
        comp.update_list();
        comp
    }

    fn is_saved_option(&self, option: &ProjectTrustOption) -> bool {
        let Some(ref saved_path) = option.saved_path else {
            return false;
        };
        self.inner
            .saved_decision
            .as_ref()
            .is_some_and(|sd| sd.decision == option.trusted && sd.path == *saved_path)
    }

    fn update_list(&self) {
        self.inner.list_container.clear();
        let selected_index = self.inner.state.lock().unwrap().selected_index;
        let t = theme();
        for (i, option) in self.inner.trust_options.iter().enumerate() {
            let is_selected = i == selected_index;
            let is_current = self.is_saved_option(option);
            let current_marker = if is_current {
                t.fg(ThemeColor::Accent, "✓ ")
            } else {
                "  ".to_string()
            };
            let prefix = if is_selected {
                t.fg(ThemeColor::Accent, "→ ")
            } else {
                "  ".to_string()
            };
            let label = if is_selected {
                t.fg(ThemeColor::Accent, &option.label)
            } else {
                t.fg(ThemeColor::Text, &option.label)
            };
            self.inner.list_container.add_child(Arc::new(Text::new(
                Some(&format!("{prefix}{current_marker}{label}")),
                Some(1),
                Some(0),
                None,
            )));
        }
    }

    pub fn handle_input(&self, key_data: &str) {
        let kb = get_keybindings();
        let len = self.inner.trust_options.len();
        if kb.matches(key_data, "tui.select.up") || key_data == "k" {
            {
                let mut state = self.inner.state.lock().unwrap();
                state.selected_index = state.selected_index.saturating_sub(1);
            }
            self.update_list();
        } else if kb.matches(key_data, "tui.select.down") || key_data == "j" {
            if len > 0 {
                let mut state = self.inner.state.lock().unwrap();
                state.selected_index = (state.selected_index + 1).min(len - 1);
            }
            self.update_list();
        } else if kb.matches(key_data, "tui.select.confirm") || key_data == "\n" {
            let idx = self.inner.state.lock().unwrap().selected_index;
            if let Some(selected) = self.inner.trust_options.get(idx) {
                (self.inner.on_select)(TrustSelection {
                    trusted: selected.trusted,
                    updates: selected.updates.clone(),
                });
            }
        } else if kb.matches(key_data, "tui.select.cancel") {
            (self.inner.on_cancel)();
        }
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.inner.container.render(width)
    }

    pub fn invalidate(&self) {
        self.inner.container.invalidate();
    }
}

impl Component for TrustSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        TrustSelectorComponent::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        TrustSelectorComponent::handle_input(self, data)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        TrustSelectorComponent::invalidate(self)
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
