//! Port of packages/coding-agent/src/experimental/session-worker-manager.ts

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use indexmap::{IndexMap, IndexSet};
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::services::errors::AggregateError;
use pi_chord::{
    Context, JsonValue, ServiceCall, ServiceProviderUpdate, ServiceUpdatePublisher, create_service_unsubscribe_call,
    decode_service_control_call, parse_service_provider_update,
};
use pi_js::BoxFuture;
use pi_js::Unsubscribe;
use pi_js::abort::{AbortReason, AbortSignal};
use pi_js::crypto::random_uuid;
use pi_server::{RoutedSessionAttachment, RoutedSessionHandle, ServerError, ServerOperationErrorCode};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::experimental::coordinator::{CoordinatorConnection, CoordinatorConnectionEvent};
use crate::experimental::durable::harness_setup::CliModelSelection;
use crate::experimental::process::{
    InternalChildProcess, InternalProcessRole, InternalProcessSpawnOptions, spawn_internal_process,
};
use crate::experimental::session_catalog::SessionCatalogMetadata;
use crate::experimental::session_worker::{
    SESSION_WORKER_CONTROL_ADDRESS_ENV, SESSION_WORKER_CONTROL_TOKEN_ENV, SESSION_WORKER_PEER_ID_ENV,
    SESSION_WORKER_SESSION_KEY_ENV, SessionWorkerCommand, SessionWorkerEvent, SessionWorkerOptions,
    WorkerOperationResponse, WorkerOperationScope,
};

const WORKER_STARTUP_TIMEOUT_MS: u64 = 15_000;
const WORKER_SHUTDOWN_TIMEOUT_MS: u64 = 10_000;
const WORKER_DISCOVERY_TIMEOUT_MS: u64 = 5_000;
const WORKER_DEMAND_TIMEOUT_MS: u64 = 5_000;

fn lock_mutex<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Optional test hook overriding SIGKILL delivery when stopping/aborting workers.
pub type WorkerKillHook = Arc<dyn Fn(u32) + Send + Sync>;

static WORKER_KILL_HOOK: Mutex<Option<WorkerKillHook>> = Mutex::new(None);

/// Install or clear the worker SIGKILL test hook, returning the previous hook.
pub fn set_worker_kill_hook(hook: Option<WorkerKillHook>) -> Option<WorkerKillHook> {
    let mut guard = lock_mutex(&WORKER_KILL_HOOK);
    std::mem::replace(&mut *guard, hook)
}

fn kill_pid_sigkill(pid: u32) {
    let hook = lock_mutex(&WORKER_KILL_HOOK).clone();
    if let Some(hook) = hook {
        hook(pid);
        return;
    }
    #[cfg(unix)]
    if let Ok(raw) = i32::try_from(pid) {
        let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(raw), nix::sys::signal::Signal::SIGKILL);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SessionPluginSelectionConflictError {
    pub message: String,
}

impl SessionPluginSelectionConflictError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn into_error(self) -> pi_js::Error {
        pi_js::Error::typed("SessionPluginSelectionConflictError", self)
    }
}

/// Abstracted coordinator interface used by `SessionWorkerManager`.
pub trait WorkerCoordinator: Send + Sync {
    fn control_path(&self) -> &str;
    fn server_connection_id(&self) -> &str;
    fn was_replaced(&self) -> bool;
    fn on_event(&self, listener: Arc<dyn Fn(CoordinatorConnectionEvent) + Send + Sync>) -> Unsubscribe;
    fn send<'a>(&'a self, peer_id: &'a str, payload: &'a Value) -> futures::future::BoxFuture<'a, pi_js::Result<()>>;
    fn broadcast<'a>(&'a self, payload: &'a Value) -> futures::future::BoxFuture<'a, pi_js::Result<()>>;
}

impl WorkerCoordinator for CoordinatorConnection {
    fn control_path(&self) -> &str {
        CoordinatorConnection::control_path(self)
    }

    fn server_connection_id(&self) -> &str {
        CoordinatorConnection::server_connection_id(self)
    }

    fn was_replaced(&self) -> bool {
        CoordinatorConnection::was_replaced(self)
    }

    fn on_event(&self, listener: Arc<dyn Fn(CoordinatorConnectionEvent) + Send + Sync>) -> Unsubscribe {
        CoordinatorConnection::on_event(self, listener)
    }

    fn send<'a>(&'a self, peer_id: &'a str, payload: &'a Value) -> futures::future::BoxFuture<'a, pi_js::Result<()>> {
        Box::pin(CoordinatorConnection::send(self, peer_id, payload))
    }

    fn broadcast<'a>(&'a self, payload: &'a Value) -> futures::future::BoxFuture<'a, pi_js::Result<()>> {
        Box::pin(CoordinatorConnection::broadcast(self, payload))
    }
}

struct WorkerRecord {
    peer_id: String,
    metadata: SessionCatalogMetadata,
    pid: u32,
    token: String,
    plugin_manifest_paths: Vec<String>,
    terminated_tx: watch::Sender<Option<Option<String>>>,
    terminated_rx: watch::Receiver<Option<Option<String>>>,
    attachment_ids: IndexSet<String>,
    expected_stop: bool,
    stop_started: bool,
    stopping: bool,
}

impl WorkerRecord {
    fn resolve_terminated(&self, error: Option<&pi_js::Error>) {
        let _ = self.terminated_tx.send_if_modified(|slot| {
            if slot.is_some() {
                return false;
            }
            *slot = Some(error.map(|e| e.message().to_string()));
            true
        });
    }

    fn terminated_future(&self) -> BoxFuture<Option<pi_js::Error>> {
        let mut rx = self.terminated_rx.clone();
        Box::pin(async move {
            let _ = rx.wait_for(|v| v.is_some()).await;
            let val = rx.borrow().clone().flatten();
            val.map(pi_js::Error::msg)
        })
    }
}

struct PendingDemand {
    attachment_id: String,
    attached: bool,
    request_id: String,
    timer: JoinHandle<()>,
    worker: Arc<Mutex<WorkerRecord>>,
    tx: Option<oneshot::Sender<pi_js::Result<()>>>,
}

struct PendingWorkerOperation {
    worker: Arc<Mutex<WorkerRecord>>,
    scope: WorkerOperationScope,
    abort_unsub: Option<pi_js::abort::ListenerGuard>,
    tx: Option<oneshot::Sender<pi_js::Result<Option<JsonValue>>>>,
}

struct WorkerServiceSubscription {
    worker: Arc<Mutex<WorkerRecord>>,
    scope: WorkerOperationScope,
    tx: mpsc::UnboundedSender<(ServiceProviderUpdate, Context)>,
}

struct PendingLaunch {
    session_key: String,
    peer_id: String,
    token: String,
    plugin_manifest_paths: Vec<String>,
    child: InternalChildProcess,
    timer: JoinHandle<()>,
    waiters: Vec<oneshot::Sender<pi_js::Result<Arc<Mutex<WorkerRecord>>>>>,
    settled: Option<pi_js::Result<Arc<Mutex<WorkerRecord>>>>,
}

