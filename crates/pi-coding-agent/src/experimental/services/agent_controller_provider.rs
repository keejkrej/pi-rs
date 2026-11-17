//! Port of packages/coding-agent/src/experimental/services/agent-controller-provider.ts

use std::sync::Arc;

use async_trait::async_trait;
use pi_ai::types::{AssistantContentBlock, ImageContent, Message, TextContent, UserContentBlock, UserMessageContent};
use pi_chord::Context;
use pi_chord::services::handle::ServiceObject;
use pi_durable::types::{InputSubmissionState, SubmissionId, SubmissionKind, SubmissionState, id_from_number};
use pi_durable::{AbortSubmissionResult, Conversation, Harness, SubmissionDraft, WhenBusy, is_conversation_busy};

use crate::experimental::services::agent_controller::{
    AgentCompactionRequest, AgentController, AgentControllerApi, AgentOperationError, AgentOperationResponse,
    AgentPromptRequest, AgentPromptResult, AgentQueueResponse, CancelQueuedOutcome, CancelQueuedResult,
    agent_controller_to_service_object,
};

struct DurableAgentController {
    harness: Harness,
    conversation: Conversation,
}

impl DurableAgentController {
    async fn queue(
        &self,
        when_busy: WhenBusy,
        request: AgentPromptRequest,
        context: Context,
    ) -> pi_js::Result<AgentQueueResponse> {
        let draft = SubmissionDraft::Input {
            request_id: None,
            content: to_input(&request),
            when_busy: Some(when_busy),
        };
        match self.conversation.submit(draft, context).await {
            Ok(submission) => Ok(AgentQueueResponse {
                accepted: true,
                entry_id: Some(submission.id().as_i64().to_string()),
                error: None,
            }),
            Err(error) => Ok(AgentQueueResponse {
                accepted: false,
                entry_id: None,
                error: Some(to_agent_error(&error)),
            }),
        }
    }
}

#[async_trait]
impl AgentControllerApi for DurableAgentController {
    async fn prompt(&self, request: AgentPromptRequest, context: Context) -> pi_js::Result<AgentOperationResponse> {
        let draft = SubmissionDraft::Input {
            request_id: None,
            content: to_input(&request),
            when_busy: Some(WhenBusy::Reject),
        };
        match self.conversation.submit(draft, context).await {
            Ok(submission) => Ok(AgentOperationResponse {
                accepted: true,
                operation_id: Some(submission.id().as_i64().to_string()),
                error: None,
            }),
            Err(error) => Ok(AgentOperationResponse {
                accepted: false,
                operation_id: None,
                error: Some(to_agent_error(&error)),
            }),
        }
    }

    async fn steer(&self, request: AgentPromptRequest, context: Context) -> pi_js::Result<AgentQueueResponse> {
        self.queue(WhenBusy::Steer, request, context).await
    }

    async fn follow_up(&self, request: AgentPromptRequest, context: Context) -> pi_js::Result<AgentQueueResponse> {
        self.queue(WhenBusy::FollowUp, request, context).await
    }

    async fn cancel_queued(&self, entry_id: String, context: Context) -> pi_js::Result<CancelQueuedResult> {
        let Some(id) = parse_submission_id(&entry_id) else {
            return Ok(CancelQueuedResult {
                outcome: CancelQueuedOutcome::NotFound,
            });
        };
        let result = self
            .harness
            .abort_submission(id, context, Some(self.conversation.id()))
            .await?;
        let outcome = match result {
            AbortSubmissionResult::Aborted => CancelQueuedOutcome::Cancelled,
            AbortSubmissionResult::NotFound => CancelQueuedOutcome::NotFound,
            AbortSubmissionResult::AlreadyPlaced | AbortSubmissionResult::Settled => {
                CancelQueuedOutcome::AlreadyConsumed
            }
        };
        Ok(CancelQueuedResult { outcome })
    }

    async fn abort(&self, context: Context) -> pi_js::Result<()> {
        self.conversation.abort(context, None).await
    }

