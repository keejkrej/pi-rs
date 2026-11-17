//! Port of packages/coding-agent/src/core/agent-session-services.ts

use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::{Model, ModelThinkingLevel};
use pi_js::abort::AbortSignal;
use serde::{Deserialize, Serialize};

use crate::config::get_agent_dir;
use crate::core::extensions::types::{FlagType, FlagValue, SessionStartEvent, ToolDefinition};
use crate::core::model_runtime::{ModelRuntime, ModelRuntimeCreateOptions, ModelRuntimeRefreshOptions};
use crate::core::resource_loader::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader, ResourceLoaderReloadOptions,
};
use crate::core::sdk::{
    CreateAgentSessionOptions, CreateAgentSessionResult, NoToolsMode, ScopedModelEntry, create_agent_session,
};
use crate::core::session_manager::SessionManager;
use crate::core::settings_manager::SettingsManager;
use crate::utils::paths::{PathInputOptions, resolve_path};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentSessionRuntimeDiagnosticType {
    Info,
    Warning,
    Error,
}

/// Non-fatal issues collected while creating services or sessions.
///
/// Runtime creation returns diagnostics to the caller instead of printing or
/// exiting. The app layer decides whether warnings should be shown and whether
/// errors should abort startup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionRuntimeDiagnostic {
    pub r#type: AgentSessionRuntimeDiagnosticType,
    pub message: String,
}

/// Inputs for creating cwd-bound runtime services.
///
/// These services are recreated whenever the effective session cwd changes.
/// CLI-provided resource paths should be resolved to absolute paths before they
/// reach this function, so later cwd switches do not reinterpret them.
#[derive(Clone, Default)]
pub struct CreateAgentSessionServicesOptions {
    pub cwd: String,
    pub agent_dir: Option<String>,
    pub settings_manager: Option<SettingsManager>,
    pub model_runtime: Option<ModelRuntime>,
    pub model_runtime_signal: Option<AbortSignal>,
    pub extension_flag_values: Option<IndexMap<String, FlagValue>>,
    pub resource_loader_options: Option<DefaultResourceLoaderOptions>,
    pub resource_loader_reload_options: Option<ResourceLoaderReloadOptions>,
}

/// Inputs for creating an AgentSession from already-created services.
///
/// Use this after services exist and any cwd-bound model/tool/session options
/// have been resolved against those services.
#[derive(Clone)]
pub struct CreateAgentSessionFromServicesOptions {
    pub services: AgentSessionServices,
    pub session_manager: SessionManager,
    pub session_start_event: Option<SessionStartEvent>,
    pub model: Option<Model>,
    pub thinking_level: Option<ModelThinkingLevel>,
    pub scoped_models: Option<Vec<ScopedModelEntry>>,
    pub tools: Option<Vec<String>>,
    pub exclude_tools: Option<Vec<String>>,
    pub no_tools: Option<NoToolsMode>,
    pub custom_tools: Option<Vec<ToolDefinition>>,
}

/// Coherent cwd-bound runtime services for one effective session cwd.
///
/// This is infrastructure only. The AgentSession itself is created separately so
/// session options can be resolved against these services first.
#[derive(Clone)]
pub struct AgentSessionServices {
    pub cwd: String,
    pub agent_dir: String,
    pub model_runtime: ModelRuntime,
    pub settings_manager: SettingsManager,
    pub resource_loader: Arc<dyn ResourceLoader>,
    pub diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
}

