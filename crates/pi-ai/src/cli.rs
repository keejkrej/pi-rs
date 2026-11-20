//! Port of packages/ai/src/cli.ts

use std::io::{BufRead, Write};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use indexmap::IndexMap;
use pi_js::abort::AbortController;
use pi_js::error::{Error, Result};
use serde_json::{Map, Value};

use crate::auth::types::{
    AuthEvent, AuthInteraction, AuthPrompt, AuthPromptKind, GetDeviceIdFn, LoginOptions, OAuthCredential,
    ProviderAuthInteraction,
};
use crate::models::Provider;
use crate::providers::all::builtin_providers;
use crate::utils::uuid::uuidv7;

const AUTH_FILE: &str = "auth.json";

fn oauth_providers() -> Vec<Arc<dyn Provider + Send + Sync>> {
    builtin_providers()
        .into_iter()
        .filter(|p| p.auth().oauth.is_some())
        .collect()
}

fn prompt_line(question: &str) -> Result<String> {
    let mut stdout = std::io::stdout();
    write!(stdout, "{question}").map_err(|e| Error::msg(format!("Failed writing stdout: {e}")))?;
    stdout
        .flush()
        .map_err(|e| Error::msg(format!("Failed flushing stdout: {e}")))?;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| Error::msg(format!("Failed reading stdin: {e}")))?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

fn load_auth_file() -> IndexMap<String, OAuthCredential> {
    if !Path::new(AUTH_FILE).exists() {
        return IndexMap::new();
    }
    let Ok(raw) = std::fs::read_to_string(AUTH_FILE) else {
        return IndexMap::new();
    };
    let Ok(Value::Object(obj)) = pi_js::json::parse(&raw) else {
        return IndexMap::new();
    };
    let mut out = IndexMap::new();
    for (k, v) in obj {
        if let Ok(cred) = serde_json::from_value::<OAuthCredential>(v) {
            out.insert(k, cred);
        }
    }
    out
}

fn save_auth_file(auth: &IndexMap<String, OAuthCredential>) -> Result<()> {
    let mut root = Map::new();
    for (k, cred) in auth {
        let mut obj = Map::new();
        obj.insert("refresh".to_string(), Value::String(cred.refresh.clone()));
        obj.insert("access".to_string(), Value::String(cred.access.clone()));
        obj.insert("expires".to_string(), Value::from(cred.expires));
        for (ek, ev) in &cred.extra {
            obj.insert(ek.clone(), ev.clone());
        }
        root.insert(k.clone(), Value::Object(obj));
    }
    let pretty = format_json_pretty(&Value::Object(root), 0);
    write_private_auth_file(Path::new(AUTH_FILE), &pretty)
}

fn write_private_auth_file(path: &Path, content: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        if path.exists() {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| Error::msg(format!("Failed setting permissions on {AUTH_FILE}: {e}")))?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| Error::msg(format!("Failed opening {AUTH_FILE}: {e}")))?;
        file.write_all(content.as_bytes())
            .map_err(|e| Error::msg(format!("Failed writing {AUTH_FILE}: {e}")))
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, content).map_err(|e| Error::msg(format!("Failed writing {AUTH_FILE}: {e}")))
    }
}

fn format_json_pretty(value: &Value, indent: usize) -> String {
    match value {
        Value::Object(map) => {
            if map.is_empty() {
                return "{}".to_string();
            }
            let pad = " ".repeat(indent + 2);
            let close_pad = " ".repeat(indent);
            let mut parts = Vec::with_capacity(map.len());
            for (k, v) in map {
                let key = pi_js::json::stringify(&Value::String(k.clone()));
                let rendered = format_json_pretty(v, indent + 2);
                parts.push(format!("{pad}{key}: {rendered}"));
            }
            format!("{{\n{}\n{close_pad}}}", parts.join(",\n"))
        }
        Value::Array(items) => {
            if items.is_empty() {
                return "[]".to_string();
            }
            let pad = " ".repeat(indent + 2);
            let close_pad = " ".repeat(indent);
            let mut parts = Vec::with_capacity(items.len());
            for item in items {
                let rendered = format_json_pretty(item, indent + 2);
                parts.push(format!("{pad}{rendered}"));
            }
            format!("[\n{}\n{close_pad}]", parts.join(",\n"))
        }
        _ => pi_js::json::stringify(value),
    }
}

fn answer_prompt(prompt: &AuthPrompt) -> Result<String> {
    match &prompt.kind {
        AuthPromptKind::Select { message, options } => {
            let mut out = std::io::stdout();
            writeln!(out, "\n{message}").map_err(|e| Error::msg(format!("Failed writing stdout: {e}")))?;
            for (idx, opt) in options.iter().enumerate() {
                writeln!(out, "  {}. {}", idx + 1, opt.label)
                    .map_err(|e| Error::msg(format!("Failed writing stdout: {e}")))?;
            }
            let answer = prompt_line(&format!("Enter number (1-{}): ", options.len()))?;
            let choice_1based = answer
                .trim()
                .parse::<usize>()
                .map_err(|_| Error::msg("Invalid selection"))?;
            let selected = choice_1based
                .checked_sub(1)
                .and_then(|idx| options.get(idx))
                .ok_or_else(|| Error::msg("Invalid selection"))?;
            Ok(selected.id.clone())
        }
        AuthPromptKind::Text { message, placeholder }
        | AuthPromptKind::Secret { message, placeholder }
        | AuthPromptKind::ManualCode { message, placeholder } => {
            let suffix = placeholder.as_deref().map(|p| format!(" ({p})")).unwrap_or_default();
            prompt_line(&format!("{message}{suffix}: "))
        }
    }
}

