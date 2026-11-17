//! Port of packages/ai/test/bedrock-raw-stop-reason.test.ts

use std::sync::{Arc, Mutex};

use aws_sdk_bedrockruntime::types::{
    ContentBlockDelta, ContentBlockDeltaEvent, ConversationRole, ConverseStreamMetadataEvent, ConverseStreamOutput,
    MessageStartEvent, MessageStopEvent, StopReason as BedrockStopReason, TokenUsage,
};
use aws_smithy_types::Document;
use indexmap::IndexMap;
use pi_ai::api::bedrock_converse_stream::{
    BedrockOptions, BedrockTransportError, BedrockTransportEvent, BedrockTransportOutcome,
    BedrockTransportStreamResponse, override_bedrock_transport, stream_with_options,
};
use pi_ai::providers::amazon_bedrock_models::amazon_bedrock_models;
use pi_ai::types::{
    AssistantContentBlock, CacheRetention, Context, JsonValue, Message, Model, StopReason, StreamOptions, TextContent,
    UserMessage, UserMessageContent,
};
use pi_ai::utils::transcript::normalize_context;
use serde_json::json;

fn model_fixture() -> Model {
    amazon_bedrock_models()
        .get("us.anthropic.claude-opus-4-8")
        .expect("us.anthropic.claude-opus-4-8 model")
        .clone()
}

fn hello_context() -> pi_ai::types::TranscriptContext {
    normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hello".to_string()),
            timestamp: 1,
        })],
        tools: None,
    })
}

#[tokio::test]
async fn forwards_sdk_stream_items_in_order_before_normalizing_them() {
    let _guard = override_bedrock_transport(|_req| {
        Box::pin(async {
            let extra = Document::Object(
                [("source".to_string(), Document::String("test".to_string()))]
                    .into_iter()
                    .collect(),
            );
            let events = vec![
                BedrockTransportEvent::Sdk(ConverseStreamOutput::MessageStart(
                    MessageStartEvent::builder()
                        .role(ConversationRole::Assistant)
                        .build()
                        .expect("message start"),
                )),
                BedrockTransportEvent::Sdk(ConverseStreamOutput::ContentBlockDelta(
                    ContentBlockDeltaEvent::builder()
                        .content_block_index(0)
                        .delta(ContentBlockDelta::Text("hello".to_string()))
                        .build()
                        .expect("delta"),
                )),
                BedrockTransportEvent::Sdk(ConverseStreamOutput::MessageStop(
                    MessageStopEvent::builder()
                        .stop_reason(BedrockStopReason::EndTurn)
                        .additional_model_response_fields(extra)
                        .build()
                        .expect("message stop"),
                )),
                BedrockTransportEvent::Sdk(ConverseStreamOutput::Metadata(
                    ConverseStreamMetadataEvent::builder()
                        .usage(
                            TokenUsage::builder()
                                .input_tokens(1)
                                .output_tokens(1)
                                .total_tokens(2)
                                .build()
                                .expect("usage"),
                        )
                        .build(),
                )),
            ];
            Ok(BedrockTransportOutcome::Stream(BedrockTransportStreamResponse {
                http_status: Some(200),
                request_id: Some("request-id".to_string()),
                headers: IndexMap::new(),
                events,
            }))
        })
    });

    let received: Arc<Mutex<Vec<JsonValue>>> = Arc::new(Mutex::new(Vec::new()));
    let event_models: Arc<Mutex<Vec<Model>>> = Arc::new(Mutex::new(Vec::new()));
    let received_ref = Arc::clone(&received);
    let event_models_ref = Arc::clone(&event_models);

    let model = model_fixture();
    let result = stream_with_options(
        &model,
        &hello_context(),
        Some(BedrockOptions {
            base: StreamOptions {
                cache_retention: Some(CacheRetention::None),
                on_provider_stream_event: Some(Arc::new(move |item, event_model| {
                    let received_inner = Arc::clone(&received_ref);
                    let models_inner = Arc::clone(&event_models_ref);
                    let item_owned = item.clone();
                    let model_owned = event_model.clone();
                    Box::pin(async move {
                        received_inner.lock().unwrap().push(item_owned);
                        models_inner.lock().unwrap().push(model_owned);
                        Ok(())
                    })
                })),
                ..StreamOptions::default()
            },
            ..BedrockOptions::default()
        }),
    )
    .result()
    .await;

    let items = received.lock().unwrap().clone();
    assert_eq!(
        items,
        vec![
            json!({ "messageStart": { "role": "assistant" } }),
            json!({ "contentBlockDelta": { "contentBlockIndex": 0, "delta": { "text": "hello" } } }),
            json!({
                "messageStop": {
                    "stopReason": "end_turn",
                    "additionalModelResponseFields": { "source": "test" }
                }
            }),
            json!({ "metadata": { "usage": { "inputTokens": 1, "outputTokens": 1, "totalTokens": 2 } } }),
        ]
    );

    let models_seen = event_models.lock().unwrap();
    assert_eq!(models_seen.len(), 4);
    for m in models_seen.iter() {
        assert_eq!(m.id, model.id);
    }

    assert_eq!(result.stop_reason, StopReason::Stop);
    assert_eq!(
        result.content,
        vec![AssistantContentBlock::Text(TextContent::new("hello"))]
    );
}

