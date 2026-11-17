//! Port of packages/coding-agent/src/cli/experimental/command-options.ts

use std::sync::LazyLock;

use pi_js::path::posix;
use pi_js::uri::decode_uri_component;
use pi_protocol::{ServerId, is_server_id};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cli::experimental::command::{
    CommandOption, CommandOptionParseResult, OptionConfig, ParsedCommandInput, string_option, value_option,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AuthInput {
    Token { token: String },
    File { path: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "lowercase")]
pub enum TransportAddress {
    Unix {
        path: String,
    },
    Radius {
        #[serde(rename = "serverId")]
        server_id: ServerId,
    },
}

pub static AUTH_TOKEN_OPTION: LazyLock<CommandOption<String>> =
    LazyLock::new(|| string_option("--auth-token", OptionConfig::default()));
pub static AUTH_TOKEN_FILE_OPTION: LazyLock<CommandOption<String>> =
    LazyLock::new(|| string_option("--auth-token-file", OptionConfig::default()));
pub static CONNECT_OPTION: LazyLock<CommandOption<TransportAddress>> = LazyLock::new(|| {
    value_option(
        "--connect",
        |value| match parse_transport_address(value) {
            Ok(address) => CommandOptionParseResult::Ok(address),
            Err(error) => CommandOptionParseResult::Err(error),
        },
        OptionConfig::default(),
    )
});

pub struct ParsedAuth {
    pub auth: Option<AuthInput>,
    pub errors: Vec<String>,
}

fn parse_auth_input(auth_token: Option<String>, auth_token_file: Option<String>) -> ParsedAuth {
    if auth_token.is_some() && auth_token_file.is_some() {
        return ParsedAuth {
            auth: None,
            errors: vec!["--auth-token and --auth-token-file are mutually exclusive".to_string()],
        };
    }
    if let Some(token) = auth_token {
        return ParsedAuth {
            auth: Some(AuthInput::Token { token }),
            errors: Vec::new(),
        };
    }
    if let Some(path) = auth_token_file {
        return ParsedAuth {
            auth: Some(AuthInput::File { path }),
            errors: Vec::new(),
        };
    }
    ParsedAuth {
        auth: None,
        errors: Vec::new(),
    }
}

fn parse_transport_address(value: &str) -> Result<TransportAddress, String> {
    let url = url::Url::parse(value).map_err(|_| format!("Invalid --connect address \"{value}\""))?;
    let scheme = url.scheme();
    if scheme == "radius" {
        let host = url.host_str().unwrap_or("");
        let expected = format!("radius://{host}{}", url.path());
        if !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
            || (!url.path().is_empty() && url.path() != "/")
            || url.query().is_some()
            || url.fragment().is_some()
            || value != expected
        {
            return Err(format!("Invalid --connect address \"{value}\""));
        }
        if !is_server_id(&Value::String(host.to_string())) {
            return Err("Radius transport address requires a lowercase UUIDv4 server ID".to_string());
        }
        return Ok(TransportAddress::Radius {
            server_id: host.to_string(),
        });
    }
    if scheme != "unix" {
        return Err(format!("Unsupported --connect transport \"{scheme}:\""));
    }
    if url.host_str().is_some_and(|h| !h.is_empty())
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("Unix transport address must not include an authority".to_string());
    }
    if !value.starts_with("unix:///")
        || value.starts_with("unix:////")
        || value.contains('?')
        || value.contains('#')
        || url.as_str() != value
    {
        return Err(format!("Invalid --connect address \"{value}\""));
    }
    let decoded = decode_uri_component(url.path()).map_err(|_| format!("Invalid --connect address \"{value}\""))?;
    if decoded.contains('\0') {
        return Err(format!("Invalid --connect address \"{value}\""));
    }
    if !posix::is_absolute(&decoded) {
        return Err("Unix transport address requires an absolute path".to_string());
    }
    Ok(TransportAddress::Unix { path: decoded })
}

pub fn parse_auth(input: &ParsedCommandInput) -> ParsedAuth {
    parse_auth_input(input.value(&AUTH_TOKEN_OPTION), input.value(&AUTH_TOKEN_FILE_OPTION))
}

pub fn unsupported_options(command: &str, input: &ParsedCommandInput) -> Vec<String> {
    if input.remaining_args().is_empty() {
        return Vec::new();
    }
    vec![format!(
        "The experimental {command} command does not support existing CLI options yet"
    )]
}
