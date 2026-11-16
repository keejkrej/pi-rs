//! Port of packages/ai/test/bedrock-custom-headers.test.ts

use std::sync::{Arc, Mutex};

use aws_sdk_bedrockruntime::config::Intercept;
use aws_sdk_bedrockruntime::config::http::HttpRequest;
use aws_smithy_types::body::SdkBody;
use indexmap::IndexMap;
use pi_ai::api::bedrock_converse_stream::{
    BedrockCustomHeadersInterceptor, BedrockOptions, BedrockTransportError, BedrockTransportOutcome,
    BedrockTransportRequest, CUSTOM_HEADERS_INTERCEPTOR_NAME, override_bedrock_transport, stream_simple,
    stream_with_options,
};
use pi_ai::providers::amazon_bedrock_models::amazon_bedrock_models;
use pi_ai::types::{
    CacheRetention, Context, Message, ProviderHeaders, SimpleStreamOptions, StreamOptions, UserMessage,
    UserMessageContent,
};
use pi_ai::utils::transcript::normalize_context;

fn model_fixture() -> pi_ai::types::Model {
    amazon_bedrock_models()
        .get("us.anthropic.claude-opus-4-8")
        .expect("us.anthropic.claude-opus-4-8 model")
        .clone()
}

async fn drive_bedrock(options: BedrockOptions) -> Vec<BedrockTransportRequest> {
    let requests: Arc<Mutex<Vec<BedrockTransportRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let requests_ref = Arc::clone(&requests);

    let _transport = override_bedrock_transport(move |req| {
        let requests_inner = Arc::clone(&requests_ref);
        Box::pin(async move {
            requests_inner.lock().unwrap().push(req);
            Ok(BedrockTransportOutcome::Error(BedrockTransportError {
                message: "mock send".to_string(),
                ..BedrockTransportError::default()
            }))
        })
    });

    let context = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hello".to_string()),
            timestamp: 1,
        })],
        tools: None,
    });
    let _ = stream_with_options(&model_fixture(), &context, Some(options))
        .result()
        .await;
    requests.lock().unwrap().clone()
}

#[tokio::test]
async fn vc1_registers_interceptor_that_injects_caller_header() {
    let mut headers = ProviderHeaders::new();
    headers.insert("x-custom".to_string(), Some("v".to_string()));

    let requests = drive_bedrock(BedrockOptions {
        base: StreamOptions {
            cache_retention: Some(CacheRetention::None),
            headers: Some(headers),
            ..StreamOptions::default()
        },
        ..BedrockOptions::default()
    })
    .await;

    assert_eq!(requests.len(), 1);
    let custom_registrations: Vec<&String> = requests[0]
        .interceptor_names
        .iter()
        .filter(|name| name.as_str() == CUSTOM_HEADERS_INTERCEPTOR_NAME)
        .collect();
    assert_eq!(custom_registrations.len(), 1);

    let custom_map = requests[0].custom_headers.clone().expect("custom headers recorded");
    let interceptor = BedrockCustomHeadersInterceptor::new(custom_map);
    assert_eq!(interceptor.name(), CUSTOM_HEADERS_INTERCEPTOR_NAME);

    let mut req = HttpRequest::new(SdkBody::empty());
    interceptor
        .apply_to_http_request(&mut req)
        .expect("apply custom headers");
    assert_eq!(req.headers().get("x-custom"), Some("v"));
}

