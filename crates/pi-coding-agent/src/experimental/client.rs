//! Port of packages/coding-agent/src/experimental/client.ts

use pi_chord::context::BACKGROUND_CONTEXT;
use pi_js::path::resolve;

use crate::cli::experimental::command_options::TransportAddress;
use crate::cli::experimental::commands::client::ClientCommand;
use crate::experimental::client_runtime::{
    ActivatedClientRuntimeServer, OpenClientRuntimeOptions, activate_builtin_client_services, open_client_runtime,
};
use crate::experimental::services::agent_controller::{AgentPromptRequest, AgentPromptResult};
use crate::experimental::services::plugins::PrepareSessionPluginsRequest;
use crate::experimental::services::sessions::{SessionAddress, SessionCreateOptions};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientResult {
    List {
        sessions: Vec<SessionAddress>,
    },
    Attached {
        server_id: String,
        session_id: String,
    },
    Prompted {
        server_id: String,
        session_id: String,
        text: String,
    },
}

#[derive(Clone, Debug, Default)]
pub struct RunClientOptions {
    pub directory: Option<String>,
}

/// Discover servers, then list Sessions, attach to one, or create one for a prompt.
pub async fn run_client(command: &ClientCommand, options: RunClientOptions) -> pi_js::Result<ClientResult> {
    let runtime = open_client_runtime(
        command,
        OpenClientRuntimeOptions {
            directory: options.directory,
        },
    )
    .await?;

    let outcome = async {
        let discovered: Vec<ActivatedClientRuntimeServer> =
            futures::future::try_join_all(runtime.servers.iter().map(activate_builtin_client_services)).await?;

        if command.session_id.is_none() && command.prompt.is_none() {
            let mut sessions = Vec::new();
            for item in &discovered {
                if let Some(state) = item.directory.state.value() {
                    for s in state.sessions {
                        sessions.push(SessionAddress {
                            server_id: item.route.server_id().clone(),
                            session_id: s.session_id,
                        });
                    }
                }
            }
            sessions.sort_by(|l, r| {
                l.server_id
                    .cmp(&r.server_id)
                    .then_with(|| l.session_id.cmp(&r.session_id))
            });
            return Ok(ClientResult::List { sessions });
        }

        let (selected, session_id) = match command.session_id {
            None => {
                if discovered.len() != 1 {
                    return Err(pi_js::Error::msg(
                        "Client prompt requires exactly one discovered server to create a Session",
                    ));
                }
                let target = &discovered[0];
                let summary = target
                    .management
                    .create(SessionCreateOptions::default(), BACKGROUND_CONTEXT.clone())
                    .await?;
                (target, summary.session_id)
            }
            Some(ref requested_id) => {
                let matches: Vec<&ActivatedClientRuntimeServer> = discovered
                    .iter()
                    .filter(|candidate| {
                        candidate
                            .directory
                            .state
                            .value()
                            .is_some_and(|st| st.sessions.iter().any(|s| &s.session_id == requested_id))
                    })
                    .collect();
                if matches.len() > 1 {
                    return Err(pi_js::Error::msg(format!(
                        "Session {requested_id} is available from more than one server"
                    )));
                }
                if let Some(&existing) = matches.first() {
                    (existing, requested_id.clone())
                } else {
                    if matches!(command.connect, Some(TransportAddress::Radius { .. }))
                        || command.prompt.is_none()
                        || discovered.len() != 1
                    {
                        return Err(pi_js::Error::msg(format!(
                            "No discovered server contains session {requested_id}"
                        )));
                    }
                    let target = &discovered[0];
                    target
                        .management
                        .create(
                            SessionCreateOptions {
                                id: Some(requested_id.clone()),
                            },
                            BACKGROUND_CONTEXT.clone(),
                        )
                        .await?;
                    (target, requested_id.clone())
                }
            }
        };

        let package_paths = if command.plugin_packages.is_empty() {
            None
        } else {
            Some(command.plugin_packages.iter().map(|p| resolve(&[p])).collect())
        };
        selected
            .plugins
            .prepare_session(
                PrepareSessionPluginsRequest {
                    session_id: session_id.clone(),
                    package_paths,
                },
                BACKGROUND_CONTEXT.clone(),
            )
            .await?;

        selected
            .management
            .attach(&session_id, BACKGROUND_CONTEXT.clone())
            .await?;

        let Some(ref prompt) = command.prompt else {
            return Ok(ClientResult::Attached {
                server_id: selected.route.server_id().clone(),
                session_id,
            });
        };

        let response = selected
            .agent
            .prompt(
                AgentPromptRequest {
                    message: prompt.clone(),
                    images: None,
                },
                BACKGROUND_CONTEXT.clone(),
            )
            .await?;
        if !response.accepted {
            let msg = response
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "Agent prompt rejected".to_string());
            return Err(pi_js::Error::msg(msg));
        }
        let op_id = response
            .operation_id
            .ok_or_else(|| pi_js::Error::msg("Accepted prompt did not return an operation ID"))?;
        let result = selected
            .agent
            .wait_for_prompt(&op_id, BACKGROUND_CONTEXT.clone())
            .await?;
        match result {
            AgentPromptResult::Unanswered { reason, .. } => {
                Err(pi_js::Error::msg(format!("Prompt was not answered: {reason}")))
            }
            AgentPromptResult::Done { text, .. } => Ok(ClientResult::Prompted {
                server_id: selected.route.server_id().clone(),
                session_id,
                text,
            }),
        }
    }
    .await;

    let dispose_res = runtime.dispose().await;
    match outcome {
        Ok(val) => {
            dispose_res?;
            Ok(val)
        }
        Err(err) => {
            let _ = dispose_res;
            Err(err)
        }
    }
}
