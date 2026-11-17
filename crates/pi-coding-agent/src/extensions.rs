//! Port of packages/coding-agent/src/extensions/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod codemode;
pub mod llama;
pub mod mcp;
pub mod tool_search;
// @generated-mods end

use crate::core::extensions::types::{InlineExtension, NamedInlineExtension};

/// Built-in inline extensions loaded alongside user and workspace extensions.
pub fn built_in_extensions() -> Vec<InlineExtension> {
    vec![
        InlineExtension::Named(NamedInlineExtension {
            name: "llama.cpp".to_string(),
            factory: llama::default_export(),
            hidden: false,
            replaceable: false,
            builtin: true,
        }),
        // Replaceable: an extension that registers `codemode`, `tool_search`, or `/mcp` (such as
        // a third-party MCP extension) takes over instead of running alongside the built-in one.
        InlineExtension::Named(NamedInlineExtension {
            name: "codemode".to_string(),
            factory: codemode::default_export(),
            hidden: false,
            replaceable: true,
            builtin: true,
        }),
        InlineExtension::Named(NamedInlineExtension {
            name: "tool-search".to_string(),
            factory: tool_search::default_export(),
            hidden: false,
            replaceable: true,
            builtin: true,
        }),
        InlineExtension::Named(NamedInlineExtension {
            name: "mcp".to_string(),
            factory: mcp::default_export(),
            hidden: false,
            replaceable: true,
            builtin: true,
        }),
    ]
}

/// Adapters for Stage 3B interactive/TUI components not yet landed in this workspace.
#[doc(hidden)]
pub(crate) mod wire {
    use std::sync::{Arc, Mutex};

    use crate::core::extensions::types::Theme;
    use pi_tui::components::select_list::{SelectListStyleFn, SelectListTheme};
    use pi_tui::components::text::Text;
    use pi_tui::keybindings::get_keybindings;
    use pi_tui::tui::{Component, TuiMouseEvent, TuiMouseEventResult};
    use pi_tui::utils::truncate_to_width;

    // WIRE(@w3-b): wire to crate::modes::interactive::components::DynamicBorder once landed
    pub struct DynamicBorder {
        color: Arc<dyn Fn(&str) -> String + Send + Sync>,
    }

    impl DynamicBorder {
        pub fn new(color: Arc<dyn Fn(&str) -> String + Send + Sync>) -> Self {
            Self { color }
        }
    }

    impl Component for DynamicBorder {
        fn render(&self, width: usize) -> Vec<String> {
            vec![(self.color)(&"─".repeat(width.max(1)))]
        }

        fn handle_input(&self, _data: &str) {}

        fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
            None
        }

