//! Port of packages/coding-agent/test/experimental-service-binding.ts

use std::sync::Arc;

use futures::FutureExt;
use futures::future::Shared;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::services::errors::AggregateError;
use pi_chord::services::handle::ServiceObject;
use pi_chord::types::{RemoteServiceSourceOpenOptions, ServiceIdRef};
use pi_chord::{Context, RemoteServices, Service, ServiceCatalogueEntry};
use pi_client::Client;
use pi_js::{BoxFuture, Unsubscribe};

use pi_coding_agent::experimental::services::connection::{
    ServerConnectionState, ServerServiceSource, ServiceSourceOptions, SessionAttachmentState, SessionServiceSource,
    TypedReplicatedState, create_server_service_source, create_session_service_source,
};

#[derive(Clone, Default)]
pub struct ServiceBindingOptions {
    pub services: Vec<ServiceIdRef>,
    pub on_error: Option<Arc<dyn Fn(pi_js::Error) + Send + Sync>>,
}

#[derive(Clone)]
pub struct ServerServiceBinding {
    pub source: ServerServiceSource,
    pub services: RemoteServices,
    pub connection: TypedReplicatedState<ServerConnectionState>,
    activation: Shared<BoxFuture<Result<(), String>>>,
}

impl ServerServiceBinding {
    pub fn accepts_unavailable_services(&self) -> bool {
        self.source.as_remote_source().accepts_unavailable_services()
    }

    pub async fn catalogue(&self, context: Context) -> pi_js::Result<Vec<ServiceCatalogueEntry>> {
        self.source.catalogue(context).await
    }

    pub fn open(&self, options: RemoteServiceSourceOpenOptions) -> RemoteServices {
        self.source.open(options)
    }

    pub fn r#use<T: Send + Sync + 'static>(&self, service: &Service) -> pi_js::Result<T> {
        self.services.r#use::<T>(service)
    }

    pub fn observe<F>(&self, service: &Service, handler: F) -> pi_js::Result<Unsubscribe>
    where
        F: Fn(ServiceObject, Context) -> BoxFuture<pi_js::Result<()>> + Send + Sync + 'static,
    {
        self.services.observe::<ServiceObject>(service, Arc::new(handler))
    }

    pub async fn ready(&self, context: Context) -> pi_js::Result<()> {
        if let Err(err) = self.activation.clone().await {
            return Err(pi_js::Error::msg(err));
        }
        self.services.ready(context).await
    }

    pub async fn dispose(&self, context: Context) -> pi_js::Result<()> {
        let mut errors = Vec::new();
        if let Err(err) = self.services.dispose(context.clone()).await {
            errors.push(err);
        }
        if let Err(err) = self.source.dispose(context).await {
            errors.push(err);
        }
        throw_failures(errors, "Failed to dispose server service binding")
    }
}

/// Test helper for binding server services without a facet host.
pub fn create_server_service_binding(
    client: Client,
    options: ServiceBindingOptions,
) -> pi_js::Result<ServerServiceBinding> {
    let source = create_server_service_source(
        client,
        ServiceSourceOptions {
            on_error: options.on_error.clone(),
        },
    )?;
    let on_error = options.on_error.unwrap_or_else(|| Arc::new(|_| {}));
    let services = source.open(RemoteServiceSourceOpenOptions {
        services: options.services,
        assert_access: Arc::new(|| Ok(())),
        on_error: Arc::clone(&on_error),
    });
    let services_for_act = services.clone();
    let on_error_act = Arc::clone(&on_error);
    let activation = async move {
        if let Err(err) = services_for_act.ready(BACKGROUND_CONTEXT.clone()).await {
            let msg = err.message().to_string();
            on_error_act(err);
            return Err(msg);
        }
        Ok(())
    }
    .boxed()
    .shared();
    let connection = source.connection.clone();
    Ok(ServerServiceBinding {
        source,
        services,
        connection,
        activation,
    })
}

