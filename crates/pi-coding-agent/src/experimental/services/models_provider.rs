//! Port of packages/coding-agent/src/experimental/services/models-provider.ts

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_ai::models::{ModelsRefreshOptions, clamp_thinking_level, get_supported_thinking_levels};
use pi_ai::types::{Model, ModelThinkingLevel};
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::{Context, Facet, JsonValue, MutableReplicatedState, define_facet};
use pi_durable::types::DocumentState;
use pi_durable::{AGENT_DOC, AgentChange, AgentState, Conversation, FieldChange, Harness, ModelRef as DurableModelRef};

use crate::core::model_runtime::ModelRuntime;
use crate::core::settings_manager::SettingsManager;
use crate::experimental::services::connection::TypedReplicatedState;
use crate::experimental::services::models::{
    MODELS_SERVICE, ModelRef, ModelSummary, Models, ModelsApi, ModelsCatalog, ModelsConfiguration, ModelsRefreshState,
    ModelsState, models_to_service_object,
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone)]
pub struct ModelsServiceRuntime {
    pub service: Models,
    inner: Arc<ModelsServiceImpl>,
}

struct ModelsServiceImpl {
    conversation: Conversation,
    agent: DocumentState<AgentState>,
    model_runtime: Option<Arc<ModelRuntime>>,
    settings_manager: Option<Arc<SettingsManager>>,
    state: TypedReplicatedState<ModelsState>,
    catalog_revision: Mutex<i64>,
}

impl ModelsServiceRuntime {
    pub async fn activate(&self, context: Context) -> pi_js::Result<()> {
        let catalog = self.inner.read_catalog();
        self.inner.state.change(context, |draft| {
            draft.catalog = catalog;
            draft.refresh = ModelsRefreshState::Idle;
            Ok(())
        })
    }

    /// Publish the agent document's model and thinking level when they changed.
    pub fn sync_configuration(&self, context: Context) -> pi_js::Result<()> {
        let next = configuration_of(self.inner.agent.value().as_ref());
        let current = self.inner.state.value();
        if let Some(ref cur) = current
            && cur.configuration == next
        {
            return Ok(());
        }
        self.inner.state.change(context, |draft| {
            draft.configuration = next;
            Ok(())
        })
    }
}

impl ModelsServiceImpl {
    fn selected_model(&self) -> Option<Model> {
        let ref_val = self.agent.value()?.model?;
        let rt = self.model_runtime.as_ref()?;
        rt.get_model(&ref_val.provider, &ref_val.model_id)
    }

    fn read_thinking_levels(&self) -> Vec<ModelThinkingLevel> {
        match self.selected_model() {
            None => vec![ModelThinkingLevel::Off],
            Some(model) => get_supported_thinking_levels(&model),
        }
    }

    fn read_catalog(&self) -> ModelsCatalog {
        let selected = self.selected_model();
        let available = self
            .model_runtime
            .as_ref()
            .map(|rt| rt.get_available_snapshot())
            .unwrap_or_default();
        let mut catalog = available.clone();
        if let Some(ref sel) = selected
            && !includes_model(&available, sel)
        {
            catalog.push(sel.clone());
        }
        let revision = {
            let mut rev = lock(&self.catalog_revision);
            *rev += 1;
            *rev
        };
        ModelsCatalog {
            revision,
            available_models: catalog
                .into_iter()
                .map(|m| ModelSummary {
                    provider: m.provider,
                    model_id: m.id,
                    name: m.name,
                    reasoning: m.reasoning,
                })
                .collect(),
        }
    }
}

#[async_trait]
impl ModelsApi for ModelsServiceImpl {
    fn state(&self) -> &TypedReplicatedState<ModelsState> {
        &self.state
    }

    async fn cycle_thinking(&self, context: Context) -> pi_js::Result<()> {
        let levels = self.read_thinking_levels();
        let current = self
            .agent
            .value()
            .and_then(|v| v.thinking_level)
            .unwrap_or(ModelThinkingLevel::Off);
        let idx = levels.iter().position(|l| *l == current).unwrap_or(0);
        let next = levels
            .get((idx + 1) % levels.len().max(1))
            .copied()
            .unwrap_or(ModelThinkingLevel::Off);
        self.conversation
            .configure(
                AgentChange {
                    thinking_level: FieldChange::set(next),
                    ..Default::default()
                },
                context,
            )
            .await
    }

    async fn get_thinking_levels(&self, _context: Context) -> pi_js::Result<Vec<ModelThinkingLevel>> {
        Ok(self.read_thinking_levels())
    }

    async fn refresh(&self, context: Context) -> pi_js::Result<()> {
        self.state.change(context.clone(), |draft| {
            draft.refresh = ModelsRefreshState::Refreshing;
            Ok(())
        })?;
        let errors = match &self.model_runtime {
            None => IndexMap::new(),
            Some(rt) => {
                let res = rt
                    .refresh(Some(ModelsRefreshOptions {
                        signal: context.abort_signal(),
                        ..Default::default()
                    }))
                    .await?;
                res.errors
                    .into_iter()
                    .map(|(id, error)| (id, error.message().to_string()))
                    .collect()
            }
        };
        let catalog = self.read_catalog();
        self.state.change(context, |draft| {
            draft.catalog = catalog;
            draft.refresh = if errors.is_empty() {
                ModelsRefreshState::Done
            } else {
                ModelsRefreshState::Warning { errors }
            };
            Ok(())
        })
    }

