//! Port of packages/ai/test/bedrock-endpoint-resolution.test.ts

use std::sync::{Arc, Mutex};

use pi_ai::api::bedrock_converse_stream::{
    BedrockClientConfig, BedrockOptions, BedrockTransportError, BedrockTransportOutcome, override_bedrock_transport,
    stream_with_options,
};
use pi_ai::providers::amazon_bedrock_models::amazon_bedrock_models;
use pi_ai::types::{
    CacheRetention, Context, Message, Model, ProviderEnv, StreamOptions, UserMessage, UserMessageContent,
};
use pi_ai::utils::transcript::normalize_context;
use pi_js::env::testing::EnvGuard;
use serial_test::serial;

fn clean_aws_env() -> EnvGuard {
    EnvGuard::set_many(&[
        ("AWS_ACCESS_KEY_ID", None),
        ("AWS_SECRET_ACCESS_KEY", None),
        ("AWS_SESSION_TOKEN", None),
        ("AWS_PROFILE", None),
        ("AWS_REGION", None),
        ("AWS_DEFAULT_REGION", None),
        ("AWS_BEARER_TOKEN_BEDROCK", None),
        ("AWS_BEDROCK_SKIP_AUTH", None),
    ])
}

async fn capture_client_config(model: &Model, options: BedrockOptions) -> BedrockClientConfig {
    let captured: Arc<Mutex<Vec<BedrockClientConfig>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_ref = Arc::clone(&captured);

    let _transport = override_bedrock_transport(move |req| {
        let captured_inner = Arc::clone(&captured_ref);
        Box::pin(async move {
            captured_inner.lock().unwrap().push(req.client_config);
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

    let mut opts = options;
    opts.base.cache_retention = Some(CacheRetention::None);
    let _ = stream_with_options(model, &context, Some(opts)).result().await;

    let calls = captured.lock().unwrap();
    assert_eq!(calls.len(), 1);
    calls[0].clone()
}

#[test]
fn assigns_eu_central_1_runtime_urls_to_built_in_eu_inference_profiles() {
    let model = amazon_bedrock_models()
        .get("eu.anthropic.claude-sonnet-4-5-20250929-v1:0")
        .expect("EU Claude Sonnet 4.5 model");

    assert_eq!(model.base_url, "https://bedrock-runtime.eu-central-1.amazonaws.com");
}

#[tokio::test]
#[serial]
async fn does_not_pin_standard_aws_endpoints_when_aws_region_is_configured() {
    let mut env = clean_aws_env();
    env.stub("AWS_REGION", Some("us-east-2"));
    let model = amazon_bedrock_models()
        .get("us.anthropic.claude-opus-4-8")
        .expect("us.anthropic.claude-opus-4-8 model");

    let config = capture_client_config(model, BedrockOptions::default()).await;

    assert_eq!(config.region.as_deref(), Some("us-east-2"));
    assert_eq!(config.endpoint, None);
}

#[tokio::test]
#[serial]
async fn derives_region_from_built_in_eu_endpoint_when_no_region_or_profile_configured() {
    let _env = clean_aws_env();
    let model = amazon_bedrock_models()
        .get("eu.anthropic.claude-sonnet-4-5-20250929-v1:0")
        .expect("EU Claude Sonnet 4.5 model");

    let config = capture_client_config(model, BedrockOptions::default()).await;

    assert_eq!(
        config.endpoint.as_deref(),
        Some("https://bedrock-runtime.eu-central-1.amazonaws.com")
    );
    assert_eq!(config.region.as_deref(), Some("eu-central-1"));
}

#[tokio::test]
#[serial]
async fn handles_missing_regions_for_explicit_scoped_and_ambient_profiles() {
    let mut env = clean_aws_env();
    let model = amazon_bedrock_models()
        .get("eu.anthropic.claude-sonnet-4-5-20250929-v1:0")
        .expect("EU Claude Sonnet 4.5 model");

    let config = capture_client_config(
        model,
        BedrockOptions {
            profile: Some("bedrock-profile".to_string()),
            ..BedrockOptions::default()
        },
    )
    .await;
    assert_eq!(config.profile.as_deref(), Some("bedrock-profile"));
    assert_eq!(
        config.endpoint.as_deref(),
        Some("https://bedrock-runtime.eu-central-1.amazonaws.com")
    );
    assert_eq!(config.region.as_deref(), Some("eu-central-1"));

    let mut scoped_env = ProviderEnv::new();
    scoped_env.insert("AWS_PROFILE".to_string(), "scoped-bedrock-profile".to_string());
    let config = capture_client_config(
        model,
        BedrockOptions {
            base: StreamOptions {
                env: Some(scoped_env),
                ..StreamOptions::default()
            },
            ..BedrockOptions::default()
        },
    )
    .await;
    assert_eq!(config.profile.as_deref(), Some("scoped-bedrock-profile"));
    assert_eq!(
        config.endpoint.as_deref(),
        Some("https://bedrock-runtime.eu-central-1.amazonaws.com")
    );
    assert_eq!(config.region.as_deref(), Some("eu-central-1"));

    env.stub("AWS_PROFILE", Some("ambient-bedrock-profile"));
    let config = capture_client_config(model, BedrockOptions::default()).await;
    assert_eq!(config.profile.as_deref(), Some("ambient-bedrock-profile"));
    assert_eq!(config.endpoint, None);
    assert_eq!(config.region, None);
}

#[tokio::test]
#[serial]
async fn still_passes_custom_bedrock_endpoints_through_to_sdk_client() {
    let mut env = clean_aws_env();
    env.stub("AWS_REGION", Some("us-west-2"));
    let mut model = amazon_bedrock_models()
        .get("us.anthropic.claude-opus-4-8")
        .expect("us.anthropic.claude-opus-4-8 model")
        .clone();
    model.base_url = "https://bedrock-vpc.example.com".to_string();

    let config = capture_client_config(&model, BedrockOptions::default()).await;

    assert_eq!(config.endpoint.as_deref(), Some("https://bedrock-vpc.example.com"));
    assert_eq!(config.region.as_deref(), Some("us-west-2"));
}

#[tokio::test]
#[serial]
async fn extracts_region_from_inference_profile_arn_regardless_of_aws_region() {
    let mut env = clean_aws_env();
    env.stub("AWS_REGION", Some("us-east-1"));
    let mut model = amazon_bedrock_models()
        .get("us.anthropic.claude-opus-4-8")
        .expect("us.anthropic.claude-opus-4-8 model")
        .clone();
    model.id = "arn:aws:bedrock:us-west-2:123456789012:application-inference-profile/abc123".to_string();

    let config = capture_client_config(&model, BedrockOptions::default()).await;

    assert_eq!(config.region.as_deref(), Some("us-west-2"));
}

#[tokio::test]
#[serial]
async fn extracts_region_from_govcloud_inference_profile_arn() {
    let mut env = clean_aws_env();
    env.stub("AWS_REGION", Some("us-east-1"));
    let mut model = amazon_bedrock_models()
        .get("us.anthropic.claude-opus-4-8")
        .expect("us.anthropic.claude-opus-4-8 model")
        .clone();
    model.id = "arn:aws-us-gov:bedrock:us-gov-west-1:123456789012:application-inference-profile/abc123".to_string();

    let config = capture_client_config(&model, BedrockOptions::default()).await;

    assert_eq!(config.region.as_deref(), Some("us-gov-west-1"));
}

#[tokio::test]
#[serial]
async fn preserves_ambient_aws_auth_for_custom_model_ids_through_compat_dispatch() {
    let mut env = clean_aws_env();
    env.stub("AWS_PROFILE", Some("bedrock-profile"));
    let mut model = amazon_bedrock_models()
        .get("us.anthropic.claude-opus-4-8")
        .expect("us.anthropic.claude-opus-4-8 model")
        .clone();
    model.id = "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/example".to_string();

    let config = capture_client_config(&model, BedrockOptions::default()).await;

    assert_eq!(config.profile.as_deref(), Some("bedrock-profile"));
    assert_eq!(config.bearer_token, None);
    assert_eq!(config.auth_scheme_preference, None);
}

#[tokio::test]
#[serial]
async fn uses_generic_api_key_option_as_bedrock_bearer_token() {
    let _env = clean_aws_env();
    let model = amazon_bedrock_models()
        .get("us.anthropic.claude-opus-4-8")
        .expect("us.anthropic.claude-opus-4-8 model");

    let config = capture_client_config(
        model,
        BedrockOptions {
            base: StreamOptions {
                api_key: Some("bedrock-api-key".to_string()),
                ..StreamOptions::default()
            },
            ..BedrockOptions::default()
        },
    )
    .await;

    assert_eq!(config.bearer_token.as_deref(), Some("bedrock-api-key"));
    assert_eq!(config.auth_scheme_preference, Some(vec!["httpBearerAuth".to_string()]));
}
