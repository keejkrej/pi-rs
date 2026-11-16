//! Port of packages/ai/test/openai-completions-tool-choice.test.ts

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use indexmap::IndexMap;
use pi_ai::api::openai_completions::{ResolvedOpenAICompletionsCompat, convert_messages, get_compat};
use pi_ai::compat::{get_model, normalize_context, stream, stream_simple};
use pi_ai::types::{
    ApiStreamOptions, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, ChatTemplateKwargValue,
    ChatTemplateVarKind, ConstrainedSampling, ConstrainedSamplingConfig, Context, JsonSchemaStrict, Message, Model,
    ModelCompat, ModelCost, ModelInput, ModelThinkingLevel, OpenAICompletionsCompat, SimpleStreamOptions, StopReason,
    StreamOptions, TextContent, ThinkingContent, ThinkingFormat, ThinkingLevel, Tool, ToolCall, ToolChoice,
    ToolResultMessage, Usage, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_js::fetch::{Headers, Response};
use pi_js::time;
use pi_js::vendor::typebox::t;
use serde_json::{Value, json};

fn default_ok_sse() -> String {
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({
            "choices": [{ "delta": {}, "finish_reason": "stop" }],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "prompt_tokens_details": { "cached_tokens": 0 },
                "completion_tokens_details": { "reasoning_tokens": 0 },
            },
        })
    )
}

fn local_model(id: &str, name: &str, compat: Option<OpenAICompletionsCompat>) -> Model {
    Model {
        r#type: None,
        id: id.to_owned(),
        name: name.to_owned(),
        api: "openai-completions".to_owned(),
        provider: "local-vllm".to_owned(),
        base_url: "http://localhost:8000/v1".to_owned(),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 128_000,
        max_tokens: 8192,
        headers: None,
        sampling_params: None,
        compat: compat.map(ModelCompat::OpenAICompletions),
    }
}

fn openai_gpt4o_mini() -> Model {
    let mut m = get_model("openai", "gpt-4o-mini").unwrap();
    m.compat = None;
    m.api = "openai-completions".to_owned();
    m
}

async fn capture_payload(model: &Model, context: Context, opts: ApiStreamOptions) -> (Value, AssistantMessage) {
    let captured = Arc::new(Mutex::new(None::<Value>));
    let c_c = captured.clone();
    let mut o = opts;
    o.base.api_key = Some("test".to_owned());
    o.base.on_payload = Some(Arc::new(move |params, _| {
        let c = c_c.clone();
        Box::pin(async move {
            *c.lock().unwrap() = Some(params);
            Ok(None)
        })
    }));
    if o.base.fetch.is_none() {
        o.base.fetch = Some(Arc::new(|_| {
            Box::pin(async {
                Ok(Response::new(
                    200,
                    Headers::from_pairs([("content-type", "text/event-stream")]),
                    default_ok_sse(),
                ))
            })
        }));
    }
    let msg = stream(model, &context, Some(o)).result().await;
    (captured.lock().unwrap().take().unwrap(), msg)
}

async fn capture_simple_payload(
    model: &Model,
    context: Context,
    opts: SimpleStreamOptions,
) -> (Value, AssistantMessage) {
    let captured = Arc::new(Mutex::new(None::<Value>));
    let c_c = captured.clone();
    let mut o = opts;
    o.base.api_key = Some("test".to_owned());
    o.base.on_payload = Some(Arc::new(move |params, _| {
        let c = c_c.clone();
        Box::pin(async move {
            *c.lock().unwrap() = Some(params);
            Ok(None)
        })
    }));
    if o.base.fetch.is_none() {
        o.base.fetch = Some(Arc::new(|_| {
            Box::pin(async {
                Ok(Response::new(
                    200,
                    Headers::from_pairs([("content-type", "text/event-stream")]),
                    default_ok_sse(),
                ))
            })
        }));
    }
    let msg = stream_simple(model, &context, Some(o)).result().await;
    (captured.lock().unwrap().take().unwrap(), msg)
}

