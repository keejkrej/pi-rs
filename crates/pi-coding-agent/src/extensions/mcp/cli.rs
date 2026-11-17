//! Port of packages/coding-agent/src/extensions/mcp/cli.ts

use std::io::IsTerminal;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

use crate::extensions::mcp::config::{
    LoadMcpConfigOptions, LoadedMcpConfig, McpExposure, McpServerConfig, McpServerEntry, add_mcp_server_config,
    get_mcp_tool_exposure, load_mcp_config, remove_mcp_server_config, validate_mcp_server_config,
};
use crate::extensions::mcp::log::McpServerLog;
use crate::extensions::mcp::oauth::{
    McpOAuthCredentialStore, McpSignInCancelledError, McpSignInPrompt, SignInMcpServerOptions, sign_in_mcp_server,
};
use crate::extensions::mcp::runtime::{
    McpServerConnection, McpServerConnectionOptions, ServerState, create_default_transport,
};
use crate::extensions::wire::{APP_NAME, CONFIG_DIR_NAME, ProjectTrustStore, open_browser};
use pi_js::Result;
use pi_js::abort::AbortSignal;

const DEFAULT_LOGIN_TIMEOUT_SECONDS: f64 = 300.0;

fn help_text() -> String {
    format!(
        "\x1b[1mUsage:\x1b[22m\n  {APP_NAME} mcp add <server> [options] -- <command> [args...]\n  {APP_NAME} mcp add <server> [options] --url <url>\n  {APP_NAME} mcp remove <server> [-l]\n  {APP_NAME} mcp list [--json]\n  {APP_NAME} mcp login <server> [--timeout <seconds>]\n  {APP_NAME} mcp logout <server>\n\nConfigure and check MCP servers and sign in to OAuth servers without starting a session.\nReads ~/{CONFIG_DIR_NAME}/agent/mcp.json and, in trusted projects, {CONFIG_DIR_NAME}/mcp.json.\n\nCommands:\n  add <server>            Add or replace a server in mcp.json\n  remove <server>         Remove a server from mcp.json\n  list                    Show state, tools, and errors (exits 1 on failure)\n  login <server>          Sign in through the browser\n  logout <server>         Delete the stored OAuth credentials\n\nOptions for add and remove:\n  -l, --local             Use {CONFIG_DIR_NAME}/mcp.json in the current project instead of the global file\n\nOptions for add:\n  --url <url>             Streamable HTTP server URL (instead of a command)\n  --env <KEY=VALUE>       Environment variable for a stdio server (repeatable)\n  --cwd <dir>             Working directory for a stdio server\n  --header <KEY=VALUE>    HTTP header (repeatable)\n  --bearer-token-env-var <NAME>\n                          Send \"Authorization: Bearer ${{NAME}}\"\n  --oauth-client-id <id>  Pre-registered OAuth client id\n  --oauth-client-secret <secret>\n                          OAuth client secret (may be ${{NAME}} or !command)\n  --oauth-callback-port <port>\n                          Fixed OAuth callback port\n  --oauth-client-name <name>\n                          Client name sent when registering with the OAuth server\n  --exposure <mode>       codemode (default), deferred, direct, or hidden\n  --description <text>    What the server offers, shown in the system prompt\n\nOther options:\n  --json                  Print the list as JSON\n  --timeout <seconds>     How long login waits for the browser (default: 300)"
    )
}

fn help_hint() -> String {
    format!("\x1b[2mUse \"{APP_NAME} mcp --help\" for usage.\x1b[22m")
}

pub type LogFn = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone)]
pub struct McpCommandOptions {
    pub cwd: String,
    pub agent_dir: String,
    pub credentials: Option<McpOAuthCredentialStore>,
    pub open_url: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    pub log: Option<LogFn>,
    pub error: Option<LogFn>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerReport {
    name: String,
    scope: String,
    source: String,
    enabled: bool,
    exposure: String,
    transport: String,
    state: String,
    tools: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_exposure: Option<IndexMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resources: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resource_templates: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn exposure_str(exp: McpExposure) -> &'static str {
    match exp {
        McpExposure::Codemode => "codemode",
        McpExposure::Deferred => "deferred",
        McpExposure::Direct => "direct",
        McpExposure::Hidden => "hidden",
    }
}

fn state_str(st: ServerState) -> &'static str {
    match st {
        ServerState::Connecting => "connecting",
        ServerState::Connected => "connected",
        ServerState::Disconnected => "disconnected",
        ServerState::NeedsAuth => "needs-auth",
        ServerState::Failed => "failed",
        ServerState::Closed => "closed",
    }
}