struct ManagerState {
    workers_by_session: IndexMap<String, Arc<Mutex<WorkerRecord>>>,
    workers_by_peer: IndexMap<String, Arc<Mutex<WorkerRecord>>>,
    worker_pids: IndexMap<String, u32>,
    pending: IndexMap<String, Arc<Mutex<PendingLaunch>>>,
    pending_demand: IndexMap<String, PendingDemand>,
    pending_operations: IndexMap<String, PendingWorkerOperation>,
    service_subscriptions: IndexMap<String, WorkerServiceSubscription>,
    discovery_peers: Option<IndexSet<String>>,
    resolve_discovery: Option<oneshot::Sender<()>>,
    detached: bool,
    shutting_down: bool,
}

struct ManagerInner {
    coordinator: Arc<dyn WorkerCoordinator>,
    session_dir: String,
    model: Option<CliModelSelection>,
    on_worker_count_changed: Option<Arc<dyn Fn(usize) + Send + Sync>>,
    state: Mutex<ManagerState>,
    remove_listener: Mutex<Option<Unsubscribe>>,
}

/// Session and process bookkeeping owned by one replaceable server process.
#[derive(Clone)]
pub struct SessionWorkerManager {
    inner: Arc<ManagerInner>,
}

impl SessionWorkerManager {
    pub fn new(
        coordinator: Arc<dyn WorkerCoordinator>,
        session_dir: impl Into<String>,
        model: Option<CliModelSelection>,
        on_worker_count_changed: Option<Arc<dyn Fn(usize) + Send + Sync>>,
    ) -> Self {
        let inner = Arc::new(ManagerInner {
            coordinator,
            session_dir: session_dir.into(),
            model,
            on_worker_count_changed,
            state: Mutex::new(ManagerState {
                workers_by_session: IndexMap::new(),
                workers_by_peer: IndexMap::new(),
                worker_pids: IndexMap::new(),
                pending: IndexMap::new(),
                pending_demand: IndexMap::new(),
                pending_operations: IndexMap::new(),
                service_subscriptions: IndexMap::new(),
                discovery_peers: None,
                resolve_discovery: None,
                detached: false,
                shutting_down: false,
            }),
            remove_listener: Mutex::new(None),
        });

        let weak = Arc::downgrade(&inner);
        let unsub = inner.coordinator.on_event(Arc::new(move |event| {
            if let Some(inner) = weak.upgrade() {
                handle_coordinator_event(&inner, event);
            }
        }));
        *lock_mutex(&inner.remove_listener) = Some(unsub);
        Self { inner }
    }

    pub fn worker_pids(&self) -> IndexMap<String, u32> {
        lock_mutex(&self.inner.state).worker_pids.clone()
    }

    pub fn tracked_sessions(&self) -> Vec<SessionCatalogMetadata> {
        let state = lock_mutex(&self.inner.state);
        state
            .workers_by_session
            .values()
            .map(|w| lock_mutex(w).metadata.clone())
            .collect()
    }

    pub fn assert_session_plugin_manifest_paths(
        &self,
        metadata: &SessionCatalogMetadata,
        manifest_paths: &[String],
    ) -> pi_js::Result<()> {
        let state = lock_mutex(&self.inner.state);
        if let Some(existing) = state.workers_by_session.get(&metadata.path)
            && !same_strings(&lock_mutex(existing).plugin_manifest_paths, manifest_paths)
        {
            return Err(SessionPluginSelectionConflictError::new(format!(
                "Session {} is active with a different plugin selection",
                metadata.id
            ))
            .into_error());
        }
        if let Some(pending) = state.pending.get(&metadata.path)
            && !same_strings(&lock_mutex(pending).plugin_manifest_paths, manifest_paths)
        {
            return Err(SessionPluginSelectionConflictError::new(format!(
                "Session {} is starting with a different plugin selection",
                metadata.id
            ))
            .into_error());
        }
        Ok(())
    }

    pub async fn discover(&self, peer_ids: &IndexSet<String>) -> pi_js::Result<()> {
        let rx_opt = {
            let mut state = lock_mutex(&self.inner.state);
            if state.detached {
                return Ok(());
            }
            let undiscovered: IndexSet<String> = peer_ids
                .iter()
                .filter(|peer_id| {
                    !state.workers_by_peer.contains_key(*peer_id) && pending_peer_locked(&state, peer_id).is_none()
                })
                .cloned()
                .collect();
            if undiscovered.is_empty() {
                return Ok(());
            }
            let (tx, rx) = oneshot::channel();
            state.discovery_peers = Some(undiscovered);
            state.resolve_discovery = Some(tx);
            Some(rx)
        };
        if let Some(rx) = rx_opt {
            let cmd = serde_json::to_value(SessionWorkerCommand::DiscoverWorkers)
                .map_err(|e| pi_js::Error::msg(e.to_string()))?;
            self.inner.coordinator.broadcast(&cmd).await?;
            let _ = tokio::time::timeout(Duration::from_millis(WORKER_DISCOVERY_TIMEOUT_MS), rx).await;
            let mut state = lock_mutex(&self.inner.state);
            state.discovery_peers = None;
            state.resolve_discovery = None;
        }
        Ok(())
    }

    pub async fn open_session(
        &self,
        metadata: &SessionCatalogMetadata,
        context: Context,
        plugin_manifest_paths: &[String],
    ) -> pi_js::Result<Arc<dyn RoutedSessionHandle>> {
        let existing_or_pending = {
            let state = lock_mutex(&self.inner.state);
            if state.detached || state.shutting_down {
                return Err(pi_js::Error::msg("Experimental server is shutting down"));
            }
            if let Some(existing) = state.workers_by_session.get(&metadata.path)
                && !same_strings(&lock_mutex(existing).plugin_manifest_paths, plugin_manifest_paths)
            {
                return Err(SessionPluginSelectionConflictError::new(format!(
                    "Session {} is active with a different plugin selection",
                    metadata.id
                ))
                .into_error());
            }
            if let Some(pending) = state.pending.get(&metadata.path)
                && !same_strings(&lock_mutex(pending).plugin_manifest_paths, plugin_manifest_paths)
            {
                return Err(SessionPluginSelectionConflictError::new(format!(
                    "Session {} is starting with a different plugin selection",
                    metadata.id
                ))
                .into_error());
            }
            if let Some(existing) = state.workers_by_session.get(&metadata.path) {
                Some(Ok(Arc::clone(existing)))
            } else {
                state.pending.get(&metadata.path).map(|p| Err(Arc::clone(p)))
            }
        };

        let worker = match existing_or_pending {
            Some(Ok(w)) => w,
            Some(Err(pending)) => await_pending_launch(&pending).await?,
            None => launch_worker(&self.inner, metadata, context, plugin_manifest_paths).await?,
        };
        Ok(Arc::new(WorkerRoutedHandle {
            inner: Arc::clone(&self.inner),
            worker,
        }))
    }

