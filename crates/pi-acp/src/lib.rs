use agent_client_protocol::{self as acp, Client as _};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use pi_core::agent::{
    AgentSession, EventSink, PermissionDecision, PermissionHandler, PermissionRequest,
    PromptStopReason, SessionControl, SessionEvent,
};
use pi_core::config::{AuthStorage, SettingsManager, StoredCredential};
use pi_core::messages::{AssistantContentBlock, ImageContent, UserContentBlock};
use pi_core::models::ModelDescriptor;
use pi_core::session::SessionManager;
use pi_openai::OpenAiCodexProvider;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

const DEFAULT_MODE_ID: &str = "default";
const MODEL_CONFIG_ID: &str = "model";
const THINKING_CONFIG_ID: &str = "thinking_level";

struct AcpEventSink {
    session_id: acp::SessionId,
    conn: Rc<acp::AgentSideConnection>,
}

#[async_trait(?Send)]
impl EventSink for AcpEventSink {
    async fn emit(&self, event: SessionEvent) -> anyhow::Result<()> {
        match event {
            SessionEvent::UserMessage { content } => {
                for block in user_blocks_to_acp(content) {
                    self.conn
                        .session_notification(acp::SessionNotification::new(
                            self.session_id.clone(),
                            acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(block)),
                        ))
                        .await
                        .map_err(|error| anyhow!(error.to_string()))?;
                }
            }
            SessionEvent::AssistantMessage { content } => {
                for update in assistant_blocks_to_updates(content) {
                    self.conn
                        .session_notification(acp::SessionNotification::new(
                            self.session_id.clone(),
                            update,
                        ))
                        .await
                        .map_err(|error| anyhow!(error.to_string()))?;
                }
            }
            SessionEvent::ToolCallStart {
                tool_call_id,
                tool_name,
                title,
                arguments,
            } => {
                self.conn
                    .session_notification(acp::SessionNotification::new(
                        self.session_id.clone(),
                        acp::SessionUpdate::ToolCall(
                            acp::ToolCall::new(tool_call_id, title)
                                .kind(tool_kind(&tool_name))
                                .raw_input(arguments),
                        ),
                    ))
                    .await
                    .map_err(|error| anyhow!(error.to_string()))?;
            }
            SessionEvent::ToolCallResult {
                tool_call_id,
                output,
                ..
            } => {
                self.conn
                    .session_notification(acp::SessionNotification::new(
                        self.session_id.clone(),
                        acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
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
                                        .map(|block| {
                                            acp::ToolCallContent::from(user_block_to_acp(block))
                                        })
                                        .collect(),
                                ))
                                .raw_output(output.details),
                        )),
                    ))
                    .await
                    .map_err(|error| anyhow!(error.to_string()))?;
            }
            SessionEvent::SessionInfo { title, updated_at } => {
                self.conn
                    .session_notification(acp::SessionNotification::new(
                        self.session_id.clone(),
                        acp::SessionUpdate::SessionInfoUpdate(
                            acp::SessionInfoUpdate::new()
                                .title(title)
                                .updated_at(updated_at),
                        ),
                    ))
                    .await
                    .map_err(|error| anyhow!(error.to_string()))?;
            }
        }
        Ok(())
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

#[derive(Clone)]
struct ManagedSession {
    session: Arc<Mutex<AgentSession>>,
    control: SessionControl,
}

impl ManagedSession {
    fn new(session: AgentSession) -> Self {
        let control = session.control();
        Self {
            session: Arc::new(Mutex::new(session)),
            control,
        }
    }
}

pub struct PiAcpAgent {
    provider: Arc<OpenAiCodexProvider>,
    sessions: Mutex<HashMap<String, ManagedSession>>,
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

    async fn cached_or_loaded_session(
        &self,
        cwd: &Path,
        session_id: &str,
    ) -> acp::Result<ManagedSession> {
        if let Some(session) = self.sessions.lock().await.get(session_id).cloned() {
            return Ok(session);
        }
        let session = self
            .load_managed_session(cwd, session_id)
            .map_err(internal_error)?;
        self.sessions
            .lock()
            .await
            .insert(session_id.to_string(), session.clone());
        Ok(session)
    }

    fn load_managed_session(&self, cwd: &Path, session_id: &str) -> Result<ManagedSession> {
        let manager = SessionManager::load_by_id(cwd, session_id)?;
        let session = build_agent_session(manager, self.provider.clone())?;
        Ok(ManagedSession::new(session))
    }

