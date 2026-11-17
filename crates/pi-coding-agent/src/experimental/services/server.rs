//! Port of packages/coding-agent/src/experimental/services/server.ts

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::services::errors::AggregateError;
use pi_chord::services::handle::ServiceObject;
use pi_chord::services::provider::RemoteServiceProviderEntry;
use pi_chord::{
    Context, JsonValue, RemoteServiceEndpoint, RemoteServiceProvider, ServiceCall, ServiceMode, ServiceUpdatePublisher,
    create_remote_service_endpoint,
};
use pi_js::BoxFuture;
use pi_server::{RoutedServerPresentation, RoutedServerServiceAttachment, RoutedServerServiceHost};

use crate::experimental::services::connection::TypedReplicatedState;
use crate::experimental::services::plugins::{
    PRESENTATION_PLUGINS_SERVICE, PrepareSessionPluginsRequest, PresentationPluginsApi,
    presentation_plugins_to_service_object,
};
use crate::experimental::services::sessions::{
    SESSION_DIRECTORY_SERVICE, SESSION_MANAGEMENT_SERVICE, SessionCreateOptions, SessionDirectory,
    SessionDirectoryState, SessionManagementApi, SessionSummary, session_management_to_service_object,
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone)]
pub struct PreparedSessionPlugins {
    pub package_paths: Vec<String>,
    pub presentation_plugins: JsonValue,
}

pub struct ServerServicesBackend {
    pub list: Arc<dyn Fn(Context) -> BoxFuture<pi_js::Result<Vec<SessionSummary>>> + Send + Sync>,
    pub create: Arc<dyn Fn(SessionCreateOptions, Context) -> BoxFuture<pi_js::Result<SessionSummary>> + Send + Sync>,
    pub remove: Arc<dyn Fn(String, Context) -> BoxFuture<pi_js::Result<()>> + Send + Sync>,
    pub prepare_session_plugins: Arc<
        dyn Fn(String, Option<Vec<String>>, Context) -> BoxFuture<pi_js::Result<PreparedSessionPlugins>> + Send + Sync,
    >,
    pub reload_presentation_plugins:
        Arc<dyn Fn(Vec<String>, Context) -> BoxFuture<pi_js::Result<JsonValue>> + Send + Sync>,
}

#[derive(Clone)]
pub struct ExperimentalServerServices {
    pub host: Arc<dyn RoutedServerServiceHost>,
    inner: Arc<ServerServicesInner>,
}

struct ServerServicesInner {
    backend: Arc<ServerServicesBackend>,
    directory: TypedReplicatedState<SessionDirectoryState>,
    revision: Mutex<i64>,
    attachments: Mutex<IndexMap<u64, Arc<ServerAttachment>>>,
    next_attachment_id: Mutex<u64>,
    mutation_lock: tokio::sync::Mutex<()>,
}

impl ExperimentalServerServices {
    pub async fn refresh(&self, context: Option<Context>) -> pi_js::Result<()> {
        let _guard = self.inner.mutation_lock.lock().await;
        refresh_now(&self.inner, context.unwrap_or_else(|| BACKGROUND_CONTEXT.clone())).await
    }

    pub async fn dispose(&self) -> pi_js::Result<()> {
        let attachments: Vec<Arc<ServerAttachment>> = {
            let mut guard = lock(&self.inner.attachments);
            guard.drain(..).map(|(_, a)| a).collect()
        };
        let mut errors = Vec::new();
        for att in attachments {
            if let Err(err) = att.release(BACKGROUND_CONTEXT.clone()).await {
                errors.push(err);
            }
        }
        let _guard = self.inner.mutation_lock.lock().await;
        match errors.len() {
            0 => Ok(()),
            1 => Err(errors
                .into_iter()
                .next()
                .unwrap_or_else(|| pi_js::Error::msg("Dispose error"))),
            _ => Err(pi_js::Error::typed(
                "AggregateError",
                AggregateError {
                    message: "Failed to release server service attachments".to_string(),
                    errors,
                },
            )),
        }
    }
}

pub async fn create_experimental_server_services(
    backend: ServerServicesBackend,
) -> pi_js::Result<ExperimentalServerServices> {
    let backend = Arc::new(backend);
    let initial_sessions = (backend.list)(BACKGROUND_CONTEXT.clone()).await?;
    let initial_state = SessionDirectoryState {
        revision: 1,
        sessions: initial_sessions,
    };
    let directory = TypedReplicatedState::new_local(&initial_state)?;
    let inner = Arc::new(ServerServicesInner {
        backend,
        directory,
        revision: Mutex::new(1),
        attachments: Mutex::new(IndexMap::new()),
        next_attachment_id: Mutex::new(1),
        mutation_lock: tokio::sync::Mutex::new(()),
    });
    let host: Arc<dyn RoutedServerServiceHost> = Arc::new(ServerServiceHostImpl {
        inner: Arc::clone(&inner),
    });
    Ok(ExperimentalServerServices { host, inner })
}

async fn refresh_now(inner: &Arc<ServerServicesInner>, context: Context) -> pi_js::Result<()> {
    let sessions = (inner.backend.list)(context.clone()).await?;
    let next_rev = {
        let mut rev = lock(&inner.revision);
        *rev += 1;
        *rev
    };
    inner.directory.change(context, |draft| {
        draft.revision = next_rev;
        draft.sessions = sessions;
        Ok(())
    })
}

struct ServerServiceHostImpl {
    inner: Arc<ServerServicesInner>,
}

