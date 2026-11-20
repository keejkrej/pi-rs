//! Port of packages/coding-agent/test/suite/agent-session-codemode.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use indexmap::IndexMap;
use pi_agent_core::types::{AgentTool, AgentToolResult};
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep,
    faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{
    AssistantImages, ClassifierAnswer, ClassifierResult, ClassifierStopReason, ImageContent, ImagesOutputContent,
    ImagesStopReason, ModelCost, ModelInput, ProviderClassifier, ProviderImages, StopReason, TextContent,
    ToolResultMessage, Usage, UsageCost, UserContentBlock,
};
use pi_ai::{get_current_system_prompt, get_current_tools};
use pi_coding_agent::core::agent_session::AgentSessionEvent;
use pi_coding_agent::core::extensions::types::{
    ExtensionApi, InlineExtension, ProviderClassifierModelConfig, ProviderConfig, ProviderImageModelConfig,
    ProviderModelConfig, SessionEntry, ToolCallEvent, ToolCallEventResult, ToolDefinition, ToolResultContentBlock,
    ToolResultEvent, ToolResultEventResult,
};
use pi_coding_agent::core::session_manager::CustomEntry;
use pi_coding_agent::core::settings_manager::{CodemodeMode, CodemodeSettings, Settings};
use pi_coding_agent::extensions::codemode::execute::read_codemode_store;
use pi_coding_agent::extensions::codemode::tool::{
    CODEMODE_STORE_ENTRY_TYPE, CodemodeToolDetails, CodemodeToolOptions, codemode_docs_path, create_codemode_tool,
};
use pi_coding_agent::extensions::codemode::{CodemodeExtensionOptions, create_codemode_extension};
use pi_js::vendor::typebox::t;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use support::suite::harness::{Harness, HarnessOptions, create_harness, get_tool_result};

const TINY_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";

fn make_echo_tool() -> AgentTool {
    AgentTool {
        name: "echo".to_string(),
        label: "Echo".to_string(),
        description: "Echo text back.\n\nSecond paragraph.".to_string(),
        parameters: t::object([("text", t::string().with("description", "Text to echo"))]).into(),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(|_id, params, _sig, _upd| {
            Box::pin(async move {
                let text = params.get("text").and_then(Value::as_str).unwrap_or("");
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new(format!("echo: {text}")))],
                    details: Some(json!({})),
                    ..Default::default()
                })
            })
        }),
        replay: None,
    }
}

fn make_stats_tool() -> AgentTool {
    AgentTool {
        name: "stats".to_string(),
        label: "Stats".to_string(),
        description: "Return structured stats".to_string(),
        parameters: t::object([] as [(&str, pi_js::vendor::typebox::Schema); 0]).into(),
        output_schema: Some(t::object([("files", t::number()), ("names", t::array(t::string()))]).to_value()),
        constrained_sampling: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(|_id, _params, _sig, _upd| {
            Box::pin(async move {
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new("2 files"))],
                    details: Some(json!({})),
                    structured_content: Some(json!({ "files": 2, "names": ["a", "b"] })),
                    ..Default::default()
                })
            })
        }),
        replay: None,
    }
}

fn make_screenshot_tool() -> AgentTool {
    AgentTool {
        name: "screenshot".to_string(),
        label: "Screenshot".to_string(),
        description: "Return a screenshot".to_string(),
        parameters: t::object([] as [(&str, pi_js::vendor::typebox::Schema); 0]).into(),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(|_id, _params, _sig, _upd| {
            Box::pin(async move {
                Ok(AgentToolResult {
                    content: vec![
                        UserContentBlock::Text(TextContent::new("captured")),
                        UserContentBlock::Image(pi_ai::ImageContent {
                            data: TINY_PNG_BASE64.to_string(),
                            mime_type: "image/png".to_string(),
                        }),
                    ],
                    details: Some(json!({})),
                    ..Default::default()
                })
            })
        }),
        replay: None,
    }
}

fn agent_tool_to_definition(tool: AgentTool) -> ToolDefinition {
    let exec = Arc::clone(&tool.execute);
    ToolDefinition {
        name: tool.name,
        label: tool.label,
        description: tool.description,
        parameters: tool.parameters.into(),
        prompt_snippet: None,
        prompt_guidelines: None,
        exposure: None,
        default_active: None,
        namespace: None,
        output_schema: tool.output_schema.map(pi_js::vendor::typebox::Schema::from),
        annotations: None,
        constrained_sampling: None,
        execution_mode: tool.execution_mode,
        prepare_loadout: None,
        prepare_arguments: tool.prepare_arguments,
        render_shell: None,
        render_call: None,
        render_result: None,
        execute: Arc::new(move |id, params, signal, on_update, _ctx| exec(id, params, signal, on_update)),
    }
}

fn usage(input: i64, cost: f64) -> Usage {
    Usage {
        input,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: input,
        cost: UsageCost {
            input: cost,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: cost,
            total_from_provider: None,
        },
    }
}

fn codemode_result(harness: &Harness) -> ToolResultMessage {
    get_tool_result(harness, "codemode")
}

fn result_text(message: &ToolResultMessage) -> String {
    let mut iter = message.content.iter();
    let Some(UserContentBlock::Text(header)) = iter.next() else {
        panic!("expected script header text block");
    };
    assert!(
        (header.text.starts_with("Script completed\n") || header.text.starts_with("Script failed\n"))
            && header.text.contains("\nWall time ")
            && header.text.ends_with(" seconds\nOutput:\n"),
        "unexpected script header: {:?}",
        header.text
    );
    iter.map(|block| match block {
        UserContentBlock::Text(t) => t.text.clone(),
        UserContentBlock::Image(_) => "<image>".to_string(),
    })
    .collect::<Vec<_>>()
    .join("\n")
}

