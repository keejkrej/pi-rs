//! Port of packages/coding-agent/examples/extensions/overlay-test.ts

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    DoneFn, Extension, ExtensionApi, ExtensionCustomOptions, ExtensionFactory, InlineExtension, NotifyType,
    RegisteredCommandOptions, Theme,
};
use pi_tui::{CURSOR_MARKER, Component, Focusable, Key, matches_key, render_fake_cursor, visible_width};
use serde_json::{Value, json};

struct MenuItem {
    label: &'static str,
    has_input: bool,
    text: Vec<char>,
    cursor: usize,
}

struct OverlayTestState {
    selected: usize,
    items: Vec<MenuItem>,
}

struct OverlayTestComponent {
    width: usize,
    focused: AtomicBool,
    state: Mutex<OverlayTestState>,
    theme: Theme,
    done: DoneFn,
}

impl OverlayTestComponent {
    fn new(theme: Theme, done: DoneFn) -> Self {
        Self {
            width: 70,
            focused: AtomicBool::new(false),
            state: Mutex::new(OverlayTestState {
                selected: 0,
                items: vec![
                    MenuItem {
                        label: "Search",
                        has_input: true,
                        text: Vec::new(),
                        cursor: 0,
                    },
                    MenuItem {
                        label: "Run",
                        has_input: true,
                        text: Vec::new(),
                        cursor: 0,
                    },
                    MenuItem {
                        label: "Settings",
                        has_input: false,
                        text: Vec::new(),
                        cursor: 0,
                    },
                    MenuItem {
                        label: "Cancel",
                        has_input: false,
                        text: Vec::new(),
                        cursor: 0,
                    },
                ],
            }),
            theme,
            done,
        }
    }
}

impl Focusable for OverlayTestComponent {
    fn focused(&self) -> bool {
        self.focused.load(Ordering::Relaxed)
    }

    fn set_focused(&self, focused: bool) {
        self.focused.store(focused, Ordering::Relaxed);
    }
}

impl Component for OverlayTestComponent {
    fn has_handle_input(&self) -> bool {
        true
    }

    fn as_focusable(&self) -> Option<&dyn Focusable> {
        Some(self)
    }

    fn handle_input(&self, data: &str) {
        if matches_key(data, Key::escape) {
            (self.done)(Value::Null);
            return;
        }

        let submit_val = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let selected = state.selected;
            let items_len = state.items.len();

            if matches_key(data, Key::r#return) {
                let current = &state.items[selected];
                if current.has_input {
                    let query: String = current.text.iter().collect();
                    Some(json!({ "action": current.label, "query": query }))
                } else {
                    Some(json!({ "action": current.label }))
                }
            } else {
                if matches_key(data, Key::up) {
                    state.selected = selected.saturating_sub(1);
                } else if matches_key(data, Key::down) {
                    state.selected = (selected + 1).min(items_len.saturating_sub(1));
                } else if let Some(current) = state.items.get_mut(selected)
                    && current.has_input
                {
                    if matches_key(data, Key::backspace) {
                        if current.cursor > 0 {
                            current.cursor -= 1;
                            current.text.drain(current.cursor..current.cursor + 1);
                        }
                    } else if matches_key(data, Key::left) {
                        current.cursor = current.cursor.saturating_sub(1);
                    } else if matches_key(data, Key::right) {
                        current.cursor = (current.cursor + 1).min(current.text.len());
                    } else {
                        let mut chars = data.chars();
                        if let (Some(ch), None) = (chars.next(), chars.next())
                            && (ch as u32) >= 32
                        {
                            current.text.insert(current.cursor, ch);
                            current.cursor += 1;
                        }
                    }
                }
                None
            }
        };

