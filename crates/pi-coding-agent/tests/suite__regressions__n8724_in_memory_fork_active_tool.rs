//! Port of packages/coding-agent/test/suite/regressions/8724-in-memory-fork-active-tool.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex, PoisonError};

use pi_agent_core::types::{AgentMessage, AgentTool, AgentToolResult};
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{StopReason, TextContent, UserContentBlock};
use pi_coding_agent::core::agent_session_runtime::{
    AgentSessionRuntime, CreateAgentSessionFromServicesOptions, CreateAgentSessionRuntimeFactory,
    CreateAgentSessionRuntimeFactoryOptions, CreateAgentSessionRuntimeResult, create_agent_session_from_services,
};
use pi_coding_agent::core::agent_session_services::AgentSessionServices;
use pi_coding_agent::core::extensions::types::SessionEntry;
use pi_coding_agent::core::sdk::NoToolsMode;
use pi_js::vendor::typebox::t;
use pretty_assertions::assert_eq;
use serde_json::json;
use support::suite::harness::{HarnessOptions, create_harness};
use tokio::sync::Notify;

fn roles(messages: &[AgentMessage]) -> Vec<&'static str> {
    messages
        .iter()
        .map(|m| match m.role() {
            Some("system") => "system",
            Some("user") => "user",
            Some("assistant") => "assistant",
            Some("toolResult") => "toolResult",
            Some("custom") => "custom",
            _ => "unknown",
        })
        .collect()
}

#[tokio::test]
async fn waits_for_active_prompt_before_resetting_in_memory_fork_session() {
    let tool_started = Arc::new(Notify::new());
    let ts_c = Arc::clone(&tool_started);

    let wait_tool = AgentTool {
        name: "wait".to_string(),
        label: "Wait".to_string(),
        description: "Wait until aborted".to_string(),
        parameters: t::object(&[]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_id, _params, signal, _upd| {
            let started = Arc::clone(&ts_c);
            Box::pin(async move {
                let aborted = Arc::new(Notify::new());
                let ab_c = Arc::clone(&aborted);
                let _guard = signal.on_abort(move |_reason| {
                    ab_c.notify_one();
                });
                if signal.aborted() {
                    aborted.notify_one();
                }
                let wait = aborted.notified();
                started.notify_one();
                wait.await;
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new("aborted"))],
                    details: Some(json!({})),
                    structured_content: None,
                    usage: None,
                })
            })
        }),
        replay: None,
    };

    let harness = create_harness(HarnessOptions {
        tools: Some(vec![wait_tool]),
        in_memory: Some(true),
        ..Default::default()
    })
    .await;

    let captured_roles: Arc<Mutex<Vec<Vec<&'static str>>>> = Arc::new(Mutex::new(Vec::new()));
    let cap_c = Arc::clone(&captured_roles);

    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call("wait", json!({}))]),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("done".to_string()),
            None,
        )),
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _st, _mdl| {
            let cap = Arc::clone(&cap_c);
            Box::pin(async move {
                let mut r: Vec<&'static str> = Vec::new();
                if ctx.system_prompt.is_some() {
                    r.push("system");
                }
                for m in &ctx.messages {
                    let role = match m {
                        pi_ai::types::Message::System(_) => "system",
                        pi_ai::types::Message::User(_) => "user",
                        pi_ai::types::Message::Assistant(_) => "assistant",
                        pi_ai::types::Message::ToolResult(_) => "toolResult",
                    };
                    r.push(role);
                }
                cap.lock().unwrap_or_else(PoisonError::into_inner).push(r);
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("after fork".to_string()),
                    None,
                ))
            })
        })),
    ]);

    let services = AgentSessionServices {
        cwd: harness.temp_dir.clone(),
        agent_dir: harness.temp_dir.clone(),
        model_runtime: harness.session.model_runtime().clone(),
        settings_manager: harness.settings_manager.clone(),
        resource_loader: harness.session.resource_loader().clone(),
        diagnostics: vec![],
    };

    let model = harness.get_model();
    let create_runtime: CreateAgentSessionRuntimeFactory = Arc::new(move |opts: CreateAgentSessionRuntimeFactoryOptions| {
        let model_c = model.clone();
        let services_c = services.clone();
        Box::pin(async move {
            let created = create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
                services: services_c.clone(),
                session_manager: opts.session_manager,
                session_start_event: opts.session_start_event,
                model: Some(model_c),
                thinking_level: None,
                scoped_models: None,
                tools: None,
                exclude_tools: None,
                no_tools: Some(NoToolsMode::All),
                custom_tools: None,
            })
            .await?;
            Ok(CreateAgentSessionRuntimeResult::from_session_result(
                created,
                services_c,
                vec![],
            ))
        })
    });

    let runtime = AgentSessionRuntime::new(
        harness.session.clone(),
        AgentSessionServices {
            cwd: harness.temp_dir.clone(),
            agent_dir: harness.temp_dir.clone(),
            model_runtime: harness.session.model_runtime().clone(),
            settings_manager: harness.settings_manager.clone(),
            resource_loader: harness.session.resource_loader().clone(),
            diagnostics: vec![],
        },
        create_runtime,
        vec![],
        None,
    );

    let first_user_entry_id = harness
        .session_manager
        .get_entries()
        .iter()
        .find_map(|e| match e {
            SessionEntry::Message(m) if m.message.role() == Some("user") => Some(m.id.clone()),
            _ => None,
        });

    let started_wait = tool_started.notified();
    let initial_session = harness.session.clone();
    let outgoing_prompt = tokio::spawn(async move { initial_session.prompt("first prompt", None).await });
    started_wait.await;

    let entry_id = first_user_entry_id.unwrap_or_else(|| {
        harness
            .session_manager
            .get_entries()
            .into_iter()
            .find_map(|e| match e {
                SessionEntry::Message(m) if m.message.role() == Some("user") => Some(m.id),
                _ => None,
            })
            .expect("expected first user message entry")
    });

    let forked = runtime.fork(&entry_id, None).await.unwrap();
    assert!(!forked.cancelled);

    let _ = outgoing_prompt.await;
    runtime.session().bind_extensions(Default::default()).await.unwrap();

    assert_eq!(roles(&runtime.session().messages()), vec!["system"]);
    let message_entry_roles: Vec<&'static str> = runtime
        .session()
        .session_manager()
        .get_entries()
        .into_iter()
        .filter_map(|e| match e {
            SessionEntry::Message(m) => Some(roles(&[m.message])[0]),
            _ => None,
        })
        .collect();
    assert_eq!(message_entry_roles, vec!["system"]);

    runtime.session().prompt("next prompt", None).await.unwrap();

    let recorded = captured_roles.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(recorded, vec![vec!["system", "system", "user"]]);

    runtime.dispose().await;
    harness.cleanup();
}
