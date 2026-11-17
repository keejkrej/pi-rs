//! Port of packages/coding-agent/src/experimental/session-worker.ts

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD};
use indexmap::IndexMap;
use pi_chord::context::{BACKGROUND_CONTEXT, TODO_CONTEXT, with_cancel};
use pi_chord::services::errors::AggregateError;
use pi_chord::{
    JsonValue, RemoteServiceError, RemoteServiceErrorCode, ServiceCall, ServiceProviderUpdate, is_json_value,
    is_remote_service_error_code, parse_service_provider_update,
};
use pi_durable::storage::sqlite::node::{NodeSqliteStorageOptions, open_node_sqlite_storage};
use pi_durable::{
    AgentChange, Conversation, FieldChange, Harness, HarnessOptions, ROOT_CONVERSATION_ID, RootConversationOptions,
    TaskGraph,
};
use pi_js::BoxFuture;
use pi_js::abort::AbortReason;
use pi_js::json::parse;
use pi_js::path::is_absolute;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::core::model_runtime::{ModelRuntime, ModelRuntimeCreateOptions};
use crate::core::settings_manager::SettingsManager;
use crate::experimental::coordinator::COORDINATOR_PROTOCOL_VERSION;
use crate::experimental::durable::harness_setup::{
    CliModelSelection, ExecutionEnvs, configure_harness_http, create_coding_registry, create_harness_settings,
    find_initial_agent_model,
};
use crate::experimental::plugins::bundled::create_session_plugin_facet_loader;
use crate::experimental::process::{MAX_CONTROL_LINE_BYTES, encode_control_line};
use crate::experimental::services::worker::{
    CreateSessionWorkerServicesOptions, SessionWorkerServices, WorkerServiceScope, create_session_worker_services,
};
use crate::experimental::session_catalog::{SessionCatalogMetadata, session_storage_path};
use crate::vendor::proper_lockfile::{LockOptions, LockRelease, RetryOptions, lock as acquire_file_lock};

pub use crate::experimental::services::worker::SessionWorkerRuntime;

pub const SESSION_WORKER_CONTROL_ADDRESS_ENV: &str = "PI_SESSION_WORKER_CONTROL_ADDRESS";
pub const SESSION_WORKER_CONTROL_TOKEN_ENV: &str = "PI_SESSION_WORKER_CONTROL_TOKEN";
pub const SESSION_WORKER_SESSION_KEY_ENV: &str = "PI_SESSION_WORKER_SESSION_KEY_BASE64";
pub const SESSION_WORKER_PEER_ID_ENV: &str = "PI_SESSION_WORKER_PEER_ID";

pub const SESSION_WORKER_INITIAL_DEMAND_GRACE_ENV: &str = "__PI_SESSION_WORKER_INITIAL_DEMAND_GRACE_MS";
pub const SESSION_WORKER_ORPHAN_DEMAND_GRACE_ENV: &str = "__PI_SESSION_WORKER_ORPHAN_DEMAND_GRACE_MS";

const DEFAULT_INITIAL_DEMAND_GRACE_MS: u64 = 10_000;
const DEFAULT_ORPHAN_DEMAND_GRACE_MS: u64 = 30_000;

fn lock_mutex<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionWorkerOptions {
    pub session_dir: String,
    pub metadata: SessionCatalogMetadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub plugin_manifest_paths: Vec<String>,
}

