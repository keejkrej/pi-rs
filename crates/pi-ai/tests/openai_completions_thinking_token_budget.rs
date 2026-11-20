//! Port of packages/ai/test/openai-completions-thinking-token-budget.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::compat::stream_simple;
use pi_ai::types::{
    ChatTemplateKwargValue, ChatTemplateVarKind, Context, Message, Model, ModelCompat, ModelCost, ModelInput,
    OpenAICompletionsCompat, SimpleStreamOptions, StreamOptions, ThinkingBudgets, ThinkingFormat, ThinkingLevel,
    ThinkingTokenBudgetField, UserMessage, UserMessageContent,
};
use pi_js::fetch::{Headers, Response};
use pi_js::time;
use serde_json::{Value, json};

fn ok_sse() -> String {
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({
            "choices": [{ "delta": {}, "finish_reason": "stop" }],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "prompt_tokens_details": { "cached_tokens": 0 },
                "completion_tokens_details": { "reasoning_tokens": 0 },
            },
        })
    )
}

fn vllm_model(compat: OpenAICompletionsCompat) -> Model {
    Model {
        r#type: None,
        id: "zai-org/glm-5.2".to_owned(),
        name: "GLM 5.2 (local vLLM)".to_owned(),
        api: "openai-completions".to_owned(),
        provider: "local-vllm".to_owned(),
        base_url: "http://localhost:8000/v1".to_owned(),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 262_144,
        max_tokens: 16_384,
        headers: None,
        sampling_params: None,
        compat: Some(ModelCompat::OpenAICompletions(compat)),
        sampling_params_by_thinking_level: None,
    }
}

async fn capture(
    model: &Model,
    reasoning: Option<ThinkingLevel>,
    thinking_budgets: Option<ThinkingBudgets>,
    max_tokens: Option<i64>,
) -> Value {
    let payload = Arc::new(Mutex::new(None::<Value>));
    let p_c = payload.clone();

    stream_simple(
        model,
        &Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Hi".to_owned()),
                timestamp: time::now_ms(),
            })],
            tools: None,
        },
        Some(SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("test".to_owned()),
                max_tokens,
                on_payload: Some(Arc::new(move |params, _| {
                    let p = p_c.clone();
                    Box::pin(async move {
                        *p.lock().unwrap() = Some(params);
                        Ok(None)
                    })
                })),
                fetch: Some(Arc::new(|_| {
                    Box::pin(async {
                        Ok(Response::new(
                            200,
                            Headers::from_pairs([("content-type", "text/event-stream")]),
                            ok_sse(),
                        ))
                    })
                })),
                ..Default::default()
            },
            reasoning,
            thinking_budgets,
            ..Default::default()
        }),
    )
    .result()
    .await;

    payload.lock().unwrap().take().unwrap()
}

fn default_vllm() -> Model {
    vllm_model(OpenAICompletionsCompat {
        thinking_format: Some(ThinkingFormat::Zai),
        supports_thinking_token_budget: Some(true),
        ..Default::default()
    })
}

#[tokio::test]
async fn sends_the_configured_budget_for_the_requested_level() {
    let params = capture(
        &default_vllm(),
        Some(ThinkingLevel::Medium),
        Some(ThinkingBudgets {
            medium: Some(4096),
            ..Default::default()
        }),
        None,
    )
    .await;
    assert_eq!(params.get("thinking_token_budget"), Some(&json!(4096)));
}

#[tokio::test]
async fn omits_the_budget_when_neither_the_field_nor_the_alias_is_set() {
    let model = vllm_model(OpenAICompletionsCompat {
        thinking_format: Some(ThinkingFormat::Zai),
        ..Default::default()
    });
    let params = capture(
        &model,
        Some(ThinkingLevel::Medium),
        Some(ThinkingBudgets {
            medium: Some(4096),
            ..Default::default()
        }),
        None,
    )
    .await;
    assert!(params.get("thinking_token_budget").is_none());
    assert!(params.get("thinking_budget").is_none());
    assert!(params.get("thinking_budget_tokens").is_none());
}

#[tokio::test]
async fn omits_the_budget_when_thinking_is_off() {
    let params = capture(
        &default_vllm(),
        None,
        Some(ThinkingBudgets {
            high: Some(8192),
            ..Default::default()
        }),
        None,
    )
    .await;
    assert!(params.get("thinking_token_budget").is_none());
}

