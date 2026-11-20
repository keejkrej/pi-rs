//! Port of packages/tui/src/autocomplete.ts

#![allow(dead_code, non_upper_case_globals, unused_imports, unused_variables)]

use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use async_trait::async_trait;
use indexmap::{IndexMap, IndexSet};

use pi_js::BoxFuture;
use pi_js::abort::AbortSignal;

use crate::utils::{autocomplete_boundary_regex, autocomplete_separator_regex};

const PATH_DELIMITERS_VALUES: &[&str] = &[" ", "\t", "\"", "'", "="];

static PATH_DELIMITERS: LazyLock<IndexSet<&'static str>> = LazyLock::new(|| {
    let mut delimiters = IndexSet::new();
    for delimiter in PATH_DELIMITERS_VALUES {
        delimiters.insert(*delimiter);
    }
    delimiters
});

// TS `new RegExp(`${autocompleteBoundaryRegex.source}$`, "u")`.
// PORT: fancy-regex; the `"u"` flag is the default unicode mode.
static token_start_regex: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    let mut pattern = String::new();
    pattern.push_str(autocomplete_boundary_regex.as_str());
    pattern.push('$');
    fancy_regex::Regex::new(&pattern).expect("tokenStartRegex")
});

// Opening wrappers that may precede a path in prose, mapped to their closing counterpart.
static PATH_WRAPPERS: LazyLock<IndexMap<&'static str, &'static str>> = LazyLock::new(|| {
    let mut wrappers = IndexMap::new();
    wrappers.insert("(", ")");
    wrappers.insert("[", "]");
    wrappers.insert("{", "}");
    wrappers.insert("<", ">");
    wrappers.insert("`", "`");
    wrappers
});

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutocompleteItem {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

/// `T | Promise<T>` argument completions. `None` is `null`.
pub type GetArgumentCompletions = Arc<dyn Fn(&str) -> BoxFuture<Option<Vec<AutocompleteItem>>> + Send + Sync>;

#[derive(Clone)]
pub struct SlashCommand {
    pub name: String,
    pub description: Option<String>,
    pub argument_hint: Option<String>,
    /// Returns `None` if no argument completion is available.
    pub get_argument_completions: Option<GetArgumentCompletions>,
}

/// `SlashCommand | AutocompleteItem`.
///
/// TS checks `"name" in cmd` first, then the item's `value`.
#[derive(Clone)]
pub enum CombinedCommand {
    SlashCommand(SlashCommand),
    AutocompleteItem(AutocompleteItem),
}

/// Field order is the object literal: `items`, `prefix`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutocompleteSuggestions {
    pub items: Vec<AutocompleteItem>,
    /// What we're matching against (e.g., "/" or "src/").
    pub prefix: String,
}

/// `{ signal, force? }` passed to [`AutocompleteProvider::get_suggestions`].
#[derive(Clone)]
pub struct GetSuggestionsOptions {
    pub signal: AbortSignal,
    pub force: Option<bool>,
}

/// `{ lines, cursorLine, cursorCol }` from `applyCompletion`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplyCompletionResult {
    pub lines: Vec<String>,
    pub cursor_line: i64,
    pub cursor_col: i64,
}

#[async_trait]
pub trait AutocompleteProvider: Send + Sync {
    /// Characters that should naturally trigger this provider at token boundaries.
    ///
    /// `None` is a missing `triggerCharacters` (`undefined`).
    fn trigger_characters(&self) -> Option<Vec<String>> {
        None
    }

    /// PORT: default does not persist. JS can assign `triggerCharacters` on any provider object.
    /// Override together with [`Self::trigger_characters`] when the assignment must stick.
    /// [`CombinedAutocompleteProvider`] stores it.
    fn set_trigger_characters(&self, _trigger_characters: Option<Vec<String>>) {}

    /// Get autocomplete suggestions for current text/cursor position.
    /// Returns null if no suggestions available.
    async fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: i64,
        cursor_col: i64,
        options: GetSuggestionsOptions,
    ) -> Option<AutocompleteSuggestions>;

    /// Apply the selected item.
    /// Returns the new text and cursor position.
    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: i64,
        cursor_col: i64,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> ApplyCompletionResult;

    /// Check if file completion should trigger for explicit Tab completion.
    ///
    /// Default matches a missing method: the editor treats absence as trigger (`!fn || fn()`).
    fn should_trigger_file_completion(&self, _lines: &[String], _cursor_line: i64, _cursor_col: i64) -> bool {
        true
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedPathPrefix {
    raw_prefix: String,
    is_at_prefix: bool,
    is_quoted_prefix: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CompletionValueOptions {
    is_directory: bool,
    is_at_prefix: bool,
    is_quoted_prefix: bool,
}

/// `{ path, isDirectory }` from `fd` and `readdir`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DirectoryEntry {
    path: String,
    is_directory: bool,
}

/// `{ baseDir, query, displayBase }`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ScopedFuzzyQuery {
    base_dir: String,
    query: String,
    display_base: String,
}

