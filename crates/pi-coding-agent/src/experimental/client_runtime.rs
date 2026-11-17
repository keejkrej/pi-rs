//! Port of packages/coding-agent/src/experimental/client-runtime.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::services::errors::AggregateError;
use pi_chord::services::handle::ServiceObject;
use pi_chord::types::{RemoteServiceSourceOpenOptions, ServiceIdRef};
use pi_chord::{Context, RemoteServices};
use pi_client::unix::{
    DiscoverUnixServersOptions, UnixServerRoute, UnixTransportOptions, create_unix_transport_factory,
    discover_unix_servers,
};
use pi_client::{Client, ClientOptions, ServerError};
use pi_js::path::basename;
use pi_protocol::{ServerId, is_server_id};
use serde_json::Value;

use crate::cli::experimental::command_options::TransportAddress;
use crate::cli::experimental::commands::client::ClientCommand;
use crate::experimental::radius_auth::RadiusRelayAuthResolver;
use crate::experimental::radius_relay::{
    RadiusClientReconnect, RadiusClientTransportFactoryOptions, create_radius_client_transport_factory,
};
use crate::experimental::server::{
    ActivateServerOptions, ENV_SERVER_ID, activate_server, resolve_server_directory, resolve_session_directory,
};
use crate::experimental::services::agent_controller::{AGENT_CONTROLLER_SERVICE, AgentController};
use crate::experimental::services::connection::{
    ServerServiceSource, ServiceSourceOptions, SessionServiceSource, create_server_service_source,
    create_session_service_source,
};
use crate::experimental::services::models::{MODELS_SERVICE, Models};
use crate::experimental::services::plugins::{PRESENTATION_PLUGINS_SERVICE, PresentationPlugins};
use crate::experimental::services::sessions::{
    SESSION_DIRECTORY_SERVICE, SESSION_MANAGEMENT_SERVICE, SessionCreateOptions, SessionDirectory, SessionManagement,
    SessionSummary,
};
use crate::experimental::services::transcript::{TRANSCRIPT_SERVICE, Transcript};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientRuntimeRoute {
    Unix { server_id: ServerId, path: String },
    Radius { server_id: ServerId },
}

impl ClientRuntimeRoute {
    pub fn server_id(&self) -> &ServerId {
        match self {
            Self::Unix { server_id, .. } | Self::Radius { server_id } => server_id,
        }
    }

    pub fn is_radius(&self) -> bool {
        matches!(self, Self::Radius { .. })
    }
}

#[derive(Clone)]
pub struct ClientRuntimeServer {
    pub route: ClientRuntimeRoute,
    pub client: Client,
    pub server: ServerServiceSource,
    pub session: SessionServiceSource,
}

#[derive(Clone)]
pub struct ActivatedSessionManagement {
    remote: SessionManagement,
    client: Client,
    session: SessionServiceSource,
}

impl ActivatedSessionManagement {
    pub async fn create(&self, options: SessionCreateOptions, context: Context) -> pi_js::Result<SessionSummary> {
        self.remote.create(options, context).await
    }

    pub async fn remove(&self, session_id: &str, context: Context) -> pi_js::Result<()> {
        let removes_current = self
            .client
            .attachment()
            .as_ref()
            .is_some_and(|a| a.session_id == session_id);
        self.remote.remove(session_id, context.clone()).await?;
        if removes_current {
            self.session.when_detached(context).await?;
        }
        Ok(())
    }

    pub async fn attach(&self, session_id: &str, context: Context) -> pi_js::Result<()> {
        self.remote.attach(session_id, context.clone()).await?;
        self.session.when_attached(session_id, context).await
    }

    pub async fn detach(&self, context: Context) -> pi_js::Result<()> {
        self.remote.detach(context.clone()).await?;
        self.session.when_detached(context).await
    }
}

#[derive(Clone)]
pub struct ActivatedClientRuntimeServer {
    pub route: ClientRuntimeRoute,
    pub client: Client,
    pub server: ServerServiceSource,
    pub session: SessionServiceSource,
    pub directory: SessionDirectory,
    pub management: ActivatedSessionManagement,
    pub plugins: PresentationPlugins,
    pub models: Models,
    pub agent: AgentController,
    pub transcript: Transcript,
    _server_services: RemoteServices,
    _session_services: RemoteServices,
}

