//! Port of packages/ai/test/azure-openai-responses-reasoning-replay.test.ts

use futures::stream;
use indexmap::IndexSet;
use pi_ai::api::openai_responses_shared::{
    ResponsesEventJsonStream, convert_responses_messages, process_responses_stream,
};
use pi_ai::compat::normalize_context;
use pi_ai::types::{
    AssistantMessage, Context, Message, Model, ModelCost, ModelInput, StopReason, Usage, UserMessage,
    UserMessageContent,
};
use pi_ai::utils::event_stream::AssistantMessageEventStream;
use pi_js::time;
use serde_json::{Value, json};

fn create_model() -> Model {
    Model {
        r#type: None,
        id: "gpt-5-mini".to_owned(),
        name: "GPT-5 Mini".to_owned(),
        api: "azure-openai-responses".to_owned(),
        provider: "azure-openai-responses".to_owned(),
        base_url: "https://example.invalid".to_owned(),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 400_000,
        max_tokens: 128_000,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

fn create_output(model: &Model) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Pending,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: time::now_ms(),
    }
}

fn create_events(done_item: Value, completed_item: Value) -> ResponsesEventJsonStream {
    let id = done_item.get("id").cloned().unwrap_or(json!("rs"));
    let events = vec![
        Ok(json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "sequence_number": 0,
            "item": { "type": "reasoning", "id": id, "summary": [] }
        })),
        Ok(json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "sequence_number": 1,
            "item": done_item
        })),
        Ok(json!({
            "type": "response.completed",
            "sequence_number": 2,
            "response": {
                "id": "resp_test",
                "status": "completed",
                "output": [completed_item]
            }
        })),
    ];
    Box::pin(stream::iter(events))
}

fn get_replayed_reasoning(model: &Model, assistant: AssistantMessage) -> Option<Value> {
    let now = time::now_ms();
    let context = normalize_context(&Context {
        system_prompt: None,
        messages: vec![
            Message::User(UserMessage {
                content: UserMessageContent::Text("first".to_owned()),
                timestamp: now - 1,
            }),
            Message::Assistant(assistant),
            Message::User(UserMessage {
                content: UserMessageContent::Text("follow-up".to_owned()),
                timestamp: now,
            }),
        ],
        tools: None,
    });
    let mut allowed = IndexSet::new();
    allowed.insert("azure-openai-responses".to_owned());
    let input = convert_responses_messages(model, &context, &allowed, None);
    input
        .into_iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
}

#[tokio::test]
async fn preserves_existing_encrypted_content_from_output_item_done() {
    let model = create_model();
    let mut output = create_output(&model);
    let done_item = json!({
        "type": "reasoning",
        "id": "rs_done",
        "summary": [],
        "encrypted_content": "from-output-item-done"
    });
    let completed_item = json!({
        "type": "reasoning",
        "id": "rs_done",
        "summary": [],
        "encrypted_content": "from-response-completed"
    });

    let stream = AssistantMessageEventStream::new();
    process_responses_stream(
        create_events(done_item, completed_item),
        &mut output,
        &stream,
        &model,
        None,
    )
    .await
    .unwrap();

    let replayed = get_replayed_reasoning(&model, output).unwrap();
    assert_eq!(replayed.get("type"), Some(&json!("reasoning")));
    assert_eq!(replayed.get("id"), Some(&json!("rs_done")));
    assert_eq!(replayed.get("encrypted_content"), Some(&json!("from-output-item-done")));
}

#[tokio::test]
async fn fills_encrypted_content_when_output_item_done_omitted_it() {
    let model = create_model();
    let mut output = create_output(&model);
    let done_item = json!({
        "type": "reasoning",
        "id": "rs_missing",
        "summary": []
    });
    let completed_item = json!({
        "type": "reasoning",
        "id": "rs_missing",
        "summary": [],
        "encrypted_content": "from-response-completed"
    });

    let stream = AssistantMessageEventStream::new();
    process_responses_stream(
        create_events(done_item, completed_item),
        &mut output,
        &stream,
        &model,
        None,
    )
    .await
    .unwrap();

    let replayed = get_replayed_reasoning(&model, output).unwrap();
    assert_eq!(replayed.get("type"), Some(&json!("reasoning")));
    assert_eq!(replayed.get("id"), Some(&json!("rs_missing")));
    assert_eq!(
        replayed.get("encrypted_content"),
        Some(&json!("from-response-completed"))
    );
}
