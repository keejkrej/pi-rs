//! Port of packages/coding-agent/src/main.ts

use std::io::{IsTerminal, Read};
use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::types::{AnyModel, ImageContent};
use pi_ai::{AuthOperationOptions, CredentialStore, ModelsRefreshOptions, models_are_equal};
use pi_js::abort::AbortController;
use pi_tui::set_capability_overrides;

use crate::cli::args::{
    ArgDiagnosticType, Args, ListModelsOption, Mode, UnknownFlagValue, build_gui_forward_args, dispatch_gui,
    is_gui_subcommand, normalize_session_name, parse_args, print_help,
};
use crate::cli::auth_check::{
    AuthCheckOptions, AuthCheckResult, AuthCheckStatus, check_provider_auth, create_auth_check_model_runtime,
    get_provider_credential,
};
use crate::cli::auth_command::{
    AuthCommandKind, get_auth_command_name, get_auth_command_usage, is_auth_command_help, parse_auth_command,
    print_auth_command_help, validate_auth_command_args,
};
use crate::cli::config_selector::{ConfigSelectorOptions, select_config};
use crate::cli::credential_print::resolve_credential_for_print;
use crate::cli::file_processor::{ProcessFileOptions, process_file_arguments};
use crate::cli::initial_message::{InitialMessageInput, build_initial_message};
use crate::cli::list_models::list_models;
use crate::cli::project_trust::{
    CreateProjectTrustContextOptions, create_project_trust_runtime_context, extension_project_trust_to_runtime_context,
};
use crate::cli::startup_ui::{should_run_first_time_setup, show_first_time_setup, show_startup_selector};
use crate::config::{APP_NAME, ENV_SESSION_DIR, VERSION, expand_tilde_path, get_agent_dir, get_package_dir};
use crate::core::agent_session_runtime::{
    AgentSessionRuntimeDiagnostic, AgentSessionRuntimeDiagnosticType, CreateAgentSessionRuntimeFactory,
    CreateAgentSessionRuntimeOptions, CreateAgentSessionRuntimeResult, create_agent_session_runtime,
};
use crate::core::agent_session_services::{
    CreateAgentSessionFromServicesOptions, CreateAgentSessionServicesOptions, create_agent_session_from_services,
    create_agent_session_services,
};
use crate::core::auth_guidance::format_no_models_available_message;
use crate::core::auth_storage::{AuthStorage, ReadOnlyAuthStorage};
use crate::core::export_html::{ExportOptions, export_from_file};
use crate::core::extensions::types::{FlagValue, InlineExtension};
use crate::core::http_dispatcher::{
    DEFAULT_HTTP_IDLE_TIMEOUT_MS, apply_http_proxy_settings, configure_http_dispatcher,
};
use crate::core::model_resolver::{ResolveCliModelOptions, ScopedModel, resolve_cli_model, resolve_model_scope};
use crate::core::model_runtime::{ModelRuntime, ModelRuntimeCreateOptions};
use crate::core::output_guard::{restore_stdout, take_over_stdout};
use crate::core::project_trust::{
    AppMode, ProjectTrustExtensionHost, ResolveProjectTrustedOptions, RunnerProjectTrustExtensionHost,
    resolve_project_trusted,
};
use crate::core::resource_loader::{
    CoreExtensionLoaderHost, DefaultResourceLoaderOptions, InlineExtension as LoaderInlineExtension,
    ResourceLoaderReloadOptions,
};
use crate::core::sdk::{CreateAgentSessionOptions, NoToolsMode, ScopedModelEntry};
use crate::core::session_cwd::{
    MissingSessionCwdError, format_missing_session_cwd_prompt, get_missing_session_cwd_issue,
};
use crate::core::session_manager::{NewSessionOptions, SessionManager, assert_valid_session_id};
use crate::core::settings_diagnostics::{collect_settings_diagnostics, deduplicate_diagnostics};
use crate::core::settings_manager::{Settings, SettingsManager, SettingsManagerCreateOptions};
use crate::core::timings::{TimingNamespaceKind, print_timings, reset_timings, time};
use crate::core::trust_manager::{ProjectTrustStore, has_trust_requiring_project_resources};
use crate::experimental::coordinator::run_coordinator_process;
use crate::experimental::process::{InternalProcessRole, consume_internal_process_role};
use crate::experimental::server::run_server_process;
use crate::experimental::session_worker::run_session_worker_process;
use crate::extensions::built_in_extensions;
use crate::extensions::mcp::cli::{McpCommandOptions, run_mcp_command};
use crate::migrations::{run_migrations, show_deprecation_warnings};
use crate::package_manager_cli::{
    ConfigSelectorInput, PackageCommandRuntimeOptions, cleanup_managed_install, handle_config_command,
    handle_package_command,
};
use crate::utils::paths::{PathInputOptions, is_local_path, normalize_path, resolve_path};
use crate::utils::windows_self_update::cleanup_windows_self_update_quarantine;
use crate::vendor::chalk::chalk;

#[derive(Clone, Default)]
pub struct MainOptions {
    pub extension_factories: Option<Vec<InlineExtension>>,
}

async fn read_piped_stdin() -> Option<String> {
    if std::io::stdin().is_terminal() {
        return None;
    }
    let data = tokio::task::spawn_blocking(|| {
        let mut buf = String::new();
        let _ = std::io::stdin().read_to_string(&mut buf);
        buf
    })
    .await
    .ok()?;
    let trimmed = pi_js::str16::trim(&data).to_string();
    if trimmed.is_empty() { None } else { Some(trimmed) }
}

fn report_diagnostics(diagnostics: &[AgentSessionRuntimeDiagnostic]) {
    let c = chalk();
    for d in diagnostics {
        let rendered = match d.r#type {
            AgentSessionRuntimeDiagnosticType::Error => c.red().apply(&format!("Error: {}", d.message)),
            AgentSessionRuntimeDiagnosticType::Warning => c.yellow().apply(&format!("Warning: {}", d.message)),
            AgentSessionRuntimeDiagnosticType::Info => c.dim().apply(&d.message),
        };
        pi_js::console::error(&rendered);
    }
}

fn is_truthy_env_flag(value: Option<&str>) -> bool {
    let Some(v) = value else {
        return false;
    };
    v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
}

fn resolve_app_mode(parsed: &Args, stdin_is_tty: bool, stdout_is_tty: bool) -> AppMode {
    if parsed.mode == Some(Mode::Rpc) {
        return AppMode::Rpc;
    }
    if parsed.mode == Some(Mode::Json) {
        return AppMode::Json;
    }
    if parsed.print == Some(true) || !stdin_is_tty || !stdout_is_tty {
        return AppMode::Print;
    }
    AppMode::Interactive
}

