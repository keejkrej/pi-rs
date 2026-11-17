//! Port of packages/coding-agent/src/experimental/server.ts

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_chord::Context;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::node::FacetBundleArtifact;
use pi_chord::services::errors::AggregateError;
use pi_client::unix::{UnixServerRoute, create_unix_transport_factory};
use pi_client::{Client, ClientOptions, DisconnectedError, ServerError as ClientServerError};
use pi_js::BoxFuture;
use pi_js::crypto::random_uuid;
use pi_js::fs::promises::{chmod, lstat, mkdir, read_to_string, write_file_with_flag};
use pi_js::json::parse;
use pi_js::path::{is_absolute, join};
use pi_js::time::now_ms;
use pi_protocol::{ServerId, is_server_id};
use pi_server::transports::unix::{UnixServerOptions, create_unix_server, get_unix_socket_path};
use pi_server::{
    RoutedServerServiceHost, RoutedSessionHandle, Server, ServerError, ServerHost, ServerOperationErrorCode,
    SessionMetadata, SessionNotFoundError,
};
use serde_json::Value;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::cli::experimental::command_options::AuthInput;
use crate::config::get_agent_dir;
use crate::experimental::coordinator::{
    CoordinatorConnection, CoordinatorConnectionOptions, CoordinatorStartupLease, ensure_coordinator,
};
use crate::experimental::durable::harness_setup::CliModelSelection;
use crate::experimental::plugins::bundled::create_presentation_facet_data;
use crate::experimental::plugins::package::{
    ConfiguredServerPluginPackage, create_server_plugin_package, normalize_plugin_package_paths,
    read_session_plugin_package_profile, remove_session_plugin_package_profile, restore_server_plugin_package_profile,
    write_session_plugin_package_profile,
};
use crate::experimental::process::{
    InternalProcessRole, InternalProcessSpawnOptions, spawn_internal_process, terminate_internal_process,
};
use crate::experimental::radius_auth::RadiusRelayAuthResolver;
use crate::experimental::radius_relay::{RadiusRelayHost, RadiusRelayHostOptions, RadiusRelayHostStatus};
use crate::experimental::services::server::{
    ExperimentalServerServices, PreparedSessionPlugins, ServerServicesBackend, create_experimental_server_services,
};
use crate::experimental::services::sessions::{SessionCreateOptions, SessionSummary};
use crate::experimental::session_catalog::{
    CreateSessionOptions as CatalogCreateSessionOptions, SessionCatalogMetadata,
    create_session as create_catalog_session, delete_session, list_sessions as list_catalog_sessions, read_session,
};
use crate::experimental::session_worker_manager::{
    SessionPluginSelectionConflictError, SessionWorkerManager, WorkerCoordinator,
};
use crate::utils::paths::{PathInputOptions, resolve_path};
use crate::vendor::proper_lockfile::{LockOptions, LockRelease, RetryOptions, lock as acquire_file_lock};

pub const ENV_SERVER_DIR: &str = "PI_SERVER_DIR";
pub const ENV_SERVER_ID: &str = "PI_SERVER_ID";

const LOCK_STALE_MS: u64 = 30_000;
const LOCK_RETRY_MS: u64 = 25;
const LOCK_WAIT_MS: u64 = 30_000;
const DEFAULT_SERVER_ID_FILE: &str = "default-server-id";

const ACTIVATION_TIMEOUT_MS: u64 = 10_000;
const ACTIVATION_RETRY_MS: u64 = 10;

const AUTO_SERVER_STARTUP_GRACE_MS: u64 = 10_000;
const AUTO_SERVER_IDLE_GRACE_MS: u64 = 1_000;

impl SessionMetadata for SessionCatalogMetadata {
    fn id(&self) -> &str {
        &self.id
    }
}

fn lock_mutex<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn resolve_server_directory(directory: Option<&str>) -> String {
    let fallback = format!("{}/.pi/server", pi_js::env::home_dir());
    let raw = directory
        .map(ToString::to_string)
        .or_else(|| pi_js::env::var(ENV_SERVER_DIR))
        .unwrap_or(fallback);
    resolve_path(&raw, None, PathInputOptions::default())
}