    pub async fn close_session(&self, metadata: &SessionCatalogMetadata, context: Context) -> pi_js::Result<()> {
        let worker_opt = {
            let state = lock_mutex(&self.inner.state);
            if let Some(w) = state.workers_by_session.get(&metadata.path) {
                Some(Ok(Arc::clone(w)))
            } else {
                state.pending.get(&metadata.path).map(|p| Err(Arc::clone(p)))
            }
        };
        let worker = match worker_opt {
            Some(Ok(w)) => Some(w),
            Some(Err(pending)) => await_pending_launch(&pending).await.ok(),
            None => None,
        };
        if let Some(worker) = worker {
            stop_worker(&self.inner, &worker, context).await?;
        }
        Ok(())
    }

    pub async fn shutdown(&self) -> pi_js::Result<()> {
        let (pending_workers, active_workers) = {
            let mut state = lock_mutex(&self.inner.state);
            if state.detached || state.shutting_down {
                return Ok(());
            }
            state.shutting_down = true;
            let pending: Vec<Arc<Mutex<PendingLaunch>>> = state.pending.values().cloned().collect();
            let active: Vec<Arc<Mutex<WorkerRecord>>> = state.workers_by_session.values().cloned().collect();
            (pending, active)
        };

        let shutdown_payload =
            serde_json::to_value(SessionWorkerCommand::Shutdown).map_err(|e| pi_js::Error::msg(e.to_string()))?;
        for pending in &pending_workers {
            let peer_id = lock_mutex(pending).peer_id.clone();
            let _ = self.inner.coordinator.send(&peer_id, &shutdown_payload).await;
        }

        let stop_pending = async {
            let wait_futs = pending_workers.iter().map(|pending| {
                let child = lock_mutex(pending).child.clone();
                async move {
                    if !child.has_exited() {
                        let _ = child.wait().await;
                    }
                }
            });
            if tokio::time::timeout(
                Duration::from_millis(WORKER_SHUTDOWN_TIMEOUT_MS),
                futures::future::join_all(wait_futs),
            )
            .await
            .is_err()
            {
                for pending in &pending_workers {
                    let child = lock_mutex(pending).child.clone();
                    kill_child_sigkill(&child);
                    let _ = child.wait().await;
                }
            }
        };

        let stop_active = futures::future::join_all(
            active_workers
                .iter()
                .map(|w| stop_worker(&self.inner, w, BACKGROUND_CONTEXT.clone())),
        );
        let (_, active_results) = tokio::join!(stop_pending, stop_active);
        detach_state(&self.inner);
        for res in active_results {
            res?;
        }
        Ok(())
    }

    /// Forget workers without stopping them when this server is replaced.
    pub fn detach(&self) {
        let (pending_list, op_ids) = {
            let mut state = lock_mutex(&self.inner.state);
            if state.detached {
                return;
            }
            state.detached = true;
            let pending_list: Vec<Arc<Mutex<PendingLaunch>>> = state.pending.values().cloned().collect();
            let op_ids: Vec<String> = state.pending_operations.keys().cloned().collect();
            (pending_list, op_ids)
        };
        for pending in pending_list {
            let mut guard = lock_mutex(&pending);
            guard.timer.abort();
            settle_pending_launch(&mut guard, Err(pi_js::Error::msg("Experimental server was replaced")));
        }
        for request_id in op_ids {
            reject_operation(
                &self.inner,
                &request_id,
                pi_js::Error::msg("Experimental server was replaced during a worker operation"),
            );
        }
        detach_state(&self.inner);
    }
}

struct WorkerRoutedHandle {
    inner: Arc<ManagerInner>,
    worker: Arc<Mutex<WorkerRecord>>,
}

#[async_trait]
impl RoutedSessionHandle for WorkerRoutedHandle {
    async fn attach_client(&self, context: Context) -> pi_js::Result<Arc<dyn RoutedSessionAttachment>> {
        let (detached_or_shutting, stopping, peer_id, current_matches) = {
            let state = lock_mutex(&self.inner.state);
            let worker = lock_mutex(&self.worker);
            let current = state
                .workers_by_peer
                .get(&worker.peer_id)
                .is_some_and(|w| Arc::ptr_eq(w, &self.worker));
            (
                state.detached || state.shutting_down,
                worker.stopping,
                worker.peer_id.clone(),
                current,
            )
        };
        if detached_or_shutting || stopping {
            return Err(pi_js::Error::msg("Experimental Session worker is stopping"));
        }
        if !current_matches {
            return Err(pi_js::Error::msg("Experimental Session worker is no longer available"));
        }

        let attachment_id = random_uuid();
        lock_mutex(&self.worker).attachment_ids.insert(attachment_id.clone());
        if let Err(err) = apply_demand(&self.inner, &self.worker, &attachment_id, true, true, context).await {
            lock_mutex(&self.worker).attachment_ids.shift_remove(&attachment_id);
            return Err(err);
        }
        let scope = operation_scope(&self.inner, &self.worker, &attachment_id)?;
        let _ = peer_id;
        Ok(Arc::new(WorkerRoutedAttachment {
            inner: Arc::clone(&self.inner),
            worker: Arc::clone(&self.worker),
            attachment_id,
            scope,
            released: AtomicBool::new(false),
        }))
    }

    fn terminated(&self) -> Option<BoxFuture<Option<pi_js::Error>>> {
        Some(lock_mutex(&self.worker).terminated_future())
    }

    async fn close(&self, context: Context) -> pi_js::Result<()> {
        stop_worker(&self.inner, &self.worker, context).await
    }
}

struct WorkerRoutedAttachment {
    inner: Arc<ManagerInner>,
    worker: Arc<Mutex<WorkerRecord>>,
    attachment_id: String,
    scope: WorkerOperationScope,
    released: AtomicBool,
}

#[async_trait]
impl RoutedSessionAttachment for WorkerRoutedAttachment {
    async fn invoke_service(
        &self,
        call: &ServiceCall,
        publish: ServiceUpdatePublisher,
        context: Context,
    ) -> pi_js::Result<Option<JsonValue>> {
        invoke_service(&self.inner, &self.worker, &self.scope, call, publish, context).await
    }

    async fn release(&self, context: Context) -> pi_js::Result<()> {
        if self.released.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let (detached, has_attachment) = {
            let state = lock_mutex(&self.inner.state);
            let worker = lock_mutex(&self.worker);
            (state.detached, worker.attachment_ids.contains(&self.attachment_id))
        };
        if detached || !has_attachment {
            return Ok(());
        }
        let res = apply_demand(&self.inner, &self.worker, &self.attachment_id, false, true, context).await;
        lock_mutex(&self.worker)
            .attachment_ids
            .shift_remove(&self.attachment_id);
        remove_service_subscriptions(&self.inner, |w, s| {
            Arc::ptr_eq(w, &self.worker) && same_scope(s, &self.scope)
        });
        match res {
            Ok(()) => Ok(()),
            Err(err) => {
                let detached_now = lock_mutex(&self.inner.state).detached;
                if detached_now || self.inner.coordinator.was_replaced() {
                    Ok(())
                } else {
                    Err(err)
                }
            }
        }
    }
}

