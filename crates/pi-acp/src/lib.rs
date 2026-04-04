use agent_client_protocol::{self as acp, Client as _};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use pi_core::agent::{
    AgentSession, EventSink, PermissionDecision, PermissionHandler, PermissionRequest, SessionEvent,
};
use pi_core::config::{AuthStorage, StoredCredential};
use pi_core::messages::{AgentMessage, UserContentBlock};
use pi_core::models::ModelDescriptor;
use pi_core::session::SessionManager;
use pi_openai::OpenAiCodexProvider;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

struct AcpEventSink {
    session_id: acp::SessionId,
    conn: Rc<acp::AgentSideConnection>,
}

#[async_trait(?Send)]
impl EventSink for AcpEventSink {
    async fn emit(&self, event: SessionEvent) -> anyhow::Result<()> {
        let update = match event {
            SessionEvent::UserMessage(text) => {
                acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(text.into()))
            }
            SessionEvent::AssistantMessage(text) => {
                acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(text.into()))
            }
            SessionEvent::ToolCallStart {
                tool_call_id,
                tool_name,
                title,
                arguments,
            } => acp::SessionUpdate::ToolCall(
                acp::ToolCall::new(tool_call_id, title)
                    .kind(tool_kind(&tool_name))
                    .raw_input(arguments),
            ),
            SessionEvent::ToolCallResult {
                tool_call_id,
                output,
                ..
            } => acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                tool_call_id,
                acp::ToolCallUpdateFields::new()
                    .status(if output.is_error {
                        Some(acp::ToolCallStatus::Failed)
                    } else {
                        Some(acp::ToolCallStatus::Completed)
                    })
                    .content(Some(
                        output
                            .content
                            .into_iter()
                            .map(|block| match block {
                                UserContentBlock::Text(text) => {
                                    acp::ToolCallContent::from(text.text)
                                }
                                UserContentBlock::Image(image) => {
                                    acp::ToolCallContent::from(acp::ContentBlock::Image(
                                        acp::ImageContent::new(image.data, image.mime_type),
                                    ))
                                }
                            })
                            .collect(),
                    ))
                    .raw_output(output.details),
            )),
            SessionEvent::SessionInfo { title } => {
                acp::SessionUpdate::SessionInfoUpdate(acp::SessionInfoUpdate::new().title(title))
            }
        };
        self.conn
            .session_notification(acp::SessionNotification::new(
                self.session_id.clone(),
                update,
            ))
            .await
            .map_err(|error| anyhow!(error.to_string()))
    }
}

struct AcpPermissionHandler {
    session_id: acp::SessionId,
    conn: Rc<acp::AgentSideConnection>,
}

#[async_trait(?Send)]
impl PermissionHandler for AcpPermissionHandler {
    async fn request(&self, request: PermissionRequest) -> anyhow::Result<PermissionDecision> {
        let response = self
            .conn
            .request_permission(acp::RequestPermissionRequest::new(
                self.session_id.clone(),
                acp::ToolCallUpdate::new(
                    request.tool_call_id.clone(),
                    acp::ToolCallUpdateFields::new()
                        .title(request.title)
                        .kind(Some(tool_kind(&request.tool_name)))
                        .raw_input(Some(request.arguments)),
                ),
                vec![
                    acp::PermissionOption::new(
                        "allow_once",
                        "Allow once",
                        acp::PermissionOptionKind::AllowOnce,
                    ),
                    acp::PermissionOption::new(
                        "reject_once",
                        "Reject",
                        acp::PermissionOptionKind::RejectOnce,
                    ),
                ],
            ))
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        Ok(match response.outcome {
            acp::RequestPermissionOutcome::Cancelled => PermissionDecision::Reject,
            acp::RequestPermissionOutcome::Selected(selected) => {
                if selected.option_id.0.as_ref() == "allow_once" {
                    PermissionDecision::AllowOnce
                } else {
                    PermissionDecision::Reject
                }
            }
            _ => PermissionDecision::Reject,
        })
    }
}

