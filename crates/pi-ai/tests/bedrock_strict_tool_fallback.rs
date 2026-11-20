//! Port of packages/ai/test/bedrock-strict-tool-fallback.test.ts

use std::sync::{Arc, Mutex};

use aws_sdk_bedrockruntime::types::{
    ConversationRole, ConverseStreamOutput, MessageStartEvent, MessageStopEvent, StopReason as BedrockStopReason,
};
use indexmap::IndexMap;
use pi_ai::api::bedrock_converse_stream::{
    BedrockOptions, BedrockTransportError, BedrockTransportEvent, BedrockTransportOutcome,
    BedrockTransportStreamResponse, override_bedrock_transport, stream_with_options,
};
use pi_ai::session_resources::cleanup_session_resources;
use pi_ai::types::{
    Api, BedrockCompat, CacheRetention, ConstrainedSampling, ConstrainedSamplingConfig, Context, JsonSchemaStrict,
    KnownApi, KnownProvider, Message, Model, ModelCompat, ModelCost, ModelInput, ProviderId, StopReason, StreamOptions,
    Tool, UserMessage, UserMessageContent,
};
use pi_ai::utils::transcript::normalize_context;
use pi_js::vendor::typebox::t;
use serde_json::{Value, json};
use serial_test::serial;

const UNION_LIMIT_MESSAGE: &str = "The model returned the following errors: Schemas contains too many parameters with union types (20 parameters with type arrays or anyOf). This causes exponential compilation cost. Reduce the number of nullable or union-typed parameters (limit: 16 parameters with unions).";

