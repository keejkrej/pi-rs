//! Port of `packages/coding-agent/test/system-prompt-updates.test.ts`.
// WIRE(@ca-cli-w3): AgentSession prompt-loop, `before_agent_start`, and `setActiveToolsByName` live tests await `core::agent_session`.

use indexmap::IndexMap;
use pi_agent_core::AgentMessage;
use pi_ai::{
    Message, SystemMessage, SystemMessageContent, Tool, get_current_system_message, get_current_system_prompt,
    get_system_message_text,
};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::system_prompt::{
    BuildSystemPromptOptions, SystemPromptState, build_system_prompt_sections, build_system_prompt_state,
    diff_system_prompt_sections,
};
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
