//! Port of packages/coding-agent/test/agent-session-stats.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;

use pi_agent_core::agent::{Agent, AgentInitialState, AgentOptions};
use pi_agent_core::types::{AgentMessage, ThinkingLevel};
use pi_ai::compat::{get_model, stream_simple};
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, Model, StopReason, TextContent, ToolResultMessage, Usage, UsageCost,
    UserContentBlock, UserMessage, UserMessageContent,
};
use pi_ai::{ApiKeyCredential, Credential, CredentialStore};
use pi_coding_agent::core::agent_session::{AgentSession, AgentSessionConfig, SessionTokenStats};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::extensions::types::ContextUsage;
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::settings_manager::SettingsManager;
use pi_coding_agent::core::usage_totals::{UsageCostBreakdownEntry, get_usage_cost_breakdown};
use pretty_assertions::assert_eq;
use support::model_runtime_test_utils::{create_in_memory_model_registry, get_model_runtime};
use support::suite::harness::create_suite_resource_loader;

fn test_model() -> Model {
    get_model("anthropic", "claude-sonnet-4-5").unwrap()
}

fn make_usage(total_tokens: i64) -> Usage {
    Usage {
        input: total_tokens,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens,
        cost: UsageCost::default(),
    }
}

fn make_assistant_message(model: &Model, text: &str, total_tokens: i64, timestamp: i64) -> AssistantMessage {
    AssistantMessage {
        content: vec![AssistantContentBlock::Text(TextContent::new(text))],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: make_usage(total_tokens),
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp,
        duration_ms: None,
    }
}

fn make_user_message(text: &str, timestamp: i64) -> AgentMessage {
    let msg = UserMessage {
        content: UserMessageContent::Text(text.to_string()),
        timestamp,
    };
    pi_ai::types::Message::User(msg).into()
}

fn make_tool_result_message(usage: Usage) -> AgentMessage {
    pi_ai::types::Message::ToolResult(ToolResultMessage {
        tool_call_id: "tool-call-1".to_string(),
        tool_name: "test_tool".to_string(),
        content: vec![UserContentBlock::Text(TextContent::new("tool result"))],
        details: None,
        usage: Some(usage),
        is_error: false,
        nested_calls: None,
        timestamp: 1,
        duration_ms: None,
    })
    .into()
}

async fn create_session() -> (AgentSession, SessionManager, Model) {
    let model = test_model();
    let settings_manager = SettingsManager::in_memory(None, None);
    let session_manager = SessionManager::in_memory(None, None, None).unwrap();
    let auth_storage = Arc::new(AuthStorage::in_memory(None));
    auth_storage
        .modify(
            "anthropic",
            Box::new(|_prev| {
                Box::pin(async move {
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("test-key".to_string()),
                        env: None,
                    })))
                })
            }),
            None,
        )
        .await
        .unwrap();

    let model_registry = create_in_memory_model_registry(auth_storage).await.unwrap();
    let agent = Agent::new(AgentOptions {
        get_api_key: Some(Arc::new(|_provider| {
            Box::pin(async { Ok(Some("test-key".to_string())) })
        })),
        stream_fn: Some(Arc::new(|m, ctx, opts| {
            Box::pin(async move { Ok(stream_simple(&m, &ctx, opts)) })
        })),
        initial_state: Some(AgentInitialState {
            model: Some(model.clone()),
            system_prompt: Some("You are a helpful assistant.".to_string()),
            tools: Some(vec![]),
            thinking_level: Some(ThinkingLevel::High),
            ..Default::default()
        }),
        ..Default::default()
    });

    let session = AgentSession::new(AgentSessionConfig {
        agent,
        session_manager: session_manager.clone(),
        settings_manager,
        cwd: pi_js::env::cwd(),
        model_runtime: get_model_runtime(&model_registry),
        resource_loader: create_suite_resource_loader(None),
        scoped_models: None,
        custom_tools: None,
        cache_warmer: None,
        initial_active_tool_names: None,
        uses_default_tools: None,
        allowed_tool_names: None,
        excluded_tool_names: None,
        base_tools_override: None,
        extension_runner_ref: None,
        session_start_event: None,
        initial_model: Some(model.clone()),
        initial_thinking_level: None,
        default_tool_modifiers: None,
    });

    (session, session_manager, model)
}