        fn invalidate(&self) {}
    }

    // WIRE(@w3-b): wire to crate::modes::interactive::components::visual_truncate once landed
    #[allow(dead_code)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum VisualTruncateKeep {
        Start,
        End,
    }

    pub struct VisualTruncateResult {
        pub visual_lines: Vec<String>,
        pub skipped_count: usize,
    }

    pub fn truncate_to_visual_lines(
        text: &str,
        max_visual_lines: usize,
        width: usize,
        padding_x: i64,
        keep: VisualTruncateKeep,
    ) -> VisualTruncateResult {
        if text.is_empty() {
            return VisualTruncateResult {
                visual_lines: Vec::new(),
                skipped_count: 0,
            };
        }
        let temp_text = Text::new(Some(text), Some(padding_x), Some(0), None);
        let all_visual_lines = temp_text.render(width);
        if all_visual_lines.len() <= max_visual_lines {
            return VisualTruncateResult {
                visual_lines: all_visual_lines,
                skipped_count: 0,
            };
        }
        let skipped_count = all_visual_lines.len() - max_visual_lines;
        let truncated_lines = match keep {
            VisualTruncateKeep::Start => all_visual_lines[..max_visual_lines].to_vec(),
            VisualTruncateKeep::End => all_visual_lines[all_visual_lines.len() - max_visual_lines..].to_vec(),
        };
        VisualTruncateResult {
            visual_lines: truncated_lines,
            skipped_count,
        }
    }

    struct VisualLinePreviewState {
        cached_width: Option<usize>,
        cached_lines: Option<Vec<String>>,
    }

    pub struct VisualLinePreview {
        text: String,
        max_visual_lines: usize,
        keep: VisualTruncateKeep,
        format_hint: Arc<dyn Fn(usize) -> String + Send + Sync>,
        state: Mutex<VisualLinePreviewState>,
    }

    impl VisualLinePreview {
        pub fn new(
            text: impl Into<String>,
            max_visual_lines: usize,
            keep: VisualTruncateKeep,
            format_hint: Arc<dyn Fn(usize) -> String + Send + Sync>,
        ) -> Self {
            Self {
                text: text.into(),
                max_visual_lines,
                keep,
                format_hint,
                state: Mutex::new(VisualLinePreviewState {
                    cached_width: None,
                    cached_lines: None,
                }),
            }
        }
    }

    impl Component for VisualLinePreview {
        fn render(&self, width: usize) -> Vec<String> {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if st.cached_lines.is_none() || st.cached_width != Some(width) {
                let preview = truncate_to_visual_lines(&self.text, self.max_visual_lines, width, 0, self.keep);
                let mut lines = preview.visual_lines;
                if preview.skipped_count > 0 {
                    let hint = truncate_to_width(
                        &(self.format_hint)(preview.skipped_count),
                        width as i64,
                        Some("..."),
                        None,
                    );
                    match self.keep {
                        VisualTruncateKeep::Start => lines.push(hint),
                        VisualTruncateKeep::End => lines.insert(0, hint),
                    }
                }
                st.cached_lines = Some(lines);
                st.cached_width = Some(width);
            }
            st.cached_lines.clone().unwrap_or_default()
        }

        fn handle_input(&self, _data: &str) {}

        fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
            None
        }

        fn invalidate(&self) {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.cached_width = None;
            st.cached_lines = None;
        }
    }

    // WIRE(@w3-b): wire to crate::modes::interactive::components::keybinding_hints once landed
    fn format_key_part(part: &str) -> String {
        if pi_js::env::platform() == "darwin" && part.eq_ignore_ascii_case("alt") {
            "option".to_string()
        } else {
            part.to_string()
        }
    }

    pub fn format_key_text(key: &str) -> String {
        key.split('/')
            .map(|k| k.split('+').map(format_key_part).collect::<Vec<_>>().join("+"))
            .collect::<Vec<_>>()
            .join("/")
    }

    pub fn key_hint(keybinding: &str, description: &str, theme: &Theme) -> String {
        let keys = get_keybindings().get_keys(keybinding);
        let key_text = if keys.is_empty() {
            // Fallback before coding-agent initializes app keybindings in headless tests.
            match keybinding {
                "app.tools.expand" => "ctrl+o".to_string(),
                _ => String::new(),
            }
        } else {
            format_key_text(&keys.join("/"))
        };
        format!(
            "{}{}",
            theme.fg("dim", &key_text),
            theme.fg("muted", &format!(" {description}"))
        )
    }

    // WIRE(@w3-b): wire to crate::modes::interactive::theme::highlight_code once landed
    pub fn highlight_code(code: &str, _lang: Option<&str>, theme: &Theme) -> Vec<String> {
        code.split('\n').map(|line| theme.fg("mdCodeBlock", line)).collect()
    }

    // WIRE(@w3-b): wire to crate::modes::interactive::theme::get_select_list_theme once landed
    pub fn get_select_list_theme(theme: &Theme) -> SelectListTheme {
        let t_prefix = theme.clone();
        let t_selected = theme.clone();
        let t_desc = theme.clone();
        let t_scroll = theme.clone();
        let t_none = theme.clone();
        let selected_prefix: SelectListStyleFn = Arc::new(move |s| t_prefix.fg("accent", s));
        let selected_text: SelectListStyleFn = Arc::new(move |s| t_selected.fg("accent", s));
        let description: SelectListStyleFn = Arc::new(move |s| t_desc.fg("muted", s));
        let scroll_info: SelectListStyleFn = Arc::new(move |s| t_scroll.fg("muted", s));
        let no_match: SelectListStyleFn = Arc::new(move |s| t_none.fg("muted", s));
        SelectListTheme {
            selected_prefix,
            selected_text,
            description,
            scroll_info,
            no_match,
        }
    }
}
