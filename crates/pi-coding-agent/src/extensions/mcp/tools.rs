//! Port of packages/coding-agent/src/extensions/mcp/tools.ts

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::extensions::types::{
    Theme, ToolAnnotations, ToolDefinition, ToolExposure, ToolNamespace, ToolRenderContext, ToolRenderResultOptions,
    ToolRenderers,
};
pub use crate::core::mcp_servers::READ_MCP_RESOURCE_TOOL;
use crate::core::tools::render_utils::{format_tool_call_with_args, get_text_output, replace_tabs};
use crate::core::tools::truncate::format_size;
use crate::extensions::mcp::config::McpExposure;
use crate::extensions::wire::{VisualLinePreview, VisualTruncateKeep, key_hint};
use crate::utils::output_files::write_output_file;
use pi_agent_core::{AgentToolResult, AgentToolUpdateCallback};
use pi_ai::{ImageContent, TextContent, UserContentBlock};
use pi_js::abort::AbortSignal;
use pi_js::vendor::typebox::Schema;
use pi_js::{BoxFuture, Result};
use pi_mcp::protocol::content::ResourceContents;
use pi_mcp::{
    CallToolResult, ContentBlock, LlmContent, McpRequestOptions, Tool as McpTool, normalize_mcp_input_schema,
    to_llm_content,
};
pub use pi_mcp::{MCP_OUTPUT_MAX_BYTES, McpOutputData, McpOutputSaver};
use pi_tui::components::spacer::Spacer;
use pi_tui::components::text::Text;
use pi_tui::tui::{Component, Container};

const OUTPUT_PREVIEW_LINES: usize = 5;

