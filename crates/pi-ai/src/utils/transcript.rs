//! Port of packages/ai/src/utils/transcript.ts

use indexmap::{IndexMap, IndexSet};
use pi_js::vendor::typebox::Schema;
use serde::{Deserialize, Serialize};

use crate::types::{Context, Message, SystemMessage, SystemMessageContent, Tool, ToolReference};
use crate::utils::text::{content_text, get_system_message_text};

pub use crate::types::TranscriptContext;

/// Any message list item. Replay helpers inspect only entries representing a system message,
/// so external crates with custom message wrappers can pass slices without filtering.
pub trait TranscriptMessage {
    fn as_system_message(&self) -> Option<&SystemMessage>;
}

impl TranscriptMessage for Message {
    fn as_system_message(&self) -> Option<&SystemMessage> {
        match self {
            Self::System(msg) => Some(msg),
            _ => None,
        }
    }
}

impl<T: TranscriptMessage> TranscriptMessage for &T {
    fn as_system_message(&self) -> Option<&SystemMessage> {
        (*self).as_system_message()
    }
}

/// Build the leading system message for a prompt and tool set. Returns `None` when
/// both are empty, so an empty transcript stays empty.
pub fn create_initial_system_message(system_prompt: Option<&str>, tools: Option<&[Tool]>) -> Option<SystemMessage> {
    let has_system_prompt = system_prompt.is_some_and(|s| !s.is_empty());
    let has_tools = tools.is_some_and(|t| !t.is_empty());
    if !has_system_prompt && !has_tools {
        return None;
    }
    Some(SystemMessage {
        content: SystemMessageContent::Text(system_prompt.unwrap_or("").to_string()),
        sections: None,
        tools_added: if has_tools { tools.map(|t| t.to_vec()) } else { None },
        tools_removed: None,
        timestamp: 0,
    })
}

/// Fold `Context.systemPrompt` and `Context.tools` into a leading system message.
/// This is the only entry point that produces a [`TranscriptContext`]; every
/// provider-facing function expects the result.
pub fn normalize_context(context: &Context) -> TranscriptContext {
    let initial = create_initial_system_message(context.system_prompt.as_deref(), context.tools.as_deref());
    let mut messages = Vec::with_capacity(context.messages.len() + usize::from(initial.is_some()));
    if let Some(msg) = initial {
        messages.push(Message::System(msg));
    }
    messages.extend(context.messages.iter().cloned());
    TranscriptContext::from_normalized(messages)
}

/// Return the leading system message, if the transcript starts with one.
pub fn get_initial_system_message<M: TranscriptMessage>(messages: &[M]) -> Option<&SystemMessage> {
    messages.first().and_then(TranscriptMessage::as_system_message)
}

/// Drop the leading system message for APIs that carry the prompt outside the message list.
pub fn without_initial_system_message(messages: &[Message]) -> Vec<Message> {
    if get_initial_system_message(messages).is_some() {
        messages[1..].to_vec()
    } else {
        messages.to_vec()
    }
}

/// Resolve the tools available after applying every transcript delta in order.
pub fn get_current_tools<M: TranscriptMessage>(messages: &[M]) -> Vec<Tool> {
    let mut tools: IndexMap<String, Tool> = IndexMap::new();
    for msg in messages {
        let Some(sys) = msg.as_system_message() else {
            continue;
        };
        if let Some(removed) = &sys.tools_removed {
            for tool_ref in removed {
                tools.shift_remove(&tool_ref.name);
            }
        }
        if let Some(added) = &sys.tools_added {
            for tool in added {
                tools.insert(tool.name.clone(), tool.clone());
            }
        }
    }
    tools.into_values().collect()
}

/// Replay every system message into one leading system message holding the current
/// prompt and tools. Later `content` is appended to the base prompt, `sections` are
/// patched by name, and tools are resolved with [`get_current_tools`].
pub fn get_current_system_message<M: TranscriptMessage>(messages: &[M]) -> Option<SystemMessage> {
    let mut content: Vec<String> = Vec::new();
    let mut sections: IndexMap<String, Option<String>> = IndexMap::new();
    let mut timestamp: Option<i64> = None;

    for msg in messages {
        let Some(sys) = msg.as_system_message() else {
            continue;
        };
        timestamp.get_or_insert(sys.timestamp);
        let text = content_text(&sys.content, None);
        if !text.is_empty() {
            content.push(text);
        }
        if let Some(msg_sections) = &sys.sections {
            for (name, value) in msg_sections {
                match value {
                    None => {
                        sections.shift_remove(name);
                    }
                    Some(v) => {
                        sections.insert(name.clone(), Some(v.clone()));
                    }
                }
            }
        }
    }

    let tools = get_current_tools(messages);
    if timestamp.is_none() && tools.is_empty() {
        return None;
    }

    Some(SystemMessage {
        content: SystemMessageContent::Text(content.join("\n\n")),
        sections: if sections.is_empty() { None } else { Some(sections) },
        tools_added: if tools.is_empty() { None } else { Some(tools) },
        tools_removed: None,
        timestamp: timestamp.unwrap_or(0),
    })
}

/// Render the current system prompt text after replaying every system message.
pub fn get_current_system_prompt<M: TranscriptMessage>(messages: &[M]) -> String {
    get_current_system_message(messages)
        .as_ref()
        .map(get_system_message_text)
        .unwrap_or_default()
}

