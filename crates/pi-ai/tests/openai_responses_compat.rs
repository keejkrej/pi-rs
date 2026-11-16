//! Port of packages/ai/test/openai-responses-compat.test.ts

use std::sync::{Arc, Mutex};

use pi_ai::api::openai_responses::stream as stream_openai_responses;
use pi_ai::compat::{get_model, normalize_context};
use pi_ai::types::{
    ApiStreamOptions, CacheRetention, ConstrainedSampling, ConstrainedSamplingConfig, Context, JsonSchemaStrict,
    Message, Model, ModelCompat, OpenAIResponsesCompat, ProviderHeaders, SessionAffinityFormat, StopReason,
    StreamOptions, Tool, UserMessage, UserMessageContent,
};
use pi_js::fetch::{Headers, Request, Response};
use pi_js::time;
use pi_js::vendor::typebox::t;
use serde_json::{Value, json};

fn done_sse() -> Response {
    Response::new(
        200,
        Headers::from_pairs([("content-type", "text/event-stream")]),
        "data: [DONE]\n\n",
    )
}

fn sys_hi_context() -> pi_ai::types::TranscriptContext {
    normalize_context(&Context {
        system_prompt: Some("sys".to_owned()),
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hi".to_owned()),
            timestamp: time::now_ms(),
        })],
        tools: None,
    })
}

async fn capture_headers(
    mut opts: ApiStreamOptions,
    model: &Model,
) -> (Option<String>, Option<String>, Option<String>, Option<Value>) {
    let captured_headers = Arc::new(Mutex::new(None::<Headers>));
    let ch_c = captured_headers.clone();
    let captured_payload = Arc::new(Mutex::new(None::<Value>));
    let cp_c = captured_payload.clone();

    opts.base.api_key.get_or_insert_with(|| "sk-test-key".to_owned());
    opts.base.fetch = Some(Arc::new(move |req: Request| {
        let ch = ch_c.clone();
        Box::pin(async move {
            *ch.lock().unwrap() = Some(req.headers);
            Ok(done_sse())
        })
    }));
    let existing_on_payload = opts.base.on_payload.take();
    opts.base.on_payload = Some(Arc::new(move |payload, model| {
        let cp = cp_c.clone();
        *cp.lock().unwrap() = Some(payload.clone());
        let fut = existing_on_payload.as_ref().map(|cb| cb(payload, model));
        Box::pin(async move { if let Some(fut) = fut { fut.await } else { Ok(None) } })
    }));

    stream_openai_responses(model, &sys_hi_context(), Some(opts))
        .result()
        .await;

    let headers = captured_headers.lock().unwrap().take().unwrap();
    let payload = captured_payload.lock().unwrap().take();
    (
        headers.get("session_id"),
        headers.get("x-client-request-id"),
        headers.get("x-session-id"),
        payload,
    )
}