pub fn to_tool_exposure(exposure: McpExposure) -> ToolExposure {
    match exposure {
        McpExposure::Codemode | McpExposure::Deferred => ToolExposure::Deferred,
        McpExposure::Direct => ToolExposure::Direct,
        McpExposure::Hidden => ToolExposure::Hidden,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolDetails {
    pub server: String,
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
}

pub async fn save_to_temp_file(data: McpOutputData, extension: &str) -> Result<String> {
    write_output_file("pi-mcp", extension, data.as_bytes()).await
}

pub fn default_output_saver() -> McpOutputSaver {
    Arc::new(|data, ext| Box::pin(async move { save_to_temp_file(data, &ext).await }))
}

#[async_trait]
pub trait McpToolCaller: Send + Sync {
    async fn call_tool(
        &self,
        name: &str,
        args: Option<IndexMap<String, Value>>,
        options: Option<McpRequestOptions>,
    ) -> Result<CallToolResult>;
}

static TRAILING_EXT_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\.[A-Za-z0-9]{1,8}$").expect("valid ext regex"));

pub fn create_mcp_tool_name<F>(server: &str, tool: &str, is_taken: F) -> String
where
    F: Fn(&str) -> bool,
{
    pi_mcp::create_mcp_tool_name(server, tool, Some(&is_taken))
}

fn text_of(content: &[UserContentBlock]) -> String {
    content
        .iter()
        .filter_map(|b| match b {
            UserContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn user_to_llm_block(block: UserContentBlock) -> LlmContent {
    match block {
        UserContentBlock::Text(t) => LlmContent::Text { text: t.text },
        UserContentBlock::Image(i) => LlmContent::Image {
            data: i.data,
            mime_type: i.mime_type,
        },
    }
}

const SCHEMA_DATA_KEYWORDS: &[&str] = &["const", "enum", "default", "examples"];
const SCHEMA_MAP_KEYWORDS: &[&str] = &[
    "properties",
    "patternProperties",
    "dependentSchemas",
    "dependencies",
    "$defs",
    "definitions",
];

/// Rewrite local `$ref` pointers inside `schema` as subschemas under `pointer`.
fn relocate_schema(schema: Value, pointer: &str) -> Value {
    match schema {
        Value::Array(items) => Value::Array(items.into_iter().map(|i| relocate_schema(i, pointer)).collect()),
        Value::Object(obj) => {
            let mut relocated = serde_json::Map::with_capacity(obj.len());
            for (key, value) in obj {
                if key == "$ref"
                    && let Value::String(ref ref_str) = value
                    && (ref_str == "#" || ref_str.starts_with("#/"))
                {
                    relocated.insert(key, Value::String(format!("{pointer}{}", &ref_str[1..])));
                } else if SCHEMA_DATA_KEYWORDS.contains(&key.as_str()) {
                    relocated.insert(key, value);
                } else if SCHEMA_MAP_KEYWORDS.contains(&key.as_str())
                    && let Value::Object(sub_map) = value
                {
                    let mapped = sub_map
                        .into_iter()
                        .map(|(n, sub)| (n, relocate_schema(sub, pointer)))
                        .collect();
                    relocated.insert(key, Value::Object(mapped));
                } else {
                    relocated.insert(key, relocate_schema(value, pointer));
                }
            }
            Value::Object(relocated)
        }
        other => other,
    }
}

pub fn create_mcp_result_schema(structured_content_schema: Option<&IndexMap<String, Value>>) -> Schema {
    let mut props = serde_json::Map::new();
    props.insert(
        "content".to_string(),
        json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "type": { "enum": ["text", "image", "audio", "resource_link", "resource"] },
                    "text": { "type": "string" },
                    "data": { "type": "string" },
                    "mimeType": { "type": "string" },
                },
                "required": ["type"],
                "additionalProperties": true,
            }
        }),
    );
    let structured_schema = match structured_content_schema {
        Some(sc) => {
            let mut sc_map = serde_json::Map::with_capacity(sc.len());
            for (k, v) in sc {
                sc_map.insert(k.clone(), v.clone());
            }
            relocate_schema(Value::Object(sc_map), "#/properties/structuredContent")
        }
        None => json!({}),
    };
    props.insert("structuredContent".to_string(), structured_schema);
    props.insert("isError".to_string(), json!({ "type": "boolean" }));
    Schema::from(json!({
        "type": "object",
        "properties": props,
        "required": ["content"],
        "additionalProperties": true,
    }))
}

pub async fn limit_mcp_content(
    content: Vec<UserContentBlock>,
    save_output: Option<McpOutputSaver>,
) -> (Vec<UserContentBlock>, Option<String>) {
    let llm_content: Vec<LlmContent> = content.into_iter().map(user_to_llm_block).collect();
    let saver = save_output.unwrap_or_else(default_output_saver);
    let limited = pi_mcp::limit_mcp_content(llm_content, Some(saver)).await;
    let blocks = limited.content.into_iter().map(llm_to_user_block).collect();
    (blocks, limited.full_output_path)
}

#[derive(Clone, Default)]
pub struct ConvertMcpResultOptions {
    pub save_output: Option<McpOutputSaver>,
    pub readable_resources: bool,
}

fn extension_of(uri: &str) -> String {
    let path_owned;
    let path = if let Ok(u) = url::Url::parse(uri) {
        path_owned = u.path().to_string();
        path_owned.as_str()
    } else {
        uri
    };
    TRAILING_EXT_RE
        .find(path)
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| ".bin".to_string())
}

fn is_text_mime_type(mime_type: Option<&str>) -> bool {
    let Some(mt) = mime_type else {
        return false;
    };
    let base = pi_js::str16::trim(mt.split(';').next().unwrap_or("")).to_ascii_lowercase();
    base.starts_with("text/") || base == "application/json" || base.ends_with("+json") || base.ends_with("+xml")
}

fn llm_to_user_block(item: LlmContent) -> UserContentBlock {
    match item {
        LlmContent::Text { text } => UserContentBlock::Text(TextContent::new(text)),
        LlmContent::Image { data, mime_type } => UserContentBlock::Image(ImageContent::new(data, mime_type)),
    }
}

