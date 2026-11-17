//! Port of `packages/coding-agent/test/suite/regressions/6768-copilot-compaction-base-url.test.ts`.
// WIRE(@mcp-helper2): full ModelRuntime OAuth baseUrl resolution flows through `ModelRuntime::stream_simple` once `core::model_runtime` lands.

use std::sync::{Arc, Mutex};

use pi_agent_core::{AgentMessage, StreamFn};
use pi_ai::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, Message, Model,
    ModelCost, ModelInput, StopReason, Usage, UserMessage, UserMessageContent, faux_text,
};
use pi_coding_agent::core::compaction::{CompactOptions, CompactionSettings, compact, prepare_compaction};
use pi_coding_agent::core::session_manager::SessionManager;

const INDIVIDUAL_BASE_URL: &str = "https://api.individual.githubcopilot.com";
const ENTERPRISE_BASE_URL: &str = "https://api.enterprise.githubcopilot.com";

fn base_model(base_url: &str) -> Model {
    Model {
        r#type: None,
        id: "gpt-4o".to_owned(),
        name: "Copilot GPT-4o".to_owned(),
        api: "openai-completions".to_owned(),
        provider: "github-copilot".to_owned(),
        base_url: base_url.to_owned(),
        reasoning: false,
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
        context_window: 128_000,
        max_tokens: 4096,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

#[tokio::test]
async fn compaction_forwards_resolved_enterprise_base_url_through_stream_wrapper() {
    let sm = SessionManager::in_memory(None, None, None).unwrap();
    sm.append_message(AgentMessage::Message(Message::User(UserMessage {
        content: UserMessageContent::Text("message to compact".to_owned()),
        timestamp: 1000,
    })))
    .unwrap();
    sm.append_message(AgentMessage::Message(Message::Assistant(AssistantMessage {
        content: vec![AssistantContentBlock::Text(faux_text("assistant response to compact"))],
        api: "openai-completions".to_owned(),
        provider: "github-copilot".to_owned(),
        model: "gpt-4o".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            input: 100,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 100,
            cost: Default::default(),
        },
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1500,
    })))
    .unwrap();

    let prep = prepare_compaction(
        &sm.get_branch(None),
        &CompactionSettings {
            enabled: true,
            reserve_tokens: 16_384,
            keep_recent_tokens: 1,
        },
    )
    .unwrap();

    let seen_base_url: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let seen_ref = Arc::clone(&seen_base_url);

    // Simulates ModelRuntime::stream_simple resolving the OAuth enterprise baseUrl over the catalog baseUrl.
    let stream_fn: StreamFn = Arc::new(move |mut model, _ctx, _opts| {
        let seen = Arc::clone(&seen_ref);
        Box::pin(async move {
            if model.provider == "github-copilot" && model.base_url == INDIVIDUAL_BASE_URL {
                model.base_url = ENTERPRISE_BASE_URL.to_owned();
            }
            *seen.lock().unwrap() = Some(model.base_url.clone());

            let stream = AssistantMessageEventStream::new();
            stream.push(AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message: AssistantMessage {
                    content: vec![AssistantContentBlock::Text(faux_text("summary"))],
                    api: model.api,
                    provider: model.provider,
                    model: model.id,
                    response_model: None,
                    response_id: None,
                    provider_thinking_level: None,
                    thinking_level: None,
                    diagnostics: None,
                    usage: Usage::default(),
                    stop_reason: StopReason::Stop,
                    deferred: None,
                    error_message: None,
                    raw_stop_reason: None,
                    end_turn: None,
                    timestamp: 0,
                },
            });
            Ok(stream)
        })
    });

    let res = compact(
        &prep,
        &base_model(INDIVIDUAL_BASE_URL),
        Some(CompactOptions {
            stream_fn: Some(stream_fn),
            ..Default::default()
        }),
    )
    .await
    .unwrap();

    assert!(res.summary.contains("summary"));
    assert_eq!(seen_base_url.lock().unwrap().as_deref(), Some(ENTERPRISE_BASE_URL));
}