fn parse_details(message: &ToolResultMessage) -> CodemodeToolDetails {
    let val = message.details.clone().expect("codemode details");
    pi_js::json::parse_as::<CodemodeToolDetails>(&pi_js::json::stringify(&val)).unwrap()
}

async fn setup_codemode_tools(extra: Vec<InlineExtension>) -> Harness {
    let register_tools = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        for tool in [make_echo_tool(), make_stats_tool(), make_screenshot_tool()] {
            pi.register_tool(agent_tool_to_definition(tool))?;
        }
        Ok(())
    }));
    let mut factories = vec![
        InlineExtension::Factory(create_codemode_extension(CodemodeExtensionOptions::default())),
        register_tools,
    ];
    factories.extend(extra);
    create_harness(HarnessOptions {
        initial_active_tool_names: Some(vec!["codemode".to_string()]),
        extension_factories: Some(factories),
        ..Default::default()
    })
    .await
}

async fn setup_store_harness() -> Harness {
    create_harness(HarnessOptions {
        initial_active_tool_names: Some(vec!["codemode".to_string()]),
        extension_factories: Some(vec![InlineExtension::Factory(create_codemode_extension(
            CodemodeExtensionOptions::default(),
        ))]),
        ..Default::default()
    })
    .await
}

async fn run_codemode(harness: &Harness, code: &str) -> ToolResultMessage {
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![FauxContentBlock::ToolCall(faux_tool_call(
                "codemode",
                [("code".to_string(), json!(code))].into_iter().collect(),
                None,
            ))]),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("done".to_string()),
            None,
        )),
    ]);
    harness.session.prompt("go", None).await.unwrap();
    codemode_result(harness)
}

fn store_entries(harness: &Harness) -> Vec<Option<Value>> {
    harness
        .session_manager
        .get_branch(None)
        .into_iter()
        .filter_map(|e| match e {
            SessionEntry::Custom(c) if c.custom_type == CODEMODE_STORE_ENTRY_TYPE => Some(c.data),
            _ => None,
        })
        .collect()
}

const INCREMENT_SCRIPT: &str = "const next = (load(\"count\") ?? 0) + 1;\nstore(\"count\", next);\nreturn next;";

#[tokio::test]
async fn presents_callable_tools_per_codemode_mode() {
    let harness = setup_codemode_tools(Vec::new()).await;
    let description = |name: &str| -> String {
        harness
            .session
            .agent()
            .state()
            .tools
            .into_iter()
            .find(|t| t.name == name)
            .map(|t| t.description)
            .unwrap_or_default()
    };
    let request_tools = Arc::new(std::sync::Mutex::new(Vec::<Vec<String>>::new()));
    let request_prompts = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));

    harness
        .session
        .set_active_tools_by_name(&["read".to_string(), "echo".to_string(), "codemode".to_string()]);
    assert!(description("echo").contains("Codemode: `tools.echo(args)` resolves to"));
    assert!(!description("echo").contains("codemode tool declaration:"));
    assert!(!description("codemode").contains("### `echo`"));

    for mode_label in ["on", "only"] {
        if mode_label == "only" {
            harness.settings_manager.apply_overrides(&Settings {
                codemode: Some(CodemodeSettings {
                    mode: Some(CodemodeMode::Only),
                    ..Default::default()
                }),
                ..Default::default()
            });
            harness
                .session
                .set_active_tools_by_name(&["read".to_string(), "echo".to_string(), "codemode".to_string()]);
            assert!(!description("echo").contains("Codemode: `tools.echo"));
            assert!(description("codemode").contains("### `echo`"));
            assert!(!description("codemode").contains("### `stats`"));
        }
        let rt_c = Arc::clone(&request_tools);
        let rp_c = Arc::clone(&request_prompts);
        harness.set_responses(vec![FauxResponseStep::Factory(Arc::new(
            move |ctx, _opts, _st, _mdl| {
                let rt = Arc::clone(&rt_c);
                let rp = Arc::clone(&rp_c);
                Box::pin(async move {
                    rt.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(get_current_tools(&ctx.messages).into_iter().map(|t| t.name).collect());
                    rp.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(get_current_system_prompt(&ctx.messages));
                    Ok(faux_assistant_message(
                        FauxAssistantMessageContent::Text("ok".to_string()),
                        None,
                    ))
                })
            },
        ))]);
        harness.session.prompt(mode_label, None).await.unwrap();
    }

    let rt = request_tools
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let rp = request_prompts
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert!(rt[0].contains(&"read".to_string()));
    assert!(rt[0].contains(&"echo".to_string()));
    assert!(rt[0].contains(&"codemode".to_string()));
    assert!(rp[0].contains("\n- read: "));

    assert!(rt[1].contains(&"codemode".to_string()));
    assert!(!rt[1].contains(&"echo".to_string()));
    assert!(!rt[1].contains(&"read".to_string()));
    assert!(!rp[1].contains("\n- read: "));
    assert!(rp[1].contains("\n- codemode: "));
    assert!(!harness.session.system_prompt().contains("\n- read: "));

    harness.session.set_active_tools_by_name(&["echo".to_string()]);
    assert_eq!(description("echo"), "Echo text back.\n\nSecond paragraph.");
    harness.cleanup();
}

