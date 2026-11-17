//! Background Tokio runtime bridge connecting `pi-gui` to `pi-coding-agent` off the UI thread.

mod ui;
mod workspace;

use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};

use indexmap::IndexMap;
use pi_ai::AuthType;
use pi_ai::types::{AssistantMessageEvent, ModelThinkingLevel};
use pi_coding_agent::config::get_agent_dir;
use pi_coding_agent::core::agent_session::{AgentSession, AgentSessionEvent, ExtensionBindings};
use pi_coding_agent::core::agent_session_runtime::{
    AgentSessionRuntime, CreateAgentSessionRuntimeFactory, CreateAgentSessionRuntimeOptions,
    CreateAgentSessionRuntimeResult, RuntimeNewSessionOptions, SwitchSessionOptions, create_agent_session_runtime,
};
use pi_coding_agent::core::agent_session_services::{
    CreateAgentSessionFromServicesOptions, CreateAgentSessionServicesOptions, create_agent_session_from_services,
    create_agent_session_services,
};
use pi_coding_agent::core::extensions::types::ExtensionMode;
use pi_coding_agent::core::model_resolver::parse_valid_thinking_level;
use pi_coding_agent::core::resource_loader::ResourceLoader;
use pi_coding_agent::core::sdk::ensure_default_stream_fn;
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::source_info::{BUILTIN_PATH_PREFIX, SourceScope};
use pi_js::Unsubscribe;
use rune::Waker;
use tokio::sync::mpsc as tokio_mpsc;

pub use workspace::parse_git_diff;

use ui::{DialogBroker, GuiAuthInteraction, GuiExtensionUiContext};
use workspace::{collect_git_diff, read_workspace_file, scan_workspace_files, spawn_shell_command};

use crate::models::{
    build_auth_snapshot, build_models_snapshot, convert_agent_messages, session_info_to_thread_summary,
};
use crate::state::{
    AuthProviderViewModel, ContextUsageView, DiffSummary, ExtensionDialogView, FileTreeEntryView, LaunchConfig,
    MessageViewModel, ModelUsageSummary, ModelViewModel, OpenFileView, PluginKind, PluginOrigin, PluginViewModel,
    TerminalOutputLine, ThreadSummary, ToastTone,
};

/// Commands sent from the Rune UI thread to the background `pi-coding-agent` worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BridgeCommand {
    Initialize,
    SwitchSession { id: String },
    NewSession,
    Prompt { text: String },
    Abort,
    SelectModel { provider: String, model_id: String },
    SetThinkingLevel { level: String },
    CompactSession,
    ReloadPlugins,
    RefreshWorkspace,
    ReadFile { path: String },
    RunShellCommand { command: String },
    LoginProvider { provider: String },
    LogoutProvider { provider: String },
    ExtensionDialogResponse { request_id: String, value: Option<String> },
    Shutdown,
}

/// Events published from the background `pi-coding-agent` worker to the Rune UI thread.
#[derive(Clone, Debug, PartialEq)]
pub enum BridgeEvent {
    SessionLoaded {
        session_id: String,
        messages: Vec<MessageViewModel>,
        models: Vec<ModelViewModel>,
        active_model: Option<ModelViewModel>,
        plugins: Vec<PluginViewModel>,
        auth_providers: Vec<AuthProviderViewModel>,
        context_usage: ContextUsageView,
    },
    ThreadsUpdated(Vec<ThreadSummary>),
    StreamStarted {
        turn_id: String,
        started_ms: i64,
    },
    StreamTextDelta(String),
    StreamThinkingDelta(String),
    MessageAppended(MessageViewModel),
    StreamFinished,
    DiffUpdated(DiffSummary),
    FilesUpdated(Vec<FileTreeEntryView>),
    PluginsUpdated(Vec<PluginViewModel>),
    ModelsUpdated {
        models: Vec<ModelViewModel>,
        active_model: Option<ModelViewModel>,
    },
    ContextUpdated(ContextUsageView),
    MessagesReplaced(Vec<MessageViewModel>),
    FileOpened(OpenFileView),
    TerminalOutput(TerminalOutputLine),
    TerminalFinished {
        exit_code: Option<i32>,
    },
    ExtensionDialogRequested(ExtensionDialogView),
    Toast {
        message: String,
        tone: ToastTone,
    },
    Error(String),
}