pub type WorkerOperationScope = WorkerServiceScope;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerOperationRequest {
    #[serde(rename = "type")]
    pub kind: String,
    pub request_id: String,
    pub scope: WorkerOperationScope,
    pub call: ServiceCall,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerOperationResponse {
    #[serde(rename_all = "camelCase")]
    OperationResult {
        request_id: String,
        scope: WorkerOperationScope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<JsonValue>,
    },
    #[serde(rename_all = "camelCase")]
    OperationError {
        request_id: String,
        scope: WorkerOperationScope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<RemoteServiceErrorCode>,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionWorkerCommand {
    Shutdown,
    DiscoverWorkers,
    #[serde(rename_all = "camelCase")]
    SessionDemand {
        server_connection_id: String,
        request_id: String,
        attachment_id: String,
        attached: bool,
    },
    #[serde(rename_all = "camelCase")]
    Operation {
        request_id: String,
        scope: WorkerOperationScope,
        call: ServiceCall,
    },
    #[serde(rename_all = "camelCase")]
    OperationCancel {
        request_id: String,
        scope: WorkerOperationScope,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionWorkerEvent {
    #[serde(rename_all = "camelCase")]
    WorkerReady {
        token: String,
        session_key: String,
        session_id: String,
        pid: u32,
        metadata: SessionCatalogMetadata,
        plugin_manifest_paths: Vec<String>,
    },
    #[serde(rename_all = "camelCase")]
    WorkerFailed {
        token: String,
        session_key: String,
        message: String,
    },
    #[serde(rename_all = "camelCase")]
    DemandApplied {
        token: String,
        session_key: String,
        request_id: String,
        attachment_id: String,
        attached: bool,
    },
    #[serde(rename_all = "camelCase")]
    DemandRejected {
        token: String,
        session_key: String,
        request_id: String,
        message: String,
    },
    #[serde(rename_all = "camelCase")]
    OperationResponse {
        token: String,
        session_key: String,
        response: WorkerOperationResponse,
    },
    #[serde(rename_all = "camelCase")]
    ServiceUpdate {
        token: String,
        session_key: String,
        scope: WorkerOperationScope,
        subscription_id: String,
        update: JsonValue,
    },
}

pub struct WorkerLifecycleOptions {
    pub initial_server_connection_id: Option<String>,
    pub initial_demand_grace_ms: u64,
    pub orphan_demand_grace_ms: u64,
    pub on_retire: Arc<dyn Fn() + Send + Sync>,
}

struct DemandEntry {
    server_connection_id: String,
    attachment_id: String,
    timer: Option<JoinHandle<()>>,
}

struct WorkerLifecycleInner {
    initial_demand_grace_ms: u64,
    orphan_demand_grace_ms: u64,
    on_retire: Arc<dyn Fn() + Send + Sync>,
    demands: IndexMap<String, DemandEntry>,
    harness_active: bool,
    current_server_connection_id: Option<String>,
    initial_timer: Option<JoinHandle<()>>,
    demand_initialized: bool,
    retirement_holds: usize,
    retiring: bool,
}

pub struct WorkerRetirementHold {
    released: AtomicBool,
    lifecycle: Arc<Mutex<WorkerLifecycleInner>>,
}

impl WorkerRetirementHold {
    pub fn release(&self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        let retire = {
            let mut inner = lock_mutex(&self.lifecycle);
            if inner.retirement_holds > 0 {
                inner.retirement_holds -= 1;
            }
            reconcile_locked(&mut inner)
        };
        if let Some(cb) = retire {
            cb();
        }
    }
}

/// Worker-local reconciliation of server-generation demand and Harness activity.
#[derive(Clone)]
pub struct WorkerLifecycle {
    inner: Arc<Mutex<WorkerLifecycleInner>>,
}

impl WorkerLifecycle {
    pub fn new(options: WorkerLifecycleOptions) -> Self {
        let initial_grace = options.initial_demand_grace_ms;
        let inner = Arc::new(Mutex::new(WorkerLifecycleInner {
            initial_demand_grace_ms: options.initial_demand_grace_ms,
            orphan_demand_grace_ms: options.orphan_demand_grace_ms,
            on_retire: options.on_retire,
            demands: IndexMap::new(),
            harness_active: false,
            current_server_connection_id: options.initial_server_connection_id,
            initial_timer: None,
            demand_initialized: false,
            retirement_holds: 0,
            retiring: false,
        }));
        let inner_weak = Arc::downgrade(&inner);
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(initial_grace)).await;
            let retire = {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                let mut guard = lock_mutex(&inner);
                guard.initial_timer = None;
                guard.demand_initialized = true;
                reconcile_locked(&mut guard)
            };
            if let Some(cb) = retire {
                cb();
            }
        });
        lock_mutex(&inner).initial_timer = Some(handle);
        Self { inner }
    }

    pub fn server_connected(&self, server_connection_id: &str) {
        let mut inner = lock_mutex(&self.inner);
        inner.current_server_connection_id = Some(server_connection_id.to_string());
        for demand in inner.demands.values_mut() {
            if demand.server_connection_id == server_connection_id
                && let Some(timer) = demand.timer.take()
            {
                timer.abort();
            }
        }
    }

    pub fn server_disconnected(&self, server_connection_id: &str) {
        let mut inner = lock_mutex(&self.inner);
        if inner.current_server_connection_id.as_deref() == Some(server_connection_id) {
            inner.current_server_connection_id = None;
        }
        let orphan_ms = inner.orphan_demand_grace_ms;
        let keys: Vec<String> = inner
            .demands
            .iter()
            .filter(|(_, d)| d.server_connection_id == server_connection_id && d.timer.is_none())
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys {
            let inner_weak = Arc::downgrade(&self.inner);
            let key_clone = key.clone();
            let handle = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(orphan_ms)).await;
                let retire = {
                    let Some(inner) = inner_weak.upgrade() else {
                        return;
                    };
                    let mut guard = lock_mutex(&inner);
                    let matches = guard.demands.get(&key_clone).is_some_and(|d| d.timer.is_some());
                    if !matches {
                        return;
                    }
                    guard.demands.shift_remove(&key_clone);
                    reconcile_locked(&mut guard)
                };
                if let Some(cb) = retire {
                    cb();
                }
            });
            if let Some(demand) = inner.demands.get_mut(&key) {
                demand.timer = Some(handle);
            }
        }
    }

    pub fn begin_request(
        &self,
        server_connection_id: &str,
        attachment_id: &str,
    ) -> pi_js::Result<WorkerRetirementHold> {
        let mut inner = lock_mutex(&self.inner);
        if inner.retiring {
            return Err(pi_js::Error::msg("Session worker is retiring"));
        }
        if inner.current_server_connection_id.as_deref() != Some(server_connection_id) {
            return Err(pi_js::Error::msg(
                "Session worker received a request from a stale server generation",
            ));
        }
        let key = demand_key(server_connection_id, attachment_id);
        match inner.demands.get(&key) {
            Some(demand) if demand.timer.is_none() => {}
            _ => {
                return Err(pi_js::Error::msg(
                    "Session worker request does not match the active attachment",
                ));
            }
        }
        inner.retirement_holds += 1;
        Ok(WorkerRetirementHold {
            released: AtomicBool::new(false),
            lifecycle: Arc::clone(&self.inner),
        })
    }

    pub fn hold_retirement(&self) -> WorkerRetirementHold {
        let mut inner = lock_mutex(&self.inner);
        inner.retirement_holds += 1;
        WorkerRetirementHold {
            released: AtomicBool::new(false),
            lifecycle: Arc::clone(&self.inner),
        }
    }

    pub fn set_demand(&self, server_connection_id: &str, attachment_id: &str, attached: bool) -> pi_js::Result<()> {
        let retire = {
            let mut inner = lock_mutex(&self.inner);
            if inner.retiring {
                return Err(pi_js::Error::msg("Session worker is retiring"));
            }
            if inner.current_server_connection_id.as_deref() != Some(server_connection_id) {
                return Err(pi_js::Error::msg(
                    "Session worker received demand from a stale server generation",
                ));
            }
            inner.demand_initialized = true;
            if let Some(timer) = inner.initial_timer.take() {
                timer.abort();
            }
            let key = demand_key(server_connection_id, attachment_id);
            if attached {
                if let Some(previous) = inner.demands.get_mut(&key)
                    && let Some(timer) = previous.timer.take()
                {
                    timer.abort();
                }
                inner.demands.insert(
                    key,
                    DemandEntry {
                        server_connection_id: server_connection_id.to_string(),
                        attachment_id: attachment_id.to_string(),
                        timer: None,
                    },
                );
            } else if let Some(mut previous) = inner.demands.shift_remove(&key)
                && let Some(timer) = previous.timer.take()
            {
                timer.abort();
            }
            reconcile_locked(&mut inner)
        };
        if let Some(cb) = retire {
            cb();
        }
        Ok(())
    }

    /// Live Harness tasks, such as runs and compactions, hold the worker until they end.
    pub fn set_harness_active(&self, active: bool) {
        let retire = {
            let mut inner = lock_mutex(&self.inner);
            inner.harness_active = active;
            if !active { reconcile_locked(&mut inner) } else { None }
        };
        if let Some(cb) = retire {
            cb();
        }
    }

    pub fn close(&self) {
        let mut inner = lock_mutex(&self.inner);
        if let Some(timer) = inner.initial_timer.take() {
            timer.abort();
        }
        for demand in inner.demands.values_mut() {
            if let Some(timer) = demand.timer.take() {
                timer.abort();
            }
        }
        inner.demands.clear();
    }
}

