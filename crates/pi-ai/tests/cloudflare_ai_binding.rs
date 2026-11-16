//! Port of packages/ai/test/cloudflare-ai-binding.test.ts

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures::stream;
use indexmap::IndexMap;
use serde_json::json;

use pi_ai::api::cloudflare_ai_binding::{AiBinding, CLOUDFLARE_GATEWAY_BINDING_AUTH_SENTINEL, create_ai_binding_fetch};
use pi_ai::api::openai_completions::stream_simple as stream_openai_completions;
use pi_ai::types::{
    Context, FetchFunction, Message, Model, ModelCost, ModelModality, ProviderHeaders, SimpleStreamOptions, StopReason,
    StreamOptions, UserMessage, UserMessageContent,
};
use pi_ai::utils::transcript::normalize_context;
use pi_js::fetch::{Body, Headers, Request, Response};

const BINDING_PREFIX: &str = "https://workers-binding.ai/ai-gateway/gateways/my-gateway";

fn fake_binding(response: Option<Response>) -> (AiBinding, Arc<Mutex<Vec<Request>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let response_slot = Arc::new(Mutex::new(response));
    let fetch_fn: FetchFunction = Arc::new(move |req: Request| {
        let captured = Arc::clone(&captured);
        let response_slot = Arc::clone(&response_slot);
        Box::pin(async move {
            captured.lock().unwrap().push(req);
            let resp = response_slot
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| Response::new(200, Headers::new(), "{}"));
            Ok(resp)
        })
    });
    let binding = AiBinding {
        ai_gateway_log_id: None,
        fetch: Some(fetch_fn),
    };
    (binding, requests)
}

#[tokio::test]
async fn passes_requests_to_the_binding_untouched() {
    let stream = stream::iter([Ok::<Bytes, pi_js::Error>(Bytes::from_static(b"data: {}\n\n"))]);
    let binding_response = Response::from_stream(
        200,
        Headers::from_pairs([("content-type", "text/event-stream"), ("cf-aig-log-id", "log-1")]),
        stream,
    );
    let (binding, requests) = fake_binding(Some(binding_response));
    let fetch_fn = create_ai_binding_fetch(&binding).unwrap();

    let body = pi_js::json::stringify(&json!({
        "model": "claude",
        "messages": [{ "role": "user", "content": "hi" }]
    }));
    let mut headers = Headers::new();
    headers.set("content-type", "application/json");
    headers.set(
        "cf-aig-authorization",
        &format!("Bearer {CLOUDFLARE_GATEWAY_BINDING_AUTH_SENTINEL}"),
    );
    headers.set("anthropic-version", "2023-06-01");

    let req = Request::post(format!("{BINDING_PREFIX}/anthropic/v1/messages?beta=true"))
        .with_headers(headers)
        .body(Body::Text(body.clone()));
    let response = fetch_fn(req).await.unwrap();

    let recorded = requests.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].url,
        format!("{BINDING_PREFIX}/anthropic/v1/messages?beta=true")
    );
    assert_eq!(recorded[0].method, "POST");
    let mut expected_headers = IndexMap::new();
    expected_headers.insert("anthropic-version".to_owned(), "2023-06-01".to_owned());
    expected_headers.insert(
        "cf-aig-authorization".to_owned(),
        format!("Bearer {CLOUDFLARE_GATEWAY_BINDING_AUTH_SENTINEL}"),
    );
    expected_headers.insert("content-type".to_owned(), "application/json".to_owned());
    let actual_headers: IndexMap<String, String> = recorded[0].headers.entries().into_iter().collect();
    assert_eq!(actual_headers, expected_headers);
    assert_eq!(recorded[0].body_text().as_deref(), Some(body.as_str()));
    assert_eq!(response.headers().get("cf-aig-log-id").as_deref(), Some("log-1"));
    assert_eq!(response.text().await.unwrap(), "data: {}\n\n");
}

#[test]
fn rejects_a_binding_with_no_fetch_at_construction_not_on_first_request() {
    let err = match create_ai_binding_fetch(&AiBinding {
        ai_gateway_log_id: None,
        fetch: None,
    }) {
        Ok(_) => panic!("expected create_ai_binding_fetch to fail without fetch()"),
        Err(err) => err,
    };
    assert!(
        err.to_string().contains("does not expose fetch()"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn keeps_sdk_placeholder_auth_off_the_wire_when_paired_with_null_auth_headers() {
    let (binding, requests) = fake_binding(Some(Response::new(
        400,
        Headers::from_pairs([("content-type", "application/json")]),
        r#"{"error":{"type":"bad_request","message":"stubbed"}}"#,
    )));

    let model = Model {
        r#type: None,
        id: "test-model".to_owned(),
        name: "Test Model".to_owned(),
        api: "openai-completions".to_owned(),
        provider: "openai".to_owned(),
        base_url: format!("{BINDING_PREFIX}/openai"),
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
        context_window: 10_000,
        max_tokens: 1_000,
        headers: None,
        sampling_params: None,
        compat: None,
    };

    let context = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hello".to_owned()),
            timestamp: 1,
        })],
        tools: None,
    });

    let mut headers = ProviderHeaders::new();
    headers.insert(
        "cf-aig-authorization".to_owned(),
        Some(format!("Bearer {CLOUDFLARE_GATEWAY_BINDING_AUTH_SENTINEL}")),
    );
    headers.insert("Authorization".to_owned(), None);
    headers.insert("x-api-key".to_owned(), None);

    let result = stream_openai_completions(
        &model,
        &context,
        Some(SimpleStreamOptions {
            base: StreamOptions {
                headers: Some(headers),
                fetch: Some(create_ai_binding_fetch(&binding).unwrap()),
                max_retries: Some(0),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(result.stop_reason, StopReason::Error);
    let recorded = requests.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].url, format!("{BINDING_PREFIX}/openai/chat/completions"));
    assert!(!recorded[0].headers.has("authorization"));
    assert!(!recorded[0].headers.has("x-api-key"));
}