pub async fn ensure_private_server_directory(directory: &str) -> pi_js::Result<()> {
    #[cfg(not(unix))]
    {
        let _ = directory;
        return Err(pi_js::Error::msg("Unix socket directory requires a POSIX user ID"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        mkdir(directory, true, Some(0o700)).await?;
        let stats = lstat(directory).await?;
        if !stats.is_directory() {
            return Err(pi_js::Error::msg(format!(
                "Unix socket directory is not a directory: {directory}"
            )));
        }
        let meta = tokio::fs::symlink_metadata(directory)
            .await
            .map_err(|e| pi_js::Error::from_io(e, "lstat", Some(directory), None))?;
        if meta.uid() != nix::unistd::Uid::current().as_raw() {
            return Err(pi_js::Error::msg(format!(
                "Unix socket directory is not owned by the current user: {directory}"
            )));
        }
        chmod(directory, 0o700).await?;
        Ok(())
    }
}

pub fn resolve_session_directory(session_dir: Option<&str>) -> String {
    let fallback = join(&[&get_agent_dir(), "experimental", "sessions"]);
    let raw = session_dir.map(ToString::to_string).unwrap_or(fallback);
    resolve_path(&raw, None, PathInputOptions::default())
}

pub struct ServerProfile {
    pub server_id: ServerId,
    release: Option<LockRelease>,
}

impl ServerProfile {
    pub async fn release(&mut self) -> pi_js::Result<()> {
        if let Some(lock_release) = self.release.take() {
            lock_release.release().await?;
        }
        Ok(())
    }
}

/// Lock one logical server ID in a shared experimental server directory.
pub async fn acquire_server_profile(
    directory: &str,
    requested_server_id: Option<&str>,
) -> pi_js::Result<ServerProfile> {
    mkdir(directory, true, Some(0o700)).await?;
    let server_id = match requested_server_id {
        Some(id) => {
            if !is_server_id(&Value::String(id.to_string())) {
                return Err(pi_js::Error::msg(format!("Invalid experimental server ID: {id}")));
            }
            id.to_string()
        }
        None => {
            let path = join(&[directory, DEFAULT_SERVER_ID_FILE]);
            match read_to_string(&path).await {
                Ok(raw) => {
                    let trimmed = raw.trim().to_string();
                    if !is_server_id(&Value::String(trimmed.clone())) {
                        return Err(pi_js::Error::msg(format!(
                            "Invalid default experimental server identity in {path}"
                        )));
                    }
                    trimmed
                }
                Err(err) if err.code() == Some("ENOENT") => {
                    let candidate = random_uuid();
                    match write_file_with_flag(&path, candidate.as_bytes(), "wx", Some(0o600)).await {
                        Ok(()) => candidate,
                        Err(write_err) if write_err.code() == Some("EEXIST") => {
                            let existing = read_to_string(&path).await?.trim().to_string();
                            if !is_server_id(&Value::String(existing.clone())) {
                                return Err(pi_js::Error::msg(format!(
                                    "Invalid default experimental server identity in {path}"
                                )));
                            }
                            existing
                        }
                        Err(write_err) => return Err(write_err),
                    }
                }
                Err(err) => return Err(err),
            }
        }
    };

    let retries = u32::try_from(LOCK_WAIT_MS.div_ceil(LOCK_RETRY_MS)).unwrap_or(u32::MAX);
    let release = acquire_file_lock(
        &join(&[directory, &format!("launcher-{server_id}")]),
        LockOptions {
            realpath: Some(false),
            stale: Some(LOCK_STALE_MS),
            update: Some(LOCK_STALE_MS / 3),
            retries: Some(RetryOptions {
                retries,
                factor: 1.0,
                min_timeout: LOCK_RETRY_MS,
                max_timeout: LOCK_RETRY_MS,
                randomize: false,
            }),
            lockfile_path: None,
            on_compromised: None,
        },
    )
    .await?;

    Ok(ServerProfile {
        server_id,
        release: Some(release),
    })
}

pub async fn acquire_server_activation(directory: &str, server_id: &ServerId) -> pi_js::Result<LockRelease> {
    let retries = u32::try_from(ACTIVATION_TIMEOUT_MS.div_ceil(25)).unwrap_or(u32::MAX);
    acquire_file_lock(
        &join(&[directory, &format!("activation-{server_id}")]),
        LockOptions {
            realpath: Some(false),
            stale: Some(ACTIVATION_TIMEOUT_MS * 2),
            update: Some(ACTIVATION_TIMEOUT_MS),
            retries: Some(RetryOptions {
                retries,
                factor: 1.0,
                min_timeout: 25,
                max_timeout: 25,
                randomize: false,
            }),
            lockfile_path: None,
            on_compromised: None,
        },
    )
    .await
}

pub struct ActivatedServer {
    pub client: Client,
    pub route: UnixServerRoute,
}

#[derive(Clone, Debug, Default)]
pub struct ActivateServerOptions {
    pub directory: String,
    pub requested_server_id: Option<String>,
    pub session_dir: String,
    pub provider: Option<String>,
    pub model: Option<String>,
}

/// Ensure the selected logical server is reachable, launching the current Pi installation if needed.
pub async fn activate_server(options: ActivateServerOptions) -> pi_js::Result<ActivatedServer> {
    if options.provider.is_some() && options.model.is_none() {
        return Err(pi_js::Error::msg("Server model provider requires a model"));
    }
    ensure_private_server_directory(&options.directory).await?;
    let mut profile = acquire_server_profile(&options.directory, options.requested_server_id.as_deref()).await?;
    let server_id = profile.server_id.clone();
    profile.release().await?;
    let route = UnixServerRoute {
        server_id: server_id.clone(),
        path: get_unix_socket_path(&server_id, &options.directory)?,
    };
    let release = acquire_server_activation(&options.directory, &server_id).await?;

    let outcome = async {
        if let Some(existing) = connect_unix(&route).await? {
            if options.model.is_some() {
                let _ = existing.dispose().await;
                return Err(pi_js::Error::msg(
                    "Model selection is only valid when automatically activating a new server",
                ));
            }
            return Ok(ActivatedServer {
                client: existing,
                route,
            });
        }

        let mut args = vec![
            options.directory.clone(),
            server_id.clone(),
            options.session_dir.clone(),
        ];
        if let Some(ref model) = options.model {
            let mut obj = serde_json::Map::new();
            if let Some(ref provider) = options.provider {
                obj.insert("provider".to_string(), Value::String(provider.clone()));
            }
            obj.insert("model".to_string(), Value::String(model.clone()));
            args.push(pi_js::json::stringify(&Value::Object(obj)));
        }

        let child = spawn_internal_process(
            InternalProcessRole::Server,
            &args,
            &InternalProcessSpawnOptions::default(),
        )?;

        let deadline = now_ms() + ACTIVATION_TIMEOUT_MS as i64;
        let wait_res = async {
            loop {
                if let Some(client) = connect_unix(&route).await? {
                    return Ok(ActivatedServer {
                        client,
                        route: route.clone(),
                    });
                }
                if child.has_exited() {
                    return Err(pi_js::Error::msg(
                        "Automatically activated server exited during startup",
                    ));
                }
                if now_ms() >= deadline {
                    return Err(pi_js::Error::msg(
                        "Timed out waiting for automatically activated server",
                    ));
                }
                tokio::time::sleep(Duration::from_millis(ACTIVATION_RETRY_MS)).await;
            }
        }
        .await;

        match wait_res {
            Ok(activated) => Ok(activated),
            Err(err) => {
                terminate_internal_process(&child).await;
                Err(err)
            }
        }
    }
    .await;

    let release_res = release.release().await;
    match outcome {
        Ok(activated) => {
            release_res?;
            Ok(activated)
        }
        Err(err) => {
            let _ = release_res;
            Err(err)
        }
    }
}

async fn connect_unix(route: &UnixServerRoute) -> pi_js::Result<Option<Client>> {
    let factory = create_unix_transport_factory(pi_client::unix::UnixTransportOptions {
        path: route.path.clone(),
        max_pending_bytes: None,
    })?;
    let res = Client::connect_options(ClientOptions {
        server_id: route.server_id.clone(),
        transport_factory: factory,
        max_frame_length: None,
        on_listener_error: None,
    })
    .await;

    match res {
        Ok(client) => Ok(Some(client)),
        Err(error) => {
            if is_ignorable_connect_error(&error) {
                Ok(None)
            } else {
                Err(error)
            }
        }
    }
}

fn is_ignorable_connect_error(error: &pi_js::Error) -> bool {
    if error.downcast_ref::<DisconnectedError>().is_some() {
        return true;
    }
    if let Some(server_err) = error.downcast_ref::<ClientServerError>()
        && server_err.code == "version"
    {
        return true;
    }
    let mut current: Option<&pi_js::Error> = Some(error);
    let mut depth = 0;
    while let Some(err) = current {
        depth += 1;
        if depth > 16 {
            break;
        }
        if let Some(code) = err.code()
            && matches!(code, "ENOENT" | "ECONNREFUSED" | "ECONNRESET" | "EPIPE" | "ETIMEDOUT")
        {
            return true;
        }
        current = err
            .downcast_ref::<DisconnectedError>()
            .and_then(DisconnectedError::cause)
            .or_else(|| err.cause());
    }
    false
}

struct ServerLifetimeInner {
    keep_alive: bool,
    connection_count: usize,
    worker_count: usize,
    startup_held: bool,
    startup_timer: Option<JoinHandle<()>>,
    retirement_timer: Option<JoinHandle<()>>,
    retire: Option<Arc<dyn Fn() + Send + Sync>>,
    stopped: bool,
}

/// Reconcile operator, startup, client, and worker holds for one server generation.
#[derive(Clone)]
pub struct ServerLifetime {
    inner: Arc<Mutex<ServerLifetimeInner>>,
}

impl ServerLifetime {
    pub fn new(keep_alive: bool) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ServerLifetimeInner {
                keep_alive,
                connection_count: 0,
                worker_count: 0,
                startup_held: !keep_alive,
                startup_timer: None,
                retirement_timer: None,
                retire: None,
                stopped: false,
            })),
        }
    }

    pub fn start(&self, retire: Arc<dyn Fn() + Send + Sync>) {
        let mut inner = lock_mutex(&self.inner);
        inner.retire = Some(retire);
        if inner.startup_held {
            let weak = Arc::downgrade(&self.inner);
            let handle = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(AUTO_SERVER_STARTUP_GRACE_MS)).await;
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let mut guard = lock_mutex(&inner);
                guard.startup_timer = None;
                guard.startup_held = false;
                reconcile_lifetime(&inner, &mut guard);
            });
            inner.startup_timer = Some(handle);
        }
        reconcile_lifetime(&self.inner, &mut inner);
    }

    pub fn set_connection_count(&self, count: usize) {
        let mut inner = lock_mutex(&self.inner);
        inner.connection_count = count;
        if count > 0 && inner.startup_held {
            inner.startup_held = false;
            if let Some(timer) = inner.startup_timer.take() {
                timer.abort();
            }
        }
        reconcile_lifetime(&self.inner, &mut inner);
    }

    pub fn set_worker_count(&self, count: usize) {
        let mut inner = lock_mutex(&self.inner);
        inner.worker_count = count;
        reconcile_lifetime(&self.inner, &mut inner);
    }

    pub fn stop(&self) {
        let mut inner = lock_mutex(&self.inner);
        inner.stopped = true;
        if let Some(t) = inner.startup_timer.take() {
            t.abort();
        }
        if let Some(t) = inner.retirement_timer.take() {
            t.abort();
        }
    }
}

