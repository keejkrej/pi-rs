//! Port of packages/coding-agent/src/core/messages.ts

use indexmap::IndexMap;
use pi_agent_core::AgentMessage;
use pi_ai::{ImageContent, Message, TextContent, UserContentBlock, UserMessage, UserMessageContent};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const COMPACTION_SUMMARY_PREFIX: &str =
    "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";
pub const COMPACTION_SUMMARY_SUFFIX: &str = "\n</summary>";
pub const BRANCH_SUMMARY_PREFIX: &str =
    "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n";
pub const BRANCH_SUMMARY_SUFFIX: &str = "</summary>";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashExecutionMessage {
    pub role: String,
    pub command: String,
    pub output: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    pub cancelled: bool,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
    pub timestamp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_from_context: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CustomMessageContent {
    Text(String),
    Blocks(Vec<UserContentBlock>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessage {
    pub role: String,
    pub custom_type: String,
    pub content: CustomMessageContent,
    pub display: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    pub timestamp: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryMessage {
    pub role: String,
    pub summary: String,
    pub from_id: Option<String>,
    pub timestamp: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSummaryMessage {
    pub role: String,
    pub summary: String,
    pub tokens_before: i64,
    pub timestamp: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CodingAgentMessage {
    Llm(Message),
    BashExecution(BashExecutionMessage),
    Custom(CustomMessage),
    BranchSummary(BranchSummaryMessage),
    CompactionSummary(CompactionSummaryMessage),
}

impl From<BashExecutionMessage> for AgentMessage {
    fn from(value: BashExecutionMessage) -> Self {
        AgentMessage::try_custom_from(&value).unwrap_or_else(|_| AgentMessage::Custom(Value::Null))
    }
}

impl From<CustomMessage> for AgentMessage {
    fn from(value: CustomMessage) -> Self {
        AgentMessage::try_custom_from(&value).unwrap_or_else(|_| AgentMessage::Custom(Value::Null))
    }
}

impl From<BranchSummaryMessage> for AgentMessage {
    fn from(value: BranchSummaryMessage) -> Self {
        AgentMessage::try_custom_from(&value).unwrap_or_else(|_| AgentMessage::Custom(Value::Null))
    }
}

impl From<CompactionSummaryMessage> for AgentMessage {
    fn from(value: CompactionSummaryMessage) -> Self {
        AgentMessage::try_custom_from(&value).unwrap_or_else(|_| AgentMessage::Custom(Value::Null))
    }
}

pub fn bash_execution_to_text(msg: &BashExecutionMessage) -> String {
    let mut text = format!("Ran `{}`\n", msg.command);
    if !msg.output.is_empty() {
        text.push_str(&format!("```\n{}\n```", msg.output));
    } else {
        text.push_str("(no output)");
    }
    if msg.cancelled {
        text.push_str("\n\n(command cancelled)");
    } else if let Some(code) = msg.exit_code
        && code != 0
    {
        text.push_str(&format!("\n\nCommand exited with code {code}"));
    }
    if msg.truncated
        && let Some(ref full) = msg.full_output_path
    {
        text.push_str(&format!("\n\n[Output truncated. Full output: {full}]"));
    }
    text
}

pub fn create_branch_summary_message(
    summary: String,
    from_id: Option<String>,
    timestamp: &str,
) -> BranchSummaryMessage {
    BranchSummaryMessage {
        role: "branchSummary".to_owned(),
        summary,
        from_id,
        timestamp: pi_js::time::parse_date(timestamp).unwrap_or(0),
    }
}

pub fn create_compaction_summary_message(
    summary: String,
    tokens_before: i64,
    timestamp: &str,
) -> CompactionSummaryMessage {
    CompactionSummaryMessage {
        role: "compactionSummary".to_owned(),
        summary,
        tokens_before,
        timestamp: pi_js::time::parse_date(timestamp).unwrap_or(0),
    }
}

pub fn create_custom_message(
    custom_type: String,
    content: CustomMessageContent,
    display: bool,
    details: Option<Value>,
    timestamp: &str,
) -> CustomMessage {
    CustomMessage {
        role: "custom".to_owned(),
        custom_type,
        content,
        display,
        details,
        timestamp: pi_js::time::parse_date(timestamp).unwrap_or(0),
    }
}

pub fn convert_to_llm(messages: &[AgentMessage]) -> Vec<Message> {
    let mut out = Vec::new();
    for m in messages {
        match m {
            AgentMessage::Message(msg) => out.push(msg.clone()),
            AgentMessage::Custom(val) => {
                let role = val.get("role").and_then(Value::as_str).unwrap_or("");
                match role {
                    "bashExecution" => {
                        if let Ok(bash) = serde_json::from_value::<BashExecutionMessage>(val.clone()) {
                            if bash.exclude_from_context.unwrap_or(false) {
                                continue;
                            }
                            out.push(Message::User(UserMessage {
                                content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new(
                                    bash_execution_to_text(&bash),
                                ))]),
                                timestamp: bash.timestamp,
                            }));
                        }
                    }
                    "custom" => {
                        if let Ok(custom) = serde_json::from_value::<CustomMessage>(val.clone()) {
                            let blocks = match custom.content {
                                CustomMessageContent::Text(t) => {
                                    vec![UserContentBlock::Text(TextContent::new(t))]
                                }
                                CustomMessageContent::Blocks(b) => b,
                            };
                            out.push(Message::User(UserMessage {
                                content: UserMessageContent::Blocks(blocks),
                                timestamp: custom.timestamp,
                            }));
                        }
                    }
                    "branchSummary" => {
                        if let Ok(bs) = serde_json::from_value::<BranchSummaryMessage>(val.clone()) {
                            out.push(Message::User(UserMessage {
                                content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new(
                                    format!("{BRANCH_SUMMARY_PREFIX}{}{BRANCH_SUMMARY_SUFFIX}", bs.summary),
                                ))]),
                                timestamp: bs.timestamp,
                            }));
                        }
                    }
                    "compactionSummary" => {
                        if let Ok(cs) = serde_json::from_value::<CompactionSummaryMessage>(val.clone()) {
                            out.push(Message::User(UserMessage {
                                content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new(
                                    format!("{COMPACTION_SUMMARY_PREFIX}{}{COMPACTION_SUMMARY_SUFFIX}", cs.summary),
                                ))]),
                                timestamp: cs.timestamp,
                            }));
                        }
                    }
                    "system" | "user" | "assistant" | "toolResult" => {
                        if let Ok(msg) = serde_json::from_value::<Message>(val.clone()) {
                            out.push(msg);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    let _ = (IndexMap::<String, Value>::new(), ImageContent::new("", ""));
    out
}
