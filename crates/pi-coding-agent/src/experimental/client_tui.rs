//! Port of packages/coding-agent/src/experimental/client-tui.ts

use std::sync::Arc;

use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::services::handle::ServiceObject;
use pi_chord::types::{RemoteServiceSourceOpenOptions, ServiceIdRef};
use pi_chord::{FacetLoader, JsonValue};
use pi_js::path::resolve;

use crate::cli::experimental::command_options::TransportAddress;
use crate::cli::experimental::commands::client::ClientCommand;
use crate::experimental::services::connection::{ServerServiceSource, SessionServiceSource};
use crate::experimental::services::plugins::{
    PRESENTATION_PLUGINS_SERVICE, PrepareSessionPluginsRequest, PresentationPlugins,
};
use crate::experimental::services::sessions::{
    SESSION_DIRECTORY_SERVICE, SESSION_MANAGEMENT_SERVICE, SessionCreateOptions, SessionDirectory, SessionManagement,
    SessionSummary,
};

#[derive(Clone, Default)]
pub struct RunClientTuiOptions {
    pub directory: Option<String>,
    pub facet_loader: Option<FacetLoader>,
}

#[derive(Clone)]
pub struct ClientTuiServer {
    pub server_id: String,
    pub radius: bool,
    pub server: ServerServiceSource,
    pub session: SessionServiceSource,
}

#[derive(Clone)]
pub struct PreparedClientSession {
    pub server: ClientTuiServer,
    pub summary: SessionSummary,
    pub presentation_plugins: JsonValue,
}

