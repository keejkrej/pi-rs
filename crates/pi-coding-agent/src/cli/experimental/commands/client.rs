//! Port of packages/coding-agent/src/cli/experimental/commands/client.ts

use std::sync::{Arc, LazyLock};

use pi_js::BoxFuture;
use serde::{Deserialize, Serialize};

use crate::cli::experimental::command::{
    Command, CommandOption, CommandParseResult, OptionConfig, flag_option, string_option,
};
use crate::cli::experimental::command_options::{
    AUTH_TOKEN_FILE_OPTION, AUTH_TOKEN_OPTION, AuthInput, CONNECT_OPTION, TransportAddress, parse_auth,
    unsupported_options,
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCommand {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect: Option<TransportAddress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(rename = "continue", default, skip_serializing_if = "is_false")]
    pub continue_: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub resume: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugin_packages: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

pub type ClientCommandHandler = Arc<dyn Fn(ClientCommand) -> BoxFuture<pi_js::Result<()>> + Send + Sync>;

#[derive(Clone)]
pub struct ClientCommandContext {
    pub handler: ClientCommandHandler,
}

impl ClientCommandContext {
    pub fn run_client(&self, command: ClientCommand) -> BoxFuture<pi_js::Result<()>> {
        (self.handler)(command)
    }
}

static SESSION_ID_OPTION: LazyLock<CommandOption<String>> =
    LazyLock::new(|| string_option("--session-id", OptionConfig::default()));
static CONTINUE_OPTION: LazyLock<CommandOption<bool>> = LazyLock::new(|| flag_option("--continue"));
static CONTINUE_SHORT_OPTION: LazyLock<CommandOption<bool>> = LazyLock::new(|| flag_option("-c"));
static RESUME_OPTION: LazyLock<CommandOption<bool>> = LazyLock::new(|| flag_option("--resume"));
static RESUME_SHORT_OPTION: LazyLock<CommandOption<bool>> = LazyLock::new(|| flag_option("-r"));
static PROVIDER_OPTION: LazyLock<CommandOption<String>> =
    LazyLock::new(|| string_option("--provider", OptionConfig::default()));
static MODEL_OPTION: LazyLock<CommandOption<String>> =
    LazyLock::new(|| string_option("--model", OptionConfig::default()));
static PLUGIN_PACKAGE_OPTION: LazyLock<CommandOption<String>> =
    LazyLock::new(|| string_option("-e", OptionConfig { repeatable: true }));

pub fn client_command() -> Command<ClientCommand, ClientCommandContext> {
    Command::new("client")
        .option(&CONNECT_OPTION)
        .option(&SESSION_ID_OPTION)
        .option(&CONTINUE_OPTION)
        .option(&CONTINUE_SHORT_OPTION)
        .option(&RESUME_OPTION)
        .option(&RESUME_SHORT_OPTION)
        .option(&PROVIDER_OPTION)
        .option(&MODEL_OPTION)
        .option(&PLUGIN_PACKAGE_OPTION)
        .option(&AUTH_TOKEN_OPTION)
        .option(&AUTH_TOKEN_FILE_OPTION)
        .build(|input| {
            let parsed_auth = parse_auth(input);
            let connect = input.value(&CONNECT_OPTION);
            let session_id = input.value(&SESSION_ID_OPTION);
            let should_continue =
                input.value(&CONTINUE_OPTION) == Some(true) || input.value(&CONTINUE_SHORT_OPTION) == Some(true);
            let should_resume =
                input.value(&RESUME_OPTION) == Some(true) || input.value(&RESUME_SHORT_OPTION) == Some(true);
            let provider = input.value(&PROVIDER_OPTION);
            let model = input.value(&MODEL_OPTION);
            let plugin_packages = input.values(&PLUGIN_PACKAGE_OPTION);
            let remaining = input.remaining_args();
            let prompt_args = if remaining.first().map(String::as_str) == Some("--") {
                &remaining[1..]
            } else {
                remaining
            };
            let prompt = if prompt_args.len() == 1
                && (remaining.first().map(String::as_str) == Some("--") || !prompt_args[0].starts_with('-'))
                && !prompt_args[0].is_empty()
            {
                Some(prompt_args[0].clone())
            } else {
                None
            };

            let mut errors = parsed_auth.errors;
            if provider.is_some() && model.is_none() {
                errors.push("--provider requires --model".to_string());
            }
            let selected_count =
                usize::from(session_id.is_some()) + usize::from(should_continue) + usize::from(should_resume);
            if selected_count > 1 {
                errors.push("--session-id, --continue, and --resume are mutually exclusive".to_string());
            }
            if !remaining.is_empty() && prompt.is_none() {
                errors.extend(unsupported_options("client", input));
            }
            if !errors.is_empty() {
                return CommandParseResult::Err { errors };
            }
            CommandParseResult::Ok {
                command: ClientCommand {
                    auth: parsed_auth.auth,
                    connect,
                    session_id,
                    continue_: should_continue,
                    resume: should_resume,
                    provider,
                    model,
                    plugin_packages,
                    prompt,
                },
            }
        })
        .action(|command, context: &ClientCommandContext| context.run_client(command))
}
