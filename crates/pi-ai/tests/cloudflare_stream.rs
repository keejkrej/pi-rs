//! Port of packages/ai/test/cloudflare-stream.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;

use pi_ai::providers::cloudflare_stream::{cloudflare_classifier, cloudflare_streams, resolve_cloudflare_model};
use pi_ai::types::{
    ClassifierContext, ClassifierModel, ClassifierOptions, ClassifierResult, ClassifierStopReason, Context, Model,
    ModelCost, ModelModality, ProviderClassifier, ProviderEnv, ProviderStreams, SimpleStreamOptions, StreamOptions,
};
use pi_ai::utils::event_stream::AssistantMessageEventStream;
use pi_ai::utils::transcript::normalize_context;

fn sample_model() -> Model {
    Model {
        r#type: None,
        id: "model".to_owned(),
        name: "model".to_owned(),
        api: "openai-completions".to_owned(),
        provider: "cloudflare-ai-gateway".to_owned(),
        base_url: "https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/openai"
            .to_owned(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelModality::Text],
        input_limits: None,
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        context_window: 1000,
        max_tokens: 100,
        headers: None,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    }
}

#[test]
fn materializes_the_model_endpoint_before_dispatch() {
    let model = sample_model();
    let context = normalize_context(&Context::default());
    let captured = Arc::new(Mutex::new(Vec::<String>::new()));

    let c1 = Arc::clone(&captured);
    let c2 = Arc::clone(&captured);
    let streams = cloudflare_streams(ProviderStreams {
        stream: Arc::new(move |request_model, _ctx, _opts| {
            c1.lock().unwrap().push(request_model.base_url.clone());
            AssistantMessageEventStream::new()
        }),
        stream_simple: Arc::new(move |request_model, _ctx, _opts| {
            c2.lock().unwrap().push(request_model.base_url.clone());
            AssistantMessageEventStream::new()
        }),
        fetch_deferred: None,
        cancel_deferred: None,
    });

    let mut env = ProviderEnv::new();
    env.insert("CLOUDFLARE_ACCOUNT_ID".to_owned(), "account".to_owned());
    env.insert("CLOUDFLARE_GATEWAY_ID".to_owned(), "gateway".to_owned());

    streams.stream(
        &model,
        &context,
        Some(
            StreamOptions {
                env: Some(env.clone()),
                ..Default::default()
            }
            .into(),
        ),
    );
    streams.stream_simple(
        &model,
        &context,
        Some(SimpleStreamOptions {
            base: StreamOptions {
                env: Some(env),
                ..Default::default()
            },
            ..Default::default()
        }),
    );

    assert_eq!(
        *captured.lock().unwrap(),
        vec![
            "https://gateway.ai.cloudflare.com/v1/account/gateway/openai".to_owned(),
            "https://gateway.ai.cloudflare.com/v1/account/gateway/openai".to_owned(),
        ]
    );
}

#[test]
fn keeps_placeholders_when_the_provider_env_does_not_resolve_them() {
    let model = sample_model();
    let context = normalize_context(&Context::default());
    let captured = Arc::new(Mutex::new(None::<String>));

    let c1 = Arc::clone(&captured);
    let c2 = Arc::clone(&captured);
    let streams = cloudflare_streams(ProviderStreams {
        stream: Arc::new(move |request_model, _ctx, _opts| {
            *c1.lock().unwrap() = Some(request_model.base_url.clone());
            AssistantMessageEventStream::new()
        }),
        stream_simple: Arc::new(move |request_model, _ctx, _opts| {
            *c2.lock().unwrap() = Some(request_model.base_url.clone());
            AssistantMessageEventStream::new()
        }),
        fetch_deferred: None,
        cancel_deferred: None,
    });

    streams.stream_simple(&model, &context, Some(SimpleStreamOptions::default()));

    assert_eq!(captured.lock().unwrap().as_deref(), Some(model.base_url.as_str()));
    assert_eq!(resolve_cloudflare_model(&model, None).base_url, model.base_url);
}

#[tokio::test]
async fn cloudflare_classifier_materializes_account_placeholder() {
    let model = ClassifierModel {
        id: "typesafe/jev".to_owned(),
        name: "Jev".to_owned(),
        api: "cloudflare-workers-ai-system-one".to_owned(),
        provider: "cloudflare-workers-ai".to_owned(),
        base_url: "https://api.cloudflare.com/client/v4/accounts/{CLOUDFLARE_ACCOUNT_ID}/ai".to_owned(),
        input: vec![ModelModality::Text],
        input_limits: None,
        context_window: 32000,
        cost: ModelCost::default(),
        headers: None,
        compat: None,
    };
    let captured = Arc::new(Mutex::new(None::<String>));
    let c = Arc::clone(&captured);
    let wrapped = cloudflare_classifier(ProviderClassifier {
        classify: Arc::new(move |req_model, _ctx, _opts| {
            *c.lock().unwrap() = Some(req_model.base_url.clone());
            let model_clone = req_model.clone();
            Box::pin(async move {
                ClassifierResult {
                    api: model_clone.api,
                    provider: model_clone.provider,
                    model: model_clone.id,
                    answers: IndexMap::new(),
                    usage: None,
                    stop_reason: ClassifierStopReason::Stop,
                    error_message: None,
                    timestamp: 0,
                }
            })
        }),
    });

    let mut env = ProviderEnv::new();
    env.insert("CLOUDFLARE_ACCOUNT_ID".to_owned(), "acct-123".to_owned());
    let mut opts = ClassifierOptions::default();
    opts.base.env = Some(env);
    let ctx = ClassifierContext {
        state: IndexMap::new(),
        questions: IndexMap::new(),
        images: None,
    };
    wrapped.classify(&model, &ctx, Some(opts)).await;
    assert_eq!(
        captured.lock().unwrap().as_deref(),
        Some("https://api.cloudflare.com/client/v4/accounts/acct-123/ai")
    );
}
