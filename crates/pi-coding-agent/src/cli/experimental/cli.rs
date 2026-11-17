//! Port of packages/coding-agent/src/cli/experimental/cli.ts

use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use crate::cli::experimental::command::{Command, CommandParseResult};
use crate::cli::experimental::commands::client::{ClientCommand, ClientCommandContext, client_command};
use crate::cli::experimental::commands::server::{ServerCommand, ServerCommandContext, server_command};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "lowercase")]
pub enum ExperimentalCliCommand {
    Server(ServerCommand),
    Client(ClientCommand),
}

#[derive(Clone)]
pub struct CliContext {
    pub server: ServerCommandContext,
    pub client: ClientCommandContext,
}

pub fn create_experimental_cli() -> Command<ExperimentalCliCommand, CliContext> {
    Command::new("experimental")
        .build(|_| CommandParseResult::Err {
            errors: vec!["Expected experimental command: server or client".to_string()],
        })
        .subcommand(server_command(), ExperimentalCliCommand::Server, |ctx: &CliContext| {
            &ctx.server
        })
        .subcommand(client_command(), ExperimentalCliCommand::Client, |ctx: &CliContext| {
            &ctx.client
        })
}

pub static CLI: LazyLock<Command<ExperimentalCliCommand, CliContext>> = LazyLock::new(create_experimental_cli);
