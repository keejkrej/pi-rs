//! Port of packages/coding-agent/src/experimental/services/slash-commands-provider.ts

use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};

use indexmap::IndexMap;
use pi_ai::types::ModelThinkingLevel;
use pi_chord::services::handle::ServiceObject;
use pi_chord::{Facet, JsonValue, define_facet};
use pi_js::{BoxFuture, Unsubscribe};

use crate::experimental::services::agent_controller::{
    AGENT_CONTROLLER_SERVICE, AgentCompactionRequest, AgentController,
};
use crate::experimental::services::models::{MODELS_SERVICE, ModelRef, ModelSummary, Models};
use crate::experimental::services::plugins::{
    PRESENTATION_PLUGINS_SERVICE, PresentationPlugins, SESSION_PLUGINS_SERVICE, SessionPlugins,
};
use crate::experimental::services::presentation_ui::{PRESENTATION_UI_SERVICE, PresentationSelectItem, PresentationUI};
use crate::experimental::services::slash_commands::{
    SLASH_COMMANDS_SERVICE, SlashCommandCompletion, SlashCommandContribution, SlashCommandRunResult, SlashCommands,
    SlashCommandsApi, SlashCommandsListener, slash_commands_to_service_object,
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

static SLASH_COMMAND_NAME_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[a-z0-9][a-z0-9:-]*$").unwrap_or_else(|e| panic!("invalid slash command regex: {e}"))
});

fn thinking_description(level: ModelThinkingLevel) -> &'static str {
    match level {
        ModelThinkingLevel::Off => "No reasoning",
        ModelThinkingLevel::Minimal => "Very brief reasoning",
        ModelThinkingLevel::Low => "Light reasoning",
        ModelThinkingLevel::Medium => "Moderate reasoning",
        ModelThinkingLevel::High => "Deep reasoning",
        ModelThinkingLevel::XHigh => "Extra-high reasoning",
        ModelThinkingLevel::Max => "Maximum reasoning",
    }
}

fn thinking_level_name(level: ModelThinkingLevel) -> &'static str {
    match level {
        ModelThinkingLevel::Off => "off",
        ModelThinkingLevel::Minimal => "minimal",
        ModelThinkingLevel::Low => "low",
        ModelThinkingLevel::Medium => "medium",
        ModelThinkingLevel::High => "high",
        ModelThinkingLevel::XHigh => "xhigh",
        ModelThinkingLevel::Max => "max",
    }
}

fn parse_thinking_level(raw: &str) -> Option<ModelThinkingLevel> {
    match raw.to_ascii_lowercase().as_str() {
        "off" => Some(ModelThinkingLevel::Off),
        "minimal" => Some(ModelThinkingLevel::Minimal),
        "low" => Some(ModelThinkingLevel::Low),
        "medium" => Some(ModelThinkingLevel::Medium),
        "high" => Some(ModelThinkingLevel::High),
        "xhigh" => Some(ModelThinkingLevel::XHigh),
        "max" => Some(ModelThinkingLevel::Max),
        _ => None,
    }
}

struct RegisteredSlashCommand {
    id: u64,
    command: SlashCommandContribution,
    closed: bool,
}

struct RegistryState {
    next_id: u64,
    commands: IndexMap<String, Vec<RegisteredSlashCommand>>,
    listeners: IndexMap<u64, SlashCommandsListener>,
}

#[derive(Clone)]
pub struct SlashCommandRegistry {
    state: Arc<Mutex<RegistryState>>,
}

