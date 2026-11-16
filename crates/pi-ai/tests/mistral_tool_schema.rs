//! Port of packages/ai/test/mistral-tool-schema.test.ts

use std::sync::{Arc, Mutex};

use pi_ai::api::mistral_conversations::{MistralOptions, stream_with_options};
use pi_ai::providers::mistral_models::mistral_models;
use pi_ai::utils::transcript::normalize_context;
use pi_ai::{
    ConstrainedSamplingConfig, Context, FetchFunction, JsonSchemaStrict, Message, Model, OnPayloadFn, StopReason,
    StreamOptions, Tool, Type, UserMessage,
};
use pi_js::{BoxFuture, Error};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

#[tokio::test]
async fn serializes_typebox_tool_schemas_cleanly_before_transport() {
    let mut model = mistral_models()
        .get("devstral-medium-latest")
        .expect("devstral-medium-latest should exist in mistral catalog")
        .clone();
    model.base_url = "http://127.0.0.1:9".to_owned();

    let parameters = Type::object([("nested", Type::object([("value", Type::string())]))]);

    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: "Hi".into(),
            timestamp: pi_js::time::now_ms(),
        })],
        tools: Some(vec![Tool {
            name: "inspect_schema".to_owned(),
            description: "Inspect the schema".to_owned(),
            parameters,
            constrained_sampling: Some(
                ConstrainedSamplingConfig::JsonSchema {
                    strict: JsonSchemaStrict::Require,
                }
                .into(),
            ),
        }]),
    };

    let captured_payload: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&captured_payload);
    let on_payload: OnPayloadFn<Model> = Arc::new(move |payload, _model| {
        let s = Arc::clone(&slot);
        Box::pin(async move {
            *s.lock().unwrap_or_else(|p| p.into_inner()) = Some(payload.clone());
            Ok(Some(payload))
        }) as BoxFuture<pi_js::Result<Option<Value>>>
    });

    // PORT: Rust `TSchema::to_value()` has no JS symbol keys, and fetch fails immediately rather than dialing port 9.
    let fail_fetch: FetchFunction =
        Arc::new(|_req| Box::pin(async { Err(Error::msg("connect ECONNREFUSED 127.0.0.1:9")) }));

    let opts = MistralOptions {
        base: StreamOptions {
            api_key: Some("fake-key".to_owned()),
            on_payload: Some(on_payload),
            fetch: Some(fail_fetch),
            ..Default::default()
        },
        ..Default::default()
    };

    let response = stream_with_options(&model, &normalize_context(&context), Some(opts))
        .result()
        .await;

    let payload = captured_payload
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
        .expect("payload should be captured");
    let tools = payload["tools"].as_array().expect("tools array expected");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["function"]["strict"], json!(true));

    let payload_params = tools[0]["function"]["parameters"]
        .as_object()
        .expect("parameters object expected");
    assert_eq!(payload_params["type"], json!("object"));
    assert_eq!(payload_params["additionalProperties"], json!(false));
    let nested = &payload_params["properties"]["nested"];
    assert_eq!(nested["type"], json!("object"));
    assert_eq!(nested["additionalProperties"], json!(false));
    assert_eq!(nested["properties"]["value"]["type"], json!("string"));

    assert_eq!(response.stop_reason, StopReason::Error);
    assert!(
        !response
            .error_message
            .as_deref()
            .is_some_and(|m| m.contains("Input validation failed")),
        "unexpected validation error: {:?}",
        response.error_message
    );
}
