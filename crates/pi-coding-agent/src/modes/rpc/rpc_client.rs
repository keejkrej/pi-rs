//! Port of packages/coding-agent/src/modes/rpc/rpc-client.ts

use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_agent_core::{AgentMessage, QueueMode};
use pi_ai::ImageContent;
use pi_js::text::Utf8StreamDecoder;
use pi_js::{BoxFuture, Error, Result, Unsubscribe};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Notify, oneshot};

use crate::modes::json_event::JsonAgentSessionEvent;
use crate::modes::rpc::jsonl::{JsonlReaderHandle, attach_jsonl_line_reader, serialize_json_line_value};
use crate::modes::rpc::rpc_types::{
    BashResult, CancelledResult, ClearQueueResult, CompactionResult, CycleThinkingLevelResult, ExportHtmlResult,
    ForkMessage, ForkMessagesResponseData, ForkResult, GetCommandsResponseData, GetEntriesResult,
    GetMessagesResponseData, GetTreeResult, LastAssistantTextResponseData, PromptDisposition, PromptResponseData,
    QueuedInputDisposition, QueuedInputResponseData, RpcCommand, RpcResponse, RpcSessionState, RpcSlashCommand,
    RpcStreamingBehavior, SessionStats, ThinkingLevel,
};

// ============================================================================
// Types
// ============================================================================

/// Options for spawning an RPC agent process via [`RpcClient`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RpcClientOptions {
    /// Path to the CLI entry point or executable
    pub cli_path: Option<String>,
    /// Working directory for the agent
    pub cwd: Option<String>,
    /// Environment variables
    pub env: Option<IndexMap<String, String>>,
    /// Provider to use
    pub provider: Option<String>,
    /// Model ID to use
    pub model: Option<String>,
    /// Additional CLI arguments
    pub args: Option<Vec<String>>,
}

/// Model summary returned by [`RpcClient::get_available_models`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub provider: String,
    pub id: String,
    pub context_window: i64,
    pub reasoning: bool,
}

/// Provider + model ID pair returned by [`RpcClient::set_model`] and [`RpcClient::cycle_model`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcModelRef {
    pub provider: String,
    pub id: String,
}

/// Result returned by [`RpcClient::cycle_model`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CycleModelResult {
    pub model: RpcModelRef,
    pub thinking_level: ThinkingLevel,
    pub is_scoped: bool,
}

/// Listener invoked for streaming agent events emitted on stdout.
pub type RpcEventListener = Arc<dyn Fn(JsonAgentSessionEvent) + Send + Sync>;

type DynWriter = Pin<Box<dyn AsyncWrite + Unpin + Send>>;

#[doc(hidden)]
pub mod testing {
    use super::*;

    pub type RpcSendOverride = Arc<dyn Fn(RpcCommand) -> BoxFuture<Result<RpcResponse>> + Send + Sync>;

    thread_local! {
        static SEND_OVERRIDE: pi_js::seam::Slot<dyn Fn(RpcCommand) -> BoxFuture<Result<RpcResponse>> + Send + Sync> =
            pi_js::seam::Slot::new();
    }

    /// Override [`RpcClient`]'s internal `send()` on the current thread until the guard drops.
    pub fn override_send(
        f: impl Fn(RpcCommand) -> BoxFuture<Result<RpcResponse>> + Send + Sync + 'static,
    ) -> pi_js::seam::Guard {
        SEND_OVERRIDE.with(|slot| slot.set(Arc::new(f)))
    }

    pub(crate) fn current_send_override() -> Option<RpcSendOverride> {
        SEND_OVERRIDE.with(|slot| slot.get())
    }
}

// ============================================================================
// RPC Client (Handle Pattern)
// ============================================================================

struct RpcClientState {
    started: bool,
    generation: u64,
    stdin_writer: Option<Arc<tokio::sync::Mutex<Option<DynWriter>>>>,
    stdout_reader: Option<JsonlReaderHandle>,
    stderr_task: Option<tokio::task::AbortHandle>,
    stop_tx: Option<oneshot::Sender<()>>,
    stopped_rx: Option<oneshot::Receiver<()>>,
    event_listeners: Vec<(u64, RpcEventListener)>,
    next_listener_id: u64,
    pending_requests: IndexMap<String, oneshot::Sender<Result<RpcResponse>>>,
    request_id: u64,
    stderr: String,
    exit_error: Option<String>,
}