#[tokio::test]
async fn clamps_xhigh_and_max_to_the_high_budget() {
    let budgets = Some(ThinkingBudgets {
        high: Some(8192),
        ..Default::default()
    });
    let xhigh = capture(&default_vllm(), Some(ThinkingLevel::XHigh), budgets, None).await;
    let max = capture(&default_vllm(), Some(ThinkingLevel::Max), budgets, None).await;
    assert_eq!(xhigh.get("thinking_token_budget"), Some(&json!(8192)));
    assert_eq!(max.get("thinking_token_budget"), Some(&json!(8192)));
}

#[tokio::test]
async fn leaves_room_for_answer_and_uses_lower_caller_max_tokens_ceiling() {
    let p1 = capture(&default_vllm(), Some(ThinkingLevel::High), None, None).await;
    assert_eq!(p1.get("thinking_token_budget"), Some(&json!(16_384 - 1024)));

    let p2 = capture(
        &default_vllm(),
        Some(ThinkingLevel::High),
        Some(ThinkingBudgets {
            high: Some(8192),
            ..Default::default()
        }),
        Some(4096),
    )
    .await;
    assert_eq!(p2.get("thinking_token_budget"), Some(&json!(4096 - 1024)));
}

#[tokio::test]
async fn sends_explicit_thinking_token_budget_field_and_overrides_boolean_alias() {
    for (field, key) in [
        (ThinkingTokenBudgetField::ThinkingBudget, "thinking_budget"),
        (ThinkingTokenBudgetField::ThinkingBudgetTokens, "thinking_budget_tokens"),
    ] {
        let model = vllm_model(OpenAICompletionsCompat {
            thinking_format: Some(ThinkingFormat::Qwen),
            thinking_token_budget_field: Some(field),
            ..Default::default()
        });
        let params = capture(
            &model,
            Some(ThinkingLevel::Medium),
            Some(ThinkingBudgets {
                medium: Some(4096),
                ..Default::default()
            }),
            None,
        )
        .await;
        assert_eq!(params.get(key), Some(&json!(4096)));
        assert!(params.get("thinking_token_budget").is_none());
    }

    let model_override = vllm_model(OpenAICompletionsCompat {
        thinking_format: Some(ThinkingFormat::Zai),
        supports_thinking_token_budget: Some(true),
        thinking_token_budget_field: Some(ThinkingTokenBudgetField::ThinkingBudget),
        ..Default::default()
    });
    let params_ov = capture(
        &model_override,
        Some(ThinkingLevel::Medium),
        Some(ThinkingBudgets {
            medium: Some(4096),
            ..Default::default()
        }),
        None,
    )
    .await;
    assert_eq!(params_ov.get("thinking_budget"), Some(&json!(4096)));
    assert!(params_ov.get("thinking_token_budget").is_none());
}

#[tokio::test]
async fn handles_thinking_budget_template_var_in_chat_template_kwargs() {
    let mut kwargs = IndexMap::new();
    kwargs.insert(
        "enable_thinking".to_owned(),
        ChatTemplateKwargValue::Var {
            var: ChatTemplateVarKind::ThinkingEnabled,
            omit_when_off: None,
        },
    );
    kwargs.insert(
        "thinking_budget".to_owned(),
        ChatTemplateKwargValue::Var {
            var: ChatTemplateVarKind::ThinkingBudget,
            omit_when_off: None,
        },
    );
    let model = vllm_model(OpenAICompletionsCompat {
        thinking_format: Some(ThinkingFormat::ChatTemplate),
        chat_template_kwargs: Some(kwargs),
        ..Default::default()
    });

    let p_on = capture(&model, Some(ThinkingLevel::High), None, None).await;
    assert_eq!(
        p_on.get("chat_template_kwargs"),
        Some(&json!({
            "enable_thinking": true,
            "thinking_budget": 16_384 - 1024,
        }))
    );
    assert!(p_on.get("thinking_token_budget").is_none());

    let p_off = capture(&model, None, None, None).await;
    assert_eq!(
        p_off.get("chat_template_kwargs"),
        Some(&json!({ "enable_thinking": false }))
    );
}