fn tool_kind(name: &str) -> acp::ToolKind {
    match name {
        "read" => acp::ToolKind::Read,
        "write" | "edit" => acp::ToolKind::Edit,
        "grep" | "find" | "ls" => acp::ToolKind::Search,
        "bash" => acp::ToolKind::Execute,
        _ => acp::ToolKind::Other,
    }
}

pub struct PiAcpAgent {
    provider: Arc<OpenAiCodexProvider>,
    sessions: Mutex<HashMap<String, AgentSession>>,
    conn: RefCell<Option<Rc<acp::AgentSideConnection>>>,
}

impl PiAcpAgent {
    pub fn new(provider: Arc<OpenAiCodexProvider>) -> Self {
        Self {
            provider,
            sessions: Mutex::new(HashMap::new()),
            conn: RefCell::new(None),
        }
    }

    fn connection(&self) -> Result<Rc<acp::AgentSideConnection>> {
        self.conn
            .borrow()
            .clone()
            .ok_or_else(|| anyhow!("ACP connection not initialized"))
    }

    pub fn set_connection(&self, conn: Rc<acp::AgentSideConnection>) {
        *self.conn.borrow_mut() = Some(conn);
    }

    fn require_auth(&self) -> acp::Result<()> {
        let auth = AuthStorage::new().map_err(internal_error)?;
        match auth.load("openai-codex").map_err(internal_error)? {
            Some(StoredCredential::OAuth { .. }) => Ok(()),
            None => Err(acp::Error::auth_required()),
        }
    }
}

#[async_trait(?Send)]
impl acp::Agent for PiAcpAgent {
    async fn initialize(
        &self,
        _args: acp::InitializeRequest,
    ) -> acp::Result<acp::InitializeResponse> {
        Ok(acp::InitializeResponse::new(acp::ProtocolVersion::V1)
            .agent_info(acp::Implementation::new("pi-rs", "0.1.0").title("pi-rs"))
            .auth_methods(vec![acp::AuthMethod::Terminal(
                acp::AuthMethodTerminal::new("terminal-login", "Terminal Login")
                    .description("Run `pi login` in a terminal to authenticate with ChatGPT Codex.")
                    .args(vec!["login".to_string()]),
            )]))
    }

    async fn authenticate(
        &self,
        _args: acp::AuthenticateRequest,
    ) -> acp::Result<acp::AuthenticateResponse> {
        self.require_auth()?;
        Ok(acp::AuthenticateResponse::new())
    }

    async fn new_session(
        &self,
        args: acp::NewSessionRequest,
    ) -> acp::Result<acp::NewSessionResponse> {
        self.require_auth()?;
        let cwd = args.cwd;
        let manager = SessionManager::new(&cwd).map_err(internal_error)?;
        let session_id = manager.header().id.clone();
        let model = ModelDescriptor::defaults()
            .into_iter()
            .next()
            .ok_or_else(acp::Error::internal_error)?;
        let session = AgentSession::new(
            cwd.clone(),
            manager,
            model.clone(),
            "medium",
            self.provider.clone(),
        )
        .map_err(internal_error)?;
        self.sessions
            .lock()
            .await
            .insert(session_id.clone(), session);
        Ok(acp::NewSessionResponse::new(session_id))
    }

