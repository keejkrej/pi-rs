//! Port of packages/ai/test/openai-responses-namespace.test.ts

use indexmap::{IndexMap, IndexSet};
use pi_ai::api::openai_responses_shared::{
    ConvertResponsesMessagesOptions, ProcessResponsesStreamOptions, ResponsesEventJsonStream,
    convert_responses_messages, create_pending_assistant_message, process_responses_stream,
};
use pi_ai::compat::{get_model, normalize_context};
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, Context, Message, Model, ModelCompat, ModelCost, ModelInput,
    OpenAIResponsesCompat, StopReason, ToolCall, Usage, UserMessage, UserMessageContent,
};
use pi_ai::utils::event_stream::create_assistant_message_event_stream;
use pi_js::time;
use serde_json::{Value, json};

fn create_function_call_events(namespace: Option<&str>, include_only_on_done: bool) -> Vec<Value> {
    let mut added_item = json!({
        "type": "function_call",
        "id": "fc_test",
        "call_id": "call_test",
        "name": "tool_a",
        "arguments": "",
    });
    if !include_only_on_done {
        if let Some(ns) = namespace {
            added_item["namespace"] = Value::String(ns.to_owned());
        }
    }

    let mut done_item = json!({
        "type": "function_call",
        "id": "fc_test",
        "call_id": "call_test",
        "name": "tool_a",
        "arguments": "{\"x\":1}",
    });
    if let Some(ns) = namespace {
        done_item["namespace"] = Value::String(ns.to_owned());
    }

    vec![
        json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": added_item,
        }),
        json!({
            "type": "response.function_call_arguments.delta",
            "output_index": 0,
            "delta": "{\"x\":1}",
        }),
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": done_item,
        }),
        json!({
            "type": "response.completed",
            "response": {
                "id": "resp_1",
                "status": "completed",
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 5,
                    "total_tokens": 15,
                    "input_tokens_details": { "cached_tokens": 0 },
                },
            },
        }),
    ]
}

fn to_json_stream(events: Vec<Value>) -> ResponsesEventJsonStream {
    Box::pin(futures::stream::iter(events.into_iter().map(Ok)))
}

fn allowed_openai() -> IndexSet<String> {
    let mut s = IndexSet::new();
    s.insert("openai".to_owned());
    s
}

#[tokio::test]
async fn strips_empty_error_message_field_for_terminal_messages() {
    let model = get_model("openai", "gpt-5.4").unwrap();
    let mut output = create_pending_assistant_message(&model);
    let event_stream = create_assistant_message_event_stream();

    process_responses_stream(
        to_json_stream(create_function_call_events(None, false)),
        &mut output,
        &event_stream,
        &model,
        None,
    )
    .await
    .unwrap();

    let serialized = serde_json::to_value(&output).unwrap();
    assert!(serialized.get("errorMessage").is_none());
}

#[tokio::test]
async fn preserves_namespace_on_function_calls_when_only_done_includes_it() {
    let model = get_model("openai", "gpt-5.4").unwrap();
    let mut output = create_pending_assistant_message(&model);
    let event_stream = create_assistant_message_event_stream();

    process_responses_stream(
        to_json_stream(create_function_call_events(Some("dynamic_tools"), true)),
        &mut output,
        &event_stream,
        &model,
        None,
    )
    .await
    .unwrap();

    let mut expected_args = IndexMap::new();
    expected_args.insert("x".to_owned(), json!(1));
    assert_eq!(
        output.content,
        vec![AssistantContentBlock::ToolCall(ToolCall {
            id: "call_test|fc_test".to_owned(),
            name: "tool_a".to_owned(),
            arguments: expected_args,
            thought_signature: None,
            namespace: Some("dynamic_tools".to_owned()),
        })]
    );

    let ctx = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::Assistant(output)],
        tools: None,
    });
    let replayed = convert_responses_messages(&model, &ctx, &allowed_openai(), None);
    let fc = replayed
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .unwrap();
    assert_eq!(fc.get("namespace"), Some(&json!("dynamic_tools")));
}

