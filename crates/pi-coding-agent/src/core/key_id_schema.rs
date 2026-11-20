//! Port of packages/coding-agent/src/core/key-id-schema.ts

use std::sync::LazyLock;

use pi_schema::{Schema, StrOptions, arr, str_with, union};
use pi_tui::Key;

const MODIFIERS: &[&str] = &["ctrl", "shift", "alt", "super"];

fn all_key_values() -> Vec<&'static str> {
    vec![
        Key::escape,
        Key::esc,
        Key::enter,
        Key::r#return,
        Key::tab,
        Key::space,
        Key::backspace,
        Key::delete,
        Key::insert,
        Key::clear,
        Key::home,
        Key::end,
        Key::page_up,
        Key::page_down,
        Key::up,
        Key::down,
        Key::left,
        Key::right,
        Key::f1,
        Key::f2,
        Key::f3,
        Key::f4,
        Key::f5,
        Key::f6,
        Key::f7,
        Key::f8,
        Key::f9,
        Key::f10,
        Key::f11,
        Key::f12,
        Key::backtick,
        Key::hyphen,
        Key::equals,
        Key::leftbracket,
        Key::rightbracket,
        Key::backslash,
        Key::semicolon,
        Key::quote,
        Key::comma,
        Key::period,
        Key::slash,
        Key::exclamation,
        Key::at,
        Key::hash,
        Key::dollar,
        Key::percent,
        Key::caret,
        Key::ampersand,
        Key::asterisk,
        Key::leftparen,
        Key::rightparen,
        Key::underscore,
        Key::plus,
        Key::pipe,
        Key::tilde,
        Key::leftbrace,
        Key::rightbrace,
        Key::colon,
        Key::lessthan,
        Key::greaterthan,
        Key::question,
    ]
}

fn escape_regex_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for ch in s.chars() {
        if matches!(
            ch,
            '\\' | '^' | '$' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

static KEY_ID_PATTERN: LazyLock<String> = LazyLock::new(|| {
    let key_alts: Vec<String> = all_key_values().into_iter().map(escape_regex_literal).collect();
    let base_key_pattern = format!("(?:[a-z0-9]|{})", key_alts.join("|"));
    let duplicate_modifier_pattern = MODIFIERS
        .iter()
        .map(|m| format!("{m}\\+.*{m}\\+"))
        .collect::<Vec<_>>()
        .join("|");
    let modifier_alts = MODIFIERS.join("|");
    format!("^(?!.*(?:{duplicate_modifier_pattern}))(?:(?:{modifier_alts})\\+){{0,4}}{base_key_pattern}$")
});

pub fn key_id_schema() -> Schema {
    str_with(StrOptions {
        annotations: pi_schema::Annotations {
            description: Some("Key identifier with optional ctrl, shift, alt, or super modifiers.".to_owned()),
            ..Default::default()
        },
        pattern: Some(KEY_ID_PATTERN.clone()),
        ..Default::default()
    })
}

pub fn keybinding_value_schema_with(description: Option<&str>) -> Schema {
    let schema = union([key_id_schema(), arr(key_id_schema())]);
    if let Some(desc) = description {
        schema.description(desc)
    } else {
        schema
    }
}

pub fn keybinding_value_schema() -> Schema {
    keybinding_value_schema_with(None)
}
