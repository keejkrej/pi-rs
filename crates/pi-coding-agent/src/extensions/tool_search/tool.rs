//! Port of packages/coding-agent/src/extensions/tool-search/tool.ts

use std::sync::{Arc, LazyLock};

use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::extensions::types::{
    ExtensionApi, ToolDefinition, ToolExposure, ToolInfo, ToolNamespace, ToolReachScope,
};
use pi_agent_core::AgentToolResult;
use pi_js::vendor::typebox::{Schema, t};
use pi_js::{Error, Result};

pub const TOOL_SEARCH_TOOL_NAME: &str = "tool_search";
pub const DEFAULT_TOOL_SEARCH_LIMIT: usize = 8;

/// The `tool_search` description. It does not list the searchable tools or their namespaces, so it
/// stays the same while tools are registered, for example when MCP servers connect.
pub const TOOL_SEARCH_DESCRIPTION: &str = "# Tool discovery\n\nSearches over deferred tool metadata with BM25 and exposes matching tools for the next model call.\n\nSome of the tools, such as tools of MCP servers, may not have been provided to you upfront, and you should use this tool (`tool_search`) to search for the required tools. For MCP tool discovery, always use `tool_search`.";

/// A tool as the ranker sees it: its name and the texts built by [`create_tool_search_document`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSearchDocument {
    pub name: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace_text: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSearchMatch {
    pub name: String,
    pub score: f64,
}

/// Ranks tools for a query. BM25 today; a hybrid ranker with embeddings can replace it.
pub trait ToolRanker: Send + Sync {
    fn rank(&self, query: &str, documents: &[ToolSearchDocument], limit: usize) -> Vec<ToolSearchMatch>;
}

static STOP_WORDS: LazyLock<IndexSet<&'static str>> = LazyLock::new(|| {
    IndexSet::from([
        "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "is", "it", "of", "on", "or", "that",
        "the", "this", "to", "with",
    ])
});

static CAMEL_SPLIT_LOWER_UPPER: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"([a-z0-9])([A-Z])").expect("valid camel regex 1"));
static CAMEL_SPLIT_UPPER_LOWER: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"([A-Z]+)([A-Z][a-z])").expect("valid camel regex 2"));
static NON_WORD_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"[^\p{L}\p{M}\p{N}]+").expect("valid non-word regex"));
static NORMALIZE_NAME_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"[^a-z0-9_$]").expect("valid normalize name regex"));
static STEM_ES_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(ches|shes|sses|xes|zes)$").expect("valid stem regex"));

/// Naive singular form, so `issues` matches `issue` and `searches` matches `search`.
fn stem(term: &str) -> String {
    if term.len() > 4
        && let Some(prefix) = term.strip_suffix("ies")
    {
        return format!("{prefix}y");
    }
    if term.len() > 4 && STEM_ES_RE.is_match(term) {
        return term[..term.len() - 2].to_string();
    }
    if term.len() > 3 && term.ends_with('s') && !term.ends_with("ss") {
        return term[..term.len() - 1].to_string();
    }
    term.to_string()
}

/// Unicode word segmentation with identifier boundaries and English stop words/singular forms.
pub fn tokenize(text: &str) -> Vec<String> {
    let nfc = pi_js::str16::normalize_nfc(text);
    let split1 = CAMEL_SPLIT_LOWER_UPPER.replace_all(&nfc, "$1 $2");
    let split2 = CAMEL_SPLIT_UPPER_LOWER.replace_all(&split1, "$1 $2");
    let lower = split2.to_lowercase();
    let cleaned = NON_WORD_RE.replace_all(&lower, " ");
    let is_ascii_plain = cleaned.is_ascii()
        && cleaned
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b' ');
    let words: Vec<&str> = if is_ascii_plain {
        cleaned.split(' ').collect()
    } else {
        pi_js::intl::words(&cleaned)
            .into_iter()
            .filter(|seg| seg.is_word_like == Some(true))
            .map(|seg| seg.segment)
            .collect()
    };
    words
        .into_iter()
        .filter(|word| !word.is_empty() && !STOP_WORDS.contains(word))
        .map(|word| {
            if word.is_ascii() && word.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()) {
                stem(word)
            } else {
                word.to_string()
            }
        })
        .collect()
}

/// Resolve a local JSON Pointer (`#` or `#/...`) within `root`.
/// Malformed escapes or non-local refs return `None`.
pub(crate) fn resolve_schema_ref<'a>(r#ref: &str, root: &'a Value) -> Option<&'a Value> {
    if r#ref == "#" {
        return Some(root);
    }
    let rest = r#ref.strip_prefix("#/")?;
    let mut current = root;
    for segment in rest.split('/') {
        if segment.is_empty() {
            continue;
        }
        let decoded = pi_js::uri::decode_uri_component(segment).ok()?;
        let key = decoded.replace("~1", "/").replace("~0", "~");
        let Value::Object(map) = current else {
            return None;
        };
        current = map.get(&key)?;
    }
    Some(current)
}