fn is_plain_runtime_metadata_command(parsed: &Args) -> bool {
    parsed.print != Some(true) && parsed.mode.is_none() && (parsed.help == Some(true) || parsed.list_models.is_some())
}

async fn run_auth_command(args: &[String]) -> pi_js::Result<bool> {
    if is_auth_command_help(args) {
        print_auth_command_help();
        return Ok(true);
    }

    let command = match parse_auth_command(args) {
        Ok(Some(cmd)) => cmd,
        Ok(None) => return Ok(false),
        Err(err) => {
            pi_js::console::error(&chalk().red().apply(&format!("Error: {err}")));
            return Err(pi_js::Error::Exit(1));
        }
    };

    let parsed = parse_args(&command.args);
    if let Some((option, _)) = parsed.unknown_flags.iter().next() {
        pi_js::console::error(&chalk().red().apply(&format!(
            "Unknown option --{option} for \"{}\".",
            get_auth_command_name(command.kind)
        )));
        pi_js::console::error(&chalk().dim().apply(&format!(
            "Use \"{} --help\" or \"{}\".",
            *APP_NAME,
            get_auth_command_usage(command.kind)
        )));
        return Err(pi_js::Error::Exit(1));
    }

    let c = chalk();
    if !parsed.diagnostics.is_empty() {
        let msg = parsed
            .diagnostics
            .iter()
            .map(|d| d.message.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        pi_js::console::error(&c.red().apply(&format!("Error: {msg}")));
        return Err(pi_js::Error::Exit(if command.kind == AuthCommandKind::Check {
            2
        } else {
            1
        }));
    }

    if command.kind != AuthCommandKind::Check {
        let controller = AbortController::new();
        let signal = controller.signal();
        let model_runtime = match ModelRuntime::create(ModelRuntimeCreateOptions {
            allow_model_network: Some(false),
            signal: Some(signal.clone()),
            ..Default::default()
        })
        .await
        {
            Ok(rt) => rt,
            Err(err) => {
                pi_js::console::error(&c.red().apply(&format!("Error: {err}")));
                return Err(pi_js::Error::Exit(1));
            }
        };
        match resolve_credential_for_print(
            &parsed,
            &model_runtime,
            command.kind,
            command.min_expiry_ms,
            Some(signal),
        )
        .await
        {
            Ok(credential) => {
                pi_js::console::stdout_write(&format!("{credential}\n"));
                return Ok(true);
            }
            Err(err) => {
                pi_js::console::error(&c.red().apply(&format!("Error: {err}")));
                return Err(pi_js::Error::Exit(1));
            }
        }
    }

    let (requested_provider, requested_model) = match validate_auth_command_args(&parsed, command.kind) {
        Ok(pair) => pair,
        Err(err) => {
            pi_js::console::error(&c.red().apply(&format!("Error: {err}")));
            return Err(pi_js::Error::Exit(2));
        }
    };

    let mut credential: Option<String> = None;
    let result = match async {
        let credentials: Arc<dyn CredentialStore> = if command.no_refresh {
            Arc::new(ReadOnlyAuthStorage::new(None))
        } else {
            Arc::new(AuthStorage::create(None))
        };
        let model_runtime = create_auth_check_model_runtime(Arc::clone(&credentials)).await?;
        let auth_opts = AuthCheckOptions {
            refresh: !command.no_refresh,
        };
        let mut res = check_provider_auth(&parsed, &model_runtime, Some(auth_opts)).await?;
        if command.credentials && res.status == AuthCheckStatus::Ready {
            credential =
                get_provider_credential(&res.provider, &model_runtime, credentials.as_ref(), auth_opts).await?;
            if credential.is_none() {
                res = AuthCheckResult {
                    status: AuthCheckStatus::NotReady,
                    provider: res.provider,
                    reason: Some(crate::cli::auth_check::AuthCheckReason::CredentialNotAvailable),
                    ..res
                };
            }
        }
        Ok::<_, pi_js::Error>(res)
    }
    .await
    {
        Ok(res) => res,
        Err(_) => AuthCheckResult {
            status: AuthCheckStatus::Invalid,
            provider: requested_provider.or(requested_model).unwrap_or_default(),
            reason: Some(crate::cli::auth_check::AuthCheckReason::InvalidState),
            auth_type: None,
        },
    };

    let output = if command.json {
        let mut obj = pi_js::json::parse(&pi_js::json::stringify(&result))?;
        if let (Some(map), Some(cred)) = (obj.as_object_mut(), credential.as_ref()) {
            map.insert("credentials".to_string(), serde_json::Value::String(cred.clone()));
        }
        pi_js::json::stringify(&obj)
    } else {
        credential.unwrap_or_else(|| match result.status {
            AuthCheckStatus::Ready => "ready".to_string(),
            AuthCheckStatus::NotReady => "not_ready".to_string(),
            AuthCheckStatus::Invalid => "invalid".to_string(),
        })
    };
    pi_js::console::stdout_write(&format!("{output}\n"));
    let code = match result.status {
        AuthCheckStatus::Ready => 0,
        AuthCheckStatus::NotReady => 1,
        AuthCheckStatus::Invalid => 2,
    };
    if code != 0 {
        return Err(pi_js::Error::Exit(code));
    }
    Ok(true)
}

async fn prepare_initial_message(
    parsed: &mut Args,
    stdin_content: Option<&str>,
) -> pi_js::Result<(Option<String>, Option<Vec<ImageContent>>)> {
    if parsed.file_args.is_empty() {
        let res = build_initial_message(InitialMessageInput {
            parsed,
            file_text: None,
            file_images: None,
            stdin_content,
        });
        return Ok((res.initial_message, res.initial_images));
    }
    let processed = process_file_arguments(
        &parsed.file_args,
        Some(ProcessFileOptions {
            auto_resize_images: Some(false),
        }),
    )
    .await?;
    let file_text = (!processed.text.is_empty()).then_some(processed.text.as_str());
    let file_images = (!processed.images.is_empty()).then_some(processed.images);
    let res = build_initial_message(InitialMessageInput {
        parsed,
        file_text,
        file_images,
        stdin_content,
    });
    Ok((res.initial_message, res.initial_images))
}

enum ResolvedSession {
    Path(String),
    Local(String),
    Global { path: String, cwd: String },
    NotFound(String),
}

fn find_local_session_by_exact_id(session_id: &str, cwd: &str, session_dir: Option<&str>) -> Option<ResolvedSession> {
    SessionManager::find_by_id(cwd, session_id, session_dir).map(ResolvedSession::Local)
}

async fn resolve_session_path(
    session_arg: &str,
    cwd: &str,
    session_dir: Option<&str>,
) -> pi_js::Result<ResolvedSession> {
    if session_arg.contains('/') || session_arg.contains('\\') || session_arg.ends_with(".jsonl") {
        return Ok(ResolvedSession::Path(resolve_path(
            session_arg,
            Some(cwd),
            PathInputOptions::default(),
        )));
    }
    if let Some(found) = find_local_session_by_exact_id(session_arg, cwd, session_dir) {
        return Ok(found);
    }
    let local_sessions = SessionManager::list(cwd, session_dir, None, None).await?;
    if let Some(m) = local_sessions.into_iter().find(|s| s.id.starts_with(session_arg)) {
        return Ok(ResolvedSession::Local(m.path));
    }
    let all_sessions = SessionManager::list_all(session_dir, None, None).await?;
    if let Some(m) = all_sessions
        .iter()
        .find(|s| s.id == session_arg)
        .or_else(|| all_sessions.iter().find(|s| s.id.starts_with(session_arg)))
    {
        return Ok(ResolvedSession::Global {
            path: m.path.clone(),
            cwd: m.cwd.clone(),
        });
    }
    Ok(ResolvedSession::NotFound(session_arg.to_string()))
}

async fn prompt_confirm(message: &str) -> bool {
    let prompt_text = format!("{message} [y/N] ");
    tokio::task::spawn_blocking(move || {
        pi_js::console::stdout_write(&prompt_text);
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_ok() {
            let trimmed = line.trim();
            return trimmed.eq_ignore_ascii_case("y") || trimmed.eq_ignore_ascii_case("yes");
        }
        false
    })
    .await
    .unwrap_or(false)
}

fn validate_fork_flags(parsed: &Args) -> pi_js::Result<()> {
    if parsed.fork.is_none() {
        return Ok(());
    }
    let mut conflicting = Vec::new();
    if parsed.session.is_some() {
        conflicting.push("--session");
    }
    if parsed.r#continue == Some(true) {
        conflicting.push("--continue");
    }
    if parsed.resume == Some(true) {
        conflicting.push("--resume");
    }
    if parsed.no_session == Some(true) {
        conflicting.push("--no-session");
    }
    if !conflicting.is_empty() {
        pi_js::console::error(&chalk().red().apply(&format!(
            "Error: --fork cannot be combined with {}",
            conflicting.join(", ")
        )));
        return Err(pi_js::Error::Exit(1));
    }
    Ok(())
}

