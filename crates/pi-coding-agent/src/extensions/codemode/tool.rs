//! Port of packages/coding-agent/src/extensions/codemode/tool.ts

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::extensions::types::mcp_wire::CodemodeMode;
use crate::core::extensions::types::{
    ConstrainedSamplingOption, ToolDefinition, ToolExposure, ToolLoadoutBackend, ToolLoadoutChanges, ToolNamespace,
};
use crate::core::tools::tool_definition_wrapper::wrap_tool_definition;
use crate::extensions::codemode::execute_lazy::execute_codemode_lazy;
use crate::extensions::codemode::renderer::{codemode_render_call, codemode_render_result};
use crate::extensions::tool_search::tool::resolve_schema_ref;
use crate::extensions::wire::get_docs_path;
use pi_agent_core::AgentTool;
use pi_ai::{ConstrainedSamplingConfig, GrammarFormat};
use pi_codemode::declarations::{
    MCP_TYPESCRIPT_PREAMBLE, mcp_structured_content_schema, render_tool_output_type, render_tool_sample,
    to_codemode_identifier,
};
use pi_codemode::source::CODEMODE_SOURCE_GRAMMAR;
use pi_codemode::types::{CodemodeJsonSchema, CodemodeTool};
use pi_js::abort::AbortSignal;
use pi_js::vendor::typebox::{Schema, t};
use pi_js::{Error, Result};

pub const CODEMODE_TOOL_NAME: &str = "codemode";
/// Custom entry type holding one script's `store()` writes: [`CodemodeStoreEntryData`].
pub const CODEMODE_STORE_ENTRY_TYPE: &str = "codemode-store";
/// Default for [`CodemodeDescriptionOptions::inline_budget`], in estimated tokens.
pub const DEFAULT_CODEMODE_INLINE_BUDGET: usize = 3000;
const CHARS_PER_TOKEN: usize = 4;

pub const CODEMODE_SNIPPET: &str = "Run JavaScript that calls other tools";
pub const CODEMODE_GUIDELINES: &[&str] = &[
    "Use codemode to batch independent tool calls (Promise.allSettled), chain them, or filter large output, instead of many separate calls.",
];