/// Non-blocking handle held by [`crate::state::PiGuiState`] for posting commands to the background worker.
#[derive(Clone)]
pub struct BridgeSender {
    tx: tokio_mpsc::UnboundedSender<BridgeCommand>,
    rx: Arc<Mutex<mpsc::Receiver<BridgeEvent>>>,
}

impl BridgeSender {
    pub fn send(&self, cmd: BridgeCommand) {
        let _ = self.tx.send(cmd);
    }

    /// Drains all queued events without blocking the Rune frame loop.
    #[must_use]
    pub fn drain_events(&self) -> Vec<BridgeEvent> {
        let Ok(guard) = self.rx.lock() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Ok(ev) = guard.try_recv() {
            out.push(ev);
        }
        out
    }
}

#[derive(Clone)]
pub(crate) struct BridgeEmitter {
    tx: mpsc::Sender<BridgeEvent>,
    waker: Waker,
}

impl BridgeEmitter {
    pub(crate) fn emit(&self, ev: BridgeEvent) {
        let _ = self.tx.send(ev);
        (self.waker)();
    }

    pub(crate) fn toast(&self, message: impl Into<String>, tone: ToastTone) {
        self.emit(BridgeEvent::Toast {
            message: message.into(),
            tone,
        });
    }

    pub(crate) fn error(&self, message: impl Into<String>) {
        self.emit(BridgeEvent::Error(message.into()));
    }
}

struct BridgeState {
    runtime: AgentSessionRuntime,
    emitter: BridgeEmitter,
    dialogs: DialogBroker,
    ui_context: Arc<GuiExtensionUiContext>,
    cmd_tx: tokio_mpsc::UnboundedSender<BridgeCommand>,
    session_dir: Option<String>,
    sub: Mutex<Option<Unsubscribe>>,
}

impl BridgeState {
    async fn attach_session(&self) {
        if let Some(prev) = self.sub.lock().ok().and_then(|mut g| g.take()) {
            prev.call();
        }
        let session = self.runtime.session();
        let (emitter, cmd_tx) = (self.emitter.clone(), self.cmd_tx.clone());
        let handle = session.subscribe(move |ev| match ev {
            AgentSessionEvent::AgentStart => {
                let now = pi_js::time::now_ms() as i64;
                emitter.emit(BridgeEvent::StreamStarted {
                    turn_id: format!("turn-{now}"),
                    started_ms: now,
                });
            }
            AgentSessionEvent::MessageUpdate {
                assistant_message_event,
                ..
            } => match assistant_message_event {
                AssistantMessageEvent::TextDelta { delta, .. } => {
                    emitter.emit(BridgeEvent::StreamTextDelta(delta));
                }
                AssistantMessageEvent::ThinkingDelta { delta, .. } => {
                    emitter.emit(BridgeEvent::StreamThinkingDelta(delta));
                }
                _ => {}
            },
            AgentSessionEvent::MessageEnd { message } => {
                for vm in convert_agent_messages(&[message]) {
                    emitter.emit(BridgeEvent::MessageAppended(vm));
                }
            }
            AgentSessionEvent::ThinkingLevelChanged { .. } => {
                let _ = cmd_tx.send(BridgeCommand::RefreshWorkspace);
            }
            AgentSessionEvent::CompactionEnd { .. } => {
                emitter.toast("Session compacted", ToastTone::Info);
                let _ = cmd_tx.send(BridgeCommand::RefreshWorkspace);
            }
            AgentSessionEvent::AgentEnd { .. } => {
                emitter.emit(BridgeEvent::StreamFinished);
                let _ = cmd_tx.send(BridgeCommand::RefreshWorkspace);
            }
            _ => {}
        });
        if let Ok(mut guard) = self.sub.lock() {
            *guard = Some(handle);
        }
        let bindings = ExtensionBindings {
            ui_context: Some(self.ui_context.clone()),
            mode: Some(ExtensionMode::Tui),
            ..Default::default()
        };
        if let Err(err) = session.bind_extensions(bindings).await {
            self.emitter.error(format!("Failed to bind extensions: {err}"));
        }
    }

