//! Port of packages/coding-agent/src/extensions/mcp/resources.ts

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};

use crate::core::extensions::types::{ToolAnnotations, ToolDefinition};
pub use crate::core::mcp_servers::{LIST_MCP_RESOURCE_TEMPLATES_TOOL, LIST_MCP_RESOURCES_TOOL, READ_MCP_RESOURCE_TOOL};
use crate::extensions::mcp::config::McpExposure;
use crate::extensions::mcp::tools::{
    ConvertMcpResultOptions, McpToolDetails, limit_mcp_content, to_model_content, to_tool_exposure,
};
use pi_agent_core::AgentToolResult;
use pi_ai::{TextContent, UserContentBlock};
use pi_js::vendor::typebox::Schema;
use pi_js::{Error, Result};
use pi_mcp::protocol::content::{EmbeddedResourceContent, EmbeddedResourceContentType, ResourceContents};
use pi_mcp::{
    ContentBlock, ListResourceTemplatesResult, ListResourcesResult, McpRequestOptions, ReadResourceResult, Resource,
    ResourceTemplate,
};

#[async_trait]
pub trait McpResourceServer: Send + Sync {
    fn name(&self) -> &str;
    fn timeout_ms(&self) -> i64;
    async fn resources_page(&self, cursor: Option<&str>, options: McpRequestOptions) -> Result<ListResourcesResult>;
    async fn resource_templates_page(
        &self,
        cursor: Option<&str>,
        options: McpRequestOptions,
    ) -> Result<ListResourceTemplatesResult>;
    async fn all_resources(&self, options: McpRequestOptions) -> Result<Vec<Resource>>;
    async fn all_resource_templates(&self, options: McpRequestOptions) -> Result<Vec<ResourceTemplate>>;
    async fn read_resource(&self, uri: &str, options: McpRequestOptions) -> Result<ReadResourceResult>;
}

static MCP_APP_MIME_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r#"(?i);\s*profile\s*=\s*"?mcp-app"?"#).expect("valid mcp-app mime regex"));

fn is_mcp_app_resource(uri: Option<&str>, mime_type: Option<&str>) -> bool {
    uri.is_some_and(|u| u.starts_with("ui://")) || mime_type.is_some_and(|m| MCP_APP_MIME_RE.is_match(m))
}

fn listed<T: Serialize>(server: &str, item: &T) -> Value {
    let Ok(mut val) = pi_js::json::parse(&pi_js::json::stringify(item)) else {
        return json!({ "server": server });
    };
    let mut map = serde_json::Map::new();
    map.insert("server".to_string(), Value::String(server.to_string()));
    if let Value::Object(obj) = &mut val {
        obj.shift_remove("_meta");
        obj.shift_remove("icons");
        for (k, v) in obj.iter() {
            map.insert(k.clone(), v.clone());
        }
    }
    Value::Object(map)
}

fn list_parameters() -> Schema {
    Schema::from(json!({
        "type": "object",
        "properties": {
            "server": {
                "type": "string",
                "description": "MCP server name. Omit to list every server with resources."
            },
            "cursor": {
                "type": "string",
                "description": "Opaque cursor from a previous call with the same server; omit for the first page."
            }
        },
        "additionalProperties": false
    }))
}

fn read_parameters() -> Schema {
    Schema::from(json!({
        "type": "object",
        "properties": {
            "server": {
                "type": "string",
                "description": "MCP server name exactly as configured. Must match the 'server' field returned by list_mcp_resources."
            },
            "uri": {
                "type": "string",
                "description": "Resource URI to read. Must be one of the URIs returned by list_mcp_resources."
            }
        },
        "required": ["server", "uri"],
        "additionalProperties": false
    }))
}

