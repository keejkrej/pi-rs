//! Port of packages/ai/test/openai-responses-mid-convo-effort.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::api::openai_responses::stream as stream_openai_responses;
use pi_ai::compat::normalize_context;
use pi_ai::types::{
    ApiStreamOptions, AssistantMessage, Context, Message, Model, ModelCompat, ModelCost, ModelInput,
    ModelThinkingLevel, OpenAIResponsesCompat, StreamOptions, TextContent, ToolResultMessage, UserContentBlock,
    UserMessage, UserMessageContent,
};
use pi_js::fetch::{Headers, Request, Response};
use serde_json::{Value, json};

fn thinking_map() -> IndexMap<ModelThinkingLevel, Option<String>> {
    let mut map = IndexMap::new();
    map.insert(ModelThinkingLevel::Off, Some("none".to_owned()));
    map.insert(ModelThinkingLevel::Minimal, None);
    map.insert(ModelThinkingLevel::Low, Some("low".to_owned()));
    map.insert(ModelThinkingLevel::Medium, Some("medium".to_owned()));
    map.insert(ModelThinkingLevel::High, Some("high".to_owned()));
    map.insert(ModelThinkingLevel::XHigh, Some("xhigh".to_owned()));
    map.insert(ModelThinkingLevel::Max, Some("max".to_owned()));
    map
}

fn openai_model(compat: OpenAIResponsesCompat) -> Model {
    Model {
        r#type: None,
        id: "gpt-6-luna".to_owned(),
        name: "GPT-6 Luna".to_owned(),
        api: "openai-responses".to_owned(),
        provider: "openai".into(),
        base_url: "https://api.openai.com/v1".to_owned(),
        reasoning: true,
        thinking_level_map: Some(thinking_map()),
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 272_000,
        max_tokens: 128_000,
        headers: None,
        sampling_params: None,
        compat: Some(ModelCompat::OpenAIResponses(compat)),
        sampling_params_by_thinking_level: None,
    }
}

fn user_msg(text: &str) -> Message {
    Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_owned()),
        timestamp: 0,
    })
}

fn tool_res(call_id: &str) -> Message {
    Message::ToolResult(ToolResultMessage {
        tool_call_id: call_id.to_owned(),
        tool_name: "lookup".to_owned(),
        content: vec![UserContentBlock::Text(TextContent::new("result"))],
        details: None,
        is_error: false,
        nested_calls: None,
        usage: None,
        timestamp: 0,
        duration_ms: None,
    })
}

fn update_item(effort: &str) -> Value {
    json!({ "type": "configuration_update", "reasoning": { "effort": effort } })
}