    async fn cached_session(&self, session_id: &str) -> acp::Result<ManagedSession> {
        self.sessions
            .lock()
            .await
            .get(session_id)
            .cloned()
            .ok_or_else(acp::Error::invalid_params)
    }
}

#[async_trait(?Send)]
impl acp::Agent for PiAcpAgent {
    async fn initialize(
        &self,
        _args: acp::InitializeRequest,
    ) -> acp::Result<acp::InitializeResponse> {
        let capabilities = acp::AgentCapabilities::new()
            .load_session(true)
            .prompt_capabilities(acp::PromptCapabilities::new().image(true))
            .session_capabilities(
                acp::SessionCapabilities::new().list(acp::SessionListCapabilities::new()),
            )
            .auth(acp::AgentAuthCapabilities::new().logout(acp::LogoutCapabilities::new()));

        Ok(acp::InitializeResponse::new(acp::ProtocolVersion::V1)
            .agent_capabilities(capabilities)
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
        let manager = SessionManager::new(&args.cwd).map_err(internal_error)?;
        let session_id = manager.header().id.clone();
        let session = ManagedSession::new(
            build_agent_session(manager, self.provider.clone()).map_err(internal_error)?,
        );
        self.sessions
            .lock()
            .await
            .insert(session_id.clone(), session.clone());

        let session = session.session.lock().await;
        Ok(acp::NewSessionResponse::new(session_id)
            .modes(session_modes())
            .config_options(session_config_options(&session)))
    }

    async fn load_session(
        &self,
        args: acp::LoadSessionRequest,
    ) -> acp::Result<acp::LoadSessionResponse> {
        self.require_auth()?;
        let managed = self
            .cached_or_loaded_session(&args.cwd, args.session_id.0.as_ref())
            .await?;
        let conn = self.connection().map_err(internal_error)?;
        let sink = AcpEventSink {
            session_id: args.session_id.clone(),
            conn,
        };

        let session = managed.session.lock().await;
        replay_session(&session, &sink)
            .await
            .map_err(internal_error)?;
        Ok(acp::LoadSessionResponse::new()
            .modes(session_modes())
            .config_options(session_config_options(&session)))
    }

    async fn prompt(&self, args: acp::PromptRequest) -> acp::Result<acp::PromptResponse> {
        self.require_auth()?;
        let conn = self.connection().map_err(internal_error)?;
        let managed = self.cached_session(args.session_id.0.as_ref()).await?;
        let sink = AcpEventSink {
            session_id: args.session_id.clone(),
            conn: conn.clone(),
        };
        let permissions = AcpPermissionHandler {
            session_id: args.session_id.clone(),
            conn,
        };
        let prompt = prompt_content_to_user_blocks(args.prompt).map_err(internal_error)?;

        let mut session = managed.session.lock().await;
        let outcome = session
            .prompt_content(prompt, &sink, &permissions)
            .await
            .map_err(internal_error)?;

        Ok(
            acp::PromptResponse::new(map_stop_reason(outcome.stop_reason))
                .user_message_id(args.message_id),
        )
    }

    async fn cancel(&self, args: acp::CancelNotification) -> acp::Result<()> {
        if let Some(session) = self
            .sessions
            .lock()
            .await
            .get(args.session_id.0.as_ref())
            .cloned()
        {
            session.control.cancel();
        }
        Ok(())
    }

    async fn set_session_mode(
        &self,
        args: acp::SetSessionModeRequest,
    ) -> acp::Result<acp::SetSessionModeResponse> {
        if args.mode_id.0.as_ref() != DEFAULT_MODE_ID {
            return Err(acp::Error::invalid_params().data("unsupported mode"));
        }
        let conn = self.connection().map_err(internal_error)?;
        conn.session_notification(acp::SessionNotification::new(
            args.session_id,
            acp::SessionUpdate::CurrentModeUpdate(acp::CurrentModeUpdate::new(DEFAULT_MODE_ID)),
        ))
        .await
        .map_err(|error| internal_error(anyhow!(error.to_string())))?;
        Ok(acp::SetSessionModeResponse::default())
    }

    async fn set_session_config_option(
        &self,
        args: acp::SetSessionConfigOptionRequest,
    ) -> acp::Result<acp::SetSessionConfigOptionResponse> {
        let managed = self.cached_session(args.session_id.0.as_ref()).await?;
        let mut session = managed.session.lock().await;
        match args.config_id.0.as_ref() {
            MODEL_CONFIG_ID => {
                let value = args
                    .value
                    .as_value_id()
                    .ok_or_else(acp::Error::invalid_params)?;
                let model = ModelDescriptor::by_id(value.0.as_ref())
                    .ok_or_else(acp::Error::invalid_params)?;
                session.set_model(model).map_err(internal_error)?;
            }
            THINKING_CONFIG_ID => {
                let value = args
                    .value
                    .as_value_id()
                    .ok_or_else(acp::Error::invalid_params)?;
                let thinking_level = value.0.as_ref();
                if !["low", "medium", "high"].contains(&thinking_level) {
                    return Err(acp::Error::invalid_params().data("unsupported thinking level"));
                }
                session
                    .set_thinking_level(thinking_level.to_string())
                    .map_err(internal_error)?;
            }
            _ => return Err(acp::Error::invalid_params().data("unknown config option")),
        }
        let config_options = session_config_options(&session);
        let conn = self.connection().map_err(internal_error)?;
        conn.session_notification(acp::SessionNotification::new(
            args.session_id.clone(),
            acp::SessionUpdate::ConfigOptionUpdate(acp::ConfigOptionUpdate::new(
                config_options.clone(),
            )),
        ))
        .await
        .map_err(|error| internal_error(anyhow!(error.to_string())))?;
        Ok(acp::SetSessionConfigOptionResponse::new(config_options))
    }

    async fn list_sessions(
        &self,
        args: acp::ListSessionsRequest,
    ) -> acp::Result<acp::ListSessionsResponse> {
        let cwd = args
            .cwd
            .unwrap_or(std::env::current_dir().map_err(|error| internal_error(error.into()))?);
        let sessions = SessionManager::list_for_cwd(&cwd)
            .map_err(internal_error)?
            .into_iter()
            .map(|session| {
                acp::SessionInfo::new(session.id, session.cwd)
                    .title(session.title)
                    .updated_at(session.updated_at)
            })
            .collect();
        Ok(acp::ListSessionsResponse::new(sessions))
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

fn build_agent_session(
    manager: SessionManager,
    provider: Arc<OpenAiCodexProvider>,
) -> Result<AgentSession> {
    let cwd = PathBuf::from(manager.header().cwd.clone());
    let settings = SettingsManager::new(&cwd)?.merged()?;
    let model_id = manager
        .current_model_id()
        .or(settings.default_model)
        .unwrap_or_else(|| "gpt-5.4".to_string());
    let model = ModelDescriptor::by_id(&model_id)
        .or_else(|| ModelDescriptor::defaults().into_iter().next())
        .ok_or_else(|| anyhow!("no default models available"))?;
    let thinking_level = manager
        .current_thinking_level()
        .or(settings.default_thinking_level)
        .unwrap_or_else(|| "medium".to_string());
    AgentSession::new(cwd, manager, model, thinking_level, provider)
}

fn session_modes() -> acp::SessionModeState {
    acp::SessionModeState::new(
        DEFAULT_MODE_ID,
        vec![
            acp::SessionMode::new(DEFAULT_MODE_ID, "Default")
                .description("General coding agent mode"),
        ],
    )
}

fn session_config_options(session: &AgentSession) -> Vec<acp::SessionConfigOption> {
    vec![
        acp::SessionConfigOption::select(
            MODEL_CONFIG_ID,
            "Model",
            session.current_model().id.clone(),
            ModelDescriptor::defaults()
                .into_iter()
                .map(|model| acp::SessionConfigSelectOption::new(model.id, model.name))
                .collect::<Vec<_>>(),
        )
        .category(acp::SessionConfigOptionCategory::Model)
        .description("Model used for new turns"),
        acp::SessionConfigOption::select(
            THINKING_CONFIG_ID,
            "Thinking level",
            session.current_thinking_level().to_string(),
            vec![
                acp::SessionConfigSelectOption::new("low", "Low"),
                acp::SessionConfigSelectOption::new("medium", "Medium"),
                acp::SessionConfigSelectOption::new("high", "High"),
            ],
        )
        .category(acp::SessionConfigOptionCategory::ThoughtLevel)
        .description("Reasoning depth for future turns"),
    ]
}

async fn replay_session(session: &AgentSession, sink: &dyn EventSink) -> Result<()> {
    sink.emit(SessionEvent::SessionInfo {
        title: session.title(),
        updated_at: session.session_info()?.updated_at,
    })
    .await?;
    for message in session.session_manager.messages() {
        match message {
            pi_core::messages::AgentMessage::User { content, .. } => {
                sink.emit(SessionEvent::UserMessage { content }).await?;
            }
            pi_core::messages::AgentMessage::Assistant { content, .. } => {
                sink.emit(SessionEvent::AssistantMessage { content })
                    .await?;
            }
            pi_core::messages::AgentMessage::ToolResult {
                tool_call_id,
                tool_name,
                content,
                is_error,
                details,
                ..
            } => {
                sink.emit(SessionEvent::ToolCallResult {
                    tool_call_id,
                    tool_name,
                    output: pi_core::tools::ToolExecutionResult {
                        content,
                        is_error,
                        details,
                    },
                })
                .await?;
            }
            pi_core::messages::AgentMessage::CompactionSummary { summary, .. } => {
                sink.emit(SessionEvent::AssistantMessage {
                    content: vec![AssistantContentBlock::Text(
                        pi_core::messages::TextContent::new(summary),
                    )],
                })
                .await?;
            }
        }
    }
    Ok(())
}

fn prompt_content_to_user_blocks(prompt: Vec<acp::ContentBlock>) -> Result<Vec<UserContentBlock>> {
    let mut blocks = Vec::new();
    for block in prompt {
        match block {
            acp::ContentBlock::Text(text) => blocks.push(UserContentBlock::Text(
                pi_core::messages::TextContent::new(text.text),
            )),
            acp::ContentBlock::Image(image) => blocks.push(UserContentBlock::Image(
                ImageContent::new(image.data, image.mime_type),
            )),
            acp::ContentBlock::Resource(resource) => blocks.push(UserContentBlock::Text(
                pi_core::messages::TextContent::new(resource_text(resource)),
            )),
            acp::ContentBlock::ResourceLink(resource) => blocks.push(UserContentBlock::Text(
                pi_core::messages::TextContent::new(format!("Resource: {}", resource.uri)),
            )),
            acp::ContentBlock::Audio(_) => {
                return Err(anyhow!("audio prompts are not supported"));
            }
            _ => {
                return Err(anyhow!("unsupported prompt content block"));
            }
        }
    }
    Ok(blocks)
}

fn resource_text(resource: acp::EmbeddedResource) -> String {
    match resource.resource {
        acp::EmbeddedResourceResource::TextResourceContents(text) => text.text,
        acp::EmbeddedResourceResource::BlobResourceContents(blob) => {
            format!("Embedded resource: {}", blob.uri)
        }
        _ => "Embedded resource".to_string(),
    }
}

fn user_blocks_to_acp(content: Vec<UserContentBlock>) -> Vec<acp::ContentBlock> {
    content.into_iter().map(user_block_to_acp).collect()
}

fn user_block_to_acp(block: UserContentBlock) -> acp::ContentBlock {
    match block {
        UserContentBlock::Text(text) => acp::ContentBlock::Text(acp::TextContent::new(text.text)),
        UserContentBlock::Image(image) => {
            acp::ContentBlock::Image(acp::ImageContent::new(image.data, image.mime_type))
        }
    }
}

fn assistant_blocks_to_updates(content: Vec<AssistantContentBlock>) -> Vec<acp::SessionUpdate> {
    let mut updates = Vec::new();
    for block in content {
        match block {
            AssistantContentBlock::Text(text) => updates.push(
                acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(
                    acp::ContentBlock::Text(acp::TextContent::new(text.text)),
                )),
            ),
            AssistantContentBlock::Thinking(thinking) => {
                updates.push(acp::SessionUpdate::AgentThoughtChunk(
                    acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(
                        thinking.thinking,
                    ))),
                ));
            }
            AssistantContentBlock::ToolCall(_) => {}
        }
    }
    updates
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

fn map_stop_reason(stop_reason: PromptStopReason) -> acp::StopReason {
    match stop_reason {
        PromptStopReason::EndTurn => acp::StopReason::EndTurn,
        PromptStopReason::MaxTokens => acp::StopReason::MaxTokens,
        PromptStopReason::MaxTurnRequests => acp::StopReason::MaxTurnRequests,
        PromptStopReason::Refusal => acp::StopReason::Refusal,
        PromptStopReason::Cancelled => acp::StopReason::Cancelled,
    }
}

fn internal_error(error: anyhow::Error) -> acp::Error {
    acp::Error::internal_error().data(error.to_string())
}
