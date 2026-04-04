use crate::messages::{
    AgentMessage, AssistantContentBlock, TextContent, UserContentBlock, UserMessage,
};
use crate::models::{CompletionRequest, ModelDescriptor, ModelProvider};
use crate::session::{SessionInfo, SessionManager};
use crate::skills::build_system_prompt;
use crate::tools::{BuiltInToolRegistry, ToolExecutionResult};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptStopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRunState {
    Idle,
    Running,
    Cancelling,
}

#[derive(Debug, Clone)]
pub struct PromptOutcome {
    pub output: String,
    pub stop_reason: PromptStopReason,
}

#[derive(Debug, Clone)]
pub enum SessionEvent {
    UserMessage {
        content: Vec<UserContentBlock>,
    },
    AssistantMessage {
        content: Vec<AssistantContentBlock>,
    },
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
        updated_at: String,
    },
}

#[derive(Clone, Default)]
pub struct SessionControl {
    inner: Arc<SessionControlInner>,
}

#[derive(Default)]
struct SessionControlInner {
    state: AtomicU8,
    cancellation: Mutex<Option<CancellationToken>>,
    last_stop_reason: Mutex<Option<PromptStopReason>>,
}

impl SessionControl {
    pub fn run_state(&self) -> SessionRunState {
        match self.inner.state.load(Ordering::SeqCst) {
            1 => SessionRunState::Running,
            2 => SessionRunState::Cancelling,
            _ => SessionRunState::Idle,
        }
    }

    pub fn last_stop_reason(&self) -> Option<PromptStopReason> {
        *self.inner.last_stop_reason.lock().expect("lock poisoned")
    }

    pub fn begin_turn(&self) -> CancellationToken {
        let token = CancellationToken::new();
        *self.inner.cancellation.lock().expect("lock poisoned") = Some(token.clone());
        self.inner.state.store(1, Ordering::SeqCst);
        *self.inner.last_stop_reason.lock().expect("lock poisoned") = None;
        token
    }

    pub fn finish_turn(&self, stop_reason: PromptStopReason) {
        *self.inner.cancellation.lock().expect("lock poisoned") = None;
        self.inner.state.store(0, Ordering::SeqCst);
        *self.inner.last_stop_reason.lock().expect("lock poisoned") = Some(stop_reason);
    }