#[derive(Clone)]
pub struct ClientRuntime {
    pub servers: Vec<ClientRuntimeServer>,
    inner: Arc<ClientRuntimeDisposeState>,
}

struct ClientRuntimeDisposeState {
    clients: Vec<Client>,
    reconnectors: Vec<RadiusClientReconnect>,
    server_sources: Vec<ServerServiceSource>,
    session_sources: Vec<SessionServiceSource>,
    disposed: AtomicBool,
    dispose_lock: tokio::sync::Mutex<()>,
}

impl ClientRuntime {
    pub async fn dispose(&self) -> pi_js::Result<()> {
        let _guard = self.inner.dispose_lock.lock().await;
        if self.inner.disposed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let mut errors = Vec::new();
        for reconnector in &self.inner.reconnectors {
            if let Err(err) = reconnector.dispose().await {
                errors.push(err);
            }
        }
        for source in &self.inner.server_sources {
            if let Err(err) = source.dispose(BACKGROUND_CONTEXT.clone()).await {
                errors.push(err);
            }
        }
        for source in &self.inner.session_sources {
            if let Err(err) = source.dispose(BACKGROUND_CONTEXT.clone()).await {
                errors.push(err);
            }
        }
        for client in &self.inner.clients {
            if let Err(err) = client.dispose().await {
                errors.push(err);
            }
        }
        match errors.len() {
            0 => Ok(()),
            1 => Err(errors
                .into_iter()
                .next()
                .unwrap_or_else(|| pi_js::Error::msg("Dispose failed"))),
            _ => Err(pi_js::Error::typed(
                "AggregateError",
                AggregateError {
                    message: "Failed to dispose experimental client runtime".to_string(),
                    errors,
                },
            )),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct OpenClientRuntimeOptions {
    pub directory: Option<String>,
}

/// Open live server/session service namespaces for one experimental presentation.
pub async fn open_client_runtime(
    command: &ClientCommand,
    options: OpenClientRuntimeOptions,
) -> pi_js::Result<ClientRuntime> {
    if command.auth.is_some() && !matches!(command.connect, Some(TransportAddress::Radius { .. })) {
        return Err(pi_js::Error::msg(
            "Authentication is only supported for experimental Radius connections",
        ));
    }
    if command.provider.is_some() && command.model.is_none() {
        return Err(pi_js::Error::msg("Server model provider requires a model"));
    }
    if command.connect.is_some() && command.model.is_some() {
        return Err(pi_js::Error::msg(
            "Model selection is only valid when automatically activating a new server",
        ));
    }
    if matches!(command.connect, Some(TransportAddress::Radius { .. })) && !command.plugin_packages.is_empty() {
        return Err(pi_js::Error::msg(
            "Plugin package paths can only be configured on a local Unix server",
        ));
    }

    let directory = resolve_server_directory(options.directory.as_deref());
    let mut activated_client: Option<Client> = None;
    let routes: Vec<ClientRuntimeRoute> = if let Some(ref connect) = command.connect {
        match connect {
            TransportAddress::Radius { server_id } => vec![ClientRuntimeRoute::Radius {
                server_id: server_id.clone(),
            }],
            TransportAddress::Unix { path } => {
                let route = route_from_explicit_path(path)?;
                vec![ClientRuntimeRoute::Unix {
                    server_id: route.server_id,
                    path: route.path,
                }]
            }
        }
    } else {
        let discovered = discover_unix_servers(DiscoverUnixServersOptions {
            directory: directory.clone(),
            timeout_ms: None,
        })
        .await?;
        if !discovered.is_empty() && command.model.is_some() {
            return Err(pi_js::Error::msg(
                "Model selection is only valid when automatically activating a new server",
            ));
        }
        if discovered.is_empty() {
            let activated = activate_server(ActivateServerOptions {
                directory: directory.clone(),
                requested_server_id: pi_js::env::var(ENV_SERVER_ID),
                session_dir: resolve_session_directory(None),
                provider: command.provider.clone(),
                model: command.model.clone(),
            })
            .await?;
            activated_client = Some(activated.client);
            vec![ClientRuntimeRoute::Unix {
                server_id: activated.route.server_id,
                path: activated.route.path,
            }]
        } else {
            discovered
                .into_iter()
                .map(|r| ClientRuntimeRoute::Unix {
                    server_id: r.server_id,
                    path: r.path,
                })
                .collect()
        }
    };

    if !command.plugin_packages.is_empty() && routes.len() != 1 {
        return Err(pi_js::Error::msg("Plugin selection requires exactly one local server"));
    }

    let mut clients: Vec<Client> = Vec::new();
    let mut reconnectors: Vec<RadiusClientReconnect> = Vec::new();
    let mut server_sources: Vec<ServerServiceSource> = Vec::new();
    let mut session_sources: Vec<SessionServiceSource> = Vec::new();
    let mut servers: Vec<ClientRuntimeServer> = Vec::new();

    let setup_res: pi_js::Result<()> = async {
        for route in &routes {
            let client = if let Some(c) = activated_client.take() {
                c
            } else {
                let transport_factory = match route {
                    ClientRuntimeRoute::Unix { path, .. } => create_unix_transport_factory(UnixTransportOptions {
                        path: path.clone(),
                        max_pending_bytes: None,
                    })?,
                    ClientRuntimeRoute::Radius { server_id } => {
                        create_radius_client_transport_factory(RadiusClientTransportFactoryOptions {
                            server_id: server_id.clone(),
                            auth: Arc::new(RadiusRelayAuthResolver::new(command.auth.clone(), None)),
                            web_socket_factory: None,
                        })
                    }
                };
                match Client::connect_options(ClientOptions {
                    server_id: route.server_id().clone(),
                    transport_factory,
                    max_frame_length: None,
                    on_listener_error: None,
                })
                .await
                {
                    Ok(c) => c,
                    Err(err) => {
                        let version_mismatch = command.connect.is_none()
                            && matches!(route, ClientRuntimeRoute::Unix { .. })
                            && err.downcast_ref::<ServerError>().is_some_and(|e| e.code == "version");
                        if !version_mismatch {
                            return Err(err);
                        }
                        activate_server(ActivateServerOptions {
                            directory: directory.clone(),
                            requested_server_id: Some(route.server_id().clone()),
                            session_dir: resolve_session_directory(None),
                            provider: None,
                            model: None,
                        })
                        .await?
                        .client
                    }
                }
            };

            clients.push(client.clone());
            let server = create_server_service_source(client.clone(), ServiceSourceOptions::default())?;
            server_sources.push(server.clone());

            if matches!(route, ClientRuntimeRoute::Radius { .. }) {
                let reconnect_services = server.open(RemoteServiceSourceOpenOptions {
                    services: vec![ServiceIdRef {
                        id: SESSION_MANAGEMENT_SERVICE.id.clone(),
                    }],
                    assert_access: Arc::new(|| Ok(())),
                    on_error: Arc::new(|_| {}),
                });
                let reconnect_mgmt_obj = reconnect_services.r#use::<ServiceObject>(&*SESSION_MANAGEMENT_SERVICE)?;
                let reconnect_mgmt = SessionManagement::from_service(reconnect_mgmt_obj);
                let reconnector = RadiusClientReconnect::new(
                    Arc::new(client.clone()),
                    Arc::new(move |session_id| {
                        let reconnect_services = reconnect_services.clone();
                        let reconnect_mgmt = reconnect_mgmt.clone();
                        Box::pin(async move {
                            reconnect_services.ready(BACKGROUND_CONTEXT.clone()).await?;
                            reconnect_mgmt.attach(&session_id, BACKGROUND_CONTEXT.clone()).await
                        })
                    }),
                )?;
                reconnectors.push(reconnector);
            }

            let session = create_session_service_source(client.clone(), ServiceSourceOptions::default())?;
            session_sources.push(session.clone());
            servers.push(ClientRuntimeServer {
                route: route.clone(),
                client,
                server,
                session,
            });
        }
        Ok(())
    }
    .await;

    let runtime = ClientRuntime {
        servers,
        inner: Arc::new(ClientRuntimeDisposeState {
            clients,
            reconnectors,
            server_sources,
            session_sources,
            disposed: AtomicBool::new(false),
            dispose_lock: tokio::sync::Mutex::new(()),
        }),
    };

    if let Err(err) = setup_res {
        if let Err(cleanup_err) = runtime.dispose().await {
            return Err(pi_js::Error::typed(
                "AggregateError",
                AggregateError {
                    message: "Experimental client startup and cleanup failed".to_string(),
                    errors: vec![err, cleanup_err],
                },
            ));
        }
        return Err(err);
    }

    Ok(runtime)
}

/// Acquire and connect the built-in service facades used by the non-interactive client.
pub async fn activate_builtin_client_services(
    server: &ClientRuntimeServer,
) -> pi_js::Result<ActivatedClientRuntimeServer> {
    let server_services = server.server.open(RemoteServiceSourceOpenOptions {
        services: vec![
            ServiceIdRef {
                id: SESSION_DIRECTORY_SERVICE.id.clone(),
            },
            ServiceIdRef {
                id: SESSION_MANAGEMENT_SERVICE.id.clone(),
            },
            ServiceIdRef {
                id: PRESENTATION_PLUGINS_SERVICE.id.clone(),
            },
        ],
        assert_access: Arc::new(|| Ok(())),
        on_error: Arc::new(|_| {}),
    });
    let session_services = server.session.open(RemoteServiceSourceOpenOptions {
        services: vec![
            ServiceIdRef {
                id: MODELS_SERVICE.id.clone(),
            },
            ServiceIdRef {
                id: AGENT_CONTROLLER_SERVICE.id.clone(),
            },
            ServiceIdRef {
                id: TRANSCRIPT_SERVICE.id.clone(),
            },
        ],
        assert_access: Arc::new(|| Ok(())),
        on_error: Arc::new(|_| {}),
    });

    let directory =
        SessionDirectory::from_service(server_services.r#use::<ServiceObject>(&*SESSION_DIRECTORY_SERVICE)?)?;
    let remote_management =
        SessionManagement::from_service(server_services.r#use::<ServiceObject>(&*SESSION_MANAGEMENT_SERVICE)?);
    let management = ActivatedSessionManagement {
        remote: remote_management,
        client: server.client.clone(),
        session: server.session.clone(),
    };
    let plugins =
        PresentationPlugins::from_service(server_services.r#use::<ServiceObject>(&*PRESENTATION_PLUGINS_SERVICE)?);
    let models = Models::from_service(session_services.r#use::<ServiceObject>(&*MODELS_SERVICE)?)?;
    let agent = AgentController::from_service(session_services.r#use::<ServiceObject>(&*AGENT_CONTROLLER_SERVICE)?);
    let transcript = Transcript::from_service(session_services.r#use::<ServiceObject>(&*TRANSCRIPT_SERVICE)?)?;

    tokio::try_join!(
        server_services.ready(BACKGROUND_CONTEXT.clone()),
        session_services.ready(BACKGROUND_CONTEXT.clone())
    )?;

    Ok(ActivatedClientRuntimeServer {
        route: server.route.clone(),
        client: server.client.clone(),
        server: server.server.clone(),
        session: server.session.clone(),
        directory,
        management,
        plugins,
        models,
        agent,
        transcript,
        _server_services: server_services,
        _session_services: session_services,
    })
}

fn route_from_explicit_path(path: &str) -> pi_js::Result<UnixServerRoute> {
    let name = basename(path, None);
    let server_id = name.strip_suffix(".sock").unwrap_or("");
    if !is_server_id(&Value::String(server_id.to_string())) {
        return Err(pi_js::Error::msg(
            "--connect path must end with <uuidv4-server-id>.sock",
        ));
    }
    Ok(UnixServerRoute {
        server_id: server_id.to_string(),
        path: path.to_string(),
    })
}