#[tokio::test]
async fn omits_reasoning_when_no_reasoning_is_requested() {
    let model = get_model("github-copilot", "gpt-5-mini").unwrap();
    let captured = Arc::new(Mutex::new(None::<Value>));
    let cap_c = captured.clone();

    stream_openai_responses(
        &model,
        &sys_hi_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("sk-test-key".to_owned()),
                fetch: Some(Arc::new(|_| Box::pin(async { Ok(done_sse()) }))),
                on_payload: Some(Arc::new(move |payload, _| {
                    let cap = cap_c.clone();
                    Box::pin(async move {
                        *cap.lock().unwrap() = Some(payload);
                        Ok(None)
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    let payload = captured.lock().unwrap().take().unwrap();
    assert!(payload.get("reasoning").is_none());
}

#[tokio::test]
async fn forwards_required_tool_choice() {
    let captured = Arc::new(Mutex::new(None::<Value>));
    let cap_c = captured.clone();
    let context = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Do not call ping. Respond with text instead.".to_owned()),
            timestamp: time::now_ms(),
        })],
        tools: Some(vec![Tool {
            name: "ping".to_owned(),
            description: "Ping".to_owned(),
            parameters: t::object([("value", t::string())]),
            constrained_sampling: None,
        }]),
    });

    stream_openai_responses(
        &get_model("openai", "gpt-5.4").unwrap(),
        &context,
        Some(
            ApiStreamOptions {
                base: StreamOptions {
                    api_key: Some("sk-test-key".to_owned()),
                    fetch: Some(Arc::new(|_| Box::pin(async { Ok(done_sse()) }))),
                    on_payload: Some(Arc::new(move |payload, _| {
                        let cap = cap_c.clone();
                        Box::pin(async move {
                            *cap.lock().unwrap() = Some(payload);
                            Ok(None)
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

    let payload = captured.lock().unwrap().take().unwrap();
    assert_eq!(payload.get("tool_choice"), Some(&json!("required")));
    assert_eq!(
        payload.get("tools").and_then(Value::as_array).unwrap()[0].get("name"),
        Some(&json!("ping"))
    );
}

#[tokio::test]
async fn sets_strict_mode_explicitly_for_cloudflare_openai_responses_tools() {
    let model = get_model("cloudflare-ai-gateway", "gpt-5.6-sol").unwrap();
    let captured = Arc::new(Mutex::new(None::<Value>));
    let cap_c = captured.clone();
    let context = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Use a tool.".to_owned()),
            timestamp: time::now_ms(),
        })],
        tools: Some(vec![
            Tool {
                name: "ordinary".to_owned(),
                description: "An ordinary tool".to_owned(),
                parameters: t::object([("path", t::string()), ("offset", t::number().optional())]),
                constrained_sampling: None,
            },
            Tool {
                name: "constrained".to_owned(),
                description: "A constrained tool".to_owned(),
                parameters: t::object([("value", t::string())]),
                constrained_sampling: Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::JsonSchema {
                    strict: JsonSchemaStrict::Prefer,
                })),
            },
        ]),
    });

    stream_openai_responses(
        &model,
        &context,
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("sk-test-key".to_owned()),
                fetch: Some(Arc::new(|_| Box::pin(async { Ok(done_sse()) }))),
                on_payload: Some(Arc::new(move |payload, _| {
                    let cap = cap_c.clone();
                    Box::pin(async move {
                        *cap.lock().unwrap() = Some(payload);
                        Ok(None)
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert!(matches!(
        &model.compat,
        Some(ModelCompat::OpenAIResponses(c)) if c.supports_strict_mode == Some(true)
    ));
    let payload = captured.lock().unwrap().take().unwrap();
    let tools = payload.get("tools").and_then(Value::as_array).unwrap();
    assert_eq!(tools[0].get("name"), Some(&json!("ordinary")));
    assert_eq!(tools[0].get("strict"), Some(&json!(false)));
    assert_eq!(tools[1].get("name"), Some(&json!("constrained")));
    assert_eq!(tools[1].get("strict"), Some(&json!(true)));
}

#[tokio::test]
async fn sends_none_reasoning_effort_for_openai_models_when_no_reasoning_is_requested() {
    for model_id in [
        "gpt-5.1",
        "gpt-5.2",
        "gpt-5.3-codex",
        "gpt-5.4",
        "gpt-5.4-mini",
        "gpt-5.4-nano",
        "gpt-5.5",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
        "gpt-6-sol",
        "gpt-6-luna",
    ] {
        let model = get_model("openai", model_id).unwrap();
        let captured = Arc::new(Mutex::new(None::<Value>));
        let cap_c = captured.clone();

        stream_openai_responses(
            &model,
            &sys_hi_context(),
            Some(ApiStreamOptions {
                base: StreamOptions {
                    api_key: Some("sk-test-key".to_owned()),
                    fetch: Some(Arc::new(|_| Box::pin(async { Ok(done_sse()) }))),
                    on_payload: Some(Arc::new(move |payload, _| {
                        let cap = cap_c.clone();
                        Box::pin(async move {
                            *cap.lock().unwrap() = Some(payload);
                            Ok(None)
                        })
                    })),
                    ..Default::default()
                },
                ..Default::default()
            }),
        )
        .result()
        .await;

        let payload = captured.lock().unwrap().take().unwrap();
        assert_eq!(
            payload.get("reasoning"),
            Some(&json!({ "effort": "none" })),
            "model {model_id}"
        );
    }
}

#[tokio::test]
async fn omits_reasoning_effort_for_openai_models_when_off_is_unsupported() {
    for model_id in [
        "gpt-5",
        "gpt-5-mini",
        "gpt-5-nano",
        "gpt-5-pro",
        "gpt-5.2-pro",
        "gpt-5.4-pro",
        "gpt-5.5-pro",
    ] {
        let model = get_model("openai", model_id).unwrap();
        let captured = Arc::new(Mutex::new(None::<Value>));
        let cap_c = captured.clone();

        stream_openai_responses(
            &model,
            &sys_hi_context(),
            Some(ApiStreamOptions {
                base: StreamOptions {
                    api_key: Some("sk-test-key".to_owned()),
                    fetch: Some(Arc::new(|_| Box::pin(async { Ok(done_sse()) }))),
                    on_payload: Some(Arc::new(move |payload, _| {
                        let cap = cap_c.clone();
                        Box::pin(async move {
                            *cap.lock().unwrap() = Some(payload);
                            Ok(None)
                        })
                    })),
                    ..Default::default()
                },
                ..Default::default()
            }),
        )
        .result()
        .await;

        let payload = captured.lock().unwrap().take().unwrap();
        assert!(payload.get("reasoning").is_none(), "model {model_id}");
    }
}

#[tokio::test]
async fn sets_cache_affinity_headers_for_official_and_proxy_requests_with_session_id() {
    let official_model = get_model("openai", "gpt-5.4").unwrap();
    let (sid, req_id, _, _) = capture_headers(
        ApiStreamOptions {
            base: StreamOptions {
                session_id: Some("session-123".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        },
        &official_model,
    )
    .await;
    assert_eq!(sid.as_deref(), Some("session-123"));
    assert_eq!(req_id.as_deref(), Some("session-123"));

    let proxy_model = Model {
        provider: "opencode".to_owned(),
        base_url: "https://proxy.example.com/v1".to_owned(),
        ..official_model
    };
    let (p_sid, p_req_id, _, _) = capture_headers(
        ApiStreamOptions {
            base: StreamOptions {
                session_id: Some("session-123".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        },
        &proxy_model,
    )
    .await;
    assert_eq!(p_sid.as_deref(), Some("session-123"));
    assert_eq!(p_req_id.as_deref(), Some("session-123"));
}

#[tokio::test]
async fn clamps_prompt_cache_key_to_openais_64_character_limit() {
    let model = get_model("openai", "gpt-5.4").unwrap();
    let (_, _, _, payload) = capture_headers(
        ApiStreamOptions {
            base: StreamOptions {
                session_id: Some("x".repeat(67)),
                ..Default::default()
            },
            ..Default::default()
        },
        &model,
    )
    .await;
    assert_eq!(
        payload.unwrap().get("prompt_cache_key").and_then(Value::as_str),
        Some("x".repeat(64).as_str())
    );
}

#[tokio::test]
async fn uses_openrouter_and_nosession_session_affinity_formats() {
    let base = get_model("openai", "gpt-5.4").unwrap();

    let mut openrouter_proxy = base.clone();
    openrouter_proxy.provider = "proxy".to_owned();
    openrouter_proxy.base_url = "https://proxy.example.com/v1".to_owned();
    openrouter_proxy.compat = Some(ModelCompat::OpenAIResponses(OpenAIResponsesCompat {
        session_affinity_format: Some(SessionAffinityFormat::OpenRouter),
        ..Default::default()
    }));
    let (sid, cid, xsid, payload) = capture_headers(
        ApiStreamOptions {
            base: StreamOptions {
                session_id: Some("session-proxy".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        },
        &openrouter_proxy,
    )
    .await;
    assert!(sid.is_none());
    assert!(cid.is_none());
    assert_eq!(xsid.as_deref(), Some("session-proxy"));
    let p = payload.unwrap();
    assert!(p.get("session_id").is_none());
    assert_eq!(p.get("prompt_cache_key"), Some(&json!("session-proxy")));

    let auto_openrouter = Model {
        provider: "openrouter".to_owned(),
        base_url: "https://openrouter.ai/api/v1".to_owned(),
        ..base.clone()
    };
    let (sid, cid, xsid, payload) = capture_headers(
        ApiStreamOptions {
            base: StreamOptions {
                session_id: Some("session-openrouter".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        },
        &auto_openrouter,
    )
    .await;
    assert!(sid.is_none());
    assert!(cid.is_none());
    assert_eq!(xsid.as_deref(), Some("session-openrouter"));
    let p = payload.unwrap();
    assert!(p.get("session_id").is_none());
    assert_eq!(p.get("prompt_cache_key"), Some(&json!("session-openrouter")));

    let mut nosession = base.clone();
    nosession.provider = "proxy".to_owned();
    nosession.base_url = "https://proxy.example.com/v1".to_owned();
    nosession.compat = Some(ModelCompat::OpenAIResponses(OpenAIResponsesCompat {
        session_affinity_format: Some(SessionAffinityFormat::OpenAINosession),
        ..Default::default()
    }));
    let (sid, cid, xsid, payload) = capture_headers(
        ApiStreamOptions {
            base: StreamOptions {
                session_id: Some("session-proxy".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        },
        &nosession,
    )
    .await;
    assert!(sid.is_none());
    assert_eq!(cid.as_deref(), Some("session-proxy"));
    assert!(xsid.is_none());
    let p = payload.unwrap();
    assert!(p.get("session_id").is_none());
    assert_eq!(p.get("prompt_cache_key"), Some(&json!("session-proxy")));

    let opencode_model = get_model("opencode", "gpt-5.4").unwrap();
    assert!(matches!(
        &opencode_model.compat,
        Some(ModelCompat::OpenAIResponses(c))
            if c.session_affinity_format == Some(SessionAffinityFormat::OpenAINosession)
    ));
    let (sid, cid, xsid, payload) = capture_headers(
        ApiStreamOptions {
            base: StreamOptions {
                session_id: Some("session-opencode".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        },
        &opencode_model,
    )
    .await;
    assert!(sid.is_none());
    assert_eq!(cid.as_deref(), Some("session-opencode"));
    assert!(xsid.is_none());
    assert_eq!(
        payload.unwrap().get("prompt_cache_key"),
        Some(&json!("session-opencode"))
    );
}

#[tokio::test]
async fn lets_explicit_headers_override_and_omits_headers_when_cache_retention_is_none() {
    let model = get_model("openai", "gpt-5.4").unwrap();
    let mut headers: ProviderHeaders = indexmap::IndexMap::new();
    headers.insert("session_id".to_owned(), Some("override-session".to_owned()));
    headers.insert("x-client-request-id".to_owned(), Some("override-request".to_owned()));

    let (sid, cid, _, _) = capture_headers(
        ApiStreamOptions {
            base: StreamOptions {
                session_id: Some("session-123".to_owned()),
                headers: Some(headers),
                ..Default::default()
            },
            ..Default::default()
        },
        &model,
    )
    .await;
    assert_eq!(sid.as_deref(), Some("override-session"));
    assert_eq!(cid.as_deref(), Some("override-request"));

    let (sid, cid, _, _) = capture_headers(
        ApiStreamOptions {
            base: StreamOptions {
                cache_retention: Some(CacheRetention::None),
                session_id: Some("session-123".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        },
        &model,
    )
    .await;
    assert!(sid.is_none());
    assert!(cid.is_none());
}

#[tokio::test]
async fn applies_cost_multipliers_for_requested_and_returned_service_tiers() {
    for (model_id, service_tier, response_service_tier, multiplier) in [
        ("gpt-5.4", "priority", "priority", 2.0),
        ("gpt-5.5", "priority", "priority", 2.5),
        ("gpt-5.5", "flex", "flex", 0.5),
        ("gpt-6-luna", "priority", "fast", 2.0),
        ("gpt-6-luna", "fast", "fast", 2.0),
    ] {
        let model = get_model("openai", model_id).unwrap();
        let token_count = 100_000_i64;
        let token_scale = token_count as f64 / 1_000_000.0;
        let sse = format!(
            "data: {}\n\n",
            pi_js::json::stringify(&json!({
                "type": "response.completed",
                "response": {
                    "status": "completed",
                    "service_tier": response_service_tier,
                    "usage": {
                        "input_tokens": token_count,
                        "output_tokens": token_count,
                        "total_tokens": token_count * 2,
                        "input_tokens_details": { "cached_tokens": 0 }
                    }
                }
            }))
        );

        let result = stream_openai_responses(
            &model,
            &sys_hi_context(),
            Some(
                ApiStreamOptions {
                    base: StreamOptions {
                        api_key: Some("sk-test-key".to_owned()),
                        fetch: Some(Arc::new(move |_| {
                            let body = sse.clone();
                            Box::pin(async move {
                                Ok(Response::new(
                                    200,
                                    Headers::from_pairs([("content-type", "text/event-stream")]),
                                    body,
                                ))
                            })
                        })),
                        ..Default::default()
                    },
                    ..Default::default()
                }
                .with_extra("serviceTier", json!(service_tier)),
            ),
        )
        .result()
        .await;

        assert_eq!(result.stop_reason, StopReason::Stop);
        assert!(
            (result.usage.cost.input - model.cost.input * multiplier * token_scale).abs() < 1e-12,
            "{model_id} {service_tier}/{response_service_tier}"
        );
        assert!((result.usage.cost.output - model.cost.output * multiplier * token_scale).abs() < 1e-12);
        assert!(
            (result.usage.cost.total - (model.cost.input + model.cost.output) * multiplier * token_scale).abs() < 1e-12
        );
    }
}

#[tokio::test]
async fn sends_and_omits_max_output_tokens_according_to_compat() {
    let base_model = get_model("openai", "gpt-5.4").unwrap();
    let (_, _, _, p1) = capture_headers(
        ApiStreamOptions {
            base: StreamOptions {
                max_tokens: Some(1024),
                ..Default::default()
            },
            ..Default::default()
        },
        &base_model,
    )
    .await;
    assert_eq!(p1.unwrap().get("max_output_tokens"), Some(&json!(1024)));

    let mut no_max_model = base_model.clone();
    let mut compat = match no_max_model.compat {
        Some(ModelCompat::OpenAIResponses(c)) => c,
        _ => OpenAIResponsesCompat::default(),
    };
    compat.supports_max_output_tokens = Some(false);
    no_max_model.compat = Some(ModelCompat::OpenAIResponses(compat));

    let (_, _, _, p2) = capture_headers(
        ApiStreamOptions {
            base: StreamOptions {
                max_tokens: Some(1024),
                ..Default::default()
            },
            ..Default::default()
        },
        &no_max_model,
    )
    .await;
    assert!(p2.unwrap().get("max_output_tokens").is_none());
}
