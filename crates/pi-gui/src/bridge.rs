//! Background Tokio runtime bridge connecting `pi-gui` to `pi-coding-agent` off the UI thread.

use std::sync::{Arc, Mutex, mpsc};

use rune::Waker;

use crate::state::{
    AuthProviderViewModel, ContextUsageView, DiffSummary, ExtensionDialogView, FileTreeEntryView, LaunchConfig,
    MessageViewModel, ModelViewModel, OpenFileView, PluginViewModel, TerminalOutputLine, ThreadSummary, ToastTone,
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
    tx: tokio::sync::mpsc::UnboundedSender<BridgeCommand>,
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

/// Owns the background Tokio current-thread runtime driving `pi-coding-agent` sessions.
pub struct GuiBridge {
    sender: BridgeSender,
}

impl GuiBridge {
    /// Spawns a dedicated background worker thread with a current-thread Tokio runtime.
    pub fn spawn(config: LaunchConfig, waker: Waker) -> Self {
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<BridgeCommand>();
        let (ev_tx, ev_rx) = mpsc::channel::<BridgeEvent>();

        let _ = std::thread::Builder::new()
            .name("pi-gui-bridge".to_string())
            .spawn(move || {
                pi_js::ensure_crypto_provider();
                let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
                    let _ = ev_tx.send(BridgeEvent::Error(
                        "Failed to initialize pi-gui background Tokio runtime".to_string(),
                    ));
                    waker();
                    return;
                };
                rt.block_on(async move {
                    while let Some(cmd) = cmd_rx.recv().await {
                        match cmd {
                            BridgeCommand::Shutdown => break,
                            BridgeCommand::Initialize => {
                                let _ = ev_tx.send(BridgeEvent::SessionLoaded {
                                    session_id: config.session.clone().unwrap_or_else(|| "default-session".to_string()),
                                    messages: Vec::new(),
                                    models: Vec::new(),
                                    active_model: None,
                                    plugins: Vec::new(),
                                    auth_providers: Vec::new(),
                                    context_usage: ContextUsageView {
                                        thinking_level: config.thinking.clone().unwrap_or_else(|| "medium".to_string()),
                                        ..ContextUsageView::default()
                                    },
                                });
                                waker();
                            }
                            _ => {
                                waker();
                            }
                        }
                    }
                });
            });

        let sender = BridgeSender {
            tx: cmd_tx,
            rx: Arc::new(Mutex::new(ev_rx)),
        };
        sender.send(BridgeCommand::Initialize);
        Self { sender }
    }

    #[must_use]
    pub fn sender(&self) -> BridgeSender {
        self.sender.clone()
    }
}