fn validate_session_id_flags(parsed: &Args) -> pi_js::Result<()> {
    let Some(ref session_id) = parsed.session_id else {
        return Ok(());
    };
    let mut conflicting = Vec::new();
    if parsed.session.is_some() {
        conflicting.push("--session");
    }
    if parsed.r#continue == Some(true) {
        conflicting.push("--continue");
    }
    if parsed.resume == Some(true) {
        conflicting.push("--resume");
    }
    if !conflicting.is_empty() {
        pi_js::console::error(&chalk().red().apply(&format!(
            "Error: --session-id cannot be combined with {}",
            conflicting.join(", ")
        )));
        return Err(pi_js::Error::Exit(1));
    }
    if let Err(err) = assert_valid_session_id(session_id) {
        pi_js::console::error(&chalk().red().apply(&format!("Error: {err}")));
        return Err(pi_js::Error::Exit(1));
    }
    Ok(())
}

fn open_session_or_exit(path: &str, session_dir: Option<&str>) -> pi_js::Result<SessionManager> {
    SessionManager::open(path, session_dir, None).map_err(|err| {
        pi_js::console::error(&chalk().red().apply(&format!("Error: {err}")));
        pi_js::Error::Exit(1)
    })
}

fn fork_session_or_exit(
    source_path: &str,
    cwd: &str,
    session_dir: Option<&str>,
    session_id: Option<&str>,
) -> pi_js::Result<SessionManager> {
    SessionManager::fork_from(
        source_path,
        cwd,
        session_dir,
        session_id.map(|id| NewSessionOptions {
            id: Some(id.to_string()),
            ..Default::default()
        }),
    )
    .map_err(|err| {
        pi_js::console::error(&chalk().red().apply(&format!("Error: {err}")));
        pi_js::Error::Exit(1)
    })
}

