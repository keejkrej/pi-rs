//! Port of `packages/coding-agent/test/compaction-summary-reasoning.test.ts`.

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_agent_core::{AgentMessage, StreamFn, ThinkingLevel};
use pi_ai::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, CacheRetention,
    Context, JsonObject, Model, ModelCompat, ModelCost, ModelInput, SimpleStreamOptions, StopReason, Usage,
    UserMessage, UserMessageContent, faux_text, faux_tool_call, normalize_context,
};
use pi_coding_agent::core::compaction::utils::create_file_ops;
use pi_coding_agent::core::compaction::{
    CompactOptions, CompactionPreparation, CompactionSettings, GenerateSummaryOptions, compact, complete_summarization,
    generate_summary, generate_summary_with_usage,
};
use serde_json::json;

fn create_model(reasoning: bool, max_tokens: i64) -> Model {
    Model {
        r#type: None,
        id: if reasoning {
            "reasoning-model".to_owned()
        } else {
            "non-reasoning-model".to_owned()
        },
        name: if reasoning {
            "Reasoning Model".to_owned()
        } else {
            "Non-reasoning Model".to_owned()
        },
        api: "anthropic-messages".to_owned(),
        provider: "anthropic".to_owned(),
        base_url: "https://api.anthropic.com".to_owned(),
        reasoning,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        context_window: 200_000,
        max_tokens,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

fn mock_summary_response() -> AssistantMessage {
    AssistantMessage {
        content: vec![AssistantContentBlock::Text(faux_text("## Goal\nTest summary"))],
        api: "anthropic-messages".to_owned(),
        provider: "anthropic".to_owned(),
        model: "claude-sonnet-4-5".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            input: 10,
            output: 10,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 20,
            cost: Default::default(),
        },
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 0,
    }
}

fn mock_tool_call_response() -> AssistantMessage {
    let mut args = JsonObject::new();
    args.insert("path".to_owned(), json!("README.md"));
    let mut resp = mock_summary_response();
    resp.content = vec![AssistantContentBlock::ToolCall(faux_tool_call("read", args, None))];
    resp.stop_reason = StopReason::ToolUse;
    resp
}

fn sample_messages() -> Vec<AgentMessage> {
    vec![AgentMessage::Message(pi_ai::Message::User(UserMessage {
        content: UserMessageContent::Text("Summarize this.".to_owned()),
        timestamp: 0,
    }))]
}

#[derive(Clone)]
struct RecordedCall {
    context: Context,
    options: Option<SimpleStreamOptions>,
}

fn scripted_stream(responses: Vec<AssistantMessage>) -> (StreamFn, Arc<Mutex<Vec<RecordedCall>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let queue = Arc::new(Mutex::new(responses.into_iter()));
    let calls_ref = Arc::clone(&calls);

    let stream_fn: StreamFn = Arc::new(move |_model, context, options| {
        let calls_inner = Arc::clone(&calls_ref);
        let queue_inner = Arc::clone(&queue);
        Box::pin(async move {
            calls_inner.lock().unwrap().push(RecordedCall { context, options });
            let msg = queue_inner.lock().unwrap().next().unwrap_or_else(mock_summary_response);
            let stream = AssistantMessageEventStream::new();
            let reason = msg.stop_reason;
            if reason == StopReason::Error || reason == StopReason::Aborted {
                stream.push(AssistantMessageEvent::Error { reason, error: msg });
            } else {
                stream.push(AssistantMessageEvent::Done { reason, message: msg });
            }
            Ok(stream)
        })
    });

    (stream_fn, calls)
}

#[tokio::test]
async fn uses_the_provided_thinking_level_for_reasoning_capable_models() {
    let (stream_fn, calls) = scripted_stream(vec![mock_summary_response()]);
    let result = generate_summary_with_usage(
        &sample_messages(),
        &create_model(true, 8192),
        2000,
        Some(GenerateSummaryOptions {
            api_key: Some("test-key".to_owned()),
            thinking_level: Some(ThinkingLevel::Medium),
            stream_fn: Some(stream_fn),
            ..Default::default()
        }),
    )
    .await
    .unwrap();

    assert_eq!(result.text, "## Goal\nTest summary");
    assert_eq!(result.usage, mock_summary_response().usage);

    let recorded = calls.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    let opts = recorded[0].options.as_ref().unwrap();
    assert_eq!(opts.reasoning, Some(pi_ai::ThinkingLevel::Medium));
    assert_eq!(opts.base.api_key.as_deref(), Some("test-key"));
}

#[tokio::test]
async fn preserves_the_string_result_from_generate_summary() {
    let (stream_fn, _calls) = scripted_stream(vec![mock_summary_response()]);
    let text = generate_summary(
        &sample_messages(),
        &create_model(false, 8192),
        2000,
        Some(GenerateSummaryOptions {
            api_key: Some("test-key".to_owned()),
            stream_fn: Some(stream_fn),
            ..Default::default()
        }),
    )
    .await
    .unwrap();

    assert_eq!(text, "## Goal\nTest summary");
}

#[tokio::test]
async fn uses_fresh_routing_sessions_without_prompt_caching() {
    let (stream_fn, calls) = scripted_stream(vec![mock_summary_response(), mock_summary_response()]);
    generate_summary(
        &sample_messages(),
        &create_model(false, 8192),
        2000,
        Some(GenerateSummaryOptions {
            api_key: Some("test-key".to_owned()),
            stream_fn: Some(Arc::clone(&stream_fn)),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    generate_summary(
        &sample_messages(),
        &create_model(false, 8192),
        2000,
        Some(GenerateSummaryOptions {
            api_key: Some("test-key".to_owned()),
            stream_fn: Some(stream_fn),
            ..Default::default()
        }),
    )
    .await
    .unwrap();

    let recorded = calls.lock().unwrap();
    assert_eq!(recorded.len(), 2);
    assert!(
        recorded
            .iter()
            .all(|c| c.options.as_ref().unwrap().base.cache_retention == Some(CacheRetention::None))
    );
    let s0 = recorded[0].options.as_ref().unwrap().base.session_id.as_ref().unwrap();
    let s1 = recorded[1].options.as_ref().unwrap().base.session_id.as_ref().unwrap();
    assert_ne!(s0, s1);
}

#[tokio::test]
async fn honors_caller_supplied_routing_session_and_tool_choice_without_prompt_caching() {
    let (stream_fn, calls) = scripted_stream(vec![mock_summary_response()]);
    let ctx = normalize_context(&Context {
        system_prompt: Some("Summarize".to_owned()),
        messages: Vec::new(),
        tools: None,
    });
    let mut opts = SimpleStreamOptions::default();
    opts.base.session_id = Some("current-routing-session".to_owned());
    opts.base.cache_retention = Some(CacheRetention::Long);
    opts.tool_choice = Some(pi_ai::ToolChoice::Auto);

    complete_summarization(&create_model(false, 8192), &ctx, opts, Some(&stream_fn), None, None)
        .await
        .unwrap();

    let recorded = calls.lock().unwrap();
    let sent = recorded[0].options.as_ref().unwrap();
    assert_eq!(sent.base.session_id.as_deref(), Some("current-routing-session"));
    assert_eq!(sent.base.cache_retention, Some(CacheRetention::None));
    assert_eq!(sent.tool_choice, Some(pi_ai::ToolChoice::Auto));
}

#[tokio::test]
async fn preserves_the_previous_summary_without_empty_history_request_for_split_turn() {
    let (stream_fn, calls) = scripted_stream(vec![mock_summary_response()]);
    let preparation = CompactionPreparation {
        first_kept_entry_id: "entry-keep".to_owned(),
        messages_to_summarize: Vec::new(),
        turn_prefix_messages: sample_messages(),
        is_split_turn: true,
        tokens_before: 100,
        previous_summary: Some("previous checkpoint".to_owned()),
        file_ops: create_file_ops(),
        settings: CompactionSettings {
            enabled: true,
            reserve_tokens: 2000,
            keep_recent_tokens: 20,
        },
    };

    let result = compact(
        &preparation,
        &create_model(false, 8192),
        Some(CompactOptions {
            api_key: Some("test-key".to_owned()),
            stream_fn: Some(stream_fn),
            ..Default::default()
        }),
    )
    .await
    .unwrap();

    let recorded = calls.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert!(result.summary.contains("previous checkpoint"));
    let prompt = pi_js::json::stringify(&recorded[0].context.messages);
    assert!(prompt.contains("# Conversation\\n[User]: Summarize this."));
    assert!(prompt.contains("# Instructions\\nThe messages above are earlier context from an ongoing conversation."));
}

#[tokio::test]
async fn rejects_tool_calls_from_conversation_summaries() {
    let (stream_fn, _) = scripted_stream(vec![mock_tool_call_response()]);
    let err = generate_summary_with_usage(
        &sample_messages(),
        &create_model(false, 8192),
        2000,
        Some(GenerateSummaryOptions {
            api_key: Some("test-key".to_owned()),
            stream_fn: Some(stream_fn),
            ..Default::default()
        }),
    )
    .await
    .unwrap_err();

    assert!(err.to_string().contains("Summarization attempted to call a tool"));
}

#[tokio::test]
async fn rejects_tool_calls_from_split_turn_summaries() {
    let (stream_fn, _) = scripted_stream(vec![mock_tool_call_response()]);
    let preparation = CompactionPreparation {
        first_kept_entry_id: "entry-keep".to_owned(),
        messages_to_summarize: Vec::new(),
        turn_prefix_messages: sample_messages(),
        is_split_turn: true,
        tokens_before: 100,
        previous_summary: None,
        file_ops: create_file_ops(),
        settings: CompactionSettings {
            enabled: true,
            reserve_tokens: 2000,
            keep_recent_tokens: 20,
        },
    };

    let err = compact(
        &preparation,
        &create_model(false, 8192),
        Some(CompactOptions {
            api_key: Some("test-key".to_owned()),
            stream_fn: Some(stream_fn),
            ..Default::default()
        }),
    )
    .await
    .unwrap_err();

    assert!(
        err.to_string()
            .contains("Turn prefix summarization attempted to call a tool")
    );
}

#[tokio::test]
async fn rejects_a_length_limited_history_summary() {
    let mut partial = mock_summary_response();
    partial.stop_reason = StopReason::Length;
    partial.content = vec![AssistantContentBlock::Text(faux_text("partial"))];
    let (stream_fn, _) = scripted_stream(vec![partial]);

    let err = generate_summary_with_usage(
        &sample_messages(),
        &create_model(false, 8192),
        2000,
        Some(GenerateSummaryOptions {
            api_key: Some("test-key".to_owned()),
            stream_fn: Some(stream_fn),
            ..Default::default()
        }),
    )
    .await
    .unwrap_err();

    assert!(err.to_string().contains("generation hit the token cap"));
}

#[tokio::test]
async fn rejects_a_length_limited_split_turn_summary() {
    let mut partial = mock_summary_response();
    partial.stop_reason = StopReason::Length;
    partial.content = vec![AssistantContentBlock::Text(faux_text("partial"))];
    let (stream_fn, _) = scripted_stream(vec![partial]);
    let preparation = CompactionPreparation {
        first_kept_entry_id: "entry-keep".to_owned(),
        messages_to_summarize: Vec::new(),
        turn_prefix_messages: sample_messages(),
        is_split_turn: true,
        tokens_before: 100,
        previous_summary: None,
        file_ops: create_file_ops(),
        settings: CompactionSettings {
            enabled: true,
            reserve_tokens: 2000,
            keep_recent_tokens: 20,
        },
    };

    let err = compact(
        &preparation,
        &create_model(false, 8192),
        Some(CompactOptions {
            api_key: Some("test-key".to_owned()),
            stream_fn: Some(stream_fn),
            ..Default::default()
        }),
    )
    .await
    .unwrap_err();

    assert!(err.to_string().contains("generation hit the token cap"));
}

#[tokio::test]
async fn does_not_set_reasoning_when_thinking_is_off() {
    let (stream_fn, calls) = scripted_stream(vec![mock_summary_response()]);
    generate_summary(
        &sample_messages(),
        &create_model(true, 8192),
        2000,
        Some(GenerateSummaryOptions {
            api_key: Some("test-key".to_owned()),
            thinking_level: Some(ThinkingLevel::Off),
            stream_fn: Some(stream_fn),
            ..Default::default()
        }),
    )
    .await
    .unwrap();

    let recorded = calls.lock().unwrap();
    let sent = recorded[0].options.as_ref().unwrap();
    assert_eq!(sent.base.api_key.as_deref(), Some("test-key"));
    assert_eq!(sent.reasoning, None);
}

#[tokio::test]
async fn does_not_set_reasoning_for_non_reasoning_models() {
    let (stream_fn, calls) = scripted_stream(vec![mock_summary_response()]);
    generate_summary(
        &sample_messages(),
        &create_model(false, 8192),
        2000,
        Some(GenerateSummaryOptions {
            api_key: Some("test-key".to_owned()),
            thinking_level: Some(ThinkingLevel::Medium),
            stream_fn: Some(stream_fn),
            ..Default::default()
        }),
    )
    .await
    .unwrap();

    let recorded = calls.lock().unwrap();
    let sent = recorded[0].options.as_ref().unwrap();
    assert_eq!(sent.base.api_key.as_deref(), Some("test-key"));
    assert_eq!(sent.reasoning, None);
}

#[tokio::test]
async fn leaves_anthropic_refusal_fallback_and_clamps_max_tokens() {
    let mut model = create_model(true, 8192);
    let mut anthropic_compat = pi_ai::AnthropicMessagesCompat::default();
    anthropic_compat.allowed_fallback_models = Some(vec![pi_ai::AnthropicAllowedFallbackModel {
        provider: "anthropic".to_owned(),
        model: "claude-opus-4-8".to_owned(),
        cost: ModelCost {
            input: 5.0,
            output: 25.0,
            cache_read: 0.5,
            cache_write: 6.25,
            tiers: None,
        },
    }]);
    model.compat = Some(ModelCompat::AnthropicMessages(anthropic_compat));
    let _ = IndexMap::<String, String>::new();

    let (stream_fn, calls) = scripted_stream(vec![mock_summary_response()]);
    generate_summary(
        &sample_messages(),
        &model,
        2000,
        Some(GenerateSummaryOptions {
            api_key: Some("test-key".to_owned()),
            stream_fn: Some(stream_fn),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(calls.lock().unwrap().len(), 1);

    let (stream_fn2, calls2) = scripted_stream(vec![mock_summary_response(), mock_summary_response()]);
    let preparation = CompactionPreparation {
        first_kept_entry_id: "entry-keep".to_owned(),
        messages_to_summarize: sample_messages(),
        turn_prefix_messages: sample_messages(),
        is_split_turn: true,
        tokens_before: 600_000,
        previous_summary: None,
        file_ops: create_file_ops(),
        settings: CompactionSettings {
            enabled: true,
            reserve_tokens: 500_000,
            keep_recent_tokens: 20_000,
        },
    };
    let result = compact(
        &preparation,
        &create_model(false, 128_000),
        Some(CompactOptions {
            api_key: Some("test-key".to_owned()),
            stream_fn: Some(stream_fn2),
            ..Default::default()
        }),
    )
    .await
    .unwrap();

    let usage = result.usage.unwrap();
    assert_eq!(usage.input, 20);
    assert_eq!(usage.output, 20);
    assert_eq!(usage.total_tokens, 40);
    let max_tokens: Vec<Option<i64>> = calls2
        .lock()
        .unwrap()
        .iter()
        .map(|c| c.options.as_ref().and_then(|o| o.base.max_tokens))
        .collect();
    assert_eq!(max_tokens, vec![Some(128_000), Some(128_000)]);
}