async fn run_sse_chunks(
    model: &Model,
    context: Context,
    chunks: Vec<Option<Value>>,
) -> (Vec<AssistantMessageEvent>, AssistantMessage) {
    let mut sse = String::new();
    for ch in chunks {
        match ch {
            None => sse.push_str("data: null\n\n"),
            Some(v) => sse.push_str(&format!("data: {v}\n\n")),
        }
    }
    sse.push_str("data: [DONE]\n\n");

    let s = stream_simple(
        model,
        &context,
        Some(SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("test".to_owned()),
                fetch: Some(Arc::new(move |_| {
                    let b = sse.clone();
                    Box::pin(async move {
                        Ok(Response::new(
                            200,
                            Headers::from_pairs([("content-type", "text/event-stream")]),
                            b,
                        ))
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    );

    let mut events = Vec::new();
    let mut reader = s.clone();
    while let Some(ev) = reader.next().await {
        events.push(ev);
    }
    let msg = s.result().await;
    (events, msg)
}

#[tokio::test]
async fn forwards_tool_choice_and_strict_settings() {
    let model = openai_gpt4o_mini();
    let ping_tool = Tool {
        name: "ping".to_owned(),
        description: "Ping tool".to_owned(),
        parameters: t::object([("ok", t::boolean())]),
        constrained_sampling: None,
    };

    // toolChoice: "required" with tools
    let (p1, _) = capture_payload(
        &model,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Call ping with ok=true".to_owned()),
                timestamp: time::now_ms(),
            })],
            tools: Some(vec![ping_tool.clone()]),
        },
        ApiStreamOptions::default().with_extra("toolChoice", json!("required")),
    )
    .await;
    assert_eq!(p1.get("tool_choice"), Some(&json!("required")));
    assert!(!p1.get("tools").and_then(Value::as_array).unwrap().is_empty());

    // toolChoice: "none" without tools
    let (p2, _) = capture_simple_payload(
        &model,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Summarize".to_owned()),
                timestamp: time::now_ms(),
            })],
            tools: None,
        },
        SimpleStreamOptions {
            tool_choice: Some(ToolChoice::None),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(p2.get("tool_choice"), Some(&json!("none")));
    assert!(p2.get("tools").is_none());

    // strict disabled via compat
    let non_strict_model = Model {
        compat: Some(ModelCompat::OpenAICompletions(OpenAICompletionsCompat {
            supports_strict_mode: Some(false),
            ..Default::default()
        })),
        ..model.clone()
    };
    let (p3, _) = capture_simple_payload(
        &non_strict_model,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Call ping".to_owned()),
                timestamp: time::now_ms(),
            })],
            tools: Some(vec![ping_tool]),
        },
        SimpleStreamOptions::default(),
    )
    .await;
    let fn_obj = p3["tools"][0]["function"].as_object().unwrap();
    assert!(!fn_obj.contains_key("strict"));

    // unknown OpenAI-compatible endpoints default to non-strict tools
    let unknown_local = local_model("local-model", "Local Model", None);
    let constrained_tool = Tool {
        name: "ping".to_owned(),
        description: "Ping tool".to_owned(),
        parameters: t::object([("required", t::string()), ("optional", t::string().optional())]),
        constrained_sampling: Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::JsonSchema {
            strict: JsonSchemaStrict::Prefer,
        })),
    };
    let (p4, _) = capture_simple_payload(
        &unknown_local,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Call ping".to_owned()),
                timestamp: time::now_ms(),
            })],
            tools: Some(vec![constrained_tool.clone()]),
        },
        SimpleStreamOptions::default(),
    )
    .await;
    assert!(p4["tools"][0]["function"].get("strict").is_none());
    assert_eq!(
        p4["tools"][0]["function"]["parameters"]["required"],
        json!(["required"])
    );

    // capable built-in Chat Completions model preserves strict tools
    let groq_oss = get_model("groq", "openai/gpt-oss-20b").unwrap();
    assert!(matches!(
        &groq_oss.compat,
        Some(ModelCompat::OpenAICompletions(c)) if c.supports_strict_mode == Some(true)
    ));
    let (p5, _) = capture_simple_payload(
        &groq_oss,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Call ping".to_owned()),
                timestamp: time::now_ms(),
            })],
            tools: Some(vec![constrained_tool]),
        },
        SimpleStreamOptions::default(),
    )
    .await;
    assert_eq!(p5["tools"][0]["function"]["strict"], json!(true));
    assert_eq!(
        p5["tools"][0]["function"]["parameters"]["required"],
        json!(["required", "optional"])
    );
}

