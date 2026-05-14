use std::fs;
use std::path::{Path, PathBuf};

use crate::fuzzy::fuzzy_filter;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutocompleteItem {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

impl AutocompleteItem {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            description: None,
        }
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AutocompleteSuggestions {
    pub items: Vec<AutocompleteItem>,
    pub prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: String,
    pub description: Option<String>,
    pub argument_hint: Option<String>,
}

impl SlashCommand {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            argument_hint: None,
        }
    }
}

pub trait AutocompleteProvider {
    fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        force: bool,
    ) -> Option<AutocompleteSuggestions>;

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> (Vec<String>, usize, usize) {
        apply_completion(lines, cursor_line, cursor_col, item, prefix)
    }

    fn should_trigger_file_completion(
        &self,
        _lines: &[String],
        _cursor_line: usize,
        _cursor_col: usize,
    ) -> bool {
        true
    }
}

#[derive(Debug, Clone)]
pub struct CombinedAutocompleteProvider {
    commands: Vec<SlashCommand>,
    base_path: PathBuf,
}

impl CombinedAutocompleteProvider {
    pub fn new(commands: Vec<SlashCommand>, base_path: impl Into<PathBuf>) -> Self {
        Self {
            commands,
            base_path: base_path.into(),
        }
    }

    fn slash_suggestions(&self, text_before_cursor: &str) -> Option<AutocompleteSuggestions> {
        if !text_before_cursor.starts_with('/') || text_before_cursor.contains(' ') {
            return None;
        }
        let prefix = text_before_cursor.strip_prefix('/').unwrap_or_default();
        let command_items = self
            .commands
            .iter()
            .map(|cmd| AutocompleteItem {
                value: cmd.name.clone(),
                label: cmd.name.clone(),
                description: cmd.description.clone(),
            })
            .collect::<Vec<_>>();
        let items = fuzzy_filter(command_items, prefix, |item| item.value.clone());
        (!items.is_empty()).then(|| AutocompleteSuggestions {
            items,
            prefix: text_before_cursor.to_string(),
        })
    }

    fn extract_path_prefix(text: &str, force: bool) -> Option<String> {
        if let Some(quote_start) = find_unclosed_quote_start(text) {
            let quote_prefix = if quote_start > 0 && text[..quote_start].ends_with('@') {
                &text[quote_start - 1..]
            } else {
                &text[quote_start..]
            };
            return Some(quote_prefix.to_string());
        }

        let delimiter = text
            .char_indices()
            .rev()
            .find(|(_, ch)| matches!(ch, ' ' | '\t' | '\'' | '='))
            .map(|(idx, ch)| idx + ch.len_utf8());
        let prefix = delimiter.map_or(text, |idx| &text[idx..]);
        if force
            || prefix.starts_with('@')
            || prefix.contains('/')
            || prefix.starts_with('.')
            || prefix.starts_with("~/")
            || (prefix.is_empty() && text.ends_with(' '))
        {
            Some(prefix.to_string())
        } else {
            None
        }
    }

    fn file_suggestions(&self, prefix: &str) -> Vec<AutocompleteItem> {
        let parsed = ParsedPrefix::new(prefix);
        let raw_prefix = parsed.raw.as_str();
        let path = expand_home(raw_prefix);
        let (search_dir, search_prefix, display_dir) =
            if raw_prefix.is_empty() || raw_prefix.ends_with('/') {
                let dir = if path.as_os_str().is_empty() {
                    self.base_path.clone()
                } else if path.is_absolute() {
                    path
                } else {
                    self.base_path.join(&path)
                };
                (dir, String::new(), raw_prefix.to_string())
            } else {
                let file = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string();
                let parent = path.parent().unwrap_or_else(|| Path::new(""));
                let dir = if parent.as_os_str().is_empty() {
                    self.base_path.clone()
                } else if parent.is_absolute() {
                    parent.to_path_buf()
                } else {
                    self.base_path.join(parent)
                };
                let display_dir = raw_prefix
                    .rsplit_once('/')
                    .map(|(dir, _)| format!("{dir}/"))
                    .unwrap_or_default();
                (dir, file, display_dir)
            };

        let mut suggestions = Vec::new();
        let Ok(entries) = fs::read_dir(search_dir) else {
            return suggestions;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name
                .to_lowercase()
                .starts_with(&search_prefix.to_lowercase())
            {
                continue;
            }
            let is_dir = entry
                .file_type()
                .is_ok_and(|ty| ty.is_dir() || ty.is_symlink() && entry.path().is_dir());
            let path_value = format!("{display_dir}{name}{}", if is_dir { "/" } else { "" });
            let value = parsed.completion_value(&path_value);
            suggestions.push(AutocompleteItem::new(
                value,
                format!("{name}{}", if is_dir { "/" } else { "" }),
            ));
        }
        suggestions.sort_by(|a, b| {
            let a_dir = a.label.ends_with('/');
            let b_dir = b.label.ends_with('/');
            b_dir.cmp(&a_dir).then_with(|| a.label.cmp(&b.label))
        });
        suggestions
    }
}