fn reconcile_locked(inner: &mut WorkerLifecycleInner) -> Option<Arc<dyn Fn() + Send + Sync>> {
    if inner.retiring
        || !inner.demand_initialized
        || inner.retirement_holds != 0
        || inner.harness_active
        || !inner.demands.is_empty()
    {
        return None;
    }
    inner.retiring = true;
    Some(Arc::clone(&inner.on_retire))
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CoordinatorInput {
    #[serde(rename_all = "camelCase")]
    PeerRegistered {
        #[allow(dead_code)]
        peer_id: String,
        #[serde(default)]
        server_connection_id: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    ServerConnected {
        server_connection_id: String,
    },
    #[serde(rename_all = "camelCase")]
    ServerDisconnected {
        server_connection_id: String,
    },
    Message {
        from: String,
        payload: Value,
    },
}

#[derive(Clone)]
struct WorkerControl {
    initial_server_connection_id: Option<String>,
    writer_tx: mpsc::UnboundedSender<String>,
    commands_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<CoordinatorInput>>>,
    closed_notify: Arc<Notify>,
}

impl WorkerControl {
    fn send(&self, event: &SessionWorkerEvent) -> pi_js::Result<()> {
        let payload = serde_json::to_value(event).map_err(|e| pi_js::Error::msg(e.to_string()))?;
        let mut envelope = Map::new();
        envelope.insert("type".to_string(), Value::String("send".to_string()));
        envelope.insert("to".to_string(), Value::String("server".to_string()));
        envelope.insert("payload".to_string(), payload);
        let line = encode_control_line(&Value::Object(envelope))?;
        self.writer_tx
            .send(line)
            .map_err(|_| pi_js::Error::msg("Session worker control channel closed"))
    }
}

async fn connect_control() -> pi_js::Result<WorkerControl> {
    let address = pi_js::env::var(SESSION_WORKER_CONTROL_ADDRESS_ENV);
    let token = pi_js::env::var(SESSION_WORKER_CONTROL_TOKEN_ENV);
    let encoded_session_key = pi_js::env::var(SESSION_WORKER_SESSION_KEY_ENV);
    if address.as_deref().is_none_or(str::is_empty)
        || token.as_deref().is_none_or(str::is_empty)
        || encoded_session_key.as_deref().is_none_or(str::is_empty)
    {
        return Err(pi_js::Error::msg("Session worker requires a control address"));
    }
    let address = address.unwrap_or_default();
    let peer_id = pi_js::env::var(SESSION_WORKER_PEER_ID_ENV)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| pi_js::Error::msg("Session worker requires a peer ID"))?;

    let stream = UnixStream::connect(&address)
        .await
        .map_err(|e| pi_js::Error::from_io(e, "connectWorkerControl", Some(&address), None))?;
    let (read_half, mut write_half) = stream.into_split();
    let (writer_tx, mut writer_rx) = mpsc::unbounded_channel::<String>();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<CoordinatorInput>();
    let closed_notify = Arc::new(Notify::new());

    let mut reg_msg = Map::new();
    reg_msg.insert("type".to_string(), Value::String("register_peer".to_string()));
    reg_msg.insert("protocol".to_string(), Value::from(COORDINATOR_PROTOCOL_VERSION));
    reg_msg.insert("peerId".to_string(), Value::String(peer_id));
    let reg_line = encode_control_line(&Value::Object(reg_msg))?;
    let _ = writer_tx.send(reg_line);

    let closed_writer = Arc::clone(&closed_notify);
    tokio::spawn(async move {
        while let Some(line) = writer_rx.recv().await {
            if write_half.write_all(line.as_bytes()).await.is_err() {
                closed_writer.notify_waiters();
                break;
            }
        }
    });

    let (reg_tx, reg_rx) = oneshot::channel::<pi_js::Result<Option<String>>>();
    let closed_reader = Arc::clone(&closed_notify);
    tokio::spawn(async move {
        let mut reader = BufReader::new(read_half);
        let mut reg_tx = Some(reg_tx);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) => {
                    if let Some(tx) = reg_tx.take() {
                        let _ = tx.send(Err(pi_js::Error::msg(
                            "Coordinator rejected the session worker registration",
                        )));
                    }
                    closed_reader.notify_waiters();
                    break;
                }
                Ok(_) => {
                    if buf.len() > MAX_CONTROL_LINE_BYTES {
                        if let Some(tx) = reg_tx.take() {
                            let _ = tx.send(Err(pi_js::Error::msg("Session worker control message is too large")));
                        }
                        closed_reader.notify_waiters();
                        break;
                    }
                    if buf.last() == Some(&b'\n') {
                        buf.pop();
                    }
                    if buf.last() == Some(&b'\r') {
                        buf.pop();
                    }
                    let Ok(text) = std::str::from_utf8(&buf) else {
                        closed_reader.notify_waiters();
                        break;
                    };
                    let Ok(val) = parse(text) else {
                        closed_reader.notify_waiters();
                        break;
                    };
                    let Ok(input) = serde_json::from_value::<CoordinatorInput>(val) else {
                        if let Some(tx) = reg_tx.take() {
                            let _ = tx.send(Err(pi_js::Error::msg("Coordinator sent an invalid worker message")));
                        }
                        closed_reader.notify_waiters();
                        break;
                    };
                    if let Some(tx) = reg_tx.take() {
                        match input {
                            CoordinatorInput::PeerRegistered {
                                server_connection_id, ..
                            } => {
                                let _ = tx.send(Ok(server_connection_id));
                            }
                            _ => {
                                let _ = tx.send(Err(pi_js::Error::msg(
                                    "Coordinator rejected the session worker registration",
                                )));
                                closed_reader.notify_waiters();
                                break;
                            }
                        }
                    } else if cmd_tx.send(input).is_err() {
                        break;
                    }
                }
                Err(_) => {
                    if let Some(tx) = reg_tx.take() {
                        let _ = tx.send(Err(pi_js::Error::msg(
                            "Coordinator rejected the session worker registration",
                        )));
                    }
                    closed_reader.notify_waiters();
                    break;
                }
            }
        }
    });

    let initial_server_connection_id = reg_rx
        .await
        .map_err(|_| pi_js::Error::msg("Coordinator rejected the session worker registration"))??;

    Ok(WorkerControl {
        initial_server_connection_id,
        writer_tx,
        commands_rx: Arc::new(tokio::sync::Mutex::new(cmd_rx)),
        closed_notify,
    })
}

