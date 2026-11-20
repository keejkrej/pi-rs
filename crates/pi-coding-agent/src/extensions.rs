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

/// Bridge helpers exposing interactive TUI components to built-in extensions.
#[doc(hidden)]
pub(crate) mod wire {
    use std::sync::Arc;

    use crate::core::extensions::types::Theme;
    use crate::modes::interactive::components::dynamic_border::DynamicBorder as InteractiveDynamicBorder;
    use crate::modes::interactive::components::keybinding_hints::{
        KeyTextFormatOptions, format_key_text as interactive_format_key_text,
    };
    use crate::modes::interactive::components::login_dialog::AuthUrlComponent as InteractiveAuthUrlComponent;
    pub use crate::modes::interactive::components::visual_truncate::VisualTruncateKeep;
    use crate::modes::interactive::components::visual_truncate::{
        VisualLinePreview as InteractiveVisualLinePreview, VisualLinePreviewOptions,
    };
    use crate::modes::interactive::theme::theme::CodeHighlighter as InteractiveCodeHighlighter;
    use pi_tui::components::select_list::{SelectListStyleFn, SelectListTheme};
    use pi_tui::keybindings::get_keybindings;
    use pi_tui::tui::{Component, TUI, TuiMouseEvent, TuiMouseEventResult};

    #[derive(Clone)]
    pub struct AuthUrlComponent(InteractiveAuthUrlComponent);

    impl AuthUrlComponent {
        pub fn new(tui: Arc<dyn TUI>, url: &str) -> Self {
            let request_render: Arc<dyn Fn() + Send + Sync> = Arc::new(move || tui.request_render(None));
            Self(InteractiveAuthUrlComponent::with_request_render(request_render, url))
        }

        pub async fn copy(&self) {
            self.0.copy().await;
        }

        pub(crate) async fn copy_with_override(
            &self,
            copy_override: Option<Arc<dyn Fn(&str) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>>,
        ) {
            self.0.copy_with_override(copy_override).await;
        }
    }

    impl Component for AuthUrlComponent {
        fn render(&self, width: usize) -> Vec<String> {
            self.0.render(width)
        }

        fn invalidate(&self) {
            self.0.invalidate();
        }
    }

    pub struct DynamicBorder(InteractiveDynamicBorder);

    impl DynamicBorder {
        pub fn new(color: Arc<dyn Fn(&str) -> String + Send + Sync>) -> Self {
            Self(InteractiveDynamicBorder::new(Some(color)))
        }
    }

    impl Component for DynamicBorder {
        fn render(&self, width: usize) -> Vec<String> {
            self.0.render(width)
        }

        fn handle_input(&self, data: &str) {
            self.0.handle_input(data);
        }

        fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
            self.0.handle_mouse(event)
        }

        fn invalidate(&self) {
            self.0.invalidate();
        }
    }

    pub struct VisualLinePreview(InteractiveVisualLinePreview);

    impl VisualLinePreview {
        pub fn new(
            text: impl Into<String>,
            max_visual_lines: usize,
            keep: VisualTruncateKeep,
            format_hint: Arc<dyn Fn(usize) -> String + Send + Sync>,
        ) -> Self {
            Self(InteractiveVisualLinePreview::new(VisualLinePreviewOptions {
                text: text.into(),
                max_visual_lines,
                keep,
                format_hint,
            }))
        }
    }

    impl Component for VisualLinePreview {
        fn render(&self, width: usize) -> Vec<String> {
            self.0.render(width)
        }

        fn handle_input(&self, data: &str) {
            self.0.handle_input(data);
        }

        fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
            self.0.handle_mouse(event)
        }

        fn invalidate(&self) {
            self.0.invalidate();
        }
    }

    pub fn format_key_text(key: &str) -> String {
        interactive_format_key_text(key, KeyTextFormatOptions::default())
    }

    pub fn key_hint(keybinding: &str, description: &str, theme: &Theme) -> String {
        let keys = get_keybindings().get_keys(keybinding);
        let key_text = if keys.is_empty() {
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

    pub fn highlight_code(code: &str, lang: Option<&str>, theme: &Theme) -> Vec<String> {
        if crate::modes::interactive::theme::theme::try_theme().is_ok() {
            crate::modes::interactive::theme::theme::highlight_code(code, lang)
        } else {
            code.split('\n').map(|line| theme.fg("mdCodeBlock", line)).collect()
        }
    }

    #[derive(Clone, Default)]
    pub struct CodeHighlighter(InteractiveCodeHighlighter);

    impl CodeHighlighter {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn highlight(&self, code: &str, lang: Option<&str>, theme: &Theme) -> Vec<String> {
            if crate::modes::interactive::theme::theme::try_theme().is_ok() {
                self.0.highlight(code, lang).as_ref().clone()
            } else {
                code.split('\n').map(|line| theme.fg("mdCodeBlock", line)).collect()
            }
        }
    }

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
