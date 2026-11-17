//! Port of packages/coding-agent/src/cli/initial-message.ts

use pi_ai::ImageContent;

use crate::cli::args::Args;

pub struct InitialMessageInput<'a> {
    pub parsed: &'a mut Args,
    pub file_text: Option<&'a str>,
    pub file_images: Option<Vec<ImageContent>>,
    pub stdin_content: Option<&'a str>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct InitialMessageResult {
    pub initial_message: Option<String>,
    pub initial_images: Option<Vec<ImageContent>>,
}

/// Combine stdin content, @file text, and the first CLI message into a single
/// initial prompt for non-interactive mode.
pub fn build_initial_message(input: InitialMessageInput<'_>) -> InitialMessageResult {
    let mut parts: Vec<String> = Vec::new();
    if let Some(stdin) = input.stdin_content {
        parts.push(stdin.to_string());
    }
    if let Some(file_text) = input.file_text
        && !file_text.is_empty()
    {
        parts.push(file_text.to_string());
    }

    if !input.parsed.messages.is_empty() {
        let first = input.parsed.messages.remove(0);
        parts.push(first);
    }

    let initial_message = if parts.is_empty() { None } else { Some(parts.concat()) };
    let initial_images = input.file_images.filter(|imgs| !imgs.is_empty());

    InitialMessageResult {
        initial_message,
        initial_images,
    }
}
