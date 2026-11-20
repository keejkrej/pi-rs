//! Port of packages/ai/test/anthropic-strict-tool-fallback.test.ts

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use pi_ai::api::anthropic_messages::{AnthropicOptions, stream_with_options};
use pi_ai::api::pi_messages;
use pi_ai::compat::normalize_context;
use pi_ai::session_resources::cleanup_session_resources;
use pi_ai::types::{
    AnthropicMessagesCompat, Api, ApiStreamOptions, AssistantMessageEvent, CacheRetention, ConstrainedSampling,
    ConstrainedSamplingConfig, Context, JsonSchemaStrict, KnownApi, KnownProvider, Message, Model, ModelCompat,
    ModelCost, ModelInput, ProviderId, StopReason, StreamOptions, Tool, TranscriptContext, UserMessage,
    UserMessageContent,
};
use pi_js::Error;
use pi_js::abort::AbortController;
use pi_js::fetch::{Headers, Request, Response};
use pi_js::vendor::typebox::t;
use serde::Deserialize;
use serde_json::{Value, json};
use serial_test::serial;

const GRAMMAR_ERROR: &str = "The compiled grammar is too large, which would cause performance issues. Simplify your tool schemas or reduce the number of strict tools.";

fn test_model() -> Model {
    Model {
        r#type: None,
        id: "claude-haiku-4-5".to_owned(),
        name: "Test Claude".to_owned(),
        api: Api::from(KnownApi::AnthropicMessages),
        provider: ProviderId::from(KnownProvider::Anthropic),
        base_url: "https://anthropic.example".to_owned(),
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
        compat: Some(ModelCompat::AnthropicMessages(AnthropicMessagesCompat {
            supports_strict_tools: Some(true),
            ..Default::default()
        })),
        sampling_params_by_thinking_level: None,
    }
}

fn tool(name: &str, strict: Option<JsonSchemaStrict>) -> Tool {
    Tool {
        name: name.to_owned(),
        description: "Look up a value".to_owned(),
        parameters: t::object([("value", t::string()), ("optional", t::number().optional())]),
        constrained_sampling: strict
            .map(|s| ConstrainedSampling::Config(ConstrainedSamplingConfig::JsonSchema { strict: s })),
    }
}

fn context(tools: Vec<Tool>) -> TranscriptContext {
    normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Hello".to_owned()),
            timestamp: 1,
        })],
        tools: Some(tools),
    })
}

fn error_response(message: &str, status: u16, err_type: &str) -> Response {
    Response::new(
        status,
        Headers::from_pairs([("content-type", "application/json")]),
        pi_js::json::stringify(&json!({
            "type": "error",
            "error": { "type": err_type, "message": message },
        })),
    )
}

fn success_response() -> Response {
    let events = [
        json!({
            "type": "message_start",
            "message": { "id": "msg_test", "model": "claude-haiku-4-5", "usage": { "input_tokens": 10, "output_tokens": 0 } },
        }),
        json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 1 } }),
        json!({ "type": "message_stop" }),
    ];
    let body = events
        .iter()
        .map(|ev| {
            let kind = ev["type"].as_str().unwrap_or("");
            format!("event: {kind}\ndata: {}\n\n", pi_js::json::stringify(ev))
        })
        .collect::<String>();
    Response::new(200, Headers::from_pairs([("content-type", "text/event-stream")]), body)
}

