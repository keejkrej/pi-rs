//! Port of packages/coding-agent/test/branch-summarization.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_agent_core::{AgentMessage, StreamFn, ThinkingLevel};
use pi_ai::providers::faux::{FauxAssistantMessageContent, faux_assistant_message};
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Model, ModelCost, ModelInput, StopReason,
    TextContent, ToolCall, Usage, UserMessage, UserMessageContent,
};
use pi_ai::utils::event_stream::create_assistant_message_event_stream;
use pi_coding_agent::core::compaction::{
    GenerateBranchSummaryOptions, generate_branch_summary, prepare_branch_entries,
};
use pi_coding_agent::core::session_manager::{
    ContextEditEntry, ContextEditReplacement, ContextEditableContent, SessionEntry, SessionMessageEntry,
};
use pi_js::abort::AbortController;
use serde_json::json;

fn test_model(reasoning: bool) -> Model {
    Model {
        r#type: None,
        id: "test-model".to_owned(),
        name: "Test Model".to_owned(),
        api: "anthropic-messages".to_owned(),
        provider: "anthropic".to_owned(),
        base_url: "https://api.anthropic.com".to_owned(),
        reasoning,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 200_000,
        max_tokens: 8192,
        headers: None,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    }
}

fn user_entry() -> Vec<SessionEntry> {
    vec![SessionEntry::Message(SessionMessageEntry {
        id: "branch-user".to_owned(),
        parent_id: None,
        timestamp: "1970-01-01T00:00:00.001Z".to_owned(),
        message: AgentMessage::Message(pi_ai::Message::User(UserMessage {
            content: UserMessageContent::Text("Abandoned request".to_owned()),
            timestamp: 1,
        })),
    })]
}

fn response(model: &Model, content: Vec<AssistantContentBlock>, stop_reason: StopReason) -> AssistantMessage {
    let mut msg = faux_assistant_message(FauxAssistantMessageContent::Text(String::new()), None);
    msg.content = content;
    msg.api = model.api.clone();
    msg.provider = model.provider.clone();
    msg.model = model.id.clone();
    msg.stop_reason = stop_reason;
    msg.usage = Usage::default();
    msg
}

#[tokio::test]
async fn passes_thinking_level_only_for_reasoning_capable_models() {
    let seen: Arc<Mutex<Vec<Option<pi_ai::ThinkingLevel>>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_c = Arc::clone(&seen);
    let stream_fn: StreamFn = Arc::new(move |model, _ctx, opts| {
        let seen_c = Arc::clone(&seen_c);
        Box::pin(async move {
            seen_c.lock().unwrap().push(opts.as_ref().and_then(|o| o.reasoning));
            let stream = create_assistant_message_event_stream();
            let done_msg = response(
                &model,
                vec![AssistantContentBlock::Text(TextContent::new("summary"))],
                StopReason::Stop,
            );
            stream.push(AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message: done_msg,
            });
            Ok(stream)
        })
    });

    let reasoning_model = test_model(true);
    let non_reasoning_model = test_model(false);
    let signal = AbortController::new().signal();

    let mut opts1 = GenerateBranchSummaryOptions::new(reasoning_model.clone());
    opts1.signal = Some(signal.clone());
    opts1.stream_fn = Some(stream_fn.clone());
    opts1.thinking_level = Some(ThinkingLevel::Xhigh);
    generate_branch_summary(&user_entry(), opts1).await;

    let mut opts2 = GenerateBranchSummaryOptions::new(reasoning_model);
    opts2.signal = Some(signal.clone());
    opts2.stream_fn = Some(stream_fn.clone());
    opts2.thinking_level = Some(ThinkingLevel::Off);
    generate_branch_summary(&user_entry(), opts2).await;

    let mut opts3 = GenerateBranchSummaryOptions::new(non_reasoning_model);
    opts3.signal = Some(signal);
    opts3.stream_fn = Some(stream_fn);
    opts3.thinking_level = Some(ThinkingLevel::High);
    generate_branch_summary(&user_entry(), opts3).await;

    assert_eq!(
        *seen.lock().unwrap(),
        vec![Some(pi_ai::ThinkingLevel::XHigh), None, None]
    );
}

#[tokio::test]
async fn does_not_override_tool_choice_for_branch_summaries() {
    let seen_tool_choice: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let seen_c = Arc::clone(&seen_tool_choice);
    let stream_fn: StreamFn = Arc::new(move |model, _ctx, opts| {
        let seen_c = Arc::clone(&seen_c);
        Box::pin(async move {
            if let Some(tc) = opts.as_ref().and_then(|o| o.tool_choice.as_ref()) {
                *seen_c.lock().unwrap() = Some(format!("{tc:?}"));
            }
            let stream = create_assistant_message_event_stream();
            let msg = response(
                &model,
                vec![AssistantContentBlock::Text(TextContent::new("summary"))],
                StopReason::Stop,
            );
            stream.push(AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message: msg,
            });
            Ok(stream)
        })
    });

    let mut opts = GenerateBranchSummaryOptions::new(test_model(false));
    opts.signal = Some(AbortController::new().signal());
    opts.stream_fn = Some(stream_fn);
    generate_branch_summary(&user_entry(), opts).await;
    assert_eq!(*seen_tool_choice.lock().unwrap(), None);
}