async fn await_pending_launch(pending: &Arc<Mutex<PendingLaunch>>) -> pi_js::Result<Arc<Mutex<WorkerRecord>>> {
    let rx_opt = {
        let mut guard = lock_mutex(pending);
        match &guard.settled {
            Some(Ok(w)) => return Ok(Arc::clone(w)),
            Some(Err(err)) => return Err(pi_js::Error::msg(err.message())),
            None => {
                let (tx, rx) = oneshot::channel();
                guard.waiters.push(tx);
                Some(rx)
            }
        }
    };
    let Some(rx) = rx_opt else {
        return Err(pi_js::Error::msg("Session worker launch cancelled"));
    };
    rx.await
        .map_err(|_| pi_js::Error::msg("Session worker launch cancelled"))?
}

fn settle_pending_launch(pending: &mut PendingLaunch, outcome: pi_js::Result<Arc<Mutex<WorkerRecord>>>) {
    if pending.settled.is_some() {
        return;
    }
    match outcome {
        Ok(ref worker) => {
            pending.settled = Some(Ok(Arc::clone(worker)));
            for waiter in pending.waiters.drain(..) {
                let _ = waiter.send(Ok(Arc::clone(worker)));
            }
        }
        Err(ref err) => {
            pending.settled = Some(Err(pi_js::Error::msg(err.message())));
            for waiter in pending.waiters.drain(..) {
                let _ = waiter.send(Err(pi_js::Error::msg(err.message())));
            }
        }
    }
}

async fn launch_worker(
    inner: &Arc<ManagerInner>,
    metadata: &SessionCatalogMetadata,
    _context: Context,
    plugin_manifest_paths: &[String],
) -> pi_js::Result<Arc<Mutex<WorkerRecord>>> {
    let session_key = metadata.path.clone();
    let peer_id = format!("worker-{}", random_uuid());
    let token = random_uuid();
    let options = SessionWorkerOptions {
        session_dir: inner.session_dir.clone(),
        metadata: metadata.clone(),
        provider: inner.model.as_ref().and_then(|m| m.provider.clone()),
        model: inner.model.as_ref().map(|m| m.model.clone()),
        plugin_manifest_paths: plugin_manifest_paths.to_vec(),
    };
    let options_val = serde_json::to_value(&options).map_err(|e| pi_js::Error::msg(e.to_string()))?;
    let serialized = pi_js::json::stringify(&options_val);

    let mut env = IndexMap::new();
    env.insert(
        SESSION_WORKER_CONTROL_ADDRESS_ENV.to_string(),
        inner.coordinator.control_path().to_string(),
    );
    env.insert(SESSION_WORKER_CONTROL_TOKEN_ENV.to_string(), token.clone());
    env.insert(
        SESSION_WORKER_SESSION_KEY_ENV.to_string(),
        URL_SAFE_NO_PAD.encode(session_key.as_bytes()),
    );
    env.insert(SESSION_WORKER_PEER_ID_ENV.to_string(), peer_id.clone());

    let child = spawn_internal_process(
        InternalProcessRole::SessionWorker,
        &[serialized],
        &InternalProcessSpawnOptions {
            entry_path: None,
            entry_url: None,
            env: Some(env),
        },
    )?;

    let (tx, rx) = oneshot::channel();
    let inner_for_timeout = Arc::clone(inner);
    let session_key_timeout = session_key.clone();
    let timer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(WORKER_STARTUP_TIMEOUT_MS)).await;
        fail_pending(
            &inner_for_timeout,
            &session_key_timeout,
            pi_js::Error::msg("Session worker startup timed out"),
        );
    });

    let pending = Arc::new(Mutex::new(PendingLaunch {
        session_key: session_key.clone(),
        peer_id: peer_id.clone(),
        token,
        plugin_manifest_paths: plugin_manifest_paths.to_vec(),
        child: child.clone(),
        timer,
        waiters: vec![tx],
        settled: None,
    }));

    {
        let mut state = lock_mutex(&inner.state);
        state.pending.insert(session_key, Arc::clone(&pending));
    }
    notify_worker_count_changed(inner);

    let inner_for_exit = Arc::clone(inner);
    let pending_for_exit = Arc::clone(&pending);
    let child_for_exit = child;
    tokio::spawn(async move {
        let outcome = child_for_exit.wait().await;
        child_exited(&inner_for_exit, &pending_for_exit, outcome);
    });

    rx.await
        .map_err(|_| pi_js::Error::msg("Session worker launch cancelled"))?
}

async fn invoke_service(
    inner: &Arc<ManagerInner>,
    worker: &Arc<Mutex<WorkerRecord>>,
    scope: &WorkerOperationScope,
    call: &ServiceCall,
    publish: ServiceUpdatePublisher,
    context: Context,
) -> pi_js::Result<Option<JsonValue>> {
    let control = decode_service_control_call(call);
    let added_key = match &control {
        Some(pi_chord::ServiceControlCall::Subscribe { subscription_id, .. }) => {
            let key = scoped_service_subscription_key(scope, subscription_id);
            let (tx, mut rx) = mpsc::unbounded_channel::<(ServiceProviderUpdate, Context)>();
            {
                let mut state = lock_mutex(&inner.state);
                if state.service_subscriptions.contains_key(&key) {
                    return Err(pi_js::Error::msg("Service subscription ID is already active"));
                }
                state.service_subscriptions.insert(
                    key.clone(),
                    WorkerServiceSubscription {
                        worker: Arc::clone(worker),
                        scope: scope.clone(),
                        tx,
                    },
                );
            }
            let sub_id = subscription_id.clone();
            let publish_fn = Arc::clone(&publish);
            tokio::spawn(async move {
                while let Some((update, update_ctx)) = rx.recv().await {
                    let _ = publish_fn(sub_id.clone(), update, update_ctx).await;
                }
            });
            Some((key, subscription_id.clone()))
        }
        _ => None,
    };

    let response = match invoke_worker(inner, worker, scope, call, context).await {
        Ok(res) => res,
        Err(err) => {
            if let Some((key, sub_id)) = added_key {
                lock_mutex(&inner.state).service_subscriptions.shift_remove(&key);
                let unsub_call = create_service_unsubscribe_call(&sub_id);
                let inner_c = Arc::clone(inner);
                let worker_c = Arc::clone(worker);
                let scope_c = scope.clone();
                tokio::spawn(async move {
                    let _ = invoke_worker(&inner_c, &worker_c, &scope_c, &unsub_call, BACKGROUND_CONTEXT.clone()).await;
                });
            }
            return Err(err);
        }
    };

    if let Some(pi_chord::ServiceControlCall::Unsubscribe { subscription_id }) = control {
        lock_mutex(&inner.state)
            .service_subscriptions
            .shift_remove(&scoped_service_subscription_key(scope, &subscription_id));
    }

    Ok(response)
}

