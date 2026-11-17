//! Port of packages/coding-agent/test/suite/regressions/7269-cli-end-of-options.test.ts

#[path = "support/mod.rs"]
mod support;

use pi_ai::providers::faux::{FauxAssistantMessageContent, FauxResponseStep, faux_assistant_message};
use pi_coding_agent::cli::args::{UnknownFlagValue, parse_args};
use pretty_assertions::assert_eq;
use support::suite::harness::{HarnessOptions, create_harness, get_user_texts};

fn s(items: &[&str]) -> Vec<String> {
    items.iter().map(|v| (*v).to_string()).collect()
}

#[tokio::test]
async fn passes_flag_like_prompts_after_end_of_options() {
    for prompt in [
        "- summarize the following points for me",
        "--answer my question briefly",
    ] {
        let parsed = parse_args(&s(&["-ne", "--no-session", "-p", "--", prompt]));
        assert_eq!(parsed.messages, vec![prompt.to_string()]);
        assert_eq!(parsed.unknown_flags.len(), 0);
        assert!(parsed.diagnostics.is_empty());

        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("ok".to_string()),
            None,
        ))]);
        harness.session.prompt(&parsed.messages[0], None).await.unwrap();
        assert_eq!(get_user_texts(&harness), vec![prompt.to_string()]);
        harness.cleanup();
    }
}

#[test]
fn stops_parsing_options_while_retaining_at_file_handling() {
    let parsed = parse_args(&s(&[
        "--unknown-flag",
        "value",
        "--",
        "--provider",
        "openai",
        "-c",
        "@prompt.md",
    ]));

    assert_eq!(
        parsed.unknown_flags.get("unknown-flag"),
        Some(&UnknownFlagValue::String("value".to_string()))
    );
    assert_eq!(parsed.provider, None);
    assert_eq!(parsed.r#continue, None);
    assert_eq!(parsed.messages, s(&["--provider", "openai", "-c"]));
    assert_eq!(parsed.file_args, s(&["prompt.md"]));
    assert!(parsed.diagnostics.is_empty());
}