/// Rebuild the transcript for APIs without mid-conversation system messages: the replayed
/// system message leads, and every later system message is dropped.
pub fn collapse_system_messages(context: &TranscriptContext) -> TranscriptContext {
    let head = get_current_system_message(&context.messages);
    let mut messages = Vec::with_capacity(context.messages.len());
    if let Some(sys) = head {
        messages.push(Message::System(sys));
    }
    for msg in &context.messages {
        if !matches!(msg, Message::System(_)) {
            messages.push(msg.clone());
        }
    }
    TranscriptContext::from_normalized(messages)
}

/// Keep later system messages in place when the model accepts them; otherwise collapse them.
pub fn resolve_transcript(
    context: &TranscriptContext,
    supports_mid_convo_system_messages: Option<bool>,
) -> TranscriptContext {
    if supports_mid_convo_system_messages == Some(true) {
        context.clone()
    } else {
        collapse_system_messages(context)
    }
}

/// Strip executable and display-only fields from a tool before transcript comparison or persistence.
pub fn to_tool_declaration(tool: &Tool) -> Tool {
    let raw_value = tool.parameters.to_value();
    let clean_value = pi_js::json::parse(&pi_js::json::stringify(&raw_value)).unwrap_or(raw_value);
    Tool {
        name: tool.name.clone(),
        description: tool.description.clone(),
        parameters: Schema::from(clean_value),
        constrained_sampling: tool.constrained_sampling.clone(),
    }
}

/// Whether two tools declare the same interface to the model.
pub fn declarations_equal(left: &Tool, right: &Tool) -> bool {
    pi_js::json::stringify(&to_tool_declaration(left)) == pi_js::json::stringify(&to_tool_declaration(right))
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolStateChanges {
    pub tools_added: Vec<Tool>,
    pub tools_removed: Vec<ToolReference>,
}

/// Compare two complete tool states. A changed definition is a removal followed by an addition.
pub fn get_tool_state_changes(previous: &[Tool], current: &[Tool]) -> ToolStateChanges {
    let previous_tools: IndexMap<&str, &Tool> = previous.iter().map(|t| (t.name.as_str(), t)).collect();
    let current_tools: IndexMap<&str, &Tool> = current.iter().map(|t| (t.name.as_str(), t)).collect();

    let tools_added = current
        .iter()
        .filter(|tool| {
            previous_tools
                .get(tool.name.as_str())
                .is_none_or(|prev| !declarations_equal(prev, tool))
        })
        .map(to_tool_declaration)
        .collect();

    let tools_removed = previous
        .iter()
        .filter(|tool| {
            current_tools
                .get(tool.name.as_str())
                .is_none_or(|curr| !declarations_equal(tool, curr))
        })
        .map(|tool| ToolReference {
            name: tool.name.clone(),
        })
        .collect();

    ToolStateChanges {
        tools_added,
        tools_removed,
    }
}

/// Every definition referenced by transcript tool state, in first-declaration order.
pub fn get_declared_tools<M: TranscriptMessage>(messages: &[M]) -> Vec<Tool> {
    let mut definitions: IndexMap<String, Tool> = IndexMap::new();
    for msg in messages {
        let Some(sys) = msg.as_system_message() else {
            continue;
        };
        if let Some(added) = &sys.tools_added {
            for tool in added {
                definitions.insert(tool.name.clone(), tool.clone());
            }
        }
    }
    definitions.into_values().collect()
}

/// Whether a tool name was declared twice with different definitions. Transports that
/// reference tools by name (Anthropic `tool_addition`/`tool_removal`) cannot express that.
pub fn has_tool_redefinitions<M: TranscriptMessage>(messages: &[M]) -> bool {
    let mut declared: IndexMap<&str, &Tool> = IndexMap::new();
    for msg in messages {
        let Some(sys) = msg.as_system_message() else {
            continue;
        };
        if let Some(added) = &sys.tools_added {
            for tool in added {
                if let Some(previous) = declared.get(tool.name.as_str()) {
                    if !declarations_equal(previous, tool) {
                        return true;
                    }
                }
                declared.insert(tool.name.as_str(), tool);
            }
        }
    }
    false
}

/// Whether tool history contains a removal or same-name redeclaration that an addition-only
/// transport cannot replay.
pub fn has_non_additive_tool_changes<M: TranscriptMessage>(messages: &[M]) -> bool {
    let mut declared: IndexSet<&str> = IndexSet::new();
    for msg in messages {
        let Some(sys) = msg.as_system_message() else {
            continue;
        };
        if sys.tools_removed.as_ref().is_some_and(|r| !r.is_empty()) {
            return true;
        }
        if let Some(added) = &sys.tools_added {
            for tool in added {
                if !declared.insert(tool.name.as_str()) {
                    return true;
                }
            }
        }
    }
    false
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptTools {
    /// Tools sent in the top-level request field.
    pub request_tools: Vec<Tool>,
    /// Whether later system messages carry their own `toolsAdded` as in-place additions.
    /// When false, `requestTools` already holds the complete current tool set.
    pub anchors_additions: bool,
}

/// Split tool declarations between the top-level request field and in-place additions.
/// Transports that can anchor additions at a system message keep the initial tools at the
/// top and load later ones where they appear; that only works when no tool was removed or
/// redeclared, so everything else sends the current tool list.
pub fn resolve_transcript_tools<M: TranscriptMessage>(
    messages: &[M],
    supports_tool_additions: bool,
) -> TranscriptTools {
    let anchors_additions = supports_tool_additions && !has_non_additive_tool_changes(messages);
    let request_tools = if anchors_additions {
        get_initial_system_message(messages)
            .and_then(|s| s.tools_added.clone())
            .unwrap_or_default()
    } else {
        get_current_tools(messages)
    };
    TranscriptTools {
        request_tools,
        anchors_additions,
    }
}