fn to_worker_service_update(update: &ServiceProviderUpdate) -> pi_js::Result<JsonValue> {
    let value = serde_json::to_value(update).map_err(|e| pi_js::Error::msg(e.to_string()))?;
    if !is_json_value(&value) {
        return Err(pi_js::Error::msg("Service produced a non-JSON update"));
    }
    let _ = parse_service_provider_update(&value)?;
    Ok(value)
}

fn demand_key(server_connection_id: &str, attachment_id: &str) -> String {
    format!("{server_connection_id}\0{attachment_id}")
}

fn same_scope(left: &WorkerOperationScope, right: &WorkerOperationScope) -> bool {
    left.server_connection_id == right.server_connection_id && left.attachment_id == right.attachment_id
}

fn lifecycle_delay(name: &str, fallback: u64) -> pi_js::Result<u64> {
    let Some(value) = pi_js::env::var(name) else {
        return Ok(fallback);
    };
    value
        .parse::<u64>()
        .map_err(|_| pi_js::Error::msg(format!("{name} must be a non-negative safe integer")))
}

fn decode_session_key(encoded: &str) -> pi_js::Result<String> {
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .or_else(|_| URL_SAFE.decode(encoded))
        .or_else(|_| STANDARD.decode(encoded))
        .map_err(|e| pi_js::Error::msg(format!("Invalid session worker key: {e}")))?;
    String::from_utf8(bytes).map_err(|e| pi_js::Error::msg(format!("Invalid session worker key UTF-8: {e}")))
}

async fn close_resources(
    runtime: Option<&SessionWorkerRuntime>,
    services: Option<&Arc<SessionWorkerServices>>,
    release_ownership: Option<LockRelease>,
) -> pi_js::Result<()> {
    let mut errors = Vec::new();
    if let Some(svc) = services
        && let Err(err) = svc.dispose().await
    {
        errors.push(err);
    }
    if let Some(rt) = runtime {
        if let Err(err) = rt.harness.close(TODO_CONTEXT.clone()).await {
            errors.push(err);
        }
        if let Some(ref cleanup) = rt.cleanup
            && let Err(err) = cleanup(TODO_CONTEXT.clone()).await
        {
            errors.push(err);
        }
    }
    if let Some(lock_release) = release_ownership
        && let Err(err) = lock_release.release().await
    {
        errors.push(err);
    }
    match errors.len() {
        0 => Ok(()),
        1 => Err(errors
            .into_iter()
            .next()
            .unwrap_or_else(|| pi_js::Error::msg("Cleanup failed"))),
        _ => Err(pi_js::Error::typed(
            "AggregateError",
            AggregateError {
                message: "Session worker cleanup failed".to_string(),
                errors,
            },
        )),
    }
}