impl AutocompleteProvider for CombinedAutocompleteProvider {
    fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        force: bool,
    ) -> Option<AutocompleteSuggestions> {
        let current_line = lines.get(cursor_line).map_or("", String::as_str);
        let text_before_cursor = &current_line[..cursor_col.min(current_line.len())];
        if let Some(suggestions) = self.slash_suggestions(text_before_cursor) {
            return Some(suggestions);
        }
        let prefix = Self::extract_path_prefix(text_before_cursor, force)?;
        let items = self.file_suggestions(&prefix);
        (!items.is_empty()).then_some(AutocompleteSuggestions { items, prefix })
    }

    fn should_trigger_file_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
    ) -> bool {
        let current_line = lines.get(cursor_line).map_or("", String::as_str);
        let text_before_cursor = &current_line[..cursor_col.min(current_line.len())];
        !(text_before_cursor.trim().starts_with('/') && !text_before_cursor.trim().contains(' '))
    }
}

#[derive(Debug, Clone)]
struct ParsedPrefix {
    raw: String,
    is_at: bool,
    is_quoted: bool,
}

impl ParsedPrefix {
    fn new(prefix: &str) -> Self {
        if let Some(raw) = prefix.strip_prefix("@\"") {
            Self {
                raw: raw.to_string(),
                is_at: true,
                is_quoted: true,
            }
        } else if let Some(raw) = prefix.strip_prefix('"') {
            Self {
                raw: raw.to_string(),
                is_at: false,
                is_quoted: true,
            }
        } else if let Some(raw) = prefix.strip_prefix('@') {
            Self {
                raw: raw.to_string(),
                is_at: true,
                is_quoted: false,
            }
        } else {
            Self {
                raw: prefix.to_string(),
                is_at: false,
                is_quoted: false,
            }
        }
    }

    fn completion_value(&self, path_value: &str) -> String {
        let needs_quotes = self.is_quoted || path_value.contains(' ');
        let at = if self.is_at { "@" } else { "" };
        if needs_quotes {
            format!("{at}\"{path_value}\"")
        } else {
            format!("{at}{path_value}")
        }
    }
}

fn find_unclosed_quote_start(text: &str) -> Option<usize> {
    let mut quote_start = None;
    for (idx, ch) in text.char_indices() {
        if ch == '"' {
            quote_start = if quote_start.is_some() {
                None
            } else {
                Some(idx)
            };
        }
    }
    quote_start
}

fn expand_home(path: &str) -> PathBuf {
    if path == "~" {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(path))
    } else if let Some(rest) = path.strip_prefix("~/") {
        std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(rest))
            .unwrap_or_else(|| PathBuf::from(path))
    } else {
        PathBuf::from(path)
    }
}

pub fn apply_completion(
    lines: &[String],
    cursor_line: usize,
    cursor_col: usize,
    item: &AutocompleteItem,
    prefix: &str,
) -> (Vec<String>, usize, usize) {
    let mut new_lines = lines.to_vec();
    let current_line = new_lines.get(cursor_line).cloned().unwrap_or_default();
    let prefix_start = cursor_col
        .saturating_sub(prefix.len())
        .min(current_line.len());
    let before_prefix = &current_line[..prefix_start];
    let after_cursor = &current_line[cursor_col.min(current_line.len())..];
    let is_quoted_prefix = prefix.starts_with('"') || prefix.starts_with("@\"");
    let adjusted_after_cursor =
        if is_quoted_prefix && item.value.ends_with('"') && after_cursor.starts_with('"') {
            &after_cursor[1..]
        } else {
            after_cursor
        };
    let is_slash_command =
        prefix.starts_with('/') && before_prefix.trim().is_empty() && !prefix[1..].contains('/');
    let suffix = if prefix.starts_with('@') && !item.label.ends_with('/') {
        " "
    } else {
        ""
    };
    let inserted = if is_slash_command {
        format!("/{} ", item.value)
    } else {
        format!("{}{}", item.value, suffix)
    };
    let new_line = format!("{before_prefix}{inserted}{adjusted_after_cursor}");
    if cursor_line >= new_lines.len() {
        new_lines.resize(cursor_line + 1, String::new());
    }
    new_lines[cursor_line] = new_line;
    (new_lines, cursor_line, before_prefix.len() + inserted.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completes_slash_command() {
        let provider = CombinedAutocompleteProvider::new(vec![SlashCommand::new("help")], ".");
        let suggestions = provider
            .get_suggestions(&["/he".into()], 0, 3, false)
            .unwrap();
        assert_eq!(suggestions.items[0].value, "help");
    }

    #[test]
    fn quotes_paths_with_spaces_and_applies_without_duplicate_quote() {
        let base = std::env::temp_dir().join(format!("pi-tui-autocomplete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("my folder")).unwrap();
        std::fs::write(base.join("my folder/test.txt"), "content").unwrap();
        let provider = CombinedAutocompleteProvider::new(Vec::new(), &base);

        let suggestions = provider
            .get_suggestions(&["my".into()], 0, 2, true)
            .expect("suggestions");
        assert!(
            suggestions
                .items
                .iter()
                .any(|item| item.value == "\"my folder/\"")
        );

        let line = "@\"my folder/te\"".to_string();
        let cursor = line.len() - 1;
        let suggestions = provider
            .get_suggestions(std::slice::from_ref(&line), 0, cursor, false)
            .expect("quoted suggestions");
        let item = suggestions
            .items
            .iter()
            .find(|item| item.value == "@\"my folder/test.txt\"")
            .expect("test.txt")
            .clone();
        let (lines, _, cursor_col) =
            provider.apply_completion(&[line], 0, cursor, &item, &suggestions.prefix);
        assert_eq!(lines[0], "@\"my folder/test.txt\" ");
        assert_eq!(cursor_col, lines[0].len());
        let _ = std::fs::remove_dir_all(&base);
    }
}
