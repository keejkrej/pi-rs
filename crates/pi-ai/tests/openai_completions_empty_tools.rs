//! Port of packages/ai/test/openai-completions-empty-tools.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::compat::{get_model, stream_simple};
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, Context, Message, Model, ModelCompat, OpenAICompletionsCompat,
    ProviderEnv, SimpleStreamOptions, StopReason, StreamOptions, TextContent, ThinkingLevel, ToolCall,
    ToolResultMessage, Usage, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_js::fetch::{Headers, Request, Response};
use pi_js::time;
use serde_json::{Value, json};

fn ok_sse() -> String {
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({
            "choices": [{ "delta": {}, "finish_reason": "stop" }],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "prompt_tokens_details": { "cached_tokens": 0 },
                "completion_tokens_details": { "reasoning_tokens": 0 },
            }
        })
    )
}

fn openai_completions_gpt4o_mini() -> Model {
    let mut m = get_model("openai", "gpt-4o-mini").unwrap();
    m.compat = None;
    m.api = "openai-completions".to_owned();
    m
}

async fn run_and_capture(model: &Model, context: Context, mut opts: SimpleStreamOptions) -> (Value, String, Headers) {
    let payload = Arc::new(Mutex::new(None::<Value>));
    let p_c = payload.clone();
    let req_url = Arc::new(Mutex::new(String::new()));
    let u_c = req_url.clone();
    let req_headers = Arc::new(Mutex::new(Headers::new()));
    let h_c = req_headers.clone();

    opts.base.on_payload = Some(Arc::new(move |params, _| {
        let p = p_c.clone();
        Box::pin(async move {
            *p.lock().unwrap() = Some(params);
            Ok(None)
        })
    }));
    opts.base.fetch = Some(Arc::new(move |req: Request| {
        let u = u_c.clone();
        let h = h_c.clone();
        Box::pin(async move {
            *u.lock().unwrap() = req.url;
            *h.lock().unwrap() = req.headers;
            Ok(Response::new(
                200,
                Headers::from_pairs([("content-type", "text/event-stream")]),
                ok_sse(),
            ))
        })
    }));

    stream_simple(model, &context, Some(opts)).result().await;

    (
        payload.lock().unwrap().take().unwrap(),
        req_url.lock().unwrap().clone(),
        req_headers.lock().unwrap().clone(),
    )
}

fn cf_env() -> ProviderEnv {
    let mut env = IndexMap::new();
    env.insert("CLOUDFLARE_API_KEY".to_owned(), "cf-token".to_owned());
    env.insert("CLOUDFLARE_ACCOUNT_ID".to_owned(), "account-id".to_owned());
    env.insert("CLOUDFLARE_GATEWAY_ID".to_owned(), "gateway-id".to_owned());
    env
}

#[tokio::test]
async fn omits_tools_field_when_context_tools_is_an_empty_array() {
    let model = openai_completions_gpt4o_mini();
    let (params, _, _) = run_and_capture(
        &model,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("hi".to_owned()),
                timestamp: time::now_ms(),
            })],
            tools: Some(Vec::new()),
        },
        SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("test".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;

    assert!(params.get("tools").is_none());
}

#[tokio::test]
async fn omits_tools_field_when_context_tools_is_undefined() {
    let model = openai_completions_gpt4o_mini();
    let (params, _, _) = run_and_capture(
        &model,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("hi".to_owned()),
                timestamp: time::now_ms(),
            })],
            tools: None,
        },
        SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("test".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;

    assert!(params.get("tools").is_none());
}

#[tokio::test]
async fn sends_default_and_explicit_max_tokens_and_clamps_to_remaining_context() {
    let model = openai_completions_gpt4o_mini();

    // Default maxTokens
    let (p1, _, _) = run_and_capture(
        &model,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("hi".to_owned()),
                timestamp: time::now_ms(),
            })],
            tools: None,
        },
        SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("test".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert!(p1.get("max_tokens").is_none());
    assert_eq!(p1.get("max_completion_tokens"), Some(&json!(model.max_tokens)));

    // Explicit maxTokens
    let (p2, _, _) = run_and_capture(
        &model,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("hi".to_owned()),
                timestamp: time::now_ms(),
            })],
            tools: None,
        },
        SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("test".to_owned()),
                max_tokens: Some(1234),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert!(p2.get("max_tokens").is_none());
    assert_eq!(p2.get("max_completion_tokens"), Some(&json!(1234)));

    // Clamp default maxTokens to remaining context
    let small_model = Model {
        context_window: 10_000,
        max_tokens: 8_000,
        ..model.clone()
    };
    let (p3, _, _) = run_and_capture(
        &small_model,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("x".repeat(8000)),
                timestamp: time::now_ms(),
            })],
            tools: None,
        },
        SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("test".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert!(p3.get("max_tokens").is_none());
    assert_eq!(p3.get("max_completion_tokens"), Some(&json!(3618)));

    // Clamp explicit maxTokens to remaining context
    let (p4, _, _) = run_and_capture(
        &small_model,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("x".repeat(8000)),
                timestamp: time::now_ms(),
            })],
            tools: None,
        },
        SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("test".to_owned()),
                max_tokens: Some(7000),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert!(p4.get("max_tokens").is_none());
    assert_eq!(p4.get("max_completion_tokens"), Some(&json!(3618)));
}