    pub fn cancel(&self) {
        self.inner.state.store(2, Ordering::SeqCst);
        if let Some(token) = self
            .inner
            .cancellation
            .lock()
            .expect("lock poisoned")
            .as_ref()
        {
            token.cancel();
        }
    }
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
    control: SessionControl,
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
            control: SessionControl::default(),
        })
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn control(&self) -> SessionControl {
        self.control.clone()
    }

    pub fn session_info(&self) -> Result<SessionInfo> {
        self.session_manager.session_info()
    }

    pub fn title(&self) -> String {
        self.session_manager.title()
    }

    pub fn run_state(&self) -> SessionRunState {
        self.control.run_state()
    }

    pub fn current_model(&self) -> &ModelDescriptor {
        &self.model
    }

    pub fn current_thinking_level(&self) -> &str {
        &self.thinking_level
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

    pub async fn prompt_text(
        &mut self,
        prompt: String,
        sink: &dyn EventSink,
        permissions: &dyn PermissionHandler,
    ) -> Result<PromptOutcome> {
        self.prompt_content(
            vec![UserContentBlock::Text(TextContent::new(prompt))],
            sink,
            permissions,
        )
        .await
    }

    pub async fn prompt_content(
        &mut self,
        content: Vec<UserContentBlock>,
        sink: &dyn EventSink,
        permissions: &dyn PermissionHandler,
    ) -> Result<PromptOutcome> {
        if let Some(outcome) = self.handle_command(&content, sink).await? {
            return Ok(outcome);
        }

        let user_message = UserMessage {
            role: "user".to_string(),
            content: content.clone(),
            timestamp: chrono::Utc::now().timestamp_millis(),
        };
        self.session_manager.push_message(AgentMessage::User {
            content: user_message.content.clone(),
            timestamp: user_message.timestamp,
        })?;
        sink.emit(SessionEvent::UserMessage { content }).await?;
        self.emit_session_info(sink).await?;

        let token = self.control.begin_turn();
        let outcome = self.prompt_loop(&token, sink, permissions).await;
        let stop_reason = outcome
            .as_ref()
            .map(|outcome| outcome.stop_reason)
            .unwrap_or(PromptStopReason::EndTurn);
        self.control.finish_turn(stop_reason);
        outcome
    }

    async fn prompt_loop(
        &mut self,
        token: &CancellationToken,
        sink: &dyn EventSink,
        permissions: &dyn PermissionHandler,
    ) -> Result<PromptOutcome> {
        let mut final_text = String::new();
        for _ in 0..16 {
            if token.is_cancelled() {
                return Ok(PromptOutcome {
                    output: final_text,
                    stop_reason: PromptStopReason::Cancelled,
                });
            }

            let response = tokio::select! {
                _ = token.cancelled() => {
                    return Ok(PromptOutcome {
                        output: final_text,
                        stop_reason: PromptStopReason::Cancelled,
                    });
                }
                response = self.provider.complete(CompletionRequest {
                    model: self.model.clone(),
                    system_prompt: self.system_prompt.clone(),
                    messages: self.session_manager.messages(),
                    tools: self.tools.specs(),
                }) => response?,
            };

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

            if !response.assistant_blocks.is_empty() {
                final_text.push_str(&response.final_text());
                sink.emit(SessionEvent::AssistantMessage {
                    content: response.assistant_blocks.clone(),
                })
                .await?;
            }

            if response.tool_calls.is_empty() {
                return Ok(PromptOutcome {
                    output: final_text,
                    stop_reason: stop_reason_from_provider(&response.stop_reason),
                });
            }

            for tool_call in response.tool_calls {
                if token.is_cancelled() {
                    return Ok(PromptOutcome {
                        output: final_text,
                        stop_reason: PromptStopReason::Cancelled,
                    });
                }

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
                    let decision = tokio::select! {
                        _ = token.cancelled() => {
                            return Ok(PromptOutcome {
                                output: final_text,
                                stop_reason: PromptStopReason::Cancelled,
                            });
                        }
                        decision = permissions.request(PermissionRequest {
                            tool_call_id: tool_call.id.clone(),
                            tool_name: tool_call.name.clone(),
                            title: title.clone(),
                            arguments: tool_call.arguments.clone(),
                        }) => decision?,
                    };
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

                let output = tokio::select! {
                    _ = token.cancelled() => {
                        return Ok(PromptOutcome {
                            output: final_text,
                            stop_reason: PromptStopReason::Cancelled,
                        });
                    }
                    output = tool.execute(&self.cwd, tool_call.arguments.clone()) => {
                        output.unwrap_or_else(|error| ToolExecutionResult::error(error.to_string()))
                    }
                };
                self.append_tool_result(&tool_call.id, &tool_call.name, output.clone())?;
                sink.emit(SessionEvent::ToolCallResult {
                    tool_call_id: tool_call.id.clone(),
                    tool_name: tool_call.name.clone(),
                    output,
                })
                .await?;
            }
        }

        Ok(PromptOutcome {
            output: final_text,
            stop_reason: PromptStopReason::MaxTurnRequests,
        })
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

    async fn emit_session_info(&self, sink: &dyn EventSink) -> Result<()> {
        let info = self.session_info()?;
        sink.emit(SessionEvent::SessionInfo {
            title: info.title,
            updated_at: info.updated_at,
        })
        .await
    }

    async fn handle_command(
        &mut self,
        content: &[UserContentBlock],
        sink: &dyn EventSink,
    ) -> Result<Option<PromptOutcome>> {
        let Some(prompt) = plain_text_command(content) else {
            return Ok(None);
        };
        if !prompt.starts_with('/') {
            return Ok(None);
        }

        let mut parts = prompt[1..].trim().splitn(2, char::is_whitespace);
        let command = parts.next().unwrap_or_default();
        let rest = parts.next().unwrap_or("").trim();
        let text = match command {
            "compact" => self.compact().await?,
            "model" => {
                if rest.is_empty() {
                    format!("Current model: {}", self.model.id)
                } else {
                    let next = ModelDescriptor::defaults()
                        .into_iter()
                        .find(|model| model.id == rest)
                        .ok_or_else(|| anyhow!("unknown model {rest}"))?;
                    self.set_model(next)?;
                    format!("Model set to {}", self.model.id)
                }
            }
            "session" => format!(
                "Session {}\n{}",
                self.session_manager.header().id,
                self.session_manager.session_file().display()
            ),
            _ => return Ok(None),
        };
        self.emit_session_info(sink).await?;
        sink.emit(SessionEvent::AssistantMessage {
            content: vec![AssistantContentBlock::Text(TextContent::new(text.clone()))],
        })
        .await?;
        Ok(Some(PromptOutcome {
            output: text,
            stop_reason: PromptStopReason::EndTurn,
        }))
    }
}

fn stop_reason_from_provider(stop_reason: &str) -> PromptStopReason {
    match stop_reason {
        "max_output_tokens" => PromptStopReason::MaxTokens,
        "refusal" => PromptStopReason::Refusal,
        _ => PromptStopReason::EndTurn,
    }
}

fn plain_text_command(content: &[UserContentBlock]) -> Option<String> {
    let mut text = Vec::new();
    for block in content {
        match block {
            UserContentBlock::Text(block) => text.push(block.text.clone()),
            UserContentBlock::Image(_) => return None,
        }
    }
    Some(text.join("\n").trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{AssistantContentBlock, TextContent, Usage};
    use crate::models::{CompletionResponse, CompletionToolCall};
    use anyhow::Result;
    use std::sync::Mutex as StdMutex;
    use tokio::time::{Duration, sleep};
    use uuid::Uuid;

    #[derive(Default)]
    struct TestProvider {
        responses: StdMutex<Vec<CompletionResponse>>,
        delay_ms: u64,
    }

    #[async_trait]
    impl ModelProvider for TestProvider {
        async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse> {
            if self.delay_ms > 0 {
                sleep(Duration::from_millis(self.delay_ms)).await;
            }
            Ok(self.responses.lock().expect("lock poisoned").remove(0))
        }
    }

    fn test_session(provider: Arc<dyn ModelProvider>) -> AgentSession {
        let cwd = std::env::temp_dir().join(format!("pi-rs-agent-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&cwd).unwrap();
        AgentSession::new(
            cwd.clone(),
            SessionManager::new(&cwd).unwrap(),
            ModelDescriptor::by_id("gpt-5.4").unwrap(),
            "medium",
            provider,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn model_command_updates_session_state() {
        let provider = Arc::new(TestProvider::default());
        let mut session = test_session(provider);

        let outcome = session
            .prompt_text(
                "/model gpt-5.4-mini".to_string(),
                &NullEventSink,
                &AllowAllPermissions,
            )
            .await
            .unwrap();

        assert_eq!(outcome.stop_reason, PromptStopReason::EndTurn);
        assert_eq!(session.current_model().id, "gpt-5.4-mini");
        assert_eq!(
            session.session_manager.current_model_id().as_deref(),
            Some("gpt-5.4-mini")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_prompt_reports_cancelled_stop_reason() {
        let provider = Arc::new(TestProvider {
            responses: StdMutex::new(vec![CompletionResponse {
                assistant_blocks: vec![AssistantContentBlock::Text(TextContent::new("late"))],
                tool_calls: Vec::<CompletionToolCall>::new(),
                usage: Usage::default(),
                stop_reason: "stop".to_string(),
            }]),
            delay_ms: 200,
        });
        let session = test_session(provider);
        let control = session.control();
        let local = tokio::task::LocalSet::new();
        let outcome = local
            .run_until(async move {
                let handle = tokio::task::spawn_local(async move {
                    let mut session = session;
                    session
                        .prompt_text("hello".to_string(), &NullEventSink, &AllowAllPermissions)
                        .await
                        .unwrap()
                });
                sleep(Duration::from_millis(50)).await;
                control.cancel();
                handle.await.unwrap()
            })
            .await;
        assert_eq!(outcome.stop_reason, PromptStopReason::Cancelled);
    }
}