fn harness<F>(respond: F) -> (Arc<Mutex<Vec<Value>>>, AnthropicOptions)
where
    F: Fn(&Value, usize) -> Response + Send + Sync + 'static,
{
    let payloads = Arc::new(Mutex::new(Vec::<Value>::new()));
    let payloads_ref = Arc::clone(&payloads);
    let respond = Arc::new(respond);
    let options = AnthropicOptions {
        base: StreamOptions {
            api_key: Some("test-key".to_owned()),
            max_retries: Some(0),
            fetch: Some(Arc::new(move |req: Request| {
                let payloads_c = Arc::clone(&payloads_ref);
                let respond_c = Arc::clone(&respond);
                Box::pin(async move {
                    let payload = req.body_json().unwrap_or(Value::Null);
                    let attempt = {
                        let mut guard = payloads_c.lock().unwrap();
                        guard.push(payload.clone());
                        guard.len()
                    };
                    Ok(respond_c(&payload, attempt))
                })
            })),
            ..Default::default()
        },
        ..Default::default()
    };
    (payloads, options)
}

fn strict_flags(payloads: &[Value]) -> Vec<Vec<Option<bool>>> {
    payloads
        .iter()
        .map(|p| {
            p["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t.get("strict").and_then(Value::as_bool))
                .collect()
        })
        .collect()
}

// Regression tests for https://github.com/earendil-works/pi/issues/10409.
#[tokio::test]
#[serial]
async fn retries_strict_tool_limit_rejections_with_non_strict_preferred_and_preserves_required() {
    cleanup_session_resources(None).unwrap();

    let messages = [
        GRAMMAR_ERROR,
        "Too many strict tools (32). The maximum number of strict tools supported is 20. Try reducing the number of tools marked as strict.",
        "Schemas contains too many parameters with union types (480 parameters with type arrays or anyOf). This causes exponential compilation cost. Reduce the number of nullable or union-typed parameters (limit: 16 parameters with unions).",
    ];

    for message in messages {
        let preferred = tool("preferred", Some(JsonSchemaStrict::Prefer));
        let required = tool("required", Some(JsonSchemaStrict::Require));
        let ordinary = tool("ordinary", None);
        let transcript = context(vec![preferred, required, ordinary]);
        let original = serde_json::to_string(&transcript).unwrap();

        let msg_owned = message.to_owned();
        let (payloads, mut options) = harness(move |_payload, attempt| {
            if attempt == 1 {
                error_response(&msg_owned, 400, "invalid_request_error")
            } else {
                success_response()
            }
        });

        let on_payload_calls = Arc::new(AtomicUsize::new(0));
        let opc = Arc::clone(&on_payload_calls);
        options.base.on_payload = Some(Arc::new(move |mut payload, _| {
            opc.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                if let Some(obj) = payload.as_object_mut() {
                    obj.insert("inference_geo".to_owned(), json!("us"));
                    obj.insert("stream".to_owned(), json!(false));
                }
                Ok(Some(payload))
            })
        }));

        let on_response_calls = Arc::new(AtomicUsize::new(0));
        let orc = Arc::clone(&on_response_calls);
        options.base.on_response = Some(Arc::new(move |_resp, _| {
            orc.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }));
        options.base.cache_retention = Some(CacheRetention::Long);

        let mut stream = stream_with_options(&test_model(), &transcript, Some(options));
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            let kind = match ev {
                AssistantMessageEvent::Start { .. } => "start",
                AssistantMessageEvent::TextStart { .. } => "text_start",
                AssistantMessageEvent::TextDelta { .. } => "text_delta",
                AssistantMessageEvent::TextEnd { .. } => "text_end",
                AssistantMessageEvent::Done { .. } => "done",
                AssistantMessageEvent::Error { .. } => "error",
                _ => "other",
            };
            events.push(kind);
        }
        let result = stream.result().await;

        assert_eq!(result.stop_reason, StopReason::Stop);
        assert_eq!(events, vec!["start", "done"]);
        assert_eq!(on_payload_calls.load(Ordering::SeqCst), 2);
        assert_eq!(on_response_calls.load(Ordering::SeqCst), 1);

        let recorded = payloads.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        let first_tools = recorded[0]["tools"].as_array().unwrap();
        let second_tools = recorded[1]["tools"].as_array().unwrap();

        assert_eq!(first_tools[0]["strict"], json!(true));
        assert_eq!(first_tools[0]["input_schema"]["required"], json!(["value", "optional"]));
        assert_eq!(
            first_tools[0]["input_schema"]["properties"]["optional"],
            json!({ "anyOf": [{ "type": "number" }, { "type": "null" }] })
        );

        assert!(second_tools[0].get("strict").is_none());
        assert_eq!(
            second_tools[0]["input_schema"],
            json!({
                "type": "object",
                "properties": { "value": { "type": "string" }, "optional": { "type": "number" } },
                "required": ["value"],
            })
        );
        assert_eq!(second_tools[1], first_tools[1]);
        assert_eq!(second_tools[1]["strict"], json!(true));
        assert_eq!(second_tools[2], first_tools[2]);
        assert_eq!(
            second_tools[2]["cache_control"],
            json!({ "type": "ephemeral", "ttl": "1h" })
        );
        assert_eq!(
            recorded.iter().map(|p| p["stream"].clone()).collect::<Vec<_>>(),
            vec![json!(true), json!(true)]
        );
        assert_eq!(recorded[1]["inference_geo"], json!("us"));
        assert_eq!(serde_json::to_string(&transcript).unwrap(), original);
        let diags = result.diagnostics.expect("diagnostics");
        assert_eq!(diags[0].r#type, "anthropic_strict_tools_fallback");
        assert_eq!(
            diags[0].details.as_ref().and_then(|d| d.get("reason")),
            Some(&json!("strict_tool_limits_exceeded"))
        );
    }

    cleanup_session_resources(None).unwrap();
}

