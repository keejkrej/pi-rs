//! Port of packages/coding-agent/src/core/sdk.ts

use std::sync::{Arc, Mutex, Once, Weak};

use indexmap::IndexSet;
use pi_agent_core::agent::{Agent, AgentInitialState, AgentOptions};
use pi_agent_core::stream_fn::set_default_stream_fn;
use pi_agent_core::types::{AgentMessage, OnPayloadFn, StreamFn, TransformContextFn};
use pi_ai::compat::stream_simple;
use pi_ai::types::{
    Message, Model, ModelThinkingLevel, OnProviderStreamEventFn, OnResponseFn, ProviderHeaders, SimpleStreamOptions,
    TextContent, UserContentBlock, UserMessageContent,
};
use pi_ai::{ModelsSimpleStreamOptions, clamp_thinking_level};
use pi_js::BoxFuture;

use crate::config::get_agent_dir;
use crate::core::agent_session::{AgentSession, AgentSessionConfig};
use crate::core::auth_guidance::format_no_models_available_message;
use crate::core::cache_warmer::CacheWarmer;
use crate::core::defaults::{DEFAULT_THINKING_LEVEL, to_agent_thinking_level};
use crate::core::extensions::types::{
    AfterProviderResponseEvent, CacheWarmingDecisionEvent, ExtensionRunnerRef, LoadExtensionsResult,
    ProviderStreamEvent, SessionStartEvent, ToolDefinition,
};
use crate::core::messages::convert_to_llm;
use crate::core::model_resolver::{FindInitialModelOptions, find_initial_model};
use crate::core::model_runtime::{ModelRuntime, ModelRuntimeCreateOptions};
use crate::core::provider_attribution::merge_provider_attribution_headers;
use crate::core::resource_loader::{DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader};
use crate::core::session_manager::{SessionEntry, SessionManager, get_default_session_dir};
use crate::core::settings_manager::{DEFAULT_TOOL_NAMES, SettingsManager};
use crate::core::timings::{TimingNamespaceKind, time};
use crate::core::virtual_models::get_branch_selection;
use crate::utils::paths::{PathInputOptions, resolve_path};

pub use crate::core::agent_session_runtime::*;
pub use crate::core::agent_session_services::{AgentSessionRuntimeDiagnostic, AgentSessionServices};
pub use crate::core::extensions::types::{
    ExtensionAPI, ExtensionCommandContext, ExtensionContext, ExtensionFactory, InlineExtension,
    ToolDefinition as SdkToolDefinition,
};
pub use crate::core::prompt_templates::PromptTemplate;
pub use crate::core::skills::Skill;
pub use crate::core::slash_commands::{SlashCommandInfo, SlashCommandSource};
pub use crate::core::tools::bash::create_bash_tool;
pub use crate::core::tools::edit::create_edit_tool;
pub use crate::core::tools::file_mutation_queue::with_file_mutation_queue;
pub use crate::core::tools::find::create_find_tool;
pub use crate::core::tools::grep::create_grep_tool;
pub use crate::core::tools::ls::create_ls_tool;
pub use crate::core::tools::powershell::create_powershell_tool;
pub use crate::core::tools::read::create_read_tool;
pub use crate::core::tools::write::create_write_tool;
pub use crate::core::tools::{Tool, create_coding_tools, create_read_only_tools};

static DEFAULT_STREAM_FN_INIT: Once = Once::new();

