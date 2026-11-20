//! Port of packages/ai/test/anthropic-strict-tool-schema.test.ts

use std::sync::{Arc, Mutex};

use pi_ai::api::anthropic_messages::stream;
use pi_ai::compat::normalize_context;
use pi_ai::types::{
    AnthropicMessagesCompat, ApiStreamOptions, CacheRetention, ConstrainedSampling, ConstrainedSamplingConfig, Context,
    JsonSchemaStrict, Message, Model, ModelCompat, ModelCost, ModelInput, StreamOptions, Tool, UserMessage,
    UserMessageContent,
};
use pi_js::vendor::typebox::{Schema, t};
use pi_js::{Error, time};
use serde_json::{Value, json};

fn create_model() -> Model {
    Model {
        r#type: None,
        id: "claude-opus-4-8".to_owned(),
        name: "Claude Opus 4.8".to_owned(),
        api: "anthropic-messages".to_owned(),
        provider: "test-anthropic".to_owned(),
        base_url: "http://127.0.0.1:9".to_owned(),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 200_000,
        max_tokens: 32_000,
        headers: None,
        sampling_params: None,
        compat: Some(ModelCompat::AnthropicMessages(AnthropicMessagesCompat {
            force_adaptive_thinking: Some(true),
            supports_strict_tools: Some(true),
            ..Default::default()
        })),
        sampling_params_by_thinking_level: None,
    }
}

fn create_tool(parameters: Schema, constrained_sampling: Option<ConstrainedSampling>) -> Tool {
    Tool {
        name: "lookup".to_owned(),
        description: "Look up a value".to_owned(),
        parameters,
        constrained_sampling,
    }
}

fn create_strict_tool(parameters: Schema) -> Tool {
    create_tool(
        parameters,
        Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::JsonSchema {
            strict: JsonSchemaStrict::Prefer,
        })),
    )
}

async fn capture_first_tool(tool: Tool) -> Value {
    let payload = Arc::new(Mutex::new(None));
    let p_c = payload.clone();

    stream(
        &create_model(),
        &normalize_context(&Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Use the tool".to_owned()),
                timestamp: time::now_ms(),
            })],
            tools: Some(vec![tool]),
        }),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("test-key".to_owned()),
                cache_retention: Some(CacheRetention::None),
                on_payload: Some(Arc::new(move |val, _| {
                    let p = p_c.clone();
                    Box::pin(async move {
                        *p.lock().unwrap() = Some(val);
                        Err(Error::js("PayloadCaptured", "payload captured"))
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    let captured = payload.lock().unwrap().take().unwrap();
    captured
        .get("tools")
        .and_then(Value::as_array)
        .unwrap()
        .first()
        .cloned()
        .expect("Expected a tool in the captured Anthropic payload")
}

#[tokio::test]
async fn only_sends_the_full_input_schema_for_strict_json_schema_tools() {
    let legacy_schema = Schema::from(json!({
        "type": "object",
        "properties": { "value": { "type": "string" } },
        "required": ["value"],
        "additionalProperties": false,
        "title": "LookupInput"
    }));
    let legacy_tool = capture_first_tool(create_tool(legacy_schema, None)).await;
    assert!(legacy_tool.get("strict").is_none());
    assert_eq!(
        legacy_tool.get("input_schema"),
        Some(&json!({
            "type": "object",
            "properties": { "value": { "type": "string" } },
            "required": ["value"]
        }))
    );

    let strict_schema = Schema::from(json!({
        "type": "object",
        "properties": {
            "value": { "type": "string" },
            "optional": { "type": "number" }
        },
        "required": ["value"],
        "title": "StrictLookupInput"
    }));
    let strict_tool = capture_first_tool(create_strict_tool(strict_schema)).await;
    assert_eq!(strict_tool.get("strict"), Some(&Value::Bool(true)));
    let input_schema = strict_tool.get("input_schema").unwrap();
    assert_eq!(input_schema.get("additionalProperties"), Some(&Value::Bool(false)));
    assert_eq!(input_schema.get("required"), Some(&json!(["value", "optional"])));
    assert_eq!(
        input_schema.get("properties").and_then(|p| p.get("optional")),
        Some(&json!({ "anyOf": [{ "type": "number" }, { "type": "null" }] }))
    );
    assert_eq!(input_schema.get("title"), Some(&json!("StrictLookupInput")));
}

// https://github.com/earendil-works/pi/issues/9557
#[tokio::test]
async fn keeps_root_definitions_in_non_strict_schemas_so_property_refs_resolve() {
    let settings = json!({
        "type": "object",
        "properties": { "verbose": { "type": "boolean" } }
    });
    for (ref_str, key, val) in [
        ("#/$defs/Settings", "$defs", json!({ "Settings": settings.clone() })),
        ("#/definitions/Settings", "definitions", json!({ "Settings": settings })),
    ] {
        let mut raw = json!({
            "type": "object",
            "properties": { "settings": { "$ref": ref_str } },
            "required": ["settings"],
        });
        raw.as_object_mut().unwrap().insert(key.to_owned(), val.clone());
        let tool = capture_first_tool(create_tool(Schema::from(raw.clone()), None)).await;
        assert!(tool.get("strict").is_none());
        assert_eq!(tool.get("input_schema"), Some(&raw));
    }
}

#[tokio::test]
async fn sends_prefer_tools_non_strict_when_they_use_keywords_anthropic_strict_mode_rejects() {
    let unsupported_parameters = [
        Schema::from(json!({
            "type": "object",
            "properties": {
                "timeoutMs": { "type": "integer", "minimum": 1, "maximum": 300000 }
            }
        })),
        Schema::from(json!({
            "type": "object",
            "properties": {
                "options": {
                    "type": "object",
                    "properties": {
                        "tags": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 2
                        }
                    },
                    "required": ["tags"]
                }
            },
            "required": ["options"]
        })),
        Schema::from(json!({
            "type": "object",
            "properties": {
                "expression": { "type": "string", "format": "regex" }
            },
            "required": ["expression"]
        })),
    ];

    for parameters in unsupported_parameters {
        let tool = capture_first_tool(create_strict_tool(parameters)).await;
        assert!(tool.get("strict").is_none(), "expected non-strict tool, got {tool}");
    }

    let _unused = t::object([("code", t::string())]);
    let supported_tool = capture_first_tool(create_strict_tool(Schema::from(json!({
        "type": "object",
        "properties": {
            "code": { "type": "string", "minLength": 1, "maxLength": 1000, "pattern": "^[a-z]+$" },
            "url": { "type": "string", "format": "uri" },
            "tags": { "type": "array", "items": { "type": "string" }, "minItems": 1 }
        },
        "required": ["code", "url", "tags"]
    }))))
    .await;
    assert_eq!(supported_tool.get("strict"), Some(&Value::Bool(true)));
}