#[tokio::test]
async fn runs_nested_calls_in_parallel_and_returns_only_the_script_result() {
    let harness = setup_codemode_tools(Vec::new()).await;
    let code = r#"
        const [a, b, stats] = await Promise.all([
            tools.echo({ text: "one" }),
            tools.echo({ text: "two" }),
            tools.stats({}),
        ]);
        console.log("files", stats.files);
        text(ALL_TOOLS.map((tool) => tool.name).join(","));
        return { a, b, names: stats.names };
    "#;
    let result = run_codemode(&harness, code).await;
    assert!(!result.is_error);
    assert_eq!(
        result_text(&result),
        "==> text 1/2 <==\necho,stats,screenshot\n==> text 2/2 <==\n{\"a\":\"echo: one\",\"b\":\"echo: two\",\"names\":[\"a\",\"b\"]}\n<console_output>\nfiles 2\n</console_output>"
    );
    let details = parse_details(&result);
    let statuses: Vec<(&str, &str)> = details
        .calls
        .iter()
        .map(|c| {
            (
                c.name.as_str(),
                match c.status {
                    pi_coding_agent::extensions::codemode::tool::CodemodeNestedCallStatus::Running => "running",
                    pi_coding_agent::extensions::codemode::tool::CodemodeNestedCallStatus::Ok => "ok",
                    pi_coding_agent::extensions::codemode::tool::CodemodeNestedCallStatus::Error => "error",
                    pi_coding_agent::extensions::codemode::tool::CodemodeNestedCallStatus::Cancelled => "cancelled",
                },
            )
        })
        .collect();
    assert_eq!(statuses, vec![("echo", "ok"), ("echo", "ok"), ("stats", "ok")]);
    assert!(
        details
            .calls
            .iter()
            .all(|c| c.id.starts_with(&format!("{}/", result.tool_call_id)))
    );
    let tool_results_count = harness
        .session
        .messages()
        .into_iter()
        .filter(|m| m.role() == Some("toolResult"))
        .count();
    assert_eq!(tool_results_count, 1);
    harness.cleanup();
}

#[tokio::test]
async fn routes_nested_calls_through_extension_hooks() {
    let hook_ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<ToolCallEvent>(|event, _ctx| {
            Box::pin(async move {
                if event.tool_name() == "echo"
                    && event.input_value().get("text").and_then(Value::as_str) == Some("forbidden")
                {
                    return Ok(Some(ToolCallEventResult {
                        block: Some(true),
                        reason: Some("echo of forbidden text is blocked".to_string()),
                        terminate: None,
                    }));
                }
                Ok(None)
            })
        });
        pi.on::<ToolResultEvent>(|event, _ctx| {
            Box::pin(async move {
                if event.tool_name() == "stats" {
                    return Ok(Some(ToolResultEventResult {
                        content: Some(vec![ToolResultContentBlock::Text(TextContent::new("redacted"))]),
                        ..Default::default()
                    }));
                }
                Ok(None)
            })
        });
        Ok(())
    }));

    let harness = setup_codemode_tools(vec![hook_ext]).await;
    let code = r#"
        let blocked;
        try {
            await tools.echo({ text: "forbidden" });
        } catch (error) {
            blocked = error.message;
        }
        const stats = await tools.stats({});
        return { blocked, stats };
    "#;
    let result = run_codemode(&harness, code).await;
    let parsed: Value = pi_js::json::parse(&result_text(&result)).unwrap();
    assert_eq!(
        parsed,
        json!({
            "blocked": "echo of forbidden text is blocked",
            "stats": "redacted",
        })
    );
    let details = parse_details(&result);
    assert_eq!(details.calls.len(), 2);
    harness.cleanup();
}

#[tokio::test]
async fn adds_the_usage_of_nested_results_to_the_codemode_result() {
    let billed_tool = AgentTool {
        name: "billed".to_string(),
        label: "Billed".to_string(),
        description: "Run a model".to_string(),
        parameters: t::object([] as [(&str, pi_js::vendor::typebox::Schema); 0]).into(),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(|_id, _params, _sig, _upd| {
            Box::pin(async move {
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new("ran"))],
                    details: Some(json!({})),
                    usage: Some(usage(100, 0.25)),
                    ..Default::default()
                })
            })
        }),
        replay: None,
    };
    let billed_ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let bt = billed_tool.clone();
        async move { pi.register_tool(agent_tool_to_definition(bt)) }
    }));

    let harness = setup_codemode_tools(vec![billed_ext]).await;
    let result = run_codemode(
        &harness,
        "await tools.billed({}); await tools.billed({}); await tools.echo({ text: \"x\" });",
    )
    .await;

    let ru = result.usage.clone().expect("codemode result usage");
    assert_eq!(ru.input, 200);
    assert_eq!(ru.total_tokens, 200);
    assert_eq!(ru.cost.total, 0.5);
    let persisted = harness
        .session_manager
        .get_entries()
        .into_iter()
        .find_map(|e| match e {
            SessionEntry::Message(m) => match m.message {
                pi_agent_core::types::AgentMessage::Message(pi_ai::Message::ToolResult(tr)) => Some(tr),
                _ => None,
            },
            _ => None,
        })
        .expect("persisted toolResult");
    assert_eq!(persisted.usage, result.usage);
    assert_eq!(harness.session.get_session_stats().cost, 0.5);
    harness.cleanup();
}