    async fn emit_session_loaded(&self) {
        let session = self.runtime.session();
        let (models, active_model) = build_models_snapshot(session.model_runtime(), session.model().as_ref());
        self.emitter.emit(BridgeEvent::SessionLoaded {
            session_id: session.session_id(),
            messages: convert_agent_messages(&session.messages()),
            models,
            active_model,
            plugins: build_plugins_snapshot(session.resource_loader().as_ref()),
            auth_providers: build_auth_snapshot(session.model_runtime()),
            context_usage: build_context_snapshot(&session),
        });
        self.refresh_workspace().await;
    }

    async fn refresh_workspace(&self) {
        let (cwd, session) = (self.runtime.cwd(), self.runtime.session());
        self.emitter
            .emit(BridgeEvent::ContextUpdated(build_context_snapshot(&session)));
        if let Ok(sessions) = SessionManager::list(&cwd, self.session_dir.as_deref(), None, None).await {
            let now = pi_js::time::now_ms() as i64;
            let threads = sessions
                .iter()
                .map(|i| session_info_to_thread_summary(i, false, now))
                .collect();
            self.emitter.emit(BridgeEvent::ThreadsUpdated(threads));
        }
        let (diff, modified) = collect_git_diff(&cwd).await;
        self.emitter.emit(BridgeEvent::DiffUpdated(diff));
        self.emitter
            .emit(BridgeEvent::FilesUpdated(scan_workspace_files(&cwd, &modified)));
    }
}

fn build_context_snapshot(session: &AgentSession) -> ContextUsageView {
    let stats = session.get_session_stats();
    let usage = session.get_context_usage();
    let used_tokens = usage.as_ref().and_then(|u| u.tokens).unwrap_or(stats.tokens.total);
    let window_tokens = usage
        .as_ref()
        .map(|u| u.context_window)
        .or_else(|| session.model().map(|m| m.context_window))
        .unwrap_or(0);
    let percent = usage.as_ref().and_then(|u| u.percent).unwrap_or_else(|| {
        if window_tokens > 0 {
            (used_tokens as f64 / window_tokens as f64) * 100.0
        } else {
            0.0
        }
    });
    let lvl = match session.thinking_level() {
        ModelThinkingLevel::Off => "off",
        ModelThinkingLevel::Minimal => "minimal",
        ModelThinkingLevel::Low => "low",
        ModelThinkingLevel::Medium => "medium",
        ModelThinkingLevel::High => "high",
        ModelThinkingLevel::XHigh | ModelThinkingLevel::Max => "xhigh",
    };
    ContextUsageView {
        used_tokens,
        window_tokens,
        percent,
        thinking_level: lvl.into(),
        session_usage: ModelUsageSummary {
            input_tokens: stats.tokens.input,
            output_tokens: stats.tokens.output,
            cache_read_tokens: stats.tokens.cache_read,
            total_cost: stats.cost,
        },
    }
}

