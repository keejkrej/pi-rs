//! Port of packages/ai/test/bedrock-cache-write-1h-cost.test.ts

use std::sync::Arc;

use aws_sdk_bedrockruntime::types::{
    CacheDetail, CacheTtl, ConversationRole, ConverseStreamMetadataEvent, ConverseStreamOutput, MessageStartEvent,
    MessageStopEvent, StopReason as BedrockStopReason, TokenUsage,
};
use pi_ai::api::bedrock_converse_stream::{
    BedrockOptions, BedrockTransportEvent, BedrockTransportOutcome, BedrockTransportStreamResponse,
    override_bedrock_transport, stream_with_options,
};
use pi_ai::providers::amazon_bedrock_models::amazon_bedrock_models;
use pi_ai::types::{CacheRetention, Context, Message, StreamOptions, UserMessage, UserMessageContent};
use pi_ai::utils::transcript::normalize_context;

#[tokio::test]
async fn prices_1h_cache_details_at_2x_while_preserving_total_cache_write() {
    // Regression test for https://github.com/earendil-works/pi/issues/9457
    let _guard = override_bedrock_transport(|_req| {
        Box::pin(async {
            let msg_start = MessageStartEvent::builder()
                .role(ConversationRole::Assistant)
                .build()
                .expect("message start");
            let usage = TokenUsage::builder()
                .input_tokens(100)
                .output_tokens(5)
                .total_tokens(1_000_105)
                .cache_write_input_tokens(1_000_000)
                .cache_details(
                    CacheDetail::builder()
                        .ttl(CacheTtl::OneHour)
                        .input_tokens(150_000)
                        .build()
                        .expect("cache detail 1h a"),
                )
                .cache_details(
                    CacheDetail::builder()
                        .ttl(CacheTtl::FiveMinutes)
                        .input_tokens(600_000)
                        .build()
                        .expect("cache detail 5m"),
                )
                .cache_details(
                    CacheDetail::builder()
                        .ttl(CacheTtl::OneHour)
                        .input_tokens(250_000)
                        .build()
                        .expect("cache detail 1h b"),
                )
                .build()
                .expect("token usage");
            let metadata = ConverseStreamMetadataEvent::builder().usage(usage).build();
            let msg_stop = MessageStopEvent::builder()
                .stop_reason(BedrockStopReason::EndTurn)
                .build()
                .expect("message stop");

            Ok(BedrockTransportOutcome::Stream(BedrockTransportStreamResponse {
                http_status: Some(200),
                request_id: None,
                headers: Default::default(),
                events: vec![
                    BedrockTransportEvent::Sdk(ConverseStreamOutput::MessageStart(msg_start)),
                    BedrockTransportEvent::Sdk(ConverseStreamOutput::Metadata(metadata)),
                    BedrockTransportEvent::Sdk(ConverseStreamOutput::MessageStop(msg_stop)),
                ],
            }))
        })
    });

    let model = amazon_bedrock_models()
        .get("us.anthropic.claude-opus-4-8")
        .expect("us.anthropic.claude-opus-4-8 model")
        .clone();
    let context = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hi".to_string()),
            timestamp: pi_js::time::now_ms(),
        })],
        tools: None,
    });
    let options = BedrockOptions {
        base: StreamOptions {
            cache_retention: Some(CacheRetention::None),
            ..StreamOptions::default()
        },
        ..BedrockOptions::default()
    };

    let _keep_arc_type: Arc<str> = Arc::from("unused");
    let result = stream_with_options(&model, &context, Some(options)).result().await;

    assert_eq!(result.usage.cache_write, 1_000_000);
    assert_eq!(result.usage.cache_write_1h, Some(400_000));
    let expected_cache_write_cost =
        (600_000.0 * model.cost.cache_write + 400_000.0 * model.cost.input * 2.0) / 1_000_000.0;
    assert!(
        (result.usage.cost.cache_write - expected_cache_write_cost).abs() < 1e-10,
        "expected {}, got {}",
        expected_cache_write_cost,
        result.usage.cost.cache_write
    );
}