#[derive(Clone)]
pub struct SessionServiceBinding {
    pub source: SessionServiceSource,
    pub services: RemoteServices,
    pub attachment: TypedReplicatedState<SessionAttachmentState>,
    activation: Shared<BoxFuture<Result<(), String>>>,
}

impl SessionServiceBinding {
    pub fn accepts_unavailable_services(&self) -> bool {
        self.source.as_remote_source().accepts_unavailable_services()
    }

    pub async fn catalogue(&self, context: Context) -> pi_js::Result<Vec<ServiceCatalogueEntry>> {
        self.source.catalogue(context).await
    }

    pub fn open(&self, options: RemoteServiceSourceOpenOptions) -> RemoteServices {
        self.source.open(options)
    }

    pub fn r#use<T: Send + Sync + 'static>(&self, service: &Service) -> pi_js::Result<T> {
        self.services.r#use::<T>(service)
    }

    pub fn observe<F>(&self, service: &Service, handler: F) -> pi_js::Result<Unsubscribe>
    where
        F: Fn(ServiceObject, Context) -> BoxFuture<pi_js::Result<()>> + Send + Sync + 'static,
    {
        self.services.observe::<ServiceObject>(service, Arc::new(handler))
    }

    pub async fn ready(&self, context: Context) -> pi_js::Result<()> {
        if let Err(err) = self.activation.clone().await {
            return Err(pi_js::Error::msg(err));
        }
        let attachment = self.source.attachment.value();
        match attachment {
            Some(SessionAttachmentState::Detached) => self.source.when_detached(context).await,
            Some(SessionAttachmentState::Attaching { session_id })
            | Some(SessionAttachmentState::Attached { session_id })
            | Some(SessionAttachmentState::Degraded { session_id }) => {
                self.source.when_attached(&session_id, context).await
            }
            None => Ok(()),
        }
    }

    pub async fn when_attached(&self, session_id: &str, context: Context) -> pi_js::Result<()> {
        self.source.when_attached(session_id, context).await
    }

    pub async fn when_detached(&self, context: Context) -> pi_js::Result<()> {
        self.source.when_detached(context).await
    }

    pub async fn dispose(&self, context: Context) -> pi_js::Result<()> {
        let mut errors = Vec::new();
        if let Err(err) = self.services.dispose(context.clone()).await {
            errors.push(err);
        }
        if let Err(err) = self.source.dispose(context).await {
            errors.push(err);
        }
        throw_failures(errors, "Failed to dispose Session service binding")
    }
}

/// Test helper for binding selected-Session services without a facet host.
pub fn create_session_service_binding(
    client: Client,
    options: ServiceBindingOptions,
) -> pi_js::Result<SessionServiceBinding> {
    let source = create_session_service_source(
        client,
        ServiceSourceOptions {
            on_error: options.on_error.clone(),
        },
    )?;
    let on_error = options.on_error.unwrap_or_else(|| Arc::new(|_| {}));
    let services = source.open(RemoteServiceSourceOpenOptions {
        services: options.services,
        assert_access: Arc::new(|| Ok(())),
        on_error: Arc::clone(&on_error),
    });
    let services_for_act = services.clone();
    let on_error_act = Arc::clone(&on_error);
    let activation = async move {
        if let Err(err) = services_for_act.ready(BACKGROUND_CONTEXT.clone()).await {
            let msg = err.message().to_string();
            on_error_act(err);
            return Err(msg);
        }
        Ok(())
    }
    .boxed()
    .shared();
    let attachment = source.attachment.clone();
    Ok(SessionServiceBinding {
        source,
        services,
        attachment,
        activation,
    })
}

fn throw_failures(errors: Vec<pi_js::Error>, message: &str) -> pi_js::Result<()> {
    match errors.len() {
        0 => Ok(()),
        1 => Err(errors.into_iter().next().unwrap_or_else(|| pi_js::Error::msg(message))),
        _ => Err(pi_js::Error::typed(
            "AggregateError",
            AggregateError {
                message: message.to_string(),
                errors,
            },
        )),
    }
}