struct CliAuthInteraction;

#[async_trait::async_trait]
impl AuthInteraction for CliAuthInteraction {
    async fn prompt(&self, prompt: AuthPrompt) -> Result<String> {
        answer_prompt(&prompt)
    }

    fn notify(&self, event: AuthEvent) {
        let mut out = std::io::stdout();
        match event {
            AuthEvent::AuthUrl { url, instructions } => {
                let _ = writeln!(out, "\nOpen this URL in your browser:\n{url}");
                if let Some(inst) = instructions {
                    let _ = writeln!(out, "{inst}");
                }
            }
            AuthEvent::DeviceCode {
                verification_uri,
                user_code,
                ..
            } => {
                let _ = writeln!(out, "\nOpen this URL in your browser:\n{verification_uri}");
                let _ = writeln!(out, "Enter code: {user_code}");
            }
            AuthEvent::Info { message, links } => {
                let _ = writeln!(out, "{message}");
                if let Some(links) = links {
                    for link in links {
                        if let Some(label) = link.label {
                            let _ = writeln!(out, "  {label}: {}", link.url);
                        } else {
                            let _ = writeln!(out, "  {}", link.url);
                        }
                    }
                }
            }
            AuthEvent::Progress { message } => {
                let _ = writeln!(out, "{message}");
            }
        }
    }
}

async fn login_provider(provider_id: &str, providers: &[Arc<dyn Provider + Send + Sync>]) -> Result<()> {
    let provider = providers
        .iter()
        .find(|p| p.id() == provider_id)
        .ok_or_else(|| Error::msg(format!("Unknown provider: {provider_id}")))?;
    let oauth = provider
        .auth()
        .oauth
        .clone()
        .ok_or_else(|| Error::msg(format!("Provider {provider_id} has no OAuth flow")))?;

    let ctrl = AbortController::new();
    let interaction = ProviderAuthInteraction::new(Arc::new(CliAuthInteraction), ctrl.signal());
    let get_device_id: GetDeviceIdFn =
        Arc::new(|| uuidv7(None).unwrap_or_else(|_| "00000000-0000-7000-8000-000000000000".to_string()));
    let credential = oauth
        .login(
            interaction,
            Some(LoginOptions {
                get_device_id: Some(get_device_id),
                agent_name: None,
            }),
        )
        .await?;

    let mut auth = load_auth_file();
    auth.insert(provider_id.to_string(), credential);
    save_auth_file(&auth)?;
    let _ = writeln!(std::io::stdout(), "\nCredentials saved to {AUTH_FILE}");
    Ok(())
}

async fn run_async() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let providers = oauth_providers();
    let command = args.first().map(String::as_str);

    match command {
        None | Some("help" | "--help" | "-h") => {
            let mut list_lines = Vec::new();
            for p in &providers {
                list_lines.push(format!("  {:<20} {}", p.id(), p.name()));
            }
            let _ = writeln!(
                std::io::stdout(),
                "Usage: pi-ai <command> [provider]\n\nCommands:\n  login [provider]  Login to an OAuth provider\n  list              List available providers\n\nProviders:\n{}",
                list_lines.join("\n")
            );
            Ok(())
        }
        Some("list") => {
            let mut out = std::io::stdout();
            for p in &providers {
                let _ = writeln!(out, "{:<20} {}", p.id(), p.name());
            }
            Ok(())
        }
        Some("login") => {
            let provider_id = if let Some(id) = args.get(1) {
                id.clone()
            } else {
                let mut out = std::io::stdout();
                for (idx, p) in providers.iter().enumerate() {
                    let _ = writeln!(out, "  {}. {}", idx + 1, p.name());
                }
                let answer = prompt_line(&format!("Enter number (1-{}): ", providers.len()))?;
                let choice = answer
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| Error::msg("Invalid selection"))?;
                choice
                    .checked_sub(1)
                    .and_then(|idx| providers.get(idx))
                    .map(|p| p.id().to_string())
                    .ok_or_else(|| Error::msg("Unknown provider"))?
            };
            if !providers.iter().any(|p| p.id() == provider_id) {
                return Err(Error::msg(format!("Unknown provider: {provider_id}")));
            }
            login_provider(&provider_id, &providers).await
        }
        Some(other) => Err(Error::msg(format!("Unknown command: {other}"))),
    }
}

pub fn run() -> ExitCode {
    pi_js::ensure_crypto_provider();
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(r) => r,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "Error: {e}");
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(run_async()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Error::Exit(c)) => ExitCode::from(c as u8),
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "Error: {e}");
            ExitCode::FAILURE
        }
    }
}