fn apply_extension_flag_values(
    resource_loader: &dyn ResourceLoader,
    extension_flag_values: Option<&IndexMap<String, FlagValue>>,
) -> Vec<AgentSessionRuntimeDiagnostic> {
    let Some(extension_flag_values) = extension_flag_values else {
        return Vec::new();
    };

    let mut diagnostics = Vec::new();
    let extensions_result = resource_loader.get_extensions();
    let mut registered_flags: IndexMap<String, FlagType> = IndexMap::new();
    for extension in &extensions_result.extensions {
        let flags_guard = extension.flags.lock().unwrap_or_else(|e| e.into_inner());
        for (name, flag) in flags_guard.iter() {
            registered_flags.insert(name.clone(), flag.r#type);
        }
    }

    let mut unknown_flags: Vec<String> = Vec::new();
    for (name, value) in extension_flag_values {
        let Some(flag_type) = registered_flags.get(name).copied() else {
            unknown_flags.push(name.clone());
            continue;
        };
        if flag_type == FlagType::Boolean {
            let mut flag_values = extensions_result
                .runtime
                .inner
                .flag_values
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            flag_values.insert(name.clone(), FlagValue::Bool(true));
            continue;
        }
        if let FlagValue::String(s) = value {
            let mut flag_values = extensions_result
                .runtime
                .inner
                .flag_values
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            flag_values.insert(name.clone(), FlagValue::String(s.clone()));
            continue;
        }
        diagnostics.push(AgentSessionRuntimeDiagnostic {
            r#type: AgentSessionRuntimeDiagnosticType::Error,
            message: format!("Extension flag \"--{name}\" requires a value"),
        });
    }

    if !unknown_flags.is_empty() {
        let plural = if unknown_flags.len() == 1 { "" } else { "s" };
        let formatted: Vec<String> = unknown_flags.iter().map(|name| format!("--{name}")).collect();
        diagnostics.push(AgentSessionRuntimeDiagnostic {
            r#type: AgentSessionRuntimeDiagnosticType::Error,
            message: format!("Unknown option{plural}: {}", formatted.join(", ")),
        });
    }

    diagnostics
}

/// Create cwd-bound runtime services.
///
/// Returns services plus diagnostics. It does not create an AgentSession.
pub async fn create_agent_session_services(
    options: CreateAgentSessionServicesOptions,
) -> pi_js::Result<AgentSessionServices> {
    let cwd = resolve_path(&options.cwd, None, PathInputOptions::default());
    let agent_dir = options
        .agent_dir
        .as_deref()
        .map(|d| resolve_path(d, None, PathInputOptions::default()))
        .unwrap_or_else(get_agent_dir);

    let model_runtime = match options.model_runtime {
        Some(mr) => mr,
        None => {
            ModelRuntime::create(ModelRuntimeCreateOptions {
                auth_path: Some(pi_js::path::join(&[&agent_dir, "auth.json"])),
                models_path: Some(pi_js::path::join(&[&agent_dir, "models.json"])),
                signal: options.model_runtime_signal,
                ..Default::default()
            })
            .await?
        }
    };

    let settings_manager = match options.settings_manager {
        Some(sm) => sm,
        None => SettingsManager::create(&cwd, Some(&agent_dir), None)?,
    };

    let mut loader_opts = options.resource_loader_options.unwrap_or_default();
    loader_opts.cwd = Some(cwd.clone());
    loader_opts.agent_dir = Some(agent_dir.clone());
    loader_opts.settings_manager = Some(settings_manager.clone());
    let resource_loader = Arc::new(DefaultResourceLoader::new(loader_opts));
    resource_loader.reload(options.resource_loader_reload_options).await?;

    let mut diagnostics = Vec::new();
    let extensions_result = resource_loader.get_extensions();

    let pending_providers = {
        let mut guard = extensions_result
            .runtime
            .inner
            .pending_provider_registrations
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *guard)
    };
    for reg in pending_providers {
        if let Err(err) = model_runtime.register_provider(&reg.name, reg.config) {
            diagnostics.push(AgentSessionRuntimeDiagnostic {
                r#type: AgentSessionRuntimeDiagnosticType::Error,
                message: format!("Extension \"{}\" error: {err}", reg.extension_path),
            });
        }
    }

    let pending_native = {
        let mut guard = extensions_result
            .runtime
            .inner
            .pending_native_provider_registrations
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *guard)
    };
    for reg in pending_native {
        if let Err(err) = model_runtime.register_native_provider(reg.provider) {
            diagnostics.push(AgentSessionRuntimeDiagnostic {
                r#type: AgentSessionRuntimeDiagnosticType::Error,
                message: format!("Extension \"{}\" error: {err}", reg.extension_path),
            });
        }
    }

    let pending_virtual = {
        let mut guard = extensions_result
            .runtime
            .inner
            .pending_virtual_model_registrations
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *guard)
    };
    for reg in pending_virtual {
        if let Err(err) = model_runtime.register_virtual_model(reg.definition) {
            diagnostics.push(AgentSessionRuntimeDiagnostic {
                r#type: AgentSessionRuntimeDiagnosticType::Error,
                message: format!("Extension \"{}\" error: {err}", reg.extension_path),
            });
        }
    }

    let _ = model_runtime
        .refresh(Some(ModelRuntimeRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        }))
        .await;

    diagnostics.extend(apply_extension_flag_values(
        resource_loader.as_ref(),
        options.extension_flag_values.as_ref(),
    ));

    Ok(AgentSessionServices {
        cwd,
        agent_dir,
        model_runtime,
        settings_manager,
        resource_loader,
        diagnostics,
    })
}

/// Create an AgentSession from previously created services.
///
/// This keeps session creation separate from service creation so callers can
/// resolve model, thinking, tools, and other session inputs against the target
/// cwd before constructing the session.
pub async fn create_agent_session_from_services(
    options: CreateAgentSessionFromServicesOptions,
) -> pi_js::Result<CreateAgentSessionResult> {
    create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(options.services.cwd),
        agent_dir: Some(options.services.agent_dir),
        model_runtime: Some(options.services.model_runtime),
        settings_manager: Some(options.services.settings_manager),
        resource_loader: Some(options.services.resource_loader),
        session_manager: Some(options.session_manager),
        model: options.model,
        thinking_level: options.thinking_level,
        scoped_models: options.scoped_models,
        tools: options.tools,
        exclude_tools: options.exclude_tools,
        no_tools: options.no_tools,
        custom_tools: options.custom_tools,
        session_start_event: options.session_start_event,
    }))
    .await
}
