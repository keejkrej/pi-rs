//! Port of packages/coding-agent/src/experimental/services/worker.ts

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use indexmap::IndexMap;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::services::errors::AggregateError;
use pi_chord::{
    Context, Facet, FacetHost, FacetLoader, JsonValue, RemoteServiceEndpoint, RemoteServiceProvider, ServiceCall,
    ServiceProviderUpdate, ServiceUpdatePublisher, create_facet_host, create_remote_service_endpoint,
    create_static_facet_loader, define_facet,
};
use pi_durable::{Conversation, Harness};
use pi_js::BoxFuture;
use serde::{Deserialize, Serialize};

use crate::core::model_runtime::ModelRuntime;
use crate::core::settings_manager::SettingsManager;
use crate::experimental::services::agent_controller::AGENT_CONTROLLER_SERVICE;
use crate::experimental::services::agent_controller_provider::create_agent_controller_service_object;
use crate::experimental::services::models_provider::{ModelsServiceFacetOptions, create_models_service_facet};
use crate::experimental::services::plugins::{
    SESSION_PLUGINS_SERVICE, SessionPluginsApi, session_plugins_to_service_object,
};
use crate::experimental::services::transcript_provider::create_transcript_service_facet;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub type CleanupFn = Arc<dyn Fn(Context) -> BoxFuture<pi_js::Result<()>> + Send + Sync>;

pub struct SessionWorkerRuntime {
    pub harness: Harness,
    /// The root conversation the services expose.
    pub conversation: Conversation,
    pub model_runtime: Option<Arc<ModelRuntime>>,
    pub settings_manager: Option<Arc<SettingsManager>>,
    pub facet_loader: Option<FacetLoader>,
    /// Release resources the Harness does not own, such as execution environments, after it closed.
    pub cleanup: Option<CleanupFn>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerServiceScope {
    pub server_connection_id: String,
    pub attachment_id: String,
}

pub type WorkerPublishFn =
    Arc<dyn Fn(WorkerServiceScope, String, ServiceProviderUpdate) -> BoxFuture<pi_js::Result<()>> + Send + Sync>;

pub struct CreateSessionWorkerServicesOptions {
    pub harness: Harness,
    pub conversation: Conversation,
    pub model_runtime: Option<Arc<ModelRuntime>>,
    pub settings_manager: Option<Arc<SettingsManager>>,
    pub facet_loader: Option<FacetLoader>,
    pub publish: WorkerPublishFn,
}

struct ScopedServiceEndpoint {
    scope: WorkerServiceScope,
    endpoint: RemoteServiceEndpoint,
}

pub struct SessionWorkerServices {
    facet_host: FacetHost,
    provider: RemoteServiceProvider,
    endpoints: Mutex<IndexMap<String, ScopedServiceEndpoint>>,
    loaded_plugins: tokio::sync::Mutex<pi_chord::LoadedFacets>,
    builtins: pi_chord::LoadedFacets,
    reload_lock: tokio::sync::Mutex<()>,
    publish: WorkerPublishFn,
}

impl SessionWorkerServices {
    pub async fn invoke(
        &self,
        call: &ServiceCall,
        scope: WorkerServiceScope,
        context: Context,
    ) -> pi_js::Result<Option<JsonValue>> {
        let key = service_scope_key(&scope);
        let endpoint = {
            let mut map = lock(&self.endpoints);
            if let Some(existing) = map.get(&key) {
                existing.endpoint.clone()
            } else {
                let endpoint = create_remote_service_endpoint(&self.provider);
                map.insert(
                    key,
                    ScopedServiceEndpoint {
                        scope: scope.clone(),
                        endpoint: endpoint.clone(),
                    },
                );
                endpoint
            }
        };
        let publish_fn = Arc::clone(&self.publish);
        let publisher: ServiceUpdatePublisher =
            Arc::new(move |subscription_id, update, _ctx| publish_fn(scope.clone(), subscription_id, update));
        endpoint.invoke(call, publisher, context).await
    }

    pub fn remove_subscriptions<F>(&self, matches: F)
    where
        F: Fn(&WorkerServiceScope) -> bool,
    {
        let mut map = lock(&self.endpoints);
        let to_remove: Vec<String> = map
            .iter()
            .filter(|(_, entry)| matches(&entry.scope))
            .map(|(k, _)| k.clone())
            .collect();
        for key in to_remove {
            if let Some(entry) = map.shift_remove(&key) {
                entry.endpoint.dispose();
            }
        }
    }