#[tokio::test]
async fn handles_cloudflare_ai_gateway_compat_url_auth_and_session_affinity() {
    let kimi = get_model("cloudflare-ai-gateway", "workers-ai/@cf/moonshotai/kimi-k2.6").unwrap();

    let (params, url, headers) = run_and_capture(
        &kimi,
        Context {
            system_prompt: Some("You are helpful.".to_owned()),
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("hi".to_owned()),
                timestamp: time::now_ms(),
            })],
            tools: None,
        },
        SimpleStreamOptions {
            base: StreamOptions {
                env: Some(cf_env()),
                max_tokens: Some(1234),
                ..Default::default()
            },
            reasoning: Some(ThinkingLevel::High),
            ..Default::default()
        },
    )
    .await;

    let msgs = params.get("messages").and_then(Value::as_array).unwrap();
    assert_eq!(msgs[0].get("role"), Some(&json!("system")));
    assert_eq!(params.get("max_tokens"), Some(&json!(1234)));
    assert!(params.get("max_completion_tokens").is_none());
    assert!(params.get("reasoning_effort").is_none());
    assert!(params.get("store").is_none());
    assert_eq!(
        url,
        "https://gateway.ai.cloudflare.com/v1/account-id/gateway-id/compat/chat/completions"
    );
    assert!(headers.get("authorization").is_none());
    assert_eq!(headers.get("cf-aig-authorization").as_deref(), Some("Bearer cf-token"));

    // Resolve base URL through provider auth without options
    let (_, url2, _) = run_and_capture(
        &kimi,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("hi".to_owned()),
                timestamp: time::now_ms(),
            })],
            tools: None,
        },
        SimpleStreamOptions {
            base: StreamOptions {
                env: Some(cf_env()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        url2,
        "https://gateway.ai.cloudflare.com/v1/account-id/gateway-id/compat/chat/completions"
    );

    // Inline upstream Authorization for BYOK
    let gpt51 = get_model("cloudflare-ai-gateway", "gpt-5.1").unwrap();
    let mut byok_headers = IndexMap::new();
    byok_headers.insert("Authorization".to_owned(), Some("Bearer upstream-token".to_owned()));
    let (_, _, headers3) = run_and_capture(
        &gpt51,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("hi".to_owned()),
                timestamp: time::now_ms(),
            })],
            tools: None,
        },
        SimpleStreamOptions {
            base: StreamOptions {
                env: Some(cf_env()),
                headers: Some(byok_headers),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert_eq!(headers3.get("authorization").as_deref(), Some("Bearer upstream-token"));
    assert_eq!(headers3.get("cf-aig-authorization").as_deref(), Some("Bearer cf-token"));

    // Session affinity headers for Workers AI
    let (_, _, headers4) = run_and_capture(
        &kimi,
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("hi".to_owned()),
                timestamp: time::now_ms(),
            })],
            tools: None,
        },
        SimpleStreamOptions {
            base: StreamOptions {
                env: Some(cf_env()),
                session_id: Some("session-1".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert_eq!(headers4.get("session_id").as_deref(), Some("session-1"));
    assert_eq!(headers4.get("x-client-request-id").as_deref(), Some("session-1"));
    assert_eq!(headers4.get("x-session-affinity").as_deref(), Some("session-1"));
}

#[tokio::test]
async fn omits_or_emits_empty_tools_when_conversation_has_tool_history_per_compat() {
    let base_model = openai_completions_gpt4o_mini();
    let now = time::now_ms();
    let context = Context {
        system_prompt: None,
        messages: vec![
            Message::User(UserMessage {
                content: UserMessageContent::Text("use the tool".to_owned()),
                timestamp: now,
            }),
            Message::Assistant(AssistantMessage {
                content: vec![AssistantContentBlock::ToolCall(ToolCall::new(
                    "t1",
                    "noop",
                    IndexMap::new(),
                ))],
                api: "openai-completions".to_owned(),
                provider: "openai".to_owned(),
                model: "gpt-4o-mini".to_owned(),
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
                timestamp: now,
                duration_ms: None,
            }),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "t1".to_owned(),
                tool_name: "noop".to_owned(),
                content: vec![UserContentBlock::Text(TextContent::new("done"))],
                details: None,
                is_error: false,
                nested_calls: None,
                usage: None,
                timestamp: now,
                duration_ms: None,
            }),
        ],
        tools: Some(Vec::new()),
    };

    // #11843: omits tools by default even with tool history.
    let (params_default, _, _) = run_and_capture(
        &base_model,
        context.clone(),
        SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("test".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert!(params_default.get("tools").is_none());

    // Sends tools: [] when compat.requires_tools_with_tool_history is true.
    let opt_in_model = Model {
        compat: Some(ModelCompat::OpenAICompletions(OpenAICompletionsCompat {
            requires_tools_with_tool_history: Some(true),
            ..Default::default()
        })),
        ..base_model
    };
    let (params_opt_in, _, _) = run_and_capture(
        &opt_in_model,
        context,
        SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("test".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert_eq!(params_opt_in.get("tools"), Some(&json!([])));
}