fn build_plugins_snapshot(loader: &dyn ResourceLoader) -> Vec<PluginViewModel> {
    let ext_res = loader.get_core_extensions();
    let errs: IndexMap<String, String> = ext_res
        .errors
        .iter()
        .map(|e| (e.path.clone(), e.error.clone()))
        .collect();
    let scope = |s| match s {
        SourceScope::User => PluginOrigin::Global,
        SourceScope::Project => PluginOrigin::Project,
        SourceScope::Temporary => PluginOrigin::Inline,
    };
    let mut plugins: Vec<PluginViewModel> = ext_res
        .extensions
        .iter()
        .map(|ext| {
            let builtin = ext.path.starts_with(BUILTIN_PATH_PREFIX) || ext.source_info.source == "builtin";
            let name = Path::new(&ext.path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&ext.path)
                .to_string();
            PluginViewModel {
                id: format!("ext:{}", ext.path),
                name,
                description: format!("{} ({})", ext.source_info.source, ext.path),
                kind: if builtin {
                    PluginKind::Builtin
                } else {
                    PluginKind::QuickJs
                },
                origin: if builtin {
                    PluginOrigin::Builtin
                } else {
                    scope(ext.source_info.scope)
                },
                enabled: true,
                tool_count: ext.tools.lock().map(|g| g.len()).unwrap_or(0),
                command_count: ext.commands.lock().map(|g| g.len()).unwrap_or(0),
                error: errs.get(&ext.path).cloned(),
            }
        })
        .collect();
    for mcp in ext_res.runtime.inner.mcp_servers.list() {
        plugins.push(PluginViewModel {
            id: format!("mcp:{}", mcp.name),
            name: mcp.name,
            description: format!("MCP server ({})", mcp.extension_path),
            kind: PluginKind::Mcp,
            origin: PluginOrigin::Global,
            enabled: true,
            tool_count: 0,
            command_count: 0,
            error: None,
        });
    }
    for sk in loader.get_skills().0 {
        plugins.push(PluginViewModel {
            id: format!("skill:{}", sk.name),
            name: sk.name,
            description: sk.description,
            kind: PluginKind::Skill,
            origin: scope(sk.source_info.scope),
            enabled: !sk.disable_model_invocation,
            tool_count: 0,
            command_count: 0,
            error: None,
        });
    }
    plugins
}

async fn create_bridge_runtime(config: &LaunchConfig) -> pi_js::Result<AgentSessionRuntime> {
    ensure_default_stream_fn();
    let cwd = if config.cwd.trim().is_empty() {
        pi_js::env::cwd()
    } else {
        config.cwd.clone()
    };
    let (agent_dir, sdir) = (get_agent_dir(), config.session_dir.as_deref());
    let session_manager = match config.session.as_deref() {
        Some(t) if Path::new(t).exists() => SessionManager::open(t, sdir, Some(&cwd))?,
        Some(t) => match SessionManager::find_by_id(&cwd, t, sdir) {
            Some(found) => SessionManager::open(&found, sdir, Some(&cwd))?,
            None => SessionManager::continue_recent(&cwd, sdir)?,
        },
        None => SessionManager::continue_recent(&cwd, sdir)?,
    };
    let (prov, mdl, think) = (config.provider.clone(), config.model.clone(), config.thinking.clone());
    let factory: CreateAgentSessionRuntimeFactory = Arc::new(move |opts| {
        let (prov, mdl, think) = (prov.clone(), mdl.clone(), think.clone());
        Box::pin(async move {
            let services = create_agent_session_services(CreateAgentSessionServicesOptions {
                cwd: opts.cwd,
                agent_dir: Some(opts.agent_dir),
                ..Default::default()
            })
            .await?;
            let model = match (prov.as_deref(), mdl.as_deref()) {
                (Some(p), Some(id)) => services.model_runtime.get_model(p, id),
                (None, Some(id)) => services
                    .model_runtime
                    .get_models(None)
                    .into_iter()
                    .find(|m| m.id == id || m.name == id),
                _ => None,
            };
            let created = create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
                services: services.clone(),
                session_manager: opts.session_manager,
                session_start_event: opts.session_start_event,
                model,
                thinking_level: think.as_deref().and_then(parse_valid_thinking_level),
                scoped_models: None,
                tools: None,
                exclude_tools: None,
                no_tools: None,
                custom_tools: None,
            })
            .await?;
            Ok(CreateAgentSessionRuntimeResult::from_session_result(
                created,
                services,
                Vec::new(),
            ))
        })
    });
    create_agent_session_runtime(
        factory,
        CreateAgentSessionRuntimeOptions {
            cwd,
            agent_dir,
            session_manager,
            session_start_event: None,
        },
    )
    .await
}