#[tokio::test]
async fn keeps_structured_content_that_tool_result_handlers_replace_along_with_the_content() {
    let hook_ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<ToolResultEvent>(|event, _ctx| {
            Box::pin(async move {
                if event.tool_name() == "stats" {
                    Ok(Some(ToolResultEventResult {
                        content: Some(vec![ToolResultContentBlock::Text(TextContent::new("0 files"))]),
                        structured_content: Some(json!({ "files": 0, "names": [] })),
                        ..Default::default()
                    }))
                } else {
                    Ok(None)
                }
            })
        });
        pi.on::<ToolResultEvent>(|event, _ctx| {
            Box::pin(async move {
                if event.tool_name() == "stats" {
                    Ok(Some(ToolResultEventResult {
                        details: Some(json!({ "audited": true })),
                        ..Default::default()
                    }))
                } else {
                    Ok(None)
                }
            })
        });
        Ok(())
    }));

    let harness = setup_codemode_tools(vec![hook_ext]).await;
    let result = run_codemode(&harness, "return await tools.stats({});").await;
    let parsed: Value = pi_js::json::parse(&result_text(&result)).unwrap();
    assert_eq!(parsed, json!({ "files": 0, "names": [] }));
    harness.cleanup();
}

#[tokio::test]
async fn attaches_only_the_images_the_script_passes_to_image_in_output_order() {
    let harness = setup_codemode_tools(Vec::new()).await;
    let code = format!(
        "const shot = await tools.screenshot({{}});\ntext(shot);\nimage(\"data:image/png;base64,{TINY_PNG_BASE64}\");\nimage(\"data:image/png;base64,{TINY_PNG_BASE64}\");\ntext(\"after\");"
    );
    let result = run_codemode(&harness, &code).await;
    let rendered = result_text(&result);
    let lines: Vec<&str> = rendered.split('\n').collect();
    assert_eq!(lines.len(), 8, "rendered={rendered}");
    assert_eq!(lines[0], "==> text 1/2 <==");
    assert_eq!(lines[1], "captured");
    assert_eq!(lines[3], "<image>");
    assert_eq!(lines[4], lines[2]);
    assert_eq!(lines[5], "<image>");
    assert_eq!(lines[6], "==> text 2/2 <==");
    assert_eq!(lines[7], "after");
    let saved_path = lines[2]
        .strip_prefix("[Image saved to ")
        .and_then(|s| s.split(" (image/png,").next())
        .expect("saved image label");
    let saved_bytes = std::fs::read(saved_path).expect("saved image file");
    assert_eq!(pi_js::b64::encode(&saved_bytes), TINY_PNG_BASE64);
    let _ = std::fs::remove_file(saved_path);
    assert_eq!(
        result.content.get(2),
        Some(&UserContentBlock::Image(pi_ai::ImageContent {
            data: TINY_PNG_BASE64.to_string(),
            mime_type: "image/png".to_string(),
        }))
    );
    harness.cleanup();
}

#[tokio::test]
async fn reports_script_failures_as_results_that_keep_partial_output_and_the_calls_that_ran() {
    let harness = setup_codemode_tools(Vec::new()).await;
    let result = run_codemode(
        &harness,
        "text(\"partial\");\nawait tools.echo({ text: \"x\" });\nthrow new Error(\"boom\");",
    )
    .await;
    assert!(result.is_error);
    let text = result_text(&result);
    assert!(text.starts_with("partial\nScript error:\nError: boom\n"));
    assert!(text.contains("codemode.js:3"));
    assert!(text.contains("Tool calls made before the failure (they are not undone): echo (ok)"));
    let details = parse_details(&result);
    let call_names: Vec<&str> = details.calls.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(call_names, vec!["echo"]);
    harness.cleanup();
}

#[tokio::test]
async fn applies_the_timeout_ms_option_and_rejects_invalid_options() {
    let harness = setup_store_harness().await;
    let timed_out = run_codemode(&harness, "// @options: {\"timeout_ms\": 200}\nwhile (true) {}").await;
    assert!(timed_out.is_error);
    assert!(result_text(&timed_out).contains("Script error:\nScript timed out"));

    let invalid = run_codemode(&harness, "// @options: {\"yield\": 1}\ntext(1)").await;
    assert!(invalid.is_error);
    assert_eq!(
        invalid.content,
        vec![UserContentBlock::Text(TextContent::new(
            "@options only supports `max_output_tokens` and `timeout_ms`; got `yield`"
        ))]
    );
    harness.cleanup();
}

#[tokio::test]
async fn limits_script_memory_so_runaway_allocations_fail_inside_the_script() {
    let harness = setup_store_harness().await;
    let result = run_codemode(
        &harness,
        "// @options: {\"timeout_ms\": 30000}\nlet a = [];\ntry { while (true) a.push(\"x\".repeat(1 << 20) + a.length); } catch (error) { const n = a.length; a = null; return { n, error: String(error) }; }",
    )
    .await;
    assert!(!result.is_error);
    let parsed: Value = pi_js::json::parse(&result_text(&result)).unwrap();
    let n = parsed.get("n").and_then(Value::as_i64).unwrap();
    let err = parsed.get("error").and_then(Value::as_str).unwrap();
    assert!(err.contains("out of memory"));
    assert!(n < 512);
    harness.cleanup();
}

