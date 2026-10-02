//! Port of packages/tui/src/autocomplete.ts

#![allow(dead_code, non_upper_case_globals, unused_imports, unused_variables)]

use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use async_trait::async_trait;
use indexmap::{IndexMap, IndexSet};

use pi_js::BoxFuture;
use pi_js::abort::AbortSignal;

use crate::utils::autocomplete_boundary_regex;

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
    todo!("port: to_display_path")
}

fn escape_regex(value: &str) -> String {
    todo!("port: escape_regex")
}

fn build_fd_path_query(query: &str) -> String {
    todo!("port: build_fd_path_query")
}

fn find_last_delimiter(text: &str) -> i64 {
    todo!("port: find_last_delimiter")
}

// Strip opening wrappers before a path, e.g. "(~/Dev" -> "~/Dev" or "`src/ma" -> "src/ma".
// Keep a wrapper if the token also contains its closer, e.g. "app/[slug]/pa" or "(group)/pa".
fn strip_leading_wrappers(token: &str) -> String {
    todo!("port: strip_leading_wrappers")
}

fn find_unclosed_quote_start(text: &str) -> Option<i64> {
    todo!("port: find_unclosed_quote_start")
}

fn is_token_start(text: &str, index: i64) -> bool {
    todo!("port: is_token_start")
}

fn extract_quoted_prefix(text: &str) -> Option<String> {
    todo!("port: extract_quoted_prefix")
}

fn parse_path_prefix(prefix: &str) -> ParsedPathPrefix {
    todo!("port: parse_path_prefix")
}

fn build_completion_value(path: &str, options: &CompletionValueOptions) -> String {
    todo!("port: build_completion_value")
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
    todo!("port: walk_directory_with_fd")
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
        todo!("port: CombinedAutocompleteProvider::new")
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
        todo!("port: CombinedAutocompleteProvider::get_suggestions")
    }

    pub fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: i64,
        cursor_col: i64,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> ApplyCompletionResult {
        todo!("port: CombinedAutocompleteProvider::apply_completion")
    }

    /// Extract @ prefix for fuzzy file suggestions.
    fn extract_at_prefix(&self, text: &str) -> Option<String> {
        todo!("port: CombinedAutocompleteProvider::extract_at_prefix")
    }

    /// Extract a path-like prefix from the text before cursor.
    ///
    /// `force_extract` defaults to false inside the body.
    fn extract_path_prefix(&self, text: &str, force_extract: Option<bool>) -> Option<String> {
        todo!("port: CombinedAutocompleteProvider::extract_path_prefix")
    }

    /// Expand home directory (~/) to actual home path.
    fn expand_home_path(&self, path: &str) -> String {
        todo!("port: CombinedAutocompleteProvider::expand_home_path")
    }

    fn resolve_scoped_fuzzy_query(&self, raw_query: &str) -> Option<ScopedFuzzyQuery> {
        todo!("port: CombinedAutocompleteProvider::resolve_scoped_fuzzy_query")
    }

    fn scoped_path_for_display(&self, display_base: &str, relative_path: &str) -> String {
        todo!("port: CombinedAutocompleteProvider::scoped_path_for_display")
    }

    /// Get file/directory suggestions for a given path prefix.
    fn get_file_suggestions(&self, prefix: &str) -> Vec<AutocompleteItem> {
        todo!("port: CombinedAutocompleteProvider::get_file_suggestions")
    }

    /// Score an entry against the query (higher = better match).
    /// `is_directory` adds a bonus to prioritize folders.
    fn score_entry(&self, file_path: &str, query: &str, is_directory: bool) -> i64 {
        todo!("port: CombinedAutocompleteProvider::score_entry")
    }

    async fn get_base_dir_suggestions(&self, base_dir: &str, query: &str, signal: &AbortSignal) -> Vec<DirectoryEntry> {
        todo!("port: CombinedAutocompleteProvider::get_base_dir_suggestions")
    }

    /// Fuzzy file search using fd (fast, respects .gitignore).
    async fn get_fuzzy_file_suggestions(
        &self,
        query: &str,
        options: FuzzyFileSuggestionOptions,
    ) -> Vec<AutocompleteItem> {
        todo!("port: CombinedAutocompleteProvider::get_fuzzy_file_suggestions")
    }

    /// Check if we should trigger file completion (called on Tab key).
    pub fn should_trigger_file_completion(&self, lines: &[String], cursor_line: i64, cursor_col: i64) -> bool {
        todo!("port: CombinedAutocompleteProvider::should_trigger_file_completion")
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
