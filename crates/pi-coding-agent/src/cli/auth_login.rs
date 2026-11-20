//! Port of packages/coding-agent/src/cli/auth-login.ts

use std::sync::{Arc, Mutex};

use pi_ai::models::{ModelsRefreshOptions, Provider};
use pi_ai::{
    AuthEvent, AuthInteraction, AuthPrompt, AuthPromptKind, AuthSelectOption, AuthType, GetDeviceIdFn, LoginOptions,
};
use pi_js::Result;
use pi_js::abort::AbortSignal;
use pi_tui::replace_control_characters;

use crate::cli::auth_terminal::LoginTerminal;
use crate::config::{APP_NAME, get_auth_path};
use crate::core::model_runtime::{CredentialSynchronizationError, ModelRuntime};
use crate::core::provider_matching::find_matching_provider_arcs;
use crate::core::radius::RADIUS_PROVIDER_ID;
use crate::core::radius_mcp::plan_radius_mcp_server;
use crate::extensions::mcp::config::{McpServerConfig, add_mcp_server_config};
use crate::utils::web_url::{display_url, web_url};

/// Dependencies of `run_auth_login`, injected so tests can script the terminal.
pub struct AuthLoginOptions<'a> {
    pub model_runtime: &'a ModelRuntime,
    pub terminal: Arc<dyn LoginTerminal>,
    pub agent_dir: &'a str,
    pub cwd: &'a str,
    pub get_device_id: GetDeviceIdFn,
    /// Aborts the login, e.g. on Ctrl+C.
    pub signal: AbortSignal,
}

fn auth_login_usage() -> String {
    format!("{} auth login <provider> [--api-key] [--no-browser]", *APP_NAME)
}

#[derive(Clone)]
struct LoginRequest {
    provider_ref: String,
    api_key: bool,
    browser: bool,
}

fn parse_login_request<S: AsRef<str>>(args: &[S]) -> std::result::Result<LoginRequest, String> {
    let usage = format!("Usage: {}", auth_login_usage());
    let mut request = LoginRequest {
        provider_ref: String::new(),
        api_key: false,
        browser: true,
    };
    for raw in args {
        let arg = raw.as_ref();
        if arg == "--api-key" {
            request.api_key = true;
        } else if arg == "--no-browser" {
            request.browser = false;
        } else if arg.starts_with('-') || !request.provider_ref.is_empty() {
            return Err(usage);
        } else {
            request.provider_ref = arg.to_string();
        }
    }
    if request.provider_ref.is_empty() {
        return Err(usage);
    }
    if request.api_key && !request.browser {
        return Err("--no-browser only applies to account sign-in; omit it with --api-key.".to_string());
    }
    Ok(request)
}

fn select_auth_type(provider: &dyn Provider, api_key: bool) -> std::result::Result<AuthType, String> {
    let auth = provider.auth();
    let has_api_key_login = auth.api_key.as_ref().is_some_and(|ak| ak.supports_login());
    if !api_key && auth.oauth.is_some() {
        return Ok(AuthType::OAuth);
    }
    if has_api_key_login {
        return Ok(AuthType::ApiKey);
    }
    if api_key && auth.oauth.is_some() {
        return Err(format!(
            "{} has no API key login. Omit --api-key to sign in.",
            provider.name()
        ));
    }
    let method = auth
        .api_key
        .as_ref()
        .map(|ak| ak.name())
        .unwrap_or("ambient credentials");
    Err(format!(
        "{} is configured outside {} ({method}).",
        provider.name(),
        *APP_NAME
    ))
}

struct TerminalLoginInteraction {
    provider_id: String,
    method_choice: &'static str,
    request: LoginRequest,
    terminal: Arc<dyn LoginTerminal>,
    signal: AbortSignal,
    skipped_methods: Mutex<Vec<String>>,
}

impl TerminalLoginInteraction {
    async fn select_method(
        &self,
        message: &str,
        options: &[AuthSelectOption],
        prompt_signal: &AbortSignal,
    ) -> Result<String> {
        let Some((first, others)) = options.split_first() else {
            return select_in_terminal(message, options, self.terminal.as_ref(), prompt_signal).await;
        };
        if self.method_choice == "ask" {
            return select_in_terminal(message, options, self.terminal.as_ref(), prompt_signal).await;
        }
        if self.method_choice == "browser" {
            let skipped = others.iter().map(|o| o.label.clone()).collect();
            *self.skipped_methods.lock().unwrap_or_else(|e| e.into_inner()) = skipped;
            return Ok(first.id.clone());
        }
        let Some((only_other, more_others)) = others.split_first() else {
            return Ok(first.id.clone());
        };
        if !more_others.is_empty() {
            return select_in_terminal(message, others, self.terminal.as_ref(), prompt_signal).await;
        }
        self.terminal.write(&format!("{message} {}", only_other.label));
        Ok(only_other.id.clone())
    }
}

