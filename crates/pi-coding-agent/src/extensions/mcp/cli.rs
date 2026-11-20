//! Port of packages/coding-agent/src/extensions/mcp/cli.ts

use std::io::IsTerminal;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::Serialize;
use serde_json::{Value, json};

use crate::config::{APP_NAME, CONFIG_DIR_NAME};
use crate::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use crate::core::trust_manager::ProjectTrustStore;
use crate::extensions::mcp::config::{
    LoadMcpConfigOptions, LoadedMcpConfig, McpExposure, McpScope, McpServerConfig, McpServerEntry,
    add_mcp_server_config, get_mcp_tool_exposure, load_mcp_config, remove_mcp_server_config,
    validate_mcp_server_config,
};
use crate::extensions::mcp::log::McpServerLog;
use crate::extensions::mcp::oauth::{
    McpOAuthCredentialStore, McpSignInCancelledError, McpSignInPrompt, SignInMcpServerOptions, sign_in_mcp_server,
};
use crate::extensions::mcp::runtime::{
    McpServerConnection, McpServerConnectionOptions, ProviderTokenFn, ServerState, create_default_transport,
};
use crate::utils::open_browser::open_browser;
use crate::utils::terminal_question::question_terminal;
use pi_js::Result;
use pi_js::abort::AbortSignal;

const DEFAULT_LOGIN_TIMEOUT_SECONDS: f64 = 300.0;

fn help_text() -> String {
    let app = *APP_NAME;
    let cfg = *CONFIG_DIR_NAME;
    format!(
        "\x1b[1mUsage:\x1b[22m\n  {app} mcp add <server> [options] -- <command> [args...]\n  {app} mcp add <server> [options] --url <url>\n  {app} mcp remove <server> [-l]\n  {app} mcp list [--json]\n  {app} mcp login <server> [-l] [--timeout <seconds>]\n  {app} mcp logout <server> [-l]\n\nConfigure and check MCP servers and sign in to OAuth servers without starting a session.\nReads ~/{cfg}/agent/mcp.json and, in trusted projects, {cfg}/mcp.json.\n\nCommands:\n  add <server>            Add or replace a server in mcp.json\n  remove <server>         Remove a server from mcp.json\n  list                    Show state, tools, and errors (exits 1 on failure)\n  login <server>          Sign in through the browser\n  logout <server>         Delete the stored OAuth credentials\n\nOptions for add, remove, login, and logout:\n  -l, --local             Use {cfg}/mcp.json in the current project instead of the global file\n\nOptions for add:\n  --url <url>             Streamable HTTP server URL (instead of a command)\n  --env <KEY=VALUE>       Environment variable for a stdio server (repeatable)\n  --cwd <dir>             Working directory for a stdio server\n  --header <KEY=VALUE>    HTTP header (repeatable)\n  --bearer-token-env-var <NAME>\n                          Send \"Authorization: Bearer ${{NAME}}\"\n  --oauth-client-id <id>  Pre-registered OAuth client id\n  --oauth-client-secret <secret>\n                          OAuth client secret (may be ${{NAME}} or !command)\n  --oauth-callback-port <port>\n                          Fixed OAuth callback port\n  --oauth-client-name <name>\n                          Client name sent when registering with the OAuth server\n  --oauth-client-registration <mode>\n                          auto (default), cimd, or dcr\n  --exposure <mode>       codemode (default), deferred, direct, or hidden\n  --description <text>    What the server offers, shown in the system prompt\n\nOther options:\n  --json                  Print the list as JSON\n  --timeout <seconds>     Time limit for the whole sign-in (default: 300)"
    )
}

fn help_hint() -> String {
    format!("\x1b[2mUse \"{} mcp --help\" for usage.\x1b[22m", *APP_NAME)
}

pub type LogFn = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone)]
pub struct McpCommandOptions {
    pub cwd: String,
    pub agent_dir: String,
    pub credentials: Option<McpOAuthCredentialStore>,
    /// Token of a pi provider, for servers with `auth.provider`. Defaults to the agent directory's `auth.json`.
    pub provider_token: Option<ProviderTokenFn>,
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
    #[serde(rename = "override", skip_serializing_if = "Option::is_none")]
    override_path: Option<String>,
    enabled: bool,
    exposure: String,
    transport: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth_provider: Option<String>,
    state: String,
    tools: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_exposure: Option<IndexMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resources: Option<bool>,
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
    provider_token: ProviderTokenFn,
) -> Arc<McpServerConnection> {
    let log_path = pi_js::path::join(&[&options.agent_dir, "mcp.log"]);
    McpServerConnection::new(McpServerConnectionOptions {
        entry,
        cwd: options.cwd.clone(),
        create_transport: Arc::new(create_default_transport),
        credentials: credentials.clone(),
        provider_token: Some(provider_token),
        on_tools: Arc::new(|_| {}),
        on_change: None,
        log: Some(Arc::new(McpServerLog::new(log_path))),
    })
}