struct FuzzyFileSuggestionOptions {
    is_quoted_prefix: bool,
    signal: AbortSignal,
}

fn to_display_path(value: &str) -> String {
    value.replace('\\', "/")
}

fn escape_regex(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(
            ch,
            '.' | '*' | '+' | '?' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn build_fd_path_query(query: &str) -> String {
    let normalized = to_display_path(query);
    if !normalized.contains('/') {
        return escape_regex(&normalized);
    }

    let has_trailing_separator = normalized.ends_with('/');
    let trimmed = normalized.trim_start_matches('/');
    let separator_pattern = "[\\\\/]";
    let segments: Vec<String> = trimmed
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(escape_regex)
        .collect();

    let mut pattern = segments.join(separator_pattern);
    if has_trailing_separator {
        pattern.push_str(separator_pattern);
    }
    pattern
}

fn find_last_delimiter(text: &str) -> i64 {
    let mut last_delimiter = -1_i64;
    let mut index = 0_i64;
    let mut utf8_buf = [0_u8; 4];
    for character in text.chars() {
        index += character.len_utf16() as i64;
        let char_str = character.encode_utf8(&mut utf8_buf);
        if PATH_DELIMITERS.contains(char_str) || autocomplete_separator_regex.is_match(char_str).unwrap_or(false) {
            last_delimiter = index - 1;
        }
    }
    last_delimiter
}

// Strip opening wrappers before a path, e.g. "(~/Dev" -> "~/Dev" or "`src/ma" -> "src/ma".
// Keep a wrapper if the token also contains its closer, e.g. "app/[slug]/pa" or "(group)/pa".
fn strip_leading_wrappers(token: &str) -> String {
    let mut result = token;
    let mut utf8_buf = [0_u8; 4];
    while let Some(first_ch) = result.chars().next() {
        let first_str = first_ch.encode_utf8(&mut utf8_buf);
        let Some(&closer) = PATH_WRAPPERS.get(first_str) else {
            break;
        };
        if result[first_ch.len_utf8()..].contains(closer) {
            break;
        }
        result = &result[first_ch.len_utf8()..];
    }
    result.to_string()
}

fn find_unclosed_quote_start(text: &str) -> Option<i64> {
    let mut in_quotes = false;
    let mut quote_start = -1_i64;
    let mut i = 0_i64;

    for ch in text.chars() {
        if ch == '"' {
            in_quotes = !in_quotes;
            if in_quotes {
                quote_start = i;
            }
        }
        i += ch.len_utf16() as i64;
    }

    if in_quotes { Some(quote_start) } else { None }
}

fn is_token_start(text: &str, index: i64) -> bool {
    let mut start = index;
    while start > 0 {
        let ch_str = pi_js::str16::slice(text, start - 1, Some(start));
        if PATH_WRAPPERS.contains_key(ch_str.as_str()) {
            start -= 1;
        } else {
            break;
        }
    }
    let prev = if start > 0 {
        pi_js::str16::slice(text, start - 1, Some(start))
    } else {
        String::new()
    };
    if PATH_DELIMITERS.contains(prev.as_str()) {
        return true;
    }
    let prefix = pi_js::str16::slice(text, 0, Some(start));
    token_start_regex.is_match(&prefix).unwrap_or(false)
}

fn extract_quoted_prefix(text: &str) -> Option<String> {
    let quote_start = find_unclosed_quote_start(text)?;

    if quote_start > 0 && pi_js::str16::char_code_at(text, (quote_start - 1) as usize) == Some(b'@' as u16) {
        if !is_token_start(text, quote_start - 1) {
            return None;
        }
        return Some(pi_js::str16::slice(text, quote_start - 1, None));
    }

    if !is_token_start(text, quote_start) {
        return None;
    }

    Some(pi_js::str16::slice(text, quote_start, None))
}

fn parse_path_prefix(prefix: &str) -> ParsedPathPrefix {
    if let Some(rest) = prefix.strip_prefix("@\"") {
        return ParsedPathPrefix {
            raw_prefix: rest.to_string(),
            is_at_prefix: true,
            is_quoted_prefix: true,
        };
    }
    if let Some(rest) = prefix.strip_prefix('"') {
        return ParsedPathPrefix {
            raw_prefix: rest.to_string(),
            is_at_prefix: false,
            is_quoted_prefix: true,
        };
    }
    if let Some(rest) = prefix.strip_prefix('@') {
        return ParsedPathPrefix {
            raw_prefix: rest.to_string(),
            is_at_prefix: true,
            is_quoted_prefix: false,
        };
    }
    ParsedPathPrefix {
        raw_prefix: prefix.to_string(),
        is_at_prefix: false,
        is_quoted_prefix: false,
    }
}

fn build_completion_value(path: &str, options: &CompletionValueOptions) -> String {
    let needs_quotes = options.is_quoted_prefix || autocomplete_separator_regex.is_match(path).unwrap_or(false);
    let prefix = if options.is_at_prefix { "@" } else { "" };

    if !needs_quotes {
        return format!("{prefix}{path}");
    }

    format!("{prefix}\"{path}\"")
}

/// Use fd to walk directory tree (fast, respects .gitignore).
async fn walk_directory_with_fd(
    base_dir: &str,
    fd_path: &str,
    query: &str,
    max_results: i64,
    signal: &AbortSignal,
    max_depth: Option<i64>,
) -> Vec<DirectoryEntry> {
    if signal.aborted() {
        return Vec::new();
    }

    let max_results_str = max_results.to_string();
    let mut args = vec![
        "--base-directory".to_string(),
        base_dir.to_string(),
        "--max-results".to_string(),
        max_results_str,
        "--type".to_string(),
        "f".to_string(),
        "--type".to_string(),
        "d".to_string(),
        "--follow".to_string(),
        "--hidden".to_string(),
        "--exclude".to_string(),
        ".git".to_string(),
        "--exclude".to_string(),
        ".git/*".to_string(),
        "--exclude".to_string(),
        ".git/**".to_string(),
    ];

    if let Some(depth) = max_depth {
        args.push("--max-depth".to_string());
        args.push(depth.to_string());
    }

    if to_display_path(query).contains('/') {
        args.push("--full-path".to_string());
    }

    if !query.is_empty() {
        args.push(build_fd_path_query(query));
    }

    let mut cmd = tokio::process::Command::new(fd_path);
    cmd.args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let read_stdout = async move {
        let mut buf = Vec::new();
        if let Some(mut pipe) = stdout_pipe {
            let _ = tokio::io::AsyncReadExt::read_to_end(&mut pipe, &mut buf).await;
        }
        buf
    };
    let drain_stderr = async move {
        let mut buf = Vec::new();
        if let Some(mut pipe) = stderr_pipe {
            let _ = tokio::io::AsyncReadExt::read_to_end(&mut pipe, &mut buf).await;
        }
    };

    let (status, stdout_bytes) = tokio::select! {
        _ = signal.cancelled() => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Vec::new();
        }
        res = async {
            let (status_res, out, ()) = tokio::join!(child.wait(), read_stdout, drain_stderr);
            (status_res, out)
        } => {
            match res.0 {
                Ok(status) => (status, res.1),
                Err(_) => return Vec::new(),
            }
        }
    };

    if signal.aborted() || !status.success() {
        return Vec::new();
    }

    let stdout = String::from_utf8_lossy(&stdout_bytes);
    let trimmed_stdout = pi_js::str16::trim(&stdout);
    if trimmed_stdout.is_empty() {
        return Vec::new();
    }

    let mut results = Vec::new();
    for line in trimmed_stdout.split('\n').filter(|s| !s.is_empty()) {
        let display_line = to_display_path(line);
        let has_trailing_separator = display_line.ends_with('/');
        let normalized_path = if has_trailing_separator {
            &display_line[..display_line.len() - 1]
        } else {
            &display_line
        };
        if normalized_path == ".git" || normalized_path.starts_with(".git/") || normalized_path.contains("/.git/") {
            continue;
        }
        results.push(DirectoryEntry {
            path: display_line,
            is_directory: has_trailing_separator,
        });
    }

    results
}

struct CombinedAutocompleteProviderFields {
    commands: Vec<CombinedCommand>,
    base_path: String,
    fd_path: Option<String>,
    /// JS may assign `triggerCharacters` onto the instance. `None` is `undefined`.
    trigger_characters: Option<Vec<String>>,
}

struct CombinedAutocompleteProviderInner {
    fields: Mutex<CombinedAutocompleteProviderFields>,
}

/// Combined provider that handles both slash commands and file paths.
///
/// PORT: TS class with identity. Handle so it can be `Arc<dyn AutocompleteProvider>`.
#[derive(Clone)]
pub struct CombinedAutocompleteProvider {
    inner: Arc<CombinedAutocompleteProviderInner>,
}

impl CombinedAutocompleteProvider {
    fn fields(&self) -> MutexGuard<'_, CombinedAutocompleteProviderFields> {
        self.inner.fields.lock().unwrap()
    }

    /// `commands` `None` is `[]`. `fd_path` `None` is `null` (also the default when omitted).
    pub fn new(commands: Option<Vec<CombinedCommand>>, base_path: &str, fd_path: Option<&str>) -> Self {
        Self {
            inner: Arc::new(CombinedAutocompleteProviderInner {
                fields: Mutex::new(CombinedAutocompleteProviderFields {
                    commands: commands.unwrap_or_default(),
                    base_path: base_path.to_string(),
                    fd_path: fd_path.map(str::to_string),
                    trigger_characters: None,
                }),
            }),
        }
    }

    pub fn trigger_characters(&self) -> Option<Vec<String>> {
        self.fields().trigger_characters.clone()
    }

    pub fn set_trigger_characters(&self, trigger_characters: Option<Vec<String>>) {
        self.fields().trigger_characters = trigger_characters;
    }

    pub async fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: i64,
        cursor_col: i64,
        options: GetSuggestionsOptions,
    ) -> Option<AutocompleteSuggestions> {
        let current_line = usize::try_from(cursor_line)
            .ok()
            .and_then(|idx| lines.get(idx).map(String::as_str))
            .unwrap_or("");
        let text_before_cursor = pi_js::str16::slice(current_line, 0, Some(cursor_col));

        if let Some(at_prefix) = self.extract_at_prefix(&text_before_cursor) {
            let ParsedPathPrefix {
                raw_prefix,
                is_quoted_prefix,
                ..
            } = parse_path_prefix(&at_prefix);
            let suggestions = self
                .get_fuzzy_file_suggestions(
                    &raw_prefix,
                    FuzzyFileSuggestionOptions {
                        is_quoted_prefix,
                        signal: options.signal.clone(),
                    },
                )
                .await;
            if suggestions.is_empty() {
                return None;
            }
            return Some(AutocompleteSuggestions {
                items: suggestions,
                prefix: at_prefix,
            });
        }

        let command_text = pi_js::str16::trim_start(&text_before_cursor);
        if !options.force.unwrap_or(false) && command_text.starts_with('/') {
            if let Some(space_index) = command_text.find(' ') {
                let command_name = &command_text[1..space_index];
                let argument_text = &command_text[space_index + 1..];

                let get_arg_completions = {
                    let fields = self.fields();
                    fields
                        .commands
                        .iter()
                        .find(|cmd| match cmd {
                            CombinedCommand::SlashCommand(sc) => sc.name == command_name,
                            CombinedCommand::AutocompleteItem(item) => item.value == command_name,
                        })
                        .and_then(|cmd| match cmd {
                            CombinedCommand::SlashCommand(sc) => sc.get_argument_completions.clone(),
                            CombinedCommand::AutocompleteItem(_) => None,
                        })
                };

                let Some(get_arg_completions) = get_arg_completions else {
                    return None;
                };

                let argument_suggestions = get_arg_completions(argument_text).await?;
                if argument_suggestions.is_empty() {
                    return None;
                }

                return Some(AutocompleteSuggestions {
                    items: argument_suggestions,
                    prefix: argument_text.to_string(),
                });
            }

            let prefix = &command_text[1..];

            #[derive(Clone)]
            struct CommandEntry {
                name: String,
                label: String,
                description: Option<String>,
            }

            let command_items: Vec<CommandEntry> = {
                let fields = self.fields();
                fields
                    .commands
                    .iter()
                    .map(|cmd| match cmd {
                        CombinedCommand::SlashCommand(sc) => {
                            let hint = sc.argument_hint.as_deref().filter(|s| !s.is_empty());
                            let desc = sc.description.as_deref().unwrap_or("");
                            let full_desc = match hint {
                                Some(h) => {
                                    if desc.is_empty() {
                                        h.to_string()
                                    } else {
                                        format!("{h} — {desc}")
                                    }
                                }
                                None => desc.to_string(),
                            };
                            CommandEntry {
                                name: sc.name.clone(),
                                label: sc.name.clone(),
                                description: if full_desc.is_empty() { None } else { Some(full_desc) },
                            }
                        }
                        CombinedCommand::AutocompleteItem(item) => CommandEntry {
                            name: item.value.clone(),
                            label: item.value.clone(),
                            description: item.description.as_ref().filter(|s| !s.is_empty()).cloned(),
                        },
                    })
                    .collect()
            };

            let all_indices: Vec<usize> = (0..command_items.len()).collect();
            let items_for_bare = command_items.clone();
            let bare_match_indices = crate::fuzzy::fuzzy_filter(
                &all_indices,
                prefix,
                Arc::new(move |&idx: &usize| {
                    let name = &items_for_bare[idx].name;
                    name.strip_prefix("skill:").unwrap_or(name).to_string()
                }),
            );
            let bare_match_set: IndexSet<usize> = bare_match_indices.iter().copied().collect();
            let remaining_skill_indices: Vec<usize> = all_indices
                .into_iter()
                .filter(|idx| command_items[*idx].name.starts_with("skill:") && !bare_match_set.contains(idx))
                .collect();
            let items_for_full = command_items.clone();
            let full_match_indices = crate::fuzzy::fuzzy_filter(
                &remaining_skill_indices,
                prefix,
                Arc::new(move |&idx: &usize| items_for_full[idx].name.clone()),
            );

            let filtered: Vec<AutocompleteItem> = bare_match_indices
                .into_iter()
                .chain(full_match_indices)
                .map(|idx| {
                    let entry = &command_items[idx];
                    AutocompleteItem {
                        value: entry.name.clone(),
                        label: entry.label.clone(),
                        description: entry.description.clone(),
                    }
                })
                .collect();

            if filtered.is_empty() {
                return None;
            }

            return Some(AutocompleteSuggestions {
                items: filtered,
                prefix: command_text.to_string(),
            });
        }

        let path_match = self.extract_path_prefix(&text_before_cursor, Some(options.force.unwrap_or(false)))?;
        let suggestions = self.get_file_suggestions(&path_match);
        if suggestions.is_empty() {
            return None;
        }

        Some(AutocompleteSuggestions {
            items: suggestions,
            prefix: path_match,
        })
    }

    pub fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: i64,
        cursor_col: i64,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> ApplyCompletionResult {
        let current_line = usize::try_from(cursor_line)
            .ok()
            .and_then(|idx| lines.get(idx).map(String::as_str))
            .unwrap_or("");
        let prefix_len_16 = pi_js::str16::len(prefix) as i64;
        let before_prefix = pi_js::str16::slice(current_line, 0, Some(cursor_col - prefix_len_16));
        let after_cursor = pi_js::str16::slice(current_line, cursor_col, None);
        let is_quoted_prefix = prefix.starts_with('"') || prefix.starts_with("@\"");
        let has_leading_quote_after_cursor = after_cursor.starts_with('"');
        let has_trailing_quote_in_item = item.value.ends_with('"');
        let adjusted_after_cursor = if is_quoted_prefix && has_trailing_quote_in_item && has_leading_quote_after_cursor
        {
            &after_cursor[1..]
        } else {
            &after_cursor
        };

        let mut new_lines = lines.to_vec();
        let set_line = |new_lines: &mut Vec<String>, new_line: String| {
            if let Ok(idx) = usize::try_from(cursor_line) {
                if idx < new_lines.len() {
                    new_lines[idx] = new_line;
                } else if idx == new_lines.len() {
                    new_lines.push(new_line);
                }
            }
        };

        let is_slash_command =
            prefix.starts_with('/') && pi_js::str16::trim(&before_prefix).is_empty() && !prefix[1..].contains('/');
        if is_slash_command {
            let new_line = format!("{before_prefix}/{} {adjusted_after_cursor}", item.value);
            set_line(&mut new_lines, new_line);
            return ApplyCompletionResult {
                lines: new_lines,
                cursor_line,
                cursor_col: (pi_js::str16::len(&before_prefix) + pi_js::str16::len(&item.value) + 2) as i64,
            };
        }

        if prefix.starts_with('@') {
            let is_directory = item.label.ends_with('/');
            let suffix = if is_directory { "" } else { " " };
            let new_line = format!("{before_prefix}{}{suffix}{adjusted_after_cursor}", item.value);
            set_line(&mut new_lines, new_line);

            let has_trailing_quote = item.value.ends_with('"');
            let item_len_16 = pi_js::str16::len(&item.value) as i64;
            let cursor_offset = if is_directory && has_trailing_quote {
                item_len_16 - 1
            } else {
                item_len_16
            };

            return ApplyCompletionResult {
                lines: new_lines,
                cursor_line,
                cursor_col: (pi_js::str16::len(&before_prefix) as i64)
                    + cursor_offset
                    + (pi_js::str16::len(suffix) as i64),
            };
        }

        let new_line = format!("{before_prefix}{}{adjusted_after_cursor}", item.value);
        set_line(&mut new_lines, new_line);

        let is_directory = item.label.ends_with('/');
        let has_trailing_quote = item.value.ends_with('"');
        let item_len_16 = pi_js::str16::len(&item.value) as i64;
        let cursor_offset = if is_directory && has_trailing_quote {
            item_len_16 - 1
        } else {
            item_len_16
        };

        ApplyCompletionResult {
            lines: new_lines,
            cursor_line,
            cursor_col: (pi_js::str16::len(&before_prefix) as i64) + cursor_offset,
        }
    }

    /// Extract @ prefix for fuzzy file suggestions.
    fn extract_at_prefix(&self, text: &str) -> Option<String> {
        if let Some(quoted_prefix) = extract_quoted_prefix(text)
            && quoted_prefix.starts_with("@\"")
        {
            return Some(quoted_prefix);
        }

        let last_delimiter_index = find_last_delimiter(text);
        let tail = if last_delimiter_index == -1 {
            text.to_string()
        } else {
            pi_js::str16::slice(text, last_delimiter_index + 1, None)
        };
        let token = strip_leading_wrappers(&tail);

        if token.starts_with('@') {
            return Some(token);
        }

        None
    }

    /// Extract a path-like prefix from the text before cursor.
    ///
    /// `force_extract` defaults to false inside the body.
    fn extract_path_prefix(&self, text: &str, force_extract: Option<bool>) -> Option<String> {
        if let Some(quoted_prefix) = extract_quoted_prefix(text) {
            return Some(quoted_prefix);
        }

        let last_delimiter_index = find_last_delimiter(text);
        let tail = if last_delimiter_index == -1 {
            text.to_string()
        } else {
            pi_js::str16::slice(text, last_delimiter_index + 1, None)
        };
        let path_prefix = strip_leading_wrappers(&tail);

        if force_extract.unwrap_or(false) {
            return Some(path_prefix);
        }

        if path_prefix.contains('/') || path_prefix.starts_with('.') || path_prefix.starts_with("~/") {
            return Some(path_prefix);
        }

        if path_prefix.is_empty() && !text.is_empty() && token_start_regex.is_match(text).unwrap_or(false) {
            return Some(path_prefix);
        }

        None
    }

    /// Expand home directory (~/) to actual home path.
    fn expand_home_path(&self, path: &str) -> String {
        if let Some(rest) = path.strip_prefix("~/") {
            let home = pi_js::env::home_dir();
            let expanded_path = pi_js::path::join(&[&home, rest]);
            if path.ends_with('/') && !expanded_path.ends_with('/') {
                format!("{expanded_path}/")
            } else {
                expanded_path
            }
        } else if path == "~" {
            pi_js::env::home_dir()
        } else {
            path.to_string()
        }
    }

    fn resolve_scoped_fuzzy_query(&self, raw_query: &str) -> Option<ScopedFuzzyQuery> {
        let normalized_query = to_display_path(raw_query);
        let slash_index = normalized_query.rfind('/')?;

        let display_base = &normalized_query[..=slash_index];
        let query = &normalized_query[slash_index + 1..];

        let base_dir = if display_base.starts_with("~/") {
            self.expand_home_path(display_base)
        } else if display_base.starts_with('/') {
            display_base.to_string()
        } else {
            let base_path = self.fields().base_path.clone();
            pi_js::path::join(&[&base_path, display_base])
        };

        let Ok(st) = pi_js::fs::stat(&base_dir) else {
            return None;
        };
        if !st.is_directory() {
            return None;
        }

        Some(ScopedFuzzyQuery {
            base_dir,
            query: query.to_string(),
            display_base: display_base.to_string(),
        })
    }

    fn scoped_path_for_display(&self, display_base: &str, relative_path: &str) -> String {
        let normalized_relative_path = to_display_path(relative_path);
        if display_base == "/" {
            format!("/{normalized_relative_path}")
        } else {
            format!("{}{normalized_relative_path}", to_display_path(display_base))
        }
    }

    /// Get file/directory suggestions for a given path prefix.
    fn get_file_suggestions(&self, prefix: &str) -> Vec<AutocompleteItem> {
        let base_path = self.fields().base_path.clone();
        let ParsedPathPrefix {
            raw_prefix,
            is_at_prefix,
            is_quoted_prefix,
        } = parse_path_prefix(prefix);
        let expanded_prefix = if raw_prefix.starts_with('~') {
            self.expand_home_path(&raw_prefix)
        } else {
            raw_prefix.clone()
        };

        let is_root_prefix = raw_prefix.is_empty()
            || raw_prefix == "./"
            || raw_prefix == "../"
            || raw_prefix == "~"
            || raw_prefix == "~/"
            || raw_prefix == "/"
            || (is_at_prefix && raw_prefix.is_empty());

        let (search_dir, search_prefix) = if is_root_prefix || raw_prefix.ends_with('/') {
            let dir = if raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
                expanded_prefix.clone()
            } else {
                pi_js::path::join(&[&base_path, &expanded_prefix])
            };
            (dir, String::new())
        } else {
            let dir = pi_js::path::dirname(&expanded_prefix);
            let file = pi_js::path::basename(&expanded_prefix, None);
            let search_dir = if raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
                dir
            } else {
                pi_js::path::join(&[&base_path, &dir])
            };
            (search_dir, file)
        };

        let Ok(entries) = pi_js::fs::readdir_with_file_types(&search_dir) else {
            return Vec::new();
        };

        let search_prefix_lower = search_prefix.to_lowercase();
        let mut suggestions = Vec::new();

        for entry in entries {
            if !entry.name.to_lowercase().starts_with(&search_prefix_lower) {
                continue;
            }

            let mut is_directory = entry.is_directory();
            if !is_directory && entry.is_symbolic_link() {
                let full_path = pi_js::path::join(&[&search_dir, &entry.name]);
                if let Ok(st) = pi_js::fs::stat(&full_path) {
                    is_directory = st.is_directory();
                }
            }

            let name = &entry.name;
            let display_prefix = &raw_prefix;
            let mut relative_path = if display_prefix.ends_with('/') {
                format!("{display_prefix}{name}")
            } else if display_prefix.contains('/') || display_prefix.contains('\\') {
                if let Some(home_rel) = display_prefix.strip_prefix("~/") {
                    let dir = pi_js::path::dirname(home_rel);
                    if dir == "." {
                        format!("~/{name}")
                    } else {
                        format!("~/{}", pi_js::path::join(&[&dir, name]))
                    }
                } else if display_prefix.starts_with('/') {
                    let dir = pi_js::path::dirname(display_prefix);
                    if dir == "/" {
                        format!("/{name}")
                    } else {
                        format!("{dir}/{name}")
                    }
                } else {
                    let dir = pi_js::path::dirname(display_prefix);
                    let joined = pi_js::path::join(&[&dir, name]);
                    if display_prefix.starts_with("./") && !joined.starts_with("./") {
                        format!("./{joined}")
                    } else {
                        joined
                    }
                }
            } else if display_prefix.starts_with('~') {
                format!("~/{name}")
            } else {
                name.clone()
            };

            relative_path = to_display_path(&relative_path);
            let path_value = if is_directory {
                format!("{relative_path}/")
            } else {
                relative_path
            };
            let value = build_completion_value(
                &path_value,
                &CompletionValueOptions {
                    is_directory,
                    is_at_prefix,
                    is_quoted_prefix,
                },
            );

            suggestions.push(AutocompleteItem {
                value,
                label: if is_directory { format!("{name}/") } else { name.clone() },
                description: None,
            });
        }

        suggestions.sort_by(|a, b| {
            let a_is_dir = a.label.ends_with('/');
            let b_is_dir = b.label.ends_with('/');
            if a_is_dir && !b_is_dir {
                std::cmp::Ordering::Less
            } else if !a_is_dir && b_is_dir {
                std::cmp::Ordering::Greater
            } else {
                pi_js::intl::locale_compare(&a.label, &b.label)
            }
        });

        suggestions
    }

    /// Score an entry against the query (higher = better match).
    /// `is_directory` adds a bonus to prioritize folders.
    fn score_entry(&self, file_path: &str, query: &str, is_directory: bool) -> i64 {
        let file_name = pi_js::path::basename(file_path, None);
        let lower_file_name = file_name.to_lowercase();
        let lower_query = query.to_lowercase();

        let mut score = if lower_file_name == lower_query {
            100
        } else if lower_file_name.starts_with(&lower_query) {
            80
        } else if lower_file_name.contains(&lower_query) {
            50
        } else if file_path.to_lowercase().contains(&lower_query) {
            30
        } else {
            0
        };

        if is_directory && score > 0 {
            score += 10;
        }

        score
    }

    async fn get_base_dir_suggestions(&self, base_dir: &str, query: &str, signal: &AbortSignal) -> Vec<DirectoryEntry> {
        let fd_path = self.fields().fd_path.clone();
        let Some(fd_path) = fd_path.filter(|p| !p.is_empty()) else {
            return Vec::new();
        };
        if signal.aborted() {
            return Vec::new();
        }
        walk_directory_with_fd(base_dir, &fd_path, query, 100, signal, Some(1)).await
    }

    /// Fuzzy file search using fd (fast, respects .gitignore).
    async fn get_fuzzy_file_suggestions(
        &self,
        query: &str,
        options: FuzzyFileSuggestionOptions,
    ) -> Vec<AutocompleteItem> {
        let (fd_path, base_path) = {
            let fields = self.fields();
            (fields.fd_path.clone(), fields.base_path.clone())
        };
        let Some(fd_path) = fd_path.filter(|p| !p.is_empty()) else {
            return Vec::new();
        };
        if options.signal.aborted() {
            return Vec::new();
        }

        let scoped_query = self.resolve_scoped_fuzzy_query(query);
        let (fd_base_dir, fd_query) = match &scoped_query {
            Some(sq) => (sq.base_dir.clone(), sq.query.clone()),
            None => (base_path, query.to_string()),
        };

        let base_dir_entries = self
            .get_base_dir_suggestions(&fd_base_dir, &fd_query, &options.signal)
            .await;
        let recursive_entries =
            walk_directory_with_fd(&fd_base_dir, &fd_path, &fd_query, 100, &options.signal, None).await;

        let mut seen_paths = IndexSet::new();
        let mut entries = Vec::with_capacity(base_dir_entries.len() + recursive_entries.len());
        for entry in base_dir_entries {
            if seen_paths.insert(entry.path.clone()) {
                entries.push(entry);
            }
        }
        for entry in recursive_entries {
            if seen_paths.insert(entry.path.clone()) {
                entries.push(entry);
            }
        }

        if options.signal.aborted() {
            return Vec::new();
        }

        struct ScoredEntry {
            path: String,
            is_directory: bool,
            score: i64,
        }

        let mut scored_entries: Vec<ScoredEntry> = entries
            .into_iter()
            .filter_map(|entry| {
                let score = if fd_query.is_empty() {
                    1
                } else {
                    self.score_entry(&entry.path, &fd_query, entry.is_directory)
                };
                (score > 0).then_some(ScoredEntry {
                    path: entry.path,
                    is_directory: entry.is_directory,
                    score,
                })
            })
            .collect();

        scored_entries.sort_by(|a, b| {
            let score_ord = b.score.cmp(&a.score);
            if score_ord != std::cmp::Ordering::Equal {
                return score_ord;
            }
            let a_depth = to_display_path(&a.path).split('/').filter(|s| !s.is_empty()).count();
            let b_depth = to_display_path(&b.path).split('/').filter(|s| !s.is_empty()).count();
            let depth_ord = a_depth.cmp(&b_depth);
            if depth_ord != std::cmp::Ordering::Equal {
                return depth_ord;
            }
            let len_ord = pi_js::str16::len(&a.path).cmp(&pi_js::str16::len(&b.path));
            if len_ord != std::cmp::Ordering::Equal {
                return len_ord;
            }
            pi_js::intl::locale_compare(&a.path, &b.path)
        });
        scored_entries.truncate(20);

        let mut suggestions = Vec::with_capacity(scored_entries.len());
        for entry in scored_entries {
            let is_directory = entry.is_directory;
            let path_without_slash = if is_directory && entry.path.ends_with('/') {
                &entry.path[..entry.path.len() - 1]
            } else {
                &entry.path
            };
            let display_path = match &scoped_query {
                Some(sq) => self.scoped_path_for_display(&sq.display_base, path_without_slash),
                None => path_without_slash.to_string(),
            };
            let entry_name = pi_js::path::basename(path_without_slash, None);
            let completion_path = if is_directory {
                format!("{display_path}/")
            } else {
                display_path.clone()
            };
            let value = build_completion_value(
                &completion_path,
                &CompletionValueOptions {
                    is_directory,
                    is_at_prefix: true,
                    is_quoted_prefix: options.is_quoted_prefix,
                },
            );
            suggestions.push(AutocompleteItem {
                value,
                label: if is_directory {
                    format!("{entry_name}/")
                } else {
                    entry_name
                },
                description: Some(display_path),
            });
        }

        suggestions
    }

    /// Check if we should trigger file completion (called on Tab key).
    pub fn should_trigger_file_completion(&self, lines: &[String], cursor_line: i64, cursor_col: i64) -> bool {
        let current_line = usize::try_from(cursor_line)
            .ok()
            .and_then(|idx| lines.get(idx).map(String::as_str))
            .unwrap_or("");
        let text_before_cursor = pi_js::str16::slice(current_line, 0, Some(cursor_col));
        let trimmed = pi_js::str16::trim(&text_before_cursor);

        !(trimmed.starts_with('/') && !trimmed.contains(' '))
    }
}

#[async_trait]
impl AutocompleteProvider for CombinedAutocompleteProvider {
    fn trigger_characters(&self) -> Option<Vec<String>> {
        CombinedAutocompleteProvider::trigger_characters(self)
    }

    fn set_trigger_characters(&self, trigger_characters: Option<Vec<String>>) {
        CombinedAutocompleteProvider::set_trigger_characters(self, trigger_characters);
    }

    async fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: i64,
        cursor_col: i64,
        options: GetSuggestionsOptions,
    ) -> Option<AutocompleteSuggestions> {
        CombinedAutocompleteProvider::get_suggestions(self, lines, cursor_line, cursor_col, options).await
    }

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: i64,
        cursor_col: i64,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> ApplyCompletionResult {
        CombinedAutocompleteProvider::apply_completion(self, lines, cursor_line, cursor_col, item, prefix)
    }

    fn should_trigger_file_completion(&self, lines: &[String], cursor_line: i64, cursor_col: i64) -> bool {
        CombinedAutocompleteProvider::should_trigger_file_completion(self, lines, cursor_line, cursor_col)
    }
}
