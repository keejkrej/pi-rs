//! Port of packages/ai/test/openai-responses-terminal-event.test.ts

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use pi_ai::api::openai_responses::stream as stream_openai_responses;
use pi_ai::api::openai_responses_shared::{
    ResponsesEventJsonStream, create_pending_assistant_message, process_responses_stream,
};
use pi_ai::compat::{get_model, normalize_context};
use pi_ai::types::{
    ApiStreamOptions, AssistantMessageEvent, Context, Message, StopReason, StreamOptions, UserMessage,
    UserMessageContent,
};
use pi_ai::utils::event_stream::create_assistant_message_event_stream;
use pi_js::fetch::{Headers, Response};
use pi_js::time;
use serde_json::{Value, json};

fn to_json_stream(events: Vec<Value>) -> ResponsesEventJsonStream {
    Box::pin(futures::stream::iter(events.into_iter().map(Ok)))
}

fn to_sse_body(events: &[Value]) -> String {
    let mut out = String::new();
    for event in events {
        let ev_type = event.get("type").and_then(Value::as_str).unwrap_or("message");
        out.push_str(&format!(
            "event: {ev_type}\ndata: {}\n\n",
            pi_js::json::stringify(event)
        ));
    }
    out
}

fn early_eof_events() -> Vec<Value> {
    vec![
        json!({
            "type": "response.created",
            "response": { "id": "resp_1", "status": "in_progress" },
        }),
        json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "type": "reasoning",
                "id": "rs_1",
                "summary": [],
            },
        }),
        json!({
            "type": "response.reasoning_text.delta",
            "output_index": 0,
            "content_index": 0,
            "delta": "thinking...",
        }),
    ]
}

async fn run_phased_messages(phases: [&str; 2], terminal_event: Value) -> (Vec<StopReason>, StopReason) {
    let model = get_model("openai", "gpt-5.4").unwrap();
    let mut output = create_pending_assistant_message(&model);
    let event_stream = create_assistant_message_event_stream();

    let events = vec![
        json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": { "type": "message", "id": "msg_1", "phase": phases[0] },
        }),
        json!({
            "type": "response.output_text.delta",
            "output_index": 0,
            "content_index": 0,
            "delta": "first ",
        }),
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "type": "message",
                "id": "msg_1",
                "phase": phases[0],
                "content": [{ "type": "output_text", "text": "first " }],
            },
        }),
        json!({
            "type": "response.output_item.added",
            "output_index": 1,
            "item": { "type": "message", "id": "msg_2", "phase": phases[1] },
        }),
        json!({
            "type": "response.output_text.delta",
            "output_index": 1,
            "content_index": 0,
            "delta": "second",
        }),
        json!({
            "type": "response.output_item.done",
            "output_index": 1,
            "item": {
                "type": "message",
                "id": "msg_2",
                "phase": phases[1],
                "content": [{ "type": "output_text", "text": "second" }],
            },
        }),
        terminal_event,
    ];

    process_responses_stream(to_json_stream(events), &mut output, &event_stream, &model, None)
        .await
        .unwrap();
    event_stream.end(Some(output.clone()));

    let mut observed = Vec::new();
    let mut reader = event_stream.clone();
    while let Some(ev) = reader.next().await {
        if let AssistantMessageEvent::TextEnd { partial, .. } = ev {
            observed.push(partial.stop_reason);
        }
    }
    (observed, output.stop_reason)
}

#[tokio::test]
async fn errors_when_stream_ends_without_terminal_response_event() {
    let model = get_model("openai", "gpt-5.4").unwrap();
    let mut output = create_pending_assistant_message(&model);
    let event_stream = create_assistant_message_event_stream();

    let err = process_responses_stream(
        to_json_stream(early_eof_events()),
        &mut output,
        &event_stream,
        &model,
        None,
    )
    .await
    .unwrap_err();

    assert!(
        err.to_string()
            .contains("OpenAI Responses stream ended before a terminal response event"),
        "unexpected err: {err}"
    );
}