#[tokio::test]
async fn truncates_output_to_the_token_budget_and_spills_the_full_text() {
    let harness = setup_store_harness().await;
    let code = format!(
        "// @options: {{\"max_output_tokens\": 30}}\nfor (let i = 0; i < 100; i++) text(\"row \" + i);\nimage(\"data:image/png;base64,{TINY_PNG_BASE64}\");"
    );
    let result = run_codemode(&harness, &code).await;
    let details = parse_details(&result);
    let path = details.full_output_path.expect("spill path");
    let body = result_text(&result);
    assert!(body.starts_with("Warning: truncated output"));
    assert!(body.contains("row 0\n"));
    assert!(body.contains("tokens truncated"));
    assert!(body.contains("row 99\n"));
    assert!(!body.contains("row 50\n"));
    assert!(body.contains(&format!("[Full output: {path} (read with offset/limit)]")));
    assert_eq!(
        result.content.last(),
        Some(&UserContentBlock::Image(pi_ai::ImageContent {
            data: TINY_PNG_BASE64.to_string(),
            mime_type: "image/png".to_string(),
        }))
    );
    let expected_spilled = (0..100)
        .map(|i| format!("==> text {}/100 <==\nrow {i}", i + 1))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(pi_js::fs::read_to_string(&path).unwrap(), expected_spilled);
    let _ = pi_js::fs::rm(&path, false, true);

    let small = run_codemode(&harness, "return { ok: true };").await;
    assert_eq!(parse_details(&small).full_output_path, None);
    harness.cleanup();
}

#[tokio::test]
async fn resolves_bash_calls_to_structured_results_also_for_non_zero_exit_codes() {
    let harness = create_harness(HarnessOptions {
        initial_active_tool_names: Some(vec!["codemode".to_string(), "bash".to_string()]),
        extension_factories: Some(vec![InlineExtension::Factory(create_codemode_extension(
            CodemodeExtensionOptions::default(),
        ))]),
        ..Default::default()
    })
    .await;
    let result = run_codemode(
        &harness,
        "const r = await tools.bash({ command: \"echo out; exit 3\" });\ntext(JSON.stringify([r.output, r.exit_code, typeof r.wall_time_seconds]));",
    )
    .await;
    assert!(!result.is_error);
    assert_eq!(result_text(&result), "[\"out\\n\",3,\"number\"]");
    harness.cleanup();
}

#[tokio::test]
async fn persists_store_writes_as_custom_entries_for_later_calls() {
    let harness = setup_store_harness().await;
    assert_eq!(result_text(&run_codemode(&harness, INCREMENT_SCRIPT).await), "1");
    assert_eq!(result_text(&run_codemode(&harness, INCREMENT_SCRIPT).await), "2");
    assert_eq!(
        store_entries(&harness),
        vec![
            Some(json!({ "set": { "count": 1 }, "delete": [] })),
            Some(json!({ "set": { "count": 2 }, "delete": [] })),
        ]
    );
    let appended_count = harness
        .events()
        .into_iter()
        .filter(|e| {
            matches!(
                e,
                AgentSessionEvent::EntryAppended { entry: SessionEntry::Custom(c), .. }
                if c.custom_type == CODEMODE_STORE_ENTRY_TYPE
            )
        })
        .count();
    assert_eq!(appended_count, 2);

    assert_eq!(
        result_text(
            &run_codemode(
                &harness,
                "store(\"count\", undefined);\nreturn load(\"count\") === undefined;"
            )
            .await
        ),
        "true"
    );
    assert_eq!(
        store_entries(&harness).last().cloned().flatten(),
        Some(json!({ "set": {}, "delete": ["count"] }))
    );
    harness.cleanup();
}

#[tokio::test]
async fn appends_nothing_for_failed_scripts_or_scripts_without_writes() {
    let harness = setup_store_harness().await;
    assert!(
        run_codemode(&harness, "store(\"count\", 5);\nthrow new Error(\"boom\");")
            .await
            .is_error
    );
    assert!(
        !run_codemode(&harness, "return load(\"count\") ?? \"missing\";")
            .await
            .is_error
    );
    assert_eq!(store_entries(&harness), Vec::<Option<Value>>::new());
    harness.cleanup();
}

#[tokio::test]
async fn runs_without_a_session_starting_from_an_empty_store() {
    let tool = create_codemode_tool(&[], CodemodeToolOptions::default());
    let result = (tool.execute)("direct".to_string(), json!({ "code": INCREMENT_SCRIPT }), None, None)
        .await
        .unwrap();
    assert_eq!(
        result.content.get(1),
        Some(&UserContentBlock::Text(TextContent::new("1")))
    );
}

#[tokio::test]
async fn loads_the_values_written_on_the_current_branch() {
    let harness = setup_store_harness().await;
    run_codemode(&harness, INCREMENT_SCRIPT).await;
    let first_prompt_id = harness
        .session_manager
        .get_branch(None)
        .into_iter()
        .find_map(|e| match e {
            SessionEntry::Message(m) => Some(m.id),
            _ => None,
        })
        .expect("first prompt entry");
    assert_eq!(result_text(&run_codemode(&harness, INCREMENT_SCRIPT).await), "2");

    harness.session_manager.branch(&first_prompt_id).unwrap();
    assert_eq!(result_text(&run_codemode(&harness, INCREMENT_SCRIPT).await), "1");
    harness.cleanup();
}

