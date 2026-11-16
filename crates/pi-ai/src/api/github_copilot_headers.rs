//! Port of packages/ai/src/api/github-copilot-headers.ts

use indexmap::IndexMap;

use crate::types::{Message, UserContentBlock, UserMessageContent};

/// Copilot carries `X-Initiator` to distinguish user-initiated turns from agent follow-ups.
pub fn infer_copilot_initiator(messages: &[Message]) -> &'static str {
    match messages.last() {
        Some(last) if !matches!(last, Message::User(_)) => "agent",
        _ => "user",
    }
}

/// Copilot requires `Copilot-Vision-Request` when messages contain images.
pub fn has_copilot_vision_input(messages: &[Message]) -> bool {
    messages.iter().any(|msg| match msg {
        Message::User(user) => match &user.content {
            UserMessageContent::Blocks(blocks) => {
                blocks.iter().any(|block| matches!(block, UserContentBlock::Image(_)))
            }
            UserMessageContent::Text(_) => false,
        },
        Message::ToolResult(tool_result) => tool_result
            .content
            .iter()
            .any(|block| matches!(block, UserContentBlock::Image(_))),
        _ => false,
    })
}

/// Build Copilot per-request headers from message history and image presence.
pub fn build_copilot_dynamic_headers(messages: &[Message], has_images: bool) -> IndexMap<String, String> {
    let mut headers = IndexMap::new();
    headers.insert("X-Initiator".to_owned(), infer_copilot_initiator(messages).to_owned());
    headers.insert("Openai-Intent".to_owned(), "conversation-edits".to_owned());
    if has_images {
        headers.insert("Copilot-Vision-Request".to_owned(), "true".to_owned());
    }
    headers
}