#[tokio::test]
#[serial]
async fn downgrades_inline_additions_and_redefinitions_without_downgrading_require() {
    use pi_ai::types::{SystemMessage, SystemMessageContent};

    cleanup_session_resources(None).unwrap();

    let mut transcript = context(vec![tool("lookup", Some(JsonSchemaStrict::Require))]);
    transcript.messages.push(Message::System(SystemMessage {
        content: SystemMessageContent::Text(String::new()),
        sections: None,
        tools_added: Some(vec![tool("lookup", Some(JsonSchemaStrict::Prefer))]),
        tools_removed: None,
        timestamp: 2,
    }));
    transcript.messages.push(Message::System(SystemMessage {
        content: SystemMessageContent::Text(String::new()),
        sections: None,
        tools_added: Some(vec![tool("lookup", Some(JsonSchemaStrict::Require))]),
        tools_removed: None,
        timestamp: 3,
    }));

    let (payloads, mut options) = harness(|_payload, attempt| {
        if attempt == 1 {
            error_response(GRAMMAR_ERROR, 400, "invalid_request_error")
        } else {
            success_response()
        }
    });
    options.base.api_key = Some("sk-ant-oat-test".to_owned());

    let mut model = test_model();
    model.compat = Some(ModelCompat::AnthropicMessages(AnthropicMessagesCompat {
        supports_strict_tools: Some(true),
        supports_mid_convo_system_messages: Some(true),
        supports_mid_convo_tool_changes: Some(true),
        ..Default::default()
    }));

    let result = stream_with_options(&model, &transcript, Some(options)).result().await;
    assert_eq!(result.stop_reason, StopReason::Stop);

    let recorded = payloads.lock().unwrap();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[1]["tools"], recorded[0]["tools"]);

    let additions: Vec<Vec<Value>> = recorded
        .iter()
        .map(|payload| {
            payload["messages"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|msg| {
                    msg["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|block| {
                            if block.get("type").and_then(Value::as_str) == Some("tool_addition")
                                && block["tool"].get("type").and_then(Value::as_str) == Some("tool_definition")
                            {
                                Some(block["tool"]["definition"].clone())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .collect()
        })
        .collect();

    assert_eq!(
        additions[0]
            .iter()
            .map(|d| d.get("strict").and_then(Value::as_bool))
            .collect::<Vec<_>>(),
        vec![Some(true), Some(true)]
    );
    assert_eq!(
        additions[1]
            .iter()
            .map(|d| d.get("strict").and_then(Value::as_bool))
            .collect::<Vec<_>>(),
        vec![None, Some(true)]
    );
    assert_eq!(additions[1][0]["input_schema"]["required"], json!(["value"]));
    assert_eq!(additions[1][1], additions[0][1]);

    cleanup_session_resources(None).unwrap();
}

#[tokio::test]
#[serial]
async fn does_not_retry_non_eligible_tool_sets_or_non_matching_errors() {
    cleanup_session_resources(None).unwrap();

    let unsupported_prefer = Tool {
        parameters: t::object([("value", t::number().with("minimum", 1))]),
        ..tool("lookup", Some(JsonSchemaStrict::Prefer))
    };
    let tool_sets = vec![
        vec![tool("lookup", Some(JsonSchemaStrict::Require))],
        vec![tool("lookup", None)],
        vec![unsupported_prefer],
    ];
    for tools in tool_sets {
        let (payloads, options) = harness(|_, _| error_response(GRAMMAR_ERROR, 400, "invalid_request_error"));
        let result = stream_with_options(&test_model(), &context(tools), Some(options))
            .result()
            .await;
        assert_eq!(result.stop_reason, StopReason::Error);
        assert!(result.error_message.as_deref().unwrap_or("").contains(GRAMMAR_ERROR));
        assert_eq!(payloads.lock().unwrap().len(), 1);
    }

    let wrong_errors = [
        ("Invalid tool schema", 400u16, "invalid_request_error"),
        (GRAMMAR_ERROR, 400, "authentication_error"),
        (GRAMMAR_ERROR, 500, "invalid_request_error"),
    ];
    for (msg, status, err_type) in wrong_errors {
        let (payloads, options) = harness(move |_, _| error_response(msg, status, err_type));
        let result = stream_with_options(
            &test_model(),
            &context(vec![tool("lookup", Some(JsonSchemaStrict::Prefer))]),
            Some(options),
        )
        .result()
        .await;
        assert_eq!(result.stop_reason, StopReason::Error);
        assert_eq!(payloads.lock().unwrap().len(), 1);
    }

    cleanup_session_resources(None).unwrap();
}

#[tokio::test]
#[serial]
async fn surfaces_second_rejection_and_does_not_downgrade_hook_errors() {
    cleanup_session_resources(None).unwrap();

    // Surfaces a second rejection rather than retrying again.
    {
        let (payloads, options) = harness(|_, attempt| {
            let msg = if attempt == 1 {
                GRAMMAR_ERROR
            } else {
                "Required grammar is still too large"
            };
            error_response(msg, 400, "invalid_request_error")
        });
        let result = stream_with_options(
            &test_model(),
            &context(vec![
                tool("lookup", Some(JsonSchemaStrict::Prefer)),
                tool("required", Some(JsonSchemaStrict::Require)),
            ]),
            Some(options),
        )
        .result()
        .await;
        assert_eq!(result.stop_reason, StopReason::Error);
        assert!(
            result
                .error_message
                .as_deref()
                .unwrap_or("")
                .contains("Required grammar is still too large")
        );
        let recorded = payloads.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[1]["tools"][1]["strict"], json!(true));
    }

    // Does not downgrade a hook error, even if its text looks like the provider rejection.
    {
        let (payloads, mut options) = harness(|_, _| success_response());
        options.base.on_payload = Some(Arc::new(|_, _| Box::pin(async { Err(Error::msg(GRAMMAR_ERROR)) })));
        let result = stream_with_options(
            &test_model(),
            &context(vec![tool("lookup", Some(JsonSchemaStrict::Prefer))]),
            Some(options),
        )
        .result()
        .await;
        assert_eq!(result.stop_reason, StopReason::Error);
        assert_eq!(payloads.lock().unwrap().len(), 0);
        assert!(result.diagnostics.is_none());
    }

    cleanup_session_resources(None).unwrap();
}

#[tokio::test]
#[serial]
async fn honors_abort_and_ignores_sse_errors_and_handles_session_memory() {
    cleanup_session_resources(None).unwrap();

    // Honors cancellation before the fallback attempt.
    {
        let controller = Arc::new(AbortController::new());
        let ctrl_c = Arc::clone(&controller);
        let (payloads, mut options) = harness(move |_, _| {
            ctrl_c.abort(None);
            error_response(GRAMMAR_ERROR, 400, "invalid_request_error")
        });
        options.base.signal = Some(controller.signal());
        let result = stream_with_options(
            &test_model(),
            &context(vec![tool("lookup", Some(JsonSchemaStrict::Prefer))]),
            Some(options),
        )
        .result()
        .await;
        assert_eq!(result.stop_reason, StopReason::Aborted);
        assert_eq!(payloads.lock().unwrap().len(), 1);
    }

    // Does not retry an SSE error after response headers.
    {
        let (payloads, options) = harness(|_, _| {
            let body = format!(
                "event: error\ndata: {}\n\n",
                pi_js::json::stringify(&json!({
                    "type": "error",
                    "error": { "type": "invalid_request_error", "message": GRAMMAR_ERROR },
                }))
            );
            Response::new(200, Headers::from_pairs([("content-type", "text/event-stream")]), body)
        });
        let result = stream_with_options(
            &test_model(),
            &context(vec![tool("lookup", Some(JsonSchemaStrict::Prefer))]),
            Some(options),
        )
        .result()
        .await;
        assert_eq!(result.stop_reason, StopReason::Error);
        assert_eq!(payloads.lock().unwrap().len(), 1);
    }

    // Does not disable strict sampling on later requests without a session.
    {
        let (payloads, options) = harness(|_, attempt| {
            if attempt == 1 {
                error_response(GRAMMAR_ERROR, 400, "invalid_request_error")
            } else {
                success_response()
            }
        });
        let transcript = context(vec![tool("lookup", Some(JsonSchemaStrict::Prefer))]);
        stream_with_options(&test_model(), &transcript, Some(options.clone()))
            .result()
            .await;
        stream_with_options(&test_model(), &transcript, Some(options))
            .result()
            .await;
        assert_eq!(
            strict_flags(&payloads.lock().unwrap()),
            vec![vec![Some(true)], vec![None], vec![Some(true)]]
        );
    }

    // Regression tests for https://github.com/earendil-works/pi/issues/11626:
    // Remembers per session and clears on cleanupSessionResources.
    {
        let (payloads, options) = harness(|_, attempt| {
            if attempt == 1 {
                error_response(GRAMMAR_ERROR, 400, "invalid_request_error")
            } else {
                success_response()
            }
        });
        let transcript = context(vec![
            tool("lookup", Some(JsonSchemaStrict::Prefer)),
            tool("required", Some(JsonSchemaStrict::Require)),
        ]);

        let mut opts_rejected = options.clone();
        opts_rejected.base.session_id = Some("rejected".to_owned());
        let first = stream_with_options(&test_model(), &transcript, Some(opts_rejected.clone()))
            .result()
            .await;
        let second = stream_with_options(&test_model(), &transcript, Some(opts_rejected))
            .result()
            .await;

        let mut opts_other = options;
        opts_other.base.session_id = Some("other".to_owned());
        stream_with_options(&test_model(), &transcript, Some(opts_other))
            .result()
            .await;

        assert_eq!(first.diagnostics.unwrap()[0].r#type, "anthropic_strict_tools_fallback");
        assert_eq!(second.stop_reason, StopReason::Stop);
        assert!(second.diagnostics.is_none());
        assert_eq!(
            strict_flags(&payloads.lock().unwrap()),
            vec![
                vec![Some(true), Some(true)],
                vec![None, Some(true)],
                vec![None, Some(true)],
                vec![Some(true), Some(true)],
            ]
        );
    }

    // Tries strict sampling again after session cleanup.
    {
        let (payloads, mut options) = harness(|_, attempt| {
            if attempt == 1 {
                error_response(GRAMMAR_ERROR, 400, "invalid_request_error")
            } else {
                success_response()
            }
        });
        options.base.session_id = Some("cleaned".to_owned());
        let transcript = context(vec![tool("lookup", Some(JsonSchemaStrict::Prefer))]);
        stream_with_options(&test_model(), &transcript, Some(options.clone()))
            .result()
            .await;
        cleanup_session_resources(Some("cleaned")).unwrap();
        stream_with_options(&test_model(), &transcript, Some(options))
            .result()
            .await;
        assert_eq!(
            strict_flags(&payloads.lock().unwrap()),
            vec![vec![Some(true)], vec![None], vec![Some(true)]]
        );
    }

    // Surfaces rejection of a remembered session's required tools without retrying.
    {
        let (payloads, mut options) = harness(|_, attempt| {
            if attempt == 2 {
                success_response()
            } else {
                error_response(GRAMMAR_ERROR, 400, "invalid_request_error")
            }
        });
        options.base.session_id = Some("required".to_owned());
        let transcript = context(vec![
            tool("lookup", Some(JsonSchemaStrict::Prefer)),
            tool("required", Some(JsonSchemaStrict::Require)),
        ]);
        stream_with_options(&test_model(), &transcript, Some(options.clone()))
            .result()
            .await;
        let result = stream_with_options(&test_model(), &transcript, Some(options))
            .result()
            .await;
        assert_eq!(result.stop_reason, StopReason::Error);
        assert!(result.error_message.as_deref().unwrap_or("").contains(GRAMMAR_ERROR));
        assert_eq!(payloads.lock().unwrap().len(), 3);
    }

    cleanup_session_resources(None).unwrap();
}

#[tokio::test]
#[serial]
async fn preserves_prefer_through_pi_messages_for_gateway_anthropic_fallback() {
    #[derive(Deserialize)]
    struct GatewayReq {
        context: TranscriptContext,
    }

    cleanup_session_resources(None).unwrap();

    let (payloads, anthropic_options) = harness(|_, attempt| {
        if attempt == 1 {
            error_response(GRAMMAR_ERROR, 400, "invalid_request_error")
        } else {
            success_response()
        }
    });
    let transcript = context(vec![tool("lookup", Some(JsonSchemaStrict::Prefer))]);
    let expected_json = serde_json::to_value(&transcript).unwrap();

    let gateway_model = Model {
        api: Api::from(KnownApi::PiMessages),
        provider: ProviderId::from("radius"),
        base_url: "https://radius.example/v1".to_owned(),
        compat: None,
        ..test_model()
    };

    let result = pi_messages::stream(
        &gateway_model,
        &transcript,
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("radius-test".to_owned()),
                fetch: Some(Arc::new(move |req: Request| {
                    let expected = expected_json.clone();
                    let opts = anthropic_options.clone();
                    Box::pin(async move {
                        let req_val = req.body_json().unwrap();
                        assert_eq!(req_val["context"], expected);
                        let parsed: GatewayReq = serde_json::from_value(req_val).unwrap();
                        let upstream = stream_with_options(&test_model(), &parsed.context, Some(opts))
                            .result()
                            .await;
                        let body = format!(
                            "data: {}\n\n",
                            pi_js::json::stringify(&json!({
                                "type": "done",
                                "reason": upstream.stop_reason,
                                "usage": upstream.usage,
                            }))
                        );
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
        }),
    )
    .result()
    .await;

    assert_eq!(result.stop_reason, StopReason::Stop);
    assert_eq!(
        strict_flags(&payloads.lock().unwrap()),
        vec![vec![Some(true)], vec![None]]
    );

    cleanup_session_resources(None).unwrap();
}
