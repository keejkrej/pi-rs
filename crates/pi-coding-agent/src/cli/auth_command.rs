//! Port of packages/coding-agent/src/cli/auth-command.ts

use pi_ai::AuthResult;

use crate::cli::args::{Args, UnknownFlagValue};
use crate::config::APP_NAME;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthCommandKind {
    Check,
    ApiKey,
    BearerToken,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthCommand {
    pub kind: AuthCommandKind,
    pub args: Vec<String>,
    pub json: bool,
    pub credentials: bool,
    pub no_refresh: bool,
    pub min_expiry_ms: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct AuthCommandError {
    pub message: String,
}

impl AuthCommandError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn into_error(self) -> pi_js::Error {
        pi_js::Error::typed("AuthCommandError", self)
    }
}

pub fn get_auth_command_name(kind: AuthCommandKind) -> &'static str {
    match kind {
        AuthCommandKind::Check => "auth check",
        AuthCommandKind::ApiKey => "auth print-api-key",
        AuthCommandKind::BearerToken => "auth print-bearer-token",
    }
}

pub fn get_auth_command_usage(kind: AuthCommandKind) -> String {
    let app: &str = *APP_NAME;
    match kind {
        AuthCommandKind::Check => {
            format!("{app} auth check --provider <provider> [--json] [--credentials] [--no-refresh]")
        }
        AuthCommandKind::ApiKey => format!("{app} auth print-api-key --provider <provider> [--model <model>]"),
        AuthCommandKind::BearerToken => {
            format!("{app} auth print-bearer-token --provider <provider> [--model <model>] [--min-expiry <duration>]")
        }
    }
}

pub fn is_auth_command_help<S: AsRef<str>>(args: &[S]) -> bool {
    args.first().map(AsRef::as_ref) == Some("auth")
        && (args.get(1).is_none()
            || args.get(1).map(AsRef::as_ref) == Some("help")
            || args.iter().any(|a| a.as_ref() == "--help" || a.as_ref() == "-h"))
}

pub fn print_auth_command_help() {
    pi_js::console::log(
        "Usage:
  pi auth login <provider> [--api-key] [--no-browser]
  pi auth logout <provider>
  pi auth print-api-key [--provider <provider>] [--model <model>]
  pi auth print-bearer-token [--provider <provider>] [--model <model>] [--min-expiry <duration>]
  pi auth check [--provider <provider>] [--model <model>] [--json] [--credentials] [--no-refresh]

Log in to and out of model providers, print their credentials for external clients, or check that they are ready.
Credentials are stored in ~/.pi/agent/auth.json.

Commands:
  login <provider>        Sign in with an account in the browser, or save an API key, then exit
  logout <provider>       Remove the stored credentials; environment variables and models.json stay
  print-api-key           Print the resolved API key
  print-bearer-token      Print a resolved OAuth bearer token (refreshes if expired)
  check                   Print ready, not_ready, or invalid; exit with 0, 1, or 2

Options for print-api-key, print-bearer-token, and check (at least one is required):
  --provider <provider>   Resolve credentials for a provider
  --model <model>         Resolve credentials from a model

Options for login:
  --api-key               Save an API key instead of signing in with an account
  --no-browser            Do not open a browser; use another sign-in method, e.g. device code

Other options:
  --min-expiry <duration> Minimum remaining token lifetime for print-bearer-token, e.g. 30m or 1h
  --json                  Print the check result as JSON
  --credentials           Emit the credential from check, or include it in JSON output
  --no-refresh            Do not refresh expired OAuth credentials during check",
    );
}