fn describe_transport(entry: &McpServerEntry) -> String {
    match &entry.config {
        McpServerConfig::Http(h) => h.url.clone(),
        McpServerConfig::Stdio(s) => {
            let mut parts = vec![s.command.clone()];
            if let Some(ref args) = s.args {
                parts.extend(args.iter().cloned());
            }
            parts.join(" ")
        }
    }
}

fn create_connection(
    entry: McpServerEntry,
    options: &McpCommandOptions,
    credentials: &McpOAuthCredentialStore,
) -> Arc<McpServerConnection> {
    let log_path = pi_js::path::join(&[&options.agent_dir, "mcp.log"]);
    McpServerConnection::new(McpServerConnectionOptions {
        entry,
        cwd: options.cwd.clone(),
        create_transport: Arc::new(create_default_transport),
        credentials: credentials.clone(),
        provider_token: None,
        on_tools: Arc::new(|_| {}),
        on_change: None,
        log: Some(Arc::new(McpServerLog::new(log_path))),
    })
}

#[derive(Clone, Copy)]
enum OptKind {
    Flag,
    Value,
    List,
}

struct ParsedOptions {
    positional: Vec<String>,
    values: IndexMap<String, Option<String>>,
    lists: IndexMap<String, Vec<String>>,
}

fn parse_options(
    args: &[String],
    known: &IndexMap<&'static str, OptKind>,
    error: &LogFn,
    max_positionals: usize,
) -> Option<ParsedOptions> {
    let mut positional = Vec::new();
    let mut values = IndexMap::new();
    let mut lists: IndexMap<String, Vec<String>> = IndexMap::new();
    let mut index = 0usize;
    while index < args.len() {
        let raw = &args[index];
        let arg = if raw == "-l" { "--local" } else { raw.as_str() };
        if arg == "--" || positional.len() >= max_positionals {
            let start = if arg == "--" { index + 1 } else { index };
            positional.extend(args[start..].iter().cloned());
            break;
        }
        if !arg.starts_with("--") {
            positional.push(arg.to_string());
            index += 1;
            continue;
        }
        let name = &arg[2..];
        let Some(&kind) = known.get(name) else {
            error(&format!("Unknown option {arg}.\n{}", help_hint()));
            return None;
        };
        match kind {
            OptKind::Flag => {
                values.insert(name.to_string(), None);
            }
            OptKind::Value | OptKind::List => {
                index += 1;
                let Some(val) = args.get(index) else {
                    error(&format!("{arg} needs a value."));
                    return None;
                };
                if matches!(kind, OptKind::List) {
                    lists.entry(name.to_string()).or_default().push(val.clone());
                } else {
                    values.insert(name.to_string(), Some(val.clone()));
                }
            }
        }
        index += 1;
    }
    Some(ParsedOptions {
        positional,
        values,
        lists,
    })
}

fn parse_pairs(option: &str, pairs: Option<&Vec<String>>, error: &LogFn) -> Option<serde_json::Map<String, Value>> {
    let mut map = serde_json::Map::new();
    let Some(pairs) = pairs else {
        return Some(map);
    };
    for pair in pairs {
        let Some(sep) = pair.find('=') else {
            error(&format!("--{option} expects KEY=VALUE, got \"{pair}\"."));
            return None;
        };
        if sep == 0 {
            error(&format!("--{option} expects KEY=VALUE, got \"{pair}\"."));
            return None;
        }
        map.insert(pair[..sep].to_string(), Value::String(pair[sep + 1..].to_string()));
    }
    Some(map)
}

