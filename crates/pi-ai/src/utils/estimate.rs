//! Port of packages/ai/src/utils/estimate.ts

use serde::{Deserialize, Serialize};

use crate::types::{
    AssistantContentBlock, ImageContent, Message, StopReason, TextContent, TranscriptContext, Usage, UserContentBlock,
    UserMessageContent,
};
use crate::utils::text::get_system_message_text;

const CHARS_PER_TOKEN: f64 = 3.5;
const ESTIMATED_IMAGE_CHARS: usize = 4800;

fn chars_to_tokens(chars: usize) -> i64 {
    (chars as f64 / CHARS_PER_TOKEN).ceil() as i64
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsageEstimate {
    /// Estimated total context tokens.
    pub tokens: i64,
    /// Tokens reported by the most recent applicable assistant usage block.
    pub usage_tokens: i64,
    /// Estimated tokens after the most recent applicable assistant usage block.
    pub trailing_tokens: i64,
    /// Index of the applicable message that provided usage, or `None` when none exists.
    pub last_usage_index: Option<usize>,
}

pub enum TextOrBlocks<'a> {
    Text(&'a str),
    TextAndImages(&'a [UserContentBlock]),
    TextBlocks(&'a [TextContent]),
    ImageBlocks(&'a [ImageContent]),
}

impl<'a> From<&'a str> for TextOrBlocks<'a> {
    fn from(value: &'a str) -> Self {
        Self::Text(value)
    }
}

impl<'a> From<&'a String> for TextOrBlocks<'a> {
    fn from(value: &'a String) -> Self {
        Self::Text(value.as_str())
    }
}

impl<'a> From<&'a [UserContentBlock]> for TextOrBlocks<'a> {
    fn from(value: &'a [UserContentBlock]) -> Self {
        Self::TextAndImages(value)
    }
}

impl<'a> From<&'a Vec<UserContentBlock>> for TextOrBlocks<'a> {
    fn from(value: &'a Vec<UserContentBlock>) -> Self {
        Self::TextAndImages(value.as_slice())
    }
}

impl<'a> From<&'a UserMessageContent> for TextOrBlocks<'a> {
    fn from(value: &'a UserMessageContent) -> Self {
        match value {
            UserMessageContent::Text(s) => Self::Text(s.as_str()),
            UserMessageContent::Blocks(blocks) => Self::TextAndImages(blocks.as_slice()),
        }
    }
}

pub trait MessagesSlice {
    fn as_messages(&self) -> &[Message];
}

impl MessagesSlice for TranscriptContext {
    fn as_messages(&self) -> &[Message] {
        &self.messages
    }
}

impl MessagesSlice for [Message] {
    fn as_messages(&self) -> &[Message] {
        self
    }
}

impl MessagesSlice for Vec<Message> {
    fn as_messages(&self) -> &[Message] {
        self.as_slice()
    }
}

impl<T: MessagesSlice + ?Sized> MessagesSlice for &T {
    fn as_messages(&self) -> &[Message] {
        (*self).as_messages()
    }
}

pub fn calculate_context_tokens(usage: &Usage) -> i64 {
    if usage.total_tokens != 0 {
        usage.total_tokens
    } else {
        usage.input + usage.output + usage.cache_read + usage.cache_write
    }
}

fn estimate_text_and_image_content_chars(content: TextOrBlocks<'_>) -> usize {
    match content {
        TextOrBlocks::Text(s) => pi_js::str16::len(s),
        TextOrBlocks::TextAndImages(blocks) => blocks
            .iter()
            .map(|b| match b {
                UserContentBlock::Text(t) => pi_js::str16::len(&t.text),
                UserContentBlock::Image(_) => ESTIMATED_IMAGE_CHARS,
            })
            .sum(),
        TextOrBlocks::TextBlocks(blocks) => blocks.iter().map(|t| pi_js::str16::len(&t.text)).sum(),
        TextOrBlocks::ImageBlocks(blocks) => blocks.len() * ESTIMATED_IMAGE_CHARS,
    }
}