async fn block_to_content(
    server: &str,
    block: &ContentBlock,
    options: &ConvertMcpResultOptions,
) -> Vec<UserContentBlock> {
    if let ContentBlock::ResourceLink(link) = block {
        let mut details = Vec::new();
        if let Some(ref mt) = link.mime_type
            && !mt.is_empty()
        {
            details.push(mt.clone());
        }
        if let Some(sz) = link.size {
            details.push(format_size(sz.max(0) as usize));
        }
        let details_str = if details.is_empty() {
            String::new()
        } else {
            format!(" ({})", details.join(", "))
        };
        let label = link.title.as_deref().unwrap_or(&link.name);
        let desc = link
            .description
            .as_deref()
            .filter(|d| !d.is_empty())
            .map(|d| format!(": {d}"))
            .unwrap_or_default();
        let read = if options.readable_resources {
            format!(". Read it with {READ_MCP_RESOURCE_TOOL} (server \"{server}\")")
        } else {
            String::new()
        };
        return vec![UserContentBlock::Text(TextContent::new(format!(
            "[Resource {} \"{label}\"{details_str}{desc}{read}]",
            link.uri
        )))];
    }

    if let ContentBlock::Resource(embedded) = block
        && let ResourceContents::Blob(ref blob_res) = embedded.resource
        && !blob_res.mime_type.as_deref().is_some_and(|m| m.starts_with("image/"))
    {
        let data = pi_js::b64::decode(&blob_res.blob);
        if is_text_mime_type(blob_res.mime_type.as_deref()) {
            return vec![UserContentBlock::Text(TextContent::new(
                String::from_utf8_lossy(&data).into_owned(),
            ))];
        }
        let kind = format!(
            "{}, {}",
            blob_res.mime_type.as_deref().unwrap_or("unknown type"),
            format_size(data.len())
        );
        let saver = options.save_output.clone().unwrap_or_else(default_output_saver);
        return match saver(McpOutputData::Bytes(data), extension_of(&blob_res.uri)).await {
            Ok(path) => vec![UserContentBlock::Text(TextContent::new(format!(
                "[Binary resource {} ({kind}) saved to {path}]",
                blob_res.uri
            )))],
            Err(err) => vec![UserContentBlock::Text(TextContent::new(format!(
                "[Binary resource {} ({kind}) could not be saved: {}]",
                blob_res.uri,
                err.message()
            )))],
        };
    }

    let single = CallToolResult {
        content: vec![block.clone()],
        structured_content: None,
        is_error: None,
        meta: None,
    };
    to_llm_content(&single).into_iter().map(llm_to_user_block).collect()
}

pub async fn to_model_content(
    server: &str,
    blocks: &[ContentBlock],
    options: &ConvertMcpResultOptions,
) -> Vec<UserContentBlock> {
    let mut out = Vec::new();
    for block in blocks {
        out.extend(block_to_content(server, block, options).await);
    }
    out
}

pub async fn convert_mcp_result(
    server: &str,
    tool: &str,
    result: CallToolResult,
    options: &ConvertMcpResultOptions,
) -> AgentToolResult {
    let mut converted = if !result.content.is_empty() {
        to_model_content(server, &result.content, options).await
    } else {
        to_llm_content(&result).into_iter().map(llm_to_user_block).collect()
    };
    if result.is_error == Some(true) && text_of(&converted).is_empty() {
        converted.push(UserContentBlock::Text(TextContent::new(format!(
            "MCP tool {server}/{tool} returned an error"
        ))));
    }
    let (content, full_output_path) = limit_mcp_content(converted, options.save_output.clone()).await;
    let details = McpToolDetails {
        server: server.to_string(),
        tool: tool.to_string(),
        full_output_path,
    };
    let mut script_result = pi_js::json::parse(&pi_js::json::stringify(&result)).unwrap_or(Value::Null);
    if let Some(obj) = script_result.as_object_mut() {
        obj.shift_remove("_meta");
    }

    let mut out = AgentToolResult::text("");
    out.content = content;
    out.details = pi_js::json::parse(&pi_js::json::stringify(&details)).ok();
    out.structured_content = Some(script_result);
    if result.is_error == Some(true) {
        out.is_error = Some(true);
    }
    out
}