fn reconcile_lifetime(inner_arc: &Arc<Mutex<ServerLifetimeInner>>, inner: &mut ServerLifetimeInner) {
    if inner.stopped || inner.keep_alive || inner.startup_held || inner.connection_count != 0 || inner.worker_count != 0
    {
        if let Some(t) = inner.retirement_timer.take() {
            t.abort();
        }
        return;
    }
    if inner.retirement_timer.is_some() || inner.retire.is_none() {
        return;
    }
    let weak = Arc::downgrade(inner_arc);
    let handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(AUTO_SERVER_IDLE_GRACE_MS)).await;
        let retire = {
            let Some(inner_arc) = weak.upgrade() else {
                return;
            };
            let mut guard = lock_mutex(&inner_arc);
            guard.retirement_timer = None;
            if !guard.stopped && !guard.startup_held && guard.connection_count == 0 && guard.worker_count == 0 {
                guard.retire.clone()
            } else {
                None
            }
        };
        if let Some(cb) = retire {
            cb();
        }
    });
    inner.retirement_timer = Some(handle);
}

#[derive(Clone)]
pub struct RunningServer {
    pub server_id: ServerId,
    pub session_dir: String,
    pub socket_path: String,
    pub server: Server<SessionCatalogMetadata>,
    workers: SessionWorkerManager,
    closed_rx: watch::Receiver<Option<Result<(), String>>>,
    close_fn: Arc<dyn Fn() -> BoxFuture<pi_js::Result<()>> + Send + Sync>,
    close_lock: Arc<tokio::sync::Mutex<()>>,
    close_result: Arc<Mutex<Option<Result<(), String>>>>,
}

impl RunningServer {
    pub fn worker_pids(&self) -> IndexMap<String, u32> {
        self.workers.worker_pids()
    }

    pub fn closed(&self) -> BoxFuture<pi_js::Result<()>> {
        let mut rx = self.closed_rx.clone();
        Box::pin(async move {
            let _ = rx.wait_for(|v| v.is_some()).await;
            let outcome = rx.borrow().clone().unwrap_or(Ok(()));
            outcome.map_err(pi_js::Error::msg)
        })
    }

    pub async fn close(&self) -> pi_js::Result<()> {
        let _guard = self.close_lock.lock().await;
        if let Some(ref outcome) = *lock_mutex(&self.close_result) {
            return outcome.clone().map_err(pi_js::Error::msg);
        }
        let res = (self.close_fn)().await;
        let stored = match &res {
            Ok(()) => Ok(()),
            Err(err) => Err(err.message().to_string()),
        };
        *lock_mutex(&self.close_result) = Some(stored);
        res
    }
}