fn sync_agent_messages(session: &AgentSession, session_manager: &SessionManager) {
    session
        .agent()
        .replace_messages(session_manager.build_session_context().messages);
}

#[tokio::test]
async fn exposes_the_current_context_usage_alongside_token_totals() {
    let (session, session_manager, model) = create_session().await;
    session_manager.append_message(make_user_message("hello", 1)).unwrap();
    session_manager
        .append_message(pi_ai::types::Message::Assistant(make_assistant_message(&model, "hi", 200, 2)).into())
        .unwrap();
    sync_agent_messages(&session, &session_manager);

    let stats = session.get_session_stats();
    assert_eq!(stats.context_usage, session.get_context_usage());
    assert_eq!(
        stats.context_usage,
        Some(ContextUsage {
            tokens: Some(200),
            context_window: model.context_window,
            percent: Some((200.0 / model.context_window as f64) * 100.0),
        })
    );

    session.dispose();
}

#[tokio::test]
async fn reports_unknown_current_context_usage_immediately_after_compaction() {
    let (session, session_manager, model) = create_session().await;
    session_manager.append_message(make_user_message("first", 1)).unwrap();
    session_manager
        .append_message(
            pi_ai::types::Message::Assistant(make_assistant_message(&model, "response1", 180_000, 2)).into(),
        )
        .unwrap();
    let kept_user_id = session_manager.append_message(make_user_message("second", 3)).unwrap();
    session_manager
        .append_message(
            pi_ai::types::Message::Assistant(make_assistant_message(&model, "response2", 195_000, 4)).into(),
        )
        .unwrap();
    session_manager
        .append_compaction("summary", Some(&kept_user_id), 195_000, None, None, None)
        .unwrap();
    session_manager.append_message(make_user_message("third", 5)).unwrap();
    sync_agent_messages(&session, &session_manager);

    let stats = session.get_session_stats();
    assert_eq!(stats.tokens.input, 375_000);
    let cu = stats.context_usage.unwrap();
    assert_eq!(cu.tokens, None);
    assert_eq!(cu.percent, None);

    session.dispose();
}

#[tokio::test]
async fn uses_post_compaction_usage_for_current_context_instead_of_stale_kept_usage() {
    let (session, session_manager, model) = create_session().await;
    session_manager.append_message(make_user_message("first", 1)).unwrap();
    session_manager
        .append_message(
            pi_ai::types::Message::Assistant(make_assistant_message(&model, "response1", 180_000, 2)).into(),
        )
        .unwrap();
    let kept_user_id = session_manager.append_message(make_user_message("second", 3)).unwrap();
    session_manager
        .append_message(
            pi_ai::types::Message::Assistant(make_assistant_message(&model, "response2", 195_000, 4)).into(),
        )
        .unwrap();
    session_manager
        .append_compaction("summary", Some(&kept_user_id), 195_000, None, None, None)
        .unwrap();
    session_manager.append_message(make_user_message("third", 5)).unwrap();
    session_manager
        .append_message(pi_ai::types::Message::Assistant(make_assistant_message(&model, "response3", 25_000, 6)).into())
        .unwrap();
    sync_agent_messages(&session, &session_manager);

    let stats = session.get_session_stats();
    assert_eq!(stats.tokens.input, 400_000);
    let cu = stats.context_usage.unwrap();
    assert_eq!(cu.tokens, Some(25_000));
    assert_eq!(cu.percent, Some((25_000.0 / model.context_window as f64) * 100.0));

    session.dispose();
}

