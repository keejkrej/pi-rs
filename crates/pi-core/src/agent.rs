use crate::messages::{AgentMessage, UserMessage};
use crate::models::{CompletionRequest, ModelDescriptor, ModelProvider};
use crate::session::SessionManager;
use crate::skills::build_system_prompt;
use crate::tools::{BuiltInToolRegistry, ToolExecutionResult};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct PermissionRequest {
    pub tool_call_id: String,
    pub tool_name: String,
    pub title: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    AllowOnce,
    Reject,
}

#[async_trait(?Send)]
pub trait PermissionHandler {
    async fn request(&self, request: PermissionRequest) -> Result<PermissionDecision>;
}

pub struct AllowAllPermissions;

#[async_trait(?Send)]
impl PermissionHandler for AllowAllPermissions {
    async fn request(&self, _request: PermissionRequest) -> Result<PermissionDecision> {
        Ok(PermissionDecision::AllowOnce)
    }
}

#[derive(Debug, Clone)]
pub enum SessionEvent {
    UserMessage(String),
    AssistantMessage(String),
    ToolCallStart {
        tool_call_id: String,
        tool_name: String,
        title: String,
        arguments: Value,
    },
    ToolCallResult {
        tool_call_id: String,
        tool_name: String,
        output: ToolExecutionResult,
    },
    SessionInfo {
        title: String,
    },
}

#[async_trait(?Send)]
pub trait EventSink {
    async fn emit(&self, event: SessionEvent) -> Result<()>;
}

pub struct NullEventSink;

#[async_trait(?Send)]
impl EventSink for NullEventSink {
    async fn emit(&self, _event: SessionEvent) -> Result<()> {
        Ok(())
    }
}

pub struct AgentSession {
    cwd: PathBuf,
    pub session_manager: SessionManager,
    pub model: ModelDescriptor,
    pub thinking_level: String,
    system_prompt: String,
    tools: BuiltInToolRegistry,
    provider: Arc<dyn ModelProvider>,
}

