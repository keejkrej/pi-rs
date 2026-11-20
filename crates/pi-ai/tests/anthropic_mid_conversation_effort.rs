//! Port of packages/ai/test/anthropic-mid-conversation-effort.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::api::anthropic_messages::{AnthropicEffort, AnthropicOptions, stream_with_options};
use pi_ai::compat::{get_model, normalize_context};
use pi_ai::types::{
    AnthropicMessagesCompat, AssistantContentBlock, AssistantMessage, CacheRetention, Context, Message, Model,
    ModelCompat, ModelCost, ModelInput, ModelThinkingLevel, StopReason, StreamOptions, TextContent, ThinkingContent,
    ThinkingLevelMap, Usage, UserMessage, UserMessageContent,
};
use pi_js::Error;
use pi_js::fetch::{Headers, Request, Response};
use serde_json::{Value, json};

fn managed_model(provider: &str) -> Model {
    let mut level_map: ThinkingLevelMap = IndexMap::new();
    level_map.insert(ModelThinkingLevel::Off, None);
    level_map.insert(ModelThinkingLevel::Minimal, Some("low".to_owned()));
    level_map.insert(ModelThinkingLevel::Low, Some("low".to_owned()));
    level_map.insert(ModelThinkingLevel::Medium, Some("medium".to_owned()));
    level_map.insert(ModelThinkingLevel::High, Some("high".to_owned()));
    level_map.insert(ModelThinkingLevel::Max, Some("max".to_owned()));

    Model {
        r#type: None,
        id: "claude-fable-5-1".to_owned(),
        name: "Claude Fable 5.1".to_owned(),
        api: "anthropic-messages".to_owned(),
        provider: provider.to_owned(),
        base_url: "http://127.0.0.1:9".to_owned(),
        reasoning: true,
        thinking_level_map: Some(level_map),
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
            supports_mid_convo_effort: Some(true),
            ..Default::default()
        })),
        sampling_params_by_thinking_level: None,
    }
}

fn assistant(model: &Model, level: Option<&str>) -> Message {
    Message::Assistant(AssistantMessage {
        content: vec![
            AssistantContentBlock::Thinking(ThinkingContent {
                thinking: "reasoning".to_owned(),
                thinking_signature: Some("signature".to_owned()),
                redacted: None,
            }),
            AssistantContentBlock::Text(TextContent::new("answer")),
        ],
        api: "anthropic-messages".to_owned(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: level.map(str::to_owned),
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1,
        duration_ms: None,
    })
}

fn user(text: &str, timestamp: i64) -> Message {
    Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_owned()),
        timestamp,
    })
}

async fn capture_with(
    model: &Model,
    context: Context,
    effort: Option<AnthropicEffort>,
    thinking_enabled: Option<bool>,
) -> (Value, AssistantMessage) {
    let payload = Arc::new(Mutex::new(None));
    let p_c = payload.clone();

    let message = stream_with_options(
        model,
        &normalize_context(&context),
        Some(AnthropicOptions {
            base: StreamOptions {
                api_key: Some("test-key".to_owned()),
                cache_retention: Some(CacheRetention::None),
                on_payload: Some(Arc::new(move |value, _| {
                    let p = p_c.clone();
                    Box::pin(async move {
                        *p.lock().unwrap() = Some(value);
                        Err(Error::msg("payload captured"))
                    })
                })),
                ..Default::default()
            },
            thinking_enabled,
            effort,
            ..Default::default()
        }),
    )
    .result()
    .await;

    let captured = payload.lock().unwrap().take().expect("Expected payload capture");
    (captured, message)
}

async fn capture(model: &Model, context: Context, effort: Option<AnthropicEffort>) -> (Value, AssistantMessage) {
    capture_with(model, context, effort, Some(true)).await
}

fn effort_messages(payload: &Value) -> Vec<Value> {
    payload
        .get("messages")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("system"))
        .cloned()
        .collect()
}

