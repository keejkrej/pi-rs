use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum UserContentBlock {
    Text(TextContent),
    Image(ImageContent),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum AssistantContentBlock {
    Text(TextContent),
    Thinking(ThinkingContent),
    ToolCall(ToolCallContent),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TextContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
}

impl TextContent {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            content_type: "text".to_string(),
            text: text.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThinkingContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub thinking: String,
}

impl ThinkingContent {
    pub fn new(thinking: impl Into<String>) -> Self {
        Self {
            content_type: "thinking".to_string(),
            thinking: thinking.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub data: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
}

impl ImageContent {
    pub fn new(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self {
            content_type: "image".to_string(),
            data: data.into(),
            mime_type: mime_type.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCallContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

impl ToolCallContent {
    pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: Value) -> Self {
        Self {
            content_type: "toolCall".to_string(),
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageCost {
    pub input: f64,
    pub output: f64,
    #[serde(rename = "cacheRead")]
    pub cache_read: f64,
    #[serde(rename = "cacheWrite")]
    pub cache_write: f64,
    pub total: f64,
}

impl Default for UsageCost {
    fn default() -> Self {
        Self {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    #[serde(rename = "cacheRead")]
    pub cache_read: u64,
    #[serde(rename = "cacheWrite")]
    pub cache_write: u64,
    #[serde(rename = "totalTokens")]
    pub total_tokens: u64,
    pub cost: UsageCost,
}

impl Default for Usage {
    fn default() -> Self {
        Self {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            total_tokens: 0,
            cost: UsageCost::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserMessage {
    pub role: String,
    pub content: Vec<UserContentBlock>,
    pub timestamp: i64,
}

impl UserMessage {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: vec![UserContentBlock::Text(TextContent::new(text))],
            timestamp: Utc::now().timestamp_millis(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AssistantMessage {
    pub role: String,
    pub content: Vec<AssistantContentBlock>,
    pub api: String,
    pub provider: String,
    pub model: String,
    pub usage: Usage,
    #[serde(rename = "stopReason")]
    pub stop_reason: String,
    #[serde(rename = "errorMessage", skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub timestamp: i64,
}

impl AssistantMessage {
    pub fn empty(
        provider: impl Into<String>,
        model: impl Into<String>,
        api: impl Into<String>,
    ) -> Self {
        Self {
            role: "assistant".to_string(),
            content: Vec::new(),
            api: api.into(),
            provider: provider.into(),
            model: model.into(),
            usage: Usage::default(),
            stop_reason: "stop".to_string(),
            error_message: None,
            timestamp: Utc::now().timestamp_millis(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolResultMessage {
    pub role: String,
    #[serde(rename = "toolCallId")]
    pub tool_call_id: String,
    #[serde(rename = "toolName")]
    pub tool_name: String,
    pub content: Vec<UserContentBlock>,
    #[serde(default)]
    pub is_error: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    pub timestamp: i64,
}

impl ToolResultMessage {
    pub fn text(
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            role: "toolResult".to_string(),
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            content: vec![UserContentBlock::Text(TextContent::new(text))],
            is_error: false,
            details: None,
            timestamp: Utc::now().timestamp_millis(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "role")]
pub enum AgentMessage {
    #[serde(rename = "user")]
    User {
        content: Vec<UserContentBlock>,
        timestamp: i64,
    },
    #[serde(rename = "assistant")]
    Assistant {
        content: Vec<AssistantContentBlock>,
        api: String,
        provider: String,
        model: String,
        usage: Usage,
        #[serde(rename = "stopReason")]
        stop_reason: String,
        #[serde(rename = "errorMessage", skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
        timestamp: i64,
    },
    #[serde(rename = "toolResult")]
    ToolResult {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        content: Vec<UserContentBlock>,
        is_error: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
        timestamp: i64,
    },
    #[serde(rename = "compactionSummary")]
    CompactionSummary {
        summary: String,
        #[serde(rename = "tokensBefore")]
        tokens_before: u64,
        timestamp: i64,
    },
    #[serde(rename = "custom")]
    Custom {
        #[serde(rename = "customType")]
        custom_type: String,
        content: Value,
        display: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
        timestamp: i64,
    },
    #[serde(rename = "branchSummary")]
    BranchSummary {
        summary: String,
        #[serde(rename = "fromId")]
        from_id: String,
        timestamp: i64,
    },
}

impl AgentMessage {
    pub fn timestamp(&self) -> i64 {
        match self {
            AgentMessage::User { timestamp, .. }
            | AgentMessage::Assistant { timestamp, .. }
            | AgentMessage::ToolResult { timestamp, .. }
            | AgentMessage::CompactionSummary { timestamp, .. }
            | AgentMessage::Custom { timestamp, .. }
            | AgentMessage::BranchSummary { timestamp, .. } => *timestamp,
        }
    }

    pub fn as_text(&self) -> String {
        match self {
            AgentMessage::User { content, .. } => user_content_to_text(content),
            AgentMessage::Assistant { content, .. } => assistant_content_to_text(content),
            AgentMessage::ToolResult { content, .. } => user_content_to_text(content),
            AgentMessage::CompactionSummary { summary, .. } => summary.clone(),
            AgentMessage::Custom { content, .. } => {
                if let Some(text) = content.as_str() {
                    text.to_string()
                } else if let Some(arr) = content.as_array() {
                    let blocks = arr.iter().filter_map(|val| serde_json::from_value::<UserContentBlock>(val.clone()).ok()).collect::<Vec<_>>();
                    user_content_to_text(&blocks)
                } else {
                    String::new()
                }
            }
            AgentMessage::BranchSummary { summary, .. } => summary.clone(),
        }
    }
}

pub fn user_content_to_text(content: &[UserContentBlock]) -> String {
    content
        .iter()
        .map(|block| match block {
            UserContentBlock::Text(text) => text.text.clone(),
            UserContentBlock::Image(image) => {
                format!("[image:{}:{} bytes]", image.mime_type, image.data.len())
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn assistant_content_to_text(content: &[AssistantContentBlock]) -> String {
    content
        .iter()
        .map(|block| match block {
            AssistantContentBlock::Text(text) => text.text.clone(),
            AssistantContentBlock::Thinking(thinking) => {
                format!("[thinking]\n{}", thinking.thinking)
            }
            AssistantContentBlock::ToolCall(call) => {
                format!("[tool:{} {}]", call.name, call.arguments)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionHeader {
    #[serde(rename = "type")]
    pub entry_type: String,
    pub version: u32,
    pub id: String,
    pub timestamp: String,
    pub cwd: String,
    #[serde(rename = "parentSession", skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
}

impl SessionHeader {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self {
            entry_type: "session".to_string(),
            version: 3,
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now().to_rfc3339(),
            cwd: cwd.into(),
            parent_session: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessageEntry {
    #[serde(rename = "type")]
    pub entry_type: String,
    pub id: String,
    #[serde(rename = "parentId")]
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub message: AgentMessage,
}

impl SessionMessageEntry {
    pub fn new(parent_id: Option<String>, message: AgentMessage) -> Self {
        Self {
            entry_type: "message".to_string(),
            id: Uuid::new_v4().simple().to_string()[..8].to_string(),
            parent_id,
            timestamp: Utc::now().to_rfc3339(),
            message,
        }
    }
}