pub fn codemode_docs_path() -> String {
    pi_js::path::join(&[&get_docs_path(), "codemode.md"])
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeStoreEntryData {
    pub set: IndexMap<String, Value>,
    #[serde(rename = "delete")]
    pub r#delete: Vec<String>,
}

/// The part of the model registry that scripts reach through `models`.
// WIRE(@mcp-w3): wire to ModelRegistry once ModelRegistryBackend adds typed lookup/classify/image methods
#[async_trait]
pub trait CodemodeModelRuntime: Send + Sync {
    fn get_models_of_type(&self, model_type: &str, provider: Option<&str>) -> Vec<Value>;
    async fn get_available_of_type(
        &self,
        model_type: &str,
        provider: Option<&str>,
        signal: Option<AbortSignal>,
    ) -> Result<Vec<Value>>;
    fn get_model_of_type(&self, model_type: &str, provider: &str, id: &str) -> Option<Value>;
    async fn classify(&self, model: Value, context: Value, signal: Option<AbortSignal>) -> Result<Value>;
    async fn generate_images(&self, model: Value, context: Value, signal: Option<AbortSignal>) -> Result<Value>;
}

pub type GetToolNamespaceFn = Arc<dyn Fn(&str) -> Option<ToolNamespace> + Send + Sync>;
pub type AppendEntryFn = Arc<dyn Fn(&str, CodemodeStoreEntryData) + Send + Sync>;
pub type GetModeFn = Arc<dyn Fn() -> CodemodeMode + Send + Sync>;
pub type GetInlineBudgetFn = Arc<dyn Fn() -> Option<usize> + Send + Sync>;

#[derive(Clone, Default)]
pub struct CodemodeToolOptions {
    /// Namespace of a tool, for `searchTools()` ranking and its `namespace` filter.
    pub get_tool_namespace: Option<GetToolNamespaceFn>,
    /// Expose the `models` namespace to scripts, backed by the session's model registry.
    pub models: bool,
    /// Optional explicit model runtime adapter (overrides `ctx.model_registry()` when provided).
    pub model_runtime: Option<Arc<dyn CodemodeModelRuntime>>,
    /// Persists `store()` writes as a session custom entry.
    pub append_entry: Option<AppendEntryFn>,
    /// How the tool presents the loadout while active (`codemode.mode`). Default: `On`.
    pub get_mode: Option<GetModeFn>,
    /// Token budget for tool declarations in the description. Default: [`DEFAULT_CODEMODE_INLINE_BUDGET`].
    pub get_inline_budget: Option<GetInlineBudgetFn>,
}

pub fn codemode_schema() -> Schema {
    t::object([("code", t::string().with("description", "Raw JavaScript source."))])
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeToolInput {
    pub code: String,
}

/// Whether a registered tool is this package's `codemode` tool rather than another extension's
/// tool with the same name.
pub fn is_codemode_tool(name: &str, parameters: &Schema) -> bool {
    name == CODEMODE_TOOL_NAME && parameters.to_value() == codemode_schema().to_value()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodemodeNestedCallStatus {
    Running,
    Ok,
    Error,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeNestedCall {
    pub id: String,
    pub name: String,
    pub args: String,
    pub status: CodemodeNestedCallStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeToolDetails {
    pub calls: Vec<CodemodeNestedCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
}

fn text_output_schema() -> CodemodeJsonSchema {
    let mut map = IndexMap::new();
    map.insert("type".to_string(), Value::String("string".to_string()));
    CodemodeJsonSchema::Object(map)
}

pub(crate) fn value_to_codemode_schema(value: &Value) -> CodemodeJsonSchema {
    match value {
        Value::Bool(b) => CodemodeJsonSchema::Boolean(*b),
        Value::Object(obj) => {
            let mut map = IndexMap::with_capacity(obj.len());
            for (k, v) in obj {
                map.insert(k.clone(), v.clone());
            }
            CodemodeJsonSchema::Object(map)
        }
        _ => CodemodeJsonSchema::Object(IndexMap::new()),
    }
}

pub(crate) fn codemode_schema_to_value(schema: &CodemodeJsonSchema) -> Value {
    match schema {
        CodemodeJsonSchema::Boolean(b) => Value::Bool(*b),
        CodemodeJsonSchema::Object(map) => {
            let mut obj = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                obj.insert(k.clone(), v.clone());
            }
            Value::Object(obj)
        }
    }
}

/// What a script sees of a tool. Tools without an output schema resolve to their text output.
pub fn to_codemode_declaration(tool: &AgentTool) -> CodemodeTool {
    let input_val = tool.parameters.to_value();
    let output_schema = tool
        .output_schema
        .as_ref()
        .map(value_to_codemode_schema)
        .unwrap_or_else(text_output_schema);
    CodemodeTool {
        name: tool.name.clone(),
        description: Some(tool.description.clone()),
        input_schema: Some(value_to_codemode_schema(&input_val)),
        output_schema: Some(output_schema),
        spread: None,
        signature: None,
        execute: Arc::new(|_, _| Box::pin(async { Ok(None) })),
    }
}

/// Tools a script may call: every given tool except the codemode tool itself.
pub fn get_codemode_callable_tools(tools: &[AgentTool]) -> Vec<AgentTool> {
    tools
        .iter()
        .filter(|tool| tool.name != CODEMODE_TOOL_NAME)
        .cloned()
        .collect()
}

#[derive(Clone, Default)]
pub struct CodemodeDescriptionOptions {
    pub models: bool,
    pub namespaces: IndexMap<String, ToolNamespace>,
    pub deferred: IndexSet<String>,
    pub inline_budget: Option<usize>,
}

const DESCRIPTION_INTRO: &str = "Run JavaScript that calls other tools. The input is raw JavaScript (not JSON, no code fence), run as an async function body in a QuickJS sandbox: top-level `await` and `return` work. No Node, file system, network, or timers.\n- `await tools.<name>({ ...args })` resolves to a string, or an object if the tool's declaration says so, and rejects with an Error on failure. Calls still running when the script ends are cancelled.\n- Optional first line: `// @options: {\"max_output_tokens\": 10000, \"timeout_ms\": 60000}`";

fn describe_globals(models: bool) -> String {
    let mut lines = vec![
        "Globals:".to_string(),
        "- `text(value)`, `image(dataUrlOrImageBlock)`, `console.log(...)`, and top-level `return` add output; `exit()` ends the script.".to_string(),
        "- `store(key, value)` and `load(key)` keep JSON values across codemode calls.".to_string(),
        "- `ALL_TOOLS`, `searchTools(query, { limit?, namespace? })`, `describeTool(name)`, `describeNamespace(name)`: find unlisted tools, such as MCP tools.".to_string(),
    ];
    if models {
        lines.push(format!(
            "- `models`: classifiers and image generation. Read {} first.",
            codemode_docs_path()
        ));
    }
    lines.join("\n")
}

fn render_tool_section(declaration: &CodemodeTool) -> String {
    let id = to_codemode_identifier(&declaration.name);
    let heading = if id == declaration.name {
        format!("### `{id}`")
    } else {
        format!("### `{id}` (`{}`)", declaration.name)
    };
    let rendered = render_tool_sample(declaration, None);
    format!("{heading}\n{}", pi_js::str16::trim(&rendered))
}

#[derive(Clone)]
struct CatalogEntry {
    name: String,
    section: String,
    cost: usize,
}

#[derive(Clone)]
struct CatalogGroup {
    namespace: Option<ToolNamespace>,
    entries: Vec<CatalogEntry>,
}

fn select_catalog(groups: &[CatalogGroup], budget: Option<usize>) -> IndexSet<String> {
    let Some(budget) = budget else {
        let mut all = IndexSet::new();
        for g in groups {
            for entry in &g.entries {
                all.insert(entry.name.clone());
            }
        }
        return all;
    };
    let mut queues: Vec<Vec<&CatalogEntry>> = groups
        .iter()
        .map(|g| {
            let mut entries: Vec<&CatalogEntry> = g.entries.iter().collect();
            entries.sort_by_key(|e| e.cost);
            entries
        })
        .collect();
    let mut shown = IndexSet::new();
    let mut remaining = budget;
    queues.retain(|q| !q.is_empty());
    while !queues.is_empty() {
        let mut next_queues = Vec::with_capacity(queues.len());
        for mut queue in queues {
            let next = queue[0];
            if next.cost > remaining {
                continue;
            }
            remaining -= next.cost;
            shown.insert(next.name.clone());
            queue.remove(0);
            if !queue.is_empty() {
                next_queues.push(queue);
            }
        }
        queues = next_queues;
    }
    shown
}

pub fn create_codemode_description(tools: &[AgentTool], options: CodemodeDescriptionOptions) -> String {
    let declarations: Vec<CodemodeTool> = get_codemode_callable_tools(tools)
        .into_iter()
        .filter(|tool| !options.deferred.contains(&tool.name))
        .map(|tool| to_codemode_declaration(&tool))
        .collect();
    let mut groups: IndexMap<String, CatalogGroup> = IndexMap::new();
    groups.insert(
        String::new(),
        CatalogGroup {
            namespace: None,
            entries: Vec::new(),
        },
    );
    for declaration in &declarations {
        let namespace = options.namespaces.get(&declaration.name).cloned();
        let key = match &namespace {
            Some(ns) => format!("ns:{}", ns.name),
            None => String::new(),
        };
        let group = groups.entry(key).or_insert_with(|| CatalogGroup {
            namespace: namespace.clone(),
            entries: Vec::new(),
        });
        let section = render_tool_section(declaration);
        let cost = section.len().div_ceil(CHARS_PER_TOKEN);
        group.entries.push(CatalogEntry {
            name: declaration.name.clone(),
            section,
            cost,
        });
    }
    let mut ordered: Vec<CatalogGroup> = groups.into_values().collect();
    ordered.sort_by(|a, b| match (&a.namespace, &b.namespace) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(na), Some(nb)) => pi_js::intl::locale_compare(&na.name, &nb.name),
    });
    let shown = select_catalog(&ordered, options.inline_budget);

    let mut sections = vec![DESCRIPTION_INTRO.to_string(), describe_globals(options.models)];
    let has_mcp_types = declarations.iter().any(|declaration| {
        shown.contains(&declaration.name) && mcp_structured_content_schema(declaration.output_schema.as_ref()).is_some()
    });
    if has_mcp_types {
        sections.push(format!("Shared MCP Types:\n```ts\n{MCP_TYPESCRIPT_PREAMBLE}\n```"));
    }
    if declarations.is_empty() {
        return sections.join("\n\n");
    }

    let mut tool_sections = vec!["Nested tools:".to_string()];
    for group in ordered {
        let visible: Vec<&CatalogEntry> = group.entries.iter().filter(|e| shown.contains(&e.name)).collect();
        if let Some(namespace) = &group.namespace {
            let listing = if visible.len() == group.entries.len() {
                ""
            } else if visible.is_empty() {
                " (tools not listed)"
            } else {
                " (some tools not listed)"
            };
            let description = namespace
                .description
                .as_deref()
                .map(|d| pi_js::str16::trim(d))
                .filter(|d| !d.is_empty());
            match description {
                Some(desc) => tool_sections.push(format!("## {}{listing}\n{desc}", namespace.name)),
                None => tool_sections.push(format!("## {}{listing}", namespace.name)),
            }
        }
        for entry in visible {
            tool_sections.push(entry.section.clone());
        }
    }
    sections.push(tool_sections.join("\n\n"));
    sections.join("\n\n")
}

/// The subschema the local `$ref` chain of `schema` names in `root`.
/// Remote, missing, and cyclic references stop at the schema holding them (#11803 / 6d3e342b2).
fn follow_ref<'a>(schema: &'a Value, root: &'a Value) -> &'a Value {
    let mut seen: IndexSet<usize> = IndexSet::new();
    let mut current = schema;
    while let Value::Object(map) = current {
        let ptr = current as *const Value as usize;
        if !seen.insert(ptr) {
            break;
        }
        let Some(Value::String(ref_str)) = map.get("$ref") else {
            break;
        };
        let Some(target) = resolve_schema_ref(ref_str, root) else {
            break;
        };
        current = target;
    }
    current
}

fn describe_fields(schema: &Value, root: &Value, depth: usize) -> Option<String> {
    let resolved = follow_ref(schema, root);
    let Value::Object(map) = resolved else {
        return None;
    };
    if map.get("type").and_then(Value::as_str) != Some("object") {
        return None;
    }
    let Value::Object(props) = map.get("properties")? else {
        return None;
    };
    let required_names: IndexSet<&str> = map
        .get("required")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let fields: Vec<String> = props
        .iter()
        .map(|(name, field)| {
            let resolved_field = follow_ref(field, root);
            let const_val = resolved_field.as_object().and_then(|o| o.get("const"));
            let nested = if let Some(cv) = const_val {
                Some(pi_js::json::stringify(cv))
            } else if depth > 0 {
                describe_fields(field, root, depth - 1)
            } else {
                None
            };
            let opt = if required_names.contains(name.as_str()) {
                ""
            } else {
                "?"
            };
            match nested {
                Some(n) => format!("{name}{opt}: {n}"),
                None => format!("{name}{opt}"),
            }
        })
        .collect();
    Some(format!("{{ {} }}", fields.join(", ")))
}

fn describe_union_fields(schema: &Value, root: &Value) -> Option<String> {
    let resolved = follow_ref(schema, root);
    let Value::Object(map) = resolved else {
        return None;
    };
    let variants = map
        .get("anyOf")
        .or_else(|| map.get("oneOf"))
        .and_then(Value::as_array)?;
    if variants.is_empty() {
        return None;
    }
    let described: Option<Vec<String>> = variants.iter().map(|v| describe_fields(v, root, 0)).collect();
    described.map(|d| d.join(" | "))
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn describe_output(schema: Option<&CodemodeJsonSchema>) -> String {
    let rendered = render_tool_output_type(schema);
    if rendered == "string" {
        return "a string".to_string();
    }
    if let Some(s) = schema
        && mcp_structured_content_schema(Some(s)).is_none()
    {
        let root_val = codemode_schema_to_value(s);
        if let Some(fields) =
            describe_fields(&root_val, &root_val, 1).or_else(|| describe_union_fields(&root_val, &root_val))
        {
            return format!("`{fields}`");
        }
    }
    format!("`{}`", collapse_whitespace(&rendered))
}

fn describe_script_call(tool: &AgentTool) -> String {
    let decl = to_codemode_declaration(tool);
    format!(
        "{}\n\nCodemode: `tools.{}(args)` resolves to {}.",
        pi_js::str16::trim(&tool.description),
        to_codemode_identifier(&tool.name),
        describe_output(decl.output_schema.as_ref()),
    )
}

fn prepare_codemode_loadout(loadout: &dyn ToolLoadoutBackend, options: &CodemodeToolOptions) -> ToolLoadoutChanges {
    let mode = options.get_mode.as_ref().map(|f| f()).unwrap_or(CodemodeMode::On);
    let is_direct = |tool: &AgentTool| loadout.get_exposure(&tool.name) == ToolExposure::Direct;
    let callable = get_codemode_callable_tools(loadout.callable());
    let callable_names: IndexSet<String> = callable.iter().map(|t| t.name.clone()).collect();

    let mut descriptions = IndexMap::new();
    if mode == CodemodeMode::On {
        for tool in loadout.declared() {
            if callable_names.contains(&tool.name) {
                descriptions.insert(tool.name.clone(), describe_script_call(tool));
            }
        }
    }
    let listed: Vec<AgentTool> = if mode == CodemodeMode::Only {
        callable.clone()
    } else {
        callable.iter().filter(|t| !is_direct(t)).cloned().collect()
    };
    let mut namespaces = IndexMap::new();
    let mut deferred = IndexSet::new();
    for tool in &listed {
        if let Some(ns) = loadout.get_namespace(&tool.name) {
            namespaces.insert(tool.name.clone(), ns);
        }
        if loadout.get_exposure(&tool.name) == ToolExposure::Deferred {
            deferred.insert(tool.name.clone());
        }
    }
    let inline_budget = match &options.get_inline_budget {
        Some(f) => f(),
        None => Some(DEFAULT_CODEMODE_INLINE_BUDGET),
    };
    descriptions.insert(
        CODEMODE_TOOL_NAME.to_string(),
        create_codemode_description(
            &listed,
            CodemodeDescriptionOptions {
                models: options.models,
                namespaces,
                deferred,
                inline_budget,
            },
        ),
    );
    let declared_names: IndexSet<&str> = loadout.declared().iter().map(|t| t.name.as_str()).collect();
    let hidden_declarations = if mode == CodemodeMode::Only {
        callable
            .iter()
            .filter(|t| is_direct(t) && declared_names.contains(t.name.as_str()))
            .map(|t| t.name.clone())
            .collect()
    } else {
        Vec::new()
    };
    ToolLoadoutChanges {
        descriptions: Some(descriptions),
        hidden_declarations: Some(hidden_declarations),
    }
}

pub fn create_codemode_tool_definition(options: CodemodeToolOptions) -> ToolDefinition {
    let initial_description = create_codemode_description(
        &[],
        CodemodeDescriptionOptions {
            models: options.models,
            namespaces: IndexMap::new(),
            deferred: IndexSet::new(),
            inline_budget: None,
        },
    );
    let opts_loadout = options.clone();
    let opts_exec = options.clone();

    let mut grammar_variants = IndexMap::new();
    grammar_variants.insert(GrammarFormat::OpenAILark, CODEMODE_SOURCE_GRAMMAR.to_string());

    ToolDefinition {
        name: CODEMODE_TOOL_NAME.to_string(),
        label: CODEMODE_TOOL_NAME.to_string(),
        description: initial_description,
        prompt_snippet: Some(CODEMODE_SNIPPET.to_string()),
        prompt_guidelines: Some(CODEMODE_GUIDELINES.iter().map(|s| (*s).to_string()).collect()),
        parameters: codemode_schema(),
        constrained_sampling: Some(ConstrainedSamplingOption::Config(ConstrainedSamplingConfig::Grammar {
            variants: grammar_variants,
        })),
        render_shell: None,
        prepare_arguments: None,
        output_schema: None,
        exposure: Some(ToolExposure::ModelOnly),
        namespace: None,
        annotations: None,
        default_active: None,
        prepare_loadout: Some(Arc::new(move |loadout| {
            Ok(Some(prepare_codemode_loadout(loadout, &opts_loadout)))
        })),
        execution_mode: None,
        execute: Arc::new(move |tool_call_id, params, signal, on_update, ctx| {
            let opts = opts_exec.clone();
            Box::pin(async move {
                let code = params
                    .get("code")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::msg("code must be a string"))?
                    .to_string();
                execute_codemode_lazy(
                    tool_call_id,
                    CodemodeToolInput { code },
                    signal,
                    on_update,
                    Some(ctx),
                    opts,
                )
                .await
            })
        }),
        render_call: Some(codemode_render_call()),
        render_result: Some(codemode_render_result()),
    }
}

/// Create the codemode tool as an `AgentTool` (for direct/unit testing).
pub fn create_codemode_tool(tools: &[AgentTool], options: CodemodeToolOptions) -> AgentTool {
    let mut def = create_codemode_tool_definition(CodemodeToolOptions {
        model_runtime: options.model_runtime.clone(),
        models: options.models,
        get_tool_namespace: options.get_tool_namespace.clone(),
        append_entry: options.append_entry.clone(),
        get_mode: options.get_mode.clone(),
        get_inline_budget: options.get_inline_budget.clone(),
    });
    def.description = create_codemode_description(
        tools,
        CodemodeDescriptionOptions {
            models: options.models,
            namespaces: IndexMap::new(),
            deferred: IndexSet::new(),
            inline_budget: None,
        },
    );
    let _store_holder: Arc<Mutex<()>> = Arc::new(Mutex::new(()));
    wrap_tool_definition(def, None)
}