#[tokio::test]
async fn maps_groq_and_zai_reasoning_tool_stream_and_metadata() {
    let groq_qwen = get_model("groq", "qwen/qwen3.6-27b").unwrap();
    let (pq, _) = capture_simple_payload(
        &groq_qwen,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Hi".to_owned()),
                timestamp: time::now_ms(),
            })],
            tools: None,
        },
        SimpleStreamOptions {
            reasoning: Some(ThinkingLevel::Medium),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(pq.get("reasoning_effort"), Some(&json!("default")));

    let groq_oss = get_model("groq", "openai/gpt-oss-20b").unwrap();
    let (po, _) = capture_simple_payload(
        &groq_oss,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Hi".to_owned()),
                timestamp: time::now_ms(),
            })],
            tools: None,
        },
        SimpleStreamOptions {
            reasoning: Some(ThinkingLevel::Medium),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(po.get("reasoning_effort"), Some(&json!("medium")));

    // z.ai tool_stream & effort metadata
    let ping = Tool {
        name: "ping".to_owned(),
        description: "Ping tool".to_owned(),
        parameters: t::object([("ok", t::boolean())]),
        constrained_sampling: None,
    };
    let glm52 = get_model("zai", "glm-5.2").unwrap();
    let (p_ts, _) = capture_simple_payload(
        &glm52,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Call ping".to_owned()),
                timestamp: time::now_ms(),
            })],
            tools: Some(vec![ping.clone()]),
        },
        SimpleStreamOptions::default(),
    )
    .await;
    assert_eq!(p_ts.get("tool_stream"), Some(&json!(true)));

    // no tools -> tool_stream omitted
    let (p_nots, _) = capture_simple_payload(
        &glm52,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Hi".to_owned()),
                timestamp: time::now_ms(),
            })],
            tools: None,
        },
        SimpleStreamOptions::default(),
    )
    .await;
    assert!(p_nots.get("tool_stream").is_none());

    for id in ["glm-4.7", "glm-5-turbo", "glm-5.2"] {
        let m = get_model("zai", id).unwrap();
        assert!(matches!(
            &m.compat,
            Some(ModelCompat::OpenAICompletions(c)) if c.zai_tool_stream == Some(true)
        ));
    }

    for id in ["glm-5.2", "glm-5.2-highspeed"] {
        let m = get_model("zai", id).unwrap();
        let tlm = m.thinking_level_map.as_ref().unwrap();
        assert_eq!(tlm.get(&ModelThinkingLevel::Off), Some(&Some("none".to_owned())));
        assert_eq!(tlm.get(&ModelThinkingLevel::Low), Some(&None));
        assert_eq!(tlm.get(&ModelThinkingLevel::High), Some(&Some("high".to_owned())));
        assert_eq!(tlm.get(&ModelThinkingLevel::Max), Some(&Some("max".to_owned())));
    }
    for prov in ["zai", "zai-coding-cn"] {
        let m = get_model(prov, "glm-5.3").unwrap();
        let tlm = m.thinking_level_map.as_ref().unwrap();
        assert_eq!(tlm.get(&ModelThinkingLevel::Off), Some(&None));
        assert_eq!(tlm.get(&ModelThinkingLevel::Low), Some(&Some("low".to_owned())));
        assert_eq!(tlm.get(&ModelThinkingLevel::High), Some(&Some("high".to_owned())));
        assert_eq!(tlm.get(&ModelThinkingLevel::Max), Some(&Some("max".to_owned())));
    }

    for (r, eff) in [
        (ThinkingLevel::Low, "high"),
        (ThinkingLevel::Medium, "high"),
        (ThinkingLevel::High, "high"),
        (ThinkingLevel::Max, "max"),
    ] {
        let (p, _) = capture_simple_payload(
            &glm52,
            Context {
                system_prompt: None,
                messages: vec![Message::User(UserMessage {
                    content: UserMessageContent::Text("Hi".to_owned()),
                    timestamp: time::now_ms(),
                })],
                tools: None,
            },
            SimpleStreamOptions {
                reasoning: Some(r),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(
            p.get("thinking"),
            Some(&json!({ "type": "enabled", "clear_thinking": false }))
        );
        assert_eq!(p.get("reasoning_effort"), Some(&json!(eff)));
    }

    // z.ai off -> disabled
    let (p_off, _) = capture_simple_payload(
        &glm52,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Hi".to_owned()),
                timestamp: time::now_ms(),
            })],
            tools: None,
        },
        SimpleStreamOptions::default(),
    )
    .await;
    assert_eq!(p_off.get("thinking"), Some(&json!({ "type": "disabled" })));
    assert!(p_off.get("reasoning_effort").is_none());

    // replay reasoning_content on z.ai
    let mut read_args = IndexMap::new();
    read_args.insert("path".to_owned(), json!("README.md"));
    let (p_rep, _) = capture_simple_payload(
        &glm52,
        Context {
            system_prompt: None,
            messages: vec![
                Message::User(UserMessage {
                    content: UserMessageContent::Text("Read README.md".to_owned()),
                    timestamp: 1,
                }),
                Message::Assistant(AssistantMessage {
                    content: vec![
                        AssistantContentBlock::Thinking(ThinkingContent {
                            thinking: "prior reasoning".to_owned(),
                            thinking_signature: Some("reasoning_content".to_owned()),
                            redacted: None,
                        }),
                        AssistantContentBlock::ToolCall(ToolCall::new("call_1", "read", read_args)),
                    ],
                    api: "openai-completions".to_owned(),
                    provider: "zai".to_owned(),
                    model: "glm-5.2".to_owned(),
                    response_model: None,
                    response_id: None,
                    provider_thinking_level: None,
                    thinking_level: None,
                    diagnostics: None,
                    usage: Usage::default(),
                    stop_reason: StopReason::ToolUse,
                    deferred: None,
                    error_message: None,
                    raw_stop_reason: None,
                    end_turn: None,
                    timestamp: 2,
                }),
                Message::ToolResult(ToolResultMessage {
                    tool_call_id: "call_1".to_owned(),
                    tool_name: "read".to_owned(),
                    content: vec![UserContentBlock::Text(TextContent::new("contents"))],
                    details: None,
                    is_error: false,
                    nested_calls: None,
                    usage: None,
                    timestamp: 3,
                }),
                Message::User(UserMessage {
                    content: UserMessageContent::Text("Continue".to_owned()),
                    timestamp: 4,
                }),
            ],
            tools: None,
        },
        SimpleStreamOptions {
            reasoning: Some(ThinkingLevel::High),
            ..Default::default()
        },
    )
    .await;
    let replayed_asst = p_rep["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m.get("role") == Some(&json!("assistant")))
        .unwrap();
    assert_eq!(replayed_asst.get("reasoning_content"), Some(&json!("prior reasoning")));
}

