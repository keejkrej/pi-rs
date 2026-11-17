//! Port of packages/coding-agent/src/cli/experimental/commands/server.ts

use std::sync::{Arc, LazyLock};

use pi_js::BoxFuture;
use pi_protocol::{ServerId, is_server_id};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cli::experimental::command::{
    Command, CommandOption, CommandOptionParseResult, CommandParseResult, OptionConfig, string_option, value_option,
};
use crate::cli::experimental::command_options::{
    AUTH_TOKEN_FILE_OPTION, AUTH_TOKEN_OPTION, AuthInput, parse_auth, unsupported_options,
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCommand {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugin_packages: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<ServerId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_dir: Option<String>,
}

pub type ServerCommandHandler = Arc<dyn Fn(ServerCommand) -> BoxFuture<pi_js::Result<()>> + Send + Sync>;

#[derive(Clone)]
pub struct ServerCommandContext {
    pub handler: ServerCommandHandler,
}

impl ServerCommandContext {
    pub fn run_server(&self, command: ServerCommand) -> BoxFuture<pi_js::Result<()>> {
        (self.handler)(command)
    }
}

static SERVER_ID_OPTION: LazyLock<CommandOption<ServerId>> = LazyLock::new(|| {
    value_option(
        "--server-id",
        |value| {
            if is_server_id(&Value::String(value.to_string())) {
                CommandOptionParseResult::Ok(value.to_string())
            } else {
                CommandOptionParseResult::Err(format!("Invalid --server-id \"{value}\"; expected a lowercase UUIDv4"))
            }
        },
        OptionConfig::default(),
    )
});
static SESSION_DIR_OPTION: LazyLock<CommandOption<String>> =
    LazyLock::new(|| string_option("--session-dir", OptionConfig::default()));
static PROVIDER_OPTION: LazyLock<CommandOption<String>> =
    LazyLock::new(|| string_option("--provider", OptionConfig::default()));
static MODEL_OPTION: LazyLock<CommandOption<String>> =
    LazyLock::new(|| string_option("--model", OptionConfig::default()));
static PLUGIN_PACKAGE_OPTION: LazyLock<CommandOption<String>> =
    LazyLock::new(|| string_option("-e", OptionConfig { repeatable: true }));

pub fn server_command() -> Command<ServerCommand, ServerCommandContext> {
    Command::new("server")
        .option(&SERVER_ID_OPTION)
        .option(&SESSION_DIR_OPTION)
        .option(&PROVIDER_OPTION)
        .option(&MODEL_OPTION)
        .option(&PLUGIN_PACKAGE_OPTION)
        .option(&AUTH_TOKEN_OPTION)
        .option(&AUTH_TOKEN_FILE_OPTION)
        .build(|input| {
            let parsed_auth = parse_auth(input);
            let server_id = input.value(&SERVER_ID_OPTION);
            let session_dir = input.value(&SESSION_DIR_OPTION);
            let provider = input.value(&PROVIDER_OPTION);
            let model = input.value(&MODEL_OPTION);
            let plugin_packages = input.values(&PLUGIN_PACKAGE_OPTION);
            let mut errors = parsed_auth.errors;
            if provider.is_some() && model.is_none() {
                errors.push("--provider requires --model".to_string());
            }
            errors.extend(unsupported_options("server", input));
            if !errors.is_empty() {
                return CommandParseResult::Err { errors };
            }
            CommandParseResult::Ok {
                command: ServerCommand {
                    auth: parsed_auth.auth,
                    provider,
                    model,
                    plugin_packages,
                    server_id,
                    session_dir,
                },
            }
        })
        .action(|command, context: &ServerCommandContext| context.run_server(command))
}