pub fn parse_auth_command<S: AsRef<str>>(args: &[S]) -> pi_js::Result<Option<AuthCommand>> {
    if args.first().map(AsRef::as_ref) != Some("auth") {
        return Ok(None);
    }

    let kind = match args.get(1).map(AsRef::as_ref) {
        Some("check") => AuthCommandKind::Check,
        Some("print-api-key") => AuthCommandKind::ApiKey,
        Some("print-bearer-token") => AuthCommandKind::BearerToken,
        other => {
            let sub = other.unwrap_or("");
            let app: &str = *APP_NAME;
            return Err(AuthCommandError::new(format!(
                "Unknown auth command \"{sub}\". Use \"{app} auth login\", \"{app} auth logout\", \"{app} auth print-api-key\", \"{app} auth print-bearer-token\", or \"{app} auth check\"."
            ))
            .into_error());
        }
    };

    let mut command_args: Vec<String> = Vec::new();
    let mut json = false;
    let mut credentials = false;
    let mut no_refresh = false;
    let mut min_expiry_ms: Option<i64> = None;

    static DURATION_RE: std::sync::LazyLock<Option<regex::Regex>> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)^(\d+)(ms|s|m|h)$").ok());

    let mut index = 2;
    while index < args.len() {
        let arg = args[index].as_ref();
        if arg == "--min-expiry" {
            if kind != AuthCommandKind::BearerToken {
                return Err(AuthCommandError::new("--min-expiry is only supported by print-bearer-token").into_error());
            }
            index += 1;
            let value = args.get(index).map(AsRef::as_ref);
            let Some(caps) = value.and_then(|v| DURATION_RE.as_ref().and_then(|re| re.captures(v))) else {
                return Err(AuthCommandError::new("--min-expiry must use a duration such as 30m or 1h").into_error());
            };
            let amount: i64 = caps[1].parse().map_err(|_| {
                AuthCommandError::new("--min-expiry must use a duration such as 30m or 1h").into_error()
            })?;
            let unit = caps[2].to_ascii_lowercase();
            let multiplier = match unit.as_str() {
                "ms" => 1,
                "s" => 1_000,
                "m" => 60_000,
                _ => 3_600_000,
            };
            min_expiry_ms = Some(amount * multiplier);
            index += 1;
            continue;
        }
        if arg == "--json" || arg == "--credentials" || arg == "--no-refresh" {
            if kind != AuthCommandKind::Check {
                return Err(AuthCommandError::new(format!("{arg} is only supported by auth check")).into_error());
            }
            if arg == "--json" {
                json = true;
            } else if arg == "--credentials" {
                credentials = true;
            } else {
                no_refresh = true;
            }
            index += 1;
            continue;
        }
        command_args.push(arg.to_string());
        index += 1;
    }

    Ok(Some(AuthCommand {
        kind,
        args: command_args,
        json,
        credentials,
        no_refresh,
        min_expiry_ms,
    }))
}

pub fn validate_auth_command_args(
    args: &Args,
    kind: AuthCommandKind,
) -> pi_js::Result<(Option<String>, Option<String>)> {
    let provider = args
        .provider
        .as_deref()
        .map(pi_js::str16::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let model = args
        .model
        .as_deref()
        .map(pi_js::str16::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    if let Some((option, _)) = args.unknown_flags.iter().next() {
        return Err(AuthCommandError::new(format!(
            "Unknown option --{option} for \"{}\".",
            get_auth_command_name(kind)
        ))
        .into_error());
    }
    if args.api_key.is_some() || !args.messages.is_empty() || !args.file_args.is_empty() {
        return Err(AuthCommandError::new("Auth commands only accept --provider and --model").into_error());
    }
    if kind == AuthCommandKind::Check {
        if provider.is_none() && model.is_none() {
            return Err(
                AuthCommandError::new("Auth checks require --provider <provider> or --model <model>").into_error(),
            );
        }
        return Ok((provider, model));
    }
    if provider.is_none() && model.is_none() {
        return Err(
            AuthCommandError::new("Credential printing requires --provider <provider> or --model <model>").into_error(),
        );
    }
    Ok((provider, model))
}

pub fn get_auth_credential(auth: Option<&AuthResult>) -> Option<String> {
    let auth = auth?;
    if let Some(api_key) = &auth.auth.api_key
        && !api_key.is_empty()
    {
        return Some(api_key.clone());
    }
    let authorization = auth
        .auth
        .headers
        .as_ref()?
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        .and_then(|(_, val)| val.as_deref())?;
    static BEARER_RE: std::sync::LazyLock<Option<regex::Regex>> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)^Bearer\s+(.+)$").ok());
    BEARER_RE
        .as_ref()?
        .captures(authorization)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

// Keep UnknownFlagValue referenced cleanly if needed by callers
#[allow(dead_code)]
fn _unknown_flag_is_bool(v: &UnknownFlagValue) -> bool {
    matches!(v, UnknownFlagValue::Bool(true))
}
