//! Port of packages/ai/src/api/transform-messages.ts

use indexmap::{IndexMap, IndexSet};

use crate::types::{
    AssistantContentBlock, AssistantMessage, Message, Model, ModelInput, StopReason, TextContent, ToolCall,
    ToolResultMessage, UserContentBlock, UserMessageContent,
};

const NON_VISION_USER_IMAGE_PLACEHOLDER: &str = "(image omitted: model does not support images)";
const NON_VISION_TOOL_IMAGE_PLACEHOLDER: &str = "(tool image omitted: model does not support images)";

fn replace_images_with_placeholder(content: &[UserContentBlock], placeholder: &str) -> Vec<UserContentBlock> {
    let mut result = Vec::new();
    let mut previous_was_placeholder = false;

    for block in content {
        match block {
            UserContentBlock::Image(_) => {
                if !previous_was_placeholder {
                    result.push(UserContentBlock::Text(TextContent::new(placeholder)));
                }
                previous_was_placeholder = true;
            }
            UserContentBlock::Text(t) => {
                previous_was_placeholder = t.text == placeholder;
                result.push(UserContentBlock::Text(t.clone()));
            }
        }
    }

    result
}

fn downgrade_unsupported_images(messages: &[Message], model: &Model) -> Vec<Message> {
    if model.input.contains(&ModelInput::Image) {
        return messages.to_vec();
    }

    messages
        .iter()
        .map(|msg| match msg {
            Message::User(user) => match &user.content {
                UserMessageContent::Blocks(blocks) => {
                    let mut next = user.clone();
                    next.content = UserMessageContent::Blocks(replace_images_with_placeholder(
                        blocks,
                        NON_VISION_USER_IMAGE_PLACEHOLDER,
                    ));
                    Message::User(next)
                }
                UserMessageContent::Text(_) => msg.clone(),
            },
            Message::ToolResult(tr) => {
                let mut next = tr.clone();
                next.content = replace_images_with_placeholder(&tr.content, NON_VISION_TOOL_IMAGE_PLACEHOLDER);
                Message::ToolResult(next)
            }
            _ => msg.clone(),
        })
        .collect()
}

pub type NormalizeToolCallIdFn<'a> = &'a dyn Fn(&str, &Model, &AssistantMessage) -> String;