#[async_trait]
impl RoutedServerServiceHost for ServerServiceHostImpl {
    async fn attach_client(
        &self,
        presentation: Arc<dyn RoutedServerPresentation>,
        _context: Context,
    ) -> pi_js::Result<Arc<dyn RoutedServerServiceAttachment>> {
        let prepared_packages: Arc<Mutex<Option<Vec<String>>>> = Arc::new(Mutex::new(None));
        let provider = RemoteServiceProvider::new(vec![
            RemoteServiceProviderEntry::Definition {
                service: SESSION_DIRECTORY_SERVICE.clone(),
                mode: ServiceMode::Singleton,
            },
            RemoteServiceProviderEntry::Definition {
                service: SESSION_MANAGEMENT_SERVICE.clone(),
                mode: ServiceMode::Singleton,
            },
            RemoteServiceProviderEntry::Definition {
                service: PRESENTATION_PLUGINS_SERVICE.clone(),
                mode: ServiceMode::Singleton,
            },
        ])?;

        let dir_service = SessionDirectory::from_state(self.inner.directory.clone()).into_service_object();
        provider.provide(&*SESSION_DIRECTORY_SERVICE, Arc::new(dir_service))?;

        let plugins_api: Arc<dyn PresentationPluginsApi> = Arc::new(PresentationPluginsImpl {
            inner: Arc::clone(&self.inner),
            prepared_packages: Arc::clone(&prepared_packages),
        });
        provider.provide(
            &*PRESENTATION_PLUGINS_SERVICE,
            Arc::new(presentation_plugins_to_service_object(plugins_api)),
        )?;

        let mgmt_api: Arc<dyn SessionManagementApi> = Arc::new(SessionManagementImpl {
            inner: Arc::clone(&self.inner),
            presentation,
            prepared_packages,
        });
        provider.provide(
            &*SESSION_MANAGEMENT_SERVICE,
            Arc::new(session_management_to_service_object(mgmt_api)),
        )?;

        let id = {
            let mut next = lock(&self.inner.next_attachment_id);
            let id = *next;
            *next += 1;
            id
        };
        let endpoint = create_remote_service_endpoint(&provider);
        let attachment = Arc::new(ServerAttachment {
            id,
            inner: Arc::clone(&self.inner),
            endpoint,
            provider,
            released: AtomicBool::new(false),
        });
        lock(&self.inner.attachments).insert(id, Arc::clone(&attachment));
        Ok(attachment)
    }
}

struct PresentationPluginsImpl {
    inner: Arc<ServerServicesInner>,
    prepared_packages: Arc<Mutex<Option<Vec<String>>>>,
}

#[async_trait]
impl PresentationPluginsApi for PresentationPluginsImpl {
    async fn prepare_session(
        &self,
        request: PrepareSessionPluginsRequest,
        context: Context,
    ) -> pi_js::Result<JsonValue> {
        let _guard = self.inner.mutation_lock.lock().await;
        let selected =
            (self.inner.backend.prepare_session_plugins)(request.session_id, request.package_paths, context).await?;
        *lock(&self.prepared_packages) = Some(selected.package_paths);
        Ok(selected.presentation_plugins)
    }

    async fn reload(&self, context: Context) -> pi_js::Result<JsonValue> {
        let _guard = self.inner.mutation_lock.lock().await;
        let paths = lock(&self.prepared_packages)
            .clone()
            .ok_or_else(|| pi_js::Error::msg("No Session plugin selection is prepared"))?;
        (self.inner.backend.reload_presentation_plugins)(paths, context).await
    }
}

struct SessionManagementImpl {
    inner: Arc<ServerServicesInner>,
    presentation: Arc<dyn RoutedServerPresentation>,
    prepared_packages: Arc<Mutex<Option<Vec<String>>>>,
}

#[async_trait]
impl SessionManagementApi for SessionManagementImpl {
    async fn create(&self, options: SessionCreateOptions, context: Context) -> pi_js::Result<SessionSummary> {
        let _guard = self.inner.mutation_lock.lock().await;
        let created = (self.inner.backend.create)(options, context.clone()).await?;
        refresh_now(&self.inner, context).await?;
        Ok(created)
    }

    async fn remove(&self, session_id: String, context: Context) -> pi_js::Result<()> {
        let _guard = self.inner.mutation_lock.lock().await;
        self.presentation
            .prepare_session_removal(&session_id, context.clone())
            .await?;
        (self.inner.backend.remove)(session_id, context.clone()).await?;
        refresh_now(&self.inner, context).await
    }

    async fn attach(&self, session_id: String, context: Context) -> pi_js::Result<()> {
        let _guard = self.inner.mutation_lock.lock().await;
        self.presentation.attach_session(&session_id, context).await
    }

    async fn detach(&self, context: Context) -> pi_js::Result<()> {
        let _guard = self.inner.mutation_lock.lock().await;
        self.presentation.detach_session(context).await?;
        *lock(&self.prepared_packages) = None;
        Ok(())
    }
}

struct ServerAttachment {
    id: u64,
    inner: Arc<ServerServicesInner>,
    endpoint: RemoteServiceEndpoint,
    provider: RemoteServiceProvider,
    released: AtomicBool,
}

#[async_trait]
impl RoutedServerServiceAttachment for ServerAttachment {
    async fn invoke_service(
        &self,
        call: &ServiceCall,
        publish: ServiceUpdatePublisher,
        context: Context,
    ) -> pi_js::Result<Option<JsonValue>> {
        if self.released.load(Ordering::Acquire) {
            return Err(pi_js::Error::msg("Server service attachment is released"));
        }
        self.endpoint.invoke(call, publish, context).await
    }

    async fn release(&self, _context: Context) -> pi_js::Result<()> {
        if self.released.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.endpoint.dispose();
        self.provider.dispose();
        lock(&self.inner.attachments).shift_remove(&self.id);
        Ok(())
    }
}

pub fn _unused_service_object(_: &ServiceObject) {}
