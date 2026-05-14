use crate::config::agent_dir;
use crate::messages::{AgentMessage, AssistantContentBlock, Usage, UserContentBlock};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDescriptor {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub api: String,
    pub reasoning: bool,
    #[serde(rename = "baseUrl", skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(rename = "apiKey", skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(rename = "contextWindow", skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(rename = "maxTokens", skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input: Vec<String>,
}

impl ModelDescriptor {
    pub fn defaults() -> Vec<Self> {
        let mut models = built_in_models();
        if let Ok(mut registered) = load_registered_models() {
            for model in registered.drain(..) {
                if let Some(existing) = models
                    .iter_mut()
                    .find(|existing| existing.provider == model.provider && existing.id == model.id)
                {
                    *existing = model;
                } else {
                    models.push(model);
                }
            }
        }
        models
    }

    pub fn by_id(id: &str) -> Option<Self> {
        let trimmed = id.trim();
        if let Some((provider, model_id)) = trimmed.split_once('/') {
            return Self::defaults()
                .into_iter()
                .find(|model| model.provider == provider && model.id == model_id);
        }
        let mut matches = Self::defaults()
            .into_iter()
            .filter(|model| model.id == trimmed)
            .collect::<Vec<_>>();
        if matches.len() == 1 {
            matches.pop()
        } else {
            None
        }
    }

    pub fn by_provider_and_id(provider: &str, id: &str) -> Option<Self> {
        Self::defaults()
            .into_iter()
            .find(|model| model.provider == provider && model.id == id)
    }

    pub fn resolve(provider: Option<&str>, id: Option<&str>) -> Option<Self> {
        match (provider, id) {
            (Some(provider), Some(id)) => Self::by_provider_and_id(provider, id),
            (None, Some(id)) => Self::by_id(id),
            (Some(provider), None) => Self::defaults()
                .into_iter()
                .find(|model| model.provider == provider),
            (None, None) => None,
        }
    }

    pub fn canonical_id(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }
}

fn built_in_models() -> Vec<ModelDescriptor> {
    vec![
        ModelDescriptor {
            id: "gpt-5.5".to_string(),
            name: "OpenAI: GPT-5.5".to_string(),
            provider: "openai-codex".to_string(),
            api: "openai-codex-responses".to_string(),
            reasoning: true,
            base_url: Some("https://chatgpt.com/backend-api".to_string()),
            api_key: None,
            headers: None,
            context_window: Some(272_000),
            max_tokens: Some(128_000),
            input: vec!["text".to_string(), "image".to_string()],
        },
        ModelDescriptor {
            id: "gpt-5.4".to_string(),
            name: "OpenAI: GPT-5.4".to_string(),
            provider: "openai-codex".to_string(),
            api: "openai-codex-responses".to_string(),
            reasoning: true,
            base_url: Some("https://chatgpt.com/backend-api".to_string()),
            api_key: None,
            headers: None,
            context_window: Some(272_000),
            max_tokens: Some(128_000),
            input: vec!["text".to_string(), "image".to_string()],
        },
        ModelDescriptor {
            id: "gpt-5.4-mini".to_string(),
            name: "OpenAI: GPT-5.4 Mini".to_string(),
            provider: "openai-codex".to_string(),
            api: "openai-codex-responses".to_string(),
            reasoning: true,
            base_url: Some("https://chatgpt.com/backend-api".to_string()),
            api_key: None,
            headers: None,
            context_window: Some(272_000),
            max_tokens: Some(128_000),
            input: vec!["text".to_string(), "image".to_string()],
        },
        ModelDescriptor {
            id: "gpt-5.3-codex".to_string(),
            name: "OpenAI: GPT-5.3-Codex".to_string(),
            provider: "openai-codex".to_string(),
            api: "openai-codex-responses".to_string(),
            reasoning: true,
            base_url: Some("https://chatgpt.com/backend-api".to_string()),
            api_key: None,
            headers: None,
            context_window: Some(272_000),
            max_tokens: Some(128_000),
            input: vec!["text".to_string(), "image".to_string()],
        },
    ]
}

#[derive(Debug, Deserialize)]
struct ModelsConfig {
    #[serde(default)]
    providers: BTreeMap<String, ProviderConfig>,
}

#[derive(Debug, Deserialize)]
struct ProviderConfig {
    name: Option<String>,
    #[serde(rename = "baseUrl")]
    base_url: Option<String>,
    #[serde(rename = "apiKey")]
    api_key: Option<String>,
    api: Option<String>,
    headers: Option<BTreeMap<String, String>>,
    #[serde(default)]
    models: Vec<ModelDefinition>,
    #[serde(rename = "modelOverrides", default)]
    model_overrides: BTreeMap<String, ModelOverride>,
}