#[tokio::test]
async fn preserves_namespace_on_custom_tool_calls_with_grammar_properties() {
    let model = get_model("openai", "gpt-5.4").unwrap();
    let mut output = create_pending_assistant_message(&model);
    let event_stream = create_assistant_message_event_stream();

    let events = vec![
        json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "type": "custom_tool_call",
                "id": "ctc_test",
                "call_id": "call_test",
                "name": "query",
                "input": "",
                "namespace": "dynamic_tools",
            },
        }),
        json!({
            "type": "response.custom_tool_call_input.delta",
            "output_index": 0,
            "delta": "hello",
        }),
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "type": "custom_tool_call",
                "id": "ctc_test",
                "call_id": "call_test",
                "name": "query",
                "input": "hello",
                "namespace": "dynamic_tools",
            },
        }),
        json!({
            "type": "response.completed",
            "response": {
                "id": "resp_2",
                "status": "completed",
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 5,
                    "total_tokens": 15,
                    "input_tokens_details": { "cached_tokens": 0 },
                },
            },
        }),
    ];

    let mut grammar_props = IndexMap::new();
    grammar_props.insert("query".to_owned(), "input".to_owned());

    process_responses_stream(
        to_json_stream(events),
        &mut output,
        &event_stream,
        &model,
        Some(&ProcessResponsesStreamOptions {
            grammar_tool_input_properties: Some(grammar_props.clone()),
            ..Default::default()
        }),
    )
    .await
    .unwrap();

    let mut expected_args = IndexMap::new();
    expected_args.insert("input".to_owned(), json!("hello"));
    assert_eq!(
        output.content,
        vec![AssistantContentBlock::ToolCall(ToolCall {
            id: "call_test|ctc_test".to_owned(),
            name: "query".to_owned(),
            arguments: expected_args,
            thought_signature: None,
            namespace: Some("dynamic_tools".to_owned()),
        })]
    );

    let ctx = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::Assistant(output)],
        tools: None,
    });
    let replayed = convert_responses_messages(
        &model,
        &ctx,
        &allowed_openai(),
        Some(&ConvertResponsesMessagesOptions {
            grammar_tool_input_properties: Some(grammar_props),
            ..Default::default()
        }),
    );
    let ctc = replayed
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("custom_tool_call"))
        .unwrap();
    assert_eq!(ctc.get("namespace"), Some(&json!("dynamic_tools")));
}

#[test]
fn drops_namespace_when_replaying_across_models_that_do_not_support_tool_search_load_items() {
    let source_model = get_model("openai", "gpt-5.4").unwrap();
    let mut fc_args = IndexMap::new();
    fc_args.insert("x".to_owned(), json!(1));
    let mut ctc_args = IndexMap::new();
    ctc_args.insert("input".to_owned(), json!("hello"));

    let assistant = AssistantMessage {
        content: vec![
            AssistantContentBlock::ToolCall(ToolCall {
                id: "call_fn|fc_1".to_owned(),
                name: "tool_a".to_owned(),
                arguments: fc_args,
                thought_signature: None,
                namespace: Some("dynamic_tools".to_owned()),
            }),
            AssistantContentBlock::ToolCall(ToolCall {
                id: "call_custom|ctc_1".to_owned(),
                name: "query".to_owned(),
                arguments: ctc_args,
                thought_signature: None,
                namespace: Some("dynamic_tools".to_owned()),
            }),
        ],
        api: source_model.api.clone(),
        provider: source_model.provider.clone(),
        model: source_model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: time::now_ms(),
    };

    let ctx = normalize_context(&Context {
        system_prompt: None,
        messages: vec![
            Message::User(UserMessage {
                content: UserMessageContent::Text("hi".to_owned()),
                timestamp: time::now_ms(),
            }),
            Message::Assistant(assistant),
        ],
        tools: None,
    });

    let mut grammar_props = IndexMap::new();
    grammar_props.insert("query".to_owned(), "input".to_owned());

    let targets = vec![
        get_model("openai", "gpt-5.2").unwrap(),
        Model {
            r#type: None,
            id: "gpt-5.4".to_owned(),
            name: "gpt-5.4".to_owned(),
            api: "azure-openai-responses".to_owned(),
            provider: "azure-openai-responses".to_owned(),
            base_url: "https://example.openai.azure.com/openai/v1".to_owned(),
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
            compat: Some(ModelCompat::OpenAIResponses(OpenAIResponsesCompat {
                supports_open_ai_grammar_tools: Some(true),
                ..Default::default()
            })),
        },
        get_model("openai-codex", "gpt-5.3-codex-spark").unwrap(),
    ];

    let mut allowed = IndexSet::new();
    allowed.insert("openai".to_owned());
    allowed.insert("openai-codex".to_owned());
    allowed.insert("azure-openai-responses".to_owned());

    for target_model in targets {
        let replayed = convert_responses_messages(
            &target_model,
            &ctx,
            &allowed,
            Some(&ConvertResponsesMessagesOptions {
                grammar_tool_input_properties: Some(grammar_props.clone()),
                ..Default::default()
            }),
        );
        let fc = replayed
            .iter()
            .find(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
            .unwrap();
        let ctc = replayed
            .iter()
            .find(|item| item.get("type").and_then(Value::as_str) == Some("custom_tool_call"))
            .unwrap();
        assert!(fc.get("namespace").is_none(), "fc for {}", target_model.id);
        assert!(ctc.get("namespace").is_none(), "ctc for {}", target_model.id);
    }
}

#[tokio::test]
async fn omits_namespace_for_ordinary_function_calls() {
    let model = get_model("openai", "gpt-5.4").unwrap();
    let mut output = create_pending_assistant_message(&model);
    let event_stream = create_assistant_message_event_stream();

    process_responses_stream(
        to_json_stream(create_function_call_events(None, false)),
        &mut output,
        &event_stream,
        &model,
        None,
    )
    .await
    .unwrap();

    if let AssistantContentBlock::ToolCall(tc) = &output.content[0] {
        assert_eq!(tc.namespace, None);
    } else {
        panic!("expected toolCall block");
    }

    let ctx = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::Assistant(output)],
        tools: None,
    });
    let replayed = convert_responses_messages(&model, &ctx, &allowed_openai(), None);
    let fc = replayed
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .unwrap();
    assert!(fc.get("namespace").is_none());
}