impl AgentSession {
    pub fn new(
        cwd: PathBuf,
        session_manager: SessionManager,
        model: ModelDescriptor,
        thinking_level: impl Into<String>,
        provider: Arc<dyn ModelProvider>,
    ) -> Result<Self> {
        let system_prompt = build_system_prompt(&cwd)?;
        Ok(Self {
            cwd,
            session_manager,
            model,
            thinking_level: thinking_level.into(),
            system_prompt,
            tools: BuiltInToolRegistry::new(),
            provider,
        })
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn set_model(&mut self, model: ModelDescriptor) -> Result<()> {
        self.session_manager
            .push_model_change(model.provider.clone(), model.id.clone())?;
        self.model = model;
        Ok(())
    }

    pub fn set_thinking_level(&mut self, thinking_level: impl Into<String>) -> Result<()> {
        let next = thinking_level.into();
        self.session_manager
            .push_thinking_level_change(next.clone())?;
        self.thinking_level = next;
        Ok(())
    }

    pub async fn compact(&mut self) -> Result<String> {
        let messages = self.session_manager.messages();
        if messages.len() <= 12 {
            return Ok("Session is already compact.".to_string());
        }
        let dropped = &messages[..messages.len() - 8];
        let summary = dropped
            .iter()
            .map(|message| message.as_text())
            .collect::<Vec<_>>()
            .join("\n")
            .chars()
            .take(4_000)
            .collect::<String>();
        self.session_manager
            .push_message(AgentMessage::CompactionSummary {
                summary: format!("Compacted prior context:\n{summary}"),
                tokens_before: dropped.len() as u64,
                timestamp: chrono::Utc::now().timestamp_millis(),
            })?;
        Ok("Compacted older messages into a summary entry.".to_string())
    }

    pub async fn prompt(
        &mut self,
        prompt: String,
        sink: &dyn EventSink,
        permissions: &dyn PermissionHandler,
    ) -> Result<String> {
        if let Some(result) = self.handle_command(&prompt).await? {
            sink.emit(SessionEvent::AssistantMessage(result.clone()))
                .await?;
            return Ok(result);
        }

        let user_message = UserMessage::text(prompt.clone());
        self.session_manager.push_message(AgentMessage::User {
            content: user_message.content.clone(),
            timestamp: user_message.timestamp,
        })?;
        sink.emit(SessionEvent::UserMessage(prompt.clone())).await?;

        let mut final_text = String::new();
        for _ in 0..16 {
            let response = self
                .provider
                .complete(CompletionRequest {
                    model: self.model.clone(),
                    system_prompt: self.system_prompt.clone(),
                    messages: self.session_manager.messages(),
                    tools: self.tools.specs(),
                })
                .await?;

            let assistant = AgentMessage::Assistant {
                content: response.assistant_blocks.clone(),
                api: self.model.api.clone(),
                provider: self.model.provider.clone(),
                model: self.model.id.clone(),
                usage: response.usage.clone(),
                stop_reason: response.stop_reason.clone(),
                error_message: None,
                timestamp: chrono::Utc::now().timestamp_millis(),
            };
            self.session_manager.push_message(assistant)?;

            let text = response.final_text();
            if !text.is_empty() {
                final_text.push_str(&text);
                sink.emit(SessionEvent::AssistantMessage(text)).await?;
            }

            if response.tool_calls.is_empty() {
                return Ok(final_text);
            }

            for tool_call in response.tool_calls {
                let title = format!("Run {}", tool_call.name);
                sink.emit(SessionEvent::ToolCallStart {
                    tool_call_id: tool_call.id.clone(),
                    tool_name: tool_call.name.clone(),
                    title: title.clone(),
                    arguments: tool_call.arguments.clone(),
                })
                .await?;

                let tool = self
                    .tools
                    .get(&tool_call.name)
                    .ok_or_else(|| anyhow!("unknown tool {}", tool_call.name))?;

                if tool.spec().requires_permission {
                    let decision = permissions
                        .request(PermissionRequest {
                            tool_call_id: tool_call.id.clone(),
                            tool_name: tool_call.name.clone(),
                            title: title.clone(),
                            arguments: tool_call.arguments.clone(),
                        })
                        .await?;
                    if decision == PermissionDecision::Reject {
                        let output = ToolExecutionResult::error("Permission denied");
                        self.append_tool_result(&tool_call.id, &tool_call.name, output.clone())?;
                        sink.emit(SessionEvent::ToolCallResult {
                            tool_call_id: tool_call.id.clone(),
                            tool_name: tool_call.name.clone(),
                            output,
                        })
                        .await?;
                        continue;
                    }
                }

                let output = tool
                    .execute(&self.cwd, tool_call.arguments.clone())
                    .await
                    .unwrap_or_else(|error| ToolExecutionResult::error(error.to_string()));
                self.append_tool_result(&tool_call.id, &tool_call.name, output.clone())?;
                sink.emit(SessionEvent::ToolCallResult {
                    tool_call_id: tool_call.id.clone(),
                    tool_name: tool_call.name.clone(),
                    output,
                })
                .await?;
            }
        }
        Err(anyhow!("tool loop exceeded maximum depth"))
    }

    fn append_tool_result(
        &mut self,
        tool_call_id: &str,
        tool_name: &str,
        output: ToolExecutionResult,
    ) -> Result<()> {
        self.session_manager.push_message(AgentMessage::ToolResult {
            tool_call_id: tool_call_id.to_string(),
            tool_name: tool_name.to_string(),
            content: output.content,
            is_error: output.is_error,
            details: output.details,
            timestamp: chrono::Utc::now().timestamp_millis(),
        })
    }

    async fn handle_command(&mut self, prompt: &str) -> Result<Option<String>> {
        if !prompt.starts_with('/') {
            return Ok(None);
        }
        let mut parts = prompt[1..].trim().splitn(2, char::is_whitespace);
        let command = parts.next().unwrap_or_default();
        let rest = parts.next().unwrap_or("").trim();
        let text = match command {
            "compact" => Some(self.compact().await?),
            "model" => {
                if rest.is_empty() {
                    Some(format!("Current model: {}", self.model.id))
                } else {
                    None
                }
            }
            "session" => Some(format!(
                "Session {}\n{}",
                self.session_manager.header().id,
                self.session_manager.session_file().display()
            )),
            "new" => Some("Use the CLI / new-session command outside the prompt loop.".to_string()),
            _ => Some(format!("Unknown command /{command}")),
        };
        Ok(text)
    }
}
