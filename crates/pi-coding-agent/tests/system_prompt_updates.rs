//! Port of `packages/coding-agent/test/system-prompt-updates.test.ts`.

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex, PoisonError};

use indexmap::IndexMap;
use pi_agent_core::{AgentMessage, AgentToolResult};
use pi_ai::providers::faux::{FauxAssistantMessageContent, FauxResponseStep, faux_assistant_message};
use pi_ai::{
    Message, SystemMessage, SystemMessageContent, TextContent, Tool, UserContentBlock, get_current_system_message,
    get_current_system_prompt, get_system_message_text,
};
use pi_coding_agent::core::extensions::types::{BeforeAgentStartEvent, ExtensionApi, InlineExtension, ToolDefinition};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::system_prompt::{
    BuildSystemPromptOptions, SystemPromptState, build_system_prompt_sections, build_system_prompt_state,
    diff_system_prompt_sections,
};
use pi_js::vendor::typebox::t;
use serde_json::json;
use support::suite::harness::{HarnessOptions, create_harness};
use tempfile::TempDir;

#[test]
fn diffs_sections_into_a_patch() {
    let mut prev_custom = IndexMap::new();
    prev_custom.insert("plan_mode".to_owned(), "Plan only.".to_owned());
    let previous = build_system_prompt_sections(&BuildSystemPromptOptions {
        cwd: "/tmp".to_owned(),
        sections: Some(prev_custom),
        ..Default::default()
    })
    .unwrap();

    let mut cur_custom = IndexMap::new();
    cur_custom.insert("plan_mode".to_owned(), "Implementation allowed.".to_owned());
    let current = build_system_prompt_sections(&BuildSystemPromptOptions {
        cwd: "/tmp".to_owned(),
        sections: Some(cur_custom),
        ..Default::default()
    })
    .unwrap();

    let patch = diff_system_prompt_sections(
        &previous.iter().map(|(k, v)| (k.clone(), Some(v.clone()))).collect(),
        &current,
    )
    .unwrap();
    assert_eq!(patch.len(), 1);
    assert_eq!(
        patch.get("plan_mode").unwrap().as_deref(),
        Some("<plan_mode>\nImplementation allowed.\n</plan_mode>")
    );

    let same_prev: IndexMap<String, Option<String>> =
        previous.iter().map(|(k, v)| (k.clone(), Some(v.clone()))).collect();
    assert!(diff_system_prompt_sections(&same_prev, &previous).is_none());

    let default_cur = build_system_prompt_sections(&BuildSystemPromptOptions {
        cwd: "/tmp".to_owned(),
        ..Default::default()
    })
    .unwrap();
    let removal = diff_system_prompt_sections(&same_prev, &default_cur).unwrap();
    assert_eq!(removal.get("plan_mode"), Some(&None));
}

