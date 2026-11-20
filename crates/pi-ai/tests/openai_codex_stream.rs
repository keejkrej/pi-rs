//! Port of packages/ai/test/openai-codex-stream.test.ts

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use indexmap::IndexMap;
use pi_ai::api::openai_codex_responses::{
    close_openai_codex_websocket_sessions, get_openai_codex_websocket_debug_stats,
    reset_openai_codex_websocket_debug_stats, stream as stream_codex, stream_simple as stream_simple_codex,
};
use pi_ai::compat::normalize_context;
use pi_ai::types::{
    ApiStreamOptions, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, CacheRetention,
    ConstrainedSampling, ConstrainedSamplingConfig, Context, GrammarFormat, GrammarVariants, JsonSchemaStrict, Message,
    Model, ModelCompat, ModelCost, ModelInput, ModelThinkingLevel, OpenAIResponsesCompat, SimpleStreamOptions,
    StopReason, StreamOptions, TextContent, ThinkingLevel, Tool, ToolResultMessage, Transport, UserContentBlock,
    UserMessage, UserMessageContent,
};
use pi_ai::utils::identity::format_user_agent;
use pi_js::abort::AbortController;
use pi_js::b64::btoa;
use pi_js::fetch::{Headers, Request, Response};
use pi_js::vendor::typebox::t;
use serde_json::{Value, json};
use serial_test::serial;
use tokio::net::TcpListener;
use tokio_tungstenite::accept_hdr_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::handshake::server::{
    Request as WsHandshakeRequest, Response as WsHandshakeResponse,
};

struct CleanupGuard;

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        pi_js::time::testing::clear_system_time();
        close_openai_codex_websocket_sessions(None);
        reset_openai_codex_websocket_debug_stats(None);
    }
}

fn reset_guard() -> CleanupGuard {
    close_openai_codex_websocket_sessions(None);
    reset_openai_codex_websocket_debug_stats(None);
    CleanupGuard
}

fn first_text(msg: &AssistantMessage) -> &str {
    msg.content
        .iter()
        .find_map(|b| match b {
            AssistantContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .expect("expected text block")
}

fn mock_token(account_id: &str) -> String {
    let payload_json = json!({
        "https://api.openai.com/auth": { "chatgpt_account_id": account_id }
    })
    .to_string();
    let payload = btoa(&payload_json).unwrap();
    format!("aaa.{payload}.bbb")
}

fn decode_codex_body(req: &Request) -> Value {
    let body = req.body.as_ref().expect("expected request body");
    let raw = body.as_bytes();
    if req.headers.get("content-encoding").as_deref() == Some("zstd") {
        let decoded = zstd::decode_all(raw).expect("valid zstd body");
        serde_json::from_slice(&decoded).expect("valid json body")
    } else {
        serde_json::from_slice(raw).expect("valid json body")
    }
}

fn build_sse_payload(status: &str, include_done: bool, end_turn: Option<bool>) -> String {
    let terminal_type = if status == "incomplete" {
        "response.incomplete"
    } else {
        "response.completed"
    };
    let mut response_obj = json!({
        "status": status,
        "incomplete_details": if status == "incomplete" { json!({ "reason": "max_output_tokens" }) } else { Value::Null },
        "usage": {
            "input_tokens": 5,
            "output_tokens": 3,
            "total_tokens": 8,
            "input_tokens_details": { "cached_tokens": 0 },
        },
    });
    if let Some(et) = end_turn {
        response_obj["end_turn"] = json!(et);
    }

    let mut events = vec![
        format!(
            "data: {}",
            json!({
                "type": "response.output_item.added",
                "item": { "type": "message", "id": "msg_1", "role": "assistant", "status": "in_progress", "content": [] },
            })
        ),
        format!(
            "data: {}",
            json!({ "type": "response.content_part.added", "part": { "type": "output_text", "text": "" } })
        ),
        format!(
            "data: {}",
            json!({ "type": "response.output_text.delta", "delta": "Hello" })
        ),
        format!(
            "data: {}",
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{ "type": "output_text", "text": "Hello" }],
                },
            })
        ),
        format!(
            "data: {}",
            json!({
                "type": terminal_type,
                "response": response_obj,
            })
        ),
    ];
    if include_done {
        events.push("data: [DONE]".to_owned());
    }
    format!("{}\n\n", events.join("\n\n"))
}

fn codex_model(id: &str, base_url: &str) -> Model {
    Model {
        r#type: None,
        id: id.to_owned(),
        name: id.to_owned(),
        api: "openai-codex-responses".to_owned(),
        provider: "openai-codex".to_owned(),
        base_url: base_url.to_owned(),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 400_000,
        max_tokens: 128_000,
        headers: None,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    }
}

fn hello_context() -> pi_ai::types::TranscriptContext {
    normalize_context(&Context {
        system_prompt: Some("You are a helpful assistant.".to_owned()),
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Say hello".to_owned()),
            timestamp: 1,
        })],
        tools: None,
    })
}

fn extract_headers_map(req: &WsHandshakeRequest) -> IndexMap<String, String> {
    let mut map = IndexMap::new();
    for (k, v) in req.headers() {
        if let Ok(s) = v.to_str() {
            map.insert(k.as_str().to_ascii_lowercase(), s.to_owned());
        }
    }
    map
}

