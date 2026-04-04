use crate::messages::{AgentMessage, AssistantContentBlock, Usage, UserContentBlock};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDescriptor {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub api: String,
    pub reasoning: bool,
    #[serde(rename = "baseUrl", skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl ModelDescriptor {
    pub fn defaults() -> Vec<Self> {
        vec![
            Self {
                id: "gpt-5.4".to_string(),
                name: "OpenAI: GPT-5.4".to_string(),
                provider: "openai-codex".to_string(),
                api: "openai-codex-responses".to_string(),
                reasoning: true,
                base_url: Some("https://chatgpt.com/backend-api".to_string()),
            },
            Self {
                id: "gpt-5.4-mini".to_string(),
                name: "OpenAI: GPT-5.4 Mini".to_string(),
                provider: "openai-codex".to_string(),
                api: "openai-codex-responses".to_string(),
                reasoning: true,
                base_url: Some("https://chatgpt.com/backend-api".to_string()),
            },
            Self {
                id: "gpt-5.3-codex".to_string(),
                name: "OpenAI: GPT-5.3-Codex".to_string(),
                provider: "openai-codex".to_string(),
                api: "openai-codex-responses".to_string(),
                reasoning: true,
                base_url: Some("https://chatgpt.com/backend-api".to_string()),
            },
        ]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub requires_permission: bool,
}

#[derive(Debug, Clone)]
pub struct CompletionRequest {
    pub model: ModelDescriptor,
    pub system_prompt: String,
    pub messages: Vec<AgentMessage>,
    pub tools: Vec<ToolSpec>,
}

#[derive(Debug, Clone)]
pub struct CompletionToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone)]
pub struct CompletionResponse {
    pub assistant_blocks: Vec<AssistantContentBlock>,
    pub tool_calls: Vec<CompletionToolCall>,
    pub usage: Usage,
    pub stop_reason: String,
}

impl CompletionResponse {
    pub fn final_text(&self) -> String {
        self.assistant_blocks
            .iter()
            .filter_map(|block| match block {
                AssistantContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}

#[async_trait]
pub trait ModelProvider: Send + Sync {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse>;
}

pub fn user_blocks_to_text(blocks: &[UserContentBlock]) -> String {
    blocks
        .iter()
        .map(|block| match block {
            UserContentBlock::Text(text) => text.text.clone(),
            UserContentBlock::Image(image) => format!("[image:{}]", image.mime_type),
        })
        .collect::<Vec<_>>()
        .join("\n")
}
