//! Port of packages/coding-agent/src/core/agent-session-runtime.ts

use std::sync::{Arc, Mutex};

use pi_ai::{Message, UserMessageContent};
use pi_js::BoxFuture;

use crate::core::agent_session::AgentSession;
use crate::core::extensions::runner::emit_session_shutdown_event;
use crate::core::extensions::types::{
    CancelledResult, ForkPosition, ProjectTrustContext, ReplacedSessionContext, SessionShutdownEvent,
    SessionShutdownReason, SessionStartEvent, SessionStartReason,
};
use crate::core::sdk::CreateAgentSessionResult;
use crate::core::session_cwd::{SessionCwdSource, assert_session_cwd_exists};
use crate::core::session_manager::{NewSessionOptions as ManagerNewSessionOptions, SessionEntry, SessionManager};
use crate::utils::paths::{PathInputOptions, resolve_path};

pub use crate::core::agent_session_services::{
    AgentSessionRuntimeDiagnostic, AgentSessionRuntimeDiagnostic as RuntimeDiagnostic,
    AgentSessionRuntimeDiagnosticType, AgentSessionServices, CreateAgentSessionFromServicesOptions,
    CreateAgentSessionServicesOptions, create_agent_session_from_services, create_agent_session_services,
};

/// Result returned by runtime creation.
///
/// The caller gets the created session, its cwd-bound services, and all
/// diagnostics collected during setup.
#[derive(Clone)]
pub struct CreateAgentSessionRuntimeResult {
    pub session: AgentSession,
    pub extensions_result: crate::core::extensions::types::LoadExtensionsResult,
    pub model_fallback_message: Option<String>,
    pub services: AgentSessionServices,
    pub diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
}

impl CreateAgentSessionRuntimeResult {
    pub fn from_session_result(
        created: CreateAgentSessionResult,
        services: AgentSessionServices,
        diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
    ) -> Self {
        Self {
            session: created.session,
            extensions_result: created.extensions_result,
            model_fallback_message: created.model_fallback_message,
            services,
            diagnostics,
        }
    }
}

#[derive(Clone)]
pub struct CreateAgentSessionRuntimeFactoryOptions {
    pub cwd: String,
    pub agent_dir: String,
    pub session_manager: SessionManager,
    pub session_start_event: Option<SessionStartEvent>,
    pub project_trust_context: Option<ProjectTrustContext>,
}

/// Creates a full runtime for a target cwd and session manager.
pub type CreateAgentSessionRuntimeFactory = Arc<
    dyn Fn(CreateAgentSessionRuntimeFactoryOptions) -> BoxFuture<pi_js::Result<CreateAgentSessionRuntimeResult>>
        + Send
        + Sync,
>;

/// Thrown when `/import` references a JSONL file path that does not exist.
#[derive(Debug, thiserror::Error)]
#[error("File not found: {file_path}")]
pub struct SessionImportFileNotFoundError {
    pub file_path: String,
}

impl SessionImportFileNotFoundError {
    pub fn new(file_path: impl Into<String>) -> Self {
        Self {
            file_path: file_path.into(),
        }
    }
}

pub type RebindSessionFn = Arc<dyn Fn(AgentSession) -> BoxFuture<pi_js::Result<()>> + Send + Sync>;
pub type BeforeSessionInvalidateFn = Arc<dyn Fn() + Send + Sync>;
pub type ProjectTrustContextFactory = Arc<dyn Fn(&str) -> ProjectTrustContext + Send + Sync>;
pub type WithSessionFn = Box<dyn FnOnce(ReplacedSessionContext) -> BoxFuture<pi_js::Result<()>> + Send>;
pub type SetupSessionManagerFn = Box<dyn FnOnce(SessionManager) -> BoxFuture<pi_js::Result<()>> + Send>;

#[derive(Default)]
pub struct SwitchSessionOptions {
    pub cwd_override: Option<String>,
    pub with_session: Option<WithSessionFn>,
    pub project_trust_context_factory: Option<ProjectTrustContextFactory>,
}

#[derive(Default)]
pub struct RuntimeNewSessionOptions {
    pub parent_session: Option<String>,
    pub setup: Option<SetupSessionManagerFn>,
    pub with_session: Option<WithSessionFn>,
}