#[derive(Clone, Default)]
pub struct StartServerOptions {
    pub directory: Option<String>,
    pub server_id: Option<ServerId>,
    pub session_dir: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub keep_alive: Option<bool>,
    pub relay_auth: Option<AuthInput>,
    pub plugin_packages: Option<Vec<String>>,
    pub on_relay_status: Option<Arc<dyn Fn(RadiusRelayHostStatus) + Send + Sync>>,
}

#[derive(Clone)]
struct ResolvedSessionPlugins {
    package_paths: Vec<String>,
    manifest_paths: Vec<String>,
    presentation_artifacts: Vec<FacetBundleArtifact>,
}

type ResolveSessionPluginsFn = Arc<
    dyn Fn(SessionCatalogMetadata, Option<Vec<String>>, Context) -> BoxFuture<pi_js::Result<ResolvedSessionPlugins>>
        + Send
        + Sync,
>;
type RemoveSessionPluginsFn = Arc<dyn Fn(SessionCatalogMetadata) -> BoxFuture<pi_js::Result<()>> + Send + Sync>;
type ReloadPresentationBundlesFn =
    Arc<dyn Fn(Vec<String>) -> BoxFuture<pi_js::Result<Vec<FacetBundleArtifact>>> + Send + Sync>;

struct ExperimentalServerHost {
    server_services_host: Arc<dyn RoutedServerServiceHost>,
    resolve_session_fn: Arc<dyn Fn(&str, Context) -> BoxFuture<pi_js::Result<SessionCatalogMetadata>> + Send + Sync>,
    open_session_fn: Arc<
        dyn Fn(SessionCatalogMetadata, Context) -> BoxFuture<pi_js::Result<Arc<dyn RoutedSessionHandle>>> + Send + Sync,
    >,
}

#[async_trait]
impl ServerHost<SessionCatalogMetadata> for ExperimentalServerHost {
    fn server_services(&self) -> Arc<dyn RoutedServerServiceHost> {
        Arc::clone(&self.server_services_host)
    }

    async fn resolve_session(&self, session_id: &str, context: Context) -> pi_js::Result<SessionCatalogMetadata> {
        (self.resolve_session_fn)(session_id, context).await
    }

    async fn open_session(
        &self,
        metadata: SessionCatalogMetadata,
        context: Context,
    ) -> pi_js::Result<Arc<dyn RoutedSessionHandle>> {
        (self.open_session_fn)(metadata, context).await
    }
}

struct BackendHandle {
    server: Server<SessionCatalogMetadata>,
    session_dir: String,
    services: ExperimentalServerServices,
    closed_rx: watch::Receiver<Option<Result<(), String>>>,
    close_lock: Arc<tokio::sync::Mutex<()>>,
}

impl BackendHandle {
    async fn refresh_sessions(&self) -> pi_js::Result<()> {
        self.services.refresh(Some(BACKGROUND_CONTEXT.clone())).await
    }

    fn closed(&self) -> BoxFuture<pi_js::Result<()>> {
        let mut rx = self.closed_rx.clone();
        Box::pin(async move {
            let _ = rx.wait_for(|v| v.is_some()).await;
            let outcome = rx.borrow().clone().unwrap_or(Ok(()));
            outcome.map_err(pi_js::Error::msg)
        })
    }

    async fn close(&self) -> pi_js::Result<()> {
        let _guard = self.close_lock.lock().await;
        let _ = self.server.close().await;
        self.closed().await
    }
}

