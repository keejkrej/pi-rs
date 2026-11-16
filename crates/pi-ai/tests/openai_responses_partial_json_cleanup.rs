//! Port of packages/ai/test/openai-responses-partial-json-cleanup.test.ts

use futures::StreamExt;
use indexmap::IndexMap;
use pi_ai::api::openai_responses_shared::{
    ResponsesEventJsonStream, create_pending_assistant_message, process_responses_stream,
};
use pi_ai::compat::get_model;
use pi_ai::types::{AssistantContentBlock, AssistantMessageEvent, ToolCall};
use pi_ai::utils::event_stream::create_assistant_message_event_stream;
use serde_json::{Value, json};

#[tokio::test]
async fn drops_partial_json_from_completed_tool_calls_before_emitting_toolcall_end_and_returning() {
    let model = get_model("openai", "gpt-5.4").unwrap();
    let mut output = create_pending_assistant_message(&model);
    let event_stream = create_assistant_message_event_stream();

    let events = vec![
        json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "type": "function_call",
                "id": "fc_edit",
                "call_id": "call_edit",
                "name": "edit",
                "arguments": "",
            },
        }),
        json!({
            "type": "response.function_call_arguments.delta",
            "output_index": 0,
            "delta": "{\"path\":\"README.md\"",
        }),
        json!({
            "type": "response.function_call_arguments.delta",
            "output_index": 0,
            "delta": ",\"content\":\"updated\"}",
        }),
        json!({
            "type": "response.function_call_arguments.done",
            "output_index": 0,
            "arguments": "{\"path\":\"README.md\",\"content\":\"updated\"}",
        }),
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "type": "function_call",
                "id": "fc_edit",
                "call_id": "call_edit",
                "name": "edit",
                "arguments": "{\"path\":\"README.md\",\"content\":\"updated\"}",
            },
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
    ];

    let json_stream: ResponsesEventJsonStream = Box::pin(futures::stream::iter(events.into_iter().map(Ok)));
    process_responses_stream(json_stream, &mut output, &event_stream, &model, None)
        .await
        .unwrap();
    event_stream.end(Some(output.clone()));

    let mut emitted_events = Vec::new();
    let mut reader = event_stream.clone();
    while let Some(ev) = reader.next().await {
        emitted_events.push(ev);
    }

    let toolcall_end = emitted_events
        .iter()
        .find_map(|ev| match ev {
            AssistantMessageEvent::ToolCallEnd { tool_call, .. } => Some(tool_call),
            _ => None,
        })
        .expect("expected toolcall_end event");

    let mut expected_args = IndexMap::new();
    expected_args.insert("path".to_owned(), Value::String("README.md".to_owned()));
    expected_args.insert("content".to_owned(), Value::String("updated".to_owned()));
    let expected = ToolCall {
        id: "call_edit|fc_edit".to_owned(),
        name: "edit".to_owned(),
        arguments: expected_args,
        thought_signature: None,
        namespace: None,
    };

    assert_eq!(output.content, vec![AssistantContentBlock::ToolCall(expected.clone())]);
    assert_eq!(toolcall_end, &expected);

    let serialized_block = serde_json::to_value(&output.content[0]).unwrap();
    assert!(serialized_block.get("partialJson").is_none());
}