    async fn load_session(
        &self,
        args: acp::LoadSessionRequest,
    ) -> acp::Result<acp::LoadSessionResponse> {
        self.require_auth()?;
        let manager = SessionManager::load(&args.cwd.join(format!("{}.jsonl", args.session_id.0)))
            .or_else(|_| SessionManager::load(&args.cwd))
            .map_err(internal_error)?;
        let session_id = manager.header().id.clone();
        let model = ModelDescriptor::defaults()
            .into_iter()
            .next()
            .ok_or_else(acp::Error::internal_error)?;
        let session = AgentSession::new(
            PathBuf::from(&manager.header().cwd),
            manager.clone(),
            model,
            "medium",
            self.provider.clone(),
        )
        .map_err(internal_error)?;
        let sink = AcpEventSink {
            session_id: acp::SessionId::new(session_id.clone()),
            conn: self.connection().map_err(internal_error)?,
        };
        for message in manager.messages() {
            match message {
                AgentMessage::User { content, .. } => {
                    let text = content
                        .into_iter()
                        .filter_map(|block| match block {
                            UserContentBlock::Text(text) => Some(text.text),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    sink.emit(SessionEvent::UserMessage(text))
                        .await
                        .map_err(internal_error)?;
                }
                AgentMessage::Assistant { content, .. } => {
                    let text = content
                        .into_iter()
                        .filter_map(|block| match block {
                            pi_core::messages::AssistantContentBlock::Text(text) => Some(text.text),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    sink.emit(SessionEvent::AssistantMessage(text))
                        .await
                        .map_err(internal_error)?;
                }
                _ => {}
            }
        }
        self.sessions.lock().await.insert(session_id, session);
        Ok(acp::LoadSessionResponse::new())
    }

    async fn prompt(&self, args: acp::PromptRequest) -> acp::Result<acp::PromptResponse> {
        self.require_auth()?;
        let conn = self.connection().map_err(internal_error)?;
        let mut sessions = self.sessions.lock().await;
        let session = sessions
            .get_mut(args.session_id.0.as_ref())
            .ok_or_else(acp::Error::invalid_params)?;
        let prompt = args
            .prompt
            .iter()
            .filter_map(|block| match block {
                acp::ContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let sink = AcpEventSink {
            session_id: args.session_id.clone(),
            conn: conn.clone(),
        };
        let permissions = AcpPermissionHandler {
            session_id: args.session_id.clone(),
            conn,
        };
        let _ = session
            .prompt(prompt, &sink, &permissions)
            .await
            .map_err(internal_error)?;
        Ok(acp::PromptResponse::new(acp::StopReason::EndTurn))
    }

    async fn cancel(&self, _args: acp::CancelNotification) -> acp::Result<()> {
        Ok(())
    }

    async fn set_session_mode(
        &self,
        _args: acp::SetSessionModeRequest,
    ) -> acp::Result<acp::SetSessionModeResponse> {
        Ok(acp::SetSessionModeResponse::default())
    }

    async fn set_session_config_option(
        &self,
        _args: acp::SetSessionConfigOptionRequest,
    ) -> acp::Result<acp::SetSessionConfigOptionResponse> {
        Ok(acp::SetSessionConfigOptionResponse::new(vec![]))
    }

    async fn list_sessions(
        &self,
        _args: acp::ListSessionsRequest,
    ) -> acp::Result<acp::ListSessionsResponse> {
        Ok(acp::ListSessionsResponse::new(vec![]))
    }

    async fn logout(&self, _args: acp::LogoutRequest) -> acp::Result<acp::LogoutResponse> {
        self.provider.logout().map_err(internal_error)?;
        Ok(acp::LogoutResponse::default())
    }
}

pub async fn run_acp_stdio() -> Result<()> {
    let provider = Arc::new(OpenAiCodexProvider::new()?);
    let agent = Rc::new(PiAcpAgent::new(provider));
    let local_set = tokio::task::LocalSet::new();
    local_set
        .run_until(async move {
            let outgoing = tokio::io::stdout().compat_write();
            let incoming = tokio::io::stdin().compat();
            let agent_for_conn = agent.clone();
            let (conn, io_task) =
                acp::AgentSideConnection::new(agent_for_conn, outgoing, incoming, |fut| {
                    tokio::task::spawn_local(fut);
                });
            agent.set_connection(Rc::new(conn));
            io_task.await.map_err(|error| anyhow!(error.to_string()))
        })
        .await
}

fn internal_error(error: anyhow::Error) -> acp::Error {
    acp::Error::internal_error().data(error.to_string())
}