/// Preserve the pre-0.81 fallback for extensions that construct Agent instances
/// or invoke low-level agent loops without supplying streamFn.
pub fn ensure_default_stream_fn() {
    DEFAULT_STREAM_FN_INIT.call_once(|| {
        let fallback: StreamFn =
            Arc::new(|model, context, options| Box::pin(async move { Ok(stream_simple(&model, &context, options)) }));
        set_default_stream_fn(Some(fallback));
    });
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScopedModelEntry {
    pub model: Model,
    pub thinking_level: Option<ModelThinkingLevel>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoToolsMode {
    All,
    Builtin,
}

#[derive(Clone, Default)]
pub struct CreateAgentSessionOptions {
    /// Working directory for project-local discovery. Default: current cwd.
    pub cwd: Option<String>,
    /// Global config directory. Default: ~/.pi/agent
    pub agent_dir: Option<String>,
    /// Canonical model/auth runtime.
    pub model_runtime: Option<ModelRuntime>,
    /// Model to use. Default: from settings, else first available.
    pub model: Option<Model>,
    /// Thinking level. Default: from settings, else 'medium' (clamped to model capabilities).
    pub thinking_level: Option<ModelThinkingLevel>,
    /// Models available for cycling (Ctrl+P in interactive mode).
    pub scoped_models: Option<Vec<ScopedModelEntry>>,
    /// Optional default tool suppression mode when no explicit allowlist is provided.
    pub no_tools: Option<NoToolsMode>,
    /// Optional allowlist of tool names.
    pub tools: Option<Vec<String>>,
    /// Optional denylist of tool names to disable. Applies after `tools` when both are provided.
    pub exclude_tools: Option<Vec<String>>,
    /// Custom tools to register (in addition to built-in tools).
    pub custom_tools: Option<Vec<ToolDefinition>>,
    /// Resource loader. When omitted, DefaultResourceLoader is used.
    pub resource_loader: Option<Arc<dyn ResourceLoader>>,
    /// Session manager. Default: SessionManager::create(cwd)
    pub session_manager: Option<SessionManager>,
    /// Settings manager. Default: SettingsManager::create(cwd, agent_dir)
    pub settings_manager: Option<SettingsManager>,
    /// Session start event metadata for extension runtime startup.
    pub session_start_event: Option<SessionStartEvent>,
}

/// Result from `create_agent_session`.
#[derive(Clone)]
pub struct CreateAgentSessionResult {
    /// The created session.
    pub session: AgentSession,
    /// Extensions result (for UI context setup in interactive mode).
    pub extensions_result: LoadExtensionsResult,
    /// Warning if session was restored with a different model than saved.
    pub model_fallback_message: Option<String>,
}

fn filter_blocked_images(messages: Vec<Message>) -> Vec<Message> {
    const PLACEHOLDER: &str = "Image reading is disabled.";
    messages
        .into_iter()
        .map(|msg| match msg {
            Message::User(mut user) => {
                if let UserMessageContent::Blocks(blocks) = user.content {
                    let has_images = blocks.iter().any(|b| matches!(b, UserContentBlock::Image(_)));
                    if has_images {
                        let mut filtered: Vec<UserContentBlock> = Vec::with_capacity(blocks.len());
                        for b in blocks {
                            let next = match b {
                                UserContentBlock::Image(_) => UserContentBlock::Text(TextContent {
                                    text: PLACEHOLDER.to_string(),
                                    text_signature: None,
                                }),
                                other => other,
                            };
                            let is_dup_placeholder = matches!(
                                (&next, filtered.last()),
                                (
                                    UserContentBlock::Text(t1),
                                    Some(UserContentBlock::Text(t2))
                                ) if t1.text == PLACEHOLDER && t2.text == PLACEHOLDER
                            );
                            if !is_dup_placeholder {
                                filtered.push(next);
                            }
                        }
                        user.content = UserMessageContent::Blocks(filtered);
                    } else {
                        user.content = UserMessageContent::Blocks(blocks);
                    }
                }
                Message::User(user)
            }
            Message::ToolResult(mut tr) => {
                let has_images = tr.content.iter().any(|b| matches!(b, UserContentBlock::Image(_)));
                if has_images {
                    let mut filtered: Vec<UserContentBlock> = Vec::with_capacity(tr.content.len());
                    for b in tr.content {
                        let next = match b {
                            UserContentBlock::Image(_) => UserContentBlock::Text(TextContent {
                                text: PLACEHOLDER.to_string(),
                                text_signature: None,
                            }),
                            other => other,
                        };
                        let is_dup_placeholder = matches!(
                            (&next, filtered.last()),
                            (
                                UserContentBlock::Text(t1),
                                Some(UserContentBlock::Text(t2))
                            ) if t1.text == PLACEHOLDER && t2.text == PLACEHOLDER
                        );
                        if !is_dup_placeholder {
                            filtered.push(next);
                        }
                    }
                    tr.content = filtered;
                }
                Message::ToolResult(tr)
            }
            other => other,
        })
        .collect()
}

/// Create an `AgentSession` with the specified options.
pub async fn create_agent_session(
    options: Option<CreateAgentSessionOptions>,
) -> pi_js::Result<CreateAgentSessionResult> {
    ensure_default_stream_fn();
    let options = options.unwrap_or_default();

    let raw_cwd = options
        .cwd
        .or_else(|| options.session_manager.as_ref().map(|sm| sm.get_cwd()))
        .unwrap_or_else(pi_js::env::cwd);
    let cwd = resolve_path(&raw_cwd, None, PathInputOptions::default());
    let agent_dir = options
        .agent_dir
        .as_deref()
        .map(|d| resolve_path(d, None, PathInputOptions::default()))
        .unwrap_or_else(get_agent_dir);

    let auth_path = options
        .agent_dir
        .as_ref()
        .map(|_| pi_js::path::join(&[&agent_dir, "auth.json"]));
    let models_path = options
        .agent_dir
        .as_ref()
        .map(|_| pi_js::path::join(&[&agent_dir, "models.json"]));
    let model_runtime = match options.model_runtime {
        Some(mr) => mr,
        None => {
            ModelRuntime::create(ModelRuntimeCreateOptions {
                auth_path,
                models_path,
                ..Default::default()
            })
            .await?
        }
    };

    let settings_manager = match options.settings_manager {
        Some(sm) => sm,
        None => SettingsManager::create(&cwd, Some(&agent_dir), None),
    };
    let session_manager = match options.session_manager {
        Some(sm) => sm,
        None => SessionManager::create(&cwd, Some(&get_default_session_dir(&cwd, Some(&agent_dir))), None)?,
    };

    let resource_loader: Arc<dyn ResourceLoader> = match options.resource_loader {
        Some(rl) => rl,
        None => {
            let rl = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
                cwd: Some(cwd.clone()),
                agent_dir: Some(agent_dir.clone()),
                settings_manager: Some(settings_manager.clone()),
                ..Default::default()
            }));
            rl.reload(None).await?;
            time("resourceLoader.reload", TimingNamespaceKind::Main);
            rl
        }
    };

    let existing_session = session_manager.build_session_context();
    let has_existing_session = !existing_session.messages.is_empty();
    let branch = session_manager.get_branch(None);
    let has_thinking_entry = branch
        .iter()
        .any(|entry| matches!(entry, SessionEntry::ThinkingLevelChange(_)));

    let mut model = options.model.clone();
    let mut model_fallback_message: Option<String> = None;

    let session_model = get_branch_selection(&branch, |provider, model_id| {
        model_runtime.get_model(provider, model_id)
    });

    if model.is_none()
        && has_existing_session
        && let Some(sm) = &session_model
    {
        if let Some(restored) = model_runtime.get_model(&sm.provider, &sm.model_id)
            && model_runtime.has_configured_auth(&restored.provider)
        {
            model = Some(restored);
        }
        if model.is_none() {
            model_fallback_message = Some(format!("Could not restore model {}/{}", sm.provider, sm.model_id));
        }
    }

    if model.is_none() {
        let initial_result = find_initial_model(FindInitialModelOptions {
            scoped_models: Vec::new(),
            is_continuing: has_existing_session,
            default_provider: settings_manager.get_default_provider(),
            default_model_id: settings_manager.get_default_model(),
            default_thinking_level: settings_manager.get_default_thinking_level(),
            model_thinking_levels: settings_manager.get_all_model_thinking_levels(),
            model_runtime: model_runtime.clone(),
        })
        .await?;
        model = initial_result.model;
        if let Some(ref m) = model {
            if let Some(ref mut fallback) = model_fallback_message {
                fallback.push_str(&format!(". Using {}/{}", m.provider, m.id));
            }
        } else {
            model_fallback_message = Some(format_no_models_available_message());
        }
    }

    let mut thinking_level = options.thinking_level;
    if thinking_level.is_none() && has_existing_session {
        thinking_level = if has_thinking_entry {
            existing_session.thinking_level
        } else {
            settings_manager
                .get_default_thinking_level()
                .or(Some(DEFAULT_THINKING_LEVEL))
        };
    }
    if thinking_level.is_none()
        && let Some(ref m) = model
    {
        thinking_level = settings_manager.get_model_thinking_level(&m.provider, &m.id);
    }
    let resolved_thinking = thinking_level
        .or_else(|| settings_manager.get_default_thinking_level())
        .unwrap_or(DEFAULT_THINKING_LEVEL);

    let clamped_thinking = match &model {
        None => ModelThinkingLevel::Off,
        Some(m) => clamp_thinking_level(m, resolved_thinking),
    };

    let configured_default_tool_names = settings_manager.get_default_tools();
    let allowed_tool_names = if let Some(ref tools) = options.tools {
        Some(tools.clone())
    } else if options.no_tools == Some(NoToolsMode::All) {
        Some(Vec::new())
    } else {
        None
    };
    let excluded_tool_names = options.exclude_tools.clone();
    let excluded_set: Option<IndexSet<String>> =
        excluded_tool_names.as_ref().map(|list| list.iter().cloned().collect());
    let base_initial_tools: Vec<String> = if let Some(ref tools) = options.tools {
        tools.clone()
    } else if options.no_tools.is_some() {
        Vec::new()
    } else if let Some(defaults) = configured_default_tool_names {
        defaults
    } else {
        DEFAULT_TOOL_NAMES.iter().map(|s| (*s).to_string()).collect()
    };
    let initial_active_tool_names: Vec<String> = base_initial_tools
        .into_iter()
        .filter(|name| !excluded_set.as_ref().is_some_and(|set| set.contains(name)))
        .collect();

    let settings_for_llm = settings_manager.clone();
    let convert_to_llm_with_block_images: pi_agent_core::types::ConvertToLlmFn = Arc::new(move |messages| {
        let converted = convert_to_llm(&messages);
        if !settings_for_llm.get_block_images() {
            return converted;
        }
        filter_blocked_images(converted)
    });

    let extension_runner_ref: ExtensionRunnerRef = Arc::new(Mutex::new(None));
    let runner_ref_for_cache = Arc::clone(&extension_runner_ref);
    let settings_for_cache = settings_manager.clone();
    let cache_warmer = Arc::new(CacheWarmer::new(
        model_runtime.clone(),
        session_manager.clone(),
        Arc::new(move || settings_for_cache.get_cache_warming_mode()),
        Arc::new(move |event: CacheWarmingDecisionEvent| {
            let runner_opt = runner_ref_for_cache.lock().unwrap_or_else(|e| e.into_inner()).clone();
            Box::pin(async move {
                if let Some(runner) = runner_opt {
                    runner.emit_cache_warming_decision(event).await
                } else {
                    event.action
                }
            })
        }),
    ));

    let agent_slot: Arc<Mutex<Option<Agent>>> = Arc::new(Mutex::new(None));
    let session_slot: Arc<Mutex<Option<Weak<crate::core::agent_session::AgentSessionInner>>>> =
        Arc::new(Mutex::new(None));

    let runner_ref_for_payload = Arc::clone(&extension_runner_ref);
    let on_payload: OnPayloadFn = Arc::new(move |payload, _model| {
        let runner_opt = runner_ref_for_payload.lock().unwrap_or_else(|e| e.into_inner()).clone();
        Box::pin(async move {
            let Some(runner) = runner_opt else {
                return Ok(None);
            };
            if !runner.has_handlers("before_provider_request") {
                return Ok(None);
            }
            let updated = runner.emit_before_provider_request(payload).await?;
            Ok(Some(updated))
        })
    });

    let runner_ref_for_response = Arc::clone(&extension_runner_ref);
    let handle_provider_response: OnResponseFn<Model> = Arc::new(move |response, _model| {
        let runner_opt = runner_ref_for_response
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        Box::pin(async move {
            let Some(runner) = runner_opt else {
                return Ok(());
            };
            if !runner.has_handlers("after_provider_response") {
                return Ok(());
            }
            runner
                .emit_after_provider_response(AfterProviderResponseEvent {
                    status: response.status,
                    headers: response.headers,
                })
                .await
        })
    });

    let runner_ref_for_stream_ev = Arc::clone(&extension_runner_ref);
    let handle_provider_stream_event: OnProviderStreamEventFn = Arc::new(move |data, event_model| {
        let runner_opt = runner_ref_for_stream_ev
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        Box::pin(async move {
            let Some(runner) = runner_opt else {
                return Ok(());
            };
            if !runner.has_handlers("provider_stream_event") {
                return Ok(());
            }
            runner
                .emit_provider_stream_event(ProviderStreamEvent {
                    data,
                    provider: event_model.provider,
                    api: event_model.api,
                    model: event_model.id,
                })
                .await
        })
    });

    let runner_ref_for_ctx = Arc::clone(&extension_runner_ref);
    let transform_context: TransformContextFn = Arc::new(move |messages, _signal| {
        let runner_opt = runner_ref_for_ctx.lock().unwrap_or_else(|e| e.into_inner()).clone();
        Box::pin(async move {
            let Some(runner) = runner_opt else {
                return Ok(messages);
            };
            runner.emit_context(messages).await
        })
    });

    let settings_for_stream = settings_manager.clone();
    let runner_ref_for_stream = Arc::clone(&extension_runner_ref);
    let model_runtime_for_stream = model_runtime.clone();
    let session_manager_for_stream = session_manager.clone();
    let cache_warmer_for_stream = Arc::clone(&cache_warmer);
    let agent_slot_for_stream = Arc::clone(&agent_slot);
    let session_slot_for_stream = Arc::clone(&session_slot);
    let resp_cb_for_stream = Arc::clone(&handle_provider_response);
    let stream_ev_cb_for_stream = Arc::clone(&handle_provider_stream_event);

    let stream_fn: StreamFn = Arc::new(move |req_model, context, req_opts| {
        let settings = settings_for_stream.clone();
        let runner_ref = Arc::clone(&runner_ref_for_stream);
        let model_runtime = model_runtime_for_stream.clone();
        let session_manager = session_manager_for_stream.clone();
        let cache_warmer = Arc::clone(&cache_warmer_for_stream);
        let agent_slot = Arc::clone(&agent_slot_for_stream);
        let session_slot = Arc::clone(&session_slot_for_stream);
        let on_response = Arc::clone(&resp_cb_for_stream);
        let on_provider_stream_event = Arc::clone(&stream_ev_cb_for_stream);

        Box::pin(async move {
            let provider_retry = settings.get_provider_retry_settings();
            let http_idle_timeout_ms = settings.get_http_idle_timeout_ms();
            let effective_timeout_ms = if http_idle_timeout_ms == 0 {
                2_147_483_647
            } else {
                http_idle_timeout_ms
            };

            let mut base = req_opts.unwrap_or_default();
            if base.reasoning == Some(pi_ai::types::ThinkingLevel::XHigh)
                && let Some(weak) = session_slot.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
                && let Some(inner) = weak.upgrade()
                && inner.state.lock().unwrap_or_else(|e| e.into_inner()).thinking_level == ModelThinkingLevel::Max
            {
                base.reasoning = Some(pi_ai::types::ThinkingLevel::Max);
            }
            if base.base.timeout_ms.is_none() {
                base.base.timeout_ms = Some(provider_retry.timeout_ms.unwrap_or(effective_timeout_ms));
            }
            if base.base.websocket_connect_timeout_ms.is_none() {
                base.base.websocket_connect_timeout_ms = settings.get_websocket_connect_timeout_ms();
            }
            if base.base.max_retries.is_none() {
                base.base.max_retries = provider_retry.max_retries;
            }
            if base.base.max_retry_delay_ms.is_none() {
                base.base.max_retry_delay_ms = provider_retry.max_retry_delay_ms;
            }
            if base.base.on_response.is_none() {
                base.base.on_response = Some(on_response);
            }
            if base.base.on_provider_stream_event.is_none() {
                base.base.on_provider_stream_event = Some(on_provider_stream_event);
            }

            let session_id_opt = base.base.session_id.clone();
            let req_model_for_headers = req_model.clone();
            let settings_for_headers = settings.clone();
            let runner_ref_for_headers = Arc::clone(&runner_ref);
            let transform_headers: pi_ai::models::TransformHeaders = Arc::new(move |request_headers| {
                let req_model = req_model_for_headers.clone();
                let settings = settings_for_headers.clone();
                let session_id = session_id_opt.clone();
                let runner_ref = Arc::clone(&runner_ref_for_headers);
                Box::pin(async move {
                    let headers: ProviderHeaders = merge_provider_attribution_headers(
                        &req_model,
                        &settings,
                        session_id.as_deref(),
                        Some(request_headers),
                    );
                    let runner_opt = runner_ref.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    if let Some(runner) = runner_opt
                        && runner.has_handlers("before_provider_headers")
                    {
                        runner.emit_before_provider_headers(headers).await
                    } else {
                        Ok(headers)
                    }
                })
            });

            let models_opts = ModelsSimpleStreamOptions {
                base,
                transform_headers: Some(transform_headers),
            };

            if models_opts.base.base.session_id.as_deref() == Some(session_manager.get_session_id().as_str())
                && let Some(agent) = agent_slot.lock().unwrap_or_else(|e| e.into_inner()).clone()
            {
                let captured_messages: Vec<AgentMessage> = agent.state().messages;
                let captured_model = req_model.clone();
                let agent_for_check = agent.clone();
                let is_current: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
                    let st = agent_for_check.state();
                    st.model.provider == captured_model.provider
                        && st.model.id == captured_model.id
                        && captured_messages.len() <= st.messages.len()
                        && captured_messages.iter().zip(st.messages.iter()).all(|(a, b)| a == b)
                });
                cache_warmer.start(&req_model, &context, &models_opts, is_current);
            }

            Ok(model_runtime.stream_simple(&req_model, &context, Some(models_opts)))
        })
    });

    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some(String::new()),
            model: model.clone(),
            thinking_level: Some(to_agent_thinking_level(clamped_thinking)),
            tools: Some(Vec::new()),
            messages: Some(existing_session.messages),
        }),
        convert_to_llm: Some(convert_to_llm_with_block_images),
        transform_context: Some(transform_context),
        steering_mode: Some(settings_manager.get_steering_mode()),
        follow_up_mode: Some(settings_manager.get_follow_up_mode()),
        stream_fn: Some(stream_fn),
        session_id: Some(session_manager.get_session_id()),
        on_payload: Some(on_payload),
        thinking_budgets: Some(settings_manager.get_thinking_budgets()),
        transport: Some(settings_manager.get_transport()),
        max_retry_delay_ms: settings_manager.get_provider_retry_settings().max_retry_delay_ms,
        ..Default::default()
    });
    *agent_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(agent.clone());

    // Restore missing settings metadata for older sessions.
    if has_existing_session {
        if !has_thinking_entry {
            let _ = session_manager.append_thinking_level_change(clamped_thinking.as_str());
        }
    } else {
        if let Some(ref m) = model {
            let _ = session_manager.append_model_change(&m.provider, &m.id);
        }
        let _ = session_manager.append_thinking_level_change(clamped_thinking.as_str());
    }

    let session = AgentSession::new(AgentSessionConfig {
        agent,
        session_manager,
        settings_manager,
        cwd,
        scoped_models: options.scoped_models,
        resource_loader: Arc::clone(&resource_loader),
        custom_tools: options.custom_tools,
        model_runtime,
        cache_warmer: Some(cache_warmer),
        initial_active_tool_names: Some(initial_active_tool_names),
        uses_default_tools: Some(options.tools.is_none() && options.no_tools.is_none()),
        allowed_tool_names,
        excluded_tool_names,
        base_tools_override: None,
        extension_runner_ref: Some(extension_runner_ref),
        session_start_event: options.session_start_event,
        initial_model: model,
        initial_thinking_level: Some(clamped_thinking),
    });
    *session_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::downgrade(&session.inner));

    let extensions_result = resource_loader.get_extensions();
    Ok(CreateAgentSessionResult {
        session,
        extensions_result,
        model_fallback_message,
    })
}