#[tokio::test]
async fn rejects_tool_calls_from_branch_summaries() {
    let stream_fn: StreamFn = Arc::new(|model, _ctx, _opts| {
        Box::pin(async move {
            let stream = create_assistant_message_event_stream();
            let mut args = IndexMap::new();
            args.insert("path".to_owned(), json!("README.md"));
            let msg = response(
                &model,
                vec![AssistantContentBlock::ToolCall(ToolCall::new(
                    "tool-call-1",
                    "read",
                    args,
                ))],
                StopReason::ToolUse,
            );
            stream.push(AssistantMessageEvent::Done {
                reason: StopReason::ToolUse,
                message: msg,
            });
            Ok(stream)
        })
    });

    let mut opts = GenerateBranchSummaryOptions::new(test_model(false));
    opts.signal = Some(AbortController::new().signal());
    opts.stream_fn = Some(stream_fn);
    let result = generate_branch_summary(&user_entry(), opts).await;
    assert_eq!(
        result.error.as_deref(),
        Some("Branch summarization attempted to call a tool")
    );
}

#[tokio::test]
async fn rejects_length_limited_branch_summaries() {
    let stream_fn: StreamFn = Arc::new(|model, _ctx, _opts| {
        Box::pin(async move {
            let stream = create_assistant_message_event_stream();
            let msg = response(
                &model,
                vec![AssistantContentBlock::Text(TextContent::new("partial"))],
                StopReason::Length,
            );
            stream.push(AssistantMessageEvent::Done {
                reason: StopReason::Length,
                message: msg,
            });
            Ok(stream)
        })
    });

    let mut opts = GenerateBranchSummaryOptions::new(test_model(false));
    opts.signal = Some(AbortController::new().signal());
    opts.stream_fn = Some(stream_fn);
    let result = generate_branch_summary(&user_entry(), opts).await;
    assert_eq!(
        result.error.as_deref(),
        Some("Branch summarization failed: generation hit the token cap and the summary is incomplete")
    );
}

#[test]
fn applies_context_edits_to_branch_entries() {
    let mut failed_msg = faux_assistant_message(FauxAssistantMessageContent::Text("FAILED_ATTEMPT".to_owned()), None);
    failed_msg.stop_reason = StopReason::Error;
    failed_msg.error_message = Some("lost".to_owned());

    let branch = vec![
        SessionEntry::Message(SessionMessageEntry {
            id: "u1".to_owned(),
            parent_id: None,
            timestamp: "1970-01-01T00:00:00.001Z".to_owned(),
            message: AgentMessage::Message(pi_ai::Message::User(UserMessage {
                content: UserMessageContent::Text("Investigate login".to_owned()),
                timestamp: 1,
            })),
        }),
        SessionEntry::Message(SessionMessageEntry {
            id: "failed".to_owned(),
            parent_id: Some("u1".to_owned()),
            timestamp: "1970-01-01T00:00:00.002Z".to_owned(),
            message: AgentMessage::Message(pi_ai::Message::Assistant(failed_msg)),
        }),
        SessionEntry::ContextEdit(ContextEditEntry {
            id: "e1".to_owned(),
            parent_id: Some("failed".to_owned()),
            timestamp: "1970-01-01T00:00:00.003Z".to_owned(),
            target_id: "failed".to_owned(),
            replacement: None,
        }),
        SessionEntry::Message(SessionMessageEntry {
            id: "ok".to_owned(),
            parent_id: Some("e1".to_owned()),
            timestamp: "1970-01-01T00:00:00.004Z".to_owned(),
            message: AgentMessage::Message(pi_ai::Message::Assistant(faux_assistant_message(
                FauxAssistantMessageContent::Text("ORIGINAL_TEXT".to_owned()),
                None,
            ))),
        }),
        SessionEntry::ContextEdit(ContextEditEntry {
            id: "e2".to_owned(),
            parent_id: Some("ok".to_owned()),
            timestamp: "1970-01-01T00:00:00.005Z".to_owned(),
            target_id: "ok".to_owned(),
            replacement: Some(ContextEditReplacement {
                content: ContextEditableContent::Text("REPLACED_TEXT".to_owned()),
            }),
        }),
    ];

    let serialized = serde_json::to_string(&prepare_branch_entries(&branch, 0).messages).unwrap();
    assert!(serialized.contains("Investigate login"));
    assert!(!serialized.contains("FAILED_ATTEMPT"));
    assert!(!serialized.contains("ORIGINAL_TEXT"));
    assert!(serialized.contains("REPLACED_TEXT"));
}