#[tokio::test]
async fn handles_stream_chunks_finish_reason_tool_coalescing_and_nulls() {
    // 1. Non-standard finish_reason -> error
    let glm52 = get_model("zai", "glm-5.2").unwrap();
    let (_, resp1) = run_sse_chunks(
        &glm52,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Hi".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        vec![
            Some(json!({ "choices": [{ "delta": { "content": "partial" }, "finish_reason": null }] })),
            Some(json!({
                "choices": [{ "delta": {}, "finish_reason": "network_error" }],
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": 1,
                    "prompt_tokens_details": { "cached_tokens": 0 },
                    "completion_tokens_details": { "reasoning_tokens": 0 },
                },
            })),
        ],
    )
    .await;
    assert_eq!(resp1.stop_reason, StopReason::Error);
    assert_eq!(
        resp1.error_message.as_deref(),
        Some("Provider finish_reason: network_error")
    );

    // 2. Ignores null stream chunks
    let gpt = openai_gpt4o_mini();
    let (_, resp2) = run_sse_chunks(
        &gpt,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Reply OK".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        vec![
            None,
            Some(json!({
                "id": "chatcmpl-test",
                "choices": [{ "delta": { "content": "OK" }, "finish_reason": null }],
            })),
            Some(json!({
                "id": "chatcmpl-test",
                "choices": [{ "delta": {}, "finish_reason": "stop" }],
                "usage": {
                    "prompt_tokens": 3,
                    "completion_tokens": 1,
                    "prompt_tokens_details": { "cached_tokens": 0 },
                    "completion_tokens_details": { "reasoning_tokens": 0 },
                },
            })),
        ],
    )
    .await;
    assert_eq!(resp2.stop_reason, StopReason::Stop);
    assert_eq!(resp2.response_id.as_deref(), Some("chatcmpl-test"));
    assert_eq!(resp2.usage.total_tokens, 4);
    assert_eq!(resp2.content, vec![AssistantContentBlock::Text(TextContent::new("OK"))]);

    // 3. Errors when stream ends after only null finish_reason chunks
    let (_, resp3) = run_sse_chunks(
        &gpt,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Reply".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        vec![
            Some(json!({ "id": "chatcmpl-trunc", "choices": [{ "delta": { "content": "partial" }, "finish_reason": null }] })),
            Some(json!({ "id": "chatcmpl-trunc", "choices": [{ "delta": { "content": " answer" }, "finish_reason": null }] })),
        ],
    )
    .await;
    assert_eq!(resp3.stop_reason, StopReason::Error);
    assert_eq!(
        resp3.error_message.as_deref(),
        Some("Stream ended without finish_reason")
    );

    // 4. Accepts streams without finish_reason when compat disables it
    let gpt_no_fr = Model {
        compat: Some(ModelCompat::OpenAICompletions(OpenAICompletionsCompat {
            supports_finish_reason: Some(false),
            ..Default::default()
        })),
        ..gpt.clone()
    };
    let (_, resp4) = run_sse_chunks(
        &gpt_no_fr,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Reply".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        vec![Some(json!({
            "id": "chatcmpl-no-fr",
            "choices": [{ "delta": { "content": "complete answer" }, "finish_reason": null }],
        }))],
    )
    .await;
    assert_eq!(resp4.stop_reason, StopReason::Stop);
    assert_eq!(resp4.error_message, None);
    assert_eq!(
        resp4.content,
        vec![AssistantContentBlock::Text(TextContent::new("complete answer"))]
    );

    // 5. Ignores empty custom objects on function tool call deltas
    let read_tool = Tool {
        name: "read".to_owned(),
        description: "Read".to_owned(),
        parameters: t::object([("path", t::string())]),
        constrained_sampling: None,
    };
    let (_, resp5) = run_sse_chunks(
        &gpt,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Read README.md".to_owned()),
                timestamp: 1,
            })],
            tools: Some(vec![read_tool.clone()]),
        },
        vec![Some(json!({
            "id": "chatcmpl-empty-custom",
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "read", "arguments": "{\"path\":\"README.md\"}" },
                        "custom": {},
                    }],
                },
                "finish_reason": "tool_calls",
            }],
        }))],
    )
    .await;
    let mut r_args = IndexMap::new();
    r_args.insert("path".to_owned(), json!("README.md"));
    assert_eq!(
        resp5.content,
        vec![AssistantContentBlock::ToolCall(ToolCall::new("call_1", "read", r_args))]
    );

    // 6. Coalesces tool call deltas by stable index when provider mutates ids
    let (evs6, resp6) = run_sse_chunks(
        &gpt,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Read README.md".to_owned()),
                timestamp: 1,
            })],
            tools: Some(vec![read_tool]),
        },
        vec![
            Some(json!({
                "id": "chatcmpl-kimi",
                "choices": [{
                    "delta": {
                        "tool_calls": [{ "index": 0, "id": "functions.read:0", "type": "function", "function": { "name": "read", "arguments": "" } }]
                    },
                    "finish_reason": null,
                }],
            })),
            Some(json!({
                "id": "chatcmpl-kimi",
                "choices": [{
                    "delta": {
                        "tool_calls": [{ "index": 0, "id": "chatcmpl-tool-a", "type": "function", "function": { "name": null, "arguments": "{\"path\":\"README" } }]
                    },
                    "finish_reason": null,
                }],
            })),
            Some(json!({
                "id": "chatcmpl-kimi",
                "choices": [{
                    "delta": {
                        "tool_calls": [{ "index": 0, "id": "chatcmpl-tool-b", "type": "function", "function": { "name": null, "arguments": ".md\"}" } }]
                    },
                    "finish_reason": "tool_calls",
                }],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 5,
                    "prompt_tokens_details": { "cached_tokens": 0 },
                    "completion_tokens_details": { "reasoning_tokens": 0 },
                },
            })),
        ],
    )
    .await;
    assert_eq!(resp6.stop_reason, StopReason::ToolUse);
    let tc_indices: Vec<usize> = evs6
        .iter()
        .filter_map(|e| match e {
            AssistantMessageEvent::ToolCallStart { content_index, .. }
            | AssistantMessageEvent::ToolCallDelta { content_index, .. }
            | AssistantMessageEvent::ToolCallEnd { content_index, .. } => Some(*content_index),
            _ => None,
        })
        .collect();
    assert_eq!(tc_indices, vec![0, 0, 0, 0, 0]);
    if let AssistantContentBlock::ToolCall(tc) = &resp6.content[0] {
        assert_eq!(tc.id, "functions.read:0");
        assert_eq!(tc.name, "read");
        assert_eq!(tc.arguments.get("path"), Some(&json!("README.md")));
    } else {
        panic!("expected ToolCall");
    }
}

