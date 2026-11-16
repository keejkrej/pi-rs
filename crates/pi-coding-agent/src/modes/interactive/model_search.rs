//! Port of packages/coding-agent/src/modes/interactive/model-search.ts

use std::sync::LazyLock;

use indexmap::IndexSet;
use regex::Regex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelSearchItem {
    pub id: String,
    pub provider: String,
    pub name: Option<String>,
}

pub fn get_model_search_text(id: &str, provider: &str, name: Option<&str>) -> String {
    let name_part = match name {
        Some(n) if !n.is_empty() => format!(" {n}"),
        _ => String::new(),
    };
    format!("{id} {provider} {provider}/{id} {provider} {id}{name_part}")
}

static NON_ALNUM_SEP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^\p{L}\p{N}]+").expect("NON_ALNUM_SEP_RE compiles"));

fn split_model_label(value: &str) -> Vec<String> {
    NON_ALNUM_SEP_RE
        .split(&value.to_lowercase())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Returns the model name when it has words the ID does not, e.g. a renamed override like
/// `"MiniMax M3 [rasmus@openrouter, parasail]"` for `"minimax/minimax-m3"`. Names that only
/// restate the ID with different casing, separators, or a dropped namespace/version suffix
/// (`"Claude Sonnet 4.5 (EU)"` for `"eu.anthropic.claude-sonnet-4-5-20250929-v1:0"`) return `None`.
pub fn get_distinct_model_name<'a>(id: &str, name: Option<&'a str>) -> Option<&'a str> {
    let trimmed = name?.trim();
    if trimmed.is_empty() {
        return None;
    }
    let id_words = split_model_label(id);
    let compact_id = id_words.join("");
    let id_tokens: IndexSet<String> = id_words.into_iter().collect();
    // Single-character words must match a whole ID token: "4" is not covered by "mistral-small-2603".
    let is_covered =
        |word: &str| id_tokens.contains(word) || (word.encode_utf16().count() > 1 && compact_id.contains(word));
    if split_model_label(trimmed).iter().all(|w| is_covered(w)) {
        None
    } else {
        Some(trimmed)
    }
}

/// The `/model` selector search should rank exact provider-prefixed queries before proxy-provider IDs
/// like `openrouter/openai/gpt-5`, so keep the bare model ID out of the leading position.
pub fn get_model_selector_search_text(id: &str, provider: &str, name: Option<&str>) -> String {
    let name_part = match name {
        Some(n) if !n.is_empty() => format!(" {n}"),
        _ => String::new(),
    };
    format!("{provider} {provider}/{id} {provider} {id}{name_part}")
}