#[tokio::test]
async fn reconstructs_an_exact_historical_marker_prefix_and_appends_the_current_marker() {
    let model = managed_model("anthropic");
    let (first_payload, first_msg) = capture(
        &model,
        Context {
            system_prompt: None,
            messages: vec![user("one", 1)],
            tools: None,
        },
        Some(AnthropicEffort::Low),
    )
    .await;
    let (second_payload, _) = capture(
        &model,
        Context {
            system_prompt: None,
            messages: vec![user("one", 1), assistant(&model, Some("low")), user("two", 2)],
            tools: None,
        },
        Some(AnthropicEffort::High),
    )
    .await;

    let first_msgs = first_payload.get("messages").and_then(Value::as_array).unwrap();
    assert_eq!(
        *first_msgs,
        vec![
            json!({ "role": "user", "content": "one" }),
            json!({ "role": "system", "content": [], "output_config": { "effort": "low" } }),
        ]
    );
    let second_msgs = second_payload.get("messages").and_then(Value::as_array).unwrap();
    assert_eq!(&second_msgs[..first_msgs.len()], &first_msgs[..]);
    assert_eq!(
        second_msgs.last(),
        Some(&json!({
            "role": "system",
            "content": [],
            "output_config": { "effort": "high" }
        }))
    );
    assert_eq!(first_payload.get("output_config"), Some(&json!({ "effort": "low" })));
    assert_eq!(second_payload.get("output_config"), Some(&json!({ "effort": "low" })));
    assert_eq!(
        second_payload.get("thinking"),
        Some(&json!({
            "type": "adaptive",
            "display": "summarized",
            "block_binding": { "prefix_mismatch_behavior": "drop_block" }
        }))
    );
    assert_eq!(first_msg.provider_thinking_level.as_deref(), Some("low"));
}

#[tokio::test]
async fn preserves_native_effort_values() {
    let model = managed_model("anthropic");
    for effort in [
        AnthropicEffort::Low,
        AnthropicEffort::Medium,
        AnthropicEffort::High,
        AnthropicEffort::XHigh,
        AnthropicEffort::Max,
    ] {
        let (payload, message) = capture(
            &model,
            Context {
                system_prompt: None,
                messages: vec![user("one", 1)],
                tools: None,
            },
            Some(effort),
        )
        .await;
        assert_eq!(
            effort_messages(&payload),
            vec![json!({
                "role": "system",
                "content": [],
                "output_config": { "effort": effort.as_str() }
            })]
        );
        assert_eq!(message.provider_thinking_level.as_deref(), Some(effort.as_str()));
    }
}

#[tokio::test]
async fn defaults_omitted_effort_to_high_and_still_enables_drop_block() {
    let (payload, message) = capture(
        &managed_model("anthropic"),
        Context {
            system_prompt: None,
            messages: vec![user("one", 1)],
            tools: None,
        },
        None,
    )
    .await;
    assert_eq!(
        payload.get("messages").and_then(Value::as_array).unwrap().last(),
        Some(&json!({
            "role": "system",
            "content": [],
            "output_config": { "effort": "high" }
        }))
    );
    assert_eq!(
        payload
            .get("thinking")
            .and_then(|t| t.get("block_binding"))
            .and_then(|b| b.get("prefix_mismatch_behavior"))
            .and_then(Value::as_str),
        Some("drop_block")
    );
    assert_eq!(message.provider_thinking_level.as_deref(), Some("high"));
}

#[tokio::test]
async fn does_not_invent_markers_for_legacy_or_other_provider_assistants() {
    let model = managed_model("anthropic");
    let legacy = assistant(&model, None);
    let other_provider = match assistant(&model, Some("low")) {
        Message::Assistant(mut a) => {
            a.provider = "other-provider".to_owned();
            Message::Assistant(a)
        }
        _ => unreachable!(),
    };
    let (payload, _) = capture(
        &model,
        Context {
            system_prompt: None,
            messages: vec![user("one", 1), legacy, user("two", 2), other_provider, user("three", 3)],
            tools: None,
        },
        Some(AnthropicEffort::Medium),
    )
    .await;
    assert_eq!(
        effort_messages(&payload),
        vec![json!({ "role": "system", "content": [], "output_config": { "effort": "medium" } })]
    );
}