#[tokio::test]
async fn errors_when_response_completes_with_an_unfinished_tool_call() {
    let model = get_model("openai", "gpt-5.4").unwrap();
    let mut output = create_pending_assistant_message(&model);
    let event_stream = create_assistant_message_event_stream();

    let events = vec![
        json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "type": "function_call",
                "id": "fc_1",
                "call_id": "call_1",
                "name": "bash",
                "arguments": "",
            },
        }),
        json!({
            "type": "response.function_call_arguments.delta",
            "output_index": 0,
            "delta": "{\"command\":\"echo hi",
        }),
        json!({
            "type": "response.completed",
            "response": {
                "id": "resp_unfinished_tc",
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

    let err = process_responses_stream(to_json_stream(events), &mut output, &event_stream, &model, None)
        .await
        .unwrap_err();

    assert!(
        err.to_string()
            .contains("OpenAI Responses stream completed with an unfinished tool call: bash (call_1|fc_1)"),
        "unexpected err: {err}"
    );
}

#[tokio::test]
async fn preserves_parallel_tool_call_partial_json_and_errors_if_one_stays_unfinished() {
    let model = get_model("openai", "gpt-5.4").unwrap();
    let mut output = create_pending_assistant_message(&model);
    let event_stream = create_assistant_message_event_stream();

    let events = vec![
        json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "type": "function_call",
                "id": "fc_a",
                "call_id": "call_a",
                "name": "bash",
                "arguments": "",
            },
        }),
        json!({
            "type": "response.output_item.added",
            "output_index": 1,
            "item": {
                "type": "function_call",
                "id": "fc_b",
                "call_id": "call_b",
                "name": "read",
                "arguments": "",
            },
        }),
        json!({
            "type": "response.function_call_arguments.delta",
            "delta": "{\"command\":\"ls\"}",
        }),
        json!({
            "type": "response.output_item.done",
            "output_index": 1,
            "item": {
                "type": "function_call",
                "id": "fc_b",
                "call_id": "call_b",
                "name": "read",
                "arguments": "{\"path\":\"a.txt\"}",
            },
        }),
        json!({
            "type": "response.completed",
            "response": {
                "id": "resp_unfinished_parallel",
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

    let err = process_responses_stream(to_json_stream(events), &mut output, &event_stream, &model, None)
        .await
        .unwrap_err();

    assert!(
        err.to_string()
            .contains("OpenAI Responses stream completed with an unfinished tool call: bash (call_a|fc_a)"),
        "unexpected err: {err}"
    );
}

#[tokio::test]
async fn emits_error_from_stream_when_upstream_closes_early_and_forwards_raw_events() {
    let model = get_model("openai", "gpt-5.4").unwrap();
    let sse = to_sse_body(&early_eof_events());
    let seen_raw: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_raw_c = seen_raw.clone();

    let ctx = normalize_context(&Context {
        system_prompt: Some("sys".to_owned()),
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hi".to_owned()),
            timestamp: time::now_ms(),
        })],
        tools: None,
    });

    let es = stream_openai_responses(
        &model,
        &ctx,
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("sk-test-key".to_owned()),
                fetch: Some(Arc::new(move |_| {
                    let body = sse.clone();
                    Box::pin(async move {
                        Ok(Response::new(
                            200,
                            Headers::from_pairs([("content-type", "text/event-stream")]),
                            body,
                        ))
                    })
                })),
                on_provider_stream_event: Some(Arc::new(move |ev, m| {
                    let sr = seen_raw_c.clone();
                    let ev_type = ev.get("type").and_then(Value::as_str).unwrap_or("").to_owned();
                    let mid = m.id.clone();
                    Box::pin(async move {
                        sr.lock().unwrap().push((ev_type, mid));
                        Ok(())
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    );

    let mut emitted = Vec::new();
    let mut reader = es.clone();
    while let Some(ev) = reader.next().await {
        emitted.push(ev);
    }
    let result = es.result().await;

    let raw = seen_raw.lock().unwrap().clone();
    assert_eq!(
        raw,
        vec![
            ("response.created".to_owned(), model.id.clone()),
            ("response.output_item.added".to_owned(), model.id.clone()),
            ("response.reasoning_text.delta".to_owned(), model.id.clone()),
        ]
    );

    match &emitted[0] {
        AssistantMessageEvent::Start { partial } => {
            assert_eq!(partial.stop_reason, StopReason::Pending);
        }
        other => panic!("expected Start event, got {other:?}"),
    }
    match emitted.last() {
        Some(AssistantMessageEvent::Error { reason, error }) => {
            assert_eq!(*reason, StopReason::Error);
            assert_eq!(error.stop_reason, StopReason::Error);
        }
        other => panic!("expected Error event, got {other:?}"),
    }
    assert_eq!(result.stop_reason, StopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .unwrap_or("")
            .contains("OpenAI Responses stream ended before a terminal response event"),
        "unexpected error_message: {:?}",
        result.error_message
    );
}

#[tokio::test]
async fn maps_phased_messages_and_terminal_events_for_stop_reasons() {
    let completed = || {
        json!({
            "type": "response.completed",
            "response": {
                "id": "resp_phased",
                "status": "completed",
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 5,
                    "total_tokens": 15,
                    "input_tokens_details": { "cached_tokens": 0 },
                },
            },
        })
    };

    assert_eq!(
        run_phased_messages(["commentary", "commentary"], completed()).await,
        (vec![StopReason::Pending, StopReason::Pending], StopReason::Stop)
    );
    assert_eq!(
        run_phased_messages(["final_answer", "final_answer"], completed()).await,
        (vec![StopReason::Stop, StopReason::Stop], StopReason::Stop)
    );
    assert_eq!(
        run_phased_messages(["commentary", "final_answer"], completed()).await,
        (vec![StopReason::Pending, StopReason::Stop], StopReason::Stop)
    );
    assert_eq!(
        run_phased_messages(
            ["final_answer", "final_answer"],
            json!({
                "type": "response.incomplete",
                "response": {
                    "id": "resp_inc",
                    "status": "incomplete",
                    "incomplete_details": { "reason": "max_output_tokens" },
                    "usage": {
                        "input_tokens": 10,
                        "output_tokens": 5,
                        "total_tokens": 15,
                        "input_tokens_details": { "cached_tokens": 0 },
                    },
                },
            })
        )
        .await,
        (vec![StopReason::Stop, StopReason::Stop], StopReason::Length)
    );
}

#[tokio::test]
async fn maps_completed_incomplete_and_failed_terminal_events_with_usage() {
    let model = get_model("openai", "gpt-5.4").unwrap();

    // response.completed
    let mut out1 = create_pending_assistant_message(&model);
    let es1 = create_assistant_message_event_stream();
    process_responses_stream(
        to_json_stream(vec![json!({
            "type": "response.completed",
            "response": {
                "id": "resp_completed",
                "status": "completed",
                "usage": {
                    "input_tokens": 20,
                    "output_tokens": 7,
                    "total_tokens": 27,
                    "input_tokens_details": {
                        "cached_tokens": 2,
                        "cache_write_tokens": 3,
                    },
                },
            },
        })]),
        &mut out1,
        &es1,
        &model,
        None,
    )
    .await
    .unwrap();
    assert_eq!(out1.response_id.as_deref(), Some("resp_completed"));
    assert_eq!(out1.stop_reason, StopReason::Stop);
    assert_eq!(out1.raw_stop_reason.as_deref(), Some("completed"));
    assert_eq!(out1.usage.input, 15);
    assert_eq!(out1.usage.output, 7);
    assert_eq!(out1.usage.cache_read, 2);
    assert_eq!(out1.usage.cache_write, 3);
    assert_eq!(out1.usage.total_tokens, 27);

    // response.incomplete (max_output_tokens)
    let mut out2 = create_pending_assistant_message(&model);
    let es2 = create_assistant_message_event_stream();
    process_responses_stream(
        to_json_stream(vec![json!({
            "type": "response.incomplete",
            "response": {
                "id": "resp_len",
                "status": "incomplete",
                "incomplete_details": { "reason": "max_output_tokens" },
                "usage": {
                    "input_tokens": 30,
                    "output_tokens": 12,
                    "total_tokens": 42,
                    "input_tokens_details": { "cached_tokens": 5 },
                },
            },
        })]),
        &mut out2,
        &es2,
        &model,
        None,
    )
    .await
    .unwrap();
    assert_eq!(out2.stop_reason, StopReason::Length);
    assert_eq!(out2.raw_stop_reason.as_deref(), Some("incomplete.max_output_tokens"));
    assert_eq!(out2.usage.input, 25);
    assert_eq!(out2.usage.output, 12);
    assert_eq!(out2.usage.cache_read, 5);
    assert_eq!(out2.usage.cache_write, 0);
    assert_eq!(out2.usage.total_tokens, 42);

    // response.incomplete (content_filter / max_time_limit)
    for reason in ["content_filter", "max_time_limit"] {
        let mut out_err = create_pending_assistant_message(&model);
        let es_err = create_assistant_message_event_stream();
        process_responses_stream(
            to_json_stream(vec![json!({
                "type": "response.incomplete",
                "response": {
                    "id": "resp_err_inc",
                    "status": "incomplete",
                    "incomplete_details": { "reason": reason },
                    "usage": {
                        "input_tokens": 10,
                        "output_tokens": 2,
                        "total_tokens": 12,
                        "input_tokens_details": { "cached_tokens": 0 },
                    },
                },
            })]),
            &mut out_err,
            &es_err,
            &model,
            None,
        )
        .await
        .unwrap();
        assert_eq!(out_err.stop_reason, StopReason::Error);
        assert_eq!(
            out_err.raw_stop_reason.as_deref(),
            Some(format!("incomplete.{reason}").as_str())
        );
        assert_eq!(
            out_err.error_message.as_deref(),
            Some(format!("Response incomplete: {reason}").as_str())
        );
    }

    // response.failed
    let mut out_failed = create_pending_assistant_message(&model);
    let es_failed = create_assistant_message_event_stream();
    let failed_err = process_responses_stream(
        to_json_stream(vec![json!({
            "type": "response.failed",
            "response": {
                "id": "resp_failed",
                "status": "failed",
                "error": { "code": "server_error", "message": "boom" },
            },
        })]),
        &mut out_failed,
        &es_failed,
        &model,
        None,
    )
    .await
    .unwrap_err();
    assert!(failed_err.to_string().contains("server_error: boom"), "{failed_err}");
    assert_eq!(out_failed.raw_stop_reason.as_deref(), Some("failed"));
}
