//! Port of packages/ai/test/bedrock-error-metadata.test.ts

use aws_sdk_bedrockruntime::types::{ConversationRole, ConverseStreamOutput, MessageStartEvent};
use indexmap::IndexMap;
use pi_ai::api::bedrock_converse_stream::{
    BedrockOptions, BedrockTransportError, BedrockTransportEvent, BedrockTransportOutcome,
    BedrockTransportStreamResponse, override_bedrock_transport, stream_with_options,
};
use pi_ai::providers::amazon_bedrock_models::amazon_bedrock_models;
use pi_ai::types::{
    AssistantMessage, CacheRetention, Context, JsonObject, JsonValue, Message, Model, StopReason, StreamOptions,
    UserMessage, UserMessageContent,
};
use pi_ai::utils::diagnostics::AssistantMessageDiagnostic;
use pi_ai::utils::transcript::normalize_context;

const DIAGNOSTIC_TYPE: &str = "bedrock_response_failure";
const VALIDATION_MESSAGE: &str = "The provided model identifier is invalid.";
const REQUEST_ID: &str = "11111111-2222-3333-4444-555555555555";

fn model_fixture() -> Model {
    amazon_bedrock_models()
        .get("us.anthropic.claude-opus-4-8")
        .expect("us.anthropic.claude-opus-4-8 model")
        .clone()
}

fn respond_with_failing_stream(err: BedrockTransportError) -> BedrockTransportOutcome {
    BedrockTransportOutcome::Stream(BedrockTransportStreamResponse {
        http_status: Some(200),
        request_id: Some(REQUEST_ID.to_string()),
        headers: IndexMap::new(),
        events: vec![
            BedrockTransportEvent::Sdk(ConverseStreamOutput::MessageStart(
                MessageStartEvent::builder()
                    .role(ConversationRole::Assistant)
                    .build()
                    .expect("message start"),
            )),
            BedrockTransportEvent::StreamError(err),
        ],
    })
}

async fn run_bedrock(signal: Option<pi_js::AbortSignal>) -> AssistantMessage {
    let context = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hello".to_string()),
            timestamp: 1,
        })],
        tools: None,
    });
    stream_with_options(
        &model_fixture(),
        &context,
        Some(BedrockOptions {
            base: StreamOptions {
                cache_retention: Some(CacheRetention::None),
                signal,
                ..StreamOptions::default()
            },
            ..BedrockOptions::default()
        }),
    )
    .result()
    .await
}

fn find_diagnostic(message: &AssistantMessage) -> Option<&AssistantMessageDiagnostic> {
    message
        .diagnostics
        .as_ref()?
        .iter()
        .find(|d| d.r#type == DIAGNOSTIC_TYPE)
}

#[tokio::test]
async fn records_status_error_code_and_request_id_for_non_2xx_from_send() {
    let _guard = override_bedrock_transport(|_req| {
        Box::pin(async {
            Ok(BedrockTransportOutcome::Error(BedrockTransportError {
                message: VALIDATION_MESSAGE.to_string(),
                status: Some(400),
                body: None,
                error_code: Some("ValidationException".to_string()),
                request_id: Some(REQUEST_ID.to_string()),
                is_service_exception: true,
            }))
        })
    });

    let message = run_bedrock(None).await;
    let diagnostic = find_diagnostic(&message).expect("diagnostic");

    assert_eq!(message.stop_reason, StopReason::Error);
    let mut expected = JsonObject::new();
    expected.insert("status".to_string(), JsonValue::from(400));
    expected.insert(
        "errorCode".to_string(),
        JsonValue::String("ValidationException".to_string()),
    );
    expected.insert("requestId".to_string(), JsonValue::String(REQUEST_ID.to_string()));
    assert_eq!(diagnostic.details.as_ref(), Some(&expected));
    assert_eq!(diagnostic.error, None);

    let serialized: JsonValue = pi_js::json::parse(&pi_js::json::stringify(diagnostic)).unwrap();
    let mut keys: Vec<String> = serialized.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, vec!["details", "timestamp", "type"]);
}

#[tokio::test]
async fn leaves_error_message_untouched_so_retry_classification_is_unaffected() {
    let _guard = override_bedrock_transport(|_req| {
        Box::pin(async {
            Ok(BedrockTransportOutcome::Error(BedrockTransportError {
                message: VALIDATION_MESSAGE.to_string(),
                status: Some(400),
                body: None,
                error_code: Some("ValidationException".to_string()),
                request_id: Some(REQUEST_ID.to_string()),
                is_service_exception: true,
            }))
        })
    });

    let message = run_bedrock(None).await;
    assert_eq!(
        message.error_message.as_deref(),
        Some(format!("Validation error: {VALIDATION_MESSAGE}").as_str())
    );
}