impl Default for SlashCommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SlashCommandRegistry {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(RegistryState {
                next_id: 1,
                commands: IndexMap::new(),
                listeners: IndexMap::new(),
            })),
        }
    }

    pub fn as_slash_commands(self: &Arc<Self>) -> SlashCommands {
        SlashCommands::from_service(slash_commands_to_service_object(
            Arc::clone(self) as Arc<dyn SlashCommandsApi>
        ))
    }

    fn validate(command: &SlashCommandContribution) -> pi_js::Result<()> {
        if !SLASH_COMMAND_NAME_RE.is_match(&command.name) {
            return Err(pi_js::Error::js(
                "TypeError",
                format!("Invalid slash command name: {}", command.name),
            ));
        }
        Ok(())
    }

    fn add(&self, command: SlashCommandContribution) -> Unsubscribe {
        let name = command.name.clone();
        let (entry_id, should_publish, commands_snap, listeners) = {
            let mut state = lock(&self.state);
            let entry_id = state.next_id;
            state.next_id += 1;
            let entries = state.commands.entry(name.clone()).or_default();
            entries.push(RegisteredSlashCommand {
                id: entry_id,
                command,
                closed: false,
            });
            let should_publish = entries.len() == 1;
            let commands_snap = if should_publish {
                snapshot_commands(&state.commands)
            } else {
                Vec::new()
            };
            let listeners = if should_publish {
                state.listeners.values().cloned().collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            (entry_id, should_publish, commands_snap, listeners)
        };
        if should_publish {
            for listener in listeners {
                listener(&commands_snap);
            }
        }
        let state_arc = Arc::clone(&self.state);
        Unsubscribe::new(move || {
            let publish_data = {
                let mut state = lock(&state_arc);
                let Some(entries) = state.commands.get_mut(&name) else {
                    return;
                };
                let mut was_head = false;
                for (idx, item) in entries.iter_mut().enumerate() {
                    if item.id == entry_id {
                        if item.closed {
                            return;
                        }
                        item.closed = true;
                        was_head = idx == 0;
                        break;
                    }
                }
                if !was_head {
                    return;
                }
                while entries.first().is_some_and(|e| e.closed) {
                    entries.remove(0);
                }
                if entries.is_empty() {
                    state.commands.shift_remove(&name);
                }
                let cmds = snapshot_commands(&state.commands);
                let listeners = state.listeners.values().cloned().collect::<Vec<_>>();
                Some((cmds, listeners))
            };
            if let Some((cmds, listeners)) = publish_data {
                for listener in listeners {
                    listener(&cmds);
                }
            }
        })
    }
}

fn snapshot_commands(map: &IndexMap<String, Vec<RegisteredSlashCommand>>) -> Vec<SlashCommandContribution> {
    map.values()
        .filter_map(|entries| entries.first().map(|e| e.command.clone()))
        .collect()
}

impl SlashCommandsApi for SlashCommandRegistry {
    fn register(&self, command: SlashCommandContribution) -> pi_js::Result<Unsubscribe> {
        Self::validate(&command)?;
        if lock(&self.state).commands.contains_key(&command.name) {
            return Err(pi_js::Error::msg(format!(
                "Slash command /{} is already registered",
                command.name
            )));
        }
        Ok(self.add(command))
    }

    fn replace(&self, command: SlashCommandContribution) -> pi_js::Result<Unsubscribe> {
        Self::validate(&command)?;
        Ok(self.add(command))
    }

    fn list(&self) -> Vec<SlashCommandContribution> {
        snapshot_commands(&lock(&self.state).commands)
    }

    fn subscribe(&self, listener: SlashCommandsListener) -> pi_js::Result<Unsubscribe> {
        let (sub_id, initial) = {
            let mut state = lock(&self.state);
            let sub_id = state.next_id;
            state.next_id += 1;
            state.listeners.insert(sub_id, Arc::clone(&listener));
            (sub_id, snapshot_commands(&state.commands))
        };
        listener(&initial);
        let state_arc = Arc::clone(&self.state);
        Ok(Unsubscribe::new(move || {
            lock(&state_arc).listeners.shift_remove(&sub_id);
        }))
    }
}

pub fn create_slash_commands_runtime_facet(registry: Option<Arc<SlashCommandRegistry>>) -> Facet {
    let registry = registry.unwrap_or_else(|| Arc::new(SlashCommandRegistry::new()));
    define_facet(Facet::new(
        "@pi/slash-commands-runtime",
        Arc::new(move |env| {
            let obj = slash_commands_to_service_object(Arc::clone(&registry) as Arc<dyn SlashCommandsApi>);
            env.provide(&*SLASH_COMMANDS_SERVICE, obj)
        }),
    ))
}

pub type ReloadPresentationPluginsFn = Arc<dyn Fn(JsonValue) -> BoxFuture<pi_js::Result<()>> + Send + Sync>;

