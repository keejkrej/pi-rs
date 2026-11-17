//! Port of packages/ai/src/utils/text.ts

use crate::types::{
    AssistantContentBlock, SystemMessage, SystemMessageContent, TextContent, UserContentBlock, UserMessageContent,
};

pub trait ContentTextSource {
    fn collect_text(&self, separator: &str) -> String;
}

impl ContentTextSource for str {
    fn collect_text(&self, _separator: &str) -> String {
        self.to_string()
    }
}

impl ContentTextSource for String {
    fn collect_text(&self, _separator: &str) -> String {
        self.clone()
    }
}

impl ContentTextSource for [TextContent] {
    fn collect_text(&self, separator: &str) -> String {
        self.iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join(separator)
    }
}

impl ContentTextSource for Vec<TextContent> {
    fn collect_text(&self, separator: &str) -> String {
        self.as_slice().collect_text(separator)
    }
}

impl ContentTextSource for [UserContentBlock] {
    fn collect_text(&self, separator: &str) -> String {
        self.iter()
            .filter_map(|b| match b {
                UserContentBlock::Text(t) => Some(t.text.as_str()),
                UserContentBlock::Image(_) => None,
            })
            .collect::<Vec<_>>()
            .join(separator)
    }
}

impl ContentTextSource for Vec<UserContentBlock> {
    fn collect_text(&self, separator: &str) -> String {
        self.as_slice().collect_text(separator)
    }
}

impl ContentTextSource for [AssistantContentBlock] {
    fn collect_text(&self, separator: &str) -> String {
        self.iter()
            .filter_map(|b| match b {
                AssistantContentBlock::Text(t) => Some(t.text.as_str()),
                AssistantContentBlock::Thinking(_) | AssistantContentBlock::ToolCall(_) => None,
            })
            .collect::<Vec<_>>()
            .join(separator)
    }
}

impl ContentTextSource for Vec<AssistantContentBlock> {
    fn collect_text(&self, separator: &str) -> String {
        self.as_slice().collect_text(separator)
    }
}

impl ContentTextSource for SystemMessageContent {
    fn collect_text(&self, separator: &str) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Blocks(blocks) => blocks.as_slice().collect_text(separator),
        }
    }
}

impl ContentTextSource for UserMessageContent {
    fn collect_text(&self, separator: &str) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Blocks(blocks) => blocks.as_slice().collect_text(separator),
        }
    }
}

impl<T: ContentTextSource + ?Sized> ContentTextSource for &T {
    fn collect_text(&self, separator: &str) -> String {
        (*self).collect_text(separator)
    }
}

/// Extract and join text from message content.
pub fn content_text<T: ContentTextSource + ?Sized>(content: &T, separator: Option<&str>) -> String {
    content.collect_text(separator.unwrap_or("\n"))
}

/// Render a system message as a complete prompt: its content followed by its sections.
pub fn get_system_message_text(message: &SystemMessage) -> String {
    let mut parts = Vec::new();
    let base = content_text(&message.content, None);
    if !base.is_empty() {
        parts.push(base);
    }
    if let Some(sections) = &message.sections {
        for value in sections.values() {
            if let Some(text) = value {
                if !text.is_empty() {
                    parts.push(text.clone());
                }
            }
        }
    }
    parts.join("\n\n")
}

/// Render a later system message for APIs that accept system messages mid-conversation.
/// Section changes are framed by name so the model can relate them to the leading prompt.
/// This framing is request-time only and may change between versions.
pub fn render_system_message_update(message: &SystemMessage) -> String {
    let mut parts = Vec::new();
    let text = content_text(&message.content, None);
    if !text.is_empty() {
        parts.push(text);
    }
    if let Some(sections) = &message.sections {
        for (name, value) in sections {
            match value {
                None => parts.push(format!("Removed system prompt section \"{name}\".")),
                Some(v) => parts.push(format!("Updated system prompt section \"{name}\":\n\n{v}")),
            }
        }
    }
    parts.join("\n\n")
}