struct RpcClientInner {
    options: RpcClientOptions,
    state: Mutex<RpcClientState>,
    exit_notify: Notify,
}

impl RpcClientInner {
    fn stderr_snapshot(&self) -> String {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).stderr.clone()
    }

    fn create_process_exit_error(&self, code: Option<i32>, signal: Option<&str>) -> String {
        let code_str = code.map_or_else(|| "null".to_string(), |c| c.to_string());
        let signal_str = signal.unwrap_or("null");
        let stderr = self.stderr_snapshot();
        format!("Agent process exited (code={code_str} signal={signal_str}). Stderr: {stderr}")
    }

    fn create_exit_error_from_status(&self, status: std::process::ExitStatus) -> String {
        let code = status.code();
        #[cfg(unix)]
        let signal_name: Option<String> = {
            use std::os::unix::process::ExitStatusExt;
            status.signal().map(|sig| {
                nix::sys::signal::Signal::try_from(sig)
                    .map(|s| s.as_str().to_string())
                    .unwrap_or_else(|_| sig.to_string())
            })
        };
        #[cfg(not(unix))]
        let signal_name: Option<String> = None;

        self.create_process_exit_error(code, signal_name.as_deref())
    }

    fn reject_pending_requests_locked(state: &mut RpcClientState, error_msg: &str) {
        let pending = std::mem::take(&mut state.pending_requests);
        for (_, tx) in pending {
            let _ = tx.send(Err(Error::msg(error_msg)));
        }
    }

    fn on_process_exit(&self, generation: u64, error_msg: String) {
        {
            let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !guard.started || guard.generation != generation {
                return;
            }
            if guard.exit_error.is_none() {
                guard.exit_error = Some(error_msg.clone());
            }
            let effective = guard.exit_error.clone().unwrap_or(error_msg);
            Self::reject_pending_requests_locked(&mut guard, &effective);
        }
        self.exit_notify.notify_waiters();
    }

    fn handle_line(&self, line: &str) {
        let Ok(val) = pi_js::json::parse(line) else {
            // Ignore non-JSON lines
            return;
        };

        // Check if it's a response to a pending request
        if val.get("type").and_then(Value::as_str) == Some("response")
            && let Some(id) = val.get("id").and_then(Value::as_str)
        {
            let pending = {
                let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
                guard.pending_requests.shift_remove(id)
            };
            if let Some(tx) = pending {
                match serde_json::from_value::<RpcResponse>(val) {
                    Ok(resp) => {
                        let _ = tx.send(Ok(resp));
                    }
                    Err(e) => {
                        let _ = tx.send(Err(Error::msg(e.to_string())));
                    }
                }
                return;
            }
        }

        // Otherwise it's an event. Iterate a snapshot so listeners that unsubscribe during dispatch
        // do not cause later listeners to miss this event.
        let Ok(event) = serde_json::from_value::<JsonAgentSessionEvent>(val) else {
            return;
        };
        let listeners: Vec<RpcEventListener> = {
            let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.event_listeners.iter().map(|(_, l)| Arc::clone(l)).collect()
        };
        for listener in listeners {
            listener(event.clone());
        }
    }
}

/// RPC Client for programmatic access to the coding agent.
///
/// Spawns the agent in RPC mode and provides a typed API for all operations.
#[derive(Clone)]
pub struct RpcClient {
    inner: Arc<RpcClientInner>,
}

impl Default for RpcClient {
    fn default() -> Self {
        Self::new(RpcClientOptions::default())
    }
}

impl RpcClient {
    pub fn new(options: RpcClientOptions) -> Self {
        Self {
            inner: Arc::new(RpcClientInner {
                options,
                state: Mutex::new(RpcClientState {
                    started: false,
                    generation: 0,
                    stdin_writer: None,
                    stdout_reader: None,
                    stderr_task: None,
                    stop_tx: None,
                    stopped_rx: None,
                    event_listeners: Vec::new(),
                    next_listener_id: 0,
                    pending_requests: IndexMap::new(),
                    request_id: 0,
                    stderr: String::new(),
                    exit_error: None,
                }),
                exit_notify: Notify::new(),
            }),
        }
    }

