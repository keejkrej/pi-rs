//! Port of packages/coding-agent/src/modes/interactive/components/first-time-setup.ts

use std::sync::{Arc, Mutex};

use pi_tui::{Component, Container, Spacer, Text, TuiMouseEvent, TuiMouseEventResult, get_keybindings};

use super::super::theme::theme::{SYSTEM_THEME_NAME, ThemeColor, theme};
use super::dynamic_border::DynamicBorder;
use super::keybinding_hints::{key_hint, raw_key_hint};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirstTimeSetupResult {
    pub theme: String,
    pub share_analytics: bool,
}

#[derive(Clone)]
pub struct FirstTimeSetupOptions {
    pub on_theme_preview: Arc<dyn Fn(&str) + Send + Sync>,
    pub on_submit: Arc<dyn Fn(FirstTimeSetupResult) + Send + Sync>,
    pub on_cancel: Arc<dyn Fn() + Send + Sync>,
}

const THEME_OPTIONS: [(&str, &str); 3] = [
    (SYSTEM_THEME_NAME, "System (matches your terminal colors)"),
    ("dark", "Dark"),
    ("light", "Light"),
];

const ANALYTICS_OPTIONS: [(bool, &str); 2] = [(true, "Share anonymous usage data"), (false, "Don't share")];