#[tokio::test]
async fn reports_only_the_request_id_for_modeled_mid_stream_exception() {
    let _guard = override_bedrock_transport(|_req| {
        Box::pin(async {
            Ok(respond_with_failing_stream(BedrockTransportError {
                message: "Too many requests, please wait.".to_string(),
                status: None,
                body: None,
                error_code: None,
                request_id: None,
                is_service_exception: false,
            }))
        })
    });

    let message = run_bedrock(None).await;
    assert_eq!(message.stop_reason, StopReason::Error);

    let mut expected = JsonObject::new();
    expected.insert("requestId".to_string(), JsonValue::String(REQUEST_ID.to_string()));
    assert_eq!(
        find_diagnostic(&message).and_then(|d| d.details.as_ref()),
        Some(&expected)
    );
}

#[tokio::test]
async fn captures_error_code_for_unmodeled_mid_stream_error() {
    let _guard = override_bedrock_transport(|_req| {
        Box::pin(async {
            Ok(respond_with_failing_stream(BedrockTransportError {
                message: "Model stream terminated unexpectedly.".to_string(),
                status: None,
                body: None,
                error_code: Some("ModelStreamErrorException".to_string()),
                request_id: None,
                is_service_exception: false,
            }))
        })
    });

    let message = run_bedrock(None).await;
    let mut expected = JsonObject::new();
    expected.insert(
        "errorCode".to_string(),
        JsonValue::String("ModelStreamErrorException".to_string()),
    );
    expected.insert("requestId".to_string(), JsonValue::String(REQUEST_ID.to_string()));
    assert_eq!(
        find_diagnostic(&message).and_then(|d| d.details.as_ref()),
        Some(&expected)
    );
}

#[tokio::test]
async fn does_not_report_transport_failure_name_as_provider_error_code() {
    let _guard = override_bedrock_transport(|_req| {
        Box::pin(async {
            Ok(respond_with_failing_stream(BedrockTransportError {
                message: "Connection timed out after 1000 ms".to_string(),
                status: None,
                body: None,
                error_code: Some("TimeoutError".to_string()),
                request_id: None,
                is_service_exception: false,
            }))
        })
    });

    let message = run_bedrock(None).await;
    let mut expected = JsonObject::new();
    expected.insert("requestId".to_string(), JsonValue::String(REQUEST_ID.to_string()));
    assert_eq!(
        find_diagnostic(&message).and_then(|d| d.details.as_ref()),
        Some(&expected)
    );
}

#[tokio::test]
async fn emits_no_diagnostic_when_failure_carries_no_provider_metadata() {
    let _guard = override_bedrock_transport(|_req| {
        Box::pin(async {
            Ok(BedrockTransportOutcome::Error(BedrockTransportError {
                message: "socket hang up".to_string(),
                ..BedrockTransportError::default()
            }))
        })
    });

    let message = run_bedrock(None).await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.error_message.as_deref(), Some("socket hang up"));
    assert_eq!(find_diagnostic(&message), None);
}

#[tokio::test]
async fn emits_no_diagnostic_for_aborted_turn() {
    let _guard = override_bedrock_transport(|_req| {
        Box::pin(async {
            Ok(BedrockTransportOutcome::Error(BedrockTransportError {
                message: VALIDATION_MESSAGE.to_string(),
                status: Some(400),
                body: None,
                error_code: Some("ValidationException".to_string()),
                request_id: Some(REQUEST_ID.to_string()),
                is_service_exception: true,
            }))
        })
    });

    let message = run_bedrock(Some(pi_js::AbortSignal::abort(None))).await;
    assert_eq!(message.stop_reason, StopReason::Aborted);
    assert_eq!(find_diagnostic(&message), None);
}

#[tokio::test]
async fn drops_header_derived_values_that_exceed_length_bound() {
    let _guard = override_bedrock_transport(|_req| {
        Box::pin(async {
            Ok(BedrockTransportOutcome::Error(BedrockTransportError {
                message: VALIDATION_MESSAGE.to_string(),
                status: Some(400),
                body: None,
                error_code: Some(format!("{}Exception", "E".repeat(5000))),
                request_id: Some("R".repeat(5000)),
                is_service_exception: true,
            }))
        })
    });

    let message = run_bedrock(None).await;
    let mut expected = JsonObject::new();
    expected.insert("status".to_string(), JsonValue::from(400));
    assert_eq!(
        find_diagnostic(&message).and_then(|d| d.details.as_ref()),
        Some(&expected)
    );
}

#[tokio::test]
async fn omits_sdk_unknown_placeholder_instead_of_reporting_it_as_code() {
    let _guard = override_bedrock_transport(|_req| {
        Box::pin(async {
            Ok(BedrockTransportOutcome::Error(BedrockTransportError {
                message: VALIDATION_MESSAGE.to_string(),
                status: Some(403),
                body: None,
                error_code: Some("Unknown".to_string()),
                request_id: Some(REQUEST_ID.to_string()),
                is_service_exception: true,
            }))
        })
    });

    let message = run_bedrock(None).await;
    let mut expected = JsonObject::new();
    expected.insert("status".to_string(), JsonValue::from(403));
    expected.insert("requestId".to_string(), JsonValue::String(REQUEST_ID.to_string()));
    assert_eq!(
        find_diagnostic(&message).and_then(|d| d.details.as_ref()),
        Some(&expected)
    );
}
