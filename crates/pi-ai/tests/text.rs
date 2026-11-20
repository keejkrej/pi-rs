//! Port of packages/ai/test/text.test.ts (unit ai-utils-messages)

use indexmap::IndexMap;
use pi_ai::types::{AssistantContentBlock, ImageContent, TextContent, ThinkingContent, ToolCall, UserContentBlock};
use pi_ai::utils::text::content_text;

#[test]
fn extracts_assistant_text_blocks_with_default_and_custom_separators() {
    let content = vec![
        AssistantContentBlock::Thinking(ThinkingContent {
            thinking: "reasoning".to_string(),
            thinking_signature: None,
            redacted: None,
        }),
        AssistantContentBlock::Text(TextContent {
            text: "first".to_string(),
            text_signature: None,
        }),
        AssistantContentBlock::ToolCall(ToolCall {
            id: "1".to_string(),
            name: "read".to_string(),
            namespace: None,
            arguments: IndexMap::new(),
            thought_signature: None,
            arguments_error: None,
        }),
        AssistantContentBlock::Text(TextContent {
            text: "second".to_string(),
            text_signature: None,
        }),
    ];

    assert_eq!(content_text(&content, None), "first\nsecond");
    assert_eq!(content_text(&content, Some("")), "firstsecond");
}

#[test]
fn passes_string_content_through_and_extracts_tool_result_text() {
    assert_eq!(content_text("hello", None), "hello");

    let tool_result_content = vec![
        UserContentBlock::Text(TextContent {
            text: "first".to_string(),
            text_signature: None,
        }),
        UserContentBlock::Image(ImageContent {
            data: "...".to_string(),
            mime_type: "image/png".to_string(),
        }),
        UserContentBlock::Text(TextContent {
            text: "second".to_string(),
            text_signature: None,
        }),
    ];
    assert_eq!(content_text(&tool_result_content, Some("")), "firstsecond");
}