#[tokio::test]
#[serial]
async fn streams_sse_responses_and_forwards_raw_provider_events() {
    let _g = reset_guard();
    let token = mock_token("acc_test");
    let sse = build_sse_payload("completed", false, None);
    let model = codex_model("gpt-5.1-codex", "https://chatgpt.com/backend-api");

    let provider_events = Arc::new(Mutex::new(Vec::<(String, Model)>::new()));
    let pe_c = provider_events.clone();
    let token_c = token.clone();

    let s = stream_codex(
        &model,
        &hello_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some(token),
                transport: Some(Transport::Sse),
                on_provider_stream_event: Some(Arc::new(move |ev, m| {
                    let pe = pe_c.clone();
                    let t = ev.get("type").and_then(Value::as_str).unwrap_or("").to_owned();
                    let m_clone = m.clone();
                    Box::pin(async move {
                        pe.lock().unwrap().push((t, m_clone));
                        Ok(())
                    })
                })),
                fetch: Some(Arc::new(move |req: Request| {
                    let b = sse.clone();
                    let tok = token_c.clone();
                    Box::pin(async move {
                        assert_eq!(req.url, "https://chatgpt.com/backend-api/codex/responses");
                        assert_eq!(
                            req.headers.get("authorization").as_deref(),
                            Some(format!("Bearer {tok}").as_str())
                        );
                        assert_eq!(req.headers.get("chatgpt-account-id").as_deref(), Some("acc_test"));
                        assert_eq!(
                            req.headers.get("openai-beta").as_deref(),
                            Some("responses=experimental")
                        );
                        assert_eq!(req.headers.get("originator").as_deref(), Some("pi"));
                        assert_eq!(
                            req.headers.get("user-agent").as_deref(),
                            Some(format_user_agent(None).as_str())
                        );
                        assert_eq!(req.headers.get("accept").as_deref(), Some("text/event-stream"));
                        assert!(!req.headers.has("x-api-key"));
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

    let mut saw_text_delta = false;
    let mut saw_done = false;
    let mut reader = s.clone();
    while let Some(ev) = reader.next().await {
        match ev {
            AssistantMessageEvent::TextDelta { .. } => saw_text_delta = true,
            AssistantMessageEvent::Done { message, .. } => {
                saw_done = true;
                let txt = message.content.iter().find_map(|b| match b {
                    AssistantContentBlock::Text(t) => Some(t.text.as_str()),
                    _ => None,
                });
                assert_eq!(txt, Some("Hello"));
            }
            _ => {}
        }
    }

    assert!(saw_text_delta);
    assert!(saw_done);
    let captured = provider_events.lock().unwrap();
    let types: Vec<&str> = captured.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        types,
        vec![
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_item.done",
            "response.completed",
        ]
    );
    assert!(captured.iter().all(|(_, m)| m == &model));
}

#[tokio::test]
#[serial]
async fn processes_terminal_sse_event_without_trailing_blank_line_and_open_streams() {
    let _g = reset_guard();
    let token = mock_token("acc_test");
    let model = codex_model("gpt-5.1-codex", "https://chatgpt.com/backend-api");

    // 1. No trailing blank line
    let sse_trim = build_sse_payload("completed", false, None).trim_end().to_owned();
    let res1 = stream_codex(
        &model,
        &hello_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some(token.clone()),
                transport: Some(Transport::Sse),
                fetch: Some(Arc::new(move |_| {
                    let s = sse_trim.clone();
                    Box::pin(async move {
                        Ok(Response::new(
                            200,
                            Headers::from_pairs([("content-type", "text/event-stream")]),
                            s,
                        ))
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;
    assert_eq!(res1.stop_reason, StopReason::Stop);
    assert_eq!(first_text(&res1), "Hello");

    // 2. Completes after response.completed even when SSE body stays open
    let sse_open_ok = build_sse_payload("completed", true, Some(false));
    let res2 = tokio::time::timeout(
        Duration::from_secs(1),
        stream_codex(
            &model,
            &hello_context(),
            Some(ApiStreamOptions {
                base: StreamOptions {
                    api_key: Some(token.clone()),
                    transport: Some(Transport::Sse),
                    fetch: Some(Arc::new(move |_| {
                        let chunk: bytes::Bytes = sse_open_ok.clone().into();
                        let s = futures::stream::once(async move { Ok(chunk) }).chain(futures::stream::pending());
                        Box::pin(async move {
                            Ok(Response::from_stream(
                                200,
                                Headers::from_pairs([("content-type", "text/event-stream")]),
                                s,
                            ))
                        })
                    })),
                    ..Default::default()
                },
                ..Default::default()
            }),
        )
        .result(),
    )
    .await
    .expect("completed stream should not wait for EOF");
    assert_eq!(res2.stop_reason, StopReason::Stop);
    assert_eq!(res2.end_turn, Some(false));
    assert_eq!(first_text(&res2), "Hello");

    // 3. Maps response.incomplete to stopReason length even when SSE body stays open
    let sse_open_inc = build_sse_payload("incomplete", false, None);
    let res3 = tokio::time::timeout(
        Duration::from_secs(1),
        stream_codex(
            &model,
            &hello_context(),
            Some(ApiStreamOptions {
                base: StreamOptions {
                    api_key: Some(token),
                    transport: Some(Transport::Sse),
                    fetch: Some(Arc::new(move |_| {
                        let chunk: bytes::Bytes = sse_open_inc.clone().into();
                        let s = futures::stream::once(async move { Ok(chunk) }).chain(futures::stream::pending());
                        Box::pin(async move {
                            Ok(Response::from_stream(
                                200,
                                Headers::from_pairs([("content-type", "text/event-stream")]),
                                s,
                            ))
                        })
                    })),
                    ..Default::default()
                },
                ..Default::default()
            }),
        )
        .result(),
    )
    .await
    .expect("incomplete stream should not wait for EOF");
    assert_eq!(res3.stop_reason, StopReason::Length);
    assert_eq!(first_text(&res3), "Hello");
}

#[tokio::test]
#[serial]
async fn aborts_sse_fetch_on_header_timeout_and_aborts_sse_body_reads() {
    let _g = reset_guard();
    let token = mock_token("acc_test");
    let model = codex_model("gpt-5.1-codex", "https://chatgpt.com/backend-api");

    // 1. Header timeout
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_c = calls.clone();
    let res_timeout = stream_codex(
        &model,
        &hello_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some(token.clone()),
                transport: Some(Transport::Sse),
                timeout_ms: Some(10),
                fetch: Some(Arc::new(move |req: Request| {
                    calls_c.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async move {
                        let sig = req.signal.expect("expected request abort signal");
                        sig.cancelled().await;
                        Err(pi_js::Error::msg("SSE fetch aborted"))
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(res_timeout.stop_reason, StopReason::Error);
    assert_eq!(
        res_timeout.error_message.as_deref(),
        Some("Codex SSE response headers timed out after 10ms")
    );

    // 2. Abort SSE body reads after response headers arrive
    struct DropFlagStream<S> {
        inner: S,
        cancelled: Arc<AtomicBool>,
    }
    impl<S> Drop for DropFlagStream<S> {
        fn drop(&mut self) {
            self.cancelled.store(true, Ordering::SeqCst);
        }
    }
    impl<S: futures::Stream + Unpin> futures::Stream for DropFlagStream<S> {
        type Item = S::Item;
        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::pin::Pin::new(&mut self.inner).poll_next(cx)
        }
    }

    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_c = cancelled.clone();
    let controller = Arc::new(AbortController::new());

    let chunk1 = format!(
        "{}\n\n",
        [
            format!(
                "data: {}",
                json!({
                    "type": "response.output_item.added",
                    "item": { "type": "message", "id": "msg_1", "role": "assistant", "status": "in_progress", "content": [] },
                })
            ),
            format!(
                "data: {}",
                json!({ "type": "response.content_part.added", "part": { "type": "output_text", "text": "" } })
            ),
            format!(
                "data: {}",
                json!({ "type": "response.output_text.delta", "delta": "one" })
            ),
        ]
        .join("\n\n")
    );
    let chunk2 = format!(
        "data: {}\n\n",
        json!({ "type": "response.output_text.delta", "delta": "two" })
    );

    let s_aborted = stream_codex(
        &model,
        &hello_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some(token),
                transport: Some(Transport::Sse),
                signal: Some(controller.signal()),
                fetch: Some(Arc::new(move |_| {
                    let canc = cancelled_c.clone();
                    let b1: bytes::Bytes = chunk1.clone().into();
                    let b2: bytes::Bytes = chunk2.clone().into();
                    Box::pin(async move {
                        let s = futures::stream::once(async move { Ok(b1) }).chain(futures::stream::once(async move {
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            Ok(b2)
                        }));
                        Ok(Response::from_stream(
                            200,
                            Headers::from_pairs([("content-type", "text/event-stream")]),
                            DropFlagStream {
                                inner: Box::pin(s),
                                cancelled: canc,
                            },
                        ))
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    );

    let mut events = Vec::new();
    let mut reader = s_aborted.clone();
    while let Some(ev) = reader.next().await {
        match &ev {
            AssistantMessageEvent::TextDelta { delta, .. } => {
                events.push(format!("text_delta:{delta}"));
                if delta == "one" {
                    controller.abort(None);
                }
            }
            other => events.push(format!("{other:?}")),
        }
    }
    let res_abort = s_aborted.result().await;
    assert_eq!(res_abort.stop_reason, StopReason::Aborted);
    assert_eq!(res_abort.error_message.as_deref(), Some("Request was aborted"));
    assert!(events.contains(&"text_delta:one".to_owned()));
    assert!(!events.contains(&"text_delta:two".to_owned()));
    assert!(cancelled.load(Ordering::SeqCst));
}

#[tokio::test]
#[serial]
async fn handles_session_id_headers_prompt_cache_key_clamping_and_zstd() {
    let _g = reset_guard();
    let token = mock_token("acc_test");
    let model = codex_model("gpt-5.1-codex", "https://chatgpt.com/backend-api");
    let sse = build_sse_payload("completed", false, None);

    // 1. sessionId provided
    let sse1 = sse.clone();
    stream_codex(
        &model,
        &hello_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some(token.clone()),
                session_id: Some("test-session-123".to_owned()),
                transport: Some(Transport::Sse),
                fetch: Some(Arc::new(move |req: Request| {
                    let s = sse1.clone();
                    Box::pin(async move {
                        assert_eq!(req.headers.get("session-id").as_deref(), Some("test-session-123"));
                        assert!(!req.headers.has("session_id"));
                        assert_eq!(
                            req.headers.get("x-client-request-id").as_deref(),
                            Some("test-session-123")
                        );
                        let body = decode_codex_body(&req);
                        assert_eq!(body.get("prompt_cache_key"), Some(&json!("test-session-123")));
                        Ok(Response::new(
                            200,
                            Headers::from_pairs([("content-type", "text/event-stream")]),
                            s,
                        ))
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    // 2. cacheRetention: None omits affinity & prompt_cache_key
    let sse2 = sse.clone();
    stream_codex(
        &model,
        &hello_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some(token.clone()),
                cache_retention: Some(CacheRetention::None),
                session_id: Some("one-off-summary".to_owned()),
                transport: Some(Transport::Sse),
                fetch: Some(Arc::new(move |req: Request| {
                    let s = sse2.clone();
                    Box::pin(async move {
                        assert!(!req.headers.has("session-id"));
                        assert!(!req.headers.has("x-client-request-id"));
                        let body = decode_codex_body(&req);
                        assert!(body.get("prompt_cache_key").is_none());
                        Ok(Response::new(
                            200,
                            Headers::from_pairs([("content-type", "text/event-stream")]),
                            s,
                        ))
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    // 3. Clamps prompt_cache_key and Codex session-id header to 64 chars + zstd compression
    let long_sid = "x".repeat(67);
    let large_text = "compress me ".repeat(400);
    let sse3 = sse.clone();
    let lt_c = large_text.clone();
    stream_codex(
        &model,
        &normalize_context(&Context {
            system_prompt: Some("You are a helpful assistant.".to_owned()),
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text(large_text),
                timestamp: 1,
            })],
            tools: None,
        }),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some(token.clone()),
                session_id: Some(long_sid),
                transport: Some(Transport::Sse),
                fetch: Some(Arc::new(move |req: Request| {
                    let s = sse3.clone();
                    let expected_text = lt_c.clone();
                    Box::pin(async move {
                        assert_eq!(req.headers.get("content-encoding").as_deref(), Some("zstd"));
                        assert_eq!(req.headers.get("session-id").as_deref(), Some("x".repeat(64).as_str()));
                        assert_eq!(
                            req.headers.get("x-client-request-id").as_deref(),
                            Some("x".repeat(64).as_str())
                        );
                        let body = decode_codex_body(&req);
                        assert_eq!(body.get("prompt_cache_key"), Some(&json!("x".repeat(64))));
                        assert_eq!(
                            body["input"][0]["content"][0]["text"].as_str(),
                            Some(expected_text.as_str())
                        );
                        Ok(Response::new(
                            200,
                            Headers::from_pairs([("content-type", "text/event-stream")]),
                            s,
                        ))
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    // 4. Omits headers when no session_id provided
    let sse4 = sse;
    stream_codex(
        &model,
        &hello_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some(token),
                transport: Some(Transport::Sse),
                fetch: Some(Arc::new(move |req: Request| {
                    let s = sse4.clone();
                    Box::pin(async move {
                        assert!(!req.headers.has("session-id"));
                        assert!(!req.headers.has("session_id"));
                        assert!(!req.headers.has("x-client-request-id"));
                        Ok(Response::new(
                            200,
                            Headers::from_pairs([("content-type", "text/event-stream")]),
                            s,
                        ))
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;
}

#[tokio::test]
#[serial]
async fn preserves_xhigh_required_tool_choice_strict_null_minimal_clamp_and_service_tier() {
    let _g = reset_guard();
    let token = mock_token("acc_test");
    let sse = build_sse_payload("completed", false, None);

    // 1. gpt-5.5 xhigh via stream_simple
    let mut xhigh_map = IndexMap::new();
    xhigh_map.insert(ModelThinkingLevel::XHigh, Some("xhigh".to_owned()));
    let mut gpt55 = codex_model("gpt-5.5", "https://chatgpt.com/backend-api");
    gpt55.thinking_level_map = Some(xhigh_map);
    let sse1 = sse.clone();
    let cap1 = Arc::new(Mutex::new(None::<Value>));
    let cap1_c = cap1.clone();
    stream_simple_codex(
        &gpt55,
        &hello_context(),
        Some(SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some(token.clone()),
                transport: Some(Transport::Sse),
                on_payload: Some(Arc::new(move |p, _| {
                    let c = cap1_c.clone();
                    Box::pin(async move {
                        *c.lock().unwrap() = Some(p);
                        Ok(None)
                    })
                })),
                fetch: Some(Arc::new(move |_| {
                    let s = sse1.clone();
                    Box::pin(async move {
                        Ok(Response::new(
                            200,
                            Headers::from_pairs([("content-type", "text/event-stream")]),
                            s,
                        ))
                    })
                })),
                ..Default::default()
            },
            reasoning: Some(ThinkingLevel::XHigh),
            ..Default::default()
        }),
    )
    .result()
    .await;
    assert_eq!(
        cap1.lock().unwrap().as_ref().unwrap().get("reasoning"),
        Some(&json!({ "effort": "xhigh", "summary": "auto" }))
    );

    // 2. toolChoice: "required" and strict null/true
    let cap2 = Arc::new(Mutex::new(None::<Value>));
    let cap2_c = cap2.clone();
    let sse2 = sse.clone();
    stream_codex(
        &gpt55,
        &normalize_context(&Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Use a tool".to_owned()),
                timestamp: 1,
            })],
            tools: Some(vec![
                Tool {
                    name: "optional".to_owned(),
                    description: "Optional constrained sampling".to_owned(),
                    parameters: t::object([("value", t::string())]),
                    constrained_sampling: Some(ConstrainedSampling::Disabled),
                },
                Tool {
                    name: "strict".to_owned(),
                    description: "Strict constrained sampling".to_owned(),
                    parameters: t::object([("value", t::string())]),
                    constrained_sampling: Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::JsonSchema {
                        strict: JsonSchemaStrict::Prefer,
                    })),
                },
            ]),
        }),
        Some(
            ApiStreamOptions {
                base: StreamOptions {
                    api_key: Some(token.clone()),
                    transport: Some(Transport::Sse),
                    on_payload: Some(Arc::new(move |p, _| {
                        let c = cap2_c.clone();
                        Box::pin(async move {
                            *c.lock().unwrap() = Some(p);
                            Ok(None)
                        })
                    })),
                    fetch: Some(Arc::new(move |_| {
                        let s = sse2.clone();
                        Box::pin(async move {
                            Ok(Response::new(
                                200,
                                Headers::from_pairs([("content-type", "text/event-stream")]),
                                s,
                            ))
                        })
                    })),
                    ..Default::default()
                },
                ..Default::default()
            }
            .with_extra("toolChoice", json!("required")),
        ),
    )
    .result()
    .await;
    let p2 = cap2.lock().unwrap().take().unwrap();
    assert_eq!(p2.get("tool_choice"), Some(&json!("required")));
    let tools = p2.get("tools").and_then(Value::as_array).unwrap();
    assert_eq!(tools[0]["type"], json!("function"));
    assert_eq!(tools[0]["name"], json!("optional"));
    assert_eq!(tools[0]["strict"], Value::Null);
    assert_eq!(tools[1]["type"], json!("function"));
    assert_eq!(tools[1]["name"], json!("strict"));
    assert_eq!(tools[1]["strict"], json!(true));

    // 3. Minimal -> low clamping
    for mid in ["gpt-5.3-codex", "gpt-5.4", "gpt-5.5"] {
        let mut min_map = IndexMap::new();
        min_map.insert(ModelThinkingLevel::Minimal, Some("low".to_owned()));
        let mut m = codex_model(mid, "https://chatgpt.com/backend-api");
        m.thinking_level_map = Some(min_map);
        let cap3 = Arc::new(Mutex::new(None::<Value>));
        let cap3_c = cap3.clone();
        let sse3 = sse.clone();
        stream_codex(
            &m,
            &hello_context(),
            Some(
                ApiStreamOptions {
                    base: StreamOptions {
                        api_key: Some(token.clone()),
                        transport: Some(Transport::Sse),
                        on_payload: Some(Arc::new(move |p, _| {
                            let c = cap3_c.clone();
                            Box::pin(async move {
                                *c.lock().unwrap() = Some(p);
                                Ok(None)
                            })
                        })),
                        fetch: Some(Arc::new(move |_| {
                            let s = sse3.clone();
                            Box::pin(async move {
                                Ok(Response::new(
                                    200,
                                    Headers::from_pairs([("content-type", "text/event-stream")]),
                                    s,
                                ))
                            })
                        })),
                        ..Default::default()
                    },
                    ..Default::default()
                }
                .with_extra("reasoningEffort", json!("minimal")),
            ),
        )
        .result()
        .await;
        assert_eq!(
            cap3.lock().unwrap().as_ref().unwrap().get("reasoning"),
            Some(&json!({ "effort": "low", "summary": "auto" }))
        );
    }

    // 4. Service tier pricing when Codex echoes "default"
    for (mid, tier, mult) in [
        ("gpt-5.1-codex", "flex", 0.5_f64),
        ("gpt-5.1-codex", "priority", 2.0_f64),
        ("gpt-5.5", "flex", 0.5_f64),
        ("gpt-5.5", "priority", 2.5_f64),
    ] {
        let mut m = codex_model(mid, "https://chatgpt.com/backend-api");
        m.cost = ModelCost {
            input: 1.0,
            output: 2.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        };
        let tier_sse = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({
                "type": "response.completed",
                "response": {
                    "status": "completed",
                    "service_tier": "default",
                    "usage": {
                        "input_tokens": 1_000_000,
                        "output_tokens": 1_000_000,
                        "total_tokens": 2_000_000,
                        "input_tokens_details": { "cached_tokens": 0 },
                    },
                },
            })
        );
        let res = stream_codex(
            &m,
            &hello_context(),
            Some(
                ApiStreamOptions {
                    base: StreamOptions {
                        api_key: Some(token.clone()),
                        transport: Some(Transport::Sse),
                        fetch: Some(Arc::new(move |_| {
                            let s = tier_sse.clone();
                            Box::pin(async move {
                                Ok(Response::new(
                                    200,
                                    Headers::from_pairs([("content-type", "text/event-stream")]),
                                    s,
                                ))
                            })
                        })),
                        ..Default::default()
                    },
                    ..Default::default()
                }
                .with_extra("serviceTier", json!(tier)),
            ),
        )
        .result()
        .await;
        assert!((res.usage.cost.input - 1.0 * mult).abs() < 1e-9);
        assert!((res.usage.cost.output - 2.0 * mult).abs() < 1e-9);
        assert!((res.usage.cost.total - 3.0 * mult).abs() < 1e-9);
    }
}

#[tokio::test]
#[serial]
async fn websocket_auto_transport_raw_events_and_account_scoping_and_cache_none() {
    let _g = reset_guard();

    // Part 1: forwards auto transport and raw provider events from streamSimple
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://127.0.0.1:{}", addr.port());

    let captured_headers = Arc::new(Mutex::new(Vec::<IndexMap<String, String>>::new()));
    let sent_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let ch_c = captured_headers.clone();
    let sb_c = sent_bodies.clone();

    let server_task = tokio::spawn(async move {
        // Accept 1 auto-transport connection
        let (stream, _) = listener.accept().await.unwrap();
        let ch_inner = ch_c.clone();
        let mut ws = accept_hdr_async(stream, move |req: &WsHandshakeRequest, resp: WsHandshakeResponse| {
            ch_inner.lock().unwrap().push(extract_headers_map(req));
            Ok(resp)
        })
        .await
        .unwrap();

        if let Some(Ok(WsMessage::Text(txt))) = ws.next().await {
            let parsed: Value = serde_json::from_str(&txt).unwrap();
            sb_c.lock().unwrap().push(parsed);
            let evs = vec![
                json!({
                    "type": "response.output_item.added",
                    "item": { "type": "message", "id": "msg_1", "role": "assistant", "status": "in_progress", "content": [] },
                }),
                json!({ "type": "response.content_part.added", "part": { "type": "output_text", "text": "" } }),
                json!({ "type": "response.output_text.delta", "delta": "Hello" }),
                json!({
                    "type": "response.output_item.done",
                    "item": {
                        "type": "message",
                        "id": "msg_1",
                        "role": "assistant",
                        "status": "completed",
                        "content": [{ "type": "output_text", "text": "Hello" }],
                    },
                }),
                json!({
                    "type": "response.done",
                    "response": {
                        "status": "completed",
                        "end_turn": false,
                        "usage": {
                            "input_tokens": 5,
                            "output_tokens": 3,
                            "total_tokens": 8,
                            "input_tokens_details": { "cached_tokens": 0 },
                        },
                    },
                }),
            ];
            for e in evs {
                ws.send(WsMessage::Text(e.to_string().into())).await.unwrap();
            }
        }
        // Hold ws open slightly for clean reuse / close on cleanup
        tokio::time::sleep(Duration::from_millis(20)).await;
    });

    let token = mock_token("acc_test");
    let model = codex_model("gpt-5.1-codex", &base_url);
    let provider_events = Arc::new(Mutex::new(Vec::<(String, Model)>::new()));
    let pe_c = provider_events.clone();
    let fetch_called = Arc::new(AtomicUsize::new(0));
    let fc_c = fetch_called.clone();

    let result = stream_simple_codex(
        &model,
        &hello_context(),
        Some(SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some(token),
                session_id: Some("session-auto".to_owned()),
                transport: Some(Transport::Auto),
                on_provider_stream_event: Some(Arc::new(move |ev, m| {
                    let pe = pe_c.clone();
                    let t = ev.get("type").and_then(Value::as_str).unwrap_or("").to_owned();
                    let mc = m.clone();
                    Box::pin(async move {
                        pe.lock().unwrap().push((t, mc));
                        Ok(())
                    })
                })),
                fetch: Some(Arc::new(move |_| {
                    fc_c.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async { Ok(Response::null_body(500, Headers::new())) })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    server_task.await.unwrap();
    assert_eq!(result.end_turn, Some(false));
    assert_eq!(sent_bodies.lock().unwrap().len(), 1);
    assert_eq!(fetch_called.load(Ordering::SeqCst), 0);
    let pe_locked = provider_events.lock().unwrap();
    let pe_types: Vec<&str> = pe_locked.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        pe_types,
        vec![
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_item.done",
            "response.done",
        ]
    );
    assert!(pe_locked.iter().all(|(_, m)| m == &model));
    let hdrs = &captured_headers.lock().unwrap()[0];
    assert_eq!(hdrs.get("session-id").map(String::as_str), Some("session-auto"));
    assert!(!hdrs.contains_key("session_id"));
    assert_eq!(
        hdrs.get("x-client-request-id").map(String::as_str),
        Some("session-auto")
    );
    let stats = get_openai_codex_websocket_debug_stats("session-auto").unwrap();
    assert_eq!(stats.cached_context_requests, 1);
    assert_eq!(stats.full_context_requests, 1);

    // Part 2: scopes cached websockets to authenticated account + closes one-shot when cacheRetention is none
    close_openai_codex_websocket_sessions(None);
    reset_openai_codex_websocket_debug_stats(None);

    let listener2 = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr2 = listener2.local_addr().unwrap();
    let model2 = codex_model("gpt-5.1-codex", &format!("http://127.0.0.1:{}", addr2.port()));
    let conn_headers = Arc::new(Mutex::new(Vec::<IndexMap<String, String>>::new()));
    let ch2 = conn_headers.clone();
    let closed_count = Arc::new(AtomicUsize::new(0));
    let cc2 = closed_count.clone();

    let server2 = tokio::spawn(async move {
        let mut resp_num = 0usize;
        // Expect 2 cached account connections + 2 one-shot connections = 4 total
        for _ in 0..4 {
            let (stream, _) = listener2.accept().await.unwrap();
            let ch_i = ch2.clone();
            let cc_i = cc2.clone();
            tokio::spawn(async move {
                let mut ws = accept_hdr_async(stream, move |req: &WsHandshakeRequest, resp: WsHandshakeResponse| {
                    ch_i.lock().unwrap().push(extract_headers_map(req));
                    Ok(resp)
                })
                .await
                .unwrap();
                while let Some(msg_res) = ws.next().await {
                    match msg_res {
                        Ok(WsMessage::Text(_)) => {
                            resp_num += 1;
                            let done = json!({
                                "type": "response.completed",
                                "response": {
                                    "id": format!("resp_{resp_num}"),
                                    "status": "completed",
                                    "usage": { "input_tokens": 1, "output_tokens": 1, "total_tokens": 2 },
                                },
                            });
                            ws.send(WsMessage::Text(done.to_string().into())).await.unwrap();
                        }
                        Ok(WsMessage::Close(_)) | Err(_) => {
                            cc_i.fetch_add(1, Ordering::SeqCst);
                            break;
                        }
                        _ => {}
                    }
                }
            });
        }
    });

    let empty_ctx = normalize_context(&Context {
        system_prompt: Some(String::new()),
        messages: Vec::new(),
        tools: None,
    });
    for acc in ["account-a", "account-b", "account-a"] {
        stream_codex(
            &model2,
            &empty_ctx,
            Some(ApiStreamOptions {
                base: StreamOptions {
                    api_key: Some(mock_token(acc)),
                    session_id: Some("shared-session".to_owned()),
                    transport: Some(Transport::WebsocketCached),
                    ..Default::default()
                },
                ..Default::default()
            }),
        )
        .result()
        .await;
    }

    let scoped_stats = get_openai_codex_websocket_debug_stats("shared-session").unwrap();
    assert_eq!(scoped_stats.connections_created, 2);
    assert_eq!(scoped_stats.connections_reused, 1);
    let hdr_list = conn_headers.lock().unwrap();
    let acct_ids: Vec<&str> = hdr_list.iter().map(|h| h["chatgpt-account-id"].as_str()).collect();
    assert_eq!(acct_ids, vec!["account-a", "account-b"]);
    drop(hdr_list);

    // One-shot when cacheRetention is none
    for _ in 0..2 {
        stream_codex(
            &model2,
            &hello_context(),
            Some(ApiStreamOptions {
                base: StreamOptions {
                    api_key: Some(mock_token("acc_test")),
                    cache_retention: Some(CacheRetention::None),
                    session_id: Some("one-off-summary".to_owned()),
                    transport: Some(Transport::Auto),
                    ..Default::default()
                },
                ..Default::default()
            }),
        )
        .result()
        .await;
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(conn_headers.lock().unwrap().len(), 4);
    assert!(closed_count.load(Ordering::SeqCst) >= 2);
    assert!(get_openai_codex_websocket_debug_stats("one-off-summary").is_none());
    let _ = server2.await;
}

#[tokio::test]
#[serial]
async fn websocket_timeouts_and_age_limit_and_delta_continuation_and_recovery() {
    let _g = reset_guard();
    let token = mock_token("acc_test");

    // 1. Connect timeout fallback to SSE
    let stall_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stall_addr = stall_listener.local_addr().unwrap();
    let _stall_task = tokio::spawn(async move {
        let (_sock, _) = stall_listener.accept().await.unwrap();
        // Keep TCP socket open without sending HTTP/WS response so connect timeout fires
        tokio::time::sleep(Duration::from_secs(2)).await;
    });
    let sse = build_sse_payload("completed", false, None);
    let res_ct = stream_codex(
        &codex_model("gpt-5.1-codex", &format!("http://127.0.0.1:{}", stall_addr.port())),
        &hello_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some(token.clone()),
                session_id: Some("ws-connect-timeout".to_owned()),
                transport: Some(Transport::Auto),
                timeout_ms: Some(300_000),
                websocket_connect_timeout_ms: Some(50),
                fetch: Some(Arc::new(move |_| {
                    let s = sse.clone();
                    Box::pin(async move {
                        Ok(Response::new(
                            200,
                            Headers::from_pairs([("content-type", "text/event-stream")]),
                            s,
                        ))
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;
    assert_eq!(first_text(&res_ct), "Hello");
    let st_ct = get_openai_codex_websocket_debug_stats("ws-connect-timeout").unwrap();
    assert_eq!(st_ct.websocket_failures, 1);
    assert_eq!(st_ct.sse_fallbacks, 1);
    assert_eq!(st_ct.websocket_fallback_active, Some(true));
    assert_eq!(
        st_ct.last_web_socket_error.as_deref(),
        Some("WebSocket connect timeout after 50ms")
    );

    // 2. Reconnect once on websocket_connection_limit_reached + idle before start + idle after start
    close_openai_codex_websocket_sessions(None);
    reset_openai_codex_websocket_debug_stats(None);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://127.0.0.1:{}", addr.port());
    let mode = Arc::new(AtomicUsize::new(0)); // 0=conn_limit, 1=idle_before, 2=idle_after
    let conns = Arc::new(AtomicUsize::new(0));
    let mode_c = mode.clone();
    let conns_c = conns.clone();

    let _ws_srv = tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                break;
            };
            let m = mode_c.load(Ordering::SeqCst);
            let cid = conns_c.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(sock).await else {
                    return;
                };
                while let Some(Ok(msg)) = ws.next().await {
                    if let WsMessage::Text(_) = msg {
                        if m == 0 {
                            if cid == 0 {
                                let err = json!({
                                    "type": "error",
                                    "error": { "code": "websocket_connection_limit_reached" },
                                });
                                let _ = ws.send(WsMessage::Text(err.to_string().into())).await;
                                break;
                            } else {
                                let done = json!({
                                    "type": "response.completed",
                                    "response": {
                                        "id": "resp_1",
                                        "status": "completed",
                                        "usage": { "input_tokens": 5, "output_tokens": 3, "total_tokens": 8 },
                                    },
                                });
                                let _ = ws.send(WsMessage::Text(done.to_string().into())).await;
                            }
                        } else if m == 1 {
                            // Idle before first event
                            tokio::time::sleep(Duration::from_millis(200)).await;
                        } else if m == 2 {
                            // Emit item added, then go idle
                            let ev = json!({
                                "type": "response.output_item.added",
                                "item": { "type": "message", "id": "msg_1", "role": "assistant", "status": "in_progress", "content": [] },
                            });
                            let _ = ws.send(WsMessage::Text(ev.to_string().into())).await;
                            tokio::time::sleep(Duration::from_millis(200)).await;
                        }
                    }
                }
            });
        }
    });

    let empty_ctx = normalize_context(&Context {
        system_prompt: Some(String::new()),
        messages: Vec::new(),
        tools: None,
    });
    let r_lim = stream_codex(
        &codex_model("gpt-5.1-codex", &base_url),
        &empty_ctx,
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some(token.clone()),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;
    assert_eq!(r_lim.stop_reason, StopReason::Stop);
    assert_eq!(conns.load(Ordering::SeqCst), 2);

    // Idle before first event -> falls back to SSE
    mode.store(1, Ordering::SeqCst);
    let sse_idle = build_sse_payload("completed", false, None);
    let r_ib = stream_codex(
        &codex_model("gpt-5.1-codex", &base_url),
        &hello_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some(token.clone()),
                session_id: Some("ws-idle-before-start".to_owned()),
                transport: Some(Transport::Auto),
                timeout_ms: Some(50),
                fetch: Some(Arc::new(move |_| {
                    let s = sse_idle.clone();
                    Box::pin(async move {
                        Ok(Response::new(
                            200,
                            Headers::from_pairs([("content-type", "text/event-stream")]),
                            s,
                        ))
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;
    assert_eq!(first_text(&r_ib), "Hello");
    let st_ib = get_openai_codex_websocket_debug_stats("ws-idle-before-start").unwrap();
    assert_eq!(st_ib.websocket_failures, 1);
    assert_eq!(st_ib.sse_fallbacks, 1);
    assert_eq!(st_ib.websocket_fallback_active, Some(true));

    // Idle after stream started -> errors
    mode.store(2, Ordering::SeqCst);
    let r_ia = stream_codex(
        &codex_model("gpt-5.1-codex", &base_url),
        &hello_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some(token.clone()),
                transport: Some(Transport::Auto),
                timeout_ms: Some(50),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;
    assert_eq!(r_ia.stop_reason, StopReason::Error);
    assert_eq!(r_ia.error_message.as_deref(), Some("WebSocket idle timeout after 50ms"));
}

#[tokio::test]
#[serial]
async fn ages_out_cached_websockets_sends_deltas_and_recovers_missing_continuation() {
    let _g = reset_guard();
    let token = mock_token("acc_test");

    // 1. Age limit test with mocked system time + input delta test
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://127.0.0.1:{}", addr.port());
    let sent_bodies = Arc::new(Mutex::new(Vec::<(usize, Value)>::new()));
    let conn_seq = Arc::new(AtomicUsize::new(0));
    let mode = Arc::new(AtomicUsize::new(0)); // 0=age_limit, 1=delta_custom_tool, 2=recover_ws, 3=recover_sse
    let req_in_mode = Arc::new(AtomicUsize::new(0));

    let sb_c = sent_bodies.clone();
    let cs_c = conn_seq.clone();
    let mode_c = mode.clone();
    let rim_c = req_in_mode.clone();

    let _srv = tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                break;
            };
            let cid = cs_c.fetch_add(1, Ordering::SeqCst) + 1;
            let sb = sb_c.clone();
            let md = mode_c.clone();
            let rim = rim_c.clone();
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(sock).await else {
                    return;
                };
                while let Some(Ok(WsMessage::Text(txt))) = ws.next().await {
                    let parsed: Value = serde_json::from_str(&txt).unwrap();
                    sb.lock().unwrap().push((cid, parsed));
                    let req_idx = rim.fetch_add(1, Ordering::SeqCst) + 1;
                    let cur_mode = md.load(Ordering::SeqCst);

                    if cur_mode == 1 && req_idx == 1 {
                        let evs = vec![
                            json!({ "type": "response.created", "response": { "id": "resp_1" } }),
                            json!({
                                "type": "response.output_item.added",
                                "item": { "type": "custom_tool_call", "id": "ctc_1", "call_id": "call_1", "name": "sample_tool", "input": "" },
                            }),
                            json!({ "type": "response.custom_tool_call_input.delta", "item_id": "ctc_1", "delta": "abc" }),
                            json!({ "type": "response.custom_tool_call_input.done", "item_id": "ctc_1", "input": "abc" }),
                            json!({
                                "type": "response.output_item.done",
                                "item": { "type": "custom_tool_call", "id": "ctc_1", "call_id": "call_1", "name": "sample_tool", "input": "abc" },
                            }),
                            json!({
                                "type": "response.completed",
                                "response": {
                                    "id": "resp_1",
                                    "status": "completed",
                                    "usage": { "input_tokens": 5, "output_tokens": 3, "total_tokens": 8 },
                                },
                            }),
                        ];
                        for e in evs {
                            let _ = ws.send(WsMessage::Text(e.to_string().into())).await;
                        }
                    } else if (cur_mode == 2 || cur_mode == 3) && req_idx == 2 {
                        let evs = vec![
                            json!({
                                "type": "codex.rate_limits",
                                "plan_type": "plus",
                                "rate_limits": { "allowed": true },
                            }),
                            json!({
                                "type": "error",
                                "status": 400,
                                "error": {
                                    "code": "previous_response_not_found",
                                    "message": "Previous response with id 'resp_1' not found.",
                                    "param": "previous_response_id",
                                },
                            }),
                        ];
                        for e in evs {
                            let _ = ws.send(WsMessage::Text(e.to_string().into())).await;
                        }
                        break;
                    } else if cur_mode == 3 && req_idx == 3 {
                        // Drop WS socket to trigger SSE recovery
                        drop(ws);
                        break;
                    } else {
                        let text = if (cur_mode == 2 || cur_mode == 3) && req_idx >= 3 {
                            "Recovered"
                        } else {
                            "Hello"
                        };
                        let resp_id = if req_idx == 1 { "resp_1" } else { "resp_2" };
                        let evs = vec![
                            json!({ "type": "response.created", "response": { "id": resp_id } }),
                            json!({
                                "type": "response.output_item.added",
                                "output_index": 0,
                                "item": { "type": "message", "id": "msg_1", "role": "assistant", "status": "in_progress", "content": [] },
                            }),
                            json!({
                                "type": "response.output_item.done",
                                "output_index": 0,
                                "item": {
                                    "type": "message",
                                    "id": "msg_1",
                                    "role": "assistant",
                                    "status": "completed",
                                    "content": [{ "type": "output_text", "text": text }],
                                },
                            }),
                            json!({
                                "type": "response.completed",
                                "response": {
                                    "id": resp_id,
                                    "status": "completed",
                                    "usage": { "input_tokens": 5, "output_tokens": 3, "total_tokens": 8 },
                                },
                            }),
                        ];
                        for e in evs {
                            let _ = ws.send(WsMessage::Text(e.to_string().into())).await;
                        }
                    }
                }
            });
        }
    });

    // Test 1: Connection age limit
    let t0 = pi_js::time::testing::parse_date_utc("2026-07-03T00:00:00Z").unwrap();
    pi_js::time::testing::set_system_time(t0);
    let m = codex_model("gpt-5.1-codex", &base_url);
    let ctx1 = hello_context();
    let first = stream_codex(
        &m,
        &ctx1,
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some(token.clone()),
                session_id: Some("aged-ws-session".to_owned()),
                transport: Some(Transport::WebsocketCached),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    pi_js::time::testing::set_system_time(t0 + 56 * 60 * 1000);
    let ctx2 = normalize_context(&Context {
        system_prompt: None,
        messages: vec![
            ctx1.messages[1].clone(),
            Message::Assistant(first),
            Message::User(UserMessage {
                content: UserMessageContent::Text("Now finish".to_owned()),
                timestamp: 2,
            }),
        ],
        tools: None,
    });
    stream_codex(
        &m,
        &ctx2,
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some(token.clone()),
                session_id: Some("aged-ws-session".to_owned()),
                transport: Some(Transport::WebsocketCached),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;
    assert_eq!(conn_seq.load(Ordering::SeqCst), 2);
    let age_stats = get_openai_codex_websocket_debug_stats("aged-ws-session").unwrap();
    assert_eq!(age_stats.connections_created, 2);
    assert_eq!(age_stats.connections_reused, 0);

    // Test 2: Delta continuation with grammar tool
    close_openai_codex_websocket_sessions(None);
    reset_openai_codex_websocket_debug_stats(None);
    sent_bodies.lock().unwrap().clear();
    req_in_mode.store(0, Ordering::SeqCst);
    mode.store(1, Ordering::SeqCst);

    let mut grammar_variants = GrammarVariants::new();
    grammar_variants.insert(GrammarFormat::OpenAILark, "start: /[a-z]+/".to_owned());
    let sample_tool = Tool {
        name: "sample_tool".to_owned(),
        description: "Sample tool".to_owned(),
        parameters: t::object([("payload", t::string())]),
        constrained_sampling: Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::Grammar {
            variants: grammar_variants,
        })),
    };
    let mut m_grammar = codex_model("gpt-5.1-codex", &base_url);
    m_grammar.compat = Some(ModelCompat::OpenAIResponses(OpenAIResponsesCompat {
        supports_open_ai_grammar_tools: Some(true),
        ..Default::default()
    }));
    let c_tool_1 = Context {
        system_prompt: Some("You are a helpful assistant.".to_owned()),
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Use the tool".to_owned()),
            timestamp: 1,
        })],
        tools: Some(vec![sample_tool.clone()]),
    };
    let first_tc = stream_codex(
        &m_grammar,
        &normalize_context(&c_tool_1),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some(token.clone()),
                session_id: Some("session-1".to_owned()),
                transport: Some(Transport::WebsocketCached),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;
    let c_tool_2 = normalize_context(&Context {
        system_prompt: c_tool_1.system_prompt,
        messages: vec![
            c_tool_1.messages[0].clone(),
            Message::Assistant(first_tc),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "call_1|ctc_1".to_owned(),
                tool_name: "sample_tool".to_owned(),
                content: vec![UserContentBlock::Text(TextContent::new("real result"))],
                details: None,
                is_error: false,
                nested_calls: None,
                usage: None,
                timestamp: 2,
                duration_ms: None,
            }),
            Message::User(UserMessage {
                content: UserMessageContent::Text("Now finish".to_owned()),
                timestamp: 3,
            }),
        ],
        tools: c_tool_1.tools,
    });
    stream_codex(
        &m_grammar,
        &c_tool_2,
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some(token.clone()),
                session_id: Some("session-1".to_owned()),
                transport: Some(Transport::WebsocketCached),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    let sb_locked = sent_bodies.lock().unwrap();
    assert_eq!(sb_locked.len(), 2);
    assert_eq!(sb_locked[0].1.get("store"), Some(&json!(false)));
    assert!(sb_locked[0].1.get("previous_response_id").is_none());
    assert_eq!(
        sb_locked[0].1.get("input"),
        Some(&json!([{ "role": "user", "content": [{ "type": "input_text", "text": "Use the tool" }] }]))
    );
    assert_eq!(sb_locked[1].1.get("store"), Some(&json!(false)));
    assert_eq!(sb_locked[1].1.get("previous_response_id"), Some(&json!("resp_1")));
    assert_eq!(
        sb_locked[1].1.get("input"),
        Some(&json!([
            { "type": "custom_tool_call_output", "call_id": "call_1", "output": "real result" },
            { "role": "user", "content": [{ "type": "input_text", "text": "Now finish" }] },
        ]))
    );
    drop(sb_locked);
    let delta_stats = get_openai_codex_websocket_debug_stats("session-1").unwrap();
    assert_eq!(delta_stats.requests, 2);
    assert_eq!(delta_stats.connections_created, 1);
    assert_eq!(delta_stats.connections_reused, 1);
    assert_eq!(delta_stats.full_context_requests, 1);
    assert_eq!(delta_stats.delta_requests, 1);
    assert_eq!(delta_stats.last_delta_input_items, Some(2));
    assert_eq!(delta_stats.last_previous_response_id.as_deref(), Some("resp_1"));

    // Test 3: Recovery via WS and SSE
    for (rec_mode, rec_name, expected_txt, expected_fetches) in [
        (2usize, "websocket", "Recovered", 0usize),
        (3usize, "sse", "Hello", 1usize),
    ] {
        close_openai_codex_websocket_sessions(None);
        reset_openai_codex_websocket_debug_stats(None);
        sent_bodies.lock().unwrap().clear();
        conn_seq.store(0, Ordering::SeqCst);
        req_in_mode.store(0, Ordering::SeqCst);
        mode.store(rec_mode, Ordering::SeqCst);

        let sid = format!("missing-continuation-{rec_name}");
        let fetch_count = Arc::new(AtomicUsize::new(0));
        let fc_c = fetch_count.clone();
        let sse_rec = build_sse_payload("completed", false, None);

        let first_r = stream_codex(
            &m,
            &ctx1,
            Some(ApiStreamOptions {
                base: StreamOptions {
                    api_key: Some(token.clone()),
                    session_id: Some(sid.clone()),
                    transport: Some(Transport::WebsocketCached),
                    ..Default::default()
                },
                ..Default::default()
            }),
        )
        .result()
        .await;
        let next_ctx = normalize_context(&Context {
            system_prompt: None,
            messages: vec![
                ctx1.messages[1].clone(),
                Message::Assistant(first_r),
                Message::User(UserMessage {
                    content: UserMessageContent::Text("Now finish".to_owned()),
                    timestamp: 2,
                }),
            ],
            tools: None,
        });
        let s2 = stream_codex(
            &m,
            &next_ctx,
            Some(ApiStreamOptions {
                base: StreamOptions {
                    api_key: Some(token.clone()),
                    session_id: Some(sid.clone()),
                    transport: Some(Transport::WebsocketCached),
                    fetch: Some(Arc::new(move |_| {
                        fc_c.fetch_add(1, Ordering::SeqCst);
                        let s = sse_rec.clone();
                        Box::pin(async move {
                            Ok(Response::new(
                                200,
                                Headers::from_pairs([("content-type", "text/event-stream")]),
                                s,
                            ))
                        })
                    })),
                    ..Default::default()
                },
                ..Default::default()
            }),
        );
        let mut evs2 = Vec::new();
        let mut r2 = s2.clone();
        while let Some(ev) = r2.next().await {
            evs2.push(ev);
        }
        let sec = s2.result().await;
        assert_eq!(sec.stop_reason, StopReason::Stop);
        assert_eq!(first_text(&sec), expected_txt);
        assert_eq!(
            evs2.iter()
                .filter(|e| matches!(e, AssistantMessageEvent::Start { .. }))
                .count(),
            1
        );
        assert!(!evs2.iter().any(|e| matches!(e, AssistantMessageEvent::Error { .. })));
        assert_eq!(fetch_count.load(Ordering::SeqCst), expected_fetches);
        let rec_stats = get_openai_codex_websocket_debug_stats(&sid).unwrap();
        assert_eq!(rec_stats.requests, 3);
        assert_eq!(rec_stats.connections_created, 2);
        assert_eq!(rec_stats.connections_reused, 1);
        assert_eq!(rec_stats.full_context_requests, 2);
        assert_eq!(rec_stats.delta_requests, 1);
        assert_eq!(rec_stats.sse_fallbacks, expected_fetches);
    }
}

#[tokio::test(start_paused = true)]
#[serial]
async fn handles_sse_retry_headers_exponential_backoff_and_delay_limit() {
    let _g = reset_guard();
    let base_ms = pi_js::time::testing::parse_date_utc("2026-05-13T00:00:00Z").unwrap();
    pi_js::time::testing::set_system_time(base_ms);
    let token = mock_token("acc_test");
    let model = codex_model("gpt-5.1-codex", "https://chatgpt.com/backend-api");
    let sse = build_sse_payload("completed", false, None);

    // 1. retry-after-ms / retry-after seconds / retry-after HTTP date
    let cases = [
        ("retry-after-ms", "1500", 1500u64),
        ("retry-after", "60", 60_000u64),
        ("retry-after", "Wed, 13 May 2026 00:00:45 GMT", 45_000u64),
    ];
    for (hdr_name, hdr_val, expected_delay) in cases {
        pi_js::time::testing::set_system_time(base_ms);
        let reqs = Arc::new(AtomicUsize::new(0));
        let r_c = reqs.clone();
        let sse_c = sse.clone();

        let s = stream_codex(
            &model,
            &hello_context(),
            Some(ApiStreamOptions {
                base: StreamOptions {
                    api_key: Some(token.clone()),
                    transport: Some(Transport::Sse),
                    max_retries: Some(1),
                    fetch: Some(Arc::new(move |_| {
                        let n = r_c.fetch_add(1, Ordering::SeqCst) + 1;
                        let s_in = sse_c.clone();
                        Box::pin(async move {
                            if n == 1 {
                                Ok(Response::new(
                                    429,
                                    Headers::from_pairs([("content-type", "application/json"), (hdr_name, hdr_val)]),
                                    json!({ "error": { "code": "rate_limit_exceeded", "message": "rate limited" } })
                                        .to_string(),
                                ))
                            } else {
                                Ok(Response::new(
                                    200,
                                    Headers::from_pairs([("content-type", "text/event-stream")]),
                                    s_in,
                                ))
                            }
                        })
                    })),
                    ..Default::default()
                },
                ..Default::default()
            }),
        );

        let handle = tokio::spawn(async move { s.result().await });
        tokio::task::yield_now().await;
        assert_eq!(reqs.load(Ordering::SeqCst), 1);

        tokio::time::advance(Duration::from_millis(expected_delay - 1)).await;
        tokio::task::yield_now().await;
        assert_eq!(reqs.load(Ordering::SeqCst), 1);

        tokio::time::advance(Duration::from_millis(1)).await;
        let res = handle.await.unwrap();
        assert_eq!(reqs.load(Ordering::SeqCst), 2);
        assert_eq!(first_text(&res), "Hello");
    }

    // 2. Fails immediately when 429 or 503 retry delay exceeds limit
    for status in [429u16, 503u16] {
        let reqs = Arc::new(AtomicUsize::new(0));
        let r_c = reqs.clone();
        let res = stream_codex(
            &model,
            &hello_context(),
            Some(ApiStreamOptions {
                base: StreamOptions {
                    api_key: Some(token.clone()),
                    transport: Some(Transport::Sse),
                    max_retries: Some(3),
                    max_retry_delay_ms: Some(1000),
                    fetch: Some(Arc::new(move |_| {
                        r_c.fetch_add(1, Ordering::SeqCst);
                        Box::pin(async move {
                            Ok(Response::new(
                                status,
                                Headers::from_pairs([("content-type", "application/json"), ("retry-after", "2")]),
                                json!({ "error": { "code": "temporarily_unavailable", "message": "retry later" } })
                                    .to_string(),
                            ))
                        })
                    })),
                    ..Default::default()
                },
                ..Default::default()
            }),
        )
        .result()
        .await;
        assert_eq!(res.stop_reason, StopReason::Error);
        assert_eq!(
            res.error_message.as_deref(),
            Some("Server requested 2s retry delay (max: 1s)")
        );
        assert_eq!(reqs.load(Ordering::SeqCst), 1);
    }

    // 3. Exponential backoff across repeated SSE retries (1000 -> 2000 -> 4000)
    let reqs_exp = Arc::new(AtomicUsize::new(0));
    let r_exp = reqs_exp.clone();
    let sse_exp = sse;
    let s_exp = stream_codex(
        &model,
        &hello_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some(token),
                transport: Some(Transport::Sse),
                max_retries: Some(3),
                fetch: Some(Arc::new(move |_| {
                    let n = r_exp.fetch_add(1, Ordering::SeqCst) + 1;
                    let s_in = sse_exp.clone();
                    Box::pin(async move {
                        if n <= 3 {
                            Ok(Response::new(
                                429,
                                Headers::from_pairs([("content-type", "application/json")]),
                                json!({ "error": { "code": "rate_limit_exceeded", "message": "rate limited" } })
                                    .to_string(),
                            ))
                        } else {
                            Ok(Response::new(
                                200,
                                Headers::from_pairs([("content-type", "text/event-stream")]),
                                s_in,
                            ))
                        }
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    );

    let handle_exp = tokio::spawn(async move { s_exp.result().await });
    tokio::task::yield_now().await;
    assert_eq!(reqs_exp.load(Ordering::SeqCst), 1);

    for delay in [1000u64, 2000, 4000] {
        tokio::time::advance(Duration::from_millis(delay - 1)).await;
        tokio::task::yield_now().await;
        // Still waiting before exact boundary
        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
    }
    let res_exp = handle_exp.await.unwrap();
    assert_eq!(reqs_exp.load(Ordering::SeqCst), 4);
    assert_eq!(first_text(&res_exp), "Hello");
}

#[tokio::test]
#[serial]
async fn lets_model_and_caller_headers_override_originator_and_user_agent() {
    let _g = reset_guard();
    let token = mock_token("acc_test");
    let sse = build_sse_payload("completed", false, None);
    let captured_headers: Arc<Mutex<Option<Headers>>> = Arc::new(Mutex::new(None));
    let cap_clone = Arc::clone(&captured_headers);

    let mut model = codex_model("gpt-5.1-codex", "https://chatgpt.com/backend-api");
    let mut model_hdrs = IndexMap::new();
    model_hdrs.insert("originator".to_string(), "my-app".to_string());
    model.headers = Some(model_hdrs);

    let mut caller_hdrs = IndexMap::new();
    caller_hdrs.insert("user-agent".to_string(), Some("my-app/1.0".to_string()));
    caller_hdrs.insert("Authorization".to_string(), Some("Bearer ignored".to_string()));

    let res = stream_codex(
        &model,
        &hello_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some(token.clone()),
                transport: Some(Transport::Sse),
                headers: Some(caller_hdrs),
                fetch: Some(Arc::new(move |req: Request| {
                    let s = sse.clone();
                    let cap = Arc::clone(&cap_clone);
                    Box::pin(async move {
                        *cap.lock().unwrap() = Some(req.headers.clone());
                        Ok(Response::new(
                            200,
                            Headers::from_pairs([("content-type", "text/event-stream")]),
                            s,
                        ))
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(res.stop_reason, StopReason::Stop);
    let hdrs = captured_headers.lock().unwrap().clone().expect("headers captured");
    assert_eq!(hdrs.get("originator").as_deref(), Some("my-app"));
    assert_eq!(hdrs.get("user-agent").as_deref(), Some("my-app/1.0"));
    assert_eq!(
        hdrs.get("authorization").as_deref(),
        Some(format!("Bearer {token}").as_str())
    );
}

// Regression test for #11708
#[tokio::test]
#[serial]
async fn completes_sse_retry_with_retry_after_zero_and_abort_signal() {
    let _g = reset_guard();
    let token = mock_token("acc_test");
    let model = codex_model("gpt-5.1-codex", "https://chatgpt.com/backend-api");
    let sse = build_sse_payload("completed", false, None);
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_c = Arc::clone(&calls);

    let controller = AbortController::new();
    let res = stream_codex(
        &model,
        &hello_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some(token),
                transport: Some(Transport::Sse),
                max_retries: Some(3),
                signal: Some(controller.signal()),
                fetch: Some(Arc::new(move |_| {
                    let n = calls_c.fetch_add(1, Ordering::SeqCst) + 1;
                    let s = sse.clone();
                    Box::pin(async move {
                        if n <= 3 {
                            Ok(Response::new(
                                429,
                                Headers::from_pairs([("content-type", "application/json"), ("retry-after", "0")]),
                                json!({ "error": { "message": "rate limited" } }).to_string(),
                            ))
                        } else {
                            Ok(Response::new(
                                200,
                                Headers::from_pairs([("content-type", "text/event-stream")]),
                                s,
                            ))
                        }
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(res.stop_reason, StopReason::Stop);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}