    async fn select(&self, model: ModelRef, context: Context) -> pi_js::Result<()> {
        let selected = self
            .model_runtime
            .as_ref()
            .and_then(|rt| rt.get_model(&model.provider, &model.model_id))
            .ok_or_else(|| pi_js::Error::msg(format!("Unknown model: {}/{}", model.provider, model.model_id)))?;
        let current_level = self
            .agent
            .value()
            .and_then(|v| v.thinking_level)
            .unwrap_or(ModelThinkingLevel::Off);
        let thinking_level = clamp_thinking_level(&selected, current_level);
        self.conversation
            .configure(
                AgentChange {
                    model: FieldChange::set(DurableModelRef {
                        provider: selected.provider.clone(),
                        model_id: selected.id.clone(),
                    }),
                    thinking_level: FieldChange::set(thinking_level),
                    ..Default::default()
                },
                context,
            )
            .await?;
        if let Some(ref sm) = self.settings_manager {
            sm.set_default_model_and_provider(&selected.provider, &selected.id);
            sm.flush().await?;
        }
        Ok(())
    }

    async fn select_thinking(&self, level: ModelThinkingLevel, context: Context) -> pi_js::Result<()> {
        let levels = self.read_thinking_levels();
        if !levels.contains(&level) {
            let avail: Vec<String> = levels.iter().map(|l| format!("{l:?}").to_lowercase()).collect();
            return Err(pi_js::Error::msg(format!(
                "Thinking level {:?} is unavailable; choose one of: {}",
                level,
                avail.join(", ")
            )));
        }
        self.conversation
            .configure(
                AgentChange {
                    thinking_level: FieldChange::set(level),
                    ..Default::default()
                },
                context,
            )
            .await
    }
}

pub fn create_models_service(
    conversation: Conversation,
    agent: DocumentState<AgentState>,
    model_runtime: Option<Arc<ModelRuntime>>,
    settings_manager: Option<Arc<SettingsManager>>,
    create_state: impl FnOnce(JsonValue) -> pi_js::Result<MutableReplicatedState<JsonValue>>,
) -> pi_js::Result<ModelsServiceRuntime> {
    let initial = ModelsState {
        catalog: ModelsCatalog {
            revision: 0,
            available_models: Vec::new(),
        },
        configuration: configuration_of(agent.value().as_ref()),
        refresh: ModelsRefreshState::Idle,
    };
    let initial_json = serde_json::to_value(&initial).map_err(|e| pi_js::Error::msg(e.to_string()))?;
    let raw_state = create_state(initial_json)?;
    let state = TypedReplicatedState::from_local_raw(raw_state);
    let inner = Arc::new(ModelsServiceImpl {
        conversation,
        agent,
        model_runtime,
        settings_manager,
        state: state.clone(),
        catalog_revision: Mutex::new(0),
    });
    let svc_obj = models_to_service_object(Arc::clone(&inner) as Arc<dyn ModelsApi>);
    let service = Models::from_service(svc_obj)?;
    Ok(ModelsServiceRuntime { service, inner })
}

pub struct ModelsServiceFacetOptions {
    pub harness: Harness,
    pub conversation: Conversation,
    pub model_runtime: Option<Arc<ModelRuntime>>,
    pub settings_manager: Option<Arc<SettingsManager>>,
    pub context: Context,
}

/// Acquire the conversation's agent document, then build the facet that owns it.
pub async fn create_models_service_facet(options: ModelsServiceFacetOptions) -> pi_js::Result<Facet> {
    let agent = options
        .harness
        .session()
        .document_state_conversation(&*AGENT_DOC, options.conversation.id(), options.context)
        .await?
        .ok_or_else(|| {
            pi_js::Error::msg(format!(
                "Conversation {} has no agent document",
                options.conversation.id().as_i64()
            ))
        })?;
    Ok(define_facet(Facet::new(
        "@pi/models",
        Arc::new(move |env| {
            let agent_dispose = agent.clone();
            env.own(Arc::new(move || {
                let a = agent_dispose.clone();
                Box::pin(async move { a.dispose() })
            }))?;
            let runtime = create_models_service(
                options.conversation.clone(),
                agent.clone(),
                options.model_runtime.clone(),
                options.settings_manager.clone(),
                |initial| env.replicated_state(initial),
            )?;
            env.provide(&*MODELS_SERVICE, runtime.service.clone().into_service_object())?;
            let agent_sub = agent.clone();
            let env_activate = env.clone();
            let rt_activate = runtime.clone();
            env.on_activate(Arc::new(move || {
                let agent_sub = agent_sub.clone();
                let env_activate = env_activate.clone();
                let rt_activate = rt_activate.clone();
                Box::pin(async move {
                    rt_activate.activate(BACKGROUND_CONTEXT.clone()).await?;
                    let rt_sub = rt_activate.clone();
                    let unsub = Arc::new(Mutex::new(Some(agent_sub.subscribe(move |_val, ctx, _del| {
                        let rt_inner = rt_sub.clone();
                        Box::pin(async move { rt_inner.sync_configuration(ctx) })
                    }))));
                    env_activate.own(Arc::new(move || {
                        let u = Arc::clone(&unsub);
                        Box::pin(async move {
                            if let Some(fn_unsub) = lock(&u).take() {
                                fn_unsub.call();
                            }
                            Ok(())
                        })
                    }))?;
                    Ok(())
                })
            }))?;
            Ok(())
        }),
    )))
}

fn configuration_of(value: Option<&AgentState>) -> ModelsConfiguration {
    ModelsConfiguration {
        model: value.and_then(|v| v.model.as_ref()).map(|m| ModelRef {
            provider: m.provider.clone(),
            model_id: m.model_id.clone(),
        }),
        thinking_level: value.and_then(|v| v.thinking_level).unwrap_or(ModelThinkingLevel::Off),
    }
}

fn includes_model(models: &[Model], selected: &Model) -> bool {
    models
        .iter()
        .any(|m| m.provider == selected.provider && m.id == selected.id)
}