fn list_output_schema() -> Schema {
    let listing_errors = json!({
        "type": "array",
        "description": "Servers that could not be listed",
        "items": {
            "type": "object",
            "properties": { "server": { "type": "string" }, "error": { "type": "string" } },
            "required": ["server", "error"]
        }
    });
    Schema::from(json!({
        "type": "object",
        "properties": {
            "server": { "type": "string" },
            "resources": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "server": { "type": "string" },
                        "uri": { "type": "string" },
                        "name": { "type": "string" },
                        "title": { "type": "string" },
                        "description": { "type": "string" },
                        "mimeType": { "type": "string" },
                        "size": { "type": "number" }
                    },
                    "required": ["server", "uri", "name"]
                }
            },
            "nextCursor": { "type": "string" },
            "errors": listing_errors
        },
        "required": ["resources"]
    }))
}

fn list_templates_output_schema() -> Schema {
    let listing_errors = json!({
        "type": "array",
        "description": "Servers that could not be listed",
        "items": {
            "type": "object",
            "properties": { "server": { "type": "string" }, "error": { "type": "string" } },
            "required": ["server", "error"]
        }
    });
    Schema::from(json!({
        "type": "object",
        "properties": {
            "server": { "type": "string" },
            "resourceTemplates": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "server": { "type": "string" },
                        "uriTemplate": { "type": "string", "description": "RFC 6570 URI template" },
                        "name": { "type": "string" },
                        "title": { "type": "string" },
                        "description": { "type": "string" },
                        "mimeType": { "type": "string" }
                    },
                    "required": ["server", "uriTemplate", "name"]
                }
            },
            "nextCursor": { "type": "string" },
            "errors": listing_errors
        },
        "required": ["resourceTemplates"]
    }))
}

fn read_output_schema() -> Schema {
    Schema::from(json!({
        "type": "object",
        "properties": {
            "server": { "type": "string" },
            "uri": { "type": "string" },
            "contents": {
                "type": "array",
                "items": {
                    "anyOf": [
                        {
                            "type": "object",
                            "properties": {
                                "uri": { "type": "string" },
                                "mimeType": { "type": "string" },
                                "text": { "type": "string" }
                            },
                            "required": ["uri", "text"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "uri": { "type": "string" },
                                "mimeType": { "type": "string" },
                                "blob": { "type": "string", "description": "base64" }
                            },
                            "required": ["uri", "blob"]
                        }
                    ]
                }
            }
        },
        "required": ["server", "uri", "contents"]
    }))
}