#[async_trait::async_trait]
impl AuthInteraction for TerminalLoginInteraction {
    fn signal(&self) -> Option<&AbortSignal> {
        Some(&self.signal)
    }

    async fn prompt(&self, prompt: AuthPrompt) -> Result<String> {
        let prompt_signal = match prompt.signal {
            Some(ref s) => AbortSignal::any(&[self.signal.clone(), s.clone()]),
            None => self.signal.clone(),
        };
        match prompt.kind {
            AuthPromptKind::Select { message, options } => self.select_method(&message, &options, &prompt_signal).await,
            AuthPromptKind::Secret { message, placeholder } => {
                let ph = placeholder
                    .as_deref()
                    .filter(|p| !p.is_empty())
                    .map(|p| format!(" ({p})"))
                    .unwrap_or_default();
                let msg = format!("{message}{ph}: ");
                self.terminal.secret(&msg, &prompt_signal).await
            }
            AuthPromptKind::Text { message, placeholder } | AuthPromptKind::ManualCode { message, placeholder } => {
                let ph = placeholder
                    .as_deref()
                    .filter(|p| !p.is_empty())
                    .map(|p| format!(" ({p})"))
                    .unwrap_or_default();
                let msg = format!("{message}{ph}: ");
                self.terminal.question(&msg, &prompt_signal).await
            }
        }
    }

    fn notify(&self, event: AuthEvent) {
        match event {
            AuthEvent::AuthUrl { url, instructions } => {
                self.terminal
                    .write(&format!("Open this URL to sign in:\n  {}", display_url(&url)));
                if let Some(instr) = instructions {
                    self.terminal.write(&instr);
                }
                if let Some(target) = web_url(&url)
                    && self.request.browser
                {
                    self.terminal.open_url(&target);
                }
                let rerun = format!("\"{} auth login {} --no-browser\"", *APP_NAME, self.provider_id);
                let skipped = self.skipped_methods.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if let Some((only_method, more_methods)) = skipped.split_first() {
                    if more_methods.is_empty() {
                        self.terminal
                            .write(&format!("To use \"{only_method}\" instead, run {rerun}"));
                    } else {
                        let labels: Vec<String> = skipped.iter().map(|l| format!("\"{l}\"")).collect();
                        self.terminal.write(&format!(
                            "Other sign-in methods: {}. Run {rerun} to choose one.",
                            labels.join(", ")
                        ));
                    }
                }
            }
            AuthEvent::DeviceCode {
                user_code,
                verification_uri,
                open_browser_on_confirm,
                ..
            } => {
                self.terminal.write(&format!(
                    "Open {} and enter code: {user_code}",
                    display_url(&verification_uri)
                ));
                if let Some(target) = web_url(&verification_uri)
                    && self.request.browser
                    && open_browser_on_confirm != Some(false)
                {
                    self.terminal.open_url(&target);
                }
                self.terminal.write("Waiting for authentication...");
            }
            AuthEvent::Info { message, links } => {
                self.terminal.write(&message);
                for link in links.into_iter().flatten() {
                    let prefix = match link.label.as_deref() {
                        Some(l) if !l.is_empty() => format!("{l}: "),
                        _ => String::new(),
                    };
                    self.terminal.write(&format!("  {prefix}{}", display_url(&link.url)));
                }
            }
            AuthEvent::Progress { message } => {
                self.terminal.write(&message);
            }
        }
    }
}

async fn select_in_terminal(
    message: &str,
    options: &[AuthSelectOption],
    terminal: &dyn LoginTerminal,
    signal: &AbortSignal,
) -> Result<String> {
    terminal.write(message);
    for (index, option) in options.iter().enumerate() {
        let desc = option
            .description
            .as_deref()
            .filter(|d| !d.is_empty())
            .map(|d| format!(" - {d}"))
            .unwrap_or_default();
        terminal.write(&format!("  {}. {}{desc}", index + 1, option.label));
    }
    loop {
        let answer = terminal
            .question(&format!("Select 1-{}: ", options.len()), signal)
            .await?;
        let trimmed = answer.trim();
        if !trimmed.is_empty()
            && trimmed.chars().all(|c| c.is_ascii_digit())
            && let Ok(num) = trimmed.parse::<usize>()
            && let Some(opt) = num.checked_sub(1).and_then(|idx| options.get(idx))
        {
            return Ok(opt.id.clone());
        }
        terminal.write(&format!("Enter a number from 1 to {}.", options.len()));
    }
}

