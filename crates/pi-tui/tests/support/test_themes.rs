//! Port of packages/tui/test/test-themes.ts

#![allow(dead_code)]

use std::sync::Arc;

use pi_tui::components::editor::EditorTheme;
use pi_tui::components::markdown::MarkdownTheme;
use pi_tui::components::select_list::SelectListTheme;

fn chalk_wrap(open: &str, close: &str, text: &str) -> String {
    if text.is_empty() {
        String::new()
    } else {
        format!("{open}{text}{close}")
    }
}

pub fn default_select_list_theme() -> SelectListTheme {
    SelectListTheme {
        selected_prefix: Arc::new(|text| chalk_wrap("\x1b[34m", "\x1b[39m", text)),
        selected_text: Arc::new(|text| chalk_wrap("\x1b[1m", "\x1b[22m", text)),
        description: Arc::new(|text| chalk_wrap("\x1b[2m", "\x1b[22m", text)),
        scroll_info: Arc::new(|text| chalk_wrap("\x1b[2m", "\x1b[22m", text)),
        no_match: Arc::new(|text| chalk_wrap("\x1b[2m", "\x1b[22m", text)),
    }
}

pub fn default_markdown_theme() -> MarkdownTheme {
    MarkdownTheme {
        heading: Arc::new(|text| chalk_wrap("\x1b[1m\x1b[36m", "\x1b[39m\x1b[22m", text)),
        link: Arc::new(|text| chalk_wrap("\x1b[34m", "\x1b[39m", text)),
        link_url: Arc::new(|text| chalk_wrap("\x1b[2m", "\x1b[22m", text)),
        code: Arc::new(|text| chalk_wrap("\x1b[33m", "\x1b[39m", text)),
        code_block: Arc::new(|text| chalk_wrap("\x1b[32m", "\x1b[39m", text)),
        code_block_border: Arc::new(|text| chalk_wrap("\x1b[2m", "\x1b[22m", text)),
        quote: Arc::new(|text| chalk_wrap("\x1b[3m", "\x1b[23m", text)),
        quote_border: Arc::new(|text| chalk_wrap("\x1b[2m", "\x1b[22m", text)),
        hr: Arc::new(|text| chalk_wrap("\x1b[2m", "\x1b[22m", text)),
        list_bullet: Arc::new(|text| chalk_wrap("\x1b[36m", "\x1b[39m", text)),
        bold: Arc::new(|text| chalk_wrap("\x1b[1m", "\x1b[22m", text)),
        italic: Arc::new(|text| chalk_wrap("\x1b[3m", "\x1b[23m", text)),
        strikethrough: Arc::new(|text| chalk_wrap("\x1b[9m", "\x1b[29m", text)),
        underline: Arc::new(|text| chalk_wrap("\x1b[4m", "\x1b[24m", text)),
        highlight_code: None,
        code_block_indent: None,
    }
}

pub fn default_editor_theme() -> EditorTheme {
    EditorTheme {
        border_color: Arc::new(|text| chalk_wrap("\x1b[2m", "\x1b[22m", text)),
        select_list: default_select_list_theme(),
    }
}