const SETUP_LOGO_LINES: [&str; 4] = ["██████", "██  ██", "████  ██", "██    ██"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum SetupStep {
    Theme,
    Analytics,
}

struct FirstTimeSetupState {
    step: SetupStep,
    theme_index: usize,
    analytics_index: usize,
}

struct FirstTimeSetupInner {
    container: Container,
    options: FirstTimeSetupOptions,
    state: Mutex<FirstTimeSetupState>,
}

/// First-time setup dialog: theme choice and analytics opt-in.
#[derive(Clone)]
pub struct FirstTimeSetupComponent {
    inner: Arc<FirstTimeSetupInner>,
}

impl FirstTimeSetupComponent {
    pub fn new(options: FirstTimeSetupOptions) -> Self {
        let comp = Self {
            inner: Arc::new(FirstTimeSetupInner {
                container: Container::new(),
                options,
                state: Mutex::new(FirstTimeSetupState {
                    step: SetupStep::Theme,
                    theme_index: 0,
                    analytics_index: 0,
                }),
            }),
        };
        comp.update();
        comp
    }

    /// Rebuild on theme changes, e.g. when the system theme receives the terminal's colors.
    pub fn invalidate(&self) {
        self.update();
        self.inner.container.invalidate();
    }

    // Rebuild the whole dialog on every change so theme previews recolor all text.
    fn update(&self) {
        let (step, theme_index, analytics_index) = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            (state.step, state.theme_index, state.analytics_index)
        };

        let t = theme();
        self.inner.container.clear();
        self.inner.container.add_child(Arc::new(DynamicBorder::default()));
        self.inner.container.add_child(Arc::new(Spacer::new(Some(1))));
        self.inner.container.add_child(Arc::new(Text::new(
            Some(&t.fg(ThemeColor::Accent, &SETUP_LOGO_LINES.join("\n"))),
            Some(1),
            Some(0),
            None,
        )));
        self.inner.container.add_child(Arc::new(Spacer::new(Some(1))));
        self.inner.container.add_child(Arc::new(Text::new(
            Some(&t.fg(
                ThemeColor::Accent,
                &t.bold(&format!(
                    "Welcome to {}, the minimal coding agent.",
                    *crate::config::APP_NAME
                )),
            )),
            Some(1),
            Some(0),
            None,
        )));
        self.inner.container.add_child(Arc::new(Spacer::new(Some(1))));

        if step == SetupStep::Theme {
            self.inner.container.add_child(Arc::new(Text::new(
                Some(&t.fg(ThemeColor::Text, "Pick a theme.")),
                Some(1),
                Some(0),
                None,
            )));
            self.inner.container.add_child(Arc::new(Spacer::new(Some(1))));
            let labels: Vec<&str> = THEME_OPTIONS.iter().map(|(_, label)| *label).collect();
            self.add_option_list(&labels, theme_index);
        } else {
            self.inner.container.add_child(Arc::new(Text::new(
                Some(&t.fg(ThemeColor::Text, "Opt-in to anonymous usage data sharing?")),
                Some(1),
                Some(0),
                None,
            )));
            self.inner.container.add_child(Arc::new(Text::new(
                Some(&t.fg(
                    ThemeColor::Muted,
                    "Opting in stores a tracking identifier in settings.json and enables anonymous\nusage analytics. This helps us to better debug, reproduce, and resolve issues\nand bugs within Pi. You can observe what is shared using /privacy and make\nchanges anytime in settings.json.",
                )),
                Some(1),
                Some(0),
                None,
            )));
            self.inner.container.add_child(Arc::new(Spacer::new(Some(1))));
            let labels: Vec<&str> = ANALYTICS_OPTIONS.iter().map(|(_, label)| *label).collect();
            self.add_option_list(&labels, analytics_index);
        }

        self.inner.container.add_child(Arc::new(Spacer::new(Some(1))));
        let confirm_desc = if step == SetupStep::Theme { "continue" } else { "finish" };
        let hints = format!(
            "{}  {}  {}",
            raw_key_hint("↑↓", "navigate"),
            key_hint(&"tui.select.confirm".to_string(), confirm_desc),
            key_hint(&"tui.select.cancel".to_string(), "skip setup"),
        );
        self.inner
            .container
            .add_child(Arc::new(Text::new(Some(&hints), Some(1), Some(0), None)));
        self.inner.container.add_child(Arc::new(Spacer::new(Some(1))));
        self.inner.container.add_child(Arc::new(DynamicBorder::default()));
    }

    fn add_option_list(&self, labels: &[&str], selected_index: usize) {
        let t = theme();
        for (i, label_text) in labels.iter().enumerate() {
            let is_selected = i == selected_index;
            let prefix = if is_selected {
                t.fg(ThemeColor::Accent, "→ ")
            } else {
                "  ".to_string()
            };
            let label = if is_selected {
                t.fg(ThemeColor::Accent, label_text)
            } else {
                t.fg(ThemeColor::Text, label_text)
            };
            self.inner.container.add_child(Arc::new(Text::new(
                Some(&format!("{prefix}{label}")),
                Some(1),
                Some(0),
                None,
            )));
        }
    }

    fn move_selection(&self, delta: isize) {
        let preview_theme: Option<&'static str> = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.step == SetupStep::Theme {
                let max_idx = THEME_OPTIONS.len().saturating_sub(1) as isize;
                let next = (state.theme_index as isize + delta).clamp(0, max_idx) as usize;
                if next != state.theme_index {
                    state.theme_index = next;
                    Some(THEME_OPTIONS[next].0)
                } else {
                    None
                }
            } else {
                let max_idx = ANALYTICS_OPTIONS.len().saturating_sub(1) as isize;
                let next = (state.analytics_index as isize + delta).clamp(0, max_idx) as usize;
                state.analytics_index = next;
                None
            }
        };
        if let Some(theme_name) = preview_theme {
            (self.inner.options.on_theme_preview)(theme_name);
        }
        self.update();
    }

    pub fn handle_input(&self, key_data: &str) {
        let kb = get_keybindings();
        if kb.matches(key_data, "tui.select.up") || key_data == "k" {
            self.move_selection(-1);
        } else if kb.matches(key_data, "tui.select.down") || key_data == "j" {
            self.move_selection(1);
        } else if kb.matches(key_data, "tui.select.confirm") || key_data == "\n" {
            enum ConfirmAction {
                NextStep,
                Submit(FirstTimeSetupResult),
            }
            let action = {
                let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.step == SetupStep::Theme {
                    state.step = SetupStep::Analytics;
                    ConfirmAction::NextStep
                } else {
                    ConfirmAction::Submit(FirstTimeSetupResult {
                        theme: THEME_OPTIONS[state.theme_index].0.to_string(),
                        share_analytics: ANALYTICS_OPTIONS[state.analytics_index].0,
                    })
                }
            };
            match action {
                ConfirmAction::NextStep => self.update(),
                ConfirmAction::Submit(result) => (self.inner.options.on_submit)(result),
            }
        } else if kb.matches(key_data, "tui.select.cancel") {
            (self.inner.options.on_cancel)();
        }
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.inner.container.render(width)
    }
}