fn add(args: &[String], project_config: &str, options: &McpCommandOptions, log: &LogFn, error: &LogFn) -> i32 {
    let usage = format!(
        "Usage: {APP_NAME} mcp add <server> [options] (--url <url> | -- <command> [args...])\n{}",
        help_hint()
    );
    let mut known = IndexMap::new();
    known.insert("local", OptKind::Flag);
    known.insert("url", OptKind::Value);
    known.insert("env", OptKind::List);
    known.insert("cwd", OptKind::Value);
    known.insert("header", OptKind::List);
    known.insert("bearer-token-env-var", OptKind::Value);
    known.insert("oauth-client-id", OptKind::Value);
    known.insert("oauth-client-secret", OptKind::Value);
    known.insert("oauth-callback-port", OptKind::Value);
    known.insert("oauth-client-name", OptKind::Value);
    known.insert("exposure", OptKind::Value);
    known.insert("description", OptKind::Value);

    let Some(parsed) = parse_options(args, &known, error, 2) else {
        return 1;
    };
    let name = parsed.positional.first().cloned().unwrap_or_default();
    let command: Vec<String> = parsed.positional.iter().skip(1).cloned().collect();
    let url = parsed.values.get("url").and_then(|v| v.as_deref());
    if name.is_empty() || (url.is_none() == command.is_empty()) {
        error(&usage);
        return 1;
    }

    let get_val = |k: &str| parsed.values.get(k).and_then(|v| v.as_deref());
    let http_only = [
        "header",
        "bearer-token-env-var",
        "oauth-client-id",
        "oauth-client-secret",
        "oauth-callback-port",
        "oauth-client-name",
    ];
    let stdio_only = ["env", "cwd"];
    let check_set: &[&str] = if url.is_none() { &http_only } else { &stdio_only };
    if let Some(misplaced) = check_set
        .iter()
        .find(|&&o| parsed.values.contains_key(o) || parsed.lists.contains_key(o))
    {
        let side = if url.is_none() {
            "HTTP servers (--url)"
        } else {
            "stdio servers"
        };
        error(&format!("--{misplaced} only applies to {side}."));
        return 1;
    }

    let mut cfg_obj = serde_json::Map::new();
    if let Some(url_str) = url {
        let Some(mut headers) = parse_pairs("header", parsed.lists.get("header"), error) else {
            return 1;
        };
        if let Some(bearer) = get_val("bearer-token-env-var") {
            headers.insert(
                "Authorization".to_string(),
                Value::String(format!("Bearer ${{{bearer}}}")),
            );
        }
        let mut oauth = serde_json::Map::new();
        if let Some(cid) = get_val("oauth-client-id") {
            oauth.insert("clientId".to_string(), Value::String(cid.to_string()));
        }
        if let Some(csec) = get_val("oauth-client-secret") {
            oauth.insert("clientSecret".to_string(), Value::String(csec.to_string()));
        }
        if let Some(port_s) = get_val("oauth-callback-port") {
            let n = port_s.parse::<f64>().unwrap_or(f64::NAN);
            if n.is_finite() {
                oauth.insert("callbackPort".to_string(), json!(n));
            } else {
                oauth.insert("callbackPort".to_string(), Value::String(port_s.to_string()));
            }
        }
        if let Some(cname) = get_val("oauth-client-name") {
            oauth.insert("clientName".to_string(), Value::String(cname.to_string()));
        }
        cfg_obj.insert("url".to_string(), Value::String(url_str.to_string()));
        if !headers.is_empty() {
            cfg_obj.insert("headers".to_string(), Value::Object(headers));
        }
        if !oauth.is_empty() {
            cfg_obj.insert("oauth".to_string(), Value::Object(oauth));
        }
    } else {
        let Some(env) = parse_pairs("env", parsed.lists.get("env"), error) else {
            return 1;
        };
        cfg_obj.insert("command".to_string(), Value::String(command[0].clone()));
        if command.len() > 1 {
            cfg_obj.insert(
                "args".to_string(),
                Value::Array(command[1..].iter().cloned().map(Value::String).collect()),
            );
        }
        if !env.is_empty() {
            cfg_obj.insert("env".to_string(), Value::Object(env));
        }
        if let Some(cwd) = get_val("cwd") {
            cfg_obj.insert("cwd".to_string(), Value::String(cwd.to_string()));
        }
    }

    if let Some(exp) = get_val("exposure") {
        cfg_obj.insert("exposure".to_string(), Value::String(exp.to_string()));
    }
    if let Some(desc) = get_val("description") {
        cfg_obj.insert("description".to_string(), Value::String(desc.to_string()));
    }

    let (validated, auth_provider) = match validate_mcp_server_config(&name, &Value::Object(cfg_obj)) {
        Ok(v) => v,
        Err(msg) => {
            error(&msg);
            return 1;
        }
    };

    let project = parsed.values.contains_key("local");
    let path = if project {
        project_config.to_string()
    } else {
        pi_js::path::join(&[&options.agent_dir, "mcp.json"])
    };
    let scope = if project { "project" } else { "global" };
    let replaced = match add_mcp_server_config(&path, &name, &validated, auth_provider.as_deref()) {
        Ok(r) => r,
        Err(err) => {
            error(&format!("Could not update {path}: {}", err.message()));
            return 1;
        }
    };
    let action_word = if replaced { "Replaced" } else { "Added" };
    log(&format!("{action_word} {scope} MCP server \"{name}\" in {path}."));
    if project && ProjectTrustStore::new(&options.agent_dir).get(&options.cwd) != Some(true) {
        log(&format!(
            "The project is not trusted, so {path} is ignored until you start {APP_NAME} in the project and trust it."
        ));
    }
    let may_need_sign_in = matches!(
        &validated,
        McpServerConfig::Http(h) if !h.headers.as_ref().is_some_and(|hdrs| hdrs.keys().any(|k| k.eq_ignore_ascii_case("authorization")))
    );
    let hint = if may_need_sign_in {
        format!(". If it requires sign-in: {APP_NAME} mcp login {name}")
    } else {
        String::new()
    };
    log(&format!("Check it with: {APP_NAME} mcp list{hint}"));
    0
}