async fn apply_demand(
    inner: &Arc<ManagerInner>,
    worker: &Arc<Mutex<WorkerRecord>>,
    attachment_id: &str,
    attached: bool,
    compensate_on_timeout: bool,
    _context: Context,
) -> pi_js::Result<()> {
    let peer_id = {
        let state = lock_mutex(&inner.state);
        let w = lock_mutex(worker);
        let current = state
            .workers_by_peer
            .get(&w.peer_id)
            .is_some_and(|existing| Arc::ptr_eq(existing, worker));
        if w.stopping || !current {
            return Err(pi_js::Error::msg("Experimental Session worker is stopping"));
        }
        w.peer_id.clone()
    };

    let request_id = random_uuid();
    let (tx, rx) = oneshot::channel();
    let inner_timeout = Arc::clone(inner);
    let req_timeout = request_id.clone();
    let timer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(WORKER_DEMAND_TIMEOUT_MS)).await;
        if compensate_on_timeout {
            reconcile_demand_timeout(&inner_timeout, &req_timeout).await;
        } else {
            reject_demand(
                &inner_timeout,
                &req_timeout,
                pi_js::Error::msg("Session worker demand update timed out"),
            );
        }
    });

    {
        let mut state = lock_mutex(&inner.state);
        state.pending_demand.insert(
            request_id.clone(),
            PendingDemand {
                attachment_id: attachment_id.to_string(),
                attached,
                request_id: request_id.clone(),
                timer,
                worker: Arc::clone(worker),
                tx: Some(tx),
            },
        );
    }

    let payload = serde_json::to_value(SessionWorkerCommand::SessionDemand {
        server_connection_id: inner.coordinator.server_connection_id().to_string(),
        request_id: request_id.clone(),
        attachment_id: attachment_id.to_string(),
        attached,
    })
    .map_err(|e| pi_js::Error::msg(e.to_string()))?;

    if let Err(err) = inner.coordinator.send(&peer_id, &payload).await {
        reject_demand(inner, &request_id, err);
    }

    rx.await
        .map_err(|_| pi_js::Error::msg("Session worker demand channel closed"))?
}

async fn invoke_worker(
    inner: &Arc<ManagerInner>,
    worker: &Arc<Mutex<WorkerRecord>>,
    scope: &WorkerOperationScope,
    call: &ServiceCall,
    context: Context,
) -> pi_js::Result<Option<JsonValue>> {
    let checked_scope = operation_scope(inner, worker, &scope.attachment_id)?;
    if let Some(sig) = context.abort_signal()
        && sig.aborted()
    {
        return Err(abort_error(&sig));
    }

    let request_id = random_uuid();
    let (tx, rx) = oneshot::channel();
    let peer_id = lock_mutex(worker).peer_id.clone();

    let abort_unsub = if let Some(sig) = context.abort_signal() {
        let inner_abort = Arc::clone(inner);
        let req_abort = request_id.clone();
        let peer_abort = peer_id.clone();
        let scope_abort = checked_scope.clone();
        let sig_clone = sig.clone();
        Some(sig.on_abort(move |_reason| {
            if !lock_mutex(&inner_abort.state)
                .pending_operations
                .contains_key(&req_abort)
            {
                return;
            }
            if let Ok(cancel_payload) = serde_json::to_value(SessionWorkerCommand::OperationCancel {
                request_id: req_abort.clone(),
                scope: scope_abort.clone(),
            }) {
                let inner_send = Arc::clone(&inner_abort);
                let peer_send = peer_abort.clone();
                tokio::spawn(async move {
                    let _ = inner_send.coordinator.send(&peer_send, &cancel_payload).await;
                });
            }
            reject_operation(&inner_abort, &req_abort, abort_error(&sig_clone));
        }))
    } else {
        None
    };

    lock_mutex(&inner.state).pending_operations.insert(
        request_id.clone(),
        PendingWorkerOperation {
            worker: Arc::clone(worker),
            scope: checked_scope.clone(),
            abort_unsub,
            tx: Some(tx),
        },
    );

    let op_payload = serde_json::to_value(SessionWorkerCommand::Operation {
        request_id: request_id.clone(),
        scope: checked_scope,
        call: call.clone(),
    })
    .map_err(|e| pi_js::Error::msg(e.to_string()))?;

    if let Err(err) = inner.coordinator.send(&peer_id, &op_payload).await {
        reject_operation(inner, &request_id, err);
    }

    rx.await
        .map_err(|_| pi_js::Error::msg("Session worker operation cancelled"))?
}

fn operation_scope(
    inner: &Arc<ManagerInner>,
    worker: &Arc<Mutex<WorkerRecord>>,
    attachment_id: &str,
) -> pi_js::Result<WorkerOperationScope> {
    let state = lock_mutex(&inner.state);
    let w = lock_mutex(worker);
    if state.detached || state.shutting_down || w.stopping {
        return Err(pi_js::Error::msg("Experimental Session worker is stopping"));
    }
    let current = state
        .workers_by_peer
        .get(&w.peer_id)
        .is_some_and(|existing| Arc::ptr_eq(existing, worker));
    if !current || !w.attachment_ids.contains(attachment_id) {
        return Err(pi_js::Error::msg(
            "Experimental Session worker has no active attachment",
        ));
    }
    Ok(WorkerOperationScope {
        server_connection_id: inner.coordinator.server_connection_id().to_string(),
        attachment_id: attachment_id.to_string(),
    })
}

async fn stop_worker(
    inner: &Arc<ManagerInner>,
    worker: &Arc<Mutex<WorkerRecord>>,
    _context: Context,
) -> pi_js::Result<()> {
    let (skip, peer_id, pid, terminated_fut) = {
        let state = lock_mutex(&inner.state);
        let mut w = lock_mutex(worker);
        let current = state
            .workers_by_peer
            .get(&w.peer_id)
            .is_some_and(|existing| Arc::ptr_eq(existing, worker));
        if state.detached || !current {
            return Ok(());
        }
        let already = w.stop_started;
        w.stop_started = true;
        w.stopping = true;
        w.expected_stop = true;
        (already, w.peer_id.clone(), w.pid, w.terminated_future())
    };
    if skip {
        let _ = terminated_fut.await;
        return Ok(());
    }

    reject_worker_operations(inner, worker, "Session worker is stopping");
    if let Ok(payload) = serde_json::to_value(SessionWorkerCommand::Shutdown) {
        let _ = inner.coordinator.send(&peer_id, &payload).await;
    }

    if tokio::time::timeout(Duration::from_millis(WORKER_SHUTDOWN_TIMEOUT_MS), terminated_fut)
        .await
        .is_err()
    {
        kill_pid_sigkill(pid);
        remove_worker(inner, worker, None);
        let term = lock_mutex(worker).terminated_future();
        let _ = term.await;
    }
    Ok(())
}