fn test_model() -> Model {
    Model {
        r#type: None,
        id: "us.anthropic.claude-opus-5-5".to_owned(),
        name: "Claude Opus 5.5 (US)".to_owned(),
        api: Api::from(KnownApi::BedrockConverseStream),
        provider: ProviderId::from(KnownProvider::AmazonBedrock),
        base_url: "https://bedrock-runtime.us-east-1.amazonaws.com".to_owned(),
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
        compat: Some(ModelCompat::Bedrock(BedrockCompat {
            supports_strict_mode: Some(true),
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

fn sdk_error(code: &str, message: &str) -> BedrockTransportOutcome {
    BedrockTransportOutcome::Error(BedrockTransportError {
        message: message.to_owned(),
        status: Some(400),
        body: None,
        error_code: Some(code.to_owned()),
        request_id: None,
        is_service_exception: true,
    })
}

fn success_response() -> BedrockTransportOutcome {
    BedrockTransportOutcome::Stream(BedrockTransportStreamResponse {
        http_status: Some(200),
        request_id: None,
        headers: IndexMap::new(),
        events: vec![
            BedrockTransportEvent::Sdk(ConverseStreamOutput::MessageStart(
                MessageStartEvent::builder()
                    .role(ConversationRole::Assistant)
                    .build()
                    .unwrap(),
            )),
            BedrockTransportEvent::Sdk(ConverseStreamOutput::MessageStop(
                MessageStopEvent::builder()
                    .stop_reason(BedrockStopReason::EndTurn)
                    .build()
                    .unwrap(),
            )),
        ],
    })
}

async fn run_fallback(
    tools: Vec<Tool>,
    on_payload: Option<pi_ai::types::OnPayloadFn<Model>>,
    session_id: Option<&str>,
) -> pi_ai::types::AssistantMessage {
    let ctx = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Hello".to_owned()),
            timestamp: 1,
        })],
        tools: Some(tools),
    });
    stream_with_options(
        &test_model(),
        &ctx,
        Some(BedrockOptions {
            base: StreamOptions {
                cache_retention: Some(CacheRetention::None),
                on_payload,
                session_id: session_id.map(str::to_owned),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await
}

fn strict_flags(inputs: &[Value]) -> Vec<Vec<Option<bool>>> {
    inputs
        .iter()
        .map(|input| {
            input["toolConfig"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["toolSpec"].get("strict").and_then(Value::as_bool))
                .collect()
        })
        .collect()
}

// Regression tests for https://github.com/earendil-works/pi/issues/11416 and #11626.
#[tokio::test]
#[serial]
async fn retries_strict_tool_limit_rejection_with_preferred_non_strict_and_remembers_session() {
    cleanup_session_resources(None).unwrap();

    // 1. Retries strict tool limit rejection keeping require strict, calls on_payload twice, emits diagnostic
    {
        let inputs = Arc::new(Mutex::new(Vec::<Value>::new()));
        let inputs_c = Arc::clone(&inputs);
        let on_payload_calls = Arc::new(Mutex::new(0usize));
        let opc = Arc::clone(&on_payload_calls);
        let _guard = override_bedrock_transport(move |req| {
            let mut guard = inputs_c.lock().unwrap();
            let attempt = guard.len() + 1;
            guard.push(req.command_input.clone());
            Box::pin(async move {
                if attempt == 1 {
                    Ok(sdk_error("ValidationException", UNION_LIMIT_MESSAGE))
                } else {
                    Ok(success_response())
                }
            })
        });
        let result = run_fallback(
            vec![
                tool("preferred", Some(JsonSchemaStrict::Prefer)),
                tool("required", Some(JsonSchemaStrict::Require)),
                tool("ordinary", None),
            ],
            Some(Arc::new(move |mut payload, _| {
                *opc.lock().unwrap() += 1;
                if let Some(obj) = payload.as_object_mut() {
                    obj.insert("requestMetadata".to_owned(), json!({ "team": "a" }));
                }
                Box::pin(async move { Ok(Some(payload)) })
            })),
            None,
        )
        .await;

        assert_eq!(result.stop_reason, StopReason::Stop);
        assert_eq!(*on_payload_calls.lock().unwrap(), 2);
        let recorded = inputs.lock().unwrap();
        let first = &recorded[0];
        let second = &recorded[1];
        assert_eq!(first["toolConfig"]["tools"][0]["toolSpec"]["strict"], json!(true));
        assert_eq!(
            first["toolConfig"]["tools"][0]["toolSpec"]["inputSchema"]["json"]["required"],
            json!(["value", "optional"])
        );
        assert!(second["toolConfig"]["tools"][0]["toolSpec"].get("strict").is_none());
        assert_eq!(
            second["toolConfig"]["tools"][0]["toolSpec"]["inputSchema"]["json"],
            json!({
                "type": "object",
                "properties": { "value": { "type": "string" }, "optional": { "type": "number" } },
                "required": ["value"],
            })
        );
        assert_eq!(second["toolConfig"]["tools"][1], first["toolConfig"]["tools"][1]);
        assert_eq!(second["toolConfig"]["tools"][1]["toolSpec"]["strict"], json!(true));
        assert_eq!(second["toolConfig"]["tools"][2], first["toolConfig"]["tools"][2]);
        assert_eq!(second["requestMetadata"], json!({ "team": "a" }));
        let diags = result.diagnostics.expect("diagnostics");
        assert_eq!(diags[0].r#type, "bedrock_strict_tools_fallback");
        assert_eq!(
            diags[0].details.as_ref().and_then(|d| d.get("reason")),
            Some(&json!("strict_tool_limits_exceeded"))
        );
    }

    // 2. Does not retry without preferred strict tools
    {
        let calls = Arc::new(Mutex::new(0usize));
        let calls_c = Arc::clone(&calls);
        let _guard = override_bedrock_transport(move |_| {
            *calls_c.lock().unwrap() += 1;
            Box::pin(async { Ok(sdk_error("ValidationException", UNION_LIMIT_MESSAGE)) })
        });
        let r = run_fallback(
            vec![
                tool("required", Some(JsonSchemaStrict::Require)),
                tool("ordinary", None),
            ],
            None,
            None,
        )
        .await;
        assert_eq!(r.stop_reason, StopReason::Error);
        assert_eq!(
            r.error_message.as_deref(),
            Some(format!("Validation error: {UNION_LIMIT_MESSAGE}").as_str())
        );
        assert_eq!(*calls.lock().unwrap(), 1);
    }

    // 3. Does not retry unrelated ValidationException
    {
        let calls = Arc::new(Mutex::new(0usize));
        let calls_c = Arc::clone(&calls);
        let _guard = override_bedrock_transport(move |_| {
            *calls_c.lock().unwrap() += 1;
            Box::pin(async { Ok(sdk_error("ValidationException", "messages: field required")) })
        });
        let r = run_fallback(vec![tool("preferred", Some(JsonSchemaStrict::Prefer))], None, None).await;
        assert_eq!(r.stop_reason, StopReason::Error);
        assert_eq!(*calls.lock().unwrap(), 1);
    }

    // 4. Retries only once
    {
        let calls = Arc::new(Mutex::new(0usize));
        let calls_c = Arc::clone(&calls);
        let _guard = override_bedrock_transport(move |_| {
            *calls_c.lock().unwrap() += 1;
            Box::pin(async { Ok(sdk_error("ValidationException", UNION_LIMIT_MESSAGE)) })
        });
        let r = run_fallback(vec![tool("preferred", Some(JsonSchemaStrict::Prefer))], None, None).await;
        assert_eq!(r.stop_reason, StopReason::Error);
        assert_eq!(*calls.lock().unwrap(), 2);
    }

    // 5. Does not remember without session; remembers per session until cleanup
    {
        let inputs = Arc::new(Mutex::new(Vec::<Value>::new()));
        let inputs_c = Arc::clone(&inputs);
        let _guard = override_bedrock_transport(move |req| {
            let mut g = inputs_c.lock().unwrap();
            let attempt = g.len() + 1;
            g.push(req.command_input.clone());
            Box::pin(async move {
                if attempt == 1 {
                    Ok(sdk_error("ValidationException", UNION_LIMIT_MESSAGE))
                } else {
                    Ok(success_response())
                }
            })
        });
        run_fallback(vec![tool("preferred", Some(JsonSchemaStrict::Prefer))], None, None).await;
        run_fallback(vec![tool("preferred", Some(JsonSchemaStrict::Prefer))], None, None).await;
        assert_eq!(
            strict_flags(&inputs.lock().unwrap()),
            vec![vec![Some(true)], vec![None], vec![Some(true)]]
        );
    }

    {
        let inputs = Arc::new(Mutex::new(Vec::<Value>::new()));
        let inputs_c = Arc::clone(&inputs);
        let _guard = override_bedrock_transport(move |req| {
            let mut g = inputs_c.lock().unwrap();
            let attempt = g.len() + 1;
            g.push(req.command_input.clone());
            Box::pin(async move {
                if attempt == 1 {
                    Ok(sdk_error("ValidationException", UNION_LIMIT_MESSAGE))
                } else {
                    Ok(success_response())
                }
            })
        });
        let tools = vec![
            tool("preferred", Some(JsonSchemaStrict::Prefer)),
            tool("required", Some(JsonSchemaStrict::Require)),
        ];
        run_fallback(tools.clone(), None, Some("rejected")).await;
        let second = run_fallback(tools.clone(), None, Some("rejected")).await;
        run_fallback(tools, None, Some("other")).await;
        assert_eq!(second.stop_reason, StopReason::Stop);
        assert!(second.diagnostics.is_none());
        assert_eq!(
            strict_flags(&inputs.lock().unwrap()),
            vec![
                vec![Some(true), Some(true)],
                vec![None, Some(true)],
                vec![None, Some(true)],
                vec![Some(true), Some(true)],
            ]
        );
    }

    {
        let inputs = Arc::new(Mutex::new(Vec::<Value>::new()));
        let inputs_c = Arc::clone(&inputs);
        let _guard = override_bedrock_transport(move |req| {
            let mut g = inputs_c.lock().unwrap();
            let attempt = g.len() + 1;
            g.push(req.command_input.clone());
            Box::pin(async move {
                if attempt == 1 {
                    Ok(sdk_error("ValidationException", UNION_LIMIT_MESSAGE))
                } else {
                    Ok(success_response())
                }
            })
        });
        run_fallback(
            vec![tool("preferred", Some(JsonSchemaStrict::Prefer))],
            None,
            Some("cleaned"),
        )
        .await;
        cleanup_session_resources(Some("cleaned")).unwrap();
        run_fallback(
            vec![tool("preferred", Some(JsonSchemaStrict::Prefer))],
            None,
            Some("cleaned"),
        )
        .await;
        assert_eq!(
            strict_flags(&inputs.lock().unwrap()),
            vec![vec![Some(true)], vec![None], vec![Some(true)]]
        );
    }

    cleanup_session_resources(None).unwrap();
}