fn remove(args: &[String], project_config: &str, options: &McpCommandOptions, log: &LogFn, error: &LogFn) -> i32 {
    let mut known = IndexMap::new();
    known.insert("local", OptKind::Flag);
    let Some(parsed) = parse_options(args, &known, error, usize::MAX) else {
        return 1;
    };
    if parsed.positional.len() != 1 {
        error(&format!("Usage: {APP_NAME} mcp remove <server> [-l]\n{}", help_hint()));
        return 1;
    }
    let name = &parsed.positional[0];
    let project = parsed.values.contains_key("local");
    let global_config = pi_js::path::join(&[&options.agent_dir, "mcp.json"]);
    let path = if project { project_config } else { &global_config };
    let scope = if project { "project" } else { "global" };
    let removed = match remove_mcp_server_config(path, name) {
        Ok(r) => r,
        Err(err) => {
            error(&format!("Could not update {path}: {}", err.message()));
            return 1;
        }
    };
    if removed {
        log(&format!("Removed {scope} MCP server \"{name}\" from {path}."));
        return 0;
    }
    let other_loaded = load_mcp_config(&LoadMcpConfigOptions {
        agent_dir: options.agent_dir.clone(),
        cwd: options.cwd.clone(),
        project_trusted: true,
    });
    let other = other_loaded
        .servers
        .into_iter()
        .find(|s| s.name == *name && s.scope.map(|sc| sc.as_str()) != Some(scope));
    let extra = match other {
        Some(o) => {
            let flag = if o.scope == Some(crate::extensions::mcp::config::McpScope::Project) {
                "; use --local"
            } else {
                "; omit --local"
            };
            format!(" It is defined in {}{flag}.", o.source)
        }
        None => String::new(),
    };
    error(&format!("No {scope} MCP server named \"{name}\" in {path}.{extra}"));
    1
}