#[test]
fn folds_store_entries_from_the_root_ignoring_malformed_data() {
    let make_entry = |data: Value, custom_type: &str| -> SessionEntry {
        SessionEntry::Custom(CustomEntry {
            id: "entry".to_string(),
            parent_id: None,
            timestamp: "1970-01-01T00:00:00.000Z".to_string(),
            custom_type: custom_type.to_string(),
            data: Some(data),
        })
    };
    let folded = read_codemode_store(&[
        make_entry(
            json!({ "set": { "a": 1, "b": { "c": 2 } }, "delete": [] }),
            CODEMODE_STORE_ENTRY_TYPE,
        ),
        make_entry(json!({ "set": { "a": 3 }, "delete": ["b"] }), CODEMODE_STORE_ENTRY_TYPE),
        make_entry(json!({ "set": { "z": 1 } }), CODEMODE_STORE_ENTRY_TYPE),
        make_entry(json!({ "set": { "other": 1 }, "delete": [] }), "other-extension"),
    ]);
    let mut expected = IndexMap::new();
    expected.insert("a".to_string(), json!(3));
    assert_eq!(folded, expected);
}

#[tokio::test]
async fn declares_models_only_for_the_sessions_own_codemode_tool() {
    let harness = setup_store_harness().await;
    let desc = harness
        .session
        .agent()
        .state()
        .tools
        .into_iter()
        .find(|t| t.name == "codemode")
        .map(|t| t.description)
        .unwrap_or_default();
    assert!(desc.contains("`models`: classifiers and image generation"));
    assert!(desc.contains(&codemode_docs_path()));

    let overridden = create_harness(HarnessOptions {
        tools: Some(vec![create_codemode_tool(&[], CodemodeToolOptions::default())]),
        ..Default::default()
    })
    .await;
    let plain_desc = overridden
        .session
        .agent()
        .state()
        .tools
        .into_iter()
        .find(|t| t.name == "codemode")
        .map(|t| t.description)
        .unwrap_or_default();
    assert!(!plain_desc.contains("`models`"));
    overridden.cleanup();
    harness.cleanup();
}

async fn setup_codemode_models_harness(active: Arc<AtomicUsize>, max_active: Arc<AtomicUsize>) -> Harness {
    let act_c = Arc::clone(&active);
    let max_c = Arc::clone(&max_active);

    let scorer_ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let act = Arc::clone(&act_c);
        let max_a = Arc::clone(&max_c);
        async move {
            let mut classifiers = IndexMap::new();
            classifiers.insert(
                "test-classifier".to_string(),
                ProviderClassifier {
                    classify: Arc::new(move |model, ctx, opts| {
                        let act = Arc::clone(&act);
                        let max_a = Arc::clone(&max_a);
                        let model = model.clone();
                        let ctx = ctx.clone();
                        Box::pin(async move {
                            let cur = act.fetch_add(1, Ordering::SeqCst) + 1;
                            max_a.fetch_max(cur, Ordering::SeqCst);
                            let _ = pi_js::time::sleep(10, None).await;
                            act.fetch_sub(1, Ordering::SeqCst);
                            let txt = ctx.state.get("text").and_then(Value::as_str).unwrap_or("");
                            assert_eq!(model.base_url, "https://classifier.test/v1");
                            assert_eq!(opts.as_ref().and_then(|o| o.api_key.as_deref()), Some("secret-key"));
                            if txt == "explode" {
                                return ClassifierResult {
                                    api: model.api,
                                    provider: model.provider,
                                    model: model.id,
                                    answers: IndexMap::new(),
                                    usage: None,
                                    stop_reason: ClassifierStopReason::Error,
                                    error_message: Some("classifier exploded".to_string()),
                                    timestamp: 0,
                                };
                            }
                            let mut answers = IndexMap::new();
                            answers.insert(
                                "approved".to_string(),
                                ClassifierAnswer::Bool {
                                    probability: if txt == "good" { 0.9 } else { 0.1 },
                                },
                            );
                            ClassifierResult {
                                api: model.api,
                                provider: model.provider,
                                model: model.id,
                                answers,
                                usage: Some(usage(300, 0.001)),
                                stop_reason: ClassifierStopReason::Stop,
                                error_message: None,
                                timestamp: 0,
                            }
                        })
                    }),
                },
            );
            let mut images = IndexMap::new();
            images.insert(
                "test-images".to_string(),
                ProviderImages {
                    generate_images: Arc::new(|model, ctx, opts| {
                        let model = model.clone();
                        let ctx = ctx.clone();
                        Box::pin(async move {
                            assert_eq!(model.base_url, "https://images.test/v1");
                            assert_eq!(opts.as_ref().and_then(|o| o.api_key.as_deref()), Some("secret-key"));
                            let prompt = ctx
                                .input
                                .iter()
                                .find_map(|b| match b {
                                    UserContentBlock::Text(t) => Some(t.text.as_str()),
                                    _ => None,
                                })
                                .unwrap_or("");
                            if prompt == "explode" {
                                return AssistantImages {
                                    api: model.api,
                                    provider: model.provider,
                                    model: model.id,
                                    output: Vec::new(),
                                    response_id: None,
                                    usage: None,
                                    stop_reason: ImagesStopReason::Error,
                                    error_message: Some("painter exploded".to_string()),
                                    timestamp: 0,
                                };
                            }
                            AssistantImages {
                                api: model.api,
                                provider: model.provider,
                                model: model.id,
                                output: vec![
                                    ImagesOutputContent::Text(TextContent::new(format!("painted {prompt}"))),
                                    ImagesOutputContent::Image(ImageContent {
                                        data: TINY_PNG_BASE64.to_string(),
                                        mime_type: "image/png".to_string(),
                                    }),
                                ],
                                response_id: None,
                                usage: Some(usage(100, 0.04)),
                                stop_reason: ImagesStopReason::Stop,
                                error_message: None,
                                timestamp: 0,
                            }
                        })
                    }),
                },
            );
            let mut headers = IndexMap::new();
            headers.insert("X-Secret".to_string(), "hunter2".to_string());
            pi.register_provider(
                "scorer",
                ProviderConfig {
                    api_key: Some("secret-key".to_string()),
                    models: Some(vec![
                        ProviderModelConfig::Classifier(ProviderClassifierModelConfig {
                            id: "judge".to_string(),
                            name: "Judge".to_string(),
                            api: Some("test-classifier".to_string()),
                            base_url: Some("https://classifier.test/v1".to_string()),
                            input: vec![ModelInput::Text],
                            input_limits: None,
                            cost: ModelCost::default(),
                            headers: Some(headers),
                            context_window: 1_000,
                            compat: None,
                        }),
                        ProviderModelConfig::Image(ProviderImageModelConfig {
                            id: "painter".to_string(),
                            name: "Painter".to_string(),
                            api: Some("test-images".to_string()),
                            base_url: Some("https://images.test/v1".to_string()),
                            input: vec![ModelInput::Text, ModelInput::Image],
                            input_limits: None,
                            cost: ModelCost::default(),
                            headers: None,
                            output: vec![ModelInput::Text, ModelInput::Image],
                        }),
                    ]),
                    classifiers: Some(classifiers),
                    images: Some(images),
                    ..Default::default()
                },
            )?;
            Ok(())
        }
    }));

    create_harness(HarnessOptions {
        initial_active_tool_names: Some(vec!["codemode".to_string()]),
        extension_factories: Some(vec![
            InlineExtension::Factory(create_codemode_extension(CodemodeExtensionOptions::default())),
            scorer_ext,
        ]),
        ..Default::default()
    })
    .await
}