fn handle_coordinator_event(inner: &Arc<ManagerInner>, event: CoordinatorConnectionEvent) {
    if lock_mutex(&inner.state).detached {
        return;
    }
    match event {
        CoordinatorConnectionEvent::PeerConnected { .. } => {}
        CoordinatorConnectionEvent::PeerDisconnected { peer_id } => {
            mark_discovered(inner, &peer_id);
            let (worker_opt, pending_opt) = {
                let state = lock_mutex(&inner.state);
                (
                    state.workers_by_peer.get(&peer_id).cloned(),
                    pending_peer_locked(&state, &peer_id),
                )
            };
            if let Some(worker) = worker_opt {
                let err = {
                    let w = lock_mutex(&worker);
                    if w.expected_stop {
                        None
                    } else {
                        Some(pi_js::Error::msg(format!(
                            "Session worker {} disconnected unexpectedly",
                            w.metadata.id
                        )))
                    }
                };
                remove_worker(inner, &worker, err.as_ref());
            }
            if let Some(pending) = pending_opt {
                let session_key = lock_mutex(&pending).session_key.clone();
                fail_pending(
                    inner,
                    &session_key,
                    pi_js::Error::msg("Session worker disconnected during startup"),
                );
            }
        }
        CoordinatorConnectionEvent::Message { from, payload } => {
            let Ok(message) = serde_json::from_value::<SessionWorkerEvent>(payload.clone()) else {
                if payload.as_object().and_then(|o| o.get("type")).and_then(Value::as_str) == Some("operation_response")
                    && let Some(worker) = lock_mutex(&inner.state).workers_by_peer.get(&from).cloned()
                {
                    reject_worker_operations(inner, &worker, "Session worker returned an invalid operation response");
                }
                return;
            };
            match message {
                SessionWorkerEvent::WorkerFailed {
                    token,
                    session_key,
                    message: err_msg,
                } => {
                    let matches = lock_mutex(&inner.state).pending.get(&session_key).is_some_and(|p| {
                        let g = lock_mutex(p);
                        g.peer_id == from && g.token == token
                    });
                    if matches {
                        fail_pending(
                            inner,
                            &session_key,
                            pi_js::Error::msg(format!("Session worker failed: {err_msg}")),
                        );
                    }
                }
                SessionWorkerEvent::DemandApplied {
                    token,
                    session_key,
                    request_id,
                    attachment_id,
                    attached,
                } => {
                    let resolved = {
                        let mut state = lock_mutex(&inner.state);
                        let valid = state.pending_demand.get(&request_id).is_some_and(|pending| {
                            let w = lock_mutex(&pending.worker);
                            w.peer_id == from
                                && w.token == token
                                && w.metadata.path == session_key
                                && pending.attachment_id == attachment_id
                                && pending.attached == attached
                        });
                        if valid {
                            state.pending_demand.shift_remove(&request_id)
                        } else {
                            None
                        }
                    };
                    if let Some(mut pending) = resolved {
                        pending.timer.abort();
                        if let Some(tx) = pending.tx.take() {
                            let _ = tx.send(Ok(()));
                        }
                    }
                }
                SessionWorkerEvent::DemandRejected {
                    token,
                    session_key,
                    request_id,
                    message: err_msg,
                } => {
                    let resolved = {
                        let mut state = lock_mutex(&inner.state);
                        let valid = state.pending_demand.get(&request_id).is_some_and(|pending| {
                            let w = lock_mutex(&pending.worker);
                            w.peer_id == from && w.token == token && w.metadata.path == session_key
                        });
                        if valid {
                            state.pending_demand.shift_remove(&request_id)
                        } else {
                            None
                        }
                    };
                    if let Some(mut pending) = resolved {
                        pending.timer.abort();
                        if let Some(tx) = pending.tx.take() {
                            let _ = tx.send(Err(pi_js::Error::msg(format!(
                                "Session worker rejected demand: {err_msg}"
                            ))));
                        }
                    }
                }
                SessionWorkerEvent::OperationResponse {
                    token,
                    session_key,
                    response,
                } => handle_operation_response(inner, &from, &token, &session_key, response),
                SessionWorkerEvent::ServiceUpdate {
                    token,
                    session_key,
                    scope,
                    subscription_id,
                    update,
                } => handle_service_event(inner, &from, &token, &session_key, &scope, &subscription_id, update),
                SessionWorkerEvent::WorkerReady {
                    token,
                    session_key,
                    session_id,
                    pid,
                    metadata,
                    plugin_manifest_paths,
                } => record_ready_worker(
                    inner,
                    &from,
                    &token,
                    &session_key,
                    &session_id,
                    pid,
                    metadata,
                    plugin_manifest_paths,
                ),
            }
        }
    }
}

fn handle_operation_response(
    inner: &Arc<ManagerInner>,
    peer_id: &str,
    token: &str,
    session_key: &str,
    response: WorkerOperationResponse,
) {
    let (request_id, resp_scope) = match &response {
        WorkerOperationResponse::OperationResult { request_id, scope, .. }
        | WorkerOperationResponse::OperationError { request_id, scope, .. } => (request_id.clone(), scope.clone()),
    };

    enum Outcome {
        None,
        Mismatch,
        Matched(PendingWorkerOperation),
    }

    let outcome = {
        let mut state = lock_mutex(&inner.state);
        match state.pending_operations.get(&request_id) {
            None => Outcome::None,
            Some(pending) => {
                let w = lock_mutex(&pending.worker);
                let ok = w.peer_id == peer_id
                    && w.token == token
                    && w.metadata.path == session_key
                    && same_scope(&resp_scope, &pending.scope);
                drop(w);
                if ok {
                    match state.pending_operations.shift_remove(&request_id) {
                        Some(p) => Outcome::Matched(p),
                        None => Outcome::None,
                    }
                } else {
                    Outcome::Mismatch
                }
            }
        }
    };

    match outcome {
        Outcome::None => {}
        Outcome::Mismatch => reject_operation(
            inner,
            &request_id,
            pi_js::Error::msg("Session worker returned a mismatched operation response"),
        ),
        Outcome::Matched(mut pending) => {
            drop(pending.abort_unsub.take());
            let settled = match response {
                WorkerOperationResponse::OperationResult { result, .. } => Ok(result),
                WorkerOperationResponse::OperationError { code, message, .. } => match code {
                    None => Err(pi_js::Error::msg(format!("Session worker operation failed: {message}"))),
                    Some(code) => Err(ServerError::new(ServerOperationErrorCode::from(code), message).into_error()),
                },
            };
            if let Some(tx) = pending.tx.take() {
                let _ = tx.send(settled);
            }
        }
    }
}

