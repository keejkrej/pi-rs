//! Port of packages/coding-agent/test/suite/regressions/11451-thinking-level-after-tree-navigation.test.ts

#[path = "support/mod.rs"]
mod support;

use pi_ai::ModelThinkingLevel;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxModelDefinition, FauxResponseStep, faux_assistant_message,
};
use pi_coding_agent::core::session_manager::SessionEntry;
use support::suite::harness::{HarnessOptions, create_harness};

#[tokio::test]
async fn records_the_live_thinking_level_on_the_new_branch_before_the_next_prompt() {
    let harness = create_harness(HarnessOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-reasoning".to_string(),
            name: None,
            reasoning: Some(true),
            input: None,
            input_limits: None,
            cost: None,
            context_window: None,
            max_tokens: None,
        }]),
        ..Default::default()
    })
    .await;

    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("one".to_string()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("two".to_string()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("three".to_string()),
            None,
        )),
    ]);

    harness.session.set_thinking_level(ModelThinkingLevel::High, None);
    harness.session.prompt("one", None).await.expect("prompt one");
    let first_answer = harness.session_manager.get_leaf_id().expect("leaf id");

    harness.session.set_thinking_level(ModelThinkingLevel::Low, None);
    harness.session.prompt("two", None).await.expect("prompt two");

    harness
        .session
        .navigate_tree(&first_answer, None)
        .await
        .expect("navigate");
    assert_eq!(harness.session_manager.build_session_context().thinking_level, "high");

    harness.session.prompt("three", None).await.expect("prompt three");
    assert_eq!(harness.session.thinking_level(), ModelThinkingLevel::Low);
    assert_eq!(harness.session_manager.build_session_context().thinking_level, "low");

    harness.cleanup();
}

#[tokio::test]
async fn does_not_record_a_level_that_only_differs_from_the_branch_by_clamping() {
    let harness = create_harness(HarnessOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-reasoning".to_string(),
            name: None,
            reasoning: Some(true),
            input: None,
            input_limits: None,
            cost: None,
            context_window: None,
            max_tokens: None,
        }]),
        ..Default::default()
    })
    .await;

    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("one".to_string()),
        None,
    ))]);

    harness.session.set_thinking_level(ModelThinkingLevel::High, None);
    harness
        .session_manager
        .append_thinking_level_change("xhigh")
        .expect("append thinking level");
    harness.session.prompt("one", None).await.expect("prompt one");

    let levels: Vec<String> = harness
        .session_manager
        .get_branch(None)
        .into_iter()
        .filter_map(|e| match e {
            SessionEntry::ThinkingLevelChange(t) => Some(t.thinking_level),
            _ => None,
        })
        .collect();
    assert_eq!(levels.last().map(String::as_str), Some("xhigh"));

    harness.cleanup();
}