async fn list(
    loaded: &LoadedMcpConfig,
    json_mode: bool,
    untrusted_note: Option<&str>,
    options: &McpCommandOptions,
    credentials: &McpOAuthCredentialStore,
    log: &LogFn,
) -> i32 {
    let futs: Vec<_> = loaded
        .servers
        .iter()
        .map(|entry| {
            let entry = entry.clone();
            let options = options.clone();
            let credentials = credentials.clone();
            async move {
                let exposure_val = match &entry.config {
                    McpServerConfig::Stdio(s) => s.exposure,
                    McpServerConfig::Http(h) => h.exposure,
                }
                .unwrap_or(McpExposure::Codemode);
                let enabled = match &entry.config {
                    McpServerConfig::Stdio(s) => s.enabled,
                    McpServerConfig::Http(h) => h.enabled,
                } != Some(false);
                let mut report = ServerReport {
                    name: entry.name.clone(),
                    scope: entry
                        .scope
                        .map(|s| s.as_str().to_string())
                        .unwrap_or_else(|| "global".to_string()),
                    source: entry.source.clone(),
                    enabled,
                    exposure: exposure_str(exposure_val).to_string(),
                    transport: describe_transport(&entry),
                    state: "disabled".to_string(),
                    tools: Vec::new(),
                    tool_exposure: None,
                    resources: None,
                    resource_templates: None,
                    error: None,
                };
                if !enabled {
                    return report;
                }
                let conn = create_connection(entry.clone(), &options, &credentials);
                let _ = conn.get_client().await;
                let st = conn.state();
                report.state = state_str(st).to_string();
                let tools = conn.tools();
                report.tools = tools.iter().map(|t| t.name.clone()).collect();
                let mut overrides = IndexMap::new();
                for tool in &tools {
                    let exp = get_mcp_tool_exposure(&entry.config, &tool.name);
                    if exposure_str(exp) != report.exposure {
                        overrides.insert(tool.name.clone(), exposure_str(exp).to_string());
                    }
                }
                if !overrides.is_empty() {
                    report.tool_exposure = Some(overrides);
                }
                if conn.has_resources() {
                    report.resources = Some(conn.resources().len());
                    report.resource_templates = Some(conn.resource_templates().len());
                }
                if st != ServerState::Connected {
                    report.error = conn.error();
                }
                conn.close().await;
                report
            }
        })
        .collect();
    let reports = futures::future::join_all(futs).await;
    let failed = !loaded.errors.is_empty() || reports.iter().any(|r| r.enabled && r.state != "connected");

    if json_mode {
        let mut obj = serde_json::Map::new();
        if let Ok(srv_val) = pi_js::json::parse(&pi_js::json::stringify(&reports)) {
            obj.insert("servers".to_string(), srv_val);
        }
        obj.insert(
            "errors".to_string(),
            Value::Array(loaded.errors.iter().cloned().map(Value::String).collect()),
        );
        if let Some(note) = untrusted_note {
            obj.insert("note".to_string(), Value::String(note.to_string()));
        }
        log(&pi_js::json::stringify_pretty(&Value::Object(obj), "  "));
        return if failed { 1 } else { 0 };
    }

    if reports.is_empty() && loaded.errors.is_empty() {
        let global_path = pi_js::path::join(&[&options.agent_dir, "mcp.json"]);
        log(&format!(
            "No MCP servers configured. Add them to {global_path} or .pi/mcp.json."
        ));
    }
    for report in &reports {
        let state_text = if report.state == "connected" {
            let noun = if report.tools.len() == 1 { "tool" } else { "tools" };
            format!("connected, {} {noun}", report.tools.len())
        } else if report.state == "needs-auth" {
            "needs sign-in".to_string()
        } else {
            report.state.clone()
        };
        log(&format!(
            "{}: {state_text} ({}, {})",
            report.name, report.exposure, report.scope
        ));
        log(&format!("  {}", report.transport));
        if report.state == "needs-auth" {
            log(&format!("  sign in with: {APP_NAME} mcp login {}", report.name));
        }
        if !report.tools.is_empty() {
            let tools: Vec<String> = report
                .tools
                .iter()
                .map(|t| {
                    if let Some(exp) = report.tool_exposure.as_ref().and_then(|m| m.get(t)) {
                        format!("{t} [{exp}]")
                    } else {
                        t.clone()
                    }
                })
                .collect();
            log(&format!("  tools: {}", tools.join(", ")));
        }
        if let Some(res_count) = report.resources {
            log(&format!(
                "  resources: {res_count}, URI templates: {}",
                report.resource_templates.unwrap_or(0)
            ));
        }
        if let Some(ref err) = report.error {
            let indented = err.split('\n').collect::<Vec<_>>().join("\n  ");
            log(&format!("  {indented}"));
        }
    }
    for cfg_err in &loaded.errors {
        log(&format!("config error: {cfg_err}"));
    }
    if let Some(note) = untrusted_note {
        log(note);
    }
    if failed { 1 } else { 0 }
}

