//! Port of packages/agent/examples/mcp-codemode/tools.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use indexmap::IndexMap;
use pi_agent_core::{
    Agent, AgentContext, AgentEventSink, AgentTool, AgentToolCall, AgentToolCallOutcome, AgentToolResult,
    RunToolCallOptions, run_tool_call,
};
use pi_ai::{ImageContent, JsonObject, UserContentBlock, faux_text};
use pi_codemode::{
    CodemodeExecuteOptions, CodemodeJsonSchema, CodemodeOutputItem, CodemodeResult, CodemodeSandbox,
    CodemodeSandboxOptions, CodemodeTool, RenderDeclarationsOptions, render_declarations,
};
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Error, Result};
use pi_mcp::{LlmContent, McpClient, McpRequestOptions, to_llm_content};
use pi_schema::{Annotations, ObjOptions, Schema, StrOptions, obj_with, str_with};
use serde_json::{Map, Value, json};

pub const CODEMODE_TOOL_NAME: &str = "codemode";

pub type NestedToolRunner =
    Arc<dyn Fn(String, Option<Value>, AbortSignal) -> BoxFuture<Result<AgentToolCallOutcome>> + Send + Sync>;

/// Provider tool names are limited to 64 characters of `[A-Za-z0-9_-]`.
fn to_tool_name(prefix: &str, name: &str) -> String {
    format!("{prefix}_{name}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

fn text_of(result: &AgentToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn value_to_args_map(value: Value) -> Option<IndexMap<String, Value>> {
    match value {
        Value::Object(map) => Some(map.into_iter().collect()),
        _ => None,
    }
}

fn value_to_json_object(value: Option<Value>) -> JsonObject {
    match value {
        Some(Value::Object(map)) => map.into_iter().collect(),
        _ => JsonObject::new(),
    }
}

fn mcp_object_schema(input_schema: &IndexMap<String, Value>) -> Schema {
    let mut map = Map::new();
    for (key, value) in input_schema {
        map.insert(key.clone(), value.clone());
    }
    map.insert("type".to_string(), Value::String("object".to_string()));
    if !map.contains_key("properties") || map.get("properties").is_some_and(Value::is_null) {
        map.insert("properties".to_string(), Value::Object(Map::new()));
    }
    Schema::from_value(Value::Object(map))
}

fn string_output_schema() -> CodemodeJsonSchema {
    let mut map = IndexMap::new();
    map.insert("type".to_string(), Value::String("string".to_string()));
    CodemodeJsonSchema::Object(map)
}

/// One `AgentTool` per tool of a connected MCP server, named `<prefix>_<tool>`.
pub async fn create_mcp_tools(client: McpClient, prefix: &str) -> Result<Vec<AgentTool>> {
    let tools = client.list_tools(None).await?;
    Ok(tools
        .into_iter()
        .map(|tool| {
            let tool_name = tool.name.clone();
            let adapted_name = to_tool_name(prefix, &tool.name);
            let label = tool.title.clone().unwrap_or_else(|| tool.name.clone());
            let description = tool
                .description
                .clone()
                .or(tool.title.clone())
                .unwrap_or_else(|| format!("MCP tool {}", tool.name));
            let parameters = mcp_object_schema(&tool.input_schema);
            let output_schema = tool.output_schema.as_ref().map(|schema| {
                let map: Map<String, Value> = schema.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                Value::Object(map)
            });
            let client = client.clone();

            AgentTool {
                name: adapted_name,
                label,
                description,
                parameters,
                constrained_sampling: None,
                output_schema,
                execution_mode: None,
                prepare_arguments: None,
                execute: Arc::new(move |_tool_call_id, params, signal, on_update| {
                    let client = client.clone();
                    let tool_name = tool_name.clone();
                    Box::pin(async move {
                        let on_progress = on_update.map(|update| {
                            Arc::new(move |progress: pi_mcp::ProgressNotification| {
                                let text = progress
                                    .message
                                    .unwrap_or_else(|| format!("Progress {}", progress.progress));
                                update(AgentToolResult::text(text));
                            }) as Arc<dyn Fn(pi_mcp::ProgressNotification) + Send + Sync>
                        });

                        let result = client
                            .call_tool(
                                &tool_name,
                                value_to_args_map(params),
                                Some(McpRequestOptions {
                                    signal,
                                    timeout_ms: None,
                                    on_progress,
                                }),
                            )
                            .await?;

                        let mut content: Vec<UserContentBlock> = to_llm_content(&result)
                            .into_iter()
                            .map(|item| match item {
                                LlmContent::Text { text } => UserContentBlock::Text(faux_text(&text)),
                                LlmContent::Image { data, mime_type } => {
                                    UserContentBlock::Image(ImageContent { data, mime_type })
                                }
                            })
                            .collect();

                        let failed = result.is_error == Some(true);
                        if failed && content.is_empty() {
                            content.push(UserContentBlock::Text(faux_text(&format!(
                                "MCP tool {tool_name} failed"
                            ))));
                        }

                        Ok(AgentToolResult {
                            content,
                            details: None,
                            structured_content: result.structured_content,
                            usage: None,
                            is_error: Some(failed),
                            terminate: None,
                            control: None,
                        })
                    })
                }),
                replay: None,
            }
        })
        .collect())
}

/// Runs a tool call on behalf of another tool through the agent's pipeline.
pub fn create_nested_tool_runner(agent: Agent) -> NestedToolRunner {
    let next_id = Arc::new(AtomicU64::new(1));
    Arc::new(move |name, args, signal| {
        let agent = agent.clone();
        let next_id = Arc::clone(&next_id);
        Box::pin(async move {
            let state = agent.state();
            let Some(assistant_message) = state.messages.iter().rev().find_map(|msg| msg.as_assistant().cloned())
            else {
                return Err(Error::msg("Nested tool calls need an assistant message"));
            };

            let id = format!("nested-{}", next_id.fetch_add(1, Ordering::SeqCst));
            let tools = state.tools.clone();
            let context = AgentContext {
                messages: state.messages,
                tools: Some(tools.clone()),
            };

            Ok(run_tool_call(
                AgentToolCall {
                    id,
                    name,
                    arguments: value_to_json_object(args),
                },
                RunToolCallOptions {
                    tools,
                    assistant_message,
                    context,
                    before_tool_call: agent.before_tool_call(),
                    after_tool_call: agent.after_tool_call(),
                    signal: Some(signal),
                    on_update: None,
                },
            )
            .await)
        })
    })
}

/// A tool that runs model-written JavaScript in the codemode sandbox, where `tools.<name>(args)`
/// calls `tools`.
pub fn create_codemode_tool(tools: &[AgentTool], run_tool: NestedToolRunner) -> AgentTool {
    let codemode_parameters = obj_with(
        [(
            "code",
            str_with(StrOptions {
                annotations: Annotations {
                    description: Some("JavaScript source. Top-level await and return work.".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
        )],
        ObjOptions {
            open: true,
            ..Default::default()
        },
    );

    let sandbox_tools: Vec<CodemodeTool> = tools
        .iter()
        .filter(|tool| tool.name != CODEMODE_TOOL_NAME)
        .map(|tool| {
            let tool_name = tool.name.clone();
            let has_output_schema = tool.output_schema.is_some();
            let input_schema = serde_json::from_value::<CodemodeJsonSchema>(tool.parameters.to_value()).ok();
            let output_schema = tool
                .output_schema
                .as_ref()
                .and_then(|v| serde_json::from_value::<CodemodeJsonSchema>(v.clone()).ok())
                .unwrap_or_else(string_output_schema);
            let run_tool = Arc::clone(&run_tool);

            CodemodeTool {
                name: tool.name.clone(),
                description: Some(tool.description.clone()),
                input_schema,
                output_schema: Some(output_schema),
                spread: None,
                signature: None,
                execute: Arc::new(move |args, ctx| {
                    let run_tool = Arc::clone(&run_tool);
                    let tool_name = tool_name.clone();
                    Box::pin(async move {
                        let outcome = run_tool(tool_name.clone(), args, ctx.signal).await?;
                        if has_output_schema && let Some(structured) = outcome.result.structured_content {
                            return Ok(Some(structured));
                        }
                        let text = text_of(&outcome.result);
                        if outcome.is_error {
                            let message = if text.is_empty() {
                                format!("Tool {tool_name} failed")
                            } else {
                                text
                            };
                            return Err(Error::msg(message));
                        }
                        Ok(Some(Value::String(text)))
                    })
                }),
            }
        })
        .collect();

    let declarations = render_declarations(RenderDeclarationsOptions {
        tools: Some(sandbox_tools.clone()),
        globals: None,
    });

    let description = [
        "Run JavaScript that calls other tools, to chain calls, run them in parallel, or filter large results.",
        "The code is the body of an async function. There is no Node, file system, network, or timers.",
        "Call tools as `await tools.<name>(args)`. Output with `text(value)` or `console.log()`, or `return` a value.",
        "Only the output and the return value reach you, not the results of the tool calls.",
        "",
        "```ts",
        &declarations,
        "```",
    ]
    .join("\n");

    let _unused: Option<&AgentEventSink> = None;

    AgentTool {
        name: CODEMODE_TOOL_NAME.to_string(),
        label: "Codemode".to_string(),
        description,
        parameters: codemode_parameters,
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_tool_call_id, params, signal, _on_update| {
            let sandbox_tools = sandbox_tools.clone();
            Box::pin(async move {
                let code = params
                    .get("code")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::msg("Missing string parameter `code`"))?;

                let sandbox = CodemodeSandbox::new(Some(CodemodeSandboxOptions {
                    tools: Some(sandbox_tools),
                    timeout_ms: Some(120_000.0),
                    ..Default::default()
                }))?;

                let exec_res = sandbox
                    .execute(
                        code,
                        Some(CodemodeExecuteOptions {
                            signal,
                            ..Default::default()
                        }),
                    )
                    .await;
                let _ = sandbox.close().await;
                let result = exec_res?;

                let (ok, output, calls, returned_value, script_error) = match result {
                    CodemodeResult::Success {
                        ok,
                        value,
                        output,
                        calls,
                        ..
                    } => (ok, output, calls, value, None),
                    CodemodeResult::Failure {
                        ok,
                        error,
                        output,
                        calls,
                        ..
                    } => (ok, output, calls, None, Some(error)),
                };

                let mut content: Vec<UserContentBlock> = output
                    .into_iter()
                    .map(|item| match item {
                        CodemodeOutputItem::Text { text } => UserContentBlock::Text(faux_text(&text)),
                        CodemodeOutputItem::Image { data, mime_type } => {
                            UserContentBlock::Image(ImageContent { data, mime_type })
                        }
                    })
                    .collect();

                if ok && let Some(value) = returned_value {
                    let text = match value {
                        Value::String(s) => s,
                        other => pi_js::json::stringify(&other),
                    };
                    content.push(UserContentBlock::Text(faux_text(&text)));
                }
                if !ok && let Some(error) = script_error {
                    let detail = error.stack.as_deref().unwrap_or(&error.message);
                    content.push(UserContentBlock::Text(faux_text(&format!("Script failed: {detail}"))));
                }

                Ok(AgentToolResult {
                    content,
                    details: Some(json!({ "calls": calls })),
                    structured_content: None,
                    usage: None,
                    is_error: Some(!ok),
                    terminate: None,
                    control: None,
                })
            })
        }),
        replay: None,
    }
}