async fn start_server_backend(
    path: &str,
    server_id: &ServerId,
    session_dir_opt: Option<&str>,
    resolve_session_plugins: ResolveSessionPluginsFn,
    remove_session_plugins: RemoveSessionPluginsFn,
    reload_presentation_facet_bundles: ReloadPresentationBundlesFn,
    workers: &SessionWorkerManager,
    on_connection_count_changed: Arc<dyn Fn(usize) + Send + Sync>,
) -> pi_js::Result<BackendHandle> {
    let server_id_str = server_id.clone();
    let session_dir = resolve_session_directory(session_dir_opt);

    let list_sessions = {
        let session_dir = session_dir.clone();
        let workers = workers.clone();
        Arc::new(move || {
            let session_dir = session_dir.clone();
            let workers = workers.clone();
            Box::pin(async move {
                let mut sessions: IndexMap<String, SessionCatalogMetadata> = IndexMap::new();
                for meta in list_catalog_sessions(&session_dir).await? {
                    sessions.insert(meta.path.clone(), meta);
                }
                for meta in workers.tracked_sessions() {
                    sessions.insert(meta.path.clone(), meta);
                }
                Ok(sessions.into_values().collect::<Vec<_>>())
            }) as BoxFuture<pi_js::Result<Vec<SessionCatalogMetadata>>>
        })
    };

    let resolve_session = {
        let session_dir = session_dir.clone();
        let workers = workers.clone();
        Arc::new(move |session_id: &str, _ctx: Context| {
            let session_id = session_id.to_string();
            let session_dir = session_dir.clone();
            let workers = workers.clone();
            Box::pin(async move {
                if let Some(found) = workers
                    .tracked_sessions()
                    .into_iter()
                    .find(|candidate| candidate.id == session_id)
                {
                    return Ok(found);
                }
                if let Some(found) = read_session(&session_dir, &session_id).await? {
                    return Ok(found);
                }
                Err(SessionNotFoundError::new(format!("Unknown session: {session_id}")).into_error())
            }) as BoxFuture<pi_js::Result<SessionCatalogMetadata>>
        })
    };

    let summarize = {
        let server_id_str = server_id_str.clone();
        Arc::new(move |metadata: &SessionCatalogMetadata| SessionSummary {
            server_id: server_id_str.clone(),
            session_id: metadata.id.clone(),
            created_at: metadata.created_at,
        })
    };

    let server_services = create_experimental_server_services(ServerServicesBackend {
        list: {
            let list_sessions = Arc::clone(&list_sessions);
            let summarize = Arc::clone(&summarize);
            Arc::new(move |_ctx| {
                let list_sessions = Arc::clone(&list_sessions);
                let summarize = Arc::clone(&summarize);
                Box::pin(async move {
                    let mut out: Vec<SessionSummary> = list_sessions().await?.iter().map(|m| summarize(m)).collect();
                    out.sort_by(|l, r| {
                        l.session_id
                            .cmp(&r.session_id)
                            .then_with(|| l.created_at.cmp(&r.created_at))
                    });
                    Ok(out)
                })
            })
        },
        create: {
            let session_dir = session_dir.clone();
            let summarize = Arc::clone(&summarize);
            Arc::new(move |create_options: SessionCreateOptions, _ctx| {
                let session_dir = session_dir.clone();
                let summarize = Arc::clone(&summarize);
                Box::pin(async move {
                    let cwd = pi_js::env::cwd();
                    let meta = create_catalog_session(
                        &session_dir,
                        CatalogCreateSessionOptions {
                            id: create_options.id,
                            cwd,
                        },
                    )
                    .await?;
                    Ok(summarize(&meta))
                })
            })
        },
        remove: {
            let resolve_session = Arc::clone(&resolve_session);
            let workers = workers.clone();
            let remove_plugins = Arc::clone(&remove_session_plugins);
            Arc::new(move |session_id, ctx| {
                let resolve_session = Arc::clone(&resolve_session);
                let workers = workers.clone();
                let remove_plugins = Arc::clone(&remove_plugins);
                Box::pin(async move {
                    let metadata = resolve_session(&session_id, ctx.clone()).await?;
                    workers.close_session(&metadata, ctx).await?;
                    delete_session(&metadata).await?;
                    remove_plugins(metadata).await
                })
            })
        },
        prepare_session_plugins: {
            let resolve_session = Arc::clone(&resolve_session);
            let resolve_plugins = Arc::clone(&resolve_session_plugins);
            let workers = workers.clone();
            Arc::new(move |session_id, package_paths, ctx| {
                let resolve_session = Arc::clone(&resolve_session);
                let resolve_plugins = Arc::clone(&resolve_plugins);
                let workers = workers.clone();
                Box::pin(async move {
                    let metadata = resolve_session(&session_id, ctx.clone()).await?;
                    let selected = resolve_plugins(metadata.clone(), package_paths, ctx).await?;
                    if let Err(err) = workers.assert_session_plugin_manifest_paths(&metadata, &selected.manifest_paths)
                    {
                        if err.downcast_ref::<SessionPluginSelectionConflictError>().is_some() {
                            return Err(
                                ServerError::new(ServerOperationErrorCode::ServiceInvalidValue, err.message())
                                    .into_error(),
                            );
                        }
                        return Err(err);
                    }
                    Ok(PreparedSessionPlugins {
                        package_paths: selected.package_paths,
                        presentation_plugins: create_presentation_facet_data(&selected.presentation_artifacts),
                    })
                })
            })
        },
        reload_presentation_plugins: {
            let reload = Arc::clone(&reload_presentation_facet_bundles);
            Arc::new(move |package_paths, _ctx| {
                let reload = Arc::clone(&reload);
                Box::pin(async move {
                    let artifacts = reload(package_paths).await?;
                    Ok(create_presentation_facet_data(&artifacts))
                })
            })
        },
    })
    .await?;

    let host: Arc<dyn ServerHost<SessionCatalogMetadata>> = Arc::new(ExperimentalServerHost {
        server_services_host: Arc::clone(&server_services.host),
        resolve_session_fn: resolve_session,
        open_session_fn: {
            let resolve_plugins = Arc::clone(&resolve_session_plugins);
            let workers = workers.clone();
            Arc::new(move |metadata, ctx| {
                let resolve_plugins = Arc::clone(&resolve_plugins);
                let workers = workers.clone();
                Box::pin(async move {
                    let selected = resolve_plugins(metadata.clone(), None, ctx.clone()).await?;
                    workers.open_session(&metadata, ctx, &selected.manifest_paths).await
                })
            })
        },
    });

    let server = create_unix_server(
        host,
        UnixServerOptions {
            server_id: server_id_str,
            path: path.to_string(),
            mode: Some(0o600),
            max_pending_bytes: None,
            graceful_close_timeout_ms: None,
            max_frame_length: None,
            handshake_timeout_ms: None,
            on_connection_count_changed: Some(on_connection_count_changed),
            on_error: None,
        },
    )?;

    if let Err(start_err) = server.start().await {
        let mut cleanup_errors = Vec::new();
        if let Err(e) = server.close().await {
            cleanup_errors.push(e);
        }
        if let Err(e) = server_services.dispose().await {
            cleanup_errors.push(e);
        }
        if !cleanup_errors.is_empty() {
            let mut all = vec![start_err];
            all.extend(cleanup_errors);
            return Err(pi_js::Error::typed(
                "AggregateError",
                AggregateError {
                    message: "Experimental server startup and cleanup failed".to_string(),
                    errors: all,
                },
            ));
        }
        return Err(start_err);
    }

    let (closed_tx, closed_rx) = watch::channel(None);
    let server_for_watch = server.clone();
    let services_for_watch = server_services.clone();
    tokio::spawn(async move {
        let srv_res = server_for_watch.closed().await;
        let svc_res = services_for_watch.dispose().await;
        let combined = match (srv_res, svc_res) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(e), Ok(())) => Err(e.message().to_string()),
            (Ok(()), Err(e)) => Err(e.message().to_string()),
            (Err(e1), Err(e2)) => Err(format!("Server and repository shutdown failed: {e1}; {e2}")),
        };
        let _ = closed_tx.send(Some(combined));
    });

    Ok(BackendHandle {
        server,
        session_dir,
        services: server_services,
        closed_rx,
        close_lock: Arc::new(tokio::sync::Mutex::new(())),
    })
}