async fn handle_command(state: &Arc<BridgeState>, cmd: BridgeCommand) -> bool {
    match cmd {
        BridgeCommand::Shutdown => return false,
        BridgeCommand::Initialize => state.emit_session_loaded().await,
        BridgeCommand::RefreshWorkspace => state.refresh_workspace().await,
        BridgeCommand::SwitchSession { id } => {
            let target =
                SessionManager::find_by_id(&state.runtime.cwd(), &id, state.session_dir.as_deref()).unwrap_or(id);
            match state
                .runtime
                .switch_session(&target, Some(SwitchSessionOptions::default()))
                .await
            {
                Ok(_) => {
                    state.attach_session().await;
                    state.emit_session_loaded().await;
                }
                Err(err) => state.emitter.error(format!("Switch session failed: {err}")),
            }
        }
        BridgeCommand::NewSession => match state
            .runtime
            .new_session(Some(RuntimeNewSessionOptions::default()))
            .await
        {
            Ok(_) => {
                state.attach_session().await;
                state.emit_session_loaded().await;
            }
            Err(err) => state.emitter.error(format!("New session failed: {err}")),
        },
        BridgeCommand::Prompt { text } => {
            let st = Arc::clone(state);
            tokio::task::spawn_local(async move {
                if let Err(err) = st.runtime.session().prompt(&text, None).await {
                    st.emitter.emit(BridgeEvent::StreamFinished);
                    st.emitter.error(format!("Prompt error: {err}"));
                }
            });
        }
        BridgeCommand::Abort => {
            state.runtime.session().abort().await;
            state.emitter.emit(BridgeEvent::StreamFinished);
            state.emitter.toast("Run aborted", ToastTone::Warning);
        }
        BridgeCommand::SelectModel { provider, model_id } => {
            let session = state.runtime.session();
            if let Some(model) = session.model_runtime().get_model(&provider, &model_id) {
                match session.set_model(model, None).await {
                    Ok(()) => {
                        let (models, active_model) =
                            build_models_snapshot(session.model_runtime(), session.model().as_ref());
                        state.emitter.emit(BridgeEvent::ModelsUpdated { models, active_model });
                        state
                            .emitter
                            .emit(BridgeEvent::ContextUpdated(build_context_snapshot(&session)));
                    }
                    Err(err) => state.emitter.error(format!("Model switch failed: {err}")),
                }
            }
        }
        BridgeCommand::SetThinkingLevel { level } => {
            if let Some(parsed) = parse_valid_thinking_level(&level) {
                let session = state.runtime.session();
                session.set_thinking_level(parsed, None);
                state
                    .emitter
                    .emit(BridgeEvent::ContextUpdated(build_context_snapshot(&session)));
            }
        }
        BridgeCommand::CompactSession => {
            let st = Arc::clone(state);
            tokio::task::spawn_local(async move {
                match st.runtime.session().compact(None).await {
                    Ok(_) => {
                        let s = st.runtime.session();
                        st.emitter
                            .emit(BridgeEvent::MessagesReplaced(convert_agent_messages(&s.messages())));
                        st.emitter.emit(BridgeEvent::ContextUpdated(build_context_snapshot(&s)));
                    }
                    Err(err) => st.emitter.error(format!("Compaction failed: {err}")),
                }
            });
        }
        BridgeCommand::ReloadPlugins => {
            let st = Arc::clone(state);
            tokio::task::spawn_local(async move {
                match st.runtime.session().reload(None).await {
                    Ok(()) => {
                        let s = st.runtime.session();
                        let (models, active_model) = build_models_snapshot(s.model_runtime(), s.model().as_ref());
                        st.emitter.emit(BridgeEvent::PluginsUpdated(build_plugins_snapshot(
                            s.resource_loader().as_ref(),
                        )));
                        st.emitter.emit(BridgeEvent::ModelsUpdated { models, active_model });
                        st.emitter.toast("Reloaded extensions & resources", ToastTone::Success);
                    }
                    Err(err) => st.emitter.error(format!("Reload failed: {err}")),
                }
            });
        }
        BridgeCommand::ReadFile { path } => match read_workspace_file(&state.runtime.cwd(), &path) {
            Ok(view) => state.emitter.emit(BridgeEvent::FileOpened(view)),
            Err(err) => state.emitter.toast(err, ToastTone::Warning),
        },
        BridgeCommand::RunShellCommand { command } => {
            spawn_shell_command(state.runtime.cwd(), command, state.emitter.clone());
        }
        BridgeCommand::LoginProvider { provider } => {
            let st = Arc::clone(state);
            tokio::task::spawn_local(async move {
                let interaction = Arc::new(GuiAuthInteraction {
                    provider: provider.clone(),
                    dialogs: st.dialogs.clone(),
                });
                match st
                    .runtime
                    .session()
                    .model_runtime()
                    .login(&provider, AuthType::OAuth, interaction, None)
                    .await
                {
                    Ok(_) => {
                        st.emitter.toast(format!("Logged in to {provider}"), ToastTone::Success);
                        st.emit_session_loaded().await;
                    }
                    Err(err) => st.emitter.error(format!("Login failed for {provider}: {err}")),
                }
            });
        }
        BridgeCommand::LogoutProvider { provider } => {
            match state.runtime.session().model_runtime().logout(&provider, None).await {
                Ok(()) => {
                    state
                        .emitter
                        .toast(format!("Logged out of {provider}"), ToastTone::Info);
                    state.emit_session_loaded().await;
                }
                Err(err) => state.emitter.error(format!("Logout failed for {provider}: {err}")),
            }
        }
        BridgeCommand::ExtensionDialogResponse { request_id, value } => {
            state.dialogs.resolve(&request_id, value);
        }
    }
    true
}