#[tokio::test]
async fn accumulates_mixed_content_reasoning_and_parallel_tool_calls_independently() {
    let gpt = openai_gpt4o_mini();
    let tools = vec![
        Tool {
            name: "read".to_owned(),
            description: "Read".to_owned(),
            parameters: t::object([("path", t::string())]),
            constrained_sampling: None,
        },
        Tool {
            name: "grep".to_owned(),
            description: "Grep".to_owned(),
            parameters: t::object([("pattern", t::string()), ("path", t::string())]),
            constrained_sampling: None,
        },
        Tool {
            name: "list".to_owned(),
            description: "List".to_owned(),
            parameters: t::object([("path", t::string())]),
            constrained_sampling: None,
        },
        Tool {
            name: "write".to_owned(),
            description: "Write".to_owned(),
            parameters: t::object([("path", t::string()), ("content", t::string())]),
            constrained_sampling: None,
        },
    ];

    let chunks = vec![
        Some(json!({
            "id": "chatcmpl-mixed",
            "choices": [{
                "delta": {
                    "content": "answer 1",
                    "reasoning_content": "think 1",
                    "tool_calls": [
                        { "index": 0, "id": "tc_read_initial", "type": "function", "function": { "name": "read", "arguments": "{\"path\":\"README" } },
                        { "index": 1, "id": "tc_grep_initial", "type": "function", "function": { "name": "grep", "arguments": "{\"pattern\":\"TODO" } },
                        { "id": "tc_list_no_index", "type": "function", "function": { "name": "list", "arguments": "{\"path\":\"packages" } },
                        { "id": "tc_write_no_index", "type": "function", "function": { "name": "write", "arguments": "{\"path\":\"out" } },
                    ],
                },
                "finish_reason": null,
            }],
        })),
        Some(json!({
            "id": "chatcmpl-mixed",
            "choices": [{
                "delta": {
                    "content": " answer 2",
                    "tool_calls": [
                        { "index": 1, "id": "tc_grep_changed", "type": "function", "function": { "arguments": "\",\"path\":\"src" } },
                        { "id": "tc_write_no_index", "type": "function", "function": { "arguments": ".txt\",\"content\":\"ok\"}" } },
                        { "id": "tc_list_no_index", "type": "function", "function": { "arguments": "/ai\"}" } },
                    ],
                },
                "finish_reason": null,
            }],
        })),
        Some(json!({
            "id": "chatcmpl-mixed",
            "choices": [{
                "delta": {
                    "content": "\n",
                    "reasoning_content": " think 2",
                    "tool_calls": [
                        { "index": 0, "id": "tc_read_changed", "type": "function", "function": { "arguments": ".md\"}" } },
                        { "index": 1, "type": "function", "function": { "arguments": "\"}" } },
                    ],
                },
                "finish_reason": "tool_calls",
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 8,
                "prompt_tokens_details": { "cached_tokens": 0 },
                "completion_tokens_details": { "reasoning_tokens": 2 },
            },
        })),
    ];

    let (events, resp) = run_sse_chunks(
        &gpt,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Go".to_owned()),
                timestamp: 1,
            })],
            tools: Some(tools),
        },
        chunks,
    )
    .await;

    assert_eq!(resp.stop_reason, StopReason::ToolUse);
    assert_eq!(resp.content.len(), 6);
    assert_eq!(
        resp.content[0],
        AssistantContentBlock::Text(TextContent::new("answer 1 answer 2\n"))
    );
    assert_eq!(
        resp.content[1],
        AssistantContentBlock::Thinking(ThinkingContent {
            thinking: "think 1 think 2".to_owned(),
            thinking_signature: Some("reasoning_content".to_owned()),
            redacted: None,
        })
    );
    let mut by_idx: IndexMap<usize, Vec<&'static str>> = IndexMap::new();
    for ev in &events {
        match ev {
            AssistantMessageEvent::ToolCallStart { content_index, .. } => {
                by_idx.entry(*content_index).or_default().push("toolcall_start");
            }
            AssistantMessageEvent::ToolCallDelta { content_index, .. } => {
                by_idx.entry(*content_index).or_default().push("toolcall_delta");
            }
            AssistantMessageEvent::ToolCallEnd { content_index, .. } => {
                by_idx.entry(*content_index).or_default().push("toolcall_end");
            }
            _ => {}
        }
    }
    assert_eq!(
        by_idx.get(&2).unwrap(),
        &vec!["toolcall_start", "toolcall_delta", "toolcall_delta", "toolcall_end"]
    );
    assert_eq!(
        by_idx.get(&3).unwrap(),
        &vec![
            "toolcall_start",
            "toolcall_delta",
            "toolcall_delta",
            "toolcall_delta",
            "toolcall_end"
        ]
    );
    assert_eq!(
        by_idx.get(&4).unwrap(),
        &vec!["toolcall_start", "toolcall_delta", "toolcall_delta", "toolcall_end"]
    );
    assert_eq!(
        by_idx.get(&5).unwrap(),
        &vec!["toolcall_start", "toolcall_delta", "toolcall_delta", "toolcall_end"]
    );
}