async fn refresh_provider_catalog(
    provider: &dyn Provider,
    model_runtime: &ModelRuntime,
    terminal: &dyn LoginTerminal,
    signal: &AbortSignal,
) {
    let app = *APP_NAME;
    let name = provider.name();
    let cancelled = || {
        terminal.write(&format!(
            "Skipped the {name} model catalog refresh; {app} refreshes it on next start."
        ));
    };
    let refresh_signal = AbortSignal::any(&[signal.clone(), AbortSignal::timeout(15_000)]);
    match model_runtime
        .refresh(Some(ModelsRefreshOptions {
            providers: Some(vec![provider.id().to_string()]),
            signal: Some(refresh_signal),
            ..Default::default()
        }))
        .await
    {
        Ok(result) => {
            if signal.aborted() {
                cancelled();
            } else if result.aborted {
                terminal.write(&format!(
                    "The {name} model catalog refresh timed out; {app} refreshes it on next start."
                ));
            } else if !result.errors.is_empty() {
                terminal.write(&format!(
                    "The {name} model catalog could not be refreshed; {app} retries on next start."
                ));
            }
        }
        Err(err) => {
            if signal.aborted() {
                cancelled();
            } else {
                terminal.write(&format!("The {name} model catalog could not be refreshed: {err}"));
            }
        }
    }
}

fn configure_radius_mcp_server(provider: &dyn Provider, options: &AuthLoginOptions<'_>) {
    let Some(plan) = plan_radius_mcp_server(provider.id(), options.agent_dir, options.cwd) else {
        return;
    };
    match add_mcp_server_config(&plan.mcp_path, &plan.name, &McpServerConfig::Http(plan.config)) {
        Ok(replaced) => {
            let verb = if replaced { "Updated" } else { "Added" };
            options.terminal.write(&format!(
                "{verb} MCP server \"{}\" in {} to use the {} login.",
                plan.name,
                plan.mcp_path,
                provider.name()
            ));
        }
        Err(err) => {
            options
                .terminal
                .error(&format!("Could not update {}: {err}", plan.mcp_path));
        }
    }
}

/// Run `pi auth login <provider>` in the terminal and return the process exit code.
pub async fn run_auth_login<S: AsRef<str>>(args: &[S], options: AuthLoginOptions<'_>) -> i32 {
    let model_runtime = options.model_runtime;
    let terminal = Arc::clone(&options.terminal);
    let signal = options.signal.clone();

    let request = match parse_login_request(args) {
        Ok(req) => req,
        Err(message) => {
            terminal.error(&message);
            return 1;
        }
    };

    let providers = model_runtime.get_providers();
    let Some(provider) = find_matching_provider_arcs(&providers, &request.provider_ref)
        .into_iter()
        .next()
        .cloned()
    else {
        let mut login_providers: Vec<&str> = providers
            .iter()
            .filter(|c| {
                let auth = c.auth();
                auth.oauth.is_some() || auth.api_key.as_ref().is_some_and(|ak| ak.supports_login())
            })
            .map(|c| c.id())
            .collect();
        login_providers.sort_unstable();
        terminal.error(&format!(
            "Unknown provider \"{}\".",
            replace_control_characters(&request.provider_ref)
        ));
        terminal.error(&format!("Providers: {}", login_providers.join(", ")));
        return 1;
    };

    if !terminal.interactive() {
        terminal.error(&format!("\"{}\" requires an interactive terminal", auth_login_usage()));
        return 1;
    }

    let auth_type = match select_auth_type(provider.as_ref(), request.api_key) {
        Ok(t) => t,
        Err(message) => {
            terminal.error(&message);
            return 1;
        }
    };

    let method_choice = if auth_type == AuthType::ApiKey {
        "ask"
    } else if request.browser {
        "browser"
    } else {
        "other"
    };
    let interaction: Arc<dyn AuthInteraction> = Arc::new(TerminalLoginInteraction {
        provider_id: provider.id().to_string(),
        method_choice,
        request: request.clone(),
        terminal: Arc::clone(&terminal),
        signal: signal.clone(),
        skipped_methods: Mutex::new(Vec::new()),
    });

    if let Err(err) = model_runtime
        .login(
            provider.id(),
            auth_type,
            interaction,
            Some(LoginOptions {
                get_device_id: Some(Arc::clone(&options.get_device_id)),
                agent_name: None,
            }),
        )
        .await
    {
        let message = err.to_string();
        if err.downcast_ref::<CredentialSynchronizationError>().is_some() {
            terminal.error(&format!(
                "Logged in to {}, but local model state could not be synchronized: {message}",
                provider.name()
            ));
        } else if signal.aborted() || message == "Login cancelled" {
            terminal.error("Login cancelled.");
        } else {
            terminal.error(&format!("Failed to log in to {}: {message}", provider.name()));
        }
        return 1;
    }

    let summary = if auth_type == AuthType::OAuth {
        format!("Logged in to {}", provider.name())
    } else {
        format!("Saved API key for {}", provider.name())
    };
    terminal.write(&format!("{summary}. Credentials saved to {}", get_auth_path()));
    refresh_provider_catalog(provider.as_ref(), model_runtime, terminal.as_ref(), &signal).await;
    if provider.id() == RADIUS_PROVIDER_ID {
        configure_radius_mcp_server(provider.as_ref(), &options);
    }
    0
}