#[tokio::test]
async fn lists_models_and_classifies_with_catalog_auth_ignoring_script_supplied_fields() {
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let harness = setup_codemode_models_harness(Arc::clone(&active), Arc::clone(&max_active)).await;

    let result = run_codemode(
        &harness,
        r#"
        const [model] = await models.getAvailableOfType("classifier", "scorer");
        const listed = await models.getModelsOfType("classifier");
        const same = await models.getModelOfType("classifier", "scorer", "judge");
        const texts = ["good", "bad", "good", "bad", "good", "bad"];
        const questions = { approved: { type: "bool", instructions: "Approval?", criteria: { true: "yes", false: "no" } } };
        const results = await Promise.all(
            texts.map((text) => models.classify({ ...model, baseUrl: "https://evil.test" }, { state: { text }, questions })),
        );
        return {
            id: model.id,
            headers: "headers" in model,
            listed: listed.some((entry) => entry.provider === "scorer" && entry.id === "judge"),
            same: same.id,
            missing: (await models.getModelOfType("classifier", "scorer", "nope")) === undefined,
            probabilities: results.map((r) => r.answers.approved.probability),
            cost: results[0].usage.cost.total,
        };
        "#,
    )
    .await;
    assert!(!result.is_error);
    assert_eq!(max_active.load(Ordering::SeqCst), 4);
    harness.cleanup();
}

#[tokio::test]
async fn generates_images_with_catalog_auth_and_attaches_them_through_image() {
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let harness = setup_codemode_models_harness(active, max_active).await;

    let code = format!(
        r#"
        const [model] = await models.getAvailableOfType("image", "scorer");
        const reference = {{ type: "image", data: "{TINY_PNG_BASE64}", mimeType: "image/png" }};
        const generated = await models.generateImages(
            {{ ...model, baseUrl: "https://evil.test" }},
            {{ input: [{{ type: "text", text: "a fox" }}, reference] }},
        );
        for (const block of generated.output) {{
            if (block.type === "image") image(block);
            else text(block.text);
        }}
        const failed = await models.generateImages(model, {{ input: [{{ type: "text", text: "explode" }}] }});
        const attempt = async (fn) => {{ try {{ await fn(); return "ok"; }} catch (error) {{ return error.message; }} }};
        return {{
            id: model.id,
            stopReason: generated.stopReason,
            failed: [failed.stopReason, failed.errorMessage],
            wrongType: await attempt(() => models.generateImages({{ provider: "scorer", id: "judge" }}, {{ input: [] }})),
        }};
    "#
    );
    let result = run_codemode(&harness, &code).await;
    assert!(!result.is_error);
    let txt = result_text(&result);
    assert!(txt.contains("painted a fox"), "unexpected output: {txt}");
    assert!(txt.contains("\"stopReason\":\"stop\""), "unexpected output: {txt}");
    assert!(
        txt.contains("\"failed\":[\"error\",\"painter exploded\"]"),
        "unexpected output: {txt}"
    );
    assert!(
        txt.contains("\\\"scorer/judge\\\" is a classifier model, not an image model"),
        "unexpected output: {txt}"
    );
    assert_eq!(
        result.content.get(2),
        Some(&UserContentBlock::Image(ImageContent {
            data: TINY_PNG_BASE64.to_string(),
            mime_type: "image/png".to_string(),
        }))
    );
    harness.cleanup();
}