pub fn create_built_in_slash_commands_facet(reload_presentation_plugins: ReloadPresentationPluginsFn) -> Facet {
    define_facet(Facet::new(
        "@pi/slash-commands-builtin",
        Arc::new(move |env| {
            let commands_obj = env.r#use::<ServiceObject>(&*SLASH_COMMANDS_SERVICE)?;
            let commands = SlashCommands::from_service(commands_obj);
            let models = Models::from_service(env.r#use::<ServiceObject>(&*MODELS_SERVICE)?)?;
            let controller = AgentController::from_service(env.r#use::<ServiceObject>(&*AGENT_CONTROLLER_SERVICE)?);
            let ui = PresentationUI::from_service(env.r#use::<ServiceObject>(&*PRESENTATION_UI_SERVICE)?);
            let presentation_plugins =
                PresentationPlugins::from_service(env.r#use::<ServiceObject>(&*PRESENTATION_PLUGINS_SERVICE)?);
            let session_plugins = SessionPlugins::from_service(env.r#use::<ServiceObject>(&*SESSION_PLUGINS_SERVICE)?);
            let reload_cb = Arc::clone(&reload_presentation_plugins);
            let env_activate = env.clone();

            env.on_activate(Arc::new(move || {
                let commands = commands.clone();
                let models = models.clone();
                let controller = controller.clone();
                let ui = ui.clone();
                let presentation_plugins = presentation_plugins.clone();
                let session_plugins = session_plugins.clone();
                let reload_cb = Arc::clone(&reload_cb);
                let env_own = env_activate.clone();

                Box::pin(async move {
                    let unsub_model = commands.replace(model_command(models.clone(), ui.clone()))?;
                    own_unsub(&env_own, unsub_model)?;

                    let unsub_thinking = commands.replace(thinking_command(models, ui.clone()))?;
                    own_unsub(&env_own, unsub_thinking)?;

                    let unsub_compact = commands.replace(compact_command(controller, ui.clone()))?;
                    own_unsub(&env_own, unsub_compact)?;

                    let unsub_reload = commands.replace(SlashCommandContribution {
                        name: "reload".to_string(),
                        description: "Reload server-selected plugins".to_string(),
                        argument_hint: None,
                        get_argument_completions: None,
                        run: Arc::new(move |_args, context| {
                            let ui = ui.clone();
                            let presentation_plugins = presentation_plugins.clone();
                            let session_plugins = session_plugins.clone();
                            let reload_cb = Arc::clone(&reload_cb);
                            Box::pin(async move {
                                let _ = ui.show_status("Reloading plugins…", context.clone());
                                let data = presentation_plugins.reload(context.clone()).await?;
                                session_plugins.reload(context.clone()).await?;
                                reload_cb(data).await?;
                                let _ = ui.show_status("Reloaded plugins.", context);
                                Ok(None)
                            })
                        }),
                    })?;
                    own_unsub(&env_own, unsub_reload)?;
                    Ok(())
                })
            }))
        }),
    ))
}

fn own_unsub(env: &pi_chord::FacetEnvironment, unsub: Unsubscribe) -> pi_js::Result<()> {
    let cell = Arc::new(Mutex::new(Some(unsub)));
    env.own(Arc::new(move || {
        let cell = Arc::clone(&cell);
        Box::pin(async move {
            if let Some(u) = lock(&cell).take() {
                u.call();
            }
            Ok(())
        })
    }))
}

fn model_command(models: Models, ui: PresentationUI) -> SlashCommandContribution {
    let models_comp = models.clone();
    SlashCommandContribution {
        name: "model".to_string(),
        description: "Select model".to_string(),
        argument_hint: Some("<provider/model>".to_string()),
        get_argument_completions: Some(Arc::new(move |prefix| {
            let normalized = prefix.to_ascii_lowercase();
            let completions: Vec<SlashCommandCompletion> = models_comp
                .state
                .value()
                .map(|s| {
                    s.catalog
                        .available_models
                        .into_iter()
                        .filter(|m| {
                            format!("{}/{} {}", m.provider, m.model_id, m.name)
                                .to_ascii_lowercase()
                                .contains(&normalized)
                        })
                        .map(|m| SlashCommandCompletion {
                            value: format!("{}/{}", m.provider, m.model_id),
                            label: m.model_id,
                            description: Some(m.provider),
                        })
                        .collect()
                })
                .unwrap_or_default();
            Box::pin(futures::future::ready(Ok(Some(completions))))
        })),
        run: Arc::new(move |args, context| {
            let models = models.clone();
            let ui = ui.clone();
            Box::pin(async move {
                let state = models
                    .state
                    .value()
                    .ok_or_else(|| pi_js::Error::msg("Models service is not ready"))?;
                let mut selected = exact_model(&state.catalog.available_models, &args);
                if !args.is_empty() && selected.is_none() {
                    return Err(pi_js::Error::msg(format!("Unknown model: {args}")));
                }
                if selected.is_none() {
                    let items: Vec<PresentationSelectItem> = state
                        .catalog
                        .available_models
                        .iter()
                        .map(|m| {
                            let is_cur = state
                                .configuration
                                .model
                                .as_ref()
                                .is_some_and(|c| c.provider == m.provider && c.model_id == m.model_id);
                            PresentationSelectItem {
                                value: format!("{}/{}", m.provider, m.model_id),
                                label: if is_cur {
                                    format!("{} (selected)", m.name)
                                } else {
                                    m.name.clone()
                                },
                                description: Some(format!("{}/{}", m.provider, m.model_id)),
                            }
                        })
                        .collect();
                    let cur_val = state
                        .configuration
                        .model
                        .as_ref()
                        .map(|m| format!("{}/{}", m.provider, m.model_id));
                    let value = ui
                        .select("Select model:", &items, cur_val.as_deref(), context.clone())
                        .await?;
                    let Some(value) = value else {
                        return Ok(None);
                    };
                    selected = exact_model(&state.catalog.available_models, &value);
                    if selected.is_none() {
                        return Err(pi_js::Error::msg(format!("Unknown model: {value}")));
                    }
                }
                let sel = selected.ok_or_else(|| pi_js::Error::msg("Unknown model"))?;
                models
                    .select(
                        ModelRef {
                            provider: sel.provider.clone(),
                            model_id: sel.model_id.clone(),
                        },
                        context.clone(),
                    )
                    .await?;
                let _ = ui.show_status(&format!("Selected {}/{}.", sel.provider, sel.model_id), context);
                Ok(None)
            })
        }),
    }
}

fn thinking_command(models: Models, ui: PresentationUI) -> SlashCommandContribution {
    SlashCommandContribution {
        name: "thinking".to_string(),
        description: "Set thinking level".to_string(),
        argument_hint: Some("<level>".to_string()),
        get_argument_completions: None,
        run: Arc::new(move |args, context| {
            let models = models.clone();
            let ui = ui.clone();
            Box::pin(async move {
                let levels = models.get_thinking_levels(context.clone()).await?;
                let mut selected = parse_thinking_level(&args).filter(|lvl| levels.contains(lvl));
                if !args.is_empty() && selected.is_none() {
                    let avail: Vec<&'static str> = levels.iter().map(|l| thinking_level_name(*l)).collect();
                    return Err(pi_js::Error::msg(format!(
                        "Unknown thinking level \"{args}\". Available levels: {}.",
                        avail.join(", ")
                    )));
                }
                if selected.is_none() {
                    let current = models.state.value().map(|s| s.configuration.thinking_level);
                    let items: Vec<PresentationSelectItem> = levels
                        .iter()
                        .map(|lvl| {
                            let name = thinking_level_name(*lvl);
                            PresentationSelectItem {
                                value: name.to_string(),
                                label: if current == Some(*lvl) {
                                    format!("{name} (selected)")
                                } else {
                                    name.to_string()
                                },
                                description: Some(thinking_description(*lvl).to_string()),
                            }
                        })
                        .collect();
                    let cur_name = current.map(thinking_level_name);
                    let value = ui
                        .select("Select thinking level:", &items, cur_name, context.clone())
                        .await?;
                    let Some(value) = value else {
                        return Ok(None);
                    };
                    selected = parse_thinking_level(&value).filter(|lvl| levels.contains(lvl));
                    if selected.is_none() {
                        return Err(pi_js::Error::msg(format!("Unknown thinking level: {value}")));
                    }
                }
                let sel = selected.unwrap_or(ModelThinkingLevel::Off);
                models.select_thinking(sel, context.clone()).await?;
                let _ = ui.show_status(&format!("Thinking level: {}.", thinking_level_name(sel)), context);
                Ok(None)
            })
        }),
    }
}

fn compact_command(controller: AgentController, ui: PresentationUI) -> SlashCommandContribution {
    SlashCommandContribution {
        name: "compact".to_string(),
        description: "Manually compact the session context".to_string(),
        argument_hint: Some("<instructions>".to_string()),
        get_argument_completions: None,
        run: Arc::new(move |args, context| {
            let controller = controller.clone();
            let ui = ui.clone();
            Box::pin(async move {
                let _ = ui.show_status("Compacting…", context.clone());
                let res = controller
                    .compact(
                        AgentCompactionRequest {
                            custom_instructions: if args.is_empty() { None } else { Some(args) },
                        },
                        context,
                    )
                    .await?;
                Ok(Some(SlashCommandRunResult::Operation(res)))
            })
        }),
    }
}

fn exact_model(models: &[ModelSummary], query: &str) -> Option<ModelSummary> {
    if query.is_empty() {
        return None;
    }
    let normalized = query.to_ascii_lowercase();
    let matches: Vec<&ModelSummary> = models
        .iter()
        .filter(|m| {
            format!("{}/{}", m.provider, m.model_id).to_ascii_lowercase() == normalized
                || m.model_id.to_ascii_lowercase() == normalized
        })
        .collect();
    if matches.len() == 1 {
        Some(matches[0].clone())
    } else {
        None
    }
}