#[test]
fn keeps_preamble_untagged_and_validates_section_names() {
    let previous = build_system_prompt_sections(&BuildSystemPromptOptions {
        custom_prompt: Some("You are A.".to_owned()),
        cwd: "/tmp".to_owned(),
        ..Default::default()
    })
    .unwrap();
    let current = build_system_prompt_sections(&BuildSystemPromptOptions {
        custom_prompt: Some("You are B.".to_owned()),
        cwd: "/tmp".to_owned(),
        ..Default::default()
    })
    .unwrap();

    assert_eq!(previous.get("preamble").map(String::as_str), Some("You are A."));
    let diff = diff_system_prompt_sections(
        &previous.iter().map(|(k, v)| (k.clone(), Some(v.clone()))).collect(),
        &current,
    )
    .unwrap();
    assert_eq!(diff.get("preamble").unwrap().as_deref(), Some("You are B."));

    assert_eq!(
        build_system_prompt_state(&BuildSystemPromptOptions {
            force_system_prompt: Some("Exact prompt.".to_owned()),
            cwd: "/tmp".to_owned(),
            ..Default::default()
        })
        .unwrap(),
        SystemPromptState {
            content: "Exact prompt.".to_owned(),
            sections: None,
        }
    );

    let default_state = build_system_prompt_state(&BuildSystemPromptOptions {
        cwd: "/tmp".to_owned(),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(default_state.content, "");
    assert_eq!(
        default_state.sections,
        Some(
            build_system_prompt_sections(&BuildSystemPromptOptions {
                cwd: "/tmp".to_owned(),
                ..Default::default()
            })
            .unwrap()
        )
    );

    let mut invalid_sections = IndexMap::new();
    invalid_sections.insert("preamble".to_owned(), "x".to_owned());
    let err = build_system_prompt_sections(&BuildSystemPromptOptions {
        cwd: "/tmp".to_owned(),
        sections: Some(invalid_sections),
        ..Default::default()
    })
    .unwrap_err();
    assert!(err.to_string().contains("Invalid system prompt section name"));
}

#[test]
fn system_message_persistence_and_tool_reorder_invariance() {
    let tmp = TempDir::new().unwrap();
    let sm = SessionManager::in_memory(Some(tmp.path().to_str().unwrap()), None, None).unwrap();

    let mut sections = build_system_prompt_sections(&BuildSystemPromptOptions {
        cwd: tmp.path().to_string_lossy().into_owned(),
        selected_tools: Some(vec![
            "read".to_owned(),
            "bash".to_owned(),
            "edit".to_owned(),
            "write".to_owned(),
        ]),
        ..Default::default()
    })
    .unwrap();

    let head = SystemMessage {
        content: SystemMessageContent::Text(String::new()),
        sections: Some(sections.iter().map(|(k, v)| (k.clone(), Some(v.clone()))).collect()),
        tools_added: Some(vec![Tool {
            name: "read".to_owned(),
            description: "Read".to_owned(),
            parameters: pi_js::json::parse_as(r#"{"type":"object","properties":{}}"#).unwrap(),
            constrained_sampling: None,
        }]),
        tools_removed: None,
        timestamp: 1,
    };
    sm.append_message(AgentMessage::Message(Message::System(head.clone())))
        .unwrap();

    // Round-trip through JSON and verify current system prompt reconstruction matches
    let messages: Vec<Message> = sm
        .get_branch(None)
        .into_iter()
        .filter_map(|entry| {
            let serialized = pi_js::json::stringify(&entry);
            let parsed =
                pi_js::json::parse_as::<pi_coding_agent::core::session_manager::SessionEntry>(&serialized).ok()?;
            match parsed {
                pi_coding_agent::core::session_manager::SessionEntry::Message(m) => match m.message {
                    AgentMessage::Message(msg) => Some(msg),
                    _ => None,
                },
                _ => None,
            }
        })
        .collect();

    let cur = get_current_system_message(&messages).unwrap();
    assert_eq!(get_current_system_prompt(&messages), get_system_message_text(&head));
    assert_eq!(cur.tools_added.as_ref().map(|t| t.len()), Some(1));

    // Recomputing identical sections produces empty diff (None)
    let cur_opt: IndexMap<String, Option<String>> = sections.drain(..).map(|(k, v)| (k, Some(v))).collect();
    let same_again = build_system_prompt_sections(&BuildSystemPromptOptions {
        cwd: tmp.path().to_string_lossy().into_owned(),
        selected_tools: Some(vec![
            "read".to_owned(),
            "bash".to_owned(),
            "edit".to_owned(),
            "write".to_owned(),
        ]),
        ..Default::default()
    })
    .unwrap();
    assert!(diff_system_prompt_sections(&cur_opt, &same_again).is_none());
}

#[tokio::test]
async fn set_active_tools_and_before_agent_start_emit_prompt_and_tool_updates() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        for name in ["first", "second"] {
            let n = name.to_owned();
            pi.register_tool(ToolDefinition {
                name: name.to_owned(),
                label: name.to_owned(),
                description: format!("{name} description"),
                prompt_snippet: Some(format!("{name} prompt snippet")),
                prompt_guidelines: Some(vec![format!("Use {name} carefully.")]),
                parameters: t::object([] as [(&str, pi_js::vendor::typebox::Schema); 0]),
                constrained_sampling: None,
                render_shell: None,
                prepare_arguments: None,
                output_schema: None,
                exposure: None,
                namespace: None,
                annotations: None,
                default_active: None,
                prepare_loadout: None,
                execution_mode: None,
                execute: Arc::new(move |_id, _params, _sig, _upd, _ctx| {
                    let n = n.clone();
                    Box::pin(async move {
                        Ok(AgentToolResult {
                            content: vec![UserContentBlock::Text(TextContent::new(n))],
                            details: Some(json!({})),
                            ..Default::default()
                        })
                    })
                }),
                render_call: None,
                render_result: None,
            })?;
        }
        let turn = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let pi_ev = pi.clone();
        pi.on::<BeforeAgentStartEvent>(move |_ev, _ctx| {
            let turn = Arc::clone(&turn);
            let pi_ev = pi_ev.clone();
            Box::pin(async move {
                if turn.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1 {
                    pi_ev.set_active_tools(vec!["second".to_owned()])?;
                }
                Ok(None)
            })
        });
        Ok(())
    }));

    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![ext]),
        initial_active_tool_names: Some(vec!["first".to_owned()]),
        ..Default::default()
    })
    .await;
    harness.session.bind_extensions(Default::default()).await.unwrap();

    let requests: Arc<Mutex<Vec<Vec<Message>>>> = Arc::new(Mutex::new(Vec::new()));
    let req_a = Arc::clone(&requests);
    let req_b = Arc::clone(&requests);
    harness.set_responses(vec![
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _st, _mdl| {
            let req_a = Arc::clone(&req_a);
            Box::pin(async move {
                req_a
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(ctx.messages.clone());
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("first".to_owned()),
                    None,
                ))
            })
        })),
        FauxResponseStep::Factory(Arc::new(move |ctx, _opts, _st, _mdl| {
            let req_b = Arc::clone(&req_b);
            Box::pin(async move {
                req_b
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(ctx.messages.clone());
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("second".to_owned()),
                    None,
                ))
            })
        })),
    ]);

    harness.session.prompt("first", None).await.unwrap();
    harness.session.prompt("second", None).await.unwrap();

    let recorded = requests.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(recorded.len(), 2);
    let second_turn_sys: Vec<&SystemMessage> = recorded[1]
        .iter()
        .filter_map(|m| match m {
            Message::System(s) => Some(s),
            _ => None,
        })
        .collect();
    let update = second_turn_sys.last().expect("update system message");
    assert_eq!(
        update
            .tools_removed
            .as_ref()
            .map(|t| t.iter().map(|d| d.name.as_str()).collect::<Vec<_>>()),
        Some(vec!["first"])
    );
    assert_eq!(harness.session.get_active_tool_names(), vec!["second"]);
    harness.cleanup();
}