/// Start a replaceable experimental server behind the stable coordinator endpoint.
pub async fn start_server(options: StartServerOptions) -> pi_js::Result<RunningServer> {
    if options.provider.is_some() && options.model.is_none() {
        return Err(pi_js::Error::msg("Server model provider requires a model"));
    }
    let worker_model = options.model.as_ref().map(|model| CliModelSelection {
        provider: options.provider.clone(),
        model: model.clone(),
    });
    let directory = resolve_server_directory(options.directory.as_deref());
    let env_server_id = pi_js::env::var(ENV_SERVER_ID);
    let requested_server_id = options.server_id.as_deref().or(env_server_id.as_deref());
    let mut profile = acquire_server_profile(&directory, requested_server_id).await?;
    let server_id = profile.server_id.clone();

    let lifetime = ServerLifetime::new(options.keep_alive.unwrap_or(true));
    let mut coordinator: Option<CoordinatorConnection> = None;
    let mut startup_lease: Option<CoordinatorStartupLease> = None;
    let mut workers: Option<SessionWorkerManager> = None;
    let mut backend: Option<BackendHandle> = None;
    let mut relay: Option<RadiusRelayHost> = None;
    let mut profile_released = false;

    let setup_res = async {
        ensure_private_server_directory(&directory).await?;
        let plugin_package_paths =
            restore_server_plugin_package_profile(&directory, &server_id, options.plugin_packages.as_deref()).await?;

        let plugin_packages: Arc<Mutex<IndexMap<String, ConfiguredServerPluginPackage>>> =
            Arc::new(Mutex::new(IndexMap::new()));
        let get_plugin_package = {
            let plugin_packages = Arc::clone(&plugin_packages);
            let directory = directory.clone();
            let server_id = server_id.clone();
            Arc::new(move |package_path: &str| -> ConfiguredServerPluginPackage {
                let mut map = lock_mutex(&plugin_packages);
                if let Some(existing) = map.get(package_path) {
                    return existing.clone();
                }
                let created = create_server_plugin_package(&directory, &server_id, package_path);
                map.insert(package_path.to_string(), created.clone());
                created
            })
        };

        let build_plugin_selection = {
            let get_pkg = Arc::clone(&get_plugin_package);
            Arc::new(move |package_paths: Vec<String>| {
                let get_pkg = Arc::clone(&get_pkg);
                Box::pin(async move {
                    let normalized = normalize_plugin_package_paths(&package_paths)?;
                    let plugins: Vec<ConfiguredServerPluginPackage> = normalized.iter().map(|p| get_pkg(p)).collect();
                    let built_lists = futures::future::try_join_all(plugins.iter().map(|p| p.build())).await?;
                    let presentation_artifacts: Vec<FacetBundleArtifact> = built_lists.into_iter().flatten().collect();
                    let manifest_paths: Vec<String> = plugins.into_iter().map(|p| p.manifest_path).collect();
                    Ok(ResolvedSessionPlugins {
                        package_paths: normalized,
                        manifest_paths,
                        presentation_artifacts,
                    })
                }) as BoxFuture<pi_js::Result<ResolvedSessionPlugins>>
            })
        };

        let default_plugin_selection = Arc::new(tokio::sync::Mutex::new(
            build_plugin_selection(plugin_package_paths).await?,
        ));
        let session_plugin_selections: Arc<Mutex<IndexMap<String, ResolvedSessionPlugins>>> =
            Arc::new(Mutex::new(IndexMap::new()));

        let workers_slot: Arc<Mutex<Option<SessionWorkerManager>>> = Arc::new(Mutex::new(None));

        let resolve_session_plugins: ResolveSessionPluginsFn = {
            let build_sel = Arc::clone(&build_plugin_selection);
            let default_sel = Arc::clone(&default_plugin_selection);
            let session_sels = Arc::clone(&session_plugin_selections);
            let workers_slot = Arc::clone(&workers_slot);
            let get_pkg = Arc::clone(&get_plugin_package);
            let directory = directory.clone();
            let server_id = server_id.clone();
            Arc::new(move |metadata, requested_paths, _ctx| {
                let build_sel = Arc::clone(&build_sel);
                let default_sel = Arc::clone(&default_sel);
                let session_sels = Arc::clone(&session_sels);
                let workers_slot = Arc::clone(&workers_slot);
                let get_pkg = Arc::clone(&get_pkg);
                let directory = directory.clone();
                let server_id = server_id.clone();
                Box::pin(async move {
                    if let Some(requested) = requested_paths {
                        let normalized = normalize_plugin_package_paths(&requested)?;
                        if let Some(current) = lock_mutex(&session_sels).get(&metadata.path).cloned()
                            && current.package_paths == normalized
                        {
                            return Ok(current);
                        }
                        let requested_manifests: Vec<String> =
                            normalized.iter().map(|p| get_pkg(p).manifest_path).collect();
                        let workers_opt = lock_mutex(&workers_slot).clone();
                        if let Some(ref w) = workers_opt {
                            w.assert_session_plugin_manifest_paths(&metadata, &requested_manifests)?;
                        }
                        let candidate = build_sel(normalized).await?;
                        if let Some(ref w) = workers_opt {
                            w.assert_session_plugin_manifest_paths(&metadata, &candidate.manifest_paths)?;
                        }
                        write_session_plugin_package_profile(
                            &directory,
                            &server_id,
                            &metadata.path,
                            &candidate.package_paths,
                        )
                        .await?;
                        lock_mutex(&session_sels).insert(metadata.path.clone(), candidate.clone());
                        return Ok(candidate);
                    }
                    if let Some(cached) = lock_mutex(&session_sels).get(&metadata.path).cloned() {
                        return Ok(cached);
                    }
                    let stored = read_session_plugin_package_profile(&directory, &server_id, &metadata.path).await?;
                    let selected = match stored {
                        None => {
                            let def = default_sel.lock().await.clone();
                            write_session_plugin_package_profile(
                                &directory,
                                &server_id,
                                &metadata.path,
                                &def.package_paths,
                            )
                            .await?;
                            def
                        }
                        Some(paths) => build_sel(paths).await?,
                    };
                    lock_mutex(&session_sels).insert(metadata.path.clone(), selected.clone());
                    Ok(selected)
                })
            })
        };

        let remove_session_plugins: RemoveSessionPluginsFn = {
            let session_sels = Arc::clone(&session_plugin_selections);
            let directory = directory.clone();
            let server_id = server_id.clone();
            Arc::new(move |metadata| {
                let session_sels = Arc::clone(&session_sels);
                let directory = directory.clone();
                let server_id = server_id.clone();
                Box::pin(async move {
                    lock_mutex(&session_sels).shift_remove(&metadata.path);
                    remove_session_plugin_package_profile(&directory, &server_id, &metadata.path).await
                })
            })
        };

        let reload_presentation_facet_bundles: ReloadPresentationBundlesFn = {
            let build_sel = Arc::clone(&build_plugin_selection);
            let default_sel = Arc::clone(&default_plugin_selection);
            let session_sels = Arc::clone(&session_plugin_selections);
            Arc::new(move |package_paths| {
                let build_sel = Arc::clone(&build_sel);
                let default_sel = Arc::clone(&default_sel);
                let session_sels = Arc::clone(&session_sels);
                Box::pin(async move {
                    let reloaded = build_sel(package_paths).await?;
                    {
                        let mut def = default_sel.lock().await;
                        if def.package_paths == reloaded.package_paths {
                            *def = reloaded.clone();
                        }
                    }
                    {
                        let mut map = lock_mutex(&session_sels);
                        let matching: Vec<String> = map
                            .iter()
                            .filter(|(_, s)| s.package_paths == reloaded.package_paths)
                            .map(|(k, _)| k.clone())
                            .collect();
                        for path in matching {
                            map.insert(path, reloaded.clone());
                        }
                    }
                    Ok(reloaded.presentation_artifacts)
                })
            })
        };

        let socket_path = get_unix_socket_path(&server_id, &directory)?;
        let control_path = join(&[&directory, &format!("control-{server_id}.sock")]);
        let server_nonce: String = random_uuid().chars().filter(|c| *c != '-').take(12).collect();
        let server_path = join(&[&directory, &format!("server-{server_id}-{server_nonce}.sock")]);

        let lease = ensure_coordinator(&socket_path, &control_path).await?;
        startup_lease = Some(lease);

        let coord = CoordinatorConnection::new(CoordinatorConnectionOptions {
            control_path,
            endpoint: server_path.clone(),
            server_connection_id: None,
        });
        coordinator = Some(coord.clone());

        let session_dir = resolve_session_directory(options.session_dir.as_deref());
        let lifetime_workers = lifetime.clone();
        let worker_mgr = SessionWorkerManager::new(
            Arc::new(coord.clone()) as Arc<dyn WorkerCoordinator>,
            session_dir,
            worker_model,
            Some(Arc::new(move |count| lifetime_workers.set_worker_count(count))),
        );
        workers = Some(worker_mgr.clone());
        *lock_mutex(&workers_slot) = Some(worker_mgr.clone());

        let lifetime_conns = lifetime.clone();
        let active_backend = start_server_backend(
            &server_path,
            &server_id,
            options.session_dir.as_deref(),
            resolve_session_plugins,
            remove_session_plugins,
            reload_presentation_facet_bundles,
            &worker_mgr,
            Arc::new(move |count| lifetime_conns.set_connection_count(count)),
        )
        .await?;
        let active_backend_closed_rx = active_backend.closed_rx.clone();
        let active_session_dir = active_backend.session_dir.clone();
        let active_server = active_backend.server.clone();
        backend = Some(active_backend);

        coord.connect().await?;
        if let Some(mut lease) = startup_lease.take() {
            lease.close();
        }
        worker_mgr.discover(&coord.peer_ids()).await?;
        if let Some(ref b) = backend {
            b.refresh_sessions().await?;
        }

        let relay_host = RadiusRelayHost::new(RadiusRelayHostOptions {
            server_id: server_id.clone(),
            server: Arc::new(active_server.clone()),
            auth: Arc::new(RadiusRelayAuthResolver::new(options.relay_auth.clone(), None)),
            web_socket_factory: None,
            on_status: options.on_relay_status.clone(),
        });
        relay_host.start();
        relay = Some(relay_host.clone());

        let backend_arc = Arc::new(
            backend
                .take()
                .ok_or_else(|| pi_js::Error::msg("Missing server backend"))?,
        );
        let coord_for_replace = coord.clone();
        let workers_for_replace = worker_mgr.clone();
        let relay_for_replace = relay_host.clone();
        let backend_for_replace = Arc::clone(&backend_arc);
        let lifetime_for_replace = lifetime.clone();
        tokio::spawn(async move {
            coord_for_replace.replaced().await;
            lifetime_for_replace.stop();
            workers_for_replace.detach();
            let _ = relay_for_replace.close().await;
            let _ = backend_for_replace.close().await;
            coord_for_replace.close();
        });

        let (closed_tx, closed_rx) = watch::channel(None);
        let mut backend_closed_rx_watch = active_backend_closed_rx;
        let relay_for_closed = relay_host.clone();
        tokio::spawn(async move {
            let _ = backend_closed_rx_watch.wait_for(|v| v.is_some()).await;
            let outcome = backend_closed_rx_watch.borrow().clone().unwrap_or(Ok(()));
            let relay_close = relay_for_closed.close().await;
            let final_outcome = match (outcome, relay_close) {
                (Ok(()), Ok(())) => Ok(()),
                (Err(e), Ok(())) => Err(e),
                (Ok(()), Err(e)) => Err(e.message().to_string()),
                (Err(e1), Err(e2)) => Err(format!("{e1}; {e2}")),
            };
            let _ = closed_tx.send(Some(final_outcome));
        });

        let lifetime_close = lifetime.clone();
        let relay_close = relay_host;
        let backend_close = backend_arc;
        let workers_close = worker_mgr.clone();
        let coord_close = coord;
        let close_fn: Arc<dyn Fn() -> BoxFuture<pi_js::Result<()>> + Send + Sync> = Arc::new(move || {
            lifetime_close.stop();
            let relay_close = relay_close.clone();
            let backend_close = Arc::clone(&backend_close);
            let workers_close = workers_close.clone();
            let coord_close = coord_close.clone();
            Box::pin(async move {
                let r1 = relay_close.close().await;
                let r2 = backend_close.close().await;
                let r3 = if coord_close.was_replaced() {
                    workers_close.detach();
                    Ok(())
                } else {
                    workers_close.shutdown().await
                };
                coord_close.close();
                r1?;
                r2?;
                r3
            })
        });

        let runtime = RunningServer {
            server_id: server_id.clone(),
            session_dir: active_session_dir,
            socket_path,
            server: active_server,
            workers: worker_mgr,
            closed_rx,
            close_fn,
            close_lock: Arc::new(tokio::sync::Mutex::new(())),
            close_result: Arc::new(Mutex::new(None)),
        };

        let runtime_retire = runtime.clone();
        lifetime.start(Arc::new(move || {
            let rt = runtime_retire.clone();
            tokio::spawn(async move {
                let _ = rt.close().await;
            });
        }));

        profile_released = true;
        profile.release().await?;
        Ok(runtime)
    }
    .await;

    match setup_res {
        Ok(rt) => Ok(rt),
        Err(err) => {
            lifetime.stop();
            if let Some(mut lease) = startup_lease.take() {
                lease.close();
            }
            let replaced = coordinator.as_ref().is_some_and(CoordinatorConnection::was_replaced);
            if replaced && let Some(ref w) = workers {
                w.detach();
            }
            let mut cleanup_errors = Vec::new();
            if let Some(r) = relay
                && let Err(e) = r.close().await
            {
                cleanup_errors.push(e);
            }
            if let Some(b) = backend
                && let Err(e) = b.close().await
            {
                cleanup_errors.push(e);
            }
            if !replaced
                && let Some(w) = workers
                && let Err(e) = w.shutdown().await
            {
                cleanup_errors.push(e);
            }
            if let Some(c) = coordinator {
                c.close();
            }
            if !profile_released && let Err(e) = profile.release().await {
                cleanup_errors.push(e);
            }
            if !cleanup_errors.is_empty() {
                let mut all = vec![err];
                all.extend(cleanup_errors);
                return Err(pi_js::Error::typed(
                    "AggregateError",
                    AggregateError {
                        message: "Server runtime startup and cleanup failed".to_string(),
                        errors: all,
                    },
                ));
            }
            Err(err)
        }
    }
}