    /// Start the RPC agent process.
    pub async fn start(&self) -> Result<()> {
        {
            let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if guard.started {
                return Err(Error::msg("Client already started"));
            }
        }

        let cli_path = self.inner.options.cli_path.as_deref().unwrap_or("pi");
        let mut args = vec!["--mode".to_string(), "rpc".to_string()];
        if let Some(ref provider) = self.inner.options.provider {
            args.push("--provider".to_string());
            args.push(provider.clone());
        }
        if let Some(ref model) = self.inner.options.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        if let Some(ref extra_args) = self.inner.options.args {
            args.extend(extra_args.iter().cloned());
        }

        let is_js_entry = cli_path.ends_with(".js") || cli_path.ends_with(".mjs") || cli_path.ends_with(".cjs");
        let mut cmd = if is_js_entry {
            let mut c = tokio::process::Command::new("node");
            c.arg(cli_path);
            c.args(&args);
            c
        } else {
            let mut c = tokio::process::Command::new(cli_path);
            c.args(&args);
            c
        };

        let mut env_map = pi_js::env::vars();
        if let Some(ref extra_env) = self.inner.options.env {
            for (k, v) in extra_env {
                env_map.insert(k.clone(), v.clone());
            }
        }
        cmd.env_clear().envs(env_map);
        if let Some(ref cwd) = self.inner.options.cwd {
            cmd.current_dir(cwd);
        }
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(err) => {
                let stderr = self.inner.stderr_snapshot();
                let msg = format!("Agent process error: {err}. Stderr: {stderr}");
                {
                    let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                    guard.exit_error = Some(msg.clone());
                }
                return Err(Error::msg(msg));
            }
        };