fn string_argument(params: &Value, key: &str) -> Result<Option<String>> {
    let Some(val) = params.get(key) else {
        return Ok(None);
    };
    if val.is_null() {
        return Ok(None);
    }
    let Some(s) = val.as_str() else {
        return Err(Error::msg(format!("{key} must be a string")));
    };
    let trimmed = pi_js::str16::trim(s);
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

async fn json_result(tool: &str, server: Option<String>, payload: Value) -> AgentToolResult {
    let (content, full_output_path) = limit_mcp_content(
        vec![UserContentBlock::Text(TextContent::new(pi_js::json::stringify(
            &payload,
        )))],
        None,
    )
    .await;
    let details = McpToolDetails {
        server: server.unwrap_or_default(),
        tool: tool.to_string(),
        full_output_path,
    };
    let mut out = AgentToolResult::text("");
    out.content = content;
    out.details = pi_js::json::parse(&pi_js::json::stringify(&details)).ok();
    out.structured_content = Some(payload);
    out
}

pub type ResourceServersProvider = Arc<dyn Fn() -> Vec<Arc<dyn McpResourceServer>> + Send + Sync>;

fn find_server(servers: &[Arc<dyn McpResourceServer>], name: &str) -> Result<Arc<dyn McpResourceServer>> {
    if let Some(found) = servers.iter().find(|s| s.name() == name) {
        return Ok(Arc::clone(found));
    }
    let available: Vec<&str> = servers.iter().map(|s| s.name()).collect();
    let suffix = if available.is_empty() {
        String::new()
    } else {
        format!(". Servers with resources: {}", available.join(", "))
    };
    Err(Error::msg(format!("MCP server \"{name}\" has no resources{suffix}")))
}

pub fn create_mcp_resource_tool_definitions(
    exposure: McpExposure,
    servers: ResourceServersProvider,
) -> Vec<ToolDefinition> {
    let read_only = Some(ToolAnnotations {
        read_only_hint: Some(true),
        destructive_hint: None,
        idempotent_hint: None,
        open_world_hint: None,
    });
    let tool_exposure = Some(to_tool_exposure(exposure));

    let servers_list = Arc::clone(&servers);
    let list_resources = ToolDefinition {
        name: LIST_MCP_RESOURCES_TOOL.to_string(),
        label: LIST_MCP_RESOURCES_TOOL.to_string(),
        description: "Lists resources provided by MCP servers. Resources allow servers to share data that provides context to language models, such as files, database schemas, or application-specific information. Prefer resources over web search when possible.".to_string(),
        prompt_snippet: None,
        prompt_guidelines: None,
        parameters: list_parameters(),
        constrained_sampling: None,
        render_shell: None,
        prepare_arguments: None,
        output_schema: Some(list_output_schema()),
        exposure: tool_exposure,
        namespace: None,
        annotations: read_only.clone(),
        default_active: None,
        prepare_loadout: None,
        execution_mode: None,
        execute: Arc::new(move |_id, params, signal, _upd, _ctx| {
            let servers_fn = Arc::clone(&servers_list);
            Box::pin(async move {
                let server_name = string_argument(&params, "server")?;
                let cursor = string_argument(&params, "cursor")?;
                let list = servers_fn();
                if let Some(ref sname) = server_name {
                    let srv = find_server(&list, sname)?;
                    let res = srv
                        .resources_page(
                            cursor.as_deref(),
                            McpRequestOptions {
                                signal,
                                timeout_ms: Some(srv.timeout_ms()),
                                on_progress: None,
                            },
                        )
                        .await?;
                    let items: Vec<Value> = res
                        .resources
                        .iter()
                        .filter(|r| !is_mcp_app_resource(Some(&r.uri), r.mime_type.as_deref()))
                        .map(|r| listed(srv.name(), r))
                        .collect();
                    let mut map = serde_json::Map::new();
                    map.insert("server".to_string(), Value::String(srv.name().to_string()));
                    map.insert("resources".to_string(), Value::Array(items));
                    if let Some(nc) = res.next_cursor {
                        map.insert("nextCursor".to_string(), Value::String(nc));
                    }
                    return Ok(json_result(LIST_MCP_RESOURCES_TOOL, server_name, Value::Object(map)).await);
                }
                if cursor.is_some() {
                    return Err(Error::msg("cursor can only be used when a server is specified"));
                }
                let mut sorted = list;
                sorted.sort_by(|a, b| pi_js::intl::locale_compare(a.name(), b.name()));
                let futs: Vec<_> = sorted
                    .iter()
                    .map(|srv| {
                        let srv = Arc::clone(srv);
                        let sig = signal.clone();
                        async move {
                            let res = srv
                                .all_resources(McpRequestOptions {
                                    signal: sig,
                                    timeout_ms: Some(srv.timeout_ms()),
                                    on_progress: None,
                                })
                                .await;
                            (srv.name().to_string(), res)
                        }
                    })
                    .collect();
                let results = futures::future::join_all(futs).await;
                let mut items = Vec::new();
                let mut errors = Vec::new();
                for (sname, outcome) in results {
                    match outcome {
                        Ok(res) => {
                            for r in res {
                                if !is_mcp_app_resource(Some(&r.uri), r.mime_type.as_deref()) {
                                    items.push(listed(&sname, &r));
                                }
                            }
                        }
                        Err(e) => errors.push(json!({ "server": sname, "error": e.message() })),
                    }
                }
                let mut map = serde_json::Map::new();
                map.insert("resources".to_string(), Value::Array(items));
                if !errors.is_empty() {
                    map.insert("errors".to_string(), Value::Array(errors));
                }
                Ok(json_result(LIST_MCP_RESOURCES_TOOL, None, Value::Object(map)).await)
            })
        }),
        render_call: None,
        render_result: None,
    };

    let servers_tpl = Arc::clone(&servers);
    let list_templates = ToolDefinition {
        name: LIST_MCP_RESOURCE_TEMPLATES_TOOL.to_string(),
        label: LIST_MCP_RESOURCE_TEMPLATES_TOOL.to_string(),
        description: "Lists resource templates provided by MCP servers. Parameterized resource templates allow servers to share data that takes parameters and provides context to language models, such as files, database schemas, or application-specific information. Prefer resource templates over web search when possible.".to_string(),
        prompt_snippet: None,
        prompt_guidelines: None,
        parameters: list_parameters(),
        constrained_sampling: None,
        render_shell: None,
        prepare_arguments: None,
        output_schema: Some(list_templates_output_schema()),
        exposure: tool_exposure,
        namespace: None,
        annotations: read_only.clone(),
        default_active: None,
        prepare_loadout: None,
        execution_mode: None,
        execute: Arc::new(move |_id, params, signal, _upd, _ctx| {
            let servers_fn = Arc::clone(&servers_tpl);
            Box::pin(async move {
                let server_name = string_argument(&params, "server")?;
                let cursor = string_argument(&params, "cursor")?;
                let list = servers_fn();
                if let Some(ref sname) = server_name {
                    let srv = find_server(&list, sname)?;
                    let res = srv
                        .resource_templates_page(
                            cursor.as_deref(),
                            McpRequestOptions {
                                signal,
                                timeout_ms: Some(srv.timeout_ms()),
                                on_progress: None,
                            },
                        )
                        .await?;
                    let items: Vec<Value> = res
                        .resource_templates
                        .iter()
                        .filter(|t| !is_mcp_app_resource(Some(&t.uri_template), t.mime_type.as_deref()))
                        .map(|t| listed(srv.name(), t))
                        .collect();
                    let mut map = serde_json::Map::new();
                    map.insert("server".to_string(), Value::String(srv.name().to_string()));
                    map.insert("resourceTemplates".to_string(), Value::Array(items));
                    if let Some(nc) = res.next_cursor {
                        map.insert("nextCursor".to_string(), Value::String(nc));
                    }
                    return Ok(
                        json_result(LIST_MCP_RESOURCE_TEMPLATES_TOOL, server_name, Value::Object(map)).await,
                    );
                }
                if cursor.is_some() {
                    return Err(Error::msg("cursor can only be used when a server is specified"));
                }
                let mut sorted = list;
                sorted.sort_by(|a, b| pi_js::intl::locale_compare(a.name(), b.name()));
                let futs: Vec<_> = sorted
                    .iter()
                    .map(|srv| {
                        let srv = Arc::clone(srv);
                        let sig = signal.clone();
                        async move {
                            let res = srv
                                .all_resource_templates(McpRequestOptions {
                                    signal: sig,
                                    timeout_ms: Some(srv.timeout_ms()),
                                    on_progress: None,
                                })
                                .await;
                            (srv.name().to_string(), res)
                        }
                    })
                    .collect();
                let results = futures::future::join_all(futs).await;
                let mut items = Vec::new();
                let mut errors = Vec::new();
                for (sname, outcome) in results {
                    match outcome {
                        Ok(res) => {
                            for t in res {
                                if !is_mcp_app_resource(Some(&t.uri_template), t.mime_type.as_deref()) {
                                    items.push(listed(&sname, &t));
                                }
                            }
                        }
                        Err(e) => errors.push(json!({ "server": sname, "error": e.message() })),
                    }
                }
                let mut map = serde_json::Map::new();
                map.insert("resourceTemplates".to_string(), Value::Array(items));
                if !errors.is_empty() {
                    map.insert("errors".to_string(), Value::Array(errors));
                }
                Ok(json_result(LIST_MCP_RESOURCE_TEMPLATES_TOOL, None, Value::Object(map)).await)
            })
        }),
        render_call: None,
        render_result: None,
    };

    let servers_read = Arc::clone(&servers);
    let read_resource = ToolDefinition {
        name: READ_MCP_RESOURCE_TOOL.to_string(),
        label: READ_MCP_RESOURCE_TOOL.to_string(),
        description: "Read a specific resource from an MCP server given the server name and resource URI.".to_string(),
        prompt_snippet: None,
        prompt_guidelines: None,
        parameters: read_parameters(),
        constrained_sampling: None,
        render_shell: None,
        prepare_arguments: None,
        output_schema: Some(read_output_schema()),
        exposure: tool_exposure,
        namespace: None,
        annotations: read_only,
        default_active: None,
        prepare_loadout: None,
        execution_mode: None,
        execute: Arc::new(move |_id, params, signal, _upd, _ctx| {
            let servers_fn = Arc::clone(&servers_read);
            Box::pin(async move {
                let server_name =
                    string_argument(&params, "server")?.ok_or_else(|| Error::msg("server must be provided"))?;
                let uri = string_argument(&params, "uri")?.ok_or_else(|| Error::msg("uri must be provided"))?;
                let list = servers_fn();
                let srv = find_server(&list, &server_name)?;
                let result = srv
                    .read_resource(
                        &uri,
                        McpRequestOptions {
                            signal,
                            timeout_ms: Some(srv.timeout_ms()),
                            on_progress: None,
                        },
                    )
                    .await?;
                let mut blocks = Vec::new();
                let multi = result.contents.len() > 1;
                for item in &result.contents {
                    if multi {
                        let item_uri = match item {
                            ResourceContents::Text(t) => &t.uri,
                            ResourceContents::Blob(b) => &b.uri,
                        };
                        blocks.push(ContentBlock::Text(pi_mcp::protocol::content::TextContent {
                            r#type: pi_mcp::protocol::content::TextContentType::Text,
                            text: format!("{item_uri}:"),
                            annotations: None,
                            meta: None,
                        }));
                    }
                    blocks.push(ContentBlock::Resource(EmbeddedResourceContent {
                        r#type: EmbeddedResourceContentType::Resource,
                        resource: item.clone(),
                        annotations: None,
                        meta: None,
                    }));
                }
                let converted = to_model_content(srv.name(), &blocks, &ConvertMcpResultOptions::default()).await;
                let initial = if converted.is_empty() {
                    vec![UserContentBlock::Text(TextContent::new(format!(
                        "Resource {uri} is empty."
                    )))]
                } else {
                    converted
                };
                let (content, full_output_path) = limit_mcp_content(initial, None).await;
                let contents_json: Vec<Value> = result
                    .contents
                    .iter()
                    .filter_map(|c| {
                        let mut val = pi_js::json::parse(&pi_js::json::stringify(c)).ok()?;
                        if let Some(obj) = val.as_object_mut() {
                            obj.shift_remove("_meta");
                        }
                        Some(val)
                    })
                    .collect();
                let details = McpToolDetails {
                    server: srv.name().to_string(),
                    tool: READ_MCP_RESOURCE_TOOL.to_string(),
                    full_output_path,
                };
                let mut out = AgentToolResult::text("");
                out.content = content;
                out.details = pi_js::json::parse(&pi_js::json::stringify(&details)).ok();
                out.structured_content = Some(json!({
                    "server": srv.name(),
                    "uri": uri,
                    "contents": contents_json,
                }));
                Ok(out)
            })
        }),
        render_call: None,
        render_result: None,
    };

    vec![list_resources, list_templates, read_resource]
}
