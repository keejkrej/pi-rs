//! Port of packages/coding-agent/src/modes/interactive/components/keybinding-hints.ts

use pi_tui::{KeyId, Keybinding, KeybindingsManager, get_keybindings};

use super::super::theme::theme::{ThemeColor, theme};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyTextFormatOptions {
    pub capitalize: bool,
}

fn format_key_part(part: &str, options: KeyTextFormatOptions) -> String {
    let display_part = if pi_js::env::platform() == "darwin" && part.eq_ignore_ascii_case("alt") {
        "option"
    } else {
        part
    };
    if options.capitalize {
        let mut chars = display_part.chars();
        match chars.next() {
            Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
            None => String::new(),
        }
    } else {
        display_part.to_string()
    }
}

pub fn format_key_text(key: &str, options: KeyTextFormatOptions) -> String {
    key.split('/')
        .map(|k| {
            k.split('+')
                .map(|part| format_key_part(part, options))
                .collect::<Vec<_>>()
                .join("+")
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn format_keys(keys: &[KeyId], options: KeyTextFormatOptions) -> String {
    if keys.is_empty() {
        return String::new();
    }
    format_key_text(&keys.join("/"), options)
}

pub fn key_text(keybinding: &Keybinding) -> String {
    format_keys(&get_keybindings().get_keys(keybinding), KeyTextFormatOptions::default())
}

pub fn key_display_text(keybinding: &Keybinding) -> String {
    format_keys(
        &get_keybindings().get_keys(keybinding),
        KeyTextFormatOptions { capitalize: true },
    )
}

pub fn key_hint(keybinding: &Keybinding, description: &str) -> String {
    let t = theme();
    format!(
        "{}{}",
        t.fg(ThemeColor::Dim, &key_text(keybinding)),
        t.fg(ThemeColor::Muted, &format!(" {description}"))
    )
}

pub fn raw_key_hint(key: &str, description: &str) -> String {
    let t = theme();
    format!(
        "{}{}",
        t.fg(ThemeColor::Dim, &format_key_text(key, KeyTextFormatOptions::default())),
        t.fg(ThemeColor::Muted, &format!(" {description}"))
    )
}

/// Shared page-up / page-down index calculation matching `getSelectPageIndex` in `@earendil-works/pi-tui`.
pub(crate) fn get_select_page_index(
    kb: &KeybindingsManager,
    data: &str,
    selected_index: usize,
    item_count: usize,
    page_size: usize,
) -> Option<usize> {
    if item_count == 0 {
        return None;
    }
    let direction: isize = if kb.matches(data, "tui.select.pageUp") {
        -1
    } else if kb.matches(data, "tui.select.pageDown") {
        1
    } else {
        return None;
    };
    let step = page_size.max(1) as isize;
    let max_idx = (item_count - 1) as isize;
    let next = ((selected_index as isize) + direction * step).clamp(0, max_idx);
    Some(next as usize)
}
