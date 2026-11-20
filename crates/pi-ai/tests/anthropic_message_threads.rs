//! Port of packages/ai/test/anthropic-message-threads.test.ts

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_ai::api::anthropic_messages::{AnthropicOptions, AnthropicToolChoice, stream_with_options};
use pi_ai::compat::normalize_context;
use pi_ai::session_resources::cleanup_session_resources;
use pi_ai::types::{
    AnthropicMessagesCompat, Api, AssistantMessage, CacheRetention, Context, KnownApi, KnownProvider, Message, Model,
    ModelCompat, ModelCost, ModelInput, ProviderHeaders, ProviderId, StopReason, StreamOptions, SystemMessage,
    SystemMessageContent, TextContent, Tool, ToolResultMessage, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_js::abort::AbortController;
use pi_js::fetch::{Headers, Request, Response};
use pi_js::vendor::typebox::t;
use serde_json::{Value, json};
use serial_test::serial;

const THREADS_BETA: &str = "message-threads-2026-08-12";
const GRAMMAR_ERROR: &str = "The compiled grammar is too large, which would cause performance issues. Simplify your tool schemas or reduce the number of strict tools.";

#[derive(Clone, Debug)]
struct CapturedRequest {
    body: Value,
    betas: Vec<String>,
}

#[derive(Default)]
struct ReplySpec {
    id: Option<String>,
    model: Option<String>,
    text: Option<String>,
    thinking: Option<String>,
    tool_calls: Vec<(String, String, Value)>,
    stop_reason: Option<String>,
    end: Option<ReplyEnd>,
}

#[derive(Clone, Copy)]
enum ReplyEnd {
    ErrorEvent,
    NoMessageStop,
}

type Responder = Box<dyn FnOnce(&CapturedRequest) -> Response + Send + Sync>;

fn sse(events: &[Value]) -> Response {
    let body = events
        .iter()
        .map(|ev| {
            let kind = ev["type"].as_str().unwrap_or("");
            format!("event: {kind}\ndata: {}\n\n", pi_js::json::stringify(ev))
        })
        .collect::<String>();
    Response::new(200, Headers::from_pairs([("content-type", "text/event-stream")]), body)
}

fn reply(request: &CapturedRequest, spec: ReplySpec) -> Response {
    let mut events = vec![json!({
        "type": "message_start",
        "message": {
            "id": spec.id.unwrap_or_else(|| "msg_x".to_owned()),
            "model": spec.model.unwrap_or_else(|| request.body["model"].as_str().unwrap_or("claude-test").to_owned()),
            "usage": { "input_tokens": 10, "output_tokens": 0 },
        }
    })];
    let mut index = 0usize;
    if let Some(thinking) = spec.thinking {
        events.push(json!({ "type": "content_block_start", "index": index, "content_block": { "type": "thinking", "thinking": "", "signature": "" } }));
        events.push(json!({ "type": "content_block_delta", "index": index, "delta": { "type": "thinking_delta", "thinking": thinking } }));
        events.push(json!({ "type": "content_block_delta", "index": index, "delta": { "type": "signature_delta", "signature": "sig" } }));
        events.push(json!({ "type": "content_block_stop", "index": index }));
        index += 1;
    }
    events.push(
        json!({ "type": "content_block_start", "index": index, "content_block": { "type": "text", "text": "" } }),
    );
    events.push(json!({ "type": "content_block_delta", "index": index, "delta": { "type": "text_delta", "text": spec.text.unwrap_or_else(|| "answer".to_owned()) } }));
    events.push(json!({ "type": "content_block_stop", "index": index }));
    index += 1;
    for (id, name, input) in &spec.tool_calls {
        events.push(json!({
            "type": "content_block_start",
            "index": index,
            "content_block": { "type": "tool_use", "id": id, "name": name, "input": {} },
        }));
        events.push(json!({
            "type": "content_block_delta",
            "index": index,
            "delta": { "type": "input_json_delta", "partial_json": pi_js::json::stringify(input) },
        }));
        events.push(json!({ "type": "content_block_stop", "index": index }));
        index += 1;
    }
    if matches!(spec.end, Some(ReplyEnd::ErrorEvent)) {
        let mut body = events
            .iter()
            .map(|ev| {
                format!(
                    "event: {}\ndata: {}\n\n",
                    ev["type"].as_str().unwrap_or(""),
                    pi_js::json::stringify(ev)
                )
            })
            .collect::<String>();
        body.push_str(&format!(
            "event: error\ndata: {}\n\n",
            pi_js::json::stringify(
                &json!({ "type": "error", "error": { "type": "overloaded_error", "message": "Overloaded" } })
            )
        ));
        return Response::new(200, Headers::from_pairs([("content-type", "text/event-stream")]), body);
    }
    let stop_reason = spec.stop_reason.unwrap_or_else(|| {
        if !spec.tool_calls.is_empty() {
            "tool_use".to_owned()
        } else {
            "end_turn".to_owned()
        }
    });
    events.push(
        json!({ "type": "message_delta", "delta": { "stop_reason": stop_reason }, "usage": { "output_tokens": 1 } }),
    );
    if !matches!(spec.end, Some(ReplyEnd::NoMessageStop)) {
        events.push(json!({ "type": "message_stop" }));
    }
    sse(&events)
}

fn error_response(status: u16, message: &str, error_code: Option<&str>, headers: &[(&str, &str)]) -> Response {
    let err_type = match status {
        404 => "not_found_error",
        429 => "rate_limit_error",
        _ => "invalid_request_error",
    };
    let mut err_obj = json!({ "type": err_type, "message": message });
    if let Some(code) = error_code {
        err_obj["details"] = json!({ "error_code": code });
    }
    let mut h = Headers::from_pairs([("content-type", "application/json")]);
    for (k, v) in headers {
        h.set(k, v);
    }
    Response::new(
        status,
        h,
        pi_js::json::stringify(&json!({ "type": "error", "error": err_obj })),
    )
}

fn fingerprint(req: &CapturedRequest) -> String {
    pi_js::json::stringify(&json!([
        req.body.get("model").cloned().unwrap_or(Value::Null),
        req.body.get("thinking").cloned().unwrap_or(Value::Null),
        req.body.get("output_config").cloned().unwrap_or(Value::Null),
        req.body.get("tool_choice").cloned().unwrap_or(Value::Null),
        req.body.get("fallbacks").is_some(),
        req.betas,
    ]))
}

static SESSION_COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Harness {
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    queue: Arc<Mutex<VecDeque<Responder>>>,
    options: AnthropicOptions,
}

impl Harness {
    fn new() -> Self {
        let requests = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
        let queue = Arc::new(Mutex::new(VecDeque::<Responder>::new()));
        let fingerprints = Arc::new(Mutex::new(HashMap::<String, String>::new()));
        let session_id = format!("thread-test-{}", SESSION_COUNTER.fetch_add(1, Ordering::SeqCst) + 1);

        let req_c = Arc::clone(&requests);
        let q_c = Arc::clone(&queue);
        let fp_c = Arc::clone(&fingerprints);

        let options = AnthropicOptions {
            base: StreamOptions {
                api_key: Some("test-key".to_owned()),
                session_id: Some(session_id),
                max_retries: Some(0),
                fetch: Some(Arc::new(move |req: Request| {
                    let req_inner = Arc::clone(&req_c);
                    let q_inner = Arc::clone(&q_c);
                    let fp_inner = Arc::clone(&fp_c);
                    Box::pin(async move {
                        let body = req.body_json().unwrap_or(Value::Null);
                        let betas = req
                            .headers
                            .get("anthropic-beta")
                            .map(|s| s.split(',').map(str::to_owned).collect::<Vec<_>>())
                            .unwrap_or_default();
                        let captured = CapturedRequest { body, betas };
                        let req_count = {
                            let mut g = req_inner.lock().unwrap();
                            g.push(captured.clone());
                            g.len()
                        };
                        let next_resp = q_inner.lock().unwrap().pop_front();
                        if let Some(responder) = next_resp {
                            return Ok(responder(&captured));
                        }
                        let id = format!("msg_{req_count}");
                        let previous = captured
                            .body
                            .get("thread")
                            .and_then(|t| t.get("previous_message_id"))
                            .and_then(Value::as_str);
                        let fp = fingerprint(&captured);
                        let mut fp_map = fp_inner.lock().unwrap();
                        let expected = match previous {
                            None => fp.clone(),
                            Some(prev) => fp_map.get(prev).cloned().unwrap_or_default(),
                        };
                        if expected != fp {
                            return Ok(error_response(
                                400,
                                "fixed field changed",
                                Some("thread_fingerprint_mismatch"),
                                &[],
                            ));
                        }
                        if captured.body.get("thread").is_some() {
                            fp_map.insert(id.clone(), expected);
                        }
                        Ok(reply(
                            &captured,
                            ReplySpec {
                                id: Some(id),
                                ..Default::default()
                            },
                        ))
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        };
        Self {
            requests,
            queue,
            options,
        }
    }

    fn req(&self, idx: usize) -> CapturedRequest {
        self.requests.lock().unwrap()[idx].clone()
    }

    fn req_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

fn base_model() -> Model {
    Model {
        r#type: None,
        id: "claude-test".to_owned(),
        name: "Test Claude".to_owned(),
        api: Api::from(KnownApi::AnthropicMessages),
        provider: ProviderId::from(KnownProvider::Anthropic),
        base_url: "https://api.anthropic.com".to_owned(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 200_000,
        max_tokens: 100,
        headers: None,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    }
}

fn tool(name: &str) -> Tool {
    Tool {
        name: name.to_owned(),
        description: format!("Tool {name}"),
        parameters: t::object([("value", t::string())]),
        constrained_sampling: None,
    }
}

fn user(text: &str) -> Message {
    Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_owned()),
        timestamp: 1,
    })
}

fn tool_result(id: &str, name: &str) -> Message {
    Message::ToolResult(ToolResultMessage {
        tool_call_id: id.to_owned(),
        tool_name: name.to_owned(),
        content: vec![UserContentBlock::Text(TextContent::new("result"))],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 1,
        duration_ms: None,
    })
}

#[derive(Default)]
struct TurnInput {
    model: Option<Model>,
    system_prompt: Option<String>,
    tools: Option<Vec<Tool>>,
    options: Option<AnthropicOptions>,
}

fn merge_options(base: &AnthropicOptions, over: Option<AnthropicOptions>) -> AnthropicOptions {
    let Some(over) = over else { return base.clone() };
    AnthropicOptions {
        base: StreamOptions {
            api_key: over.base.api_key.or_else(|| base.base.api_key.clone()),
            fetch: over.base.fetch.or_else(|| base.base.fetch.clone()),
            session_id: over.base.session_id.or_else(|| base.base.session_id.clone()),
            max_retries: over.base.max_retries.or(base.base.max_retries),
            headers: over.base.headers.or_else(|| base.base.headers.clone()),
            cache_retention: over.base.cache_retention.or(base.base.cache_retention),
            signal: over.base.signal.or_else(|| base.base.signal.clone()),
            on_payload: over.base.on_payload.or_else(|| base.base.on_payload.clone()),
            on_provider_stream_event: over
                .base
                .on_provider_stream_event
                .or_else(|| base.base.on_provider_stream_event.clone()),
            max_tokens: over.base.max_tokens.or(base.base.max_tokens),
            disable_continuation: over.base.disable_continuation.or(base.base.disable_continuation),
            ..base.base.clone()
        },
        thinking_enabled: over.thinking_enabled.or(base.thinking_enabled),
        thinking_budget_tokens: over.thinking_budget_tokens.or(base.thinking_budget_tokens),
        effort: over.effort.or(base.effort),
        thinking_display: over.thinking_display.or(base.thinking_display),
        interleaved_thinking: over.interleaved_thinking.or(base.interleaved_thinking),
        tool_choice: over.tool_choice.or_else(|| base.tool_choice.clone()),
        client: over.client.or_else(|| base.client.clone()),
    }
}

async fn turn(h: &Harness, messages: &[Message], input: TurnInput) -> AssistantMessage {
    let ctx = normalize_context(&Context {
        system_prompt: Some(
            input
                .system_prompt
                .unwrap_or_else(|| "You are a test assistant.".to_owned()),
        ),
        messages: messages.to_vec(),
        tools: Some(input.tools.unwrap_or_else(|| vec![tool("lookup")])),
    });
    let model = input.model.unwrap_or_else(base_model);
    let opts = merge_options(&h.options, input.options);
    stream_with_options(&model, &ctx, Some(opts)).result().await
}

async fn say(h: &Harness, messages: &mut Vec<Message>, input: TurnInput) -> AssistantMessage {
    let result = turn(h, messages, input).await;
    messages.push(Message::Assistant(result.clone()));
    result
}

fn thread_type(req: &CapturedRequest) -> Option<&str> {
    req.body
        .get("thread")
        .and_then(|t| t.get("type"))
        .and_then(Value::as_str)
}

#[tokio::test]
#[serial]
async fn creates_and_continues_threads_across_turns_tools_effort_and_tool_changes() {
    cleanup_session_resources(None).unwrap();

    // 1. Creates a thread, then continues it with only the new messages.
    {
        let h = Harness::new();
        let mut messages = vec![user("first")];
        say(&h, &mut messages, TurnInput::default()).await;
        messages.push(user("second"));
        let result = say(&h, &mut messages, TurnInput::default()).await;

        assert_eq!(result.stop_reason, StopReason::Stop);
        let create = h.req(0);
        let next = h.req(1);
        assert_eq!(create.body["thread"], json!({ "type": "create" }));
        assert!(create.body.get("system").is_some());
        assert!(create.body.get("tools").is_some());
        assert_eq!(
            next.body["thread"],
            json!({ "type": "continue", "previous_message_id": "msg_1" })
        );
        assert!(next.body.get("system").is_none());
        assert!(next.body.get("tools").is_none());
        assert_eq!(
            create.body["messages"][0]["content"],
            json!([{ "type": "text", "text": "first", "cache_control": { "type": "ephemeral" } }])
        );
        assert_eq!(
            next.body["messages"],
            json!([{ "role": "user", "content": [{ "type": "text", "text": "second", "cache_control": { "type": "ephemeral" } }] }])
        );
        assert!(create.betas.iter().any(|b| b == THREADS_BETA));
        assert_eq!(next.betas, create.betas);
    }

    // 2. Continues after a reply that ends in tool calls.
    {
        let h = Harness::new();
        h.queue.lock().unwrap().push_back(Box::new(|req| {
            reply(
                req,
                ReplySpec {
                    id: Some("msg_tool".to_owned()),
                    tool_calls: vec![("toolu_1".to_owned(), "lookup".to_owned(), json!({ "value": "x" }))],
                    ..Default::default()
                },
            )
        }));
        let mut messages = vec![user("look it up")];
        let first = say(&h, &mut messages, TurnInput::default()).await;
        assert_eq!(first.stop_reason, StopReason::ToolUse);
        messages.push(tool_result("toolu_1", "lookup"));
        say(&h, &mut messages, TurnInput::default()).await;

        let next = h.req(1);
        assert_eq!(
            next.body["thread"],
            json!({ "type": "continue", "previous_message_id": "msg_tool" })
        );
        assert_eq!(next.body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(next.body["messages"][0]["role"], json!("user"));
        assert_eq!(next.body["messages"][0]["content"][0]["type"], json!("tool_result"));
        assert_eq!(next.body["messages"][0]["content"][0]["tool_use_id"], json!("toolu_1"));
    }

    // 3. Continues when the effort level changes between turns.
    {
        use pi_ai::api::anthropic_messages::AnthropicEffort;
        let model = Model {
            reasoning: true,
            compat: Some(ModelCompat::AnthropicMessages(AnthropicMessagesCompat {
                force_adaptive_thinking: Some(true),
                supports_mid_convo_effort: Some(true),
                ..Default::default()
            })),
            ..base_model()
        };
        let h = Harness::new();
        h.queue.lock().unwrap().push_back(Box::new(|req| {
            reply(
                req,
                ReplySpec {
                    id: Some("msg_a".to_owned()),
                    thinking: Some("thought".to_owned()),
                    ..Default::default()
                },
            )
        }));
        let mut messages = vec![user("first")];
        say(
            &h,
            &mut messages,
            TurnInput {
                model: Some(model.clone()),
                options: Some(AnthropicOptions {
                    thinking_enabled: Some(true),
                    effort: Some(AnthropicEffort::High),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await;
        messages.push(user("second"));
        say(
            &h,
            &mut messages,
            TurnInput {
                model: Some(model),
                options: Some(AnthropicOptions {
                    thinking_enabled: Some(true),
                    effort: Some(AnthropicEffort::Low),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await;

        let req0 = h.req(0);
        let roles_0: Vec<&str> = req0.body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                m.get("output_config")
                    .and_then(|o| o.get("effort"))
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| m["role"].as_str().unwrap())
            })
            .collect();
        assert_eq!(roles_0, vec!["user", "high"]);

        let next = h.req(1);
        assert_eq!(
            next.body["thread"],
            json!({ "type": "continue", "previous_message_id": "msg_a" })
        );
        let roles_1: Vec<&str> = next.body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                m.get("output_config")
                    .and_then(|o| o.get("effort"))
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| m["role"].as_str().unwrap())
            })
            .collect();
        assert_eq!(roles_1, vec!["user", "low"]);
    }

    // 4. Continues when a system message with a native tool change follows the reply.
    {
        let model = Model {
            compat: Some(ModelCompat::AnthropicMessages(AnthropicMessagesCompat {
                supports_mid_convo_system_messages: Some(true),
                supports_mid_convo_tool_changes: Some(true),
                ..Default::default()
            })),
            ..base_model()
        };
        let h = Harness::new();
        let mut messages = vec![user("first")];
        say(
            &h,
            &mut messages,
            TurnInput {
                model: Some(model.clone()),
                ..Default::default()
            },
        )
        .await;
        messages.push(Message::System(SystemMessage {
            content: SystemMessageContent::Text("Update".to_owned()),
            sections: None,
            tools_added: Some(vec![tool("other")]),
            tools_removed: None,
            timestamp: 2,
        }));
        messages.push(user("second"));
        say(
            &h,
            &mut messages,
            TurnInput {
                model: Some(model),
                ..Default::default()
            },
        )
        .await;

        let next = h.req(1);
        assert_eq!(thread_type(&next), Some("continue"));
        let roles: Vec<&str> = next.body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, vec!["user", "system"]);
        assert!(pi_js::json::stringify(&next.body["messages"][1]["content"]).contains("tool_addition"));
    }

    cleanup_session_resources(None).unwrap();
}

#[tokio::test]
#[serial]
async fn creates_new_thread_when_fixed_fields_change_or_no_new_messages() {
    cleanup_session_resources(None).unwrap();

    let cases: Vec<(TurnInput, TurnInput)> = vec![
        (
            TurnInput::default(),
            TurnInput {
                system_prompt: Some("Different prompt.".to_owned()),
                ..Default::default()
            },
        ),
        (
            TurnInput::default(),
            TurnInput {
                tools: Some(vec![tool("lookup"), tool("other")]),
                ..Default::default()
            },
        ),
        (
            TurnInput::default(),
            TurnInput {
                model: Some(Model {
                    id: "claude-other".to_owned(),
                    ..base_model()
                }),
                ..Default::default()
            },
        ),
        (
            TurnInput::default(),
            TurnInput {
                options: Some(AnthropicOptions {
                    tool_choice: Some(AnthropicToolChoice::Any),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
        (
            TurnInput::default(),
            TurnInput {
                options: Some(AnthropicOptions {
                    base: StreamOptions {
                        cache_retention: Some(CacheRetention::Long),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
        (
            TurnInput {
                model: Some(Model {
                    reasoning: true,
                    ..base_model()
                }),
                options: Some(AnthropicOptions {
                    thinking_enabled: Some(true),
                    thinking_budget_tokens: Some(1024),
                    ..Default::default()
                }),
                ..Default::default()
            },
            TurnInput {
                model: Some(Model {
                    reasoning: true,
                    ..base_model()
                }),
                options: Some(AnthropicOptions {
                    thinking_enabled: Some(true),
                    thinking_budget_tokens: Some(2048),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
    ];

    for (before, after) in cases {
        let h = Harness::new();
        let mut messages = vec![user("first")];
        say(&h, &mut messages, before).await;
        messages.push(user("second"));
        say(&h, &mut messages, after).await;
        let second_req = h.req(1);
        assert_eq!(second_req.body["thread"], json!({ "type": "create" }));
        assert_eq!(second_req.body["messages"].as_array().unwrap().len(), 3);
    }

    // Creates a new thread when the request has no messages beyond the stored prefix.
    {
        let h = Harness::new();
        let mut messages = vec![user("first")];
        say(&h, &mut messages, TurnInput::default()).await;
        turn(&h, &messages, TurnInput::default()).await;
        assert_eq!(h.req(1).body["thread"], json!({ "type": "create" }));
    }

    cleanup_session_resources(None).unwrap();
}

#[tokio::test]
#[serial]
async fn thread_error_recovery_fallbacks_and_header_handling() {
    cleanup_session_resources(None).unwrap();

    for (error_code, status) in [
        ("thread_not_found", 404u16),
        ("thread_already_continued", 400),
        ("thread_continue_in_flight", 400),
        ("thread_fingerprint_mismatch", 400),
    ] {
        let h = Harness::new();
        let mut messages = vec![user("first")];
        say(&h, &mut messages, TurnInput::default()).await;
        messages.push(user("second"));
        h.queue.lock().unwrap().push_back(Box::new(move |_| {
            error_response(status, "thread rejected", Some(error_code), &[])
        }));
        let on_payload_calls = Arc::new(AtomicUsize::new(0));
        let opc = Arc::clone(&on_payload_calls);
        let result = say(
            &h,
            &mut messages,
            TurnInput {
                options: Some(AnthropicOptions {
                    base: StreamOptions {
                        on_payload: Some(Arc::new(move |payload, _| {
                            opc.fetch_add(1, Ordering::SeqCst);
                            Box::pin(async move { Ok(Some(payload)) })
                        })),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await;

        assert_eq!(result.stop_reason, StopReason::Stop);
        assert_eq!(on_payload_calls.load(Ordering::SeqCst), 1);
        assert_eq!(h.req_count(), 3);
        let rejected = h.req(1);
        let resend = h.req(2);
        assert_eq!(thread_type(&rejected), Some("continue"));
        assert_eq!(resend.body["thread"], json!({ "type": "create" }));
        assert_eq!(resend.body["messages"].as_array().unwrap().len(), 3);
        assert!(resend.body.get("system").is_some());
        assert_eq!(resend.betas, rejected.betas);
        let diags = result.diagnostics.expect("diagnostics");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].r#type, "anthropic_thread_fallback");
        assert_eq!(diags[0].details.as_ref().unwrap()["reason"], json!(error_code));

        messages.push(user("third"));
        say(&h, &mut messages, TurnInput::default()).await;
        assert_eq!(
            h.req(3).body["thread"],
            json!({ "type": "continue", "previous_message_id": "msg_3" })
        );
    }

    // Turns threads off for the session after unsupported/unknown thread 400 on continue.
    for (error_code, msg) in [
        (Some("thread_unsupported_request"), "rejected"),
        (None, "thread: Extra inputs are not permitted"),
    ] {
        let h = Harness::new();
        let mut messages = vec![user("first")];
        say(&h, &mut messages, TurnInput::default()).await;
        messages.push(user("second"));
        h.queue
            .lock()
            .unwrap()
            .push_back(Box::new(move |_| error_response(400, msg, error_code, &[])));
        let result = say(&h, &mut messages, TurnInput::default()).await;
        assert_eq!(result.stop_reason, StopReason::Stop);
        let diags = result.diagnostics.expect("diagnostics");
        assert_eq!(diags[0].r#type, "anthropic_thread_fallback");
        assert_eq!(
            diags[0].details.as_ref().unwrap()["reason"],
            json!(error_code.unwrap_or("400"))
        );

        messages.push(user("third"));
        say(&h, &mut messages, TurnInput::default()).await;
        let later = h.req(3);
        assert!(later.body.get("thread").is_none());
        assert_eq!(later.betas, h.req(0).betas);
    }

    // Shows the error when the resend fails too and keeps threads on.
    {
        let h = Harness::new();
        let mut messages = vec![user("first")];
        say(&h, &mut messages, TurnInput::default()).await;
        messages.push(user("second"));
        {
            let mut q = h.queue.lock().unwrap();
            q.push_back(Box::new(|_| {
                error_response(400, "thread: Extra inputs are not permitted", None, &[])
            }));
            q.push_back(Box::new(|_| error_response(400, "Real problem", None, &[])));
        }
        let failed = turn(&h, &messages, TurnInput::default()).await;
        assert_eq!(failed.stop_reason, StopReason::Error);
        assert!(failed.error_message.as_deref().unwrap_or("").contains("Real problem"));
        assert_eq!(h.req_count(), 3);

        turn(&h, &messages, TurnInput::default()).await;
        assert_eq!(h.req(3).body["thread"], json!({ "type": "create" }));
    }

    // Passes non-thread 400/404 through without resending.
    for (status, msg) in [
        (400u16, "prompt is too long: 250000 tokens > 200000 maximum"),
        (404, "model: claude-test"),
    ] {
        let h = Harness::new();
        let mut messages = vec![user("first")];
        say(&h, &mut messages, TurnInput::default()).await;
        messages.push(user("second"));
        h.queue
            .lock()
            .unwrap()
            .push_back(Box::new(move |_| error_response(status, msg, None, &[])));
        let failed = turn(&h, &messages, TurnInput::default()).await;
        assert_eq!(failed.stop_reason, StopReason::Error);
        assert!(failed.error_message.as_deref().unwrap_or("").contains(msg));
        assert_eq!(h.req_count(), 2);
        assert!(failed.diagnostics.is_none());

        turn(&h, &messages, TurnInput::default()).await;
        assert_eq!(h.req(2).body["thread"], json!({ "type": "create" }));
    }

    // Leaves strict tool rejections to strict tool fallback.
    {
        use pi_ai::types::{ConstrainedSampling, ConstrainedSamplingConfig, JsonSchemaStrict};
        let model = Model {
            compat: Some(ModelCompat::AnthropicMessages(AnthropicMessagesCompat {
                supports_strict_tools: Some(true),
                ..Default::default()
            })),
            ..base_model()
        };
        let strict_tool = Tool {
            constrained_sampling: Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::JsonSchema {
                strict: JsonSchemaStrict::Prefer,
            })),
            ..tool("lookup")
        };
        let h = Harness::new();
        h.queue
            .lock()
            .unwrap()
            .push_back(Box::new(|_| error_response(400, GRAMMAR_ERROR, None, &[])));
        let result = turn(
            &h,
            &[user("first")],
            TurnInput {
                model: Some(model),
                tools: Some(vec![strict_tool]),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(result.stop_reason, StopReason::Stop);
        assert_eq!(h.req_count(), 2);
        assert_eq!(
            [thread_type(&h.req(0)), thread_type(&h.req(1))],
            [Some("create"), Some("create")]
        );
        assert_eq!(
            result
                .diagnostics
                .unwrap()
                .iter()
                .map(|d| d.r#type.as_str())
                .collect::<Vec<_>>(),
            vec!["anthropic_strict_tools_fallback"]
        );
    }

    // Starts a new thread when a retried continue finds the thread in flight.
    {
        let h = Harness::new();
        let mut messages = vec![user("first")];
        say(&h, &mut messages, TurnInput::default()).await;
        messages.push(user("second"));
        {
            let mut q = h.queue.lock().unwrap();
            q.push_back(Box::new(|_| {
                error_response(503, "overloaded", None, &[("retry-after-ms", "0")])
            }));
            q.push_back(Box::new(|_| {
                error_response(400, "in flight", Some("thread_continue_in_flight"), &[])
            }));
        }
        let result = say(
            &h,
            &mut messages,
            TurnInput {
                options: Some(AnthropicOptions {
                    base: StreamOptions {
                        max_retries: Some(1),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(result.stop_reason, StopReason::Stop);
        assert_eq!(
            [
                thread_type(&h.req(0)),
                thread_type(&h.req(1)),
                thread_type(&h.req(2)),
                thread_type(&h.req(3)),
            ],
            [Some("create"), Some("continue"), Some("continue"), Some("create")]
        );
    }

    // Fails once, then turns threads off, with explicit anthropic-beta header without threads beta or set to null.
    for header in [Some("fine-grained-tool-streaming-2025-05-14".to_owned()), None] {
        let h = Harness::new();
        for n in 1usize..=3 {
            h.queue.lock().unwrap().push_back(Box::new(move |req| {
                if req.body.get("thread").is_some() && !req.betas.iter().any(|b| b == THREADS_BETA) {
                    error_response(400, "thread: Extra inputs are not permitted", None, &[])
                } else {
                    reply(
                        req,
                        ReplySpec {
                            id: Some(format!("msg_{n}")),
                            ..Default::default()
                        },
                    )
                }
            }));
        }
        let mut hdrs = ProviderHeaders::new();
        hdrs.insert("anthropic-beta".to_owned(), header);
        let opts = AnthropicOptions {
            base: StreamOptions {
                headers: Some(hdrs),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut messages = vec![user("first")];
        let result = say(
            &h,
            &mut messages,
            TurnInput {
                options: Some(opts.clone()),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(result.stop_reason, StopReason::Stop);
        messages.push(user("second"));
        say(
            &h,
            &mut messages,
            TurnInput {
                options: Some(opts),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(
            [thread_type(&h.req(0)), thread_type(&h.req(1)), thread_type(&h.req(2)),],
            [Some("create"), None, None]
        );
    }

    cleanup_session_resources(None).unwrap();
}

#[tokio::test]
#[serial]
async fn skips_recording_incomplete_or_fallback_turns_and_handles_oauth_and_cache_warming() {
    cleanup_session_resources(None).unwrap();

    // Does not record incomplete/aborted/refusal/fallback replies.
    enum Case {
        Abort,
        Spec(ReplySpec),
    }
    let cases = [
        Case::Abort,
        Case::Spec(ReplySpec {
            end: Some(ReplyEnd::ErrorEvent),
            ..Default::default()
        }),
        Case::Spec(ReplySpec {
            end: Some(ReplyEnd::NoMessageStop),
            ..Default::default()
        }),
        Case::Spec(ReplySpec {
            stop_reason: Some("refusal".to_owned()),
            ..Default::default()
        }),
        Case::Spec(ReplySpec {
            model: Some("claude-fallback".to_owned()),
            ..Default::default()
        }),
    ];
    for case in cases {
        let h = Harness::new();
        let controller = Arc::new(AbortController::new());
        let ctrl_c = Arc::clone(&controller);
        let turn_opts = match case {
            Case::Abort => AnthropicOptions {
                base: StreamOptions {
                    signal: Some(controller.signal()),
                    on_provider_stream_event: Some(Arc::new(move |ev, _| {
                        let ctrl = ctrl_c.clone();
                        let is_start = ev.get("type").and_then(Value::as_str) == Some("message_start");
                        Box::pin(async move {
                            if is_start {
                                ctrl.abort(None);
                            }
                            Ok(())
                        })
                    })),
                    ..Default::default()
                },
                ..Default::default()
            },
            Case::Spec(spec) => {
                h.queue.lock().unwrap().push_back(Box::new(move |req| {
                    reply(
                        req,
                        ReplySpec {
                            id: Some("msg_bad".to_owned()),
                            ..spec
                        },
                    )
                }));
                AnthropicOptions::default()
            }
        };
        let mut messages = vec![user("first")];
        let first = turn(
            &h,
            &messages,
            TurnInput {
                options: Some(turn_opts),
                ..Default::default()
            },
        )
        .await;
        messages.push(Message::Assistant(first));
        messages.push(user("second"));
        turn(&h, &messages, TurnInput::default()).await;
        assert_eq!(h.req(1).body["thread"], json!({ "type": "create" }));
    }

    // Does not use threads with cacheRetention none.
    {
        let h = Harness::new();
        let mut messages = vec![user("first")];
        say(
            &h,
            &mut messages,
            TurnInput {
                options: Some(AnthropicOptions {
                    base: StreamOptions {
                        cache_retention: Some(CacheRetention::None),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await;
        assert!(h.req(0).body.get("thread").is_none());
        assert!(!h.req(0).betas.iter().any(|b| b == THREADS_BETA));
    }

    // Matches Claude Code tool names across turns with OAuth.
    {
        let h = Harness::new();
        h.queue.lock().unwrap().push_back(Box::new(|req| {
            reply(
                req,
                ReplySpec {
                    id: Some("msg_oauth".to_owned()),
                    tool_calls: vec![("toolu_1".to_owned(), "Read".to_owned(), json!({ "value": "x" }))],
                    ..Default::default()
                },
            )
        }));
        let oauth_opts = AnthropicOptions {
            base: StreamOptions {
                api_key: Some("sk-ant-oat-test".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut messages = vec![user("read it")];
        let first = say(
            &h,
            &mut messages,
            TurnInput {
                tools: Some(vec![tool("read")]),
                options: Some(oauth_opts.clone()),
                ..Default::default()
            },
        )
        .await;
        let first_tool_name = first.content.iter().find_map(|b| match b {
            pi_ai::types::AssistantContentBlock::ToolCall(tc) => Some(tc.name.as_str()),
            _ => None,
        });
        assert_eq!(first_tool_name, Some("read"));
        messages.push(tool_result("toolu_1", "read"));
        say(
            &h,
            &mut messages,
            TurnInput {
                tools: Some(vec![tool("read")]),
                options: Some(oauth_opts),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(
            h.req(1).body["thread"],
            json!({ "type": "continue", "previous_message_id": "msg_oauth" })
        );
    }

    // Sends cache warming replays without a thread and keeps the conversation's thread.
    {
        let h = Harness::new();
        let mut messages = vec![user("first")];
        say(&h, &mut messages, TurnInput::default()).await;
        turn(
            &h,
            &messages[..1],
            TurnInput {
                options: Some(AnthropicOptions {
                    base: StreamOptions {
                        max_tokens: Some(1),
                        disable_continuation: Some(true),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await;
        messages.push(user("second"));
        say(&h, &mut messages, TurnInput::default()).await;

        let real = h.req(0);
        let warm = h.req(1);
        let next = h.req(2);
        assert!(warm.body.get("thread").is_none());
        assert_eq!(warm.body["max_tokens"], json!(1));
        assert_eq!(warm.body["messages"], real.body["messages"]);
        assert_eq!(warm.betas, real.betas);
        assert_eq!(
            next.body["thread"],
            json!({ "type": "continue", "previous_message_id": "msg_1" })
        );
    }

    cleanup_session_resources(None).unwrap();
}