pub fn estimate_text_tokens(text: &str) -> i64 {
    chars_to_tokens(pi_js::str16::len(text))
}

pub fn estimate_text_and_image_content_tokens<'a>(content: impl Into<TextOrBlocks<'a>>) -> i64 {
    chars_to_tokens(estimate_text_and_image_content_chars(content.into()))
}

fn estimate_tools_tokens<T: Serialize>(tools: Option<&[T]>) -> i64 {
    let Some(tools) = tools else {
        return 0;
    };
    if tools.is_empty() {
        return 0;
    }
    let json = pi_js::json::try_stringify(&tools).unwrap_or_else(|_| "[unserializable]".to_string());
    estimate_text_tokens(&json)
}

pub fn estimate_message_tokens(message: &Message) -> i64 {
    match message {
        Message::System(sys) => {
            estimate_text_tokens(&get_system_message_text(sys))
                + estimate_tools_tokens(sys.tools_added.as_deref())
                + estimate_tools_tokens(sys.tools_removed.as_deref())
        }
        Message::User(user) => estimate_text_and_image_content_tokens(&user.content),
        Message::ToolResult(res) => estimate_text_and_image_content_tokens(&res.content),
        Message::Assistant(assistant) => {
            let mut chars = 0usize;
            for block in &assistant.content {
                match block {
                    AssistantContentBlock::Text(t) => {
                        chars += pi_js::str16::len(&t.text);
                    }
                    AssistantContentBlock::Thinking(t) => {
                        chars += pi_js::str16::len(&t.thinking);
                    }
                    AssistantContentBlock::ToolCall(tc) => {
                        let args_json = pi_js::json::try_stringify(&tc.arguments)
                            .unwrap_or_else(|_| "[unserializable]".to_string());
                        chars += pi_js::str16::len(&tc.name) + pi_js::str16::len(&args_json);
                    }
                }
            }
            chars_to_tokens(chars)
        }
    }
}

fn get_last_assistant_usage_info(messages: &[Message]) -> Option<(Usage, usize)> {
    let mut latest_prefix_timestamp: Option<i64> = None;
    let mut usage_info: Option<(Usage, usize)> = None;

    for (i, message) in messages.iter().enumerate() {
        if let Message::Assistant(assistant) = message {
            // A newer prefix message was inserted after this response (for example, a
            // compaction summary), so its usage cannot describe the current prefix.
            let usage_applies_to_prefix = latest_prefix_timestamp.is_none_or(|latest| assistant.timestamp >= latest);
            if usage_applies_to_prefix
                && assistant.stop_reason != StopReason::Aborted
                && assistant.stop_reason != StopReason::Error
                && calculate_context_tokens(&assistant.usage) > 0
            {
                usage_info = Some((assistant.usage.clone(), i));
            }
        }
        let ts = message.timestamp();
        latest_prefix_timestamp = Some(match latest_prefix_timestamp {
            Some(prev) => prev.max(ts),
            None => ts,
        });
    }

    usage_info
}

pub fn estimate_context_tokens<C: MessagesSlice + ?Sized>(context: &C) -> ContextUsageEstimate {
    let messages = context.as_messages();
    if let Some((usage, last_index)) = get_last_assistant_usage_info(messages) {
        let usage_tokens = calculate_context_tokens(&usage);
        let trailing_tokens: i64 = messages[last_index + 1..].iter().map(estimate_message_tokens).sum();
        return ContextUsageEstimate {
            tokens: usage_tokens + trailing_tokens,
            usage_tokens,
            trailing_tokens,
            last_usage_index: Some(last_index),
        };
    }

    let tokens: i64 = messages.iter().map(estimate_message_tokens).sum();
    ContextUsageEstimate {
        tokens,
        usage_tokens: 0,
        trailing_tokens: tokens,
        last_usage_index: None,
    }
}