/// Owns the background Tokio current-thread runtime driving `pi-coding-agent` sessions.
pub struct GuiBridge {
    sender: BridgeSender,
}

impl GuiBridge {
    /// Spawns a dedicated background worker thread with a current-thread Tokio runtime.
    pub fn spawn(config: LaunchConfig, waker: Waker) -> Self {
        let (cmd_tx, mut cmd_rx) = tokio_mpsc::unbounded_channel::<BridgeCommand>();
        let (ev_tx, ev_rx) = mpsc::channel::<BridgeEvent>();
        let worker_cmd_tx = cmd_tx.clone();
        let _ = std::thread::Builder::new().name("pi-gui-bridge".into()).spawn(move || {
            pi_js::ensure_crypto_provider();
            let emitter = BridgeEmitter { tx: ev_tx, waker };
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
                emitter.error("Failed to initialize pi-gui background Tokio runtime");
                return;
            };
            let local = tokio::task::LocalSet::new();
            rt.block_on(local.run_until(async move {
                let runtime = match create_bridge_runtime(&config).await {
                    Ok(r) => r,
                    Err(err) => {
                        emitter.error(format!("Failed to start agent session: {err}"));
                        return;
                    }
                };
                let dialogs = DialogBroker::new(emitter.clone());
                let ui_context = Arc::new(GuiExtensionUiContext::new(dialogs.clone()));
                let state = Arc::new(BridgeState {
                    runtime,
                    emitter,
                    dialogs,
                    ui_context,
                    cmd_tx: worker_cmd_tx,
                    session_dir: config.session_dir.clone(),
                    sub: Mutex::new(None),
                });
                state.attach_session().await;
                state.emit_session_loaded().await;
                while let Some(cmd) = cmd_rx.recv().await {
                    if !handle_command(&state, cmd).await {
                        break;
                    }
                }
                state.runtime.dispose().await;
            }));
        });
        Self {
            sender: BridgeSender {
                tx: cmd_tx,
                rx: Arc::new(Mutex::new(ev_rx)),
            },
        }
    }

    #[must_use]
    pub fn sender(&self) -> BridgeSender {
        self.sender.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn drain_until<F>(sender: &BridgeSender, wake_rx: &mpsc::Receiver<()>, mut pred: F) -> Vec<BridgeEvent>
    where
        F: FnMut(&[BridgeEvent]) -> bool,
    {
        let mut collected = Vec::new();
        for _ in 0..40 {
            collected.extend(sender.drain_events());
            if pred(&collected) {
                return collected;
            }
            if wake_rx.recv_timeout(Duration::from_millis(500)).is_err() {
                collected.extend(sender.drain_events());
                if pred(&collected) {
                    return collected;
                }
            }
        }
        collected
    }

    #[test]
    fn bridge_loads_quickjs_plugin_git_diff_file_and_shell() {
        let root = std::env::temp_dir().join(format!("pi-gui-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ext_dir = root.join(".pi/extensions");
        let sess_dir = root.join(".sessions");
        std::fs::create_dir_all(&ext_dir).unwrap();
        std::fs::create_dir_all(&sess_dir).unwrap();
        std::fs::write(
            ext_dir.join("hello.ts"),
            "export default function (pi) { pi.registerCommand('hi', { description: 'hi', handler: async () => {} }); }\n",
        )
        .unwrap();
        std::fs::write(root.join("README.md"), "before\n").unwrap();
        let _ = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&root)
            .status();
        let _ = std::process::Command::new("git")
            .args(["add", "README.md"])
            .current_dir(&root)
            .status();
        let _ = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Pi",
                "-c",
                "user.email=pi@example.com",
                "commit",
                "-q",
                "-m",
                "init",
            ])
            .current_dir(&root)
            .status();
        std::fs::write(root.join("README.md"), "after\n").unwrap();

        let (wake_tx, wake_rx) = mpsc::channel::<()>();
        let waker: Waker = Arc::new(move || {
            let _ = wake_tx.send(());
        });
        let cfg = LaunchConfig {
            cwd: root.to_string_lossy().into_owned(),
            session_dir: Some(sess_dir.to_string_lossy().into_owned()),
            ..LaunchConfig::default()
        };
        let bridge = GuiBridge::spawn(cfg, waker);
        let sender = bridge.sender();

        let init_events = drain_until(&sender, &wake_rx, |evs| {
            evs.iter().any(|e| matches!(e, BridgeEvent::SessionLoaded { .. }))
                && evs.iter().any(|e| matches!(e, BridgeEvent::DiffUpdated(_)))
        });
        let plugins = init_events
            .iter()
            .find_map(|e| match e {
                BridgeEvent::SessionLoaded { plugins, .. } => Some(plugins.clone()),
                _ => None,
            })
            .expect("SessionLoaded emitted");
        assert!(
            plugins
                .iter()
                .any(|p| p.name == "hello.ts" && p.kind == PluginKind::QuickJs),
            "expected QuickJS hello.ts in {plugins:?}"
        );

        sender.send(BridgeCommand::ReadFile {
            path: "README.md".into(),
        });
        sender.send(BridgeCommand::RunShellCommand {
            command: "echo bridge-ok".into(),
        });
        let cmd_events = drain_until(&sender, &wake_rx, |evs| {
            evs.iter().any(|e| matches!(e, BridgeEvent::FileOpened(_)))
                && evs.iter().any(|e| matches!(e, BridgeEvent::TerminalFinished { .. }))
        });
        assert!(
            cmd_events
                .iter()
                .any(|e| matches!(e, BridgeEvent::FileOpened(f) if f.content.contains("after")))
        );
        assert!(
            cmd_events
                .iter()
                .any(|e| matches!(e, BridgeEvent::TerminalOutput(l) if l.text.contains("bridge-ok")))
        );
        sender.send(BridgeCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&root);
    }
}