/// Open the Session's Harness over `database_path` and return it with its root conversation.
pub type CreateSessionWorkerHarness =
    Arc<dyn Fn(String, SessionWorkerOptions) -> BoxFuture<pi_js::Result<SessionWorkerRuntime>> + Send + Sync>;

type ActiveRequestCancelMap = IndexMap<String, (WorkerOperationScope, Arc<dyn Fn(Option<AbortReason>) + Send + Sync>)>;

async fn run_worker(
    options: SessionWorkerOptions,
    create_harness: CreateSessionWorkerHarness,
    failure_control_out: &Mutex<Option<WorkerControl>>,
) -> pi_js::Result<()> {
    let metadata = options.metadata.clone();
    let session_id = metadata.id.clone();
    let control = connect_control().await?;
    *lock_mutex(failure_control_out) = Some(control.clone());

    let token = pi_js::env::var(SESSION_WORKER_CONTROL_TOKEN_ENV).unwrap_or_default();
    let encoded_session_key = pi_js::env::var(SESSION_WORKER_SESSION_KEY_ENV).unwrap_or_default();
    let session_key = decode_session_key(&encoded_session_key)?;
    let plugin_manifest_paths = options.plugin_manifest_paths.clone();

    let release_ownership = acquire_file_lock(
        &metadata.path,
        LockOptions {
            realpath: Some(true),
            stale: Some(2_000),
            update: Some(1_000),
            retries: Some(RetryOptions {
                retries: 320,
                factor: 1.0,
                min_timeout: 25,
                max_timeout: 25,
                randomize: false,
            }),
            lockfile_path: None,
            on_compromised: None,
        },
    )
    .await?;

    let mut release_slot = Some(release_ownership);
    let runtime = match create_harness(session_storage_path(&metadata), options.clone()).await {
        Ok(rt) => rt,
        Err(err) => {
            if let Err(cleanup_err) = close_resources(None, None, release_slot.take()).await {
                return Err(pi_js::Error::typed(
                    "AggregateError",
                    AggregateError {
                        message: "Session worker startup and cleanup failed".to_string(),
                        errors: vec![err, cleanup_err],
                    },
                ));
            }
            return Err(err);
        }
    };

    let task_graph = match runtime.harness.task_graph(TODO_CONTEXT.clone()).await {
        Ok(tg) => tg,
        Err(err) => {
            if let Err(cleanup_err) = close_resources(Some(&runtime), None, release_slot.take()).await {
                return Err(pi_js::Error::typed(
                    "AggregateError",
                    AggregateError {
                        message: "Session worker startup and cleanup failed".to_string(),
                        errors: vec![err, cleanup_err],
                    },
                ));
            }
            return Err(err);
        }
    };

    let control_for_publish = control.clone();
    let token_for_publish = token.clone();
    let session_key_for_publish = session_key.clone();
    let services = match create_session_worker_services(CreateSessionWorkerServicesOptions {
        harness: runtime.harness.clone(),
        conversation: runtime.conversation.clone(),
        model_runtime: runtime.model_runtime.clone(),
        settings_manager: runtime.settings_manager.clone(),
        facet_loader: runtime.facet_loader.clone(),
        publish: Arc::new(move |scope, subscription_id, update| {
            let control = control_for_publish.clone();
            let token = token_for_publish.clone();
            let session_key = session_key_for_publish.clone();
            Box::pin(async move {
                let encoded_update = to_worker_service_update(&update)?;
                control.send(&SessionWorkerEvent::ServiceUpdate {
                    token,
                    session_key,
                    scope,
                    subscription_id,
                    update: encoded_update,
                })
            })
        }),
    })
    .await
    {
        Ok(svc) => svc,
        Err(err) => {
            let _ = task_graph.dispose();
            if let Err(cleanup_err) = close_resources(Some(&runtime), None, release_slot.take()).await {
                return Err(pi_js::Error::typed(
                    "AggregateError",
                    AggregateError {
                        message: "Session worker startup and cleanup failed".to_string(),
                        errors: vec![err, cleanup_err],
                    },
                ));
            }
            return Err(err);
        }
    };

    let initial_grace = lifecycle_delay(SESSION_WORKER_INITIAL_DEMAND_GRACE_ENV, DEFAULT_INITIAL_DEMAND_GRACE_MS)?;
    let orphan_grace = lifecycle_delay(SESSION_WORKER_ORPHAN_DEMAND_GRACE_ENV, DEFAULT_ORPHAN_DEMAND_GRACE_MS)?;

    let active_requests: Arc<Mutex<ActiveRequestCancelMap>> = Arc::new(Mutex::new(IndexMap::new()));
    let shutdown_notify = Arc::new(Notify::new());
    let shutdown_requested = Arc::new(AtomicBool::new(false));
    let trigger_shutdown: Arc<dyn Fn() + Send + Sync> = {
        let shutdown_requested = Arc::clone(&shutdown_requested);
        let shutdown_notify = Arc::clone(&shutdown_notify);
        Arc::new(move || {
            if !shutdown_requested.swap(true, Ordering::AcqRel) {
                shutdown_notify.notify_waiters();
            }
        })
    };

    let lifecycle = WorkerLifecycle::new(WorkerLifecycleOptions {
        initial_server_connection_id: control.initial_server_connection_id.clone(),
        initial_demand_grace_ms: initial_grace,
        orphan_demand_grace_ms: orphan_grace,
        on_retire: Arc::clone(&trigger_shutdown),
    });

    let lifecycle_for_activity = lifecycle.clone();
    let stop_activity = task_graph.subscribe(Arc::new(move |graph: TaskGraph, _ctx, _del| {
        lifecycle_for_activity.set_harness_active(!graph.tasks.is_empty());
        Box::pin(futures::future::ready(Ok(())))
    }));

    let _ = runtime.harness.resume();

    let ready_flag = Arc::new(AtomicBool::new(false));
    let send_ready = {
        let control = control.clone();
        let token = token.clone();
        let session_key = session_key.clone();
        let session_id = session_id.clone();
        let metadata = metadata.clone();
        let plugin_manifest_paths = plugin_manifest_paths.clone();
        move || {
            control.send(&SessionWorkerEvent::WorkerReady {
                token: token.clone(),
                session_key: session_key.clone(),
                session_id: session_id.clone(),
                pid: std::process::id(),
                metadata: metadata.clone(),
                plugin_manifest_paths: plugin_manifest_paths.clone(),
            })
        }
    };

    let closed_notify_wait = Arc::clone(&control.closed_notify);
    let shutdown_on_close = Arc::clone(&trigger_shutdown);
    let close_watch_handle = tokio::spawn(async move {
        closed_notify_wait.notified().await;
        shutdown_on_close();
    });

    let cmd_task = {
        let control = control.clone();
        let lifecycle = lifecycle.clone();
        let services = Arc::clone(&services);
        let active_requests = Arc::clone(&active_requests);
        let token = token.clone();
        let session_key = session_key.clone();
        let session_id = session_id.clone();
        let metadata = metadata.clone();
        let plugin_manifest_paths = plugin_manifest_paths.clone();
        let ready_flag = Arc::clone(&ready_flag);
        let trigger_shutdown = Arc::clone(&trigger_shutdown);
        tokio::spawn(async move {
            let mut rx = control.commands_rx.lock().await;
            while let Some(input) = rx.recv().await {
                match input {
                    CoordinatorInput::ServerConnected { server_connection_id } => {
                        lifecycle.server_connected(&server_connection_id);
                    }
                    CoordinatorInput::ServerDisconnected { server_connection_id } => {
                        services.remove_subscriptions(|scope| scope.server_connection_id == server_connection_id);
                        let cancels: Vec<Arc<dyn Fn(Option<AbortReason>) + Send + Sync>> = {
                            let guard = lock_mutex(&active_requests);
                            guard
                                .values()
                                .filter(|(scope, _)| scope.server_connection_id == server_connection_id)
                                .map(|(_, cancel)| Arc::clone(cancel))
                                .collect()
                        };
                        for cancel in cancels {
                            cancel(Some(AbortReason::error("AbortError", "Server disconnected")));
                        }
                        lifecycle.server_disconnected(&server_connection_id);
                    }
                    CoordinatorInput::Message { from, payload } if from == "server" => {
                        let Ok(command) = serde_json::from_value::<SessionWorkerCommand>(payload) else {
                            continue;
                        };
                        match command {
                            SessionWorkerCommand::Shutdown => {
                                trigger_shutdown();
                                break;
                            }
                            SessionWorkerCommand::DiscoverWorkers => {
                                if ready_flag.load(Ordering::Acquire)
                                    && control
                                        .send(&SessionWorkerEvent::WorkerReady {
                                            token: token.clone(),
                                            session_key: session_key.clone(),
                                            session_id: session_id.clone(),
                                            pid: std::process::id(),
                                            metadata: metadata.clone(),
                                            plugin_manifest_paths: plugin_manifest_paths.clone(),
                                        })
                                        .is_err()
                                {
                                    trigger_shutdown();
                                    break;
                                }
                            }
                            SessionWorkerCommand::SessionDemand {
                                server_connection_id,
                                request_id,
                                attachment_id,
                                attached,
                            } => {
                                let hold = lifecycle.hold_retirement();
                                if !attached {
                                    services.remove_subscriptions(|scope| {
                                        scope.server_connection_id == server_connection_id
                                            && scope.attachment_id == attachment_id
                                    });
                                }
                                let event = match lifecycle.set_demand(&server_connection_id, &attachment_id, attached)
                                {
                                    Ok(()) => SessionWorkerEvent::DemandApplied {
                                        token: token.clone(),
                                        session_key: session_key.clone(),
                                        request_id,
                                        attachment_id,
                                        attached,
                                    },
                                    Err(err) => SessionWorkerEvent::DemandRejected {
                                        token: token.clone(),
                                        session_key: session_key.clone(),
                                        request_id,
                                        message: err.message().to_string(),
                                    },
                                };
                                let send_res = control.send(&event);
                                hold.release();
                                if send_res.is_err() {
                                    trigger_shutdown();
                                    break;
                                }
                            }
                            SessionWorkerCommand::Operation {
                                request_id,
                                scope,
                                call,
                            } => {
                                let control = control.clone();
                                let lifecycle = lifecycle.clone();
                                let services = Arc::clone(&services);
                                let active_requests = Arc::clone(&active_requests);
                                let token = token.clone();
                                let session_key = session_key.clone();
                                let trigger_shutdown = Arc::clone(&trigger_shutdown);
                                tokio::spawn(async move {
                                    let cancellable = with_cancel(BACKGROUND_CONTEXT.clone());
                                    let hold = match lifecycle
                                        .begin_request(&scope.server_connection_id, &scope.attachment_id)
                                    {
                                        Ok(h) => Some(h),
                                        Err(err) => {
                                            if control
                                                .send(&SessionWorkerEvent::OperationResponse {
                                                    token,
                                                    session_key,
                                                    response: WorkerOperationResponse::OperationError {
                                                        request_id,
                                                        scope,
                                                        code: extract_remote_service_code(&err),
                                                        message: err.message().to_string(),
                                                    },
                                                })
                                                .is_err()
                                            {
                                                trigger_shutdown();
                                            }
                                            return;
                                        }
                                    };
                                    lock_mutex(&active_requests)
                                        .insert(request_id.clone(), (scope.clone(), Arc::clone(&cancellable.cancel)));
                                    let outcome = services
                                        .invoke(&call, scope.clone(), cancellable.context)
                                        .await
                                        .and_then(|res| {
                                            if let Some(ref val) = res
                                                && !is_json_value(val)
                                            {
                                                return Err(pi_js::Error::msg("Service produced a non-JSON result"));
                                            }
                                            Ok(res)
                                        });
                                    lock_mutex(&active_requests).shift_remove(&request_id);
                                    if let Some(h) = hold {
                                        h.release();
                                    }
                                    let response = match outcome {
                                        Ok(result) => WorkerOperationResponse::OperationResult {
                                            request_id,
                                            scope,
                                            result,
                                        },
                                        Err(err) => WorkerOperationResponse::OperationError {
                                            request_id,
                                            scope,
                                            code: extract_remote_service_code(&err),
                                            message: err.message().to_string(),
                                        },
                                    };
                                    if control
                                        .send(&SessionWorkerEvent::OperationResponse {
                                            token,
                                            session_key,
                                            response,
                                        })
                                        .is_err()
                                    {
                                        trigger_shutdown();
                                    }
                                });
                            }
                            SessionWorkerCommand::OperationCancel { request_id, scope } => {
                                let cancel = {
                                    let guard = lock_mutex(&active_requests);
                                    guard
                                        .get(&request_id)
                                        .filter(|(active_scope, _)| same_scope(active_scope, &scope))
                                        .map(|(_, cancel)| Arc::clone(cancel))
                                };
                                if let Some(cancel) = cancel {
                                    cancel(Some(AbortReason::error("AbortError", "Service operation cancelled")));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        })
    };

    ready_flag.store(true, Ordering::Release);
    if let Err(err) = send_ready() {
        lifecycle.close();
        stop_activity.call();
        let _ = task_graph.dispose();
        close_watch_handle.abort();
        cmd_task.abort();
        if let Err(cleanup_err) = close_resources(Some(&runtime), Some(&services), release_slot.take()).await {
            return Err(pi_js::Error::typed(
                "AggregateError",
                AggregateError {
                    message: "Session worker readiness and cleanup failed".to_string(),
                    errors: vec![err, cleanup_err],
                },
            ));
        }
        return Err(err);
    }

    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigint = signal(SignalKind::interrupt()).ok();
        let mut sigterm = signal(SignalKind::terminate()).ok();
        tokio::select! {
            _ = shutdown_notify.notified(), if !shutdown_requested.load(Ordering::Acquire) => {}
            _ = async {
                if let Some(ref mut s) = sigint {
                    let _ = s.recv().await;
                } else {
                    futures::future::pending::<()>().await;
                }
            } => {}
            _ = async {
                if let Some(ref mut s) = sigterm {
                    let _ = s.recv().await;
                } else {
                    futures::future::pending::<()>().await;
                }
            } => {}
        }
    }
    #[cfg(not(unix))]
    {
        if !shutdown_requested.load(Ordering::Acquire) {
            shutdown_notify.notified().await;
        }
    }

    lifecycle.close();
    stop_activity.call();
    let _ = task_graph.dispose();
    close_watch_handle.abort();
    cmd_task.abort();
    services.remove_subscriptions(|_| true);
    let pending_cancels: Vec<_> = {
        let mut guard = lock_mutex(&active_requests);
        guard.drain(..).map(|(_, (_, cancel))| cancel).collect()
    };
    for cancel in pending_cancels {
        cancel(Some(AbortReason::error("AbortError", "Session worker is closing")));
    }

    close_resources(Some(&runtime), Some(&services), release_slot.take()).await
}

pub fn extract_remote_service_code(error: &pi_js::Error) -> Option<RemoteServiceErrorCode> {
    if let Some(remote) = error.downcast_ref::<RemoteServiceError>() {
        return Some(remote.code);
    }
    error
        .code()
        .filter(|&c| is_remote_service_error_code(&Value::String(c.to_string())))
        .and_then(|c| serde_json::from_value::<RemoteServiceErrorCode>(Value::String(c.to_string())).ok())
}

pub async fn run_session_worker_with_harness(
    args: &[String],
    create_harness: CreateSessionWorkerHarness,
) -> pi_js::Result<()> {
    let failure_control: Mutex<Option<WorkerControl>> = Mutex::new(None);
    let result = async {
        if args.len() != 1 {
            return Err(pi_js::Error::msg("Session worker requires one options argument"));
        }
        let parsed_val = parse(&args[0])
            .map_err(|err| pi_js::Error::js_with_cause("Error", "Session worker received invalid options", err))?;
        let options: SessionWorkerOptions = serde_json::from_value(parsed_val)
            .map_err(|_| pi_js::Error::msg("Session worker received invalid options"))?;
        if options.session_dir.is_empty()
            || options.metadata.id.is_empty()
            || options.plugin_manifest_paths.iter().any(|p| p.is_empty())
            || options.provider.as_deref().is_some_and(str::is_empty)
            || options.model.as_deref().is_some_and(str::is_empty)
            || (options.provider.is_some() && options.model.is_none())
            || !is_absolute(&options.session_dir)
            || !is_absolute(&options.metadata.cwd)
            || !is_absolute(&options.metadata.path)
        {
            return Err(pi_js::Error::msg("Session worker received invalid options"));
        }
        run_worker(options, create_harness, &failure_control).await
    }
    .await;

    if let Err(ref error) = result
        && let Some(control) = lock_mutex(&failure_control).clone()
        && let Some(token) = pi_js::env::var(SESSION_WORKER_CONTROL_TOKEN_ENV)
        && let Some(encoded) = pi_js::env::var(SESSION_WORKER_SESSION_KEY_ENV)
        && let Ok(session_key) = decode_session_key(&encoded)
    {
        let _ = control.send(&SessionWorkerEvent::WorkerFailed {
            token,
            session_key,
            message: error.message().to_string(),
        });
    }

    result
}

async fn create_coding_agent_harness(
    database_path: String,
    options: SessionWorkerOptions,
) -> pi_js::Result<SessionWorkerRuntime> {
    let cwd = options.metadata.cwd.clone();
    let model_runtime = Arc::new(ModelRuntime::create(ModelRuntimeCreateOptions::default()).await?);
    let settings_manager = Arc::new(SettingsManager::create(Some(&cwd), None)?);
    configure_harness_http(&settings_manager);
    let envs = Arc::new(ExecutionEnvs::new(cwd.clone()));
    let mut harness: Option<Harness> = None;

    let setup_res = async {
        let storage = Arc::new(open_node_sqlite_storage(&database_path, NodeSqliteStorageOptions::default()).await?);
        let opened = Harness::open(
            storage,
            HarnessOptions {
                models: model_runtime.as_models().clone(),
                registry: create_coding_registry(Arc::clone(&settings_manager), &cwd),
                settings: Some(create_harness_settings(Arc::clone(&settings_manager))),
                env: Some(envs.env_factory()),
                conversation_created: None,
                now: None,
                on_report: Some(Arc::new(|err| {
                    pi_js::console::error(&err.to_string());
                })),
            },
            TODO_CONTEXT.clone(),
        )
        .await?;
        harness = Some(opened.clone());

        let created = opened
            .conversation(ROOT_CONVERSATION_ID, TODO_CONTEXT.clone())
            .await?
            .is_none();
        let initial = if created {
            let cli = options.model.as_ref().map(|model| CliModelSelection {
                provider: options.provider.clone(),
                model: model.clone(),
            });
            Some(find_initial_agent_model(&settings_manager, &model_runtime, cli.as_ref()).await?)
        } else {
            None
        };

        let root_agent = AgentChange {
            cwd: FieldChange::set(cwd),
            model: initial
                .as_ref()
                .and_then(|i| i.model.clone())
                .map_or(FieldChange::Unchanged, FieldChange::set),
            thinking_level: initial
                .as_ref()
                .and_then(|i| i.thinking_level)
                .map_or(FieldChange::Unchanged, FieldChange::set),
            ..Default::default()
        };
        let conversation: Conversation = opened
            .root(
                TODO_CONTEXT.clone(),
                Some(RootConversationOptions {
                    agent: Some(root_agent),
                    init: None,
                }),
            )
            .await?;
        let envs_for_cleanup = Arc::clone(&envs);
        Ok(SessionWorkerRuntime {
            harness: opened,
            conversation,
            model_runtime: Some(model_runtime),
            settings_manager: Some(settings_manager),
            facet_loader: create_session_plugin_facet_loader(&options.plugin_manifest_paths),
            cleanup: Some(Arc::new(move |context| {
                let envs = Arc::clone(&envs_for_cleanup);
                Box::pin(async move {
                    envs.cleanup(&context).await;
                    Ok(())
                })
            })),
        })
    }
    .await;

    match setup_res {
        Ok(rt) => Ok(rt),
        Err(err) => {
            let mut cleanup_errors = Vec::new();
            if let Some(h) = harness
                && let Err(e) = h.close(TODO_CONTEXT.clone()).await
            {
                cleanup_errors.push(e);
            }
            envs.cleanup(&TODO_CONTEXT.clone()).await;
            if !cleanup_errors.is_empty() {
                let mut all = vec![err];
                all.extend(cleanup_errors);
                return Err(pi_js::Error::typed(
                    "AggregateError",
                    AggregateError {
                        message: "Session worker Harness startup and cleanup failed".to_string(),
                        errors: all,
                    },
                ));
            }
            Err(err)
        }
    }
}

pub async fn run_session_worker_process(args: &[String]) -> pi_js::Result<()> {
    run_session_worker_with_harness(
        args,
        Arc::new(|db_path, opts| Box::pin(create_coding_agent_harness(db_path, opts))),
    )
    .await
}

pub fn serialize_worker_options_for_test(options: &SessionWorkerOptions) -> pi_js::Result<String> {
    let val = serde_json::to_value(options).map_err(|e| pi_js::Error::msg(e.to_string()))?;
    Ok(pi_js::json::stringify(&val))
}
