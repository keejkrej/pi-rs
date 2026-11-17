//! Port of packages/coding-agent/test/experimental-cli-resolution.test.ts

use std::sync::{Arc, Mutex};

use pi_coding_agent::cli::experimental::cli::{CLI, CliContext, ExperimentalCliCommand};
use pi_coding_agent::cli::experimental::command::CommandParseResult;
use pi_coding_agent::cli::experimental::commands::client::{ClientCommand, ClientCommandContext};
use pi_coding_agent::cli::experimental::commands::server::{ServerCommand, ServerCommandContext};

#[test]
fn requires_an_experimental_subcommand() {
    let result = CLI.parse(&[]);
    assert_eq!(
        result,
        CommandParseResult::Err {
            errors: vec!["Expected experimental command: server or client".to_string()],
        }
    );
}

#[tokio::test]
async fn passes_server_options_to_the_command_action() {
    let server_calls: Arc<Mutex<Vec<ServerCommand>>> = Arc::new(Mutex::new(Vec::new()));
    let client_calls: Arc<Mutex<Vec<ClientCommand>>> = Arc::new(Mutex::new(Vec::new()));
    let server_capture = Arc::clone(&server_calls);
    let client_capture = Arc::clone(&client_calls);

    let ctx = CliContext {
        server: ServerCommandContext {
            handler: Arc::new(move |cmd| {
                server_capture.lock().expect("lock").push(cmd);
                Box::pin(async { Ok(()) })
            }),
        },
        client: ClientCommandContext {
            handler: Arc::new(move |cmd| {
                client_capture.lock().expect("lock").push(cmd);
                Box::pin(async { Ok(()) })
            }),
        },
    };

    let result = CLI
        .execute(
            &[
                "server",
                "--server-id",
                "00000000-0000-4000-8000-000000000001",
                "--session-dir",
                "./sessions",
                "--provider",
                "anthropic",
                "--model",
                "claude-sonnet-4-5",
            ],
            &ctx,
        )
        .await
        .expect("execute succeeds");

    let expected = ServerCommand {
        auth: None,
        server_id: Some("00000000-0000-4000-8000-000000000001".to_string()),
        session_dir: Some("./sessions".to_string()),
        provider: Some("anthropic".to_string()),
        model: Some("claude-sonnet-4-5".to_string()),
        plugin_packages: Vec::new(),
    };
    assert_eq!(
        result,
        CommandParseResult::Ok {
            command: ExperimentalCliCommand::Server(expected.clone()),
        }
    );
    assert_eq!(server_calls.lock().expect("lock").as_slice(), &[expected]);
    assert!(client_calls.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn executes_parsed_server_and_client_commands() {
    for name in ["server", "client"] {
        let server_calls: Arc<Mutex<Vec<ServerCommand>>> = Arc::new(Mutex::new(Vec::new()));
        let client_calls: Arc<Mutex<Vec<ClientCommand>>> = Arc::new(Mutex::new(Vec::new()));
        let server_capture = Arc::clone(&server_calls);
        let client_capture = Arc::clone(&client_calls);

        let ctx = CliContext {
            server: ServerCommandContext {
                handler: Arc::new(move |cmd| {
                    server_capture.lock().expect("lock").push(cmd);
                    Box::pin(async { Ok(()) })
                }),
            },
            client: ClientCommandContext {
                handler: Arc::new(move |cmd| {
                    client_capture.lock().expect("lock").push(cmd);
                    Box::pin(async { Ok(()) })
                }),
            },
        };

        let result = CLI.execute(&[name], &ctx).await.expect("execute succeeds");
        if name == "server" {
            assert_eq!(
                result,
                CommandParseResult::Ok {
                    command: ExperimentalCliCommand::Server(ServerCommand::default()),
                }
            );
            assert_eq!(server_calls.lock().expect("lock").len(), 1);
            assert_eq!(client_calls.lock().expect("lock").len(), 0);
        } else {
            assert_eq!(
                result,
                CommandParseResult::Ok {
                    command: ExperimentalCliCommand::Client(ClientCommand::default()),
                }
            );
            assert_eq!(server_calls.lock().expect("lock").len(), 0);
            assert_eq!(client_calls.lock().expect("lock").len(), 1);
        }
    }
}
