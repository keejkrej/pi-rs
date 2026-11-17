//! Port of packages/coding-agent/test/initial-message.test.ts

use indexmap::IndexMap;
use pi_coding_agent::cli::args::Args;
use pi_coding_agent::cli::initial_message::{InitialMessageInput, build_initial_message};

fn create_args(messages: Vec<String>) -> Args {
    Args {
        messages,
        file_args: Vec::new(),
        unknown_flags: IndexMap::new(),
        diagnostics: Vec::new(),
        ..Default::default()
    }
}

#[test]
fn merges_piped_stdin_with_the_first_cli_message_into_one_prompt() {
    let mut parsed = create_args(vec!["Summarize the text given".to_string()]);
    let result = build_initial_message(InitialMessageInput {
        parsed: &mut parsed,
        stdin_content: Some("README contents\n"),
        file_text: None,
        file_images: None,
    });

    assert_eq!(
        result.initial_message.as_deref(),
        Some("README contents\nSummarize the text given")
    );
    assert!(parsed.messages.is_empty());
}

#[test]
fn uses_stdin_as_the_initial_prompt_when_no_cli_message_is_present() {
    let mut parsed = create_args(Vec::new());
    let result = build_initial_message(InitialMessageInput {
        parsed: &mut parsed,
        stdin_content: Some("README contents"),
        file_text: None,
        file_images: None,
    });

    assert_eq!(result.initial_message.as_deref(), Some("README contents"));
    assert!(parsed.messages.is_empty());
}

#[test]
fn combines_stdin_file_text_and_first_cli_message_in_one_prompt() {
    let mut parsed = create_args(vec!["Explain it".to_string(), "Second message".to_string()]);
    let result = build_initial_message(InitialMessageInput {
        parsed: &mut parsed,
        stdin_content: Some("stdin\n"),
        file_text: Some("file\n"),
        file_images: None,
    });

    assert_eq!(result.initial_message.as_deref(), Some("stdin\nfile\nExplain it"));
    assert_eq!(parsed.messages, vec!["Second message"]);
}