#[tokio::test]
async fn includes_branch_summary_usage_in_session_totals() {
    let (session, session_manager, _model) = create_session().await;
    session_manager
        .branch_with_summary(
            None,
            "summary",
            None,
            Some(false),
            Some(Usage {
                input: 10,
                output: 20,
                cache_read: 30,
                cache_write: 40,
                cache_write_1h: None,
                reasoning: None,
                total_tokens: 100,
                cost: UsageCost {
                    input: 0.1,
                    output: 0.2,
                    cache_read: 0.3,
                    cache_write: 0.4,
                    total: 1.0,
                    total_from_provider: None,
                },
            }),
        )
        .unwrap();
    sync_agent_messages(&session, &session_manager);

    let stats = session.get_session_stats();
    assert_eq!(
        stats.tokens,
        SessionTokenStats {
            input: 10,
            output: 20,
            cache_read: 30,
            cache_write: 40,
            total: 100,
        }
    );
    assert_eq!(stats.cost, 1.0);

    session.dispose();
}

#[tokio::test]
async fn includes_compaction_usage_in_session_totals() {
    let (session, session_manager, _model) = create_session().await;
    let first_kept = session_manager.append_message(make_user_message("hello", 1)).unwrap();
    session_manager
        .append_compaction(
            "summary",
            Some(&first_kept),
            100,
            None,
            Some(false),
            Some(Usage {
                input: 10,
                output: 20,
                cache_read: 30,
                cache_write: 40,
                cache_write_1h: None,
                reasoning: None,
                total_tokens: 100,
                cost: UsageCost {
                    input: 0.1,
                    output: 0.2,
                    cache_read: 0.3,
                    cache_write: 0.4,
                    total: 1.0,
                    total_from_provider: None,
                },
            }),
        )
        .unwrap();
    sync_agent_messages(&session, &session_manager);

    let stats = session.get_session_stats();
    assert_eq!(
        stats.tokens,
        SessionTokenStats {
            input: 10,
            output: 20,
            cache_read: 30,
            cache_write: 40,
            total: 100,
        }
    );
    assert_eq!(stats.cost, 1.0);

    session.dispose();
}

#[tokio::test]
async fn includes_cache_warming_usage_exactly_once_without_adding_messages() {
    let (session, session_manager, model) = create_session().await;
    session_manager
        .append_usage(
            "cache_warm",
            "anthropic",
            &model.id,
            Usage {
                input: 2,
                output: 1,
                cache_read: 97,
                cache_write: 0,
                cache_write_1h: None,
                reasoning: None,
                total_tokens: 100,
                cost: UsageCost {
                    input: 0.001,
                    output: 0.002,
                    cache_read: 0.007,
                    cache_write: 0.0,
                    total: 0.01,
                    total_from_provider: None,
                },
            },
            Some("extension override"),
        )
        .unwrap();

    let entries = session_manager.get_entries();
    assert_eq!(entries.len(), 1);
    match &entries[0] {
        pi_coding_agent::core::session_manager::SessionEntry::Usage(u) => {
            assert_eq!(u.kind, "cache_warm");
            assert_eq!(u.note.as_deref(), Some("extension override"));
        }
        other => panic!("expected Usage entry, got {other:?}"),
    }

    let stats = session.get_session_stats();
    assert_eq!(
        stats.tokens,
        SessionTokenStats {
            input: 2,
            output: 1,
            cache_read: 97,
            cache_write: 0,
            total: 100,
        }
    );
    assert_eq!(stats.total_messages, 0);
    assert!(session_manager.build_session_context().messages.is_empty());
    assert_eq!(
        get_usage_cost_breakdown(&session_manager.get_entries()),
        vec![UsageCostBreakdownEntry {
            key: format!("anthropic/{}", model.id),
            cost: 0.01,
            tokens: 100,
        }]
    );

    session.dispose();
}