/// Start an operator-held server while serializing against automatic cold activation.
pub async fn start_foreground_server(mut options: StartServerOptions) -> pi_js::Result<RunningServer> {
    let directory = resolve_server_directory(options.directory.as_deref());
    ensure_private_server_directory(&directory).await?;
    let env_server_id = pi_js::env::var(ENV_SERVER_ID);
    let requested = options.server_id.as_deref().or(env_server_id.as_deref());
    let mut profile = acquire_server_profile(&directory, requested).await?;
    let server_id = profile.server_id.clone();
    profile.release().await?;
    let release = acquire_server_activation(&directory, &server_id).await?;
    options.directory = Some(directory);
    options.server_id = Some(server_id);
    options.keep_alive = Some(true);
    let res = start_server(options).await;
    let release_res = release.release().await;
    match res {
        Ok(rt) => {
            release_res?;
            Ok(rt)
        }
        Err(err) => {
            let _ = release_res;
            Err(err)
        }
    }
}

fn parse_server_model_options(value: Option<&str>) -> pi_js::Result<Option<CliModelSelection>> {
    let Some(raw) = value else {
        return Ok(None);
    };
    let parsed = parse(raw)
        .map_err(|err| pi_js::Error::js_with_cause("Error", "Internal server received invalid model options", err))?;
    let Some(obj) = parsed.as_object() else {
        return Err(pi_js::Error::msg("Internal server received invalid model options"));
    };
    if obj.keys().any(|k| k != "provider" && k != "model") {
        return Err(pi_js::Error::msg("Internal server received invalid model options"));
    }
    let Some(model) = obj.get("model").and_then(Value::as_str) else {
        return Err(pi_js::Error::msg("Internal server received invalid model options"));
    };
    if model.is_empty() {
        return Err(pi_js::Error::msg("Internal server received invalid model options"));
    }
    let provider = match obj.get("provider") {
        None => None,
        Some(v) => {
            let Some(p) = v.as_str() else {
                return Err(pi_js::Error::msg("Internal server received invalid model options"));
            };
            if p.is_empty() {
                return Err(pi_js::Error::msg("Internal server received invalid model options"));
            }
            Some(p.to_string())
        }
    };
    Ok(Some(CliModelSelection {
        provider,
        model: model.to_string(),
    }))
}