/// Schema descriptions and property names, recursively, following local `$ref`s into `root`.
/// `seen` holds the subschemas already reached through a reference (by pointer address inside
/// `root`), so recursive and shared definitions are read once and unreferenced definitions stay out.
fn schema_text(schema: &Value, root: &Value, parts: &mut Vec<String>, seen: &mut IndexSet<usize>) {
    let Value::Object(map) = schema else {
        return;
    };
    if let Some(Value::String(ref_str)) = map.get("$ref")
        && let Some(target) = resolve_schema_ref(ref_str, root)
        && target.is_object()
    {
        let ptr = target as *const Value as usize;
        if seen.insert(ptr) {
            schema_text(target, root, parts, seen);
        }
    }
    if let Some(Value::String(desc)) = map.get("description") {
        parts.push(desc.clone());
    }
    if let Some(Value::Object(props)) = map.get("properties") {
        for (name, property) in props {
            parts.push(name.clone());
            schema_text(property, root, parts, seen);
        }
    }
    if let Some(items) = map.get("items") {
        schema_text(items, root, parts, seen);
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(Value::Array(variants)) = map.get(key) {
            for variant in variants {
                schema_text(variant, root, parts, seen);
            }
        }
    }
}

/// Subset of `ToolInfo` needed to build a [`ToolSearchDocument`].
pub struct ToolSearchDocumentInput<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub parameters: &'a pi_schema::Schema,
}