fn handle_service_event(
    inner: &Arc<ManagerInner>,
    peer_id: &str,
    token: &str,
    session_key: &str,
    scope: &WorkerOperationScope,
    subscription_id: &str,
    update: Value,
) {
    let tx_opt = {
        let state = lock_mutex(&inner.state);
        state
            .service_subscriptions
            .get(&scoped_service_subscription_key(scope, subscription_id))
            .and_then(|entry| {
                let w = lock_mutex(&entry.worker);
                if w.peer_id == peer_id
                    && w.token == token
                    && w.metadata.path == session_key
                    && same_scope(&entry.scope, scope)
                {
                    Some(entry.tx.clone())
                } else {
                    None
                }
            })
    };
    let Some(tx) = tx_opt else {
        return;
    };
    let Ok(parsed) = parse_service_provider_update(&update) else {
        return;
    };
    let _ = tx.send((parsed, BACKGROUND_CONTEXT.clone()));
}

#[allow(clippy::too_many_arguments)]
fn record_ready_worker(
    inner: &Arc<ManagerInner>,
    peer_id: &str,
    token: &str,
    session_key: &str,
    session_id: &str,
    pid: u32,
    metadata: SessionCatalogMetadata,
    plugin_manifest_paths: Vec<String>,
) {
    if session_key != metadata.path
        || session_id != metadata.id
        || !pi_js::path::is_absolute(&metadata.cwd)
        || !pi_js::path::is_absolute(&metadata.path)
    {
        return;
    }
    mark_discovered(inner, peer_id);

    let (pending_opt, existing_opt) = {
        let state = lock_mutex(&inner.state);
        (
            state.pending.get(session_key).cloned(),
            state.workers_by_session.get(session_key).cloned(),
        )
    };

    if let Some(ref pending) = pending_opt {
        let pg = lock_mutex(pending);
        if pg.peer_id == peer_id && !same_strings(&pg.plugin_manifest_paths, &plugin_manifest_paths) {
            drop(pg);
            fail_pending(
                inner,
                session_key,
                pi_js::Error::msg("Session worker started with stale plugin packages"),
            );
            return;
        }
    }

    if let Some(ref existing) = existing_opt
        && lock_mutex(existing).peer_id != peer_id
    {
        if let Ok(payload) = serde_json::to_value(SessionWorkerCommand::Shutdown) {
            let inner_s = Arc::clone(inner);
            let peer_s = peer_id.to_string();
            tokio::spawn(async move {
                let _ = inner_s.coordinator.send(&peer_s, &payload).await;
            });
        }
        return;
    }

    if let Some(ref pending) = pending_opt {
        let pg = lock_mutex(pending);
        if pg.peer_id != peer_id || pg.token != token || pg.child.pid() != Some(pid) {
            return;
        }
    }

    if existing_opt.is_some() {
        return;
    }

    let (terminated_tx, terminated_rx) = watch::channel(None);
    let worker = Arc::new(Mutex::new(WorkerRecord {
        peer_id: peer_id.to_string(),
        metadata: metadata.clone(),
        pid,
        token: token.to_string(),
        plugin_manifest_paths,
        terminated_tx,
        terminated_rx,
        attachment_ids: IndexSet::new(),
        expected_stop: false,
        stop_started: false,
        stopping: false,
    }));

    let taken_pending = {
        let mut state = lock_mutex(&inner.state);
        state
            .workers_by_session
            .insert(session_key.to_string(), Arc::clone(&worker));
        state.workers_by_peer.insert(peer_id.to_string(), Arc::clone(&worker));
        state.worker_pids.insert(session_id.to_string(), pid);
        state.pending.shift_remove(session_key)
    };

    if let Some(pending) = taken_pending {
        let mut pg = lock_mutex(&pending);
        pg.timer.abort();
        settle_pending_launch(&mut pg, Ok(worker));
    }
    notify_worker_count_changed(inner);
}

fn child_exited(
    inner: &Arc<ManagerInner>,
    pending: &Arc<Mutex<PendingLaunch>>,
    outcome: Option<crate::experimental::process::ExitOutcome>,
) {
    let (session_key, peer_id, is_current_pending) = {
        let state = lock_mutex(&inner.state);
        let pg = lock_mutex(pending);
        let is_cur = state
            .pending
            .get(&pg.session_key)
            .is_some_and(|p| Arc::ptr_eq(p, pending));
        (pg.session_key.clone(), pg.peer_id.clone(), is_cur)
    };
    let detail = outcome
        .and_then(|o| {
            o.signal
                .map(|s| format!("signal {s}"))
                .or_else(|| o.code.map(|c| c.to_string()))
        })
        .unwrap_or_else(|| "unknown".to_string());
    if is_current_pending {
        fail_pending(
            inner,
            &session_key,
            pi_js::Error::msg(format!("Session worker exited before readiness ({detail})")),
        );
        return;
    }
    let worker_opt = lock_mutex(&inner.state).workers_by_peer.get(&peer_id).cloned();
    if let Some(worker) = worker_opt {
        let err = {
            let w = lock_mutex(&worker);
            if w.expected_stop {
                None
            } else {
                Some(pi_js::Error::msg(format!(
                    "Session worker {} exited unexpectedly ({detail})",
                    w.metadata.id
                )))
            }
        };
        remove_worker(inner, &worker, err.as_ref());
    }
}

fn fail_pending(inner: &Arc<ManagerInner>, session_key: &str, error: pi_js::Error) {
    let pending_opt = lock_mutex(&inner.state).pending.shift_remove(session_key);
    let Some(pending) = pending_opt else {
        return;
    };
    let child = {
        let mut pg = lock_mutex(&pending);
        pg.timer.abort();
        let child = pg.child.clone();
        settle_pending_launch(&mut pg, Err(error));
        child
    };
    notify_worker_count_changed(inner);
    if !child.has_exited() {
        kill_child_sigkill(&child);
    }
}

fn kill_child_sigkill(child: &InternalChildProcess) {
    if let Some(pid) = child.pid() {
        kill_pid_sigkill(pid);
    }
}

fn reject_demand(inner: &Arc<ManagerInner>, request_id: &str, error: pi_js::Error) {
    let pending_opt = lock_mutex(&inner.state).pending_demand.shift_remove(request_id);
    if let Some(mut pending) = pending_opt {
        pending.timer.abort();
        if let Some(tx) = pending.tx.take() {
            let _ = tx.send(Err(error));
        }
    }
}

fn reject_operation(inner: &Arc<ManagerInner>, request_id: &str, error: pi_js::Error) {
    let pending_opt = lock_mutex(&inner.state).pending_operations.shift_remove(request_id);
    if let Some(mut pending) = pending_opt {
        drop(pending.abort_unsub.take());
        if let Some(tx) = pending.tx.take() {
            let _ = tx.send(Err(error));
        }
    }
}

fn reject_worker_operations(inner: &Arc<ManagerInner>, worker: &Arc<Mutex<WorkerRecord>>, message: &str) {
    let request_ids: Vec<String> = {
        let state = lock_mutex(&inner.state);
        state
            .pending_operations
            .iter()
            .filter(|(_, p)| Arc::ptr_eq(&p.worker, worker))
            .map(|(id, _)| id.clone())
            .collect()
    };
    for request_id in request_ids {
        reject_operation(inner, &request_id, pi_js::Error::msg(message));
    }
}