struct CliSignInPrompt {
    server_name: String,
    timeout_ms: u64,
    interactive: bool,
    open_url: Arc<dyn Fn(&str) + Send + Sync>,
    log: LogFn,
}

#[async_trait]
impl McpSignInPrompt for CliSignInPrompt {
    fn show_authorization_url(&self, url: &url::Url) {
        (self.log)(&format!(
            "Sign in to MCP server \"{}\" in your browser:\n{}",
            self.server_name,
            url.as_str()
        ));
        (self.open_url)(url.as_str());
    }

    async fn prompt_for_redirect_url(&self, signal: AbortSignal) -> Result<Option<String>> {
        wait_for_redirect_url(signal, self.timeout_ms, self.interactive).await
    }
}

async fn wait_for_redirect_url(signal: AbortSignal, timeout_ms: u64, interactive: bool) -> Result<Option<String>> {
    let token = signal.token();
    if !interactive {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), token.cancelled()).await;
        return Ok(None);
    }
    let mut stderr = tokio::io::stderr();
    let _ = stderr
        .write_all(b"If the browser cannot reach this machine, paste the URL it was redirected to: ")
        .await;
    let _ = stderr.flush().await;
    let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
    let mut line = String::new();
    tokio::select! {
        _ = token.cancelled() => Ok(None),
        _ = tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)) => Ok(None),
        res = reader.read_line(&mut line) => match res {
            Ok(0) | Err(_) => Ok(None),
            Ok(_) => Ok(Some(line)),
        }
    }
}

async fn login(
    entry: &McpServerEntry,
    connection: &Arc<McpServerConnection>,
    url: &str,
    timeout_ms: u64,
    options: &McpCommandOptions,
    credentials: &McpOAuthCredentialStore,
    log: &LogFn,
    error: &LogFn,
) -> i32 {
    let name = &entry.name;
    match connection.get_client().await {
        Ok(_) => {
            log(&format!(
                "Already signed in to MCP server \"{name}\" ({} tools).",
                connection.tools().len()
            ));
            return 0;
        }
        Err(_) => {
            if connection.state() != ServerState::NeedsAuth {
                error(&format!(
                    "MCP server \"{name}\" failed to connect: {}",
                    connection.error().unwrap_or_else(|| "unknown error".to_string())
                ));
                return 1;
            }
        }
    }

    let open_url: Arc<dyn Fn(&str) + Send + Sync> = options.open_url.clone().unwrap_or_else(|| {
        Arc::new(|u| {
            let _ = open_browser(u);
        })
    });
    let interactive = std::io::stdin().is_terminal() && options.open_url.is_none();
    let Ok(store) = credentials.for_server(name, url) else {
        error("Invalid server URL for OAuth credential store.");
        return 1;
    };
    let settings = match connection.oauth_settings() {
        Ok(s) => s,
        Err(err) => {
            error(&format!("Sign-in to MCP server \"{name}\" failed: {}", err.message()));
            return 1;
        }
    };
    let res = sign_in_mcp_server(SignInMcpServerOptions {
        server_url: url.to_string(),
        store,
        settings,
        challenge: connection.challenge(),
        prompt: Arc::new(CliSignInPrompt {
            server_name: name.clone(),
            timeout_ms,
            interactive,
            open_url,
            log: Arc::clone(log),
        }),
    })
    .await;

    if let Err(err) = res {
        if err.downcast_ref::<McpSignInCancelledError>().is_some() {
            let secs = (timeout_ms as f64 / 1000.0).round() as i64;
            error(&format!(
                "Sign-in to MCP server \"{name}\" was cancelled or not completed within {secs} seconds."
            ));
        } else {
            error(&format!("Sign-in to MCP server \"{name}\" failed: {}", err.message()));
        }
        return 1;
    }

    connection.clear_challenge();
    if let Err(err) = connection.reconnect().await {
        error(&format!("Signed in, but {}", err.message()));
        return 1;
    }
    log(&format!(
        "Signed in to MCP server \"{name}\" ({} tools).",
        connection.tools().len()
    ));
    0
}