#[tokio::test]
async fn checks_roles_metadata_usage_chat_template_opencode_deepseek_moonshot_and_ant_ling() {
    // 1. OpenRouter developer vs system roles
    let ds = get_model("openrouter", "deepseek/deepseek-v4-pro").unwrap();
    let (p_ds, _) = capture_simple_payload(
        &ds,
        Context {
            system_prompt: Some("Follow.".to_owned()),
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Hi".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        SimpleStreamOptions::default(),
    )
    .await;
    assert_eq!(p_ds["messages"][0]["role"], json!("system"));

    for m in [
        get_model("openrouter", "openai/gpt-5.2-codex").unwrap(),
        get_model("openrouter", "anthropic/claude-fable-5.1:batch").unwrap(),
    ] {
        let (p, _) = capture_simple_payload(
            &m,
            Context {
                system_prompt: Some("Follow.".to_owned()),
                messages: vec![Message::User(UserMessage {
                    content: UserMessageContent::Text("Hi".to_owned()),
                    timestamp: 1,
                })],
                tools: None,
            },
            SimpleStreamOptions::default(),
        )
        .await;
        assert_eq!(p["messages"][0]["role"], json!("developer"));
    }

    let mut gpt55 = get_model("openai", "gpt-5.5").unwrap();
    gpt55.compat = None;
    gpt55.api = "openai-completions".to_owned();
    let (p55, _) = capture_simple_payload(
        &gpt55,
        Context {
            system_prompt: Some("Follow.".to_owned()),
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Hi".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        SimpleStreamOptions::default(),
    )
    .await;
    assert_eq!(p55["messages"][0]["role"], json!("developer"));

    // 2. Built-in metadata checks
    let kimi_or = get_model("openrouter", "moonshotai/kimi-k2.6").unwrap();
    assert!(matches!(
        &kimi_or.compat,
        Some(ModelCompat::OpenAICompletions(c))
            if c.supports_developer_role == Some(false)
                && c.requires_reasoning_content_on_assistant_messages == Some(true)
    ));

    for prov in [
        "xiaomi",
        "xiaomi-token-plan-cn",
        "xiaomi-token-plan-ams",
        "xiaomi-token-plan-sgp",
    ] {
        let m = get_model(prov, "mimo-v2.5-pro").unwrap();
        assert!(matches!(
            &m.compat,
            Some(ModelCompat::OpenAICompletions(c))
                if c.requires_reasoning_content_on_assistant_messages == Some(true)
                    && c.thinking_format == Some(ThinkingFormat::DeepSeek)
                    && c.max_tokens_field.is_none()
                    && c.supports_developer_role.is_none()
        ));
    }

    for prov in ["qwen-token-plan", "qwen-token-plan-cn", "qwen-token-plan-individual"] {
        let m = get_model(prov, "qwen3.7-max").unwrap();
        assert!(matches!(
            &m.compat,
            Some(ModelCompat::OpenAICompletions(c))
                if c.thinking_format == Some(ThinkingFormat::Qwen)
                    && c.requires_reasoning_content_on_assistant_messages.is_none()
                    && c.supports_developer_role == Some(false)
                    && c.supports_store == Some(false)
        ));
    }

    // 3. OpenCode Go & Moonshot & DeepSeek & Z.AI & Grok Build
    let kimi_go = get_model("opencode-go", "kimi-k3").unwrap();
    let (_, r_go) = run_sse_chunks(
        &Model {
            compat: None,
            api: "openai-completions".to_owned(),
            ..kimi_go.clone()
        },
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Think.".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        vec![Some(json!({
            "id": "chatcmpl-go",
            "choices": [{ "delta": { "reasoning": "think" }, "finish_reason": "stop" }],
        }))],
    )
    .await;
    assert_eq!(
        r_go.content,
        vec![AssistantContentBlock::Thinking(ThinkingContent {
            thinking: "think".to_owned(),
            thinking_signature: Some("reasoning_content".to_owned()),
            redacted: None,
        })]
    );

    // Non-OpenCode Go keeps reasoning field name
    let (_, r_non_go) = run_sse_chunks(
        &openai_gpt4o_mini(),
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Think.".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        vec![Some(json!({
            "id": "chatcmpl-r",
            "choices": [{ "delta": { "reasoning": "think" }, "finish_reason": "stop" }],
        }))],
    )
    .await;
    assert_eq!(
        r_non_go.content,
        vec![AssistantContentBlock::Thinking(ThinkingContent {
            thinking: "think".to_owned(),
            thinking_signature: Some("reasoning".to_owned()),
            redacted: None,
        })]
    );

    // OpenCode Go replays reasoning thinking block as reasoning_content
    let go_plain = Model {
        api: "openai-completions".to_owned(),
        compat: None,
        ..kimi_go.clone()
    };
    let go_compat = ResolvedOpenAICompletionsCompat {
        supports_store: false,
        supports_developer_role: false,
        ..get_compat(&go_plain)
    };
    let mut a1 = IndexMap::new();
    a1.insert("path".to_owned(), json!("README.md"));
    let converted_go = convert_messages(
        &go_plain,
        &normalize_context(&Context {
            system_prompt: None,
            messages: vec![Message::Assistant(AssistantMessage {
                content: vec![
                    AssistantContentBlock::Thinking(ThinkingContent {
                        thinking: "think".to_owned(),
                        thinking_signature: Some("reasoning".to_owned()),
                        redacted: None,
                    }),
                    AssistantContentBlock::ToolCall(ToolCall::new("call_1", "read", a1)),
                ],
                api: "openai-completions".to_owned(),
                provider: "opencode-go".to_owned(),
                model: "kimi-k3".to_owned(),
                response_model: None,
                response_id: None,
                provider_thinking_level: None,
                thinking_level: None,
                diagnostics: None,
                usage: Usage::default(),
                stop_reason: StopReason::Stop,
                deferred: None,
                error_message: None,
                raw_stop_reason: None,
                end_turn: None,
                timestamp: 1,
            })],
            tools: None,
        }),
        &go_compat,
        None,
    );
    assert_eq!(converted_go[0].get("reasoning_content"), Some(&json!("think")));
    assert!(converted_go[0].get("reasoning").is_none());

    // OpenCode Kimi K2.6 off & on + Moonshot K2.7 / K2.6
    let kimi_oc = get_model("opencode", "kimi-k2.6").unwrap();
    let (p_oc_off, _) = capture_simple_payload(
        &kimi_oc,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Hi".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        SimpleStreamOptions::default(),
    )
    .await;
    assert_eq!(p_oc_off.get("thinking"), Some(&json!({ "type": "disabled" })));

    let (p_oc_on, _) = capture_simple_payload(
        &kimi_oc,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Hi".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        SimpleStreamOptions {
            reasoning: Some(ThinkingLevel::High),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(p_oc_on.get("thinking"), Some(&json!({ "type": "enabled" })));

    for prov in ["moonshotai", "moonshotai-cn"] {
        let m27 = get_model(prov, "kimi-k2.7-code").unwrap();
        let (pm, _) = capture_simple_payload(
            &m27,
            Context {
                system_prompt: None,
                messages: vec![Message::User(UserMessage {
                    content: UserMessageContent::Text("Hi".to_owned()),
                    timestamp: 1,
                })],
                tools: None,
            },
            SimpleStreamOptions::default(),
        )
        .await;
        assert!(pm.get("thinking").is_none());
    }
    let m26 = get_model("moonshotai-cn", "kimi-k2.6").unwrap();
    let (pm26, _) = capture_simple_payload(
        &m26,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Hi".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        SimpleStreamOptions::default(),
    )
    .await;
    assert_eq!(pm26.get("thinking"), Some(&json!({ "type": "disabled" })));

    // DeepSeek max_tokens & Grok Build reasoning omission
    for m in [
        get_model("deepseek", "deepseek-flash").unwrap(),
        get_model("deepseek", "deepseek-v4-pro").unwrap(),
        Model {
            base_url: "https://api.deepseek.com".to_owned(),
            provider: "custom-deepseek".to_owned(),
            ..local_model("custom-ds", "Custom DS", None)
        },
        Model {
            base_url: "https://API.DeepSeek.COM".to_owned(),
            provider: "custom-deepseek".to_owned(),
            ..local_model("custom-ds-up", "Custom DS Upper", None)
        },
    ] {
        let (p, _) = capture_simple_payload(
            &m,
            Context {
                system_prompt: None,
                messages: vec![Message::User(UserMessage {
                    content: UserMessageContent::Text("Hi".to_owned()),
                    timestamp: 1,
                })],
                tools: None,
            },
            SimpleStreamOptions {
                base: StreamOptions {
                    max_tokens: Some(123),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;
        assert_eq!(p.get("max_tokens"), Some(&json!(123)));
        assert!(p.get("max_completion_tokens").is_none());
    }

    let grok = get_model("opencode", "grok-build-0.1").unwrap();
    let (pg, _) = capture_simple_payload(
        &grok,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Hi".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        SimpleStreamOptions {
            reasoning: Some(ThinkingLevel::High),
            ..Default::default()
        },
    )
    .await;
    assert!(pg.get("reasoning_effort").is_none());

    // Usage checks: reasoning_tokens not double counted, cache_read / cache_write preserved
    let (_, r_u1) = run_sse_chunks(
        &openai_gpt4o_mini(),
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("R".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        vec![Some(json!({
            "id": "chatcmpl-u1",
            "choices": [{ "delta": {}, "finish_reason": "stop" }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 33,
                "prompt_tokens_details": { "cached_tokens": 0 },
                "completion_tokens_details": { "reasoning_tokens": 21 },
            },
        }))],
    )
    .await;
    assert_eq!(r_u1.usage.input, 10);
    assert_eq!(r_u1.usage.output, 33);
    assert_eq!(r_u1.usage.total_tokens, 43);

    let (_, r_u2) = run_sse_chunks(
        &openai_gpt4o_mini(),
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("C".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        vec![Some(json!({
            "id": "chatcmpl-u2",
            "choices": [{ "delta": { "content": "OK" }, "finish_reason": "stop" }],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 5,
                "prompt_tokens_details": { "cached_tokens": 50, "cache_write_tokens": 30 },
                "completion_tokens_details": { "reasoning_tokens": 0 },
            },
        }))],
    )
    .await;
    assert_eq!(r_u2.usage.input, 20);
    assert_eq!(r_u2.usage.cache_read, 50);
    assert_eq!(r_u2.usage.cache_write, 30);
    assert_eq!(r_u2.usage.total_tokens, 105);

    // Choice usage fallback
    let (_, r_u3) = run_sse_chunks(
        &openai_gpt4o_mini(),
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("C2".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        vec![Some(json!({
            "id": "chatcmpl-u3",
            "choices": [{
                "delta": { "content": "OK" },
                "finish_reason": "stop",
                "usage": {
                    "prompt_tokens": 100,
                    "completion_tokens": 5,
                    "prompt_tokens_details": { "cached_tokens": 50, "cache_write_tokens": 30 },
                    "completion_tokens_details": { "reasoning_tokens": 0 },
                },
            }],
        }))],
    )
    .await;
    assert_eq!(r_u3.usage.input, 20);
    assert_eq!(r_u3.usage.cache_read, 50);
    assert_eq!(r_u3.usage.cache_write, 30);
    assert_eq!(r_u3.usage.total_tokens, 105);

    // OpenRouter reasoning object + chat template kwargs + Ant Ling
    let or_r1 = get_model("openrouter", "deepseek/deepseek-r1").unwrap();
    let (por, _) = capture_simple_payload(
        &or_r1,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Hi".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        SimpleStreamOptions {
            reasoning: Some(ThinkingLevel::High),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(por.get("reasoning"), Some(&json!({ "effort": "high" })));
    assert!(por.get("reasoning_effort").is_none());

    let mut ct1 = IndexMap::new();
    ct1.insert(
        "thinking".to_owned(),
        ChatTemplateKwargValue::Var {
            var: ChatTemplateVarKind::ThinkingEnabled,
            omit_when_off: None,
        },
    );
    let m_ct1 = local_model(
        "deepseek-ai/DeepSeek-V3.1",
        "DS V3.1",
        Some(OpenAICompletionsCompat {
            thinking_format: Some(ThinkingFormat::ChatTemplate),
            supports_reasoning_effort: Some(false),
            chat_template_kwargs: Some(ct1),
            ..Default::default()
        }),
    );
    for (r, exp) in [(Some(ThinkingLevel::High), true), (None, false)] {
        let (p, _) = capture_simple_payload(
            &m_ct1,
            Context {
                system_prompt: None,
                messages: vec![Message::User(UserMessage {
                    content: UserMessageContent::Text("Hi".to_owned()),
                    timestamp: 1,
                })],
                tools: None,
            },
            SimpleStreamOptions {
                reasoning: r,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(p.get("chat_template_kwargs"), Some(&json!({ "thinking": exp })));
    }

    let m_qwen_ct = local_model(
        "Qwen/Qwen3-Coder",
        "Qwen3 Coder",
        Some(OpenAICompletionsCompat {
            thinking_format: Some(ThinkingFormat::QwenChatTemplate),
            supports_reasoning_effort: Some(false),
            ..Default::default()
        }),
    );
    let (pq_ct, _) = capture_simple_payload(
        &m_qwen_ct,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Hi".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        SimpleStreamOptions {
            reasoning: Some(ThinkingLevel::High),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        pq_ct.get("chat_template_kwargs"),
        Some(&json!({ "enable_thinking": true, "preserve_thinking": true }))
    );

    let mut ct_eff = IndexMap::new();
    ct_eff.insert("preserve_thinking".to_owned(), ChatTemplateKwargValue::Bool(true));
    ct_eff.insert(
        "reasoning_effort".to_owned(),
        ChatTemplateKwargValue::Var {
            var: ChatTemplateVarKind::ThinkingEffort,
            omit_when_off: Some(true),
        },
    );
    let mut oss_map = IndexMap::new();
    oss_map.insert(ModelThinkingLevel::XHigh, Some("max".to_owned()));
    let m_eff = Model {
        thinking_level_map: Some(oss_map),
        ..local_model(
            "unsloth/gpt-oss-120b-GGUF",
            "GPT OSS",
            Some(OpenAICompletionsCompat {
                thinking_format: Some(ThinkingFormat::ChatTemplate),
                supports_reasoning_effort: Some(false),
                chat_template_kwargs: Some(ct_eff),
                ..Default::default()
            }),
        )
    };
    let (pe, _) = capture_simple_payload(
        &m_eff,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Hi".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        SimpleStreamOptions {
            reasoning: Some(ThinkingLevel::XHigh),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        pe.get("chat_template_kwargs"),
        Some(&json!({ "preserve_thinking": true, "reasoning_effort": "max" }))
    );

    // Ant Ling
    let ring = get_model("ant-ling", "Ring-2.6-1T").unwrap();
    let (pr, _) = capture_simple_payload(
        &ring,
        Context {
            system_prompt: Some("Follow instructions.".to_owned()),
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Hi".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        SimpleStreamOptions {
            base: StreamOptions {
                max_tokens: Some(123),
                cache_retention: Some(pi_ai::types::CacheRetention::Long),
                session_id: Some("ant-ling-session".to_owned()),
                ..Default::default()
            },
            reasoning: Some(ThinkingLevel::High),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(pr.get("max_tokens"), Some(&json!(123)));
    assert!(pr.get("max_completion_tokens").is_none());
    assert_eq!(pr["messages"][0]["role"], json!("system"));
    assert_eq!(pr.get("reasoning"), Some(&json!({ "effort": "high" })));
    assert!(pr.get("store").is_none());
    assert!(pr.get("prompt_cache_key").is_none());

    let (pr_unmapped, _) = capture_payload(
        &ring,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Hi".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        ApiStreamOptions::default().with_extra("reasoningEffort", json!("medium")),
    )
    .await;
    assert!(pr_unmapped.get("reasoning").is_none());

    let ling = get_model("ant-ling", "Ling-2.6-flash").unwrap();
    let (pl, _) = capture_simple_payload(
        &ling,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Hi".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        SimpleStreamOptions {
            reasoning: Some(ThinkingLevel::High),
            ..Default::default()
        },
    )
    .await;
    assert!(pl.get("reasoning").is_none());
}