    async fn compact(
        &self,
        request: AgentCompactionRequest,
        context: Context,
    ) -> pi_js::Result<AgentOperationResponse> {
        match self.conversation.compact(request.custom_instructions, context).await {
            Ok(task_id) => Ok(AgentOperationResponse {
                accepted: true,
                operation_id: Some(task_id.as_i64().to_string()),
                error: None,
            }),
            Err(error) => Ok(AgentOperationResponse {
                accepted: false,
                operation_id: None,
                error: Some(to_agent_error(&error)),
            }),
        }
    }

    async fn wait_for_prompt(&self, operation_id: String, context: Context) -> pi_js::Result<AgentPromptResult> {
        let id = parse_submission_id(&operation_id);
        let submission = match id {
            Some(parsed_id) => self.harness.submission(parsed_id, context.clone()).await?,
            None => None,
        };
        let Some(submission) = submission else {
            return Err(pi_js::Error::msg(format!("Unknown prompt: {operation_id}")));
        };
        let settled = submission.wait(context.clone()).await?;
        match settled.record.state {
            SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. })
            | SubmissionState::Write(pi_durable::types::WriteSubmissionState::Unanswered { reason, .. }) => {
                Ok(AgentPromptResult::Unanswered { text: None, reason })
            }
            SubmissionState::Input(InputSubmissionState::Done { answer, .. }) => {
                let entry = self
                    .harness
                    .session()
                    .commit(move |tx| async move { tx.entry(answer).await }, context)
                    .await?;
                let first_msg = entry.and_then(|e| e.model).and_then(|mut msgs| {
                    if msgs.is_empty() {
                        None
                    } else {
                        Some(msgs.swap_remove(0))
                    }
                });
                let text = match first_msg {
                    Some(Message::Assistant(assistant)) => assistant
                        .content
                        .into_iter()
                        .filter_map(|block| match block {
                            AssistantContentBlock::Text(t) => Some(t.text),
                            _ => None,
                        })
                        .collect::<String>(),
                    _ => String::new(),
                };
                Ok(AgentPromptResult::Done { text, reason: None })
            }
            _ => Ok(AgentPromptResult::Done {
                text: String::new(),
                reason: None,
            }),
        }
    }
}

pub fn create_agent_controller_api(harness: Harness, conversation: Conversation) -> Arc<dyn AgentControllerApi> {
    Arc::new(DurableAgentController { harness, conversation })
}

pub fn create_agent_controller_service_object(harness: Harness, conversation: Conversation) -> ServiceObject {
    agent_controller_to_service_object(create_agent_controller_api(harness, conversation))
}

pub fn create_agent_controller(harness: Harness, conversation: Conversation) -> AgentController {
    AgentController::from_service(create_agent_controller_service_object(harness, conversation))
}

fn parse_submission_id(value: &str) -> Option<SubmissionId> {
    if value.is_empty() || value.starts_with('0') || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let parsed: i64 = value.parse().ok()?;
    Some(id_from_number::<SubmissionKind, pi_durable::JsonValue>(parsed))
}

fn to_input(request: &AgentPromptRequest) -> UserMessageContent {
    match &request.images {
        None => UserMessageContent::Text(request.message.clone()),
        Some(images) if images.is_empty() => UserMessageContent::Text(request.message.clone()),
        Some(images) => {
            let mut blocks = Vec::with_capacity(1 + images.len());
            blocks.push(UserContentBlock::Text(TextContent::new(request.message.clone())));
            for img in images {
                blocks.push(UserContentBlock::Image(ImageContent::new(
                    img.data.clone(),
                    img.mime_type.clone(),
                )));
            }
            UserMessageContent::Blocks(blocks)
        }
    }
}

fn to_agent_error(error: &pi_js::Error) -> AgentOperationError {
    if is_conversation_busy(error) {
        AgentOperationError {
            code: "busy".to_string(),
            message: error.message().to_string(),
        }
    } else {
        AgentOperationError {
            code: "operation_failed".to_string(),
            message: error.message().to_string(),
        }
    }
}