fn response_sse(response_id: &str, with_tool_call: bool) -> String {
    let item = if with_tool_call {
        json!({ "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "lookup", "arguments": "{}", "status": "completed" })
    } else {
        json!({
            "type": "message",
            "id": format!("msg_{response_id}"),
            "role": "assistant",
            "status": "completed",
            "content": [{ "type": "output_text", "text": "answer", "annotations": [] }],
        })
    };
    let events = vec![
        json!({ "type": "response.created", "response": { "id": response_id } }),
        json!({ "type": "response.output_item.added", "output_index": 0, "item": item }),
        json!({ "type": "response.output_item.done", "output_index": 0, "item": item }),
        json!({ "type": "response.completed", "response": { "id": response_id, "status": "completed" } }),
    ];
    events
        .into_iter()
        .map(|ev| format!("data: {}\n\n", pi_js::json::stringify(&ev)))
        .collect()
}

async fn run_openai(
    messages: &[Message],
    reasoning_effort: Option<&str>,
    api_key: Option<&str>,
    model: Option<&Model>,
    with_tool_call: bool,
) -> (Value, AssistantMessage) {
    let default_model = openai_model(OpenAIResponsesCompat {
        supports_mid_convo_effort: Some(true),
        ..Default::default()
    });
    let used_model = model.unwrap_or(&default_model);
    let captured_body = Arc::new(Mutex::new(None::<Value>));
    let cb = Arc::clone(&captured_body);

    let mut opts = ApiStreamOptions {
        base: StreamOptions {
            api_key: Some(api_key.unwrap_or("sk-test").to_owned()),
            fetch: Some(Arc::new(move |req: Request| {
                let body_val = req.body_json().unwrap();
                *cb.lock().unwrap() = Some(body_val);
                let sse = response_sse("resp_1", with_tool_call);
                Box::pin(async move {
                    Ok(Response::new(
                        200,
                        Headers::from_pairs([("content-type", "text/event-stream")]),
                        sse,
                    ))
                })
            })),
            ..Default::default()
        },
        ..Default::default()
    };
    if let Some(effort) = reasoning_effort {
        opts = opts.with_extra("reasoningEffort", json!(effort));
    }

    let message = stream_openai_responses(
        used_model,
        &normalize_context(&Context {
            system_prompt: None,
            messages: messages.to_vec(),
            tools: None,
        }),
        Some(opts),
    )
    .result()
    .await;
    assert_ne!(
        message.stop_reason,
        pi_ai::types::StopReason::Error,
        "{:?}",
        message.error_message
    );
    let payload = captured_body.lock().unwrap().take().expect("captured body");
    (payload, message)
}

#[tokio::test]
async fn keeps_reasoning_effort_constant_and_appends_configuration_update_items() {
    let mut history = vec![user_msg("one")];
    let (p1, m1) = run_openai(&history, Some("high"), None, None, false).await;
    history.push(Message::Assistant(m1.clone()));
    history.push(user_msg("two"));
    let (p2, m2) = run_openai(&history, Some("high"), None, None, false).await;
    history.push(Message::Assistant(m2.clone()));
    history.push(user_msg("three"));
    let (p3, m3) = run_openai(&history, Some("low"), None, None, false).await;

    assert_eq!(
        p1["input"],
        json!([
            { "role": "user", "content": [{ "type": "input_text", "text": "one" }] },
            update_item("high"),
        ])
    );
    for p in [&p1, &p2, &p3] {
        assert_eq!(p["reasoning"]["effort"], json!("medium"));
    }
    let i1 = p1["input"].as_array().unwrap();
    let i2 = p2["input"].as_array().unwrap();
    let i3 = p3["input"].as_array().unwrap();
    assert_eq!(&i2[..i1.len()], i1.as_slice());
    assert_eq!(&i3[..i2.len()], i2.as_slice());
    let updates: Vec<&Value> = i3
        .iter()
        .filter(|i| i.get("type").and_then(Value::as_str) == Some("configuration_update"))
        .collect();
    assert_eq!(
        updates,
        vec![&update_item("high"), &update_item("high"), &update_item("low")]
    );
    assert_eq!(i3.last(), Some(&update_item("low")));
    assert_eq!(m1.provider_thinking_level.as_deref(), Some("high"));
    assert_eq!(m2.provider_thinking_level.as_deref(), Some("high"));
    assert_eq!(m3.provider_thinking_level.as_deref(), Some("low"));
}

#[tokio::test]
async fn applies_effort_change_during_tool_loop_and_filters_by_model_and_provider() {
    let mut history = vec![user_msg("one")];
    let (p_tc, m_tc) = run_openai(&history, Some("high"), None, None, true).await;
    history.push(Message::Assistant(m_tc.clone()));
    history.push(tool_res("call_1|fc_1"));
    let (p_after, m_after) = run_openai(&history, Some("low"), None, None, false).await;
    let i_tc = p_tc["input"].as_array().unwrap();
    let i_after = p_after["input"].as_array().unwrap();
    assert_eq!(&i_after[..i_tc.len()], i_tc.as_slice());
    assert_eq!(i_after[i_after.len() - 2]["type"], json!("function_call_output"));
    assert_eq!(i_after[i_after.len() - 2]["call_id"], json!("call_1"));
    assert_eq!(i_after[i_after.len() - 1], update_item("low"));
    assert_eq!(m_after.provider_thinking_level.as_deref(), Some("low"));

    // Only adds items before replies recorded by matching provider/model with provider_thinking_level
    let no_flag_model = openai_model(OpenAIResponsesCompat::default());
    let (_, unstamped) = run_openai(&[user_msg("one")], Some("high"), None, Some(&no_flag_model), false).await;
    let other_provider = AssistantMessage {
        provider: "opencode".into(),
        provider_thinking_level: Some("high".to_owned()),
        ..unstamped.clone()
    };
    let other_model = AssistantMessage {
        model: "gpt-6-sol".to_owned(),
        provider_thinking_level: Some("high".to_owned()),
        ..unstamped.clone()
    };
    let recorded = AssistantMessage {
        provider_thinking_level: Some("high".to_owned()),
        ..unstamped.clone()
    };

    let (p_mixed, _) = run_openai(
        &[
            user_msg("one"),
            Message::Assistant(unstamped),
            user_msg("two"),
            Message::Assistant(other_provider),
            user_msg("three"),
            Message::Assistant(other_model),
            user_msg("four"),
            Message::Assistant(recorded),
            user_msg("five"),
        ],
        Some("low"),
        None,
        None,
        false,
    )
    .await;

    let kinds: Vec<&str> = p_mixed["input"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            i.get("type")
                .and_then(Value::as_str)
                .unwrap_or_else(|| i.get("role").and_then(Value::as_str).unwrap())
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            "user",
            "message",
            "user",
            "message",
            "user",
            "message",
            "user",
            "configuration_update",
            "message",
            "user",
            "configuration_update",
        ]
    );
    let updates: Vec<&Value> = p_mixed["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i.get("type").and_then(Value::as_str) == Some("configuration_update"))
        .collect();
    assert_eq!(updates, vec![&update_item("high"), &update_item("low")]);
}

#[tokio::test]
async fn sends_level_as_reasoning_effort_for_chatgpt_disabled_compat_or_off() {
    let default_model = openai_model(OpenAIResponsesCompat {
        supports_mid_convo_effort: Some(true),
        ..Default::default()
    });
    let off_model = openai_model(OpenAIResponsesCompat {
        supports_mid_convo_effort: Some(false),
        ..Default::default()
    });

    for (api_key, model, level, expected_effort) in [
        ("chatgpt-access-token", &default_model, Some("low"), "low"),
        ("sk-test", &off_model, Some("low"), "low"),
        ("sk-test", &default_model, None, "none"),
    ] {
        let (_, first) = run_openai(&[user_msg("one")], Some("high"), Some(api_key), Some(model), false).await;
        let (p, msg) = run_openai(
            &[user_msg("one"), Message::Assistant(first), user_msg("two")],
            level,
            Some(api_key),
            Some(model),
            false,
        )
        .await;
        assert_eq!(p["reasoning"]["effort"], json!(expected_effort));
        assert!(
            !p["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i.get("type").and_then(Value::as_str) == Some("configuration_update"))
        );
        assert_eq!(msg.provider_thinking_level, None);
    }
}