impl Component for FirstTimeSetupComponent {
    fn render(&self, width: usize) -> Vec<String> {
        FirstTimeSetupComponent::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        FirstTimeSetupComponent::handle_input(self, data)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        FirstTimeSetupComponent::invalidate(self)
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

#[cfg(test)]
mod tests {
    use pi_tui::{KeybindingsManager, TUI_KEYBINDINGS, set_keybindings, strip_terminal_sequences};
    use pretty_assertions::assert_eq;
    use serial_test::serial;

    use super::*;
    use crate::modes::interactive::theme::theme::init_theme;

    fn render_plain(comp: &FirstTimeSetupComponent) -> String {
        strip_terminal_sequences(&comp.render(100).join("\n"))
    }

    #[test]
    #[serial]
    fn navigates_theme_and_analytics_steps_and_submits() {
        init_theme(Some("dark"), None);
        set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));

        let previews: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let submitted: Arc<Mutex<Option<FirstTimeSetupResult>>> = Arc::new(Mutex::new(None));
        let cancelled: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));

        let previews_c = Arc::clone(&previews);
        let submitted_c = Arc::clone(&submitted);
        let cancelled_c = Arc::clone(&cancelled);

        let comp = FirstTimeSetupComponent::new(FirstTimeSetupOptions {
            on_theme_preview: Arc::new(move |t| {
                previews_c.lock().unwrap_or_else(|e| e.into_inner()).push(t.to_string());
            }),
            on_submit: Arc::new(move |res| {
                *submitted_c.lock().unwrap_or_else(|e| e.into_inner()) = Some(res);
            }),
            on_cancel: Arc::new(move || {
                *cancelled_c.lock().unwrap_or_else(|e| e.into_inner()) += 1;
            }),
        });

        let initial = render_plain(&comp);
        assert!(initial.contains("Welcome to pi, the minimal coding agent."));
        assert!(initial.contains("Pick a theme."));
        assert!(initial.contains("→ System (matches your terminal colors)"));
        assert!(initial.contains("continue"));

        comp.handle_input("j");
        assert_eq!(*previews.lock().unwrap_or_else(|e| e.into_inner()), vec!["dark"]);
        assert!(render_plain(&comp).contains("→ Dark"));

        comp.handle_input("\n");
        let step2 = render_plain(&comp);
        assert!(step2.contains("Opt-in to anonymous usage data sharing?"));
        assert!(step2.contains("→ Share anonymous usage data"));
        assert!(step2.contains("finish"));

        comp.handle_input("j");
        assert!(render_plain(&comp).contains("→ Don't share"));

        comp.handle_input("\r");
        assert_eq!(
            *submitted.lock().unwrap_or_else(|e| e.into_inner()),
            Some(FirstTimeSetupResult {
                theme: "dark".to_string(),
                share_analytics: false,
            })
        );
        assert_eq!(*cancelled.lock().unwrap_or_else(|e| e.into_inner()), 0);
    }

    #[test]
    #[serial]
    fn cancels_on_escape() {
        init_theme(Some("dark"), None);
        set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));

        let cancelled: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let cancelled_c = Arc::clone(&cancelled);
        let comp = FirstTimeSetupComponent::new(FirstTimeSetupOptions {
            on_theme_preview: Arc::new(|_| {}),
            on_submit: Arc::new(|_| {}),
            on_cancel: Arc::new(move || {
                *cancelled_c.lock().unwrap_or_else(|e| e.into_inner()) += 1;
            }),
        });

        comp.handle_input("\x1b");
        assert_eq!(*cancelled.lock().unwrap_or_else(|e| e.into_inner()), 1);
    }
}
