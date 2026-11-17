//! Port of packages/coding-agent/src/experimental/commands.ts

use std::io::IsTerminal;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::cli::experimental::cli::{CLI, CliContext};
use crate::cli::experimental::command::CommandParseResult;
use crate::cli::experimental::commands::client::{ClientCommand, ClientCommandContext};
use crate::cli::experimental::commands::server::{ServerCommand, ServerCommandContext};
use crate::core::experimental::are_experimental_features_enabled;
use crate::experimental::client::{ClientResult, RunClientOptions, run_client};
use crate::experimental::client_tui::{RunClientTuiOptions, run_client_tui};
use crate::experimental::radius_relay::RadiusRelayHostStatus;
use crate::experimental::server::{StartServerOptions, start_foreground_server};
use crate::vendor::chalk;

fn lock_mutex<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

async fn run_server_command(command: ServerCommand) -> pi_js::Result<()> {
    struct RelayPrinterState {
        previous: String,
        output_ready: bool,
        pending: Option<RadiusRelayHostStatus>,
    }

    let printer = Arc::new(Mutex::new(RelayPrinterState {
        previous: String::new(),
        output_ready: false,
        pending: None,
    }));

    fn describe_status(status: &RadiusRelayHostStatus) -> Option<String> {
        match status {
            RadiusRelayHostStatus::Connecting => None,
            RadiusRelayHostStatus::Connected => Some("connected".to_string()),
            RadiusRelayHostStatus::NotAuthenticated => Some("not connected; local only".to_string()),
            RadiusRelayHostStatus::Retrying { error } => Some(format!("reconnecting: {error}")),
        }
    }

    let emit_status = |state: &mut RelayPrinterState, status: &RadiusRelayHostStatus| {
        let Some(description) = describe_status(status) else {
            return;
        };
        if description == state.previous {
            return;
        }
        state.previous = description.clone();
        pi_js::console::log(&format!("Radius: {description}"));
    };

    let printer_cb = Arc::clone(&printer);
    let runtime = start_foreground_server(StartServerOptions {
        directory: None,
        server_id: command.server_id,
        session_dir: command.session_dir,
        provider: command.provider,
        model: command.model,
        keep_alive: Some(true),
        relay_auth: command.auth,
        plugin_packages: Some(command.plugin_packages),
        on_relay_status: Some(Arc::new(move |status| {
            let mut st = lock_mutex(&printer_cb);
            if st.output_ready {
                emit_status(&mut st, &status);
            } else {
                st.pending = Some(status);
            }
        })),
    })
    .await?;

    pi_js::console::log(&format!("Server: {}", runtime.server_id));
    pi_js::console::log(&format!("Socket: {}", runtime.socket_path));
    {
        let mut st = lock_mutex(&printer);
        st.output_ready = true;
        if let Some(pending) = st.pending.take() {
            emit_status(&mut st, &pending);
        }
    }

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
            } => Ok(()),
            _ = async {
                if let Some(ref mut s) = sigterm {
                    let _ = s.recv().await;
                } else {
                    futures::future::pending::<()>().await;
                }
            } => Ok(()),
        }
    };
    #[cfg(not(unix))]
    let wait_res = runtime.closed().await;

    let close_res = runtime.close().await;
    wait_res?;
    close_res
}

async fn run_client_command(command: ClientCommand) -> pi_js::Result<()> {
    if command.prompt.is_none() && std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        return run_client_tui(&command, RunClientTuiOptions::default()).await;
    }
    match run_client(&command, RunClientOptions::default()).await? {
        ClientResult::Attached { server_id, session_id } => {
            pi_js::console::log(&format!("{server_id}\t{session_id}\tattached"));
        }
        ClientResult::Prompted { text, .. } => {
            pi_js::console::log(&text);
        }
        ClientResult::List { sessions } => {
            for session in sessions {
                pi_js::console::log(&format!("{}\t{}", session.server_id, session.session_id));
            }
        }
    }
    Ok(())
}

/// Development-only command dispatch. Published entrypoints must not import this module.
///
/// PORT: returns `None` when not an enabled experimental subcommand, or `Some(exit_code)`.
pub async fn run_experimental_command(args: &[String]) -> Option<i32> {
    if !are_experimental_features_enabled() || !matches!(args.first().map(String::as_str), Some("server" | "client")) {
        return None;
    }

    let context = CliContext {
        server: ServerCommandContext {
            handler: Arc::new(|cmd| Box::pin(run_server_command(cmd))),
        },
        client: ClientCommandContext {
            handler: Arc::new(|cmd| Box::pin(run_client_command(cmd))),
        },
    };

    let code = match CLI.execute(args, &context).await {
        Ok(CommandParseResult::Ok { .. }) => 0,
        Ok(CommandParseResult::Err { errors }) => {
            for error in errors {
                pi_js::console::error(&chalk::red(&format!("Error: {error}")));
            }
            1
        }
        Err(error) => {
            pi_js::console::error(&chalk::red(&format!("Error: {}", error.message())));
            1
        }
    };
    Some(code)
}