fn to_parameters(schema: &IndexMap<String, Value>) -> Schema {
    let normalized = normalize_mcp_input_schema(schema);
    let mut obj = serde_json::Map::with_capacity(normalized.len());
    for (k, v) in normalized {
        obj.insert(k, v);
    }
    Schema::from(Value::Object(obj))
}

fn to_tool_annotations(tool: &McpTool) -> Option<ToolAnnotations> {
    let a = tool.annotations.as_ref()?;
    if a.read_only_hint.is_none()
        && a.destructive_hint.is_none()
        && a.idempotent_hint.is_none()
        && a.open_world_hint.is_none()
    {
        return None;
    }
    Some(ToolAnnotations {
        read_only_hint: a.read_only_hint,
        destructive_hint: a.destructive_hint,
        idempotent_hint: a.idempotent_hint,
        open_world_hint: a.open_world_hint,
    })
}

pub struct CreateMcpToolDefinitionOptions {
    pub server: String,
    pub tool: McpTool,
    pub name: String,
    pub exposure: McpExposure,
    pub namespace: ToolNamespace,
    pub timeout_ms: i64,
    pub get_client: Arc<dyn Fn() -> BoxFuture<Result<Arc<dyn McpToolCaller>>> + Send + Sync>,
    pub readable_resources: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

fn content_block_to_value(block: &UserContentBlock) -> Value {
    match block {
        UserContentBlock::Text(t) => json!({ "type": "text", "text": t.text }),
        UserContentBlock::Image(i) => json!({ "type": "image", "data": i.data, "mimeType": i.mime_type }),
    }
}

pub fn create_mcp_tool_definition(options: CreateMcpToolDefinitionOptions) -> ToolDefinition {
    let server = options.server;
    let tool = options.tool;
    let tool_name = tool.name.clone();
    let title = tool
        .title
        .clone()
        .or_else(|| tool.annotations.as_ref().and_then(|a| a.title.clone()));
    let annotations = to_tool_annotations(&tool);
    let label = format!("{server}/{tool_name}");
    let description = tool
        .description
        .as_deref()
        .map(pi_js::str16::trim)
        .filter(|d| !d.is_empty())
        .map(String::from)
        .or(title)
        .unwrap_or_else(|| format!("MCP tool {tool_name} from server {server}"));

    let server_exec = server.clone();
    let tool_exec = tool_name.clone();
    let get_client = options.get_client;
    let readable_resources = options.readable_resources;
    let timeout_ms = options.timeout_ms;
    let renderers = create_mcp_tool_renderers(&label);

    ToolDefinition {
        name: options.name,
        label,
        description,
        prompt_snippet: None,
        prompt_guidelines: None,
        parameters: to_parameters(&tool.input_schema),
        constrained_sampling: None,
        render_shell: None,
        prepare_arguments: None,
        output_schema: Some(create_mcp_result_schema(tool.output_schema.as_ref())),
        exposure: Some(to_tool_exposure(options.exposure)),
        namespace: Some(options.namespace),
        annotations,
        default_active: None,
        prepare_loadout: None,
        execution_mode: None,
        execute: Arc::new(move |_tool_call_id, params, signal, on_update, _ctx| {
            let server = server_exec.clone();
            let tool_name = tool_exec.clone();
            let get_client = Arc::clone(&get_client);
            let readable = readable_resources.clone();
            Box::pin(async move {
                let client = get_client().await?;
                let args: Option<IndexMap<String, Value>> = params
                    .as_object()
                    .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
                let server_prog = server.clone();
                let tool_prog = tool_name.clone();
                let on_progress = on_update.map(|upd: AgentToolUpdateCallback| {
                    Arc::new(move |progress: pi_mcp::protocol::types::ProgressNotification| {
                        let total = progress.total.map(|t| format!("/{t}")).unwrap_or_default();
                        let text = progress
                            .message
                            .unwrap_or_else(|| format!("Progress {}{total}", progress.progress));
                        let mut partial = AgentToolResult::text(text);
                        partial.details = Some(json!({ "server": server_prog, "tool": tool_prog }));
                        upd(partial);
                    }) as Arc<dyn Fn(pi_mcp::protocol::types::ProgressNotification) + Send + Sync>
                });
                let result = client
                    .call_tool(
                        &tool_name,
                        args,
                        Some(McpRequestOptions {
                            signal: signal as Option<AbortSignal>,
                            timeout_ms: Some(timeout_ms),
                            on_progress,
                        }),
                    )
                    .await?;
                let readable_flag = readable.as_ref().is_some_and(|f| f());
                Ok(convert_mcp_result(
                    &server,
                    &tool_name,
                    result,
                    &ConvertMcpResultOptions {
                        save_output: None,
                        readable_resources: readable_flag,
                    },
                )
                .await)
            })
        }),
        render_call: renderers.render_call,
        render_result: renderers.render_result,
    }
}

/// Renderers of calls to an MCP tool, labeled `server/tool`, also used before the tool is registered.
pub fn create_mcp_tool_renderers(label: &str) -> ToolRenderers {
    let label_call = label.to_string();
    ToolRenderers {
        render_shell: None,
        render_call: Some(Arc::new(
            move |args: Value, theme: Theme, context: ToolRenderContext| {
                let formatted = format_tool_call_with_args(&label_call, Some(&args), &theme, context.expanded);
                Arc::new(Text::new(Some(&formatted), Some(0), Some(0), None)) as Arc<dyn Component>
            },
        )),
        render_result: Some(Arc::new(
            move |result: AgentToolResult,
                  options: ToolRenderResultOptions,
                  theme: Theme,
                  context: ToolRenderContext| {
                let container = Container::new();
                let values: Vec<Value> = result.content.iter().map(content_block_to_value).collect();
                let output = pi_js::str16::trim(&get_text_output(&values, context.show_images)).to_string();
                if output.is_empty() {
                    return Arc::new(container);
                }
                let color = if context.is_error { "error" } else { "toolOutput" };
                let styled = replace_tabs(&output)
                    .split('\n')
                    .map(|line| theme.fg(color, line))
                    .collect::<Vec<_>>()
                    .join("\n");
                container.add_child(Arc::new(Spacer::new(Some(1))));
                if options.expanded {
                    container.add_child(Arc::new(Text::new(Some(&styled), Some(0), Some(0), None)));
                } else {
                    let t = theme.clone();
                    container.add_child(Arc::new(VisualLinePreview::new(
                        styled,
                        OUTPUT_PREVIEW_LINES,
                        VisualTruncateKeep::Start,
                        Arc::new(move |hidden| {
                            format!(
                                "{} {}{}",
                                t.fg("muted", &format!("... ({hidden} more lines,")),
                                key_hint("app.tools.expand", "to expand", &t),
                                t.fg("muted", ")")
                            )
                        }),
                    )));
                    if let Some(full_path) = result
                        .details
                        .as_ref()
                        .and_then(|d| d.get("fullOutputPath"))
                        .and_then(Value::as_str)
                    {
                        container.add_child(Arc::new(Text::new(
                            Some(&theme.fg("muted", &format!("Full output: {full_path}"))),
                            Some(0),
                            Some(0),
                            None,
                        )));
                    }
                }
                Arc::new(container)
            },
        )),
    }
}