#[tokio::test]
async fn notes_generated_images_that_the_script_did_not_show() {
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let harness = setup_codemode_models_harness(active, max_active).await;

    let result = run_codemode(
        &harness,
        r#"
        const [model] = await models.getAvailableOfType("image", "scorer");
        const generated = await models.generateImages(model, { input: [{ type: "text", text: "a fox" }] });
        return generated.stopReason;
        "#,
    )
    .await;
    assert!(!result.is_error);
    assert_eq!(
        result_text(&result),
        "stop\nNote: models.generateImages() returned 1 image that the script did not show. Show each image block of result.output with image(block)."
    );
    harness.cleanup();
}

#[tokio::test]
async fn reports_provider_errors_as_results_and_invalid_arguments_as_exceptions() {
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let harness = setup_codemode_models_harness(active, max_active).await;
    let questions =
        r#"{ approved: { type: "bool", instructions: "Approval?", criteria: { true: "yes", false: "no" } } }"#;

    let code = format!(
        r#"
        const model = await models.getModelOfType("classifier", "scorer", "judge");
        const failed = await models.classify(model, {{ state: {{ text: "explode" }}, questions: {questions} }});
        const textOnly = await models.classify(model, {{
            state: {{ text: "good" }},
            images: [{{ type: "image", data: "aW1hZ2U=", mimeType: "image/png" }}],
            questions: {questions},
        }});
        const attempt = async (fn) => {{ try {{ await fn(); return "ok"; }} catch (error) {{ return error.message; }} }};
        return {{
            failed: [failed.stopReason, failed.errorMessage],
            textOnly: [textOnly.stopReason, textOnly.errorMessage],
            badClassifierImage: await attempt(() =>
                models.classify(model, {{ state: {{}}, images: [{{ data: "aW1hZ2U=" }}], questions: {questions} }}),
            ),
            badType: await attempt(() => models.getModelsOfType("video")),
            unknown: await attempt(() => models.classify({{ provider: "scorer", id: "nope" }}, {{}})),
            noModel: await attempt(() => models.classify("judge", {{}})),
            undefinedModel: await attempt(() => models.classify(undefined, {{}})),
            noState: await attempt(() => models.classify(model, {{ questions: {questions} }})),
            badQuestion: await attempt(() =>
                models.classify(model, {{ state: {{}}, questions: {{ kind: {{ type: "choice", instructions: "Kind?", criteria: ["a", "b"] }} }} }}),
            ),
            badImage: await attempt(() => models.generateImages({{ provider: "scorer", id: "painter" }}, {{ prompt: "a fox" }})),
            badSplit: await attempt(() => models.getModelOfType("classifier", "scorer/judge")),
        }};
    "#
    );
    let result = run_codemode(&harness, &code).await;
    assert!(!result.is_error);
    let parsed: Value = pi_js::json::parse(&result_text(&result)).unwrap();
    assert_eq!(parsed["failed"], json!(["error", "classifier exploded"]));
    assert_eq!(
        parsed["textOnly"],
        json!(["error", "Model scorer/judge does not accept image input"])
    );
    assert!(
        parsed["badClassifierImage"]
            .as_str()
            .unwrap_or("")
            .contains("models.classify() context.images[0] must be an image block")
    );
    assert!(
        parsed["badType"]
            .as_str()
            .unwrap_or("")
            .contains("Unknown model type \"video\"")
    );
    assert_eq!(
        parsed["unknown"],
        json!(
            "Unknown classifier model \"scorer/nope\". List the classifier models you can use with models.getAvailableOfType(\"classifier\")."
        )
    );
    assert!(
        parsed["noModel"]
            .as_str()
            .unwrap_or("")
            .contains("models.classify() expects a classifier model as its first argument, got a string.")
    );
    assert!(
        parsed["undefinedModel"]
            .as_str()
            .unwrap_or("")
            .contains("models.getModelOfType() returns undefined for an unknown provider or id.")
    );
    assert!(
        parsed["noState"]
            .as_str()
            .unwrap_or("")
            .contains("models.classify() context.state must be an object, got undefined.")
    );
    assert!(
        parsed["badQuestion"]
            .as_str()
            .unwrap_or("")
            .contains("context.questions.kind is a \"choice\" question")
    );
    assert!(
        parsed["badImage"]
            .as_str()
            .unwrap_or("")
            .contains("models.generateImages() context.input must be a non-empty array of blocks")
    );
    assert!(
        parsed["badSplit"]
            .as_str()
            .unwrap_or("")
            .contains("The provider and the id are separate arguments")
    );
    let details = parse_details(&result);
    let call_summary: Vec<(&str, &str, Option<&str>)> = details
        .calls
        .iter()
        .map(|c| {
            let status = match c.status {
                pi_coding_agent::extensions::codemode::tool::CodemodeNestedCallStatus::Ok => "ok",
                pi_coding_agent::extensions::codemode::tool::CodemodeNestedCallStatus::Error => "error",
                pi_coding_agent::extensions::codemode::tool::CodemodeNestedCallStatus::Running => "running",
                pi_coding_agent::extensions::codemode::tool::CodemodeNestedCallStatus::Cancelled => "cancelled",
            };
            (c.name.as_str(), status, c.error.as_deref())
        })
        .collect();
    assert_eq!(
        call_summary,
        vec![
            ("models.classify", "error", Some("classifier exploded")),
            (
                "models.classify",
                "error",
                Some("Model scorer/judge does not accept image input")
            ),
        ]
    );
    assert!(result.usage.is_none());
    harness.cleanup();
}