pub async fn prepare_client_session(
    command: &ClientCommand,
    servers: &[ClientTuiServer],
) -> pi_js::Result<PreparedClientSession> {
    let opened: Vec<(&ClientTuiServer, pi_chord::RemoteServices)> = servers
        .iter()
        .map(|server| {
            let services = server.server.open(RemoteServiceSourceOpenOptions {
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
            (server, services)
        })
        .collect();

    struct ServerFeature<'a> {
        server: &'a ClientTuiServer,
        directory: SessionDirectory,
        management: SessionManagement,
        plugins: PresentationPlugins,
    }

    let outcome = async {
        let mut features = Vec::with_capacity(opened.len());
        for (server, services) in &opened {
            let directory =
                SessionDirectory::from_service(services.r#use::<ServiceObject>(&*SESSION_DIRECTORY_SERVICE)?)?;
            let management =
                SessionManagement::from_service(services.r#use::<ServiceObject>(&*SESSION_MANAGEMENT_SERVICE)?);
            let plugins =
                PresentationPlugins::from_service(services.r#use::<ServiceObject>(&*PRESENTATION_PLUGINS_SERVICE)?);
            features.push(ServerFeature {
                server,
                directory,
                management,
                plugins,
            });
        }
        futures::future::try_join_all(
            opened
                .iter()
                .map(|(_, services)| services.ready(BACKGROUND_CONTEXT.clone())),
        )
        .await?;

        struct Selected<'a> {
            server: &'a ClientTuiServer,
            management: &'a SessionManagement,
            plugins: &'a PresentationPlugins,
            summary: SessionSummary,
        }

        let mut selected: Option<Selected<'_>> = None;
        if let Some(ref requested_id) = command.session_id {
            let mut matches: Vec<Selected<'_>> = Vec::new();
            for feature in &features {
                if let Some(state) = feature.directory.state.value() {
                    for summary in state.sessions {
                        if &summary.session_id == requested_id {
                            matches.push(Selected {
                                server: feature.server,
                                management: &feature.management,
                                plugins: &feature.plugins,
                                summary,
                            });
                        }
                    }
                }
            }
            if matches.len() > 1 {
                return Err(pi_js::Error::msg(format!(
                    "Session {requested_id} is available from more than one server"
                )));
            }
            if let Some(found) = matches.into_iter().next() {
                selected = Some(found);
            } else {
                if matches!(command.connect, Some(TransportAddress::Radius { .. })) {
                    return Err(pi_js::Error::msg(format!(
                        "Remote server does not contain Session {requested_id}"
                    )));
                }
                let feature = require_single_server(&features)?;
                let summary = feature
                    .management
                    .create(
                        SessionCreateOptions {
                            id: Some(requested_id.clone()),
                        },
                        BACKGROUND_CONTEXT.clone(),
                    )
                    .await?;
                selected = Some(Selected {
                    server: feature.server,
                    management: &feature.management,
                    plugins: &feature.plugins,
                    summary,
                });
            }
        } else if command.continue_ || command.resume {
            let mut candidates: Vec<Selected<'_>> = Vec::new();
            for feature in &features {
                if let Some(state) = feature.directory.state.value() {
                    for summary in state.sessions {
                        candidates.push(Selected {
                            server: feature.server,
                            management: &feature.management,
                            plugins: &feature.plugins,
                            summary,
                        });
                    }
                }
            }
            candidates.sort_by(|l, r| {
                r.summary
                    .created_at
                    .cmp(&l.summary.created_at)
                    .then_with(|| l.summary.server_id.cmp(&r.summary.server_id))
                    .then_with(|| l.summary.session_id.cmp(&r.summary.session_id))
            });
            selected = candidates.into_iter().next();
        }

        let selected = match selected {
            Some(sel) => sel,
            None => {
                let feature = require_single_server(&features)?;
                let summary = feature
                    .management
                    .create(SessionCreateOptions::default(), BACKGROUND_CONTEXT.clone())
                    .await?;
                Selected {
                    server: feature.server,
                    management: &feature.management,
                    plugins: &feature.plugins,
                    summary,
                }
            }
        };

        let package_paths = if command.plugin_packages.is_empty() {
            None
        } else {
            Some(command.plugin_packages.iter().map(|p| resolve(&[p])).collect())
        };
        let presentation_plugins = selected
            .plugins
            .prepare_session(
                PrepareSessionPluginsRequest {
                    session_id: selected.summary.session_id.clone(),
                    package_paths,
                },
                BACKGROUND_CONTEXT.clone(),
            )
            .await?;

        selected
            .management
            .attach(&selected.summary.session_id, BACKGROUND_CONTEXT.clone())
            .await?;
        selected
            .server
            .session
            .when_attached(&selected.summary.session_id, BACKGROUND_CONTEXT.clone())
            .await?;

        Ok(PreparedClientSession {
            server: selected.server.clone(),
            summary: selected.summary,
            presentation_plugins,
        })
    }
    .await;

    for (_, services) in opened {
        let _ = services.dispose(BACKGROUND_CONTEXT.clone()).await;
    }

    outcome
}

pub struct ExperimentalClientTui;

impl ExperimentalClientTui {
    pub async fn create(_command: &ClientCommand, _servers: &[ClientTuiServer]) -> pi_js::Result<Self> {
        // WIRE(@tui-w3): connect modes::interactive::{chat_viewport::create_chat_viewport,
        // components::CustomEditor, theme::*, tui_renderer::create_interactive_tui}
        // once @tui lands src/modes/interactive/**.
        Err(pi_js::Error::msg(
            "ExperimentalClientTui requires modes::interactive widgets from @tui (@tui-w3)",
        ))
    }
}

pub async fn run_client_tui(_command: &ClientCommand, _options: RunClientTuiOptions) -> pi_js::Result<()> {
    // WIRE(@tui-w3): connect modes::interactive::{chat_viewport, components, theme, theme_controller,
    // tui_renderer} once @tui lands src/modes/interactive/**.
    Err(pi_js::Error::msg(
        "Client TUI requires modes::interactive widgets from @tui (@tui-w3)",
    ))
}

fn require_single_server<T>(features: &[T]) -> pi_js::Result<&T> {
    if features.len() != 1 {
        return Err(pi_js::Error::msg("Starting a Session requires exactly one server"));
    }
    Ok(&features[0])
}