/// Normalize tool call IDs, downgrade unsupported images, handle cross-provider thinking blocks,
/// and synthesize empty tool results for orphaned tool calls.
pub fn transform_messages(
    messages: &[Message],
    model: &Model,
    normalize_tool_call_id: Option<NormalizeToolCallIdFn<'_>>,
) -> Vec<Message> {
    let mut tool_call_id_map: IndexMap<String, String> = IndexMap::new();
    let image_aware_messages = downgrade_unsupported_images(messages, model);

    // First pass: transform messages (unsupported image downgrade, thinking blocks, tool call ID normalization)
    let transformed: Vec<Message> = image_aware_messages
        .into_iter()
        .map(|msg| match msg {
            Message::System(_) | Message::User(_) => msg,
            Message::ToolResult(mut tr) => {
                if let Some(normalized_id) = tool_call_id_map.get(&tr.tool_call_id) {
                    if normalized_id != &tr.tool_call_id {
                        tr.tool_call_id = normalized_id.clone();
                    }
                }
                Message::ToolResult(tr)
            }
            Message::Assistant(assistant_msg) => {
                let is_same_model = assistant_msg.provider == model.provider
                    && assistant_msg.api == model.api
                    && assistant_msg.model == model.id;

                let mut transformed_content = Vec::with_capacity(assistant_msg.content.len());
                for block in &assistant_msg.content {
                    match block {
                        AssistantContentBlock::Thinking(t) => {
                            if t.redacted == Some(true) {
                                if is_same_model {
                                    transformed_content.push(block.clone());
                                }
                                continue;
                            }
                            if is_same_model && t.thinking_signature.as_deref().is_some_and(|s| !s.is_empty()) {
                                transformed_content.push(block.clone());
                                continue;
                            }
                            if t.thinking.trim().is_empty() {
                                continue;
                            }
                            if is_same_model {
                                transformed_content.push(block.clone());
                            } else {
                                transformed_content.push(AssistantContentBlock::Text(TextContent::new(&t.thinking)));
                            }
                        }
                        AssistantContentBlock::Text(t) => {
                            if is_same_model {
                                transformed_content.push(block.clone());
                            } else {
                                transformed_content.push(AssistantContentBlock::Text(TextContent::new(&t.text)));
                            }
                        }
                        AssistantContentBlock::ToolCall(tc) => {
                            let mut normalized_tc = tc.clone();
                            if !is_same_model {
                                normalized_tc.thought_signature = None;
                            }
                            if !is_same_model {
                                if let Some(norm_fn) = normalize_tool_call_id {
                                    let normalized_id = norm_fn(&tc.id, model, &assistant_msg);
                                    if normalized_id != tc.id {
                                        tool_call_id_map.insert(tc.id.clone(), normalized_id.clone());
                                        normalized_tc.id = normalized_id;
                                    }
                                }
                            }
                            transformed_content.push(AssistantContentBlock::ToolCall(normalized_tc));
                        }
                    }
                }

                let mut next = assistant_msg;
                next.content = transformed_content;
                Message::Assistant(next)
            }
        })
        .collect();

    // Second pass: insert synthetic empty tool results for orphaned tool calls
    let mut result: Vec<Message> = Vec::new();
    let mut pending_tool_calls: Vec<ToolCall> = Vec::new();
    let mut existing_tool_result_ids: IndexSet<String> = IndexSet::new();
    let mut held_system_messages: Vec<Message> = Vec::new();

    let close_pending_tool_calls = |result: &mut Vec<Message>,
                                    pending: &mut Vec<ToolCall>,
                                    existing_ids: &mut IndexSet<String>,
                                    held_sys: &mut Vec<Message>| {
        if !pending.is_empty() {
            for tc in pending.drain(..) {
                if !existing_ids.contains(&tc.id) {
                    result.push(Message::ToolResult(ToolResultMessage {
                        tool_call_id: tc.id,
                        tool_name: tc.name,
                        content: vec![UserContentBlock::Text(TextContent::new("No result provided"))],
                        details: None,
                        usage: None,
                        nested_calls: None,
                        is_error: true,
                        timestamp: pi_js::time::now_ms(),
                    }));
                }
            }
            existing_ids.clear();
        }
        result.append(held_sys);
    };

    for msg in transformed {
        match &msg {
            Message::Assistant(assistant_msg) => {
                close_pending_tool_calls(
                    &mut result,
                    &mut pending_tool_calls,
                    &mut existing_tool_result_ids,
                    &mut held_system_messages,
                );

                if assistant_msg.stop_reason == StopReason::Error || assistant_msg.stop_reason == StopReason::Aborted {
                    continue;
                }

                let tool_calls: Vec<ToolCall> = assistant_msg
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        AssistantContentBlock::ToolCall(tc) => Some(tc.clone()),
                        _ => None,
                    })
                    .collect();

                if !tool_calls.is_empty() {
                    pending_tool_calls = tool_calls;
                    existing_tool_result_ids.clear();
                }

                result.push(msg);
            }
            Message::ToolResult(tr) => {
                existing_tool_result_ids.insert(tr.tool_call_id.clone());
                result.push(msg);
            }
            Message::System(_) => {
                if !pending_tool_calls.is_empty() {
                    held_system_messages.push(msg);
                } else {
                    result.push(msg);
                }
            }
            Message::User(_) => {
                close_pending_tool_calls(
                    &mut result,
                    &mut pending_tool_calls,
                    &mut existing_tool_result_ids,
                    &mut held_system_messages,
                );
                result.push(msg);
            }
        }
    }

    close_pending_tool_calls(
        &mut result,
        &mut pending_tool_calls,
        &mut existing_tool_result_ids,
        &mut held_system_messages,
    );

    result
}