/// Search text of a tool: the description and the schema descriptions and property names, and
/// separately the namespace name, description, and instructions.
pub fn create_tool_search_document<'a>(
    tool: impl Into<ToolSearchDocumentInput<'a>>,
    namespace: Option<&ToolNamespace>,
) -> ToolSearchDocument {
    let tool = tool.into();
    let mut parts = vec![tool.description.to_string()];
    let root = tool.parameters.as_value();
    let mut seen = IndexSet::new();
    seen.insert(root as *const Value as usize);
    schema_text(root, root, &mut parts, &mut seen);
    let namespace_text = namespace.map(|ns| {
        format!(
            "{} {} {}",
            ns.name,
            ns.description.as_deref().unwrap_or(""),
            ns.instructions.as_deref().unwrap_or("")
        )
    });
    let text = parts
        .into_iter()
        .filter(|part| !pi_js::str16::trim(part).is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    ToolSearchDocument {
        name: tool.name.to_string(),
        text,
        namespace_text,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Bm25Options {
    pub k1: f64,
    pub b: f64,
}

impl Default for Bm25Options {
    fn default() -> Self {
        Self { k1: 1.2, b: 0.75 }
    }
}

fn normalize_name(name: &str) -> String {
    NORMALIZE_NAME_RE
        .replace_all(&name.trim().to_ascii_lowercase(), "_")
        .into_owned()
}

fn is_exact_name_query(exact_query: &str, name: &str) -> bool {
    let normalized = normalize_name(name);
    normalized == exact_query || normalized.ends_with(&format!("__{exact_query}"))
}

struct IndexedDocument<'a> {
    name: &'a str,
    name_terms: IndexSet<String>,
    local_name_terms: Vec<String>,
    text_counts: IndexMap<String, usize>,
    text_length: usize,
    namespace_terms: IndexSet<String>,
}

fn index_document(doc: &ToolSearchDocument) -> IndexedDocument<'_> {
    let mut text_counts = IndexMap::new();
    for term in tokenize(&doc.text) {
        *text_counts.entry(term).or_insert(0) += 1;
    }
    let text_length = text_counts.values().sum();
    let last_seg = doc.name.rsplit("__").next().unwrap_or(&doc.name);
    let local_set: IndexSet<String> = tokenize(last_seg).into_iter().collect();
    IndexedDocument {
        name: &doc.name,
        name_terms: tokenize(&doc.name).into_iter().collect(),
        local_name_terms: local_set.into_iter().collect(),
        text_counts,
        text_length,
        namespace_terms: tokenize(doc.namespace_text.as_deref().unwrap_or(""))
            .into_iter()
            .collect(),
    }
}

const NAME_WEIGHT: f64 = 3.0;
const NAMESPACE_WEIGHT: f64 = 0.5;

/// Okapi BM25 over description + name + namespace weights. Ties keep document order.
#[derive(Clone, Copy, Debug, Default)]
pub struct Bm25Ranker {
    k1: f64,
    b: f64,
}

impl Bm25Ranker {
    pub fn new(options: Option<Bm25Options>) -> Self {
        let opts = options.unwrap_or_default();
        Self { k1: opts.k1, b: opts.b }
    }

    pub fn rank(&self, query: &str, documents: &[ToolSearchDocument], limit: usize) -> Vec<ToolSearchMatch> {
        let query_terms: IndexSet<String> = tokenize(query).into_iter().collect();
        if query_terms.is_empty() || documents.is_empty() || limit == 0 {
            return Vec::new();
        }
        let indexed: Vec<IndexedDocument<'_>> = documents.iter().map(index_document).collect();
        let total_len: usize = indexed.iter().map(|d| d.text_length).sum();
        let avg = (total_len as f64) / (indexed.len() as f64);
        let average_length = if avg == 0.0 { 1.0 } else { avg };
        let doc_count = indexed.len() as f64;

        let idf: IndexMap<&str, f64> = query_terms
            .iter()
            .map(|term| {
                let frequency = indexed
                    .iter()
                    .filter(|d| {
                        d.name_terms.contains(term.as_str())
                            || d.text_counts.contains_key(term.as_str())
                            || d.namespace_terms.contains(term.as_str())
                    })
                    .count() as f64;
                let value = (1.0 + (doc_count - frequency + 0.5) / (frequency + 0.5)).ln();
                (term.as_str(), value)
            })
            .collect();

        let full_name_bonus: f64 = idf.values().sum();
        let exact_query = normalize_name(query);
        struct RankedMatch {
            name: String,
            score: f64,
            covered: usize,
            exact: bool,
        }
        let mut matches: Vec<RankedMatch> = Vec::new();
        for doc in &indexed {
            let norm = self.k1 * (1.0 - self.b + (self.b * (doc.text_length as f64)) / average_length);
            let mut score = 0.0;
            let mut covered = 0usize;
            for term in &query_terms {
                let count = doc.text_counts.get(term.as_str()).copied().unwrap_or(0) as f64;
                let name_w = if doc.name_terms.contains(term.as_str()) {
                    NAME_WEIGHT
                } else {
                    0.0
                };
                let ns_w = if doc.namespace_terms.contains(term.as_str()) {
                    NAMESPACE_WEIGHT
                } else {
                    0.0
                };
                let text_w = (count * (self.k1 + 1.0)) / (count + norm);
                let weight = name_w + ns_w + text_w;
                if weight > 0.0 {
                    covered += 1;
                }
                score += idf.get(term.as_str()).copied().unwrap_or(0.0) * weight;
            }
            if score == 0.0 {
                continue;
            }
            if !doc.local_name_terms.is_empty() && doc.local_name_terms.iter().all(|t| query_terms.contains(t)) {
                score += full_name_bonus;
            }
            matches.push(RankedMatch {
                name: doc.name.to_string(),
                score,
                covered,
                exact: is_exact_name_query(&exact_query, doc.name),
            });
        }
        matches.sort_by(|a, b| {
            b.exact
                .cmp(&a.exact)
                .then_with(|| b.covered.cmp(&a.covered))
                .then_with(|| b.score.total_cmp(&a.score))
        });
        matches.truncate(limit);
        matches
            .into_iter()
            .map(|m| ToolSearchMatch {
                name: m.name,
                score: m.score,
            })
            .collect()
    }
}

impl ToolRanker for Bm25Ranker {
    fn rank(&self, query: &str, documents: &[ToolSearchDocument], limit: usize) -> Vec<ToolSearchMatch> {
        Bm25Ranker::rank(self, query, documents, limit)
    }
}

pub fn tool_search_schema() -> Schema {
    t::object([
        (
            "query",
            t::string().with("description", "Search query for deferred tools."),
        ),
        (
            "limit",
            t::number()
                .with(
                    "description",
                    format!("Maximum number of tools to return. Defaults to {DEFAULT_TOOL_SEARCH_LIMIT}."),
                )
                .optional(),
        ),
    ])
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSearchResultTool {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSearchToolDetails {
    /// Tools loaded by this call.
    pub loaded: Vec<String>,
}

/// Subset of `ExtensionApi` needed by `tool_search`.
pub trait ToolSearchToolsBackend: Send + Sync {
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>>;
    fn get_active_tools(&self) -> Result<Vec<String>>;
    fn set_active_tools(&self, tool_names: Vec<String>) -> Result<()>;
}

impl ToolSearchToolsBackend for ExtensionApi {
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>> {
        ExtensionApi::get_all_tools(self)
    }

    fn get_active_tools(&self) -> Result<Vec<String>> {
        ExtensionApi::get_active_tools(self)
    }

    fn set_active_tools(&self, tool_names: Vec<String>) -> Result<()> {
        ExtensionApi::set_active_tools(self, tool_names)
    }
}

#[derive(Clone, Default)]
pub struct ToolSearchToolOptions {
    pub tools: Option<Arc<dyn ToolSearchToolsBackend>>,
}

/// Whether `tool_search` can load a tool with this exposure.
fn is_searchable(exposure: ToolExposure) -> bool {
    matches!(exposure, ToolExposure::Codemode | ToolExposure::Deferred)
}

/// Rank the searchable tools that are not active yet and activate the matches, so the next model
/// call declares them. Activation is recorded in the transcript like any tool change.
fn search_and_load(tools: &dyn ToolSearchToolsBackend, query: &str, limit: usize) -> Result<Vec<ToolSearchResultTool>> {
    let active = tools.get_active_tools()?;
    let active_set: IndexSet<&str> = active.iter().map(String::as_str).collect();
    let all_tools = tools.get_all_tools()?;
    let candidates: Vec<&ToolInfo> = all_tools
        .iter()
        .filter(|tool| is_searchable(tool.exposure) && !active_set.contains(tool.name.as_str()))
        .collect();
    let documents: Vec<ToolSearchDocument> = candidates
        .iter()
        .map(|tool| {
            let schema = pi_schema::Schema::from(tool.parameters.clone());
            create_tool_search_document(
                ToolSearchDocumentInput {
                    name: &tool.name,
                    description: &tool.description,
                    parameters: &schema,
                },
                tool.namespace.as_ref(),
            )
        })
        .collect();
    let matches = Bm25Ranker::default().rank(query, &documents, limit);
    if !matches.is_empty() {
        let mut next = active;
        for m in &matches {
            next.push(m.name.clone());
        }
        tools.set_active_tools(next)?;
    }
    Ok(matches
        .into_iter()
        .map(|m| {
            let description = candidates
                .iter()
                .find(|t| t.name == m.name)
                .map(|t| t.description.clone())
                .unwrap_or_default();
            ToolSearchResultTool {
                name: m.name,
                description,
            }
        })
        .collect())
}

pub fn create_tool_search_tool_definition(options: ToolSearchToolOptions) -> ToolDefinition {
    let tools_opt = options.tools;
    ToolDefinition {
        name: TOOL_SEARCH_TOOL_NAME.to_string(),
        label: TOOL_SEARCH_TOOL_NAME.to_string(),
        description: TOOL_SEARCH_DESCRIPTION.to_string(),
        prompt_snippet: Some("Search for tools that are not loaded yet and load the matches".to_string()),
        prompt_guidelines: None,
        parameters: tool_search_schema(),
        constrained_sampling: None,
        render_shell: None,
        prepare_arguments: None,
        output_schema: None,
        // Searching is not something scripts need; it changes what the model sees.
        exposure: Some(ToolExposure::ModelOnly),
        namespace: None,
        annotations: None,
        default_active: None,
        prepare_loadout: None,
        execution_mode: None,
        execute: Arc::new(move |_tool_call_id, params, _signal, _on_update, _ctx| {
            let tools_opt = tools_opt.clone();
            Box::pin(async move {
                let query = params
                    .get("query")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::msg("query must be a string"))?
                    .to_string();
                if pi_js::str16::trim(&query).is_empty() {
                    return Err(Error::msg("query must not be empty"));
                }
                let max = match params.get("limit") {
                    None | Some(Value::Null) => DEFAULT_TOOL_SEARCH_LIMIT,
                    Some(v) => {
                        let Some(n) = v.as_f64() else {
                            return Err(Error::msg("limit must be a positive integer"));
                        };
                        if !n.is_finite() || n.fract() != 0.0 || n <= 0.0 {
                            return Err(Error::msg("limit must be a positive integer"));
                        }
                        n as usize
                    }
                };
                let loaded_tools = match tools_opt {
                    Some(ref backend) => search_and_load(backend.as_ref(), &query, max)?,
                    None => Vec::new(),
                };
                let text = if loaded_tools.is_empty() {
                    "No matching tools found.".to_string()
                } else {
                    let noun = if loaded_tools.len() == 1 { "tool" } else { "tools" };
                    let bullets: Vec<String> = loaded_tools
                        .iter()
                        .map(|tool| {
                            let first_line = pi_js::str16::trim(&tool.description)
                                .split(['\r', '\n'])
                                .next()
                                .unwrap_or("");
                            format!("- {}: {first_line}", tool.name)
                        })
                        .collect();
                    format!(
                        "Loaded {} {noun}. They are available from your next call:\n{}",
                        loaded_tools.len(),
                        bullets.join("\n")
                    )
                };
                let loaded_names: Vec<String> = loaded_tools.into_iter().map(|t| t.name).collect();
                let mut result = AgentToolResult::text(text);
                result.details = Some(json!({ "loaded": loaded_names }));
                Ok(result)
            })
        }),
        render_call: None,
        render_result: None,
    }
    .with_reaches_tools(Arc::new(|_| ToolReachScope::All))
}