    pub async fn dispose(&self) -> pi_js::Result<()> {
        self.remove_subscriptions(|_| true);
        let _reload_guard = self.reload_lock.lock().await;
        let mut errors = Vec::new();
        if let Err(err) = self.facet_host.dispose().await {
            errors.push(err);
        }
        let loaded = self.loaded_plugins.lock().await;
        if let Err(err) = loaded.dispose().await {
            errors.push(err);
        }
        if let Err(err) = self.builtins.dispose().await {
            errors.push(err);
        }
        match errors.len() {
            0 => Ok(()),
            1 => Err(errors
                .into_iter()
                .next()
                .unwrap_or_else(|| pi_js::Error::msg("Dispose error"))),
            _ => Err(pi_js::Error::typed(
                "AggregateError",
                AggregateError {
                    message: "Failed to dispose Session facets".to_string(),
                    errors,
                },
            )),
        }
    }
}

struct SessionPluginsApiImpl {
    reload_fn: Arc<dyn Fn() -> BoxFuture<pi_js::Result<()>> + Send + Sync>,
}

#[async_trait::async_trait]
impl SessionPluginsApi for SessionPluginsApiImpl {
    async fn reload(&self, _context: Context) -> pi_js::Result<()> {
        (self.reload_fn)().await
    }
}

pub async fn create_session_worker_services(
    options: CreateSessionWorkerServicesOptions,
) -> pi_js::Result<Arc<SessionWorkerServices>> {
    let harness_ctrl = options.harness.clone();
    let conv_ctrl = options.conversation.clone();
    let agent_controller_runtime_facet = define_facet(Facet::new(
        "@pi/agent-controller-runtime",
        Arc::new(move |env| {
            let obj = create_agent_controller_service_object(harness_ctrl.clone(), conv_ctrl.clone());
            env.provide(&*AGENT_CONTROLLER_SERVICE, obj)
        }),
    ));

    let reload_slot: Arc<Mutex<Option<Arc<dyn Fn() -> BoxFuture<pi_js::Result<()>> + Send + Sync>>>> =
        Arc::new(Mutex::new(None));
    let reload_slot_ref = Arc::clone(&reload_slot);
    let plugin_runtime_facet = define_facet(Facet::new(
        "@pi/session-plugins-runtime",
        Arc::new(move |env| {
            let slot = Arc::clone(&reload_slot_ref);
            let api: Arc<dyn SessionPluginsApi> = Arc::new(SessionPluginsApiImpl {
                reload_fn: Arc::new(move || {
                    let fn_opt = lock(&slot).clone();
                    Box::pin(async move {
                        let Some(f) = fn_opt else {
                            return Err(pi_js::Error::msg("Session plugins are not ready"));
                        };
                        f().await
                    })
                }),
            });
            env.provide(&*SESSION_PLUGINS_SERVICE, session_plugins_to_service_object(api))
        }),
    ));

    let models_facet = create_models_service_facet(ModelsServiceFacetOptions {
        harness: options.harness,
        conversation: options.conversation.clone(),
        model_runtime: options.model_runtime,
        settings_manager: options.settings_manager,
        context: BACKGROUND_CONTEXT.clone(),
    })
    .await?;
    let transcript_facet = create_transcript_service_facet(options.conversation, BACKGROUND_CONTEXT.clone()).await?;

    let builtins = create_static_facet_loader(vec![
        agent_controller_runtime_facet,
        plugin_runtime_facet,
        models_facet,
        transcript_facet,
    ])
    .load()
    .await?;

    let plugin_loader = options
        .facet_loader
        .unwrap_or_else(|| create_static_facet_loader(Vec::new()));
    let loaded_plugins = match plugin_loader.load().await {
        Ok(loaded) => loaded,
        Err(err) => {
            let _ = builtins.dispose().await;
            return Err(err);
        }
    };

    let mut all_facets = builtins.facets().to_vec();
    all_facets.extend(loaded_plugins.facets().iter().cloned());

    let facet_host = match create_facet_host(pi_chord::FacetOptions {
        facets: all_facets,
        service_sources: None,
        on_error: None,
    })
    .await
    {
        Ok(host) => host,
        Err(err) => {
            let mut cleanup_errors = Vec::new();
            if let Err(e) = loaded_plugins.dispose().await {
                cleanup_errors.push(e);
            }
            if let Err(e) = builtins.dispose().await {
                cleanup_errors.push(e);
            }
            if !cleanup_errors.is_empty() {
                let mut all = vec![err];
                all.extend(cleanup_errors);
                return Err(pi_js::Error::typed(
                    "AggregateError",
                    AggregateError {
                        message: "Session facets failed to start and clean up".to_string(),
                        errors: all,
                    },
                ));
            }
            return Err(err);
        }
    };

    let provider = facet_host.services();
    let services = Arc::new(SessionWorkerServices {
        facet_host,
        provider,
        endpoints: Mutex::new(IndexMap::new()),
        loaded_plugins: tokio::sync::Mutex::new(loaded_plugins),
        builtins,
        reload_lock: tokio::sync::Mutex::new(()),
        publish: options.publish,
    });

    let services_weak = Arc::downgrade(&services);
    *lock(&reload_slot) = Some(Arc::new(move || {
        let services_weak = services_weak.clone();
        let plugin_loader = plugin_loader.clone();
        Box::pin(async move {
            let Some(services) = services_weak.upgrade() else {
                return Err(pi_js::Error::msg("Session worker services are disposed"));
            };
            let _reload_guard = services.reload_lock.lock().await;
            let candidate = plugin_loader.load().await?;
            if let Err(err) = services.facet_host.reload(candidate.facets().to_vec()).await {
                if let Err(cleanup_err) = candidate.dispose().await {
                    return Err(pi_js::Error::typed(
                        "AggregateError",
                        AggregateError {
                            message: "Session plugin reload and cleanup failed".to_string(),
                            errors: vec![err, cleanup_err],
                        },
                    ));
                }
                return Err(err);
            }
            let retired = {
                let mut guard = services.loaded_plugins.lock().await;
                std::mem::replace(&mut *guard, candidate)
            };
            retired.dispose().await
        })
    }));

    Ok(services)
}

fn service_scope_key(scope: &WorkerServiceScope) -> String {
    format!("{}\0{}", scope.server_connection_id, scope.attachment_id)
}