#[derive(Debug, Deserialize)]
struct ModelDefinition {
    id: String,
    name: Option<String>,
    api: Option<String>,
    #[serde(rename = "baseUrl")]
    base_url: Option<String>,
    reasoning: Option<bool>,
    input: Option<Vec<String>>,
    #[serde(rename = "contextWindow")]
    context_window: Option<u64>,
    #[serde(rename = "maxTokens")]
    max_tokens: Option<u64>,
    headers: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Deserialize)]
struct ModelOverride {
    name: Option<String>,
    reasoning: Option<bool>,
    input: Option<Vec<String>>,
    #[serde(rename = "contextWindow")]
    context_window: Option<u64>,
    #[serde(rename = "maxTokens")]
    max_tokens: Option<u64>,
    headers: Option<BTreeMap<String, String>>,
}

pub fn load_registered_models() -> Result<Vec<ModelDescriptor>> {
    let path = agent_dir()?.join("models.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path)?;
    let config: ModelsConfig = serde_json::from_str(&strip_json_comments(&content))?;
    let mut models = Vec::new();

    for (provider, provider_config) in config.providers {
        for model in provider_config.models {
            let api = model
                .api
                .or_else(|| provider_config.api.clone())
                .unwrap_or_else(|| "openai-completions".to_string());
            let base_url = model.base_url.or_else(|| provider_config.base_url.clone());
            let mut headers = provider_config.headers.clone().unwrap_or_default();
            if let Some(model_headers) = model.headers {
                headers.extend(model_headers);
            }
            let display_provider = provider_config.name.as_deref().unwrap_or(&provider);
            models.push(ModelDescriptor {
                id: model.id.clone(),
                name: model
                    .name
                    .unwrap_or_else(|| format!("{}: {}", display_provider, model.id)),
                provider: provider.clone(),
                api,
                reasoning: model.reasoning.unwrap_or(false),
                base_url,
                api_key: provider_config.api_key.as_deref().map(resolve_config_value),
                headers: (!headers.is_empty()).then_some(headers),
                context_window: model.context_window.or(Some(128_000)),
                max_tokens: model.max_tokens.or(Some(16_384)),
                input: model.input.unwrap_or_else(|| vec!["text".to_string()]),
            });
        }

        for (model_id, override_config) in provider_config.model_overrides {
            if let Some(existing) = models
                .iter_mut()
                .find(|model| model.provider == provider && model.id == model_id)
            {
                apply_override(existing, override_config);
            } else if let Some(mut builtin) = built_in_models()
                .into_iter()
                .find(|model| model.provider == provider && model.id == model_id)
            {
                if let Some(base_url) = provider_config.base_url.clone() {
                    builtin.base_url = Some(base_url);
                }
                if let Some(api_key) = provider_config.api_key.as_deref() {
                    builtin.api_key = Some(resolve_config_value(api_key));
                }
                if let Some(headers) = provider_config.headers.clone() {
                    builtin.headers = Some(headers);
                }
                apply_override(&mut builtin, override_config);
                models.push(builtin);
            }
        }
    }

    Ok(models)
}

fn apply_override(model: &mut ModelDescriptor, override_config: ModelOverride) {
    if let Some(name) = override_config.name {
        model.name = name;
    }
    if let Some(reasoning) = override_config.reasoning {
        model.reasoning = reasoning;
    }
    if let Some(input) = override_config.input {
        model.input = input;
    }
    if let Some(context_window) = override_config.context_window {
        model.context_window = Some(context_window);
    }
    if let Some(max_tokens) = override_config.max_tokens {
        model.max_tokens = Some(max_tokens);
    }
    if let Some(headers) = override_config.headers {
        model
            .headers
            .get_or_insert_with(BTreeMap::new)
            .extend(headers);
    }
}

fn resolve_config_value(value: &str) -> String {
    if let Some(command) = value.strip_prefix('!') {
        return Command::new("sh")
            .arg("-lc")
            .arg(command)
            .output()
            .ok()
            .and_then(|output| output.status.success().then_some(output.stdout))
            .map(|stdout| String::from_utf8_lossy(&stdout).trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_default();
    }
    std::env::var(value).unwrap_or_else(|_| value.to_string())
}

fn strip_json_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;

    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        if ch == '"' {
            in_string = true;
            out.push(ch);
        } else if ch == '/' && chars.peek() == Some(&'/') {
            for next in chars.by_ref() {
                if next == '\n' {
                    out.push('\n');
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }

    out
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_canonical_model_lookup() {
        let model = ModelDescriptor::by_id("openai-codex/gpt-5.4").unwrap();
        assert_eq!(model.provider, "openai-codex");
        assert_eq!(model.id, "gpt-5.4");
    }
}