fn reconcile_demand_timeout<'a>(
    inner: &'a Arc<ManagerInner>,
    request_id: &'a str,
) -> futures::future::BoxFuture<'a, ()> {
    Box::pin(async move {
        let pending_opt = lock_mutex(&inner.state).pending_demand.shift_remove(request_id);
        let Some(mut pending) = pending_opt else {
            return;
        };
        pending.timer.abort();
        let timeout_error = pi_js::Error::msg("Session worker demand update timed out");
        let tx = pending.tx.take();
        let res = apply_demand(
            inner,
            &pending.worker,
            &pending.attachment_id,
            false,
            false,
            BACKGROUND_CONTEXT.clone(),
        )
        .await;
        match res {
            Ok(()) => {
                if let Some(tx) = tx {
                    if pending.attached {
                        let _ = tx.send(Err(timeout_error));
                    } else {
                        let _ = tx.send(Ok(()));
                    }
                }
            }
            Err(cleanup_error) => match stop_worker(inner, &pending.worker, BACKGROUND_CONTEXT.clone()).await {
                Ok(()) => {
                    if let Some(tx) = tx {
                        let _ = tx.send(Err(pi_js::Error::typed(
                            "AggregateError",
                            AggregateError {
                                message: "Session worker demand reconciliation failed; worker was terminated"
                                    .to_string(),
                                errors: vec![timeout_error, cleanup_error],
                            },
                        )));
                    }
                }
                Err(stop_error) => {
                    if let Some(tx) = tx {
                        let _ = tx.send(Err(pi_js::Error::typed(
                            "AggregateError",
                            AggregateError {
                                message: "Session worker demand reconciliation and termination failed".to_string(),
                                errors: vec![timeout_error, cleanup_error, stop_error],
                            },
                        )));
                    }
                }
            },
        }
    })
}

fn remove_worker(inner: &Arc<ManagerInner>, worker: &Arc<Mutex<WorkerRecord>>, error: Option<&pi_js::Error>) {
    let (peer_id, path, id, pid) = {
        let state = lock_mutex(&inner.state);
        let w = lock_mutex(worker);
        let is_current = state
            .workers_by_peer
            .get(&w.peer_id)
            .is_some_and(|existing| Arc::ptr_eq(existing, worker));
        if !is_current {
            return;
        }
        (w.peer_id.clone(), w.metadata.path.clone(), w.metadata.id.clone(), w.pid)
    };

    reject_worker_operations(inner, worker, "Session worker disconnected during an operation");
    remove_service_subscriptions(inner, |w, _| Arc::ptr_eq(w, worker));
    let demand_ids: Vec<String> = {
        let state = lock_mutex(&inner.state);
        state
            .pending_demand
            .iter()
            .filter(|(_, p)| Arc::ptr_eq(&p.worker, worker))
            .map(|(k, _)| k.clone())
            .collect()
    };
    for req_id in demand_ids {
        reject_demand(
            inner,
            &req_id,
            pi_js::Error::msg("Session worker disconnected during demand update"),
        );
    }

    {
        let mut state = lock_mutex(&inner.state);
        state.workers_by_peer.shift_remove(&peer_id);
        state.workers_by_session.shift_remove(&path);
        if state.worker_pids.get(&id).copied() == Some(pid) {
            state.worker_pids.shift_remove(&id);
        }
    }
    lock_mutex(worker).resolve_terminated(error);
    notify_worker_count_changed(inner);
}

fn remove_service_subscriptions<F>(inner: &Arc<ManagerInner>, matches: F)
where
    F: Fn(&Arc<Mutex<WorkerRecord>>, &WorkerOperationScope) -> bool,
{
    let mut state = lock_mutex(&inner.state);
    let keys: Vec<String> = state
        .service_subscriptions
        .iter()
        .filter(|(_, entry)| matches(&entry.worker, &entry.scope))
        .map(|(k, _)| k.clone())
        .collect();
    for key in keys {
        state.service_subscriptions.shift_remove(&key);
    }
}

fn notify_worker_count_changed(inner: &Arc<ManagerInner>) {
    if let Some(ref cb) = inner.on_worker_count_changed {
        let count = {
            let state = lock_mutex(&inner.state);
            state.workers_by_session.len() + state.pending.len()
        };
        cb(count);
    }
}

fn pending_peer_locked(state: &ManagerState, peer_id: &str) -> Option<Arc<Mutex<PendingLaunch>>> {
    state
        .pending
        .values()
        .find(|p| lock_mutex(p).peer_id == peer_id)
        .cloned()
}

fn mark_discovered(inner: &Arc<ManagerInner>, peer_id: &str) {
    let tx_opt = {
        let mut state = lock_mutex(&inner.state);
        let Some(ref mut peers) = state.discovery_peers else {
            return;
        };
        if !peers.shift_remove(peer_id) || !peers.is_empty() {
            return;
        }
        state.resolve_discovery.take()
    };
    if let Some(tx) = tx_opt {
        let _ = tx.send(());
    }
}

fn detach_state(inner: &Arc<ManagerInner>) {
    drop(lock_mutex(&inner.remove_listener).take());
    let op_ids: Vec<String> = lock_mutex(&inner.state).pending_operations.keys().cloned().collect();
    for request_id in op_ids {
        reject_operation(
            inner,
            &request_id,
            pi_js::Error::msg("Experimental server detached during a worker operation"),
        );
    }
    let (demands, resolve_discovery) = {
        let mut state = lock_mutex(&inner.state);
        let demands: Vec<PendingDemand> = state.pending_demand.drain(..).map(|(_, d)| d).collect();
        state.pending.clear();
        state.service_subscriptions.clear();
        state.workers_by_peer.clear();
        state.workers_by_session.clear();
        state.worker_pids.clear();
        state.discovery_peers = None;
        let resolve = state.resolve_discovery.take();
        (demands, resolve)
    };
    for mut pending in demands {
        pending.timer.abort();
        if let Some(tx) = pending.tx.take() {
            let _ = tx.send(Ok(()));
        }
    }
    if let Some(tx) = resolve_discovery {
        let _ = tx.send(());
    }
}

fn same_strings(left: &[String], right: &[String]) -> bool {
    left == right
}

fn same_scope(left: &WorkerOperationScope, right: &WorkerOperationScope) -> bool {
    left.server_connection_id == right.server_connection_id && left.attachment_id == right.attachment_id
}

fn scoped_service_subscription_key(scope: &WorkerOperationScope, subscription_id: &str) -> String {
    format!(
        "{}\0{}\0{}",
        scope.server_connection_id, scope.attachment_id, subscription_id
    )
}

fn abort_error(signal: &AbortSignal) -> pi_js::Error {
    match signal.reason() {
        Some(reason) => pi_js::Error::Abort(reason),
        None => pi_js::Error::Abort(AbortReason::error("AbortError", "The operation was aborted")),
    }
}
