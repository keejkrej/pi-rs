//! WIRE(@w3-b): Temporary local adapter mirroring interactive TUI helpers
//! (`keybinding-hints`, `diff`, `visual-truncate`, `theme`) until Stage 3B lands.

use std::sync::Arc;

use pi_tui::Text;

use crate::core::extensions::types::Theme;
use crate::core::tools::render_utils::replace_tabs;

/// Format a tool action keybinding hint.
/// Mirrors `keyHint(action, description)` from `modes/interactive/components/keybinding-hints.ts`.
pub fn key_hint(action: &str, description: &str) -> String {
    let key = default_action_key(action);
    format!("{key} {description}")
}

/// Format an action key name.
pub fn key_text(action: &str) -> String {
    default_action_key(action).to_string()
}

fn default_action_key(action: &str) -> &'static str {
    match action {
        "expandTools" => "ctrl+o",
        _ => "ctrl+o",
    }
}

/// Language detection from a file path for code highlighting.
pub fn get_language_from_path(file_path: &str) -> Option<&'static str> {
    let ext = file_path.rsplit('.').next()?.to_ascii_lowercase();
    let lang = match ext.as_str() {
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "jsx",
        "rs" => "rust",
        "py" => "python",
        "rb" => "ruby",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" => "cpp",
        "cs" => "csharp",
        "sh" | "bash" | "zsh" => "bash",
        "ps1" | "psm1" => "powershell",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "md" | "markdown" => "markdown",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" => "scss",
        "sql" => "sql",
        "xml" | "svg" => "xml",
        _ => return None,
    };
    Some(lang)
}

/// Highlight source code text line by line.
/// Plain-text tab-safe pass until `theme::highlight_code` lands in Stage 3B.
pub fn highlight_code(code: &str, _lang: Option<&str>) -> Vec<String> {
    code.split('\n').map(replace_tabs).collect()
}

/// Minimal stateful highlighter placeholder matching `CodeHighlighter` in `theme.ts`.
pub struct CodeHighlighter {
    _lang: Option<String>,
}

impl CodeHighlighter {
    pub fn new(lang: Option<&str>) -> Self {
        Self {
            _lang: lang.map(ToString::to_string),
        }
    }

    pub fn highlight_line(&mut self, line: &str) -> String {
        replace_tabs(line)
    }
}

/// Render a unified diff string with theme colors.
/// Mirrors `renderDiff(diffText, _options, theme)` in `modes/interactive/components/diff.ts`.
pub fn render_diff(diff_text: &str, theme: Option<&Theme>) -> String {
    let lines: Vec<&str> = diff_text.split('\n').collect();
    let mut rendered = Vec::with_capacity(lines.len());
    for line in lines {
        let formatted = if line.starts_with('+') {
            theme
                .map(|t| t.fg("toolDiffAdded", line))
                .unwrap_or_else(|| line.to_string())
        } else if line.starts_with('-') {
            theme
                .map(|t| t.fg("toolDiffRemoved", line))
                .unwrap_or_else(|| line.to_string())
        } else {
            theme
                .map(|t| t.fg("toolDiffContext", line))
                .unwrap_or_else(|| line.to_string())
        };
        rendered.push(formatted);
    }
    rendered.join("\n")
}

/// Create a `Text` component representing a fixed-max visual-line preview.
pub fn build_visual_line_preview(lines: &[String], max_lines: usize, max_width: Option<usize>) -> Text {
    let mut visible: Vec<String> = lines.iter().take(max_lines).cloned().collect();
    if let Some(w) = max_width {
        for line in &mut visible {
            if line.chars().count() > w {
                let mut truncated: String = line.chars().take(w.saturating_sub(1)).collect();
                truncated.push('…');
                *line = truncated;
            }
        }
    }
    let text = visible.join("\n");
    Text::new(Some(&text), Some(0), Some(0), None)
}

pub fn new_shared_text(text: &str, px: i64, py: i64) -> (Text, Arc<dyn pi_tui::Component>) {
    let widget = Text::new(Some(text), Some(px), Some(py), None);
    let comp: Arc<dyn pi_tui::Component> = Arc::new(widget.clone());
    (widget, comp)
}