#[tokio::test]
async fn vc2_skips_reserved_headers_case_insensitively_while_applying_allowed_ones() {
    let mut headers = ProviderHeaders::new();
    headers.insert("authorization".to_string(), Some("evil".to_string()));
    headers.insert("x-amz-date".to_string(), Some("evil".to_string()));
    headers.insert("x-allowed".to_string(), Some("ok".to_string()));
    headers.insert("Authorization".to_string(), Some("evil2".to_string()));
    headers.insert("X-Amz-Date".to_string(), Some("evil2".to_string()));
    headers.insert("HOST".to_string(), Some("evil3".to_string()));

    let requests = drive_bedrock(BedrockOptions {
        base: StreamOptions {
            cache_retention: Some(CacheRetention::None),
            headers: Some(headers),
            ..StreamOptions::default()
        },
        ..BedrockOptions::default()
    })
    .await;

    let custom_map = requests[0].custom_headers.clone().expect("custom headers recorded");
    let interceptor = BedrockCustomHeadersInterceptor::new(custom_map);

    let mut req = HttpRequest::new(SdkBody::empty());
    req.headers_mut().insert("authorization", "real-auth");
    req.headers_mut().insert("x-amz-date", "real-date");
    req.headers_mut().insert("host", "real-host");

    interceptor
        .apply_to_http_request(&mut req)
        .expect("apply custom headers");

    assert_eq!(req.headers().get("authorization"), Some("real-auth"));
    assert_eq!(req.headers().get("x-amz-date"), Some("real-date"));
    assert_eq!(req.headers().get("host"), Some("real-host"));
    assert_eq!(req.headers().get("x-allowed"), Some("ok"));

    let mut keys: Vec<String> = req.headers().iter().map(|(k, _)| k.to_string()).collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "authorization".to_string(),
            "host".to_string(),
            "x-allowed".to_string(),
            "x-amz-date".to_string(),
        ]
    );
}

#[tokio::test]
async fn vc3_registers_no_interceptor_when_headers_none_or_empty() {
    let requests_none = drive_bedrock(BedrockOptions {
        base: StreamOptions {
            cache_retention: Some(CacheRetention::None),
            headers: None,
            ..StreamOptions::default()
        },
        ..BedrockOptions::default()
    })
    .await;
    assert_eq!(requests_none.len(), 1);
    assert!(
        !requests_none[0]
            .interceptor_names
            .iter()
            .any(|name| name == CUSTOM_HEADERS_INTERCEPTOR_NAME)
    );

    let requests_empty = drive_bedrock(BedrockOptions {
        base: StreamOptions {
            cache_retention: Some(CacheRetention::None),
            headers: Some(ProviderHeaders::new()),
            ..StreamOptions::default()
        },
        ..BedrockOptions::default()
    })
    .await;
    assert_eq!(requests_empty.len(), 1);
    assert!(
        !requests_empty[0]
            .interceptor_names
            .iter()
            .any(|name| name == CUSTOM_HEADERS_INTERCEPTOR_NAME)
    );
}

#[tokio::test]
async fn vc4_stream_simple_forwards_headers_end_to_end() {
    let requests: Arc<Mutex<Vec<BedrockTransportRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let requests_ref = Arc::clone(&requests);

    let _transport = override_bedrock_transport(move |req| {
        let requests_inner = Arc::clone(&requests_ref);
        Box::pin(async move {
            requests_inner.lock().unwrap().push(req);
            Ok(BedrockTransportOutcome::Error(BedrockTransportError {
                message: "mock send".to_string(),
                ..BedrockTransportError::default()
            }))
        })
    });

    let mut headers = ProviderHeaders::new();
    headers.insert("x-custom".to_string(), Some("v".to_string()));

    let context = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hello".to_string()),
            timestamp: 1,
        })],
        tools: None,
    });

    let _ = stream_simple(
        &model_fixture(),
        &context,
        Some(SimpleStreamOptions {
            base: StreamOptions {
                headers: Some(headers),
                ..StreamOptions::default()
            },
            tool_choice: None,
            reasoning: None,
            deferred: None,
            thinking_budgets: None,
        }),
    )
    .result()
    .await;

    let captured = requests.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert!(
        captured[0]
            .interceptor_names
            .iter()
            .any(|name| name == CUSTOM_HEADERS_INTERCEPTOR_NAME)
    );
    let mut expected = IndexMap::new();
    expected.insert("x-custom".to_string(), "v".to_string());
    assert_eq!(captured[0].custom_headers.as_ref(), Some(&expected));
}