#[derive(Default)]
pub struct RuntimeForkOptions {
    pub position: Option<ForkPosition>,
    pub with_session: Option<WithSessionFn>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeForkResult {
    pub cancelled: bool,
    pub selected_text: Option<String>,
}

#[derive(Clone)]
pub struct CreateAgentSessionRuntimeOptions {
    pub cwd: String,
    pub agent_dir: String,
    pub session_manager: SessionManager,
    pub session_start_event: Option<SessionStartEvent>,
}

struct AgentSessionRuntimeInner {
    rebind_session: Option<RebindSessionFn>,
    before_session_invalidate: Option<BeforeSessionInvalidateFn>,
    session: AgentSession,
    services: AgentSessionServices,
    create_runtime: CreateAgentSessionRuntimeFactory,
    diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
    model_fallback_message: Option<String>,
}

/// Owns the current AgentSession plus its cwd-bound services.
///
/// Session replacement methods tear down the current runtime first, then create
/// and apply the next runtime. If creation fails, the error is propagated to the
/// caller. The caller is responsible for user-facing error handling.
#[derive(Clone)]
pub struct AgentSessionRuntime {
    inner: Arc<Mutex<AgentSessionRuntimeInner>>,
}

fn extract_user_message_text(content: &UserMessageContent) -> String {
    match content {
        UserMessageContent::Text(s) => s.clone(),
        UserMessageContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                pi_ai::UserContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
    }
}

impl AgentSessionRuntime {
    pub fn new(
        session: AgentSession,
        services: AgentSessionServices,
        create_runtime: CreateAgentSessionRuntimeFactory,
        diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
        model_fallback_message: Option<String>,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(AgentSessionRuntimeInner {
                rebind_session: None,
                before_session_invalidate: None,
                session,
                services,
                create_runtime,
                diagnostics,
                model_fallback_message,
            })),
        }
    }

    pub fn services(&self) -> AgentSessionServices {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).services.clone()
    }

    pub fn session(&self) -> AgentSession {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).session.clone()
    }

    pub fn cwd(&self) -> String {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .services
            .cwd
            .clone()
    }

    pub fn diagnostics(&self) -> Vec<AgentSessionRuntimeDiagnostic> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).diagnostics.clone()
    }

    pub fn model_fallback_message(&self) -> Option<String> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .model_fallback_message
            .clone()
    }

    pub fn set_rebind_session(&self, rebind_session: Option<RebindSessionFn>) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).rebind_session = rebind_session;
    }

    /// Set a synchronous callback that runs after `session_shutdown` handlers finish
    /// but before the current session is invalidated.
    pub fn set_before_session_invalidate(&self, before_session_invalidate: Option<BeforeSessionInvalidateFn>) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .before_session_invalidate = before_session_invalidate;
    }

    async fn emit_before_switch(
        &self,
        reason: crate::core::extensions::types::SessionBeforeSwitchReason,
        target_session_file: Option<String>,
    ) -> pi_js::Result<CancelledResult> {
        let session = self.session();
        let Some(runner) = session.extension_runner() else {
            return Ok(CancelledResult { cancelled: false });
        };
        if !runner.has_handlers("session_before_switch") {
            return Ok(CancelledResult { cancelled: false });
        }
        let result = runner
            .emit_session_before_switch(crate::core::extensions::types::SessionBeforeSwitchEvent {
                reason,
                target_session_file,
            })
            .await;
        Ok(CancelledResult {
            cancelled: result.is_some_and(|r| r.cancel == Some(true)),
        })
    }

    async fn emit_before_fork(&self, entry_id: &str, position: ForkPosition) -> pi_js::Result<CancelledResult> {
        let session = self.session();
        let Some(runner) = session.extension_runner() else {
            return Ok(CancelledResult { cancelled: false });
        };
        if !runner.has_handlers("session_before_fork") {
            return Ok(CancelledResult { cancelled: false });
        }
        let result = runner
            .emit_session_before_fork(crate::core::extensions::types::SessionBeforeForkEvent {
                entry_id: entry_id.to_string(),
                position,
            })
            .await;
        Ok(CancelledResult {
            cancelled: result.is_some_and(|r| r.cancel == Some(true)),
        })
    }

    async fn teardown_current(&self, reason: SessionShutdownReason, target_session_file: Option<String>) {
        let (session, before_invalidate) = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            (guard.session.clone(), guard.before_session_invalidate.clone())
        };
        session.abort().await;
        if let Some(ref runner) = session.extension_runner() {
            emit_session_shutdown_event(
                runner,
                SessionShutdownEvent {
                    reason,
                    target_session_file,
                },
            )
            .await;
        }
        if let Some(cb) = before_invalidate {
            cb();
        }
        session.dispose();
    }

    fn apply(&self, result: CreateAgentSessionRuntimeResult) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.session = result.session;
        guard.services = result.services;
        guard.diagnostics = result.diagnostics;
        guard.model_fallback_message = result.model_fallback_message;
    }

    async fn finish_session_replacement(&self, with_session: Option<WithSessionFn>) -> pi_js::Result<()> {
        let (rebind, session) = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            (guard.rebind_session.clone(), guard.session.clone())
        };
        if let Some(rebind) = rebind {
            rebind(session.clone()).await?;
        }
        if let Some(with_session) = with_session {
            with_session(session.create_replaced_session_context()?).await?;
        }
        Ok(())
    }

    pub async fn switch_session(
        &self,
        session_path: &str,
        options: Option<SwitchSessionOptions>,
    ) -> pi_js::Result<CancelledResult> {
        let options = options.unwrap_or_default();
        let before_result = self
            .emit_before_switch(
                crate::core::extensions::types::SessionBeforeSwitchReason::Resume,
                Some(session_path.to_string()),
            )
            .await?;
        if before_result.cancelled {
            return Ok(before_result);
        }

        let (previous_session_file, current_cwd, agent_dir, create_runtime) = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            (
                guard.session.session_file(),
                guard.services.cwd.clone(),
                guard.services.agent_dir.clone(),
                guard.create_runtime.clone(),
            )
        };
        let session_manager = SessionManager::open(session_path, None, options.cwd_override.as_deref())?;
        assert_session_cwd_exists(&session_manager as &dyn SessionCwdSource, &current_cwd)?;
        let target_session_file = session_manager.get_session_file();
        let target_cwd = session_manager.get_cwd();
        self.teardown_current(SessionShutdownReason::Resume, target_session_file)
            .await;

        let project_trust_context = options
            .project_trust_context_factory
            .map(|factory| factory(&target_cwd));
        let created = create_runtime(CreateAgentSessionRuntimeFactoryOptions {
            cwd: target_cwd,
            agent_dir,
            session_manager,
            session_start_event: Some(SessionStartEvent {
                reason: SessionStartReason::Resume,
                previous_session_file,
            }),
            project_trust_context,
        })
        .await?;
        self.apply(created);
        self.finish_session_replacement(options.with_session).await?;
        Ok(CancelledResult { cancelled: false })
    }

    pub async fn new_session(&self, options: Option<RuntimeNewSessionOptions>) -> pi_js::Result<CancelledResult> {
        let options = options.unwrap_or_default();
        let before_result = self
            .emit_before_switch(crate::core::extensions::types::SessionBeforeSwitchReason::New, None)
            .await?;
        if before_result.cancelled {
            return Ok(before_result);
        }

        let (current_session, current_cwd, agent_dir, create_runtime) = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            (
                guard.session.clone(),
                guard.services.cwd.clone(),
                guard.services.agent_dir.clone(),
                guard.create_runtime.clone(),
            )
        };
        let previous_session_file = current_session.session_file();
        let session_dir = current_session.session_manager().get_session_dir();
        let session_manager = if current_session.session_manager().is_persisted() {
            SessionManager::create(&current_cwd, Some(&session_dir), None)?
        } else {
            SessionManager::in_memory(Some(&current_cwd), None, None)?
        };
        if let Some(parent_session) = options.parent_session {
            session_manager.new_session(Some(ManagerNewSessionOptions {
                parent_session: Some(parent_session),
                ..Default::default()
            }))?;
        }

        let target_session_file = session_manager.get_session_file();
        self.teardown_current(SessionShutdownReason::New, target_session_file)
            .await;
        let created = create_runtime(CreateAgentSessionRuntimeFactoryOptions {
            cwd: current_cwd,
            agent_dir,
            session_manager,
            session_start_event: Some(SessionStartEvent {
                reason: SessionStartReason::New,
                previous_session_file,
            }),
            project_trust_context: None,
        })
        .await?;
        self.apply(created);
        if let Some(setup) = options.setup {
            let sess = self.session();
            setup(sess.session_manager().clone()).await?;
            sess.refresh_context();
        }
        self.finish_session_replacement(options.with_session).await?;
        Ok(CancelledResult { cancelled: false })
    }

    pub async fn fork(&self, entry_id: &str, options: Option<RuntimeForkOptions>) -> pi_js::Result<RuntimeForkResult> {
        let options = options.unwrap_or_default();
        let position = options.position.unwrap_or(ForkPosition::Before);
        let before_result = self.emit_before_fork(entry_id, position).await?;
        if before_result.cancelled {
            return Ok(RuntimeForkResult {
                cancelled: true,
                selected_text: None,
            });
        }

        let (current_session, current_cwd, agent_dir, create_runtime) = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            (
                guard.session.clone(),
                guard.services.cwd.clone(),
                guard.services.agent_dir.clone(),
                guard.create_runtime.clone(),
            )
        };

        let selected_entry = current_session
            .session_manager()
            .get_entry(entry_id)
            .ok_or_else(|| pi_js::Error::msg("Invalid entry ID for forking"))?;

        let (target_leaf_id, selected_text) = match position {
            ForkPosition::At => (Some(selected_entry.id().to_string()), None),
            ForkPosition::Before => {
                let SessionEntry::Message(msg_entry) = &selected_entry else {
                    return Err(pi_js::Error::msg("Invalid entry ID for forking"));
                };
                let Message::User(user_msg) = msg_entry
                    .message
                    .as_message()
                    .ok_or_else(|| pi_js::Error::msg("Invalid entry ID for forking"))?
                else {
                    return Err(pi_js::Error::msg("Invalid entry ID for forking"));
                };
                (
                    msg_entry.parent_id.clone(),
                    Some(extract_user_message_text(&user_msg.content)),
                )
            }
        };

        let previous_session_file = current_session.session_file();
        if current_session.session_manager().is_persisted() {
            let current_session_file = previous_session_file
                .clone()
                .ok_or_else(|| pi_js::Error::msg("Persisted session is missing a session file"))?;
            let session_dir = current_session.session_manager().get_session_dir();

            if target_leaf_id.is_none() {
                let session_manager = SessionManager::create(&current_cwd, Some(&session_dir), None)?;
                session_manager.new_session(Some(ManagerNewSessionOptions {
                    parent_session: Some(current_session_file),
                    ..Default::default()
                }))?;
                let next_file = session_manager.get_session_file();
                self.teardown_current(SessionShutdownReason::Fork, next_file).await;
                let created = create_runtime(CreateAgentSessionRuntimeFactoryOptions {
                    cwd: current_cwd,
                    agent_dir,
                    session_manager,
                    session_start_event: Some(SessionStartEvent {
                        reason: SessionStartReason::Fork,
                        previous_session_file,
                    }),
                    project_trust_context: None,
                })
                .await?;
                self.apply(created);
                self.finish_session_replacement(options.with_session).await?;
                return Ok(RuntimeForkResult {
                    cancelled: false,
                    selected_text,
                });
            }

            if !pi_js::fs::exists(&current_session_file) {
                return Err(pi_js::Error::msg(
                    "This session has not been saved yet. Send a message before cloning or forking it.",
                ));
            }
            let session_manager = SessionManager::open(&current_session_file, Some(&session_dir), None)?;
            let leaf_id = target_leaf_id.as_deref().unwrap_or("");
            let forked_path = session_manager
                .create_branched_session(leaf_id)?
                .ok_or_else(|| pi_js::Error::msg("Failed to create forked session"))?;
            let _ = forked_path;
            let next_file = session_manager.get_session_file();
            let target_cwd = session_manager.get_cwd();
            self.teardown_current(SessionShutdownReason::Fork, next_file).await;
            let created = create_runtime(CreateAgentSessionRuntimeFactoryOptions {
                cwd: target_cwd,
                agent_dir,
                session_manager,
                session_start_event: Some(SessionStartEvent {
                    reason: SessionStartReason::Fork,
                    previous_session_file,
                }),
                project_trust_context: None,
            })
            .await?;
            self.apply(created);
            self.finish_session_replacement(options.with_session).await?;
            return Ok(RuntimeForkResult {
                cancelled: false,
                selected_text,
            });
        }

        let session_manager = current_session.session_manager().clone();
        let next_file = session_manager.get_session_file();
        self.teardown_current(SessionShutdownReason::Fork, next_file).await;
        match target_leaf_id {
            None => {
                session_manager.new_session(Some(ManagerNewSessionOptions {
                    parent_session: previous_session_file.clone(),
                    ..Default::default()
                }))?;
            }
            Some(ref leaf_id) => {
                session_manager.create_branched_session(leaf_id)?;
            }
        }
        let created = create_runtime(CreateAgentSessionRuntimeFactoryOptions {
            cwd: current_cwd,
            agent_dir,
            session_manager,
            session_start_event: Some(SessionStartEvent {
                reason: SessionStartReason::Fork,
                previous_session_file,
            }),
            project_trust_context: None,
        })
        .await?;
        self.apply(created);
        self.finish_session_replacement(options.with_session).await?;
        Ok(RuntimeForkResult {
            cancelled: false,
            selected_text,
        })
    }

    /// Import a session JSONL file and switch runtime state to the imported session.
    pub async fn import_from_jsonl(
        &self,
        input_path: &str,
        cwd_override: Option<&str>,
    ) -> pi_js::Result<CancelledResult> {
        let resolved_path = resolve_path(input_path, None, PathInputOptions::default());
        if !pi_js::fs::exists(&resolved_path) {
            return Err(pi_js::Error::typed(
                "SessionImportFileNotFoundError",
                SessionImportFileNotFoundError::new(resolved_path),
            ));
        }

        let (current_session, current_cwd, agent_dir, create_runtime) = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            (
                guard.session.clone(),
                guard.services.cwd.clone(),
                guard.services.agent_dir.clone(),
                guard.create_runtime.clone(),
            )
        };
        let session_dir = current_session.session_manager().get_session_dir();
        if !pi_js::fs::exists(&session_dir) {
            pi_js::fs::mkdir(&session_dir, true, None)?;
        }

        let base_name = pi_js::path::basename(&resolved_path, None);
        let mut destination_path = pi_js::path::join(&[&session_dir, &base_name]);
        let source_already_stored = pi_js::path::resolve(&[&destination_path]) == resolved_path;
        if !source_already_stored {
            let ext = pi_js::path::extname(&destination_path);
            let stem = pi_js::path::basename(&destination_path, Some(&ext));
            let mut suffix = 1usize;
            while pi_js::fs::exists(&destination_path) {
                destination_path = pi_js::path::join(&[&session_dir, &format!("{stem}-{suffix}{ext}")]);
                suffix += 1;
            }
        }

        let before_result = self
            .emit_before_switch(
                crate::core::extensions::types::SessionBeforeSwitchReason::Resume,
                Some(destination_path.clone()),
            )
            .await?;
        if before_result.cancelled {
            return Ok(before_result);
        }

        let previous_session_file = current_session.session_file();
        if !source_already_stored {
            pi_js::fs::copy_file(&resolved_path, &destination_path)?;
        }

        let session_manager = SessionManager::open(&destination_path, Some(&session_dir), cwd_override)?;
        assert_session_cwd_exists(&session_manager as &dyn SessionCwdSource, &current_cwd)?;
        let next_file = session_manager.get_session_file();
        let target_cwd = session_manager.get_cwd();
        self.teardown_current(SessionShutdownReason::Resume, next_file).await;
        let created = create_runtime(CreateAgentSessionRuntimeFactoryOptions {
            cwd: target_cwd,
            agent_dir,
            session_manager,
            session_start_event: Some(SessionStartEvent {
                reason: SessionStartReason::Resume,
                previous_session_file,
            }),
            project_trust_context: None,
        })
        .await?;
        self.apply(created);
        self.finish_session_replacement(None).await?;
        Ok(CancelledResult { cancelled: false })
    }

    pub async fn dispose(&self) {
        let (session, before_invalidate) = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            (guard.session.clone(), guard.before_session_invalidate.clone())
        };
        if let Some(ref runner) = session.extension_runner() {
            emit_session_shutdown_event(
                runner,
                SessionShutdownEvent {
                    reason: SessionShutdownReason::Quit,
                    target_session_file: None,
                },
            )
            .await;
        }
        if let Some(cb) = before_invalidate {
            cb();
        }
        session.dispose();
    }
}

/// Create the initial runtime from a runtime factory and initial session target.
pub async fn create_agent_session_runtime(
    create_runtime: CreateAgentSessionRuntimeFactory,
    options: CreateAgentSessionRuntimeOptions,
) -> pi_js::Result<AgentSessionRuntime> {
    assert_session_cwd_exists(&options.session_manager as &dyn SessionCwdSource, &options.cwd)?;
    let result = create_runtime(CreateAgentSessionRuntimeFactoryOptions {
        cwd: options.cwd,
        agent_dir: options.agent_dir,
        session_manager: options.session_manager,
        session_start_event: options.session_start_event,
        project_trust_context: None,
    })
    .await?;
    Ok(AgentSessionRuntime::new(
        result.session,
        result.services,
        create_runtime,
        result.diagnostics,
        result.model_fallback_message,
    ))
}