#[tokio::test]
async fn leaves_unsupported_models_on_top_level_effort() {
    let mut model = managed_model("anthropic");
    model.compat = Some(ModelCompat::AnthropicMessages(AnthropicMessagesCompat {
        force_adaptive_thinking: Some(true),
        ..Default::default()
    }));
    let (payload, message) = capture(
        &model,
        Context {
            system_prompt: None,
            messages: vec![user("one", 1)],
            tools: None,
        },
        Some(AnthropicEffort::Low),
    )
    .await;
    assert_eq!(
        payload.get("messages"),
        Some(&json!([{ "role": "user", "content": "one" }]))
    );
    assert_eq!(payload.get("output_config"), Some(&json!({ "effort": "low" })));
    assert_eq!(
        payload.get("thinking"),
        Some(&json!({ "type": "adaptive", "display": "summarized" }))
    );
    assert!(message.provider_thinking_level.is_none());
}

#[tokio::test]
async fn sends_the_effort_and_binding_beta_headers() {
    let events = [
        json!({
            "type": "message_start",
            "message": {
                "id": "msg_test",
                "model": "claude-fable-5-1",
                "usage": { "input_tokens": 1, "output_tokens": 0 }
            }
        }),
        json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn" },
            "usage": { "input_tokens": 1, "output_tokens": 1 }
        }),
        json!({ "type": "message_stop" }),
    ];
    let body: String = events
        .iter()
        .map(|ev| {
            format!(
                "event: {}\ndata: {}\n\n",
                ev.get("type").unwrap().as_str().unwrap(),
                pi_js::json::stringify(ev)
            )
        })
        .collect();

    let beta_header = Arc::new(Mutex::new(None));
    let bh_c = beta_header.clone();
    let result = stream_with_options(
        &managed_model("anthropic"),
        &normalize_context(&Context {
            system_prompt: None,
            messages: vec![user("one", 1)],
            tools: None,
        }),
        Some(AnthropicOptions {
            base: StreamOptions {
                api_key: Some("test-key".to_owned()),
                cache_retention: Some(CacheRetention::None),
                fetch: Some(Arc::new(move |req: Request| {
                    let bh = bh_c.clone();
                    let b = body.clone();
                    Box::pin(async move {
                        *bh.lock().unwrap() = req.headers.get("anthropic-beta");
                        Ok(Response::new(
                            200,
                            Headers::from_pairs([("content-type", "text/event-stream")]),
                            b,
                        ))
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(result.stop_reason, StopReason::Stop);
    let beta = beta_header.lock().unwrap().clone().unwrap_or_default();
    assert!(beta.contains("mid-conversation-output-config-2026-07-01"));
    assert!(beta.contains("thinking-binding-controls-2026-08-01"));
}

#[test]
fn generates_exact_model_and_transport_gates() {
    let direct = get_model("anthropic", "claude-fable-5-1").unwrap();
    let open_router = get_model("openrouter", "anthropic/claude-fable-5.1").unwrap();
    let unsupported = get_model("anthropic", "claude-opus-4-8").unwrap();
    assert!(matches!(
        &direct.compat,
        Some(ModelCompat::AnthropicMessages(c)) if c.supports_mid_convo_effort == Some(true)
    ));
    assert_eq!(
        direct
            .thinking_level_map
            .as_ref()
            .and_then(|m| m.get(&ModelThinkingLevel::Off))
            .cloned(),
        Some(None)
    );
    assert_eq!(open_router.api, "anthropic-messages");
    assert_eq!(open_router.base_url, "https://openrouter.ai/api");
    assert!(matches!(
        &open_router.compat,
        Some(ModelCompat::AnthropicMessages(c)) if c.supports_mid_convo_effort == Some(true)
    ));
    assert!(matches!(
        &unsupported.compat,
        Some(ModelCompat::AnthropicMessages(c)) if c.supports_mid_convo_effort.is_none()
    ));

    let or_opus5 = get_model("openrouter", "anthropic/claude-opus-5").unwrap();
    let opus5 = get_model("anthropic", "claude-opus-5").unwrap();
    assert!(matches!(
        &or_opus5.compat,
        Some(ModelCompat::AnthropicMessages(c)) if c.supports_mid_convo_effort.is_none()
    ));
    assert!(matches!(
        &opus5.compat,
        Some(ModelCompat::AnthropicMessages(c)) if c.allowed_fallback_models.is_none()
    ));
}

#[tokio::test]
async fn sends_plain_disabled_thinking_without_effort_when_off_is_supported() {
    let mut model = managed_model("anthropic");
    let mut level_map: ThinkingLevelMap = IndexMap::new();
    level_map.insert(ModelThinkingLevel::Minimal, None);
    level_map.insert(ModelThinkingLevel::Low, Some("low".to_owned()));
    level_map.insert(ModelThinkingLevel::Medium, Some("medium".to_owned()));
    level_map.insert(ModelThinkingLevel::High, Some("high".to_owned()));
    level_map.insert(ModelThinkingLevel::Max, Some("max".to_owned()));
    model.thinking_level_map = Some(level_map);

    let (payload, message) = capture_with(
        &model,
        Context {
            system_prompt: None,
            messages: vec![
                user("one", 1),
                assistant(&model, Some("low")),
                user("two", 2),
                assistant(&model, Some("medium")),
                user("three", 3),
            ],
            tools: None,
        },
        None,
        Some(false),
    )
    .await;
    assert_eq!(payload.get("thinking"), Some(&json!({ "type": "disabled" })));
    assert!(payload.get("output_config").is_none());
    assert!(effort_messages(&payload).is_empty());
    assert!(message.provider_thinking_level.is_none());
}

#[tokio::test]
async fn keeps_adaptive_thinking_when_off_is_unsupported() {
    let (payload, message) = capture_with(
        &managed_model("anthropic"),
        Context {
            system_prompt: None,
            messages: vec![user("one", 1)],
            tools: None,
        },
        None,
        Some(false),
    )
    .await;
    assert_eq!(
        payload
            .get("thinking")
            .and_then(|t| t.get("type"))
            .and_then(Value::as_str),
        Some("adaptive")
    );
    assert_eq!(
        effort_messages(&payload),
        vec![json!({ "role": "system", "content": [], "output_config": { "effort": "high" } })]
    );
    assert_eq!(message.provider_thinking_level.as_deref(), Some("high"));
}

#[test]
fn supports_off_only_on_managed_models_that_accept_disabled_thinking() {
    for id in ["claude-opus-5", "claude-haiku-5-5"] {
        let model = get_model("anthropic", id).unwrap();
        assert!(matches!(
            &model.compat,
            Some(ModelCompat::AnthropicMessages(c)) if c.supports_mid_convo_effort == Some(true)
        ));
        assert!(
            !matches!(
                model
                    .thinking_level_map
                    .as_ref()
                    .and_then(|m| m.get(&ModelThinkingLevel::Off)),
                Some(None)
            ),
            "{id} should not set thinking_level_map.off to null"
        );
    }
    for id in [
        "claude-opus-5-5",
        "claude-sonnet-5-5",
        "claude-fable-5-1",
        "claude-fable-5-2",
    ] {
        let model = get_model("anthropic", id).unwrap();
        assert_eq!(
            model
                .thinking_level_map
                .as_ref()
                .and_then(|m| m.get(&ModelThinkingLevel::Off))
                .cloned(),
            Some(None)
        );
    }
}

#[test]
fn gates_fable_5_2_like_fable_5_1_and_keeps_only_fallbacks_that_accept_per_message_effort() {
    let direct = get_model("anthropic", "claude-fable-5-2").unwrap();
    assert!(matches!(
        &direct.compat,
        Some(ModelCompat::AnthropicMessages(c))
            if c.supports_mid_convo_effort == Some(true) && c.supports_mid_convo_system_messages == Some(true)
    ));
    let level_map = direct.thinking_level_map.as_ref().unwrap();
    assert_eq!(
        level_map.get(&ModelThinkingLevel::XHigh),
        Some(&Some("xhigh".to_owned()))
    );
    assert_eq!(level_map.get(&ModelThinkingLevel::Max), Some(&Some("max".to_owned())));
    let fallbacks: Vec<&str> = match &direct.compat {
        Some(ModelCompat::AnthropicMessages(c)) => c
            .allowed_fallback_models
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|f| f.model.as_str())
            .collect(),
        _ => Vec::new(),
    };
    assert_eq!(fallbacks, vec!["claude-opus-5"]);

    let or_fable = get_model("openrouter", "anthropic/claude-fable-5.2").unwrap();
    assert!(matches!(
        &or_fable.compat,
        Some(ModelCompat::AnthropicMessages(c)) if c.supports_mid_convo_effort == Some(true)
    ));
}