pub async fn create_session_manager(
    parsed: &Args,
    cwd: &str,
    session_dir: Option<&str>,
    settings_manager: &SettingsManager,
) -> pi_js::Result<SessionManager> {
    if parsed.no_session == Some(true) || parsed.help == Some(true) || parsed.list_models.is_some() {
        return SessionManager::in_memory(
            Some(cwd),
            parsed.session_id.as_ref().map(|id| NewSessionOptions {
                id: Some(id.clone()),
                ..Default::default()
            }),
            None,
        );
    }

    if let Some(ref fork_arg) = parsed.fork {
        if let Some(ref id) = parsed.session_id
            && find_local_session_by_exact_id(id, cwd, session_dir).is_some()
        {
            pi_js::console::error(&chalk().red().apply(&format!("Session already exists with id '{id}'")));
            return Err(pi_js::Error::Exit(1));
        }
        match resolve_session_path(fork_arg, cwd, session_dir).await? {
            ResolvedSession::Path(path) | ResolvedSession::Local(path) | ResolvedSession::Global { path, .. } => {
                return fork_session_or_exit(&path, cwd, session_dir, parsed.session_id.as_deref());
            }
            ResolvedSession::NotFound(arg) => {
                pi_js::console::error(&chalk().red().apply(&format!("No session found matching '{arg}'")));
                return Err(pi_js::Error::Exit(1));
            }
        }
    }

    if let Some(ref session_arg) = parsed.session {
        match resolve_session_path(session_arg, cwd, session_dir).await? {
            ResolvedSession::Path(path) | ResolvedSession::Local(path) => {
                return open_session_or_exit(&path, session_dir);
            }
            ResolvedSession::Global { path, cwd: orig_cwd } => {
                pi_js::console::log(
                    &chalk()
                        .yellow()
                        .apply(&format!("Session found in different project: {orig_cwd}")),
                );
                let should_fork = prompt_confirm("Fork this session into current directory?").await;
                if !should_fork {
                    pi_js::console::log(&chalk().dim().apply("Aborted."));
                    return Err(pi_js::Error::Exit(0));
                }
                return fork_session_or_exit(&path, cwd, session_dir, None);
            }
            ResolvedSession::NotFound(arg) => {
                pi_js::console::error(&chalk().red().apply(&format!("No session found matching '{arg}'")));
                return Err(pi_js::Error::Exit(1));
            }
        }
    }

    if parsed.resume == Some(true) {
        let cwd_owned = cwd.to_string();
        let sd_owned = session_dir.map(str::to_string);
        let selected = crate::cli::session_picker::select_session(
            Arc::new(move |on_progress, signal| {
                let cwd = cwd_owned.clone();
                let sd = sd_owned.clone();
                Box::pin(async move {
                    let progress = on_progress
                        .as_deref()
                        .map(|cb| cb as crate::core::session_manager::SessionListProgress<'_>);
                    SessionManager::list(&cwd, sd.as_deref(), progress, signal.as_ref()).await
                })
            }),
            {
                let sd = session_dir.map(str::to_string);
                Arc::new(move |on_progress, signal| {
                    let sd = sd.clone();
                    Box::pin(async move {
                        let progress = on_progress
                            .as_deref()
                            .map(|cb| cb as crate::core::session_manager::SessionListProgress<'_>);
                        SessionManager::list_all(sd.as_deref(), progress, signal.as_ref()).await
                    })
                })
            },
            settings_manager,
        )
        .await;
        // WIRE(@tui-w3): modes::interactive::theme::theme::stop_theme_watcher()
        let selected_path = selected?;
        let Some(path) = selected_path else {
            pi_js::console::log(&chalk().dim().apply("No session selected"));
            return Err(pi_js::Error::Exit(0));
        };
        return open_session_or_exit(&path, session_dir);
    }

    if parsed.r#continue == Some(true) {
        return SessionManager::continue_recent(cwd, session_dir);
    }

    if let Some(ref session_id) = parsed.session_id {
        if let Some(ResolvedSession::Local(path)) = find_local_session_by_exact_id(session_id, cwd, session_dir) {
            return open_session_or_exit(&path, session_dir);
        }
        pi_js::console::error(&chalk().yellow().apply(&format!(
            "Warning: No project session found with id '{session_id}'; creating a new session with that id."
        )));
    }

    SessionManager::create(
        cwd,
        session_dir,
        parsed.session_id.as_ref().map(|id| NewSessionOptions {
            id: Some(id.clone()),
            ..Default::default()
        }),
    )
}

struct BuiltSessionOptions {
    options: CreateAgentSessionOptions,
    cli_thinking_from_model: bool,
    diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
}

fn build_session_options(
    parsed: &Args,
    scoped_models: &[ScopedModel],
    has_existing_session: bool,
    model_runtime: &ModelRuntime,
    settings_manager: &SettingsManager,
) -> BuiltSessionOptions {
    let mut options = CreateAgentSessionOptions::default();
    let mut diagnostics = Vec::new();
    let mut cli_thinking_from_model = false;

    if let Some(ref provider) = parsed.provider
        && parsed.model.is_none()
    {
        diagnostics.push(AgentSessionRuntimeDiagnostic {
            r#type: AgentSessionRuntimeDiagnosticType::Error,
            message: format!("--provider requires --model (for example: --provider {provider} --model <pattern>)"),
        });
    }
    if let Some(ref cli_model) = parsed.model {
        let resolved = resolve_cli_model(ResolveCliModelOptions {
            cli_provider: parsed.provider.as_deref(),
            cli_model: Some(cli_model.as_str()),
            cli_thinking: parsed.thinking,
            model_runtime,
        });
        if let Some(warning) = resolved.warning {
            diagnostics.push(AgentSessionRuntimeDiagnostic {
                r#type: AgentSessionRuntimeDiagnosticType::Warning,
                message: warning,
            });
        }
        if let Some(error) = resolved.error {
            diagnostics.push(AgentSessionRuntimeDiagnostic {
                r#type: AgentSessionRuntimeDiagnosticType::Error,
                message: error,
            });
        }
        if let Some(model) = resolved.model {
            options.model = Some(model);
            if parsed.thinking.is_none()
                && let Some(level) = resolved.thinking_level
            {
                options.thinking_level = Some(level);
                cli_thinking_from_model = true;
            }
        }
    }

    if options.model.is_none() && !scoped_models.is_empty() && !has_existing_session {
        let saved_provider = settings_manager.get_default_provider();
        let saved_model_id = settings_manager.get_default_model();
        let saved_model = saved_provider
            .as_deref()
            .zip(saved_model_id.as_deref())
            .and_then(|(p, id)| model_runtime.get_model(p, id));
        let saved_in_scope = saved_model.as_ref().and_then(|sm| {
            let sm_any = AnyModel::Chat(sm.clone());
            scoped_models.iter().find(|entry| {
                let entry_any = AnyModel::Chat(entry.model.clone());
                models_are_equal(Some(&entry_any), Some(&sm_any))
            })
        });
        let chosen = saved_in_scope.unwrap_or(&scoped_models[0]);
        options.model = Some(chosen.model.clone());
        if parsed.thinking.is_none()
            && let Some(level) = chosen.thinking_level
        {
            options.thinking_level = Some(level);
        }
    }

    if let Some(thinking) = parsed.thinking {
        options.thinking_level = Some(thinking);
    }

    if !scoped_models.is_empty() {
        options.scoped_models = Some(
            scoped_models
                .iter()
                .map(|sm| ScopedModelEntry {
                    model: sm.model.clone(),
                    thinking_level: sm.thinking_level,
                })
                .collect(),
        );
    }

    if parsed.no_tools == Some(true) {
        options.no_tools = Some(NoToolsMode::All);
    } else if parsed.no_builtin_tools == Some(true) {
        options.no_tools = Some(NoToolsMode::Builtin);
    }
    if let Some(ref tools) = parsed.tools {
        options.tools = Some(tools.clone());
    }
    if let Some(ref exclude_tools) = parsed.exclude_tools {
        options.exclude_tools = Some(exclude_tools.clone());
    }

    BuiltSessionOptions {
        options,
        cli_thinking_from_model,
        diagnostics,
    }
}

fn resolve_cli_paths(cwd: &str, paths: Option<&[String]>) -> Option<Vec<String>> {
    paths.map(|list| {
        list.iter()
            .map(|value| {
                if is_local_path(value) {
                    resolve_path(value, Some(cwd), PathInputOptions::default())
                } else {
                    value.clone()
                }
            })
            .collect()
    })
}

async fn prompt_for_missing_session_cwd(
    issue: &crate::core::session_cwd::SessionCwdIssue,
    settings_manager: &SettingsManager,
) -> Option<String> {
    let prompt = format_missing_session_cwd_prompt(issue);
    let options = vec![
        ("Continue".to_string(), Some(issue.fallback_cwd.clone())),
        ("Cancel".to_string(), None),
    ];
    show_startup_selector(settings_manager, &prompt, &options)
        .await
        .ok()
        .flatten()
        .flatten()
}

fn convert_unknown_flags(flags: &IndexMap<String, UnknownFlagValue>) -> IndexMap<String, FlagValue> {
    flags
        .iter()
        .map(|(k, v)| {
            let val = match v {
                UnknownFlagValue::Bool(b) => FlagValue::Bool(*b),
                UnknownFlagValue::String(s) => FlagValue::String(s.clone()),
            };
            (k.clone(), val)
        })
        .collect()
}

pub async fn main(args: &[String], options: Option<MainOptions>) -> pi_js::Result<()> {
    if let Some(role) = consume_internal_process_role()? {
        return match role {
            InternalProcessRole::Coordinator => run_coordinator_process(args).await,
            InternalProcessRole::Server => run_server_process(args).await,
            InternalProcessRole::SessionWorker => run_session_worker_process(args).await,
        };
    }
    reset_timings(TimingNamespaceKind::Main);
    let mut extension_factories = built_in_extensions();
    if let Some(extra) = options.and_then(|o| o.extension_factories) {
        extension_factories.extend(extra);
    }

    let env_offline = pi_js::env::var("PI_OFFLINE");
    let offline_mode = args.iter().any(|a| a == "--offline") || is_truthy_env_flag(env_offline.as_deref());
    if offline_mode {
        pi_js::env::set_var("PI_OFFLINE", "1");
        pi_js::env::set_var("PI_SKIP_VERSION_CHECK", "1");
    }

    if is_gui_subcommand(args) {
        let cwd = pi_js::env::cwd();
        let code = dispatch_gui(args, &cwd).await?;
        if code != 0 {
            return Err(pi_js::Error::Exit(code));
        }
        return Ok(());
    }

    if run_auth_command(args).await? {
        return Ok(());
    }

    if pi_js::env::platform() == "win32" {
        cleanup_windows_self_update_quarantine(&get_package_dir());
    }
    cleanup_managed_install();

    let cwd = pi_js::env::cwd();
    let agent_dir = get_agent_dir();
    let bootstrap_settings = SettingsManager::create(
        &cwd,
        Some(&agent_dir),
        Some(SettingsManagerCreateOptions {
            project_trusted: Some(false),
        }),
    );
    apply_http_proxy_settings(bootstrap_settings.get_global_settings().http_proxy.as_deref());
    let _ = configure_http_dispatcher(None);

    let loader_extension_factories: Vec<LoaderInlineExtension> = extension_factories
        .iter()
        .cloned()
        .map(LoaderInlineExtension::Core)
        .collect();
    let pkg_opts = PackageCommandRuntimeOptions {
        extension_factories: loader_extension_factories,
        select_config: Some(Arc::new(|input: ConfigSelectorInput| {
            Box::pin(select_config(ConfigSelectorOptions {
                resolved_paths_global: input.resolved_paths_global,
                resolved_paths_project: input.resolved_paths_project,
                settings_manager: input.settings_manager,
                cwd: input.cwd,
                agent_dir: input.agent_dir,
                write_scope: input.write_scope,
                project_mode_available: input.project_mode_available,
            }))
        })),
        create_project_trust_context: Some(Arc::new(|cwd, mode, settings_manager, has_ui| {
            create_project_trust_runtime_context(CreateProjectTrustContextOptions {
                cwd: cwd.to_owned(),
                mode,
                settings_manager: settings_manager.clone(),
                has_ui,
            })
        })),
        ..Default::default()
    };
    if handle_package_command(args, pkg_opts.clone()).await {
        return Ok(());
    }

    if handle_config_command(args, pkg_opts).await {
        return Ok(());
    }

    if args.first().map(String::as_str) == Some("mcp") {
        let exit_code = run_mcp_command(
            &args[1..],
            McpCommandOptions {
                cwd,
                agent_dir,
                credentials: None,
                open_url: None,
                log: None,
                error: None,
            },
        )
        .await?;
        if exit_code != 0 {
            return Err(pi_js::Error::Exit(exit_code));
        }
        return Ok(());
    }

    let mut parsed = parse_args(args);
    if !parsed.diagnostics.is_empty() {
        let c = chalk();
        for d in &parsed.diagnostics {
            let line = match d.r#type {
                ArgDiagnosticType::Error => c.red().apply(&format!("Error: {}", d.message)),
                ArgDiagnosticType::Warning => c.yellow().apply(&format!("Warning: {}", d.message)),
            };
            pi_js::console::error(&line);
        }
        if parsed.diagnostics.iter().any(|d| d.r#type == ArgDiagnosticType::Error) {
            return Err(pi_js::Error::Exit(1));
        }
    }
    time("parseArgs", TimingNamespaceKind::Main);

    if parsed.version == Some(true) {
        pi_js::console::log(&VERSION);
        return Ok(());
    }

    if parsed.gui == Some(true) {
        let code = dispatch_gui(args, &cwd).await?;
        if code != 0 {
            return Err(pi_js::Error::Exit(code));
        }
        return Ok(());
    }
    let _ = build_gui_forward_args::<String>;

    if let Some(ref export_path) = parsed.export {
        let output_path = parsed.messages.first().map(String::as_str);
        let export_opts = output_path.map(|p| ExportOptions {
            output_path: Some(p.to_string()),
            ..Default::default()
        });
        let result = match export_from_file(export_path, export_opts).await {
            Ok(res) => res,
            Err(err) => {
                pi_js::console::error(&chalk().red().apply(&format!("Error: {err}")));
                return Err(pi_js::Error::Exit(1));
            }
        };
        pi_js::console::log(&format!("Exported to: {result}"));
        return Ok(());
    }

    let stdin_is_tty = std::io::stdin().is_terminal();
    let stdout_is_tty = std::io::stdout().is_terminal();
    let mut app_mode = resolve_app_mode(&parsed, stdin_is_tty, stdout_is_tty);
    let should_take_over_stdout = app_mode != AppMode::Interactive && !is_plain_runtime_metadata_command(&parsed);
    if should_take_over_stdout {
        take_over_stdout();
    }

    if parsed.mode == Some(Mode::Rpc) && !parsed.file_args.is_empty() {
        pi_js::console::error(
            &chalk()
                .red()
                .apply("Error: @file arguments are not supported in RPC mode"),
        );
        return Err(pi_js::Error::Exit(1));
    }

    validate_fork_flags(&parsed)?;
    validate_session_id_flags(&parsed)?;

    let migration_res = run_migrations(&cwd);
    time("runMigrations", TimingNamespaceKind::Main);

    let startup_settings = SettingsManager::create(&cwd, Some(&agent_dir), None);
    let startup_settings_diagnostics = collect_settings_diagnostics(&startup_settings);

    if app_mode == AppMode::Interactive
        && parsed.help != Some(true)
        && parsed.list_models.is_none()
        && should_run_first_time_setup(None)
    {
        show_first_time_setup(&startup_settings).await?;
        time("firstTimeSetup", TimingNamespaceKind::Main);
    }

    if app_mode == AppMode::Interactive
        && let Some(ref theme) = parsed.use_theme
    {
        startup_settings.apply_overrides(&Settings {
            theme: Some(theme.clone()),
            ..Default::default()
        });
    }

    let env_session_dir = pi_js::env::var(&ENV_SESSION_DIR);
    let session_dir = parsed
        .session_dir
        .as_deref()
        .map(|s| normalize_path(s, PathInputOptions::default()))
        .or_else(|| env_session_dir.as_deref().map(expand_tilde_path))
        .or_else(|| startup_settings.get_session_dir());

    let mut session_manager = create_session_manager(&parsed, &cwd, session_dir.as_deref(), &startup_settings).await?;
    if let Some(issue) = get_missing_session_cwd_issue(&session_manager, &cwd) {
        if app_mode == AppMode::Interactive {
            let Some(selected_cwd) = prompt_for_missing_session_cwd(&issue, &startup_settings).await else {
                return Ok(());
            };
            let sf = issue
                .session_file
                .as_deref()
                .ok_or_else(|| pi_js::Error::msg("Missing session file for missing cwd recovery"))?;
            session_manager = SessionManager::open(sf, session_dir.as_deref(), Some(&selected_cwd))?;
        } else {
            pi_js::console::error(&chalk().red().apply(&MissingSessionCwdError::new(issue).to_string()));
            return Err(pi_js::Error::Exit(1));
        }
    }

    if let Some(ref raw_name) = parsed.name {
        let Some(name) = normalize_session_name(raw_name) else {
            pi_js::console::error(&chalk().red().apply("Error: --name requires a non-empty value"));
            return Err(pi_js::Error::Exit(1));
        };
        session_manager.append_session_info(&name)?;
    }
    time("createSessionManager", TimingNamespaceKind::Main);

    let trust_store = ProjectTrustStore::new(&agent_dir);
    let session_cwd = session_manager.get_cwd();
    let auto_trust_on_reload_cwd =
        if parsed.project_trust_override.is_none() && !has_trust_requiring_project_resources(&session_cwd) {
            Some(session_cwd.clone())
        } else {
            None
        };
    let trust_prompt_mode = if parsed.help == Some(true) || parsed.list_models.is_some() {
        AppMode::Print
    } else {
        app_mode
    };
    let project_trust_by_cwd: Arc<std::sync::Mutex<IndexMap<String, bool>>> =
        Arc::new(std::sync::Mutex::new(IndexMap::new()));

    let resolved_ext_paths = resolve_cli_paths(&cwd, parsed.extensions.as_deref());
    let resolved_skill_paths = resolve_cli_paths(&cwd, parsed.skills.as_deref());
    let resolved_prompt_paths = resolve_cli_paths(&cwd, parsed.prompt_templates.as_deref());
    let resolved_theme_paths = resolve_cli_paths(&cwd, parsed.themes.as_deref());
    let extension_flags = convert_unknown_flags(&parsed.unknown_flags);

    let parsed_clone = parsed.clone();
    let startup_settings_clone = startup_settings.clone();
    let trust_store_clone = trust_store.clone();
    let ext_factories_clone = extension_factories.clone();
    let trust_cache_clone = Arc::clone(&project_trust_by_cwd);

    let create_runtime: CreateAgentSessionRuntimeFactory = Arc::new(move |opts| {
        let parsed = parsed_clone.clone();
        let startup_settings = startup_settings_clone.clone();
        let trust_store = trust_store_clone.clone();
        let extension_factories = ext_factories_clone.clone();
        let project_trust_by_cwd = Arc::clone(&trust_cache_clone);
        let resolved_ext_paths = resolved_ext_paths.clone();
        let resolved_skill_paths = resolved_skill_paths.clone();
        let resolved_prompt_paths = resolved_prompt_paths.clone();
        let resolved_theme_paths = resolved_theme_paths.clone();
        let extension_flags = extension_flags.clone();

        Box::pin(async move {
            let cwd = opts.cwd;
            let agent_dir = opts.agent_dir;
            let session_manager = opts.session_manager;
            let session_start_event = opts.session_start_event;
            let project_trust_context = opts.project_trust_context;
            let is_initial_runtime = session_start_event.is_none();

            let mut project_trust_diagnostics = Vec::new();
            let cached_trust = project_trust_by_cwd
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&cwd)
                .copied();
            let has_trust_resources = has_trust_requiring_project_resources(&cwd);
            let should_resolve_trust =
                parsed.project_trust_override.is_none() && cached_trust.is_none() && has_trust_resources;

            let saved_trust =
                if should_resolve_trust || cached_trust.is_some() || parsed.project_trust_override.is_some() {
                    None
                } else if !has_trust_resources {
                    Some(true)
                } else {
                    Some(trust_store.get(&cwd).ok().flatten() == Some(true))
                };
            let project_trusted = if should_resolve_trust {
                false
            } else {
                cached_trust
                    .or(parsed.project_trust_override)
                    .or(saved_trust)
                    .unwrap_or(false)
            };

            let runtime_settings = SettingsManager::create(
                &cwd,
                Some(&agent_dir),
                Some(SettingsManagerCreateOptions {
                    project_trusted: Some(project_trusted),
                }),
            );
            let trust_diagnostics_slot: Arc<std::sync::Mutex<Vec<AgentSessionRuntimeDiagnostic>>> =
                Arc::new(std::sync::Mutex::new(Vec::new()));

            let core_host = Arc::new(CoreExtensionLoaderHost::new());
            let reload_opts = if should_resolve_trust {
                let cwd_for_trust = cwd.clone();
                let trust_store_for_cb = trust_store.clone();
                let startup_settings_for_cb = startup_settings.clone();
                let diag_slot = Arc::clone(&trust_diagnostics_slot);
                let trust_cache = Arc::clone(&project_trust_by_cwd);
                let core_host_for_trust = Arc::clone(&core_host);
                let override_val = parsed.project_trust_override;
                let prompt_mode = if is_initial_runtime {
                    trust_prompt_mode
                } else {
                    app_mode
                };
                Some(ResourceLoaderReloadOptions {
                    resolve_project_trust: Some(Arc::new(move |_extensions_result| {
                        let cwd = cwd_for_trust.clone();
                        let trust_store = trust_store_for_cb.clone();
                        let startup_settings = startup_settings_for_cb.clone();
                        let pt_ctx = project_trust_context.clone();
                        let diag_slot = Arc::clone(&diag_slot);
                        let trust_cache = Arc::clone(&trust_cache);
                        let core_host = Arc::clone(&core_host_for_trust);
                        Box::pin(async move {
                            let resolved_ctx = match pt_ctx {
                                Some(ctx) => extension_project_trust_to_runtime_context(ctx),
                                None => create_project_trust_runtime_context(CreateProjectTrustContextOptions {
                                    cwd: cwd.clone(),
                                    mode: prompt_mode,
                                    settings_manager: startup_settings.clone(),
                                    has_ui: is_initial_runtime && trust_prompt_mode == AppMode::Interactive,
                                }),
                            };
                            let trust_ext_host = core_host
                                .loaded_extensions_result()
                                .map(RunnerProjectTrustExtensionHost::new);
                            let diag_cb_slot = Arc::clone(&diag_slot);
                            let on_ext_error = move |msg: &str| {
                                diag_cb_slot.lock().unwrap_or_else(|e| e.into_inner()).push(
                                    AgentSessionRuntimeDiagnostic {
                                        r#type: AgentSessionRuntimeDiagnosticType::Warning,
                                        message: msg.to_string(),
                                    },
                                );
                            };
                            let trusted = resolve_project_trusted(ResolveProjectTrustedOptions {
                                cwd: &cwd,
                                trust_store: &trust_store,
                                trust_override: override_val,
                                default_project_trust: Some(startup_settings.get_default_project_trust()),
                                extension_host: trust_ext_host.as_ref().map(|h| h as &dyn ProjectTrustExtensionHost),
                                project_trust_context: resolved_ctx.as_ref(),
                                on_extension_error: Some(&on_ext_error),
                            })
                            .await;
                            trust_cache
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .insert(cwd, trusted);
                            trusted
                        })
                    })),
                })
            } else {
                None
            };

            let model_signal = AbortController::new().signal();
            let loader_extension_factories: Vec<LoaderInlineExtension> = extension_factories
                .into_iter()
                .map(LoaderInlineExtension::Core)
                .collect();
            let services = create_agent_session_services(CreateAgentSessionServicesOptions {
                cwd: cwd.clone(),
                agent_dir: Some(agent_dir),
                settings_manager: Some(runtime_settings),
                model_runtime: None,
                model_runtime_signal: Some(model_signal),
                extension_flag_values: Some(extension_flags),
                resource_loader_reload_options: reload_opts,
                resource_loader_options: Some(DefaultResourceLoaderOptions {
                    additional_extension_paths: resolved_ext_paths.unwrap_or_default(),
                    additional_skill_paths: resolved_skill_paths.unwrap_or_default(),
                    additional_prompt_template_paths: resolved_prompt_paths.unwrap_or_default(),
                    additional_theme_paths: resolved_theme_paths.unwrap_or_default(),
                    no_extensions: parsed.no_extensions.unwrap_or(false),
                    no_skills: parsed.no_skills.unwrap_or(false),
                    no_prompt_templates: parsed.no_prompt_templates.unwrap_or(false),
                    no_themes: parsed.no_themes.unwrap_or(false),
                    no_context_files: parsed.no_context_files.unwrap_or(false),
                    system_prompt: parsed.system_prompt.clone(),
                    append_system_prompt: parsed.append_system_prompt.clone(),
                    extension_factories: loader_extension_factories,
                    extension_host: Some(core_host),
                    ..Default::default()
                }),
            })
            .await?;

            project_trust_diagnostics.extend(
                trust_diagnostics_slot
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .drain(..),
            );

            let mut diagnostics = project_trust_diagnostics;
            diagnostics.extend(services.diagnostics.clone());
            diagnostics.extend(collect_settings_diagnostics(&services.settings_manager));
            let ext_res = services.resource_loader.get_extensions();
            for err in &ext_res.errors {
                diagnostics.push(AgentSessionRuntimeDiagnostic {
                    r#type: AgentSessionRuntimeDiagnosticType::Error,
                    message: format!("Failed to load extension \"{}\": {}", err.path, err.error),
                });
            }
            for warn in &ext_res.warnings {
                diagnostics.push(AgentSessionRuntimeDiagnostic {
                    r#type: AgentSessionRuntimeDiagnosticType::Warning,
                    message: format!("Extension package \"{}\": {}", warn.path, warn.warning),
                });
            }

            let model_patterns = parsed
                .models
                .clone()
                .or_else(|| services.settings_manager.get_enabled_models());
            let scoped_models = if let Some(ref patterns) = model_patterns
                && !patterns.is_empty()
            {
                let sig = AbortController::new().signal();
                resolve_model_scope(
                    patterns,
                    &services.model_runtime,
                    Some(AuthOperationOptions { signal: Some(sig) }),
                )
                .await?
            } else {
                Vec::new()
            };

            let built = build_session_options(
                &parsed,
                &scoped_models,
                !session_manager.build_session_context().messages.is_empty(),
                &services.model_runtime,
                &services.settings_manager,
            );
            diagnostics.extend(built.diagnostics);

            if let Some(ref api_key) = parsed.api_key {
                if let Some(ref model) = built.options.model {
                    services
                        .model_runtime
                        .set_runtime_api_key(&model.provider, api_key, None)
                        .await?;
                } else {
                    diagnostics.push(AgentSessionRuntimeDiagnostic {
                        r#type: AgentSessionRuntimeDiagnosticType::Error,
                        message:
                            "--api-key requires a model to be specified via --model, --provider/--model, or --models"
                                .to_string(),
                    });
                }
            }

            let created = create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
                services: services.clone(),
                session_manager,
                session_start_event,
                model: built.options.model,
                thinking_level: built.options.thinking_level,
                scoped_models: built.options.scoped_models,
                tools: built.options.tools,
                exclude_tools: built.options.exclude_tools,
                no_tools: built.options.no_tools,
                custom_tools: built.options.custom_tools,
            })
            .await?;

            let cli_thinking_override = parsed.thinking.is_some() || built.cli_thinking_from_model;
            if created.session.model().is_some() && cli_thinking_override {
                created
                    .session
                    .set_thinking_level(created.session.thinking_level(), None);
            }

            Ok(CreateAgentSessionRuntimeResult::from_session_result(
                created,
                services,
                diagnostics,
            ))
        })
    });

    time("createRuntime", TimingNamespaceKind::Main);
    let runtime = create_agent_session_runtime(
        create_runtime,
        CreateAgentSessionRuntimeOptions {
            cwd: session_manager.get_cwd(),
            agent_dir: agent_dir.clone(),
            session_manager,
            session_start_event: None,
        },
    )
    .await?;
    time("createAgentSessionRuntime", TimingNamespaceKind::Main);

    let services = runtime.services();
    let session = runtime.session();
    set_capability_overrides(services.settings_manager.get_terminal_capability_overrides());
    apply_http_proxy_settings(services.settings_manager.get_global_settings().http_proxy.as_deref());
    let idle_timeout_ms = services
        .settings_manager
        .get_http_idle_timeout_ms()
        .unwrap_or(DEFAULT_HTTP_IDLE_TIMEOUT_MS);
    let _ = configure_http_dispatcher(Some(idle_timeout_ms));

    if parsed.help == Some(true) {
        report_diagnostics(&startup_settings_diagnostics);
        let extension_flags: Vec<_> = services
            .resource_loader
            .get_core_extensions()
            .extensions
            .into_iter()
            .flat_map(|ext| {
                ext.flags
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .values()
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .collect();
        print_help(Some(&extension_flags));
        return Ok(());
    }

    if let Some(ref list_opt) = parsed.list_models {
        report_diagnostics(&startup_settings_diagnostics);
        let search_pattern = match list_opt {
            ListModelsOption::All => None,
            ListModelsOption::Pattern(p) => Some(p.as_str()),
        };
        let sig = AbortController::new().signal();
        list_models(&services.model_runtime, search_pattern, Some(sig)).await?;
        return Ok(());
    }

    let stdin_content = if app_mode != AppMode::Rpc {
        let content = read_piped_stdin().await;
        if content.is_some() && app_mode == AppMode::Interactive {
            app_mode = AppMode::Print;
        }
        content
    } else {
        None
    };
    time("readPipedStdin", TimingNamespaceKind::Main);

    let (initial_message, initial_images) = prepare_initial_message(&mut parsed, stdin_content.as_deref()).await?;
    time("prepareInitialMessage", TimingNamespaceKind::Main);

    // WIRE(@tui-w3): set_theme_json_validator(validate_theme_json); init_theme(services.settings_manager.get_theme(), app_mode == AppMode::Interactive);
    time("initTheme", TimingNamespaceKind::Main);

    if app_mode == AppMode::Interactive && !migration_res.deprecation_warnings.is_empty() {
        show_deprecation_warnings(&migration_res.deprecation_warnings).await;
    }

    time("resolveModelScope", TimingNamespaceKind::Main);
    let mut combined_diags = startup_settings_diagnostics;
    combined_diags.extend(runtime.diagnostics());
    let startup_diagnostics = deduplicate_diagnostics(&combined_diags);
    let has_runtime_errors = runtime
        .diagnostics()
        .iter()
        .any(|d| d.r#type == AgentSessionRuntimeDiagnosticType::Error);

    if app_mode != AppMode::Interactive || has_runtime_errors {
        report_diagnostics(&startup_diagnostics);
    }
    if has_runtime_errors {
        if runtime
            .diagnostics()
            .iter()
            .any(|d| d.message.contains("Failed to load extension"))
        {
            pi_js::console::error(
                &chalk()
                    .yellow()
                    .apply(&format!("Hint: Start without extensions using \"{} -ne\".", *APP_NAME)),
            );
        }
        return Err(pi_js::Error::Exit(1));
    }
    time("createAgentSession", TimingNamespaceKind::Main);

    if app_mode != AppMode::Interactive && session.model().is_none() {
        pi_js::console::error(&chalk().red().apply(&format_no_models_available_message()));
        return Err(pi_js::Error::Exit(1));
    }

    let startup_benchmark = is_truthy_env_flag(pi_js::env::var("PI_STARTUP_BENCHMARK").as_deref());
    if startup_benchmark && app_mode != AppMode::Interactive {
        pi_js::console::error(
            &chalk()
                .red()
                .apply("Error: PI_STARTUP_BENCHMARK only supports interactive mode"),
        );
        return Err(pi_js::Error::Exit(1));
    }

    if !offline_mode && app_mode == AppMode::Rpc {
        let mr = services.model_runtime.clone();
        tokio::spawn(async move {
            let controller = AbortController::new();
            let sig = controller.signal();
            let _ = mr
                .refresh(Some(ModelsRefreshOptions {
                    signal: Some(sig),
                    ..Default::default()
                }))
                .await;
        });
    }

    match app_mode {
        AppMode::Rpc => {
            print_timings();
            // WIRE(@tui-w3): crate::modes::run_rpc_mode(runtime).await
            let _ = (
                &runtime,
                &migration_res.migrated_auth_providers,
                &auto_trust_on_reload_cwd,
            );
            Err(pi_js::Error::msg(
                "RPC mode is not wired yet (pending Stage 3B-2 @tui merge)",
            ))
        }
        AppMode::Interactive => {
            print_timings();
            // WIRE(@tui-w3): InteractiveMode::new(runtime, InteractiveModeOptions { ... }).run().await (plus startup_benchmark)
            let _ = (
                &runtime,
                &startup_diagnostics,
                &initial_message,
                &initial_images,
                &parsed,
            );
            Err(pi_js::Error::msg(
                "Interactive mode is not wired yet (pending Stage 3B-2 @tui merge)",
            ))
        }
        AppMode::Print | AppMode::Json => {
            print_timings();
            // WIRE(@tui-w3): crate::modes::run_print_mode(runtime, PrintModeOptions { ... }).await; stop_theme_watcher();
            restore_stdout();
            let _ = (&runtime, &initial_message, &initial_images, &parsed);
            Err(pi_js::Error::msg(
                "Print/JSON mode is not wired yet (pending Stage 3B-2 @tui merge)",
            ))
        }
    }
}