        if let Some(val) = submit_val {
            (self.done)(val);
        }
    }

    fn render(&self, _width: usize) -> Vec<String> {
        let th = &self.theme;
        let inner_w = self.width - 2;
        let pad = |s: &str, len: usize| -> String {
            let vis = visible_width(s).max(0) as usize;
            let spaces = len.saturating_sub(vis);
            format!("{s}{}", " ".repeat(spaces))
        };
        let row = |content: &str| -> String {
            format!(
                "{}{}{}",
                th.fg("border", "│"),
                pad(content, inner_w),
                th.fg("border", "│")
            )
        };

        let mut lines = Vec::new();
        lines.push(th.fg("border", &format!("╭{}╮", "─".repeat(inner_w))));
        lines.push(row(&format!(" {}", th.fg("accent", "🧪 Overlay Test"))));
        lines.push(row(""));

        lines.push(row(&format!(
            " {}",
            th.fg("dim", "─── Edge Cases (borders should align) ───")
        )));
        lines.push(row(&format!(
            " Wide: {}",
            th.fg(
                "warning",
                "中文日本語한글テスト漢字繁體简体ひらがなカタカナ가나다라마바"
            )
        )));
        lines.push(row(&format!(
            " Styled: {} {} {} {} {} {} {}",
            th.fg("error", "RED"),
            th.fg("success", "GREEN"),
            th.fg("warning", "YELLOW"),
            th.fg("accent", "ACCENT"),
            th.fg("dim", "DIM"),
            th.fg("error", "more"),
            th.fg("success", "colors")
        )));
        lines.push(row(
            " Emoji: 👨‍👩‍👧‍👦 🇯🇵 🚀 💻 🎉 🔥 😀 🎯 🌟 💡 🎨 🔧 📦 🏆 🌈 🎪 🎭 🎬 🎮 🎲",
        ));
        lines.push(row(""));

        lines.push(row(&format!(" {}", th.fg("dim", "─── Actions ───"))));

        let is_focused = self.focused.load(Ordering::Relaxed);
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        for (i, item) in state.items.iter().enumerate() {
            let is_selected = i == state.selected;
            let prefix = if is_selected { " ▶ " } else { "   " };

            let content = if item.has_input {
                let label_text = format!("{}:", item.label);
                let label = if is_selected {
                    th.fg("accent", &label_text)
                } else {
                    th.fg("text", &label_text)
                };

                let input_display = if is_selected {
                    let before: String = item.text[..item.cursor].iter().collect();
                    let cursor_ch = item.text.get(item.cursor).copied().unwrap_or(' ');
                    let after: String = if item.cursor < item.text.len() {
                        item.text[item.cursor + 1..].iter().collect()
                    } else {
                        String::new()
                    };
                    let marker = if is_focused { CURSOR_MARKER } else { "" };
                    format!("{before}{marker}{}{after}", render_fake_cursor(&cursor_ch.to_string()))
                } else {
                    item.text.iter().collect()
                };
                format!("{prefix}{label} {input_display}")
            } else {
                let lbl = if is_selected {
                    th.fg("accent", item.label)
                } else {
                    th.fg("text", item.label)
                };
                format!("{prefix}{lbl}")
            };

            lines.push(row(&content));
        }
        drop(state);

        lines.push(row(""));
        lines.push(row(&format!(
            " {}",
            th.fg("dim", "↑↓ navigate • type to input • Enter select • Esc cancel")
        )));
        lines.push(th.fg("border", &format!("╰{}╯", "─".repeat(inner_w))));

        lines
    }

    fn invalidate(&self) {}
}

pub struct OverlayTestExtension;

#[async_trait]
impl Extension for OverlayTestExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.register_command(
            "overlay-test",
            RegisteredCommandOptions {
                description: Some("Test overlay rendering with edge cases".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| {
                    Box::pin(async move {
                        let base = ctx.as_context();
                        let ui = base.ui()?;
                        let result = ui
                            .custom(
                                Box::new(|_tui, theme, _kb, done| {
                                    Box::pin(async move {
                                        Ok(Arc::new(OverlayTestComponent::new(theme, done)) as Arc<dyn Component>)
                                    })
                                }),
                                Some(ExtensionCustomOptions {
                                    overlay: Some(true),
                                    ..Default::default()
                                }),
                            )
                            .await?;

                        if let Value::Object(map) = result
                            && let Some(action) = map.get("action").and_then(|v| v.as_str())
                        {
                            let query = map.get("query").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
                            let msg = match query {
                                Some(q) => format!("{action}: \"{q}\""),
                                None => action.to_string(),
                            };
                            ui.notify(&msg, Some(NotifyType::Info));
                        }

                        Ok(())
                    })
                }),
            },
        )?;

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(OverlayTestExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