pub async fn run_mcp_command(args: &[String], options: McpCommandOptions) -> Result<i32> {
    let log: LogFn = options
        .log
        .clone()
        .unwrap_or_else(|| Arc::new(|line| pi_js::console::log(line)));
    let error: LogFn = options
        .error
        .clone()
        .unwrap_or_else(|| Arc::new(|line| pi_js::console::error(line)));

    let Some(command) = args.first().map(String::as_str) else {
        log(&help_text());
        return Ok(0);
    };
    if command == "help" || args.iter().any(|a| a == "--help" || a == "-h") {
        log(&help_text());
        return Ok(0);
    }
    let rest = &args[1..];
    let project_config = pi_js::path::join(&[&options.cwd, CONFIG_DIR_NAME, "mcp.json"]);
    if command == "add" {
        return Ok(add(rest, &project_config, &options, &log, &error));
    }
    if command == "remove" {
        return Ok(remove(rest, &project_config, &options, &log, &error));
    }

    let project_trusted = ProjectTrustStore::new(&options.agent_dir).get(&options.cwd) == Some(true);
    let loaded = load_mcp_config(&LoadMcpConfigOptions {
        agent_dir: options.agent_dir.clone(),
        cwd: options.cwd.clone(),
        project_trusted,
    });
    let untrusted_note = if !project_trusted && Path::new(&project_config).exists() {
        Some(format!(
            "{project_config} is ignored because the project is not trusted. Start {APP_NAME} in the project to trust it."
        ))
    } else {
        None
    };
    let credentials = options
        .credentials
        .clone()
        .unwrap_or_else(|| McpOAuthCredentialStore::new(None, None));

    match command {
        "list" => {
            let mut known = IndexMap::new();
            known.insert("json", OptKind::Flag);
            let Some(parsed) = parse_options(rest, &known, &error, usize::MAX) else {
                return Ok(1);
            };
            if !parsed.positional.is_empty() {
                error(&format!("Usage: {APP_NAME} mcp list [--json]\n{}", help_hint()));
                return Ok(1);
            }
            Ok(list(
                &loaded,
                parsed.values.contains_key("json"),
                untrusted_note.as_deref(),
                &options,
                &credentials,
                &log,
            )
            .await)
        }
        "login" | "logout" => {
            let mut known = IndexMap::new();
            if command == "login" {
                known.insert("timeout", OptKind::Value);
            }
            let Some(parsed) = parse_options(rest, &known, &error, usize::MAX) else {
                return Ok(1);
            };
            if parsed.positional.len() != 1 {
                error(&format!("Usage: {APP_NAME} mcp {command} <server>\n{}", help_hint()));
                return Ok(1);
            }
            let name = &parsed.positional[0];
            let Some(entry) = loaded.servers.iter().find(|s| s.name == *name) else {
                let note = untrusted_note.as_deref().map(|n| format!(" {n}")).unwrap_or_default();
                let configured = if loaded.servers.is_empty() {
                    "none".to_string()
                } else {
                    loaded
                        .servers
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                error(&format!(
                    "No MCP server named \"{name}\".{note} Configured: {configured}."
                ));
                return Ok(1);
            };
            let connection = create_connection(entry.clone(), &options, &credentials);
            let Some(url) = connection.oauth_url().map(String::from) else {
                error(&format!(
                    "MCP server \"{name}\" does not use OAuth. Only HTTP servers without an Authorization header do."
                ));
                return Ok(1);
            };
            if command == "logout" {
                let removed = credentials.remove(name, &url);
                let msg = if removed {
                    format!("Signed out of MCP server \"{name}\".")
                } else {
                    format!("No stored credentials for MCP server \"{name}\".")
                };
                log(&msg);
                return Ok(0);
            }
            let timeout = match parsed.values.get("timeout").and_then(|v| v.as_deref()) {
                Some(s) => s.parse::<f64>().unwrap_or(f64::NAN),
                None => DEFAULT_LOGIN_TIMEOUT_SECONDS,
            };
            if !timeout.is_finite() || timeout <= 0.0 {
                error("--timeout must be a positive number of seconds.");
                return Ok(1);
            }
            let code = login(
                entry,
                &connection,
                &url,
                (timeout * 1000.0) as u64,
                &options,
                &credentials,
                &log,
                &error,
            )
            .await;
            connection.close().await;
            Ok(code)
        }
        other => {
            error(&format!("Unknown mcp command \"{other}\".\n{}", help_hint()));
            Ok(1)
        }
    }
}