#[tokio::test]
async fn forwards_sdk_error_items_before_reporting_them() {
    let _guard = override_bedrock_transport(|_req| {
        Box::pin(async {
            let events = vec![
                BedrockTransportEvent::Sdk(ConverseStreamOutput::MessageStart(
                    MessageStartEvent::builder()
                        .role(ConversationRole::Assistant)
                        .build()
                        .expect("message start"),
                )),
                BedrockTransportEvent::StreamError(BedrockTransportError {
                    message: "bedrock stream failed".to_string(),
                    status: None,
                    body: None,
                    error_code: Some("InternalServerException".to_string()),
                    request_id: None,
                    is_service_exception: false,
                }),
            ];
            Ok(BedrockTransportOutcome::Stream(BedrockTransportStreamResponse {
                http_status: Some(200),
                request_id: Some("request-id".to_string()),
                headers: IndexMap::new(),
                events,
            }))
        })
    });

    let received: Arc<Mutex<Vec<JsonValue>>> = Arc::new(Mutex::new(Vec::new()));
    let received_ref = Arc::clone(&received);

    let result = stream_with_options(
        &model_fixture(),
        &hello_context(),
        Some(BedrockOptions {
            base: StreamOptions {
                cache_retention: Some(CacheRetention::None),
                on_provider_stream_event: Some(Arc::new(move |item, _model| {
                    let received_inner = Arc::clone(&received_ref);
                    let item_owned = item.clone();
                    Box::pin(async move {
                        received_inner.lock().unwrap().push(item_owned);
                        Ok(())
                    })
                })),
                ..StreamOptions::default()
            },
            ..BedrockOptions::default()
        }),
    )
    .result()
    .await;

    assert_eq!(
        *received.lock().unwrap(),
        vec![
            json!({ "messageStart": { "role": "assistant" } }),
            json!({ "internalServerException": { "message": "bedrock stream failed" } }),
        ]
    );
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(result.error_message.as_deref(), Some("bedrock stream failed"));
}

#[tokio::test]
async fn preserves_raw_bedrock_stop_reasons_for_successful_stops() {
    let _guard = override_bedrock_transport(|_req| {
        Box::pin(async {
            let events = vec![
                BedrockTransportEvent::Sdk(ConverseStreamOutput::MessageStart(
                    MessageStartEvent::builder()
                        .role(ConversationRole::Assistant)
                        .build()
                        .expect("message start"),
                )),
                BedrockTransportEvent::Sdk(ConverseStreamOutput::MessageStop(
                    MessageStopEvent::builder()
                        .stop_reason(BedrockStopReason::EndTurn)
                        .build()
                        .expect("message stop"),
                )),
            ];
            Ok(BedrockTransportOutcome::Stream(BedrockTransportStreamResponse {
                http_status: Some(200),
                request_id: Some("request-id".to_string()),
                headers: IndexMap::new(),
                events,
            }))
        })
    });

    let message = stream_with_options(
        &model_fixture(),
        &hello_context(),
        Some(BedrockOptions {
            base: StreamOptions {
                cache_retention: Some(CacheRetention::None),
                ..StreamOptions::default()
            },
            ..BedrockOptions::default()
        }),
    )
    .result()
    .await;

    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(message.error_message, None);
}

#[tokio::test]
async fn preserves_raw_bedrock_stop_reasons_for_provider_error_stops() {
    let _guard = override_bedrock_transport(|_req| {
        Box::pin(async {
            let events = vec![
                BedrockTransportEvent::Sdk(ConverseStreamOutput::MessageStart(
                    MessageStartEvent::builder()
                        .role(ConversationRole::Assistant)
                        .build()
                        .expect("message start"),
                )),
                BedrockTransportEvent::Sdk(ConverseStreamOutput::MessageStop(
                    MessageStopEvent::builder()
                        .stop_reason(BedrockStopReason::GuardrailIntervened)
                        .build()
                        .expect("message stop"),
                )),
            ];
            Ok(BedrockTransportOutcome::Stream(BedrockTransportStreamResponse {
                http_status: Some(200),
                request_id: Some("request-id".to_string()),
                headers: IndexMap::new(),
                events,
            }))
        })
    });

    let message = stream_with_options(
        &model_fixture(),
        &hello_context(),
        Some(BedrockOptions {
            base: StreamOptions {
                cache_retention: Some(CacheRetention::None),
                ..StreamOptions::default()
            },
            ..BedrockOptions::default()
        }),
    )
    .result()
    .await;

    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("guardrail_intervened"));
    assert_eq!(
        message.error_message.as_deref(),
        Some("Provider stopped with: guardrail_intervened")
    );
}
