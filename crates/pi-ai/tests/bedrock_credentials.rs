//! Port of packages/ai/test/bedrock-credentials.test.ts

use std::sync::{Arc, Mutex};

use pi_ai::api::bedrock_converse_stream::{
    BedrockClientConfig, BedrockCredentials, BedrockOptions, BedrockTransportError, BedrockTransportOutcome,
    override_bedrock_transport, stream_with_options,
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

#[tokio::test]
#[serial]
async fn prefers_explicit_and_scoped_profiles_over_ambient_aws_access_keys() {
    let mut env = clean_aws_env();
    env.stub("AWS_ACCESS_KEY_ID", Some("AKIAEXAMPLE"));
    env.stub("AWS_SECRET_ACCESS_KEY", Some("secretexample"));

    let model = amazon_bedrock_models()
        .get("us.anthropic.claude-opus-4-8")
        .expect("us.anthropic.claude-opus-4-8 model");

    let config = capture_client_config(
        model,
        BedrockOptions {
            profile: Some("explicit-profile".to_string()),
            ..BedrockOptions::default()
        },
    )
    .await;
    assert_eq!(config.profile.as_deref(), Some("explicit-profile"));
    assert_eq!(config.credentials, None);

    let mut scoped_env = ProviderEnv::new();
    scoped_env.insert("AWS_PROFILE".to_string(), "scoped-profile".to_string());
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
    assert_eq!(config.profile.as_deref(), Some("scoped-profile"));
    assert_eq!(config.credentials, None);
}

#[tokio::test]
#[serial]
async fn uses_ambient_aws_access_keys_when_no_profile_is_configured() {
    let mut env = clean_aws_env();
    env.stub("AWS_ACCESS_KEY_ID", Some("AKIAEXAMPLE"));
    env.stub("AWS_SECRET_ACCESS_KEY", Some("secretexample"));

    let model = amazon_bedrock_models()
        .get("us.anthropic.claude-opus-4-8")
        .expect("us.anthropic.claude-opus-4-8 model");

    let config = capture_client_config(model, BedrockOptions::default()).await;
    assert_eq!(config.profile, None);
    assert_eq!(
        config.credentials,
        Some(BedrockCredentials {
            access_key_id: "AKIAEXAMPLE".to_string(),
            secret_access_key: "secretexample".to_string(),
            session_token: None,
        })
    );
}

#[tokio::test]
#[serial]
async fn uses_ambient_aws_access_keys_when_only_ambient_profile_is_set() {
    let mut env = clean_aws_env();
    env.stub("AWS_ACCESS_KEY_ID", Some("AKIAEXAMPLE"));
    env.stub("AWS_SECRET_ACCESS_KEY", Some("secretexample"));
    env.stub("AWS_PROFILE", Some("ambient-profile"));

    let model = amazon_bedrock_models()
        .get("us.anthropic.claude-opus-4-8")
        .expect("us.anthropic.claude-opus-4-8 model");

    let config = capture_client_config(model, BedrockOptions::default()).await;
    assert_eq!(config.profile.as_deref(), Some("ambient-profile"));
    assert_eq!(
        config.credentials,
        Some(BedrockCredentials {
            access_key_id: "AKIAEXAMPLE".to_string(),
            secret_access_key: "secretexample".to_string(),
            session_token: None,
        })
    );
}