fn create_provider_token_reader(agent_dir: &str) -> ProviderTokenFn {
    let runtime_cell = Arc::new(tokio::sync::OnceCell::<Option<ModelRuntime>>::new());
    let agent_dir = agent_dir.to_string();
    Arc::new(move |provider: &str| {
        let cell = Arc::clone(&runtime_cell);
        let dir = agent_dir.clone();
        let provider = provider.to_string();
        Box::pin(async move {
            let rt_opt = cell
                .get_or_init(|| async {
                    ModelRuntime::create(CreateModelRuntimeOptions {
                        auth_path: Some(pi_js::path::join(&[&dir, "auth.json"])),
                        models_path: Some(Some(pi_js::path::join(&[&dir, "models.json"]))),
                        refresh_on_create: Some(false),
                        ..Default::default()
                    })
                    .await
                    .ok()
                })
                .await;
            let Some(rt) = rt_opt else {
                return None;
            };
            rt.get_auth_provider_id(&provider, None)
                .await
                .ok()
                .flatten()
                .and_then(|res| res.auth.api_key)
        })
    })
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b_chars.len()).collect();
    for (i, &ca) in a_chars.iter().enumerate() {
        let mut current = vec![0usize; b_chars.len() + 1];
        current[0] = i + 1;
        for (j, &cb) in b_chars.iter().enumerate() {
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + usize::from(ca != cb));
        }
        previous = current;
    }
    previous[b_chars.len()]
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
            error(&format!("Unknown option {raw}.\n{}", help_hint()));
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
        "Usage: {} mcp add <server> [options] (--url <url> | -- <command> [args...])\n{}",
        *APP_NAME,
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
    known.insert("oauth-client-registration", OptKind::Value);
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
        "oauth-client-registration",
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
        if let Some(creg) = get_val("oauth-client-registration") {
            oauth.insert("clientRegistration".to_string(), Value::String(creg.to_string()));
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

    let validated = match validate_mcp_server_config(&name, &Value::Object(cfg_obj)) {
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
    let replaced = match add_mcp_server_config(&path, &name, &validated) {
        Ok(r) => r,
        Err(err) => {
            error(&format!("Could not update {path}: {}", err.message()));
            return 1;
        }
    };
    let action_word = if replaced { "Replaced" } else { "Added" };
    let app = *APP_NAME;
    log(&format!("{action_word} {scope} MCP server \"{name}\" in {path}."));
    if project
        && ProjectTrustStore::new(&options.agent_dir)
            .get(&options.cwd)
            .ok()
            .flatten()
            != Some(true)
    {
        log(&format!(
            "The project is not trusted, so {path} is ignored until you start {app} in the project and trust it."
        ));
    }
    let may_need_sign_in = matches!(
        &validated,
        McpServerConfig::Http(h) if !h.headers.as_ref().is_some_and(|hdrs| hdrs.keys().any(|k| k.eq_ignore_ascii_case("authorization")))
    );
    let hint = if may_need_sign_in {
        format!(". If it requires sign-in: {app} mcp login {name}")
    } else {
        String::new()
    };
    log(&format!("Check it with: {app} mcp list{hint}"));
    0
}

fn remove(args: &[String], project_config: &str, options: &McpCommandOptions, log: &LogFn, error: &LogFn) -> i32 {
    let mut known = IndexMap::new();
    known.insert("local", OptKind::Flag);
    let Some(parsed) = parse_options(args, &known, error, usize::MAX) else {
        return 1;
    };
    if parsed.positional.len() != 1 {
        error(&format!(
            "Usage: {} mcp remove <server> [-l]\n{}",
            *APP_NAME,
            help_hint()
        ));
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
    provider_token: &ProviderTokenFn,
    log: &LogFn,
) -> i32 {
    let futs: Vec<_> = loaded
        .servers
        .iter()
        .map(|entry| {
            let entry = entry.clone();
            let options = options.clone();
            let credentials = credentials.clone();
            let provider_token = Arc::clone(provider_token);
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
                let auth_provider = entry.auth_provider().map(String::from);
                let mut report = ServerReport {
                    name: entry.name.clone(),
                    scope: entry
                        .scope
                        .map(|s| s.as_str().to_string())
                        .unwrap_or_else(|| "global".to_string()),
                    source: entry.source.clone(),
                    override_path: entry.override_path.clone(),
                    enabled,
                    exposure: exposure_str(exposure_val).to_string(),
                    transport: describe_transport(&entry),
                    auth_provider,
                    state: "disabled".to_string(),
                    tools: Vec::new(),
                    tool_exposure: None,
                    resources: None,
                    error: None,
                };
                if !enabled {
                    return report;
                }
                let conn = create_connection(entry.clone(), &options, &credentials, provider_token);
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
                    report.resources = Some(true);
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
        if let Some(ref ovr) = report.override_path {
            log(&format!("  project override: {ovr}"));
        }
        if report.state == "needs-auth" {
            if let Some(ref auth_prov) = report.auth_provider {
                log(&format!("  sign in with: /login {auth_prov} in {}", *APP_NAME));
            } else {
                log(&format!("  sign in with: {} mcp login {}", *APP_NAME, report.name));
            }
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
        if report.resources == Some(true) {
            log("  offers resources");
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
        wait_for_redirect_url(signal, self.interactive).await
    }
}

async fn wait_for_redirect_url(signal: AbortSignal, interactive: bool) -> Result<Option<String>> {
    if !interactive {
        if !signal.aborted() {
            signal.cancelled().await;
        }
        return Ok(None);
    }
    match question_terminal(
        "If the browser cannot reach this machine, paste the URL it was redirected to: ",
        Some(&signal),
        None,
    )
    .await
    {
        Ok(line) => Ok(Some(line)),
        Err(_) => Ok(None),
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
            open_browser(u);
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
            interactive,
            open_url,
            log: Arc::clone(log),
        }),
        signal: Some(AbortSignal::timeout(timeout_ms)),
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
    let project_config = pi_js::path::join(&[&options.cwd, *CONFIG_DIR_NAME, "mcp.json"]);
    if command == "add" {
        return Ok(add(rest, &project_config, &options, &log, &error));
    }
    if command == "remove" {
        return Ok(remove(rest, &project_config, &options, &log, &error));
    }

    let project_trusted = ProjectTrustStore::new(&options.agent_dir)
        .get(&options.cwd)
        .ok()
        .flatten()
        == Some(true);
    let loaded = load_mcp_config(&LoadMcpConfigOptions {
        agent_dir: options.agent_dir.clone(),
        cwd: options.cwd.clone(),
        project_trusted,
    });
    let app = *APP_NAME;
    let untrusted_note = if !project_trusted && Path::new(&project_config).exists() {
        Some(format!(
            "{project_config} is ignored because the project is not trusted. Start {app} in the project to trust it."
        ))
    } else {
        None
    };
    let credentials = options
        .credentials
        .clone()
        .unwrap_or_else(|| McpOAuthCredentialStore::new(None, None));
    let provider_token = options
        .provider_token
        .clone()
        .unwrap_or_else(|| create_provider_token_reader(&options.agent_dir));

    match command {
        "list" => {
            let mut known = IndexMap::new();
            known.insert("json", OptKind::Flag);
            let Some(parsed) = parse_options(rest, &known, &error, usize::MAX) else {
                return Ok(1);
            };
            if !parsed.positional.is_empty() {
                error(&format!("Usage: {app} mcp list [--json]\n{}", help_hint()));
                return Ok(1);
            }
            Ok(list(
                &loaded,
                parsed.values.contains_key("json"),
                untrusted_note.as_deref(),
                &options,
                &credentials,
                &provider_token,
                &log,
            )
            .await)
        }
        "login" | "logout" => {
            let mut known = IndexMap::new();
            known.insert("local", OptKind::Flag);
            if command == "login" {
                known.insert("timeout", OptKind::Value);
            }
            let Some(parsed) = parse_options(rest, &known, &error, usize::MAX) else {
                return Ok(1);
            };
            if parsed.positional.len() != 1 {
                error(&format!("Usage: {app} mcp {command} <server> [-l]\n{}", help_hint()));
                return Ok(1);
            }
            let name = &parsed.positional[0];
            let Some(entry) = loaded.servers.iter().find(|s| s.name == *name) else {
                let suggestion = loaded
                    .servers
                    .iter()
                    .find(|candidate| edit_distance(&candidate.name, name) <= 2)
                    .map(|candidate| format!(" Did you mean \"{}\"?", candidate.name))
                    .unwrap_or_default();
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
                    "No MCP server named \"{name}\".{suggestion}{note} Configured: {configured}."
                ));
                return Ok(1);
            };
            if parsed.values.contains_key("local") && entry.scope != Some(McpScope::Project) {
                error(&format!(
                    "MCP server \"{name}\" is defined in {}, not in the project; omit --local.",
                    entry.source
                ));
                return Ok(1);
            }
            if let Some(auth_prov) = entry.auth_provider() {
                error(&format!(
                    "MCP server \"{name}\" uses the credentials of provider \"{auth_prov}\". Run \"{app} auth {command} {auth_prov}\"."
                ));
                return Ok(1);
            }
            let connection = create_connection(entry.clone(), &options, &credentials, provider_token);
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
