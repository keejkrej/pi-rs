//! Port of packages/coding-agent/src/experimental/durable/harness-setup.ts

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use indexmap::IndexMap;
use pi_ai::types::ModelThinkingLevel;
use pi_chord::Context;
use pi_durable::Registry;
use pi_durable::create_registry;
use pi_durable::env::node::NodeExecutionEnv;
use pi_durable::env::{EnvCleanup, ExecutionEnv};
use pi_durable::harness::registry::RegistryImpl;
use pi_durable::harness::types::{
    ConversationStreamOptions, EnvFactory, EnvTarget, HarnessSettings, HarnessSettingsSnapshot, ModelRef,
    PartialCompactionPolicy, PartialRetryPolicy, QueueMode,
};
use pi_durable::tools::coding_tools;

use crate::core::http_dispatcher::{apply_http_proxy_settings, configure_http_dispatcher};
use crate::core::model_resolver::{
    FindInitialModelOptions, ResolveCliModelOptions, find_initial_model, resolve_cli_model,
};
use crate::core::model_runtime::ModelRuntime;
use crate::core::settings_manager::SettingsManager;
use crate::experimental::durable::prompt::create_pi_prompt;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// pi's HTTP setup: proxy, idle timeouts, and one dispatcher for fetch.
pub fn configure_harness_http(settings_manager: &SettingsManager) {
    apply_http_proxy_settings(settings_manager.get_global_settings().http_proxy.as_deref());
    configure_http_dispatcher(Some(settings_manager.get_http_idle_timeout_ms()));
}

struct SettingsBackedHarnessSettings {
    settings_manager: Arc<SettingsManager>,
}

impl HarnessSettings for SettingsBackedHarnessSettings {
    fn get(&self) -> HarnessSettingsSnapshot {
        let provider = self.settings_manager.get_provider_retry_settings();
        let idle = self.settings_manager.get_http_idle_timeout_ms();
        let compaction = self.settings_manager.get_compaction_settings();
        let retry = self.settings_manager.get_retry_settings();
        HarnessSettingsSnapshot {
            extensions: None,
            stream: Some(ConversationStreamOptions {
                timeout_ms: Some(
                    provider
                        .timeout_ms
                        .unwrap_or(if idle == 0 { 2_147_483_647 } else { idle }),
                ),
                max_retries: provider.max_retries,
                max_retry_delay_ms: provider.max_retry_delay_ms,
                ..Default::default()
            }),
            retry: Some(PartialRetryPolicy {
                enabled: retry.enabled,
                max_retries: retry.max_retries,
                base_delay_ms: retry.base_delay_ms,
                max_agent_delay_ms: retry.max_agent_delay_ms,
            }),
            compaction: Some(PartialCompactionPolicy {
                enabled: compaction.enabled,
                reserve_tokens: compaction.reserve_tokens,
                keep_recent_tokens: compaction.keep_recent_tokens,
                background_tokens: None,
            }),
            tool_execution: None,
            steering_mode: Some(parse_queue_mode(&self.settings_manager.get_steering_mode())),
            follow_up_mode: Some(parse_queue_mode(&self.settings_manager.get_follow_up_mode())),
        }
    }
}

fn parse_queue_mode(raw: &str) -> QueueMode {
    match raw {
        "all" => QueueMode::All,
        _ => QueueMode::OneAtATime,
    }
}

/// Harness settings read at every use from pi's settings as loaded at startup.
pub fn create_harness_settings(settings_manager: Arc<SettingsManager>) -> Arc<dyn HarnessSettings> {
    Arc::new(SettingsBackedHarnessSettings { settings_manager })
}

/// A registry with pi's coding tools and system prompt.
pub fn create_coding_registry(settings_manager: Arc<SettingsManager>, cwd: &str) -> Arc<RegistryImpl> {
    let registry = create_registry();
    let _ = registry.install(coding_tools());
    let _ = registry.install(create_pi_prompt(settings_manager, cwd.to_string()));
    registry
}

/// One execution environment per directory, shared by every conversation in it.
#[derive(Clone)]
pub struct ExecutionEnvs {
    default_cwd: String,
    envs: Arc<Mutex<IndexMap<String, Arc<NodeExecutionEnv>>>>,
}

impl ExecutionEnvs {
    pub fn new(default_cwd: impl Into<String>) -> Self {
        Self {
            default_cwd: default_cwd.into(),
            envs: Arc::new(Mutex::new(IndexMap::new())),
        }
    }

    pub fn get_or_create(&self, target: &EnvTarget) -> Arc<dyn ExecutionEnv> {
        let cwd = target.cwd.clone().unwrap_or_else(|| self.default_cwd.clone());
        let mut map = lock(&self.envs);
        if let Some(existing) = map.get(&cwd) {
            return Arc::clone(existing) as Arc<dyn ExecutionEnv>;
        }
        let env = Arc::new(NodeExecutionEnv::new(cwd.clone()));
        map.insert(cwd, Arc::clone(&env));
        env
    }

    pub fn env_factory(self: &Arc<Self>) -> EnvFactory {
        let envs = Arc::clone(self);
        Arc::new(move |target: EnvTarget, _ctx: Context| {
            let env = envs.get_or_create(&target);
            Box::pin(futures::future::ready(Ok(Some(env))))
        })
    }

    pub async fn cleanup(&self, context: &Context) {
        let envs: Vec<Arc<NodeExecutionEnv>> = {
            let mut map = lock(&self.envs);
            map.drain(..).map(|(_, env)| env).collect()
        };
        for env in envs {
            env.cleanup(context).await;
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InitialModel {
    pub model: Option<ModelRef>,
    pub thinking_level: Option<ModelThinkingLevel>,
    pub fallback_message: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CliModelSelection {
    pub provider: Option<String>,
    pub model: String,
}

/// The model a new root conversation starts with: an explicit `--provider`/`--model`, or pi's default resolution.
pub async fn find_initial_agent_model(
    settings_manager: &SettingsManager,
    model_runtime: &ModelRuntime,
    cli: Option<&CliModelSelection>,
) -> pi_js::Result<InitialModel> {
    if let Some(cli) = cli {
        let resolved = resolve_cli_model(ResolveCliModelOptions {
            cli_provider: cli.provider.clone(),
            cli_model: Some(cli.model.clone()),
            cli_thinking: None,
            model_runtime,
        });
        if resolved.error.is_some() || resolved.model.is_none() {
            let detail = resolved.error.unwrap_or_else(|| cli.model.clone());
            return Err(pi_js::Error::msg(format!("Could not resolve model: {detail}")));
        }
        let model = resolved
            .model
            .ok_or_else(|| pi_js::Error::msg(format!("Could not resolve model: {}", cli.model)))?;
        return Ok(InitialModel {
            model: Some(ModelRef {
                provider: model.provider,
                model_id: model.id,
            }),
            thinking_level: resolved.thinking_level,
            fallback_message: None,
        });
    }

    let initial = find_initial_model(FindInitialModelOptions {
        scoped_models: Vec::new(),
        is_continuing: false,
        default_provider: settings_manager.get_default_provider(),
        default_model_id: settings_manager.get_default_model(),
        default_thinking_level: settings_manager.get_default_thinking_level(),
        model_thinking_levels: None,
        model_runtime,
    })
    .await?;

    Ok(InitialModel {
        model: initial.model.as_ref().map(|m| ModelRef {
            provider: m.provider.clone(),
            model_id: m.id.clone(),
        }),
        thinking_level: initial.model.as_ref().map(|_| initial.thinking_level),
        fallback_message: initial.fallback_message,
    })
}