        let child_stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::msg("Failed to capture child stdin"))?;
        let child_stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::msg("Failed to capture child stdout"))?;
        let child_stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::msg("Failed to capture child stderr"))?;

        let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
        let (stopped_tx, stopped_rx) = oneshot::channel::<()>();

        let inner_for_stdout = Arc::clone(&self.inner);
        let stdout_reader = attach_jsonl_line_reader(
            child_stdout,
            Arc::new(move |line| {
                inner_for_stdout.handle_line(line);
            }),
            None,
        );

        let inner_for_stderr = Arc::clone(&self.inner);
        let stderr_handle = tokio::spawn(async move {
            let mut stderr_pipe = child_stderr;
            let mut decoder = Utf8StreamDecoder::new();
            let mut buf = [0u8; 4096];
            loop {
                match stderr_pipe.read(&mut buf).await {
                    Ok(0) | Err(_) => {
                        let tail = decoder.decode(&[], false);
                        if !tail.is_empty() {
                            {
                                let mut guard = inner_for_stderr.state.lock().unwrap_or_else(|e| e.into_inner());
                                guard.stderr.push_str(&tail);
                            }
                            pi_js::console::stderr_write(&tail);
                        }
                        break;
                    }
                    Ok(n) => {
                        let chunk = decoder.decode(&buf[..n], true);
                        if !chunk.is_empty() {
                            {
                                let mut guard = inner_for_stderr.state.lock().unwrap_or_else(|e| e.into_inner());
                                guard.stderr.push_str(&chunk);
                            }
                            pi_js::console::stderr_write(&chunk);
                        }
                    }
                }
            }
        })
        .abort_handle();

        let generation = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if guard.started {
                stdout_reader.detach();
                stderr_handle.abort();
                let _ = child.start_kill();
                return Err(Error::msg("Client already started"));
            }
            guard.generation += 1;
            guard.started = true;
            guard.exit_error = None;
            guard.stdin_writer = Some(Arc::new(tokio::sync::Mutex::new(Some(Box::pin(child_stdin)))));
            guard.stdout_reader = Some(stdout_reader);
            guard.stderr_task = Some(stderr_handle);
            guard.stop_tx = Some(stop_tx);
            guard.stopped_rx = Some(stopped_rx);
            guard.generation
        };

        let inner_for_wait = Arc::clone(&self.inner);
        tokio::spawn(async move {
            tokio::select! {
                status_res = child.wait() => {
                    let msg = match status_res {
                        Ok(status) => inner_for_wait.create_exit_error_from_status(status),
                        Err(e) => format!("Agent process error: {e}. Stderr: {}", inner_for_wait.stderr_snapshot()),
                    };
                    inner_for_wait.on_process_exit(generation, msg);
                    let _ = stopped_tx.send(());
                }
                _ = &mut stop_rx => {
                    #[cfg(unix)]
                    if let Some(pid) = child.id() {
                        let _ = nix::sys::signal::kill(
                            nix::unistd::Pid::from_raw(pid as i32),
                            nix::sys::signal::Signal::SIGTERM,
                        );
                    }
                    #[cfg(not(unix))]
                    {
                        let _ = child.start_kill();
                    }

                    tokio::select! {
                        _ = child.wait() => {}
                        _ = pi_js::time::sleep(1000, None) => {
                            let _ = child.kill().await;
                            let _ = child.wait().await;
                        }
                    }
                    let _ = stopped_tx.send(());
                }
            }
        });

        // Wait a moment for process to initialize
        let _ = pi_js::time::sleep(100, None).await;

        let exit_err = {
            let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.exit_error.clone()
        };
        if let Some(err) = exit_err {
            return Err(Error::msg(err));
        }

        Ok(())
    }

    /// Attach the client to an existing in-memory or custom async stream pair.
    pub async fn start_with_io<R, W>(&self, reader: R, writer: W) -> Result<()>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if guard.started {
            return Err(Error::msg("Client already started"));
        }
        guard.generation += 1;
        guard.started = true;
        guard.exit_error = None;
        guard.stdin_writer = Some(Arc::new(tokio::sync::Mutex::new(Some(Box::pin(writer)))));

        let inner_for_stdout = Arc::clone(&self.inner);
        let inner_for_end = Arc::clone(&self.inner);
        let generation = guard.generation;
        let stdout_reader = attach_jsonl_line_reader(
            reader,
            Arc::new(move |line| {
                inner_for_stdout.handle_line(line);
            }),
            Some(Arc::new(move || {
                let msg = inner_for_end.create_process_exit_error(Some(0), None);
                inner_for_end.on_process_exit(generation, msg);
            })),
        );
        guard.stdout_reader = Some(stdout_reader);
        Ok(())
    }

    /// Stop the RPC agent process.
    pub async fn stop(&self) {
        let (stdout_reader, stderr_task, stop_tx, stopped_rx, stdin_writer) = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if !guard.started {
                return;
            }
            guard.started = false;
            guard.pending_requests.clear();
            (
                guard.stdout_reader.take(),
                guard.stderr_task.take(),
                guard.stop_tx.take(),
                guard.stopped_rx.take(),
                guard.stdin_writer.take(),
            )
        };

        if let Some(reader) = stdout_reader {
            reader.detach();
        }
        if let Some(task) = stderr_task {
            task.abort();
        }
        if let Some(writer_arc) = stdin_writer {
            let mut w = writer_arc.lock().await;
            *w = None;
        }
        if let Some(tx) = stop_tx {
            let _ = tx.send(());
        }
        if let Some(rx) = stopped_rx {
            let _ = rx.await;
        }
    }

    /// Subscribe to agent events.
    pub fn on_event(&self, listener: RpcEventListener) -> Unsubscribe {
        let id = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.next_listener_id += 1;
            let id = guard.next_listener_id;
            guard.event_listeners.push((id, listener));
            id
        };
        let inner = Arc::clone(&self.inner);
        Unsubscribe::new(move || {
            let mut guard = inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.event_listeners.retain(|(entry_id, _)| *entry_id != id);
        })
    }

    /// Get collected stderr output (useful for debugging).
    pub fn get_stderr(&self) -> String {
        self.inner.stderr_snapshot()
    }

    // =========================================================================
    // Command Methods
    // =========================================================================

    /// Send a prompt to the agent.
    ///
    /// Returns the prompt's disposition after acceptance; use `on_event()` to receive streaming events.
    /// If the disposition is `Handled`, no run started for this prompt, so don't wait for `agent_settled`.
    pub async fn prompt(
        &self,
        message: &str,
        images: Option<Vec<ImageContent>>,
        streaming_behavior: Option<RpcStreamingBehavior>,
    ) -> Result<PromptDisposition> {
        let response = self
            .send(RpcCommand::Prompt {
                message: message.to_string(),
                images,
                streaming_behavior,
                id: None,
            })
            .await?;
        Ok(Self::get_data::<PromptResponseData>(response)?.disposition)
    }

    /// Queue a steering message to interrupt the agent mid-run.
    pub async fn steer(&self, message: &str, images: Option<Vec<ImageContent>>) -> Result<QueuedInputDisposition> {
        let response = self
            .send(RpcCommand::Steer {
                message: message.to_string(),
                images,
                id: None,
            })
            .await?;
        Ok(Self::get_data::<QueuedInputResponseData>(response)?.disposition)
    }

    /// Queue a follow-up message to be processed after the agent finishes.
    pub async fn follow_up(&self, message: &str, images: Option<Vec<ImageContent>>) -> Result<QueuedInputDisposition> {
        let response = self
            .send(RpcCommand::FollowUp {
                message: message.to_string(),
                images,
                id: None,
            })
            .await?;
        Ok(Self::get_data::<QueuedInputResponseData>(response)?.disposition)
    }

    /// Abort current operation.
    pub async fn abort(&self) -> Result<()> {
        let response = self.send(RpcCommand::Abort { id: None }).await?;
        Self::check_success(&response)
    }

    /// Clear queued steering and follow-up messages, returning their text.
    pub async fn clear_queue(&self) -> Result<ClearQueueResult> {
        let response = self.send(RpcCommand::ClearQueue { id: None }).await?;
        Self::get_data(response)
    }

    /// Start a new session, optionally with parent tracking.
    pub async fn new_session(&self, parent_session: Option<&str>) -> Result<CancelledResult> {
        let response = self
            .send(RpcCommand::NewSession {
                parent_session: parent_session.map(ToOwned::to_owned),
                id: None,
            })
            .await?;
        Self::get_data(response)
    }

    /// Get current session state.
    pub async fn get_state(&self) -> Result<RpcSessionState> {
        let response = self.send(RpcCommand::GetState { id: None }).await?;
        Self::get_data(response)
    }

    /// Set model by provider and ID.
    pub async fn set_model(&self, provider: &str, model_id: &str) -> Result<RpcModelRef> {
        let response = self
            .send(RpcCommand::SetModel {
                provider: provider.to_string(),
                model_id: model_id.to_string(),
                id: None,
            })
            .await?;
        Self::get_data(response)
    }

    /// Cycle to next model.
    pub async fn cycle_model(&self) -> Result<Option<CycleModelResult>> {
        let response = self.send(RpcCommand::CycleModel { id: None }).await?;
        Self::get_data(response)
    }

    /// Get list of available models.
    pub async fn get_available_models(&self) -> Result<Vec<ModelInfo>> {
        #[derive(Deserialize)]
        struct ModelsWrapper {
            models: Vec<ModelInfo>,
        }
        let response = self.send(RpcCommand::GetAvailableModels { id: None }).await?;
        Ok(Self::get_data::<ModelsWrapper>(response)?.models)
    }

    /// Set thinking level.
    pub async fn set_thinking_level(&self, level: ThinkingLevel) -> Result<()> {
        let response = self.send(RpcCommand::SetThinkingLevel { level, id: None }).await?;
        Self::check_success(&response)
    }

    /// Cycle thinking level.
    pub async fn cycle_thinking_level(&self) -> Result<Option<CycleThinkingLevelResult>> {
        let response = self.send(RpcCommand::CycleThinkingLevel { id: None }).await?;
        Self::get_data(response)
    }

    /// Get list of available thinking levels for the current model.
    pub async fn get_available_thinking_levels(&self) -> Result<Vec<ThinkingLevel>> {
        #[derive(Deserialize)]
        struct LevelsWrapper {
            levels: Vec<ThinkingLevel>,
        }
        let response = self.send(RpcCommand::GetAvailableThinkingLevels { id: None }).await?;
        Ok(Self::get_data::<LevelsWrapper>(response)?.levels)
    }

    /// Set steering mode.
    pub async fn set_steering_mode(&self, mode: QueueMode) -> Result<()> {
        let response = self.send(RpcCommand::SetSteeringMode { mode, id: None }).await?;
        Self::check_success(&response)
    }

    /// Set follow-up mode.
    pub async fn set_follow_up_mode(&self, mode: QueueMode) -> Result<()> {
        let response = self.send(RpcCommand::SetFollowUpMode { mode, id: None }).await?;
        Self::check_success(&response)
    }

    /// Compact session context.
    pub async fn compact(&self, custom_instructions: Option<&str>) -> Result<CompactionResult> {
        let response = self
            .send(RpcCommand::Compact {
                custom_instructions: custom_instructions.map(ToOwned::to_owned),
                id: None,
            })
            .await?;
        Self::get_data(response)
    }

    /// Set auto-compaction enabled/disabled.
    pub async fn set_auto_compaction(&self, enabled: bool) -> Result<()> {
        let response = self.send(RpcCommand::SetAutoCompaction { enabled, id: None }).await?;
        Self::check_success(&response)
    }

    /// Set auto-retry enabled/disabled.
    pub async fn set_auto_retry(&self, enabled: bool) -> Result<()> {
        let response = self.send(RpcCommand::SetAutoRetry { enabled, id: None }).await?;
        Self::check_success(&response)
    }

    /// Abort in-progress retry.
    pub async fn abort_retry(&self) -> Result<()> {
        let response = self.send(RpcCommand::AbortRetry { id: None }).await?;
        Self::check_success(&response)
    }

    /// Execute a bash command.
    pub async fn bash(&self, command: &str) -> Result<BashResult> {
        let response = self
            .send(RpcCommand::Bash {
                command: command.to_string(),
                exclude_from_context: None,
                id: None,
            })
            .await?;
        Self::get_data(response)
    }

    /// Abort running bash command.
    pub async fn abort_bash(&self) -> Result<()> {
        let response = self.send(RpcCommand::AbortBash { id: None }).await?;
        Self::check_success(&response)
    }

    /// Get session statistics.
    pub async fn get_session_stats(&self) -> Result<SessionStats> {
        let response = self.send(RpcCommand::GetSessionStats { id: None }).await?;
        Self::get_data(response)
    }

    /// Export session to HTML.
    pub async fn export_html(&self, output_path: Option<&str>) -> Result<ExportHtmlResult> {
        let response = self
            .send(RpcCommand::ExportHtml {
                output_path: output_path.map(ToOwned::to_owned),
                id: None,
            })
            .await?;
        Self::get_data(response)
    }

    /// Switch to a different session file.
    pub async fn switch_session(&self, session_path: &str) -> Result<CancelledResult> {
        let response = self
            .send(RpcCommand::SwitchSession {
                session_path: session_path.to_string(),
                id: None,
            })
            .await?;
        Self::get_data(response)
    }

    /// Fork from a specific message.
    pub async fn fork(&self, entry_id: &str) -> Result<ForkResult> {
        let response = self
            .send(RpcCommand::Fork {
                entry_id: entry_id.to_string(),
                id: None,
            })
            .await?;
        Self::get_data(response)
    }

    /// Clone the current active branch into a new session.
    pub async fn clone(&self) -> Result<CancelledResult> {
        let response = self.send(RpcCommand::Clone { id: None }).await?;
        Self::get_data(response)
    }

    /// Get messages available for forking.
    pub async fn get_fork_messages(&self) -> Result<Vec<ForkMessage>> {
        let response = self.send(RpcCommand::GetForkMessages { id: None }).await?;
        Ok(Self::get_data::<ForkMessagesResponseData>(response)?.messages)
    }

    /// Get session entries in append order, optionally only those after the `since` entry id.
    pub async fn get_entries(&self, since: Option<&str>) -> Result<GetEntriesResult> {
        let response = self
            .send(RpcCommand::GetEntries {
                since: since.map(ToOwned::to_owned),
                id: None,
            })
            .await?;
        Self::get_data(response)
    }

    /// Get the session entry tree.
    pub async fn get_tree(&self) -> Result<GetTreeResult> {
        let response = self.send(RpcCommand::GetTree { id: None }).await?;
        Self::get_data(response)
    }

    /// Get text of last assistant message.
    pub async fn get_last_assistant_text(&self) -> Result<Option<String>> {
        let response = self.send(RpcCommand::GetLastAssistantText { id: None }).await?;
        Ok(Self::get_data::<LastAssistantTextResponseData>(response)?.text)
    }

    /// Set the session display name.
    pub async fn set_session_name(&self, name: &str) -> Result<()> {
        let response = self
            .send(RpcCommand::SetSessionName {
                name: name.to_string(),
                id: None,
            })
            .await?;
        Self::check_success(&response)
    }

    /// Get all messages in the session.
    pub async fn get_messages(&self) -> Result<Vec<AgentMessage>> {
        let response = self.send(RpcCommand::GetMessages { id: None }).await?;
        Ok(Self::get_data::<GetMessagesResponseData>(response)?.messages)
    }

    /// Get available commands (extension commands, prompt templates, skills).
    pub async fn get_commands(&self) -> Result<Vec<RpcSlashCommand>> {
        let response = self.send(RpcCommand::GetCommands { id: None }).await?;
        Ok(Self::get_data::<GetCommandsResponseData>(response)?.commands)
    }

    // =========================================================================
    // Helpers
    // =========================================================================

    fn is_agent_settled(event: &JsonAgentSessionEvent) -> bool {
        matches!(event, JsonAgentSessionEvent::AgentSettled { .. })
            || matches!(
                event,
                JsonAgentSessionEvent::Extra(map) if map.get("type").and_then(Value::as_str) == Some("agent_settled")
            )
    }

    /// Wait for agent to become idle (no streaming).
    /// Resolves when `agent_settled` event is received.
    pub async fn wait_for_idle(&self, timeout_ms: Option<u64>) -> Result<()> {
        let timeout = timeout_ms.unwrap_or(60_000);
        let (tx, rx) = oneshot::channel::<()>();
        let tx_cell = Arc::new(Mutex::new(Some(tx)));
        let unsub = self.on_event(Arc::new(move |event| {
            if Self::is_agent_settled(&event)
                && let Some(sender) = tx_cell.lock().unwrap_or_else(|e| e.into_inner()).take()
            {
                let _ = sender.send(());
            }
        }));

        let res = tokio::select! {
            _ = rx => Ok(()),
            _ = pi_js::time::sleep(timeout, None) => {
                Err(Error::msg(format!(
                    "Timeout waiting for agent to become idle. Stderr: {}",
                    self.get_stderr()
                )))
            }
        };
        unsub.call();
        res
    }

    fn start_collecting_events(&self) -> (oneshot::Receiver<Vec<JsonAgentSessionEvent>>, Unsubscribe) {
        let (tx, rx) = oneshot::channel::<Vec<JsonAgentSessionEvent>>();
        let state = Arc::new(Mutex::new((Vec::new(), Some(tx))));
        let unsub = self.on_event(Arc::new(move |event| {
            let settled = Self::is_agent_settled(&event);
            let tx_done = {
                let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                if guard.1.is_none() {
                    return;
                }
                guard.0.push(event);
                if settled {
                    guard.1.take().map(|sender| (sender, std::mem::take(&mut guard.0)))
                } else {
                    None
                }
            };
            if let Some((sender, events)) = tx_done {
                let _ = sender.send(events);
            }
        }));
        (rx, unsub)
    }

    /// Collect events until agent becomes idle.
    pub async fn collect_events(&self, timeout_ms: Option<u64>) -> Result<Vec<JsonAgentSessionEvent>> {
        let timeout = timeout_ms.unwrap_or(60_000);
        let (rx, unsub) = self.start_collecting_events();
        let res = tokio::select! {
            done = rx => done.map_err(|_| Error::msg("Event collector cancelled")),
            _ = pi_js::time::sleep(timeout, None) => {
                Err(Error::msg(format!(
                    "Timeout collecting events. Stderr: {}",
                    self.get_stderr()
                )))
            }
        };
        unsub.call();
        res
    }

    /// Send prompt and wait for completion, returning all events.
    pub async fn prompt_and_wait(
        &self,
        message: &str,
        images: Option<Vec<ImageContent>>,
        timeout_ms: Option<u64>,
    ) -> Result<Vec<JsonAgentSessionEvent>> {
        let timeout = timeout_ms.unwrap_or(60_000);
        let (rx, unsub) = self.start_collecting_events();
        if let Err(err) = self.prompt(message, images, None).await {
            unsub.call();
            return Err(err);
        }
        let res = tokio::select! {
            done = rx => done.map_err(|_| Error::msg("Event collector cancelled")),
            _ = pi_js::time::sleep(timeout, None) => {
                Err(Error::msg(format!(
                    "Timeout collecting events. Stderr: {}",
                    self.get_stderr()
                )))
            }
        };
        unsub.call();
        res
    }

    // =========================================================================
    // Internal
    // =========================================================================

    async fn send(&self, mut command: RpcCommand) -> Result<RpcResponse> {
        if let Some(override_fn) = testing::current_send_override() {
            return override_fn(command).await;
        }

        let cmd_name = command.command_type().as_str();
        let (id, rx, stdin_writer, generation) = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if !guard.started {
                return Err(Error::msg("Client not started"));
            }
            if let Some(ref exit_err) = guard.exit_error {
                return Err(Error::msg(exit_err.clone()));
            }
            let Some(writer) = guard.stdin_writer.clone() else {
                return Err(Error::msg("Client not started"));
            };
            guard.request_id += 1;
            let id = format!("req_{}", guard.request_id);
            command.set_id(Some(id.clone()));
            let (tx, rx) = oneshot::channel::<Result<RpcResponse>>();
            guard.pending_requests.insert(id.clone(), tx);
            (id, rx, writer, guard.generation)
        };

        let line = serialize_json_line_value(&command)?;
        let write_result = {
            let mut writer_guard = stdin_writer.lock().await;
            match writer_guard.as_mut() {
                Some(w) => match w.write_all(line.as_bytes()).await {
                    Ok(()) => w.flush().await,
                    Err(e) => Err(e),
                },
                None => Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "stdin closed")),
            }
        };

        if let Err(write_err) = write_result {
            {
                let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                guard.pending_requests.shift_remove(&id);
            }
            // Allow an already-dead child waiter task a chance to record the exact exit status first.
            tokio::task::yield_now().await;
            let err_msg = {
                let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref existing) = guard.exit_error {
                    existing.clone()
                } else {
                    let msg = format!("Agent process stdin error: {write_err}. Stderr: {}", guard.stderr);
                    if guard.started && guard.generation == generation {
                        guard.exit_error = Some(msg.clone());
                        RpcClientInner::reject_pending_requests_locked(&mut guard, &msg);
                    }
                    msg
                }
            };
            return Err(Error::msg(err_msg));
        }

        tokio::select! {
            res = rx => match res {
                Ok(inner_res) => inner_res,
                Err(_) => {
                    let msg = {
                        let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                        guard.exit_error.clone().unwrap_or_else(|| {
                            format!("Agent process closed unexpectedly. Stderr: {}", guard.stderr)
                        })
                    };
                    Err(Error::msg(msg))
                }
            },
            _ = pi_js::time::sleep(30_000, None) => {
                {
                    let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                    guard.pending_requests.shift_remove(&id);
                }
                Err(Error::msg(format!(
                    "Timeout waiting for response to {cmd_name}. Stderr: {}",
                    self.get_stderr()
                )))
            }
        }
    }

    fn check_success(response: &RpcResponse) -> Result<()> {
        if !response.success {
            return Err(Error::msg(response.error.as_deref().unwrap_or("RPC command failed")));
        }
        Ok(())
    }

    fn get_data<T: DeserializeOwned>(response: RpcResponse) -> Result<T> {
        Self::check_success(&response)?;
        let data = response.data.unwrap_or(Value::Null);
        serde_json::from_value(data).map_err(|e| Error::msg(e.to_string()))
    }
}