#[tokio::test]
async fn includes_tool_result_usage_in_session_totals() {
    let (session, session_manager, _model) = create_session().await;
    session_manager
        .append_message(make_tool_result_message(Usage {
            input: 10,
            output: 20,
            cache_read: 30,
            cache_write: 40,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 100,
            cost: UsageCost {
                input: 0.1,
                output: 0.2,
                cache_read: 0.3,
                cache_write: 0.4,
                total: 1.0,
                total_from_provider: None,
            },
        }))
        .unwrap();
    sync_agent_messages(&session, &session_manager);

    let stats = session.get_session_stats();
    assert_eq!(
        stats.tokens,
        SessionTokenStats {
            input: 10,
            output: 20,
            cache_read: 30,
            cache_write: 40,
            total: 100,
        }
    );
    assert_eq!(stats.cost, 1.0);

    session.dispose();
}

#[test]
fn groups_tool_and_summary_usage_separately_from_model_attributed_usage() {
    let model = test_model();
    let session_manager = SessionManager::in_memory(None, None, None).unwrap();
    let root_id = session_manager.append_message(make_user_message("hello", 1)).unwrap();

    let mut assistant = make_assistant_message(&model, "response", 100, 2);
    assistant.usage.cost.total = 0.5;
    session_manager
        .append_message(pi_ai::types::Message::Assistant(assistant).into())
        .unwrap();

    let mut tr_usage = make_usage(100);
    tr_usage.cost.total = 1.0;
    session_manager
        .append_message(make_tool_result_message(tr_usage))
        .unwrap();

    let mut comp_usage = make_usage(100);
    comp_usage.cost.total = 2.0;
    session_manager
        .append_compaction("summary", Some(&root_id), 100, None, Some(false), Some(comp_usage))
        .unwrap();

    let mut branch_usage = make_usage(100);
    branch_usage.cost.total = 3.0;
    session_manager
        .branch_with_summary(None, "branch summary", None, Some(false), Some(branch_usage))
        .unwrap();

    assert_eq!(
        get_usage_cost_breakdown(&session_manager.get_entries()),
        vec![
            UsageCostBreakdownEntry {
                key: "Tools/summaries".to_string(),
                cost: 6.0,
                tokens: 300,
            },
            UsageCostBreakdownEntry {
                key: format!("{}/{}", model.provider, model.id),
                cost: 0.5,
                tokens: 100,
            },
        ]
    );
}

#[tokio::test]
async fn ignores_zero_usage_messages_when_checking_for_post_compaction_context_usage() {
    let (session, session_manager, model) = create_session().await;
    session_manager.append_message(make_user_message("first", 1)).unwrap();
    session_manager
        .append_message(
            pi_ai::types::Message::Assistant(make_assistant_message(&model, "response1", 180_000, 2)).into(),
        )
        .unwrap();
    let kept_user_id = session_manager.append_message(make_user_message("second", 3)).unwrap();
    session_manager
        .append_message(
            pi_ai::types::Message::Assistant(make_assistant_message(&model, "response2", 195_000, 4)).into(),
        )
        .unwrap();
    session_manager
        .append_compaction("summary", Some(&kept_user_id), 195_000, None, None, None)
        .unwrap();
    session_manager.append_message(make_user_message("third", 5)).unwrap();
    session_manager
        .append_message(pi_ai::types::Message::Assistant(make_assistant_message(&model, "response3", 25_000, 6)).into())
        .unwrap();
    session_manager
        .append_message(make_user_message("continue", 7))
        .unwrap();
    session_manager
        .append_message(pi_ai::types::Message::Assistant(make_assistant_message(&model, "partial", 0, 8)).into())
        .unwrap();
    sync_agent_messages(&session, &session_manager);

    let stats = session.get_session_stats();
    let cu = stats.context_usage.unwrap();
    assert!(cu.tokens.is_some_and(|t| t > 25_000));

    session.dispose();
}