/// Run an automatically activated server until its client and Session demand disappears.
pub async fn run_server_process(args: &[String]) -> pi_js::Result<()> {
    if args.len() > 4 {
        return Err(pi_js::Error::msg("Internal server received unexpected arguments"));
    }
    let directory = args
        .first()
        .filter(|s| !s.is_empty() && is_absolute(s))
        .ok_or_else(|| pi_js::Error::msg("Internal server requires an absolute server directory"))?
        .clone();
    let server_id = args
        .get(1)
        .filter(|s| is_server_id(&Value::String((*s).clone())))
        .ok_or_else(|| pi_js::Error::msg("Internal server requires a canonical server ID"))?
        .clone();
    let session_dir = args
        .get(2)
        .filter(|s| !s.is_empty() && is_absolute(s))
        .ok_or_else(|| pi_js::Error::msg("Internal server requires an absolute Session directory"))?
        .clone();
    let worker_model = parse_server_model_options(args.get(3).map(String::as_str))?;

    let runtime = start_server(StartServerOptions {
        directory: Some(directory),
        server_id: Some(server_id),
        session_dir: Some(session_dir),
        provider: worker_model.as_ref().and_then(|m| m.provider.clone()),
        model: worker_model.map(|m| m.model),
        keep_alive: Some(false),
        relay_auth: None,
        plugin_packages: None,
        on_relay_status: None,
    })
    .await?;

    let close_rt = runtime.clone();
    #[cfg(unix)]
    let wait_res = {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigint = signal(SignalKind::interrupt()).ok();
        let mut sigterm = signal(SignalKind::terminate()).ok();
        tokio::select! {
            res = runtime.closed() => res,
            _ = async {
                if let Some(ref mut s) = sigint {
                    let _ = s.recv().await;
                } else {
                    futures::future::pending::<()>().await;
                }
            } => {
                let _ = close_rt.close().await;
                Ok(())
            }
            _ = async {
                if let Some(ref mut s) = sigterm {
                    let _ = s.recv().await;
                } else {
                    futures::future::pending::<()>().await;
                }
            } => {
                let _ = close_rt.close().await;
                Ok(())
            }
        }
    };
    #[cfg(not(unix))]
    let wait_res = runtime.closed().await;

    let close_res = runtime.close().await;
    wait_res?;
    close_res
}
