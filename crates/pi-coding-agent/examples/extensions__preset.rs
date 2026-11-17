//! Port of packages/coding-agent/examples/extensions/preset.ts

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_agent_core::types::ThinkingLevel;
use pi_ai::types::Model;
use pi_coding_agent::config::{CONFIG_DIR_NAME, get_agent_dir};
use pi_coding_agent::core::extensions::types::{
    BeforeAgentStartEvent, BeforeAgentStartEventResult, Extension, ExtensionApi, ExtensionContext, ExtensionFactory,
    FlagOptions, FlagType, FlagValue, InlineExtension, NotifyType, RegisteredCommandOptions, SessionStartEvent,
    ShortcutOptions, Theme, TurnStartEvent,
};
use pi_tui::components::select_list::{SelectItem, SelectList, SelectListTheme};
use pi_tui::{Component, Container, Key, Text};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Preset {
    provider: Option<String>,
    model: Option<String>,
    thinking_level: Option<String>,
    tools: Option<Vec<String>>,
    instructions: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PresetStateEntry {
    name: String,
}

#[derive(Clone)]
struct OriginalState {
    model: Option<Model>,
    thinking_level: ThinkingLevel,
    tools: Vec<String>,
}

struct SharedState {
    presets: IndexMap<String, Preset>,
    active_preset_name: Option<String>,
    active_preset: Option<Preset>,
    original_state: Option<OriginalState>,
}

struct FallbackDynamicBorder {
    // WIRE(@tui-w3): use pi_coding_agent::modes::interactive::components::DynamicBorder once @tui merges.
    color: Arc<dyn Fn(&str) -> String + Send + Sync>,
}

impl FallbackDynamicBorder {
    fn new(color: Arc<dyn Fn(&str) -> String + Send + Sync>) -> Self {
        Self { color }
    }
}

impl Component for FallbackDynamicBorder {
    fn render(&self, width: usize) -> Vec<String> {
        vec![(self.color)(&"─".repeat(width.max(1)))]
    }

    fn invalidate(&self) {}
}

struct PresetSelectorModal {
    container: Container,
    select_list: Arc<SelectList>,
    tui: Arc<dyn pi_tui::TUI>,
}

impl Component for PresetSelectorModal {
    fn render(&self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn handle_input(&self, data: &str) {
        self.select_list.handle_input(data);
        self.tui.request_render(None);
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn invalidate(&self) {
        self.container.invalidate();
    }
}

fn parse_thinking_level(raw: &str) -> Option<ThinkingLevel> {
    match raw.trim().to_lowercase().as_str() {
        "off" => Some(ThinkingLevel::Off),
        "minimal" => Some(ThinkingLevel::Minimal),
        "low" => Some(ThinkingLevel::Low),
        "medium" => Some(ThinkingLevel::Medium),
        "high" => Some(ThinkingLevel::High),
        "xhigh" | "max" => Some(ThinkingLevel::Xhigh),
        _ => None,
    }
}

fn load_presets(cwd: &str) -> IndexMap<String, Preset> {
    let global_path = pi_js::path::join(&[&get_agent_dir(), "presets.json"]);
    let project_path = pi_js::path::join(&[cwd, *CONFIG_DIR_NAME, "presets.json"]);
    let mut merged: IndexMap<String, Preset> = IndexMap::new();

    if pi_js::fs::exists(&global_path) {
        match pi_js::fs::read_to_string(&global_path).and_then(|s| {
            serde_json::from_str::<IndexMap<String, Preset>>(&s).map_err(|e| pi_js::Error::msg(e.to_string()))
        }) {
            Ok(global_map) => {
                for (k, v) in global_map {
                    merged.insert(k, v);
                }
            }
            Err(err) => {
                pi_js::console::error(&format!("Failed to load global presets from {global_path}: {err}"));
            }
        }
    }

    if pi_js::fs::exists(&project_path) {
        match pi_js::fs::read_to_string(&project_path).and_then(|s| {
            serde_json::from_str::<IndexMap<String, Preset>>(&s).map_err(|e| pi_js::Error::msg(e.to_string()))
        }) {
            Ok(project_map) => {
                for (k, v) in project_map {
                    merged.insert(k, v);
                }
            }
            Err(err) => {
                pi_js::console::error(&format!("Failed to load project presets from {project_path}: {err}"));
            }
        }
    }

    merged
}

fn build_preset_description(preset: &Preset) -> String {
    let mut parts = Vec::new();
    if let (Some(p), Some(m)) = (preset.provider.as_deref(), preset.model.as_deref()) {
        parts.push(format!("{p}/{m}"));
    }
    if let Some(level) = preset.thinking_level.as_deref() {
        parts.push(format!("thinking:{level}"));
    }
    if let Some(tools) = preset.tools.as_ref() {
        parts.push(format!("tools:{}", tools.join(",")));
    }
    if let Some(instr) = preset.instructions.as_deref() {
        let truncated = if instr.chars().count() > 30 {
            format!("{}...", instr.chars().take(27).collect::<String>())
        } else {
            instr.to_string()
        };
        parts.push(format!("\"{truncated}\""));
    }
    parts.join(" | ")
}

fn update_status(ctx: &ExtensionContext, active_name: Option<&str>) -> pi_js::Result<()> {
    let ui = ctx.ui()?;
    if let Some(name) = active_name {
        let theme = ui.theme();
        ui.set_status("preset", Some(&theme.fg("accent", &format!("preset:{name}"))));
    } else {
        ui.set_status("preset", None);
    }
    Ok(())
}

async fn apply_preset(
    pi: &ExtensionApi,
    state: &Mutex<SharedState>,
    name: &str,
    preset: &Preset,
    ctx: &ExtensionContext,
) -> pi_js::Result<()> {
    let need_snapshot = {
        state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active_preset_name
            .is_none()
    };
    if need_snapshot {
        let snap = OriginalState {
            model: ctx.model()?,
            thinking_level: pi.get_thinking_level()?,
            tools: pi.get_active_tools()?,
        };
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        if guard.active_preset_name.is_none() {
            guard.original_state = Some(snap);
        }
    }

    if let (Some(prov), Some(model_id)) = (preset.provider.as_deref(), preset.model.as_deref()) {
        let model = ctx
            .model_registry()?
            .get_available()
            .into_iter()
            .find(|m| m.provider == prov && m.id == model_id);
        match model {
            Some(m) => {
                if !pi.set_model(m).await? {
                    ctx.ui()?.notify(
                        &format!("Preset \"{name}\": No API key for {prov}/{model_id}"),
                        Some(NotifyType::Warning),
                    );
                }
            }
            None => {
                ctx.ui()?.notify(
                    &format!("Preset \"{name}\": Model {prov}/{model_id} not found"),
                    Some(NotifyType::Warning),
                );
            }
        }
    }

    if let Some(level_str) = preset.thinking_level.as_deref()
        && let Some(level) = parse_thinking_level(level_str)
    {
        pi.set_thinking_level(level)?;
    }

    if let Some(tools) = preset.tools.as_ref()
        && !tools.is_empty()
    {
        let all_tool_names: Vec<String> = pi.get_all_tools()?.into_iter().map(|t| t.name).collect();
        let valid: Vec<String> = tools
            .iter()
            .filter(|t| all_tool_names.iter().any(|a| a == *t))
            .cloned()
            .collect();
        let invalid: Vec<String> = tools
            .iter()
            .filter(|t| !all_tool_names.iter().any(|a| a == *t))
            .cloned()
            .collect();
        if !invalid.is_empty() {
            ctx.ui()?.notify(
                &format!("Preset \"{name}\": Unknown tools: {}", invalid.join(", ")),
                Some(NotifyType::Warning),
            );
        }
        if !valid.is_empty() {
            pi.set_active_tools(valid)?;
        }
    }

    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
    guard.active_preset_name = Some(name.to_string());
    guard.active_preset = Some(preset.clone());
    Ok(())
}

async fn clear_preset(pi: &ExtensionApi, state: &Mutex<SharedState>, ctx: &ExtensionContext) -> pi_js::Result<()> {
    let orig = {
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        guard.active_preset_name = None;
        guard.active_preset = None;
        guard.original_state.clone()
    };
    if let Some(orig) = orig {
        if let Some(m) = orig.model {
            pi.set_model(m).await?;
        }
        pi.set_thinking_level(orig.thinking_level)?;
        pi.set_active_tools(orig.tools)?;
    } else {
        pi.set_active_tools(vec![
            "read".to_string(),
            "bash".to_string(),
            "edit".to_string(),
            "write".to_string(),
        ])?;
    }
    ctx.ui()?
        .notify("Preset cleared, defaults restored", Some(NotifyType::Info));
    update_status(ctx, None)
}

async fn show_preset_selector(
    pi: &ExtensionApi,
    state: &Mutex<SharedState>,
    ctx: &ExtensionContext,
) -> pi_js::Result<()> {
    let (presets_snapshot, active_name) = {
        let guard = state.lock().unwrap_or_else(|e| e.into_inner());
        (guard.presets.clone(), guard.active_preset_name.clone())
    };
    if presets_snapshot.is_empty() {
        ctx.ui()?.notify(
            &format!(
                "No presets defined. Add presets to {} or {}",
                pi_js::path::join(&[&get_agent_dir(), "presets.json"]),
                pi_js::path::join(&[&ctx.cwd()?, *CONFIG_DIR_NAME, "presets.json"])
            ),
            Some(NotifyType::Warning),
        );
        return Ok(());
    }

    let mut items: Vec<SelectItem> = presets_snapshot
        .iter()
        .map(|(name, preset)| {
            let is_active = active_name.as_deref() == Some(name.as_str());
            SelectItem {
                value: name.clone(),
                label: if is_active {
                    format!("{name} (active)")
                } else {
                    name.clone()
                },
                description: Some(build_preset_description(preset)),
            }
        })
        .collect();
    items.push(SelectItem {
        value: "(none)".to_string(),
        label: "(none)".to_string(),
        description: Some("Clear active preset, restore defaults".to_string()),
    });

    let val = ctx
        .ui()?
        .custom(
            Box::new(move |tui, theme: Theme, _kb, done| {
                Box::pin(async move {
                    let container = Container::new();
                    let t1 = theme.clone();
                    container.add_child(Arc::new(FallbackDynamicBorder::new(Arc::new(move |s| {
                        t1.fg("accent", s)
                    }))));
                    container.add_child(Arc::new(Text::new(
                        Some(&theme.fg("accent", &theme.bold("Select Preset"))),
                        None,
                        None,
                        None,
                    )));

                    let max_visible = items.len().clamp(1, 10) as i64;
                    let t_pref = theme.clone();
                    let t_text = theme.clone();
                    let t_desc = theme.clone();
                    let t_scroll = theme.clone();
                    let t_nomatch = theme.clone();
                    let select_list = Arc::new(SelectList::new(
                        items,
                        max_visible,
                        SelectListTheme {
                            selected_prefix: Arc::new(move |s| t_pref.fg("accent", s)),
                            selected_text: Arc::new(move |s| t_text.fg("accent", s)),
                            description: Arc::new(move |s| t_desc.fg("muted", s)),
                            scroll_info: Arc::new(move |s| t_scroll.fg("dim", s)),
                            no_match: Arc::new(move |s| t_nomatch.fg("warning", s)),
                        },
                        None,
                    ));

                    let done_ok = Arc::clone(&done);
                    select_list.set_on_select(Some(Arc::new(move |item| {
                        done_ok(serde_json::Value::String(item.value.clone()));
                    })));
                    let done_cancel = Arc::clone(&done);
                    select_list.set_on_cancel(Some(Arc::new(move || {
                        done_cancel(serde_json::Value::Null);
                    })));

                    container.add_child(select_list.clone());
                    container.add_child(Arc::new(Text::new(
                        Some(&theme.fg("dim", "↑↓ navigate • enter select • esc cancel")),
                        None,
                        None,
                        None,
                    )));
                    let t2 = theme.clone();
                    container.add_child(Arc::new(FallbackDynamicBorder::new(Arc::new(move |s| {
                        t2.fg("accent", s)
                    }))));

                    Ok(Arc::new(PresetSelectorModal {
                        container,
                        select_list,
                        tui,
                    }) as Arc<dyn Component>)
                })
            }),
            None,
        )
        .await?;

    let Some(result) = val.as_str() else {
        return Ok(());
    };
    if result == "(none)" {
        clear_preset(pi, state, ctx).await?;
        return Ok(());
    }
    if let Some(preset) = presets_snapshot.get(result) {
        apply_preset(pi, state, result, preset, ctx).await?;
        ctx.ui()?
            .notify(&format!("Preset \"{result}\" activated"), Some(NotifyType::Info));
        update_status(ctx, Some(result))?;
    }
    Ok(())
}

async fn cycle_preset(pi: &ExtensionApi, state: &Mutex<SharedState>, ctx: &ExtensionContext) -> pi_js::Result<()> {
    let (presets_snapshot, active_name) = {
        let guard = state.lock().unwrap_or_else(|e| e.into_inner());
        (guard.presets.clone(), guard.active_preset_name.clone())
    };
    if presets_snapshot.is_empty() {
        ctx.ui()?.notify(
            &format!(
                "No presets defined. Add presets to {} or {}",
                pi_js::path::join(&[&get_agent_dir(), "presets.json"]),
                pi_js::path::join(&[&ctx.cwd()?, *CONFIG_DIR_NAME, "presets.json"])
            ),
            Some(NotifyType::Warning),
        );
        return Ok(());
    }

    let mut preset_names: Vec<String> = presets_snapshot.keys().cloned().collect();
    preset_names.sort();
    let mut cycle_list = vec!["(none)".to_string()];
    cycle_list.extend(preset_names);

    let current_name = active_name.as_deref().unwrap_or("(none)");
    let next_idx = match cycle_list.iter().position(|n| n == current_name) {
        Some(idx) => (idx + 1) % cycle_list.len(),
        None => 0,
    };
    let next_name = &cycle_list[next_idx];
    if next_name == "(none)" {
        clear_preset(pi, state, ctx).await?;
        return Ok(());
    }

    if let Some(preset) = presets_snapshot.get(next_name) {
        apply_preset(pi, state, next_name, preset, ctx).await?;
        ctx.ui()?
            .notify(&format!("Preset \"{next_name}\" activated"), Some(NotifyType::Info));
        update_status(ctx, Some(next_name))?;
    }
    Ok(())
}

pub struct PresetExtension;

#[async_trait]
impl Extension for PresetExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let state = Arc::new(Mutex::new(SharedState {
            presets: IndexMap::new(),
            active_preset_name: None,
            active_preset: None,
            original_state: None,
        }));

        pi.register_flag(
            "preset",
            FlagOptions {
                description: Some("Preset configuration to use".to_string()),
                r#type: Some(FlagType::String),
                default: None,
            },
        )?;

        {
            let pi_sc = pi.clone();
            let state_sc = Arc::clone(&state);
            pi.register_shortcut(
                Key::ctrl_shift("u"),
                ShortcutOptions {
                    description: Some("Cycle presets".to_string()),
                    handler: Arc::new(move |ctx| {
                        let pi = pi_sc.clone();
                        let state = Arc::clone(&state_sc);
                        Box::pin(async move { cycle_preset(&pi, &state, &ctx).await })
                    }),
                },
            )?;
        }

        {
            let pi_cmd = pi.clone();
            let state_cmd = Arc::clone(&state);
            pi.register_command(
                "preset",
                RegisteredCommandOptions {
                    description: Some("Switch preset configuration".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |args, ctx| {
                        let pi = pi_cmd.clone();
                        let state = Arc::clone(&state_cmd);
                        Box::pin(async move {
                            let base = ctx.as_context();
                            let trimmed = args.trim();
                            if !trimmed.is_empty() {
                                let (preset_opt, available) = {
                                    let guard = state.lock().unwrap_or_else(|e| e.into_inner());
                                    let avail = if guard.presets.is_empty() {
                                        "(none defined)".to_string()
                                    } else {
                                        guard.presets.keys().cloned().collect::<Vec<_>>().join(", ")
                                    };
                                    (guard.presets.get(trimmed).cloned(), avail)
                                };
                                let Some(preset) = preset_opt else {
                                    base.ui()?.notify(
                                        &format!("Unknown preset \"{trimmed}\". Available: {available}"),
                                        Some(NotifyType::Error),
                                    );
                                    return Ok(());
                                };
                                apply_preset(&pi, &state, trimmed, &preset, base).await?;
                                base.ui()?
                                    .notify(&format!("Preset \"{trimmed}\" activated"), Some(NotifyType::Info));
                                update_status(base, Some(trimmed))?;
                                return Ok(());
                            }
                            show_preset_selector(&pi, &state, base).await
                        })
                    }),
                },
            )?;
        }

        {
            let state_bas = Arc::clone(&state);
            pi.on::<BeforeAgentStartEvent>(move |event, _ctx| {
                let state = Arc::clone(&state_bas);
                Box::pin(async move {
                    let instr = {
                        state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .active_preset
                            .as_ref()
                            .and_then(|p| p.instructions.clone())
                    };
                    if let Some(instructions) = instr {
                        return Ok(Some(BeforeAgentStartEventResult {
                            message: None,
                            system_prompt: Some(format!("{}\n\n{instructions}", event.system_prompt)),
                        }));
                    }
                    Ok(None)
                })
            });
        }

        {
            let pi_ss = pi.clone();
            let state_ss = Arc::clone(&state);
            pi.on::<SessionStartEvent>(move |_event, ctx| {
                let pi = pi_ss.clone();
                let state = Arc::clone(&state_ss);
                Box::pin(async move {
                    let loaded = load_presets(&ctx.cwd()?);
                    {
                        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                        guard.presets = loaded.clone();
                    }

                    let preset_flag = match pi.get_flag("preset")? {
                        Some(FlagValue::String(s)) if !s.is_empty() => Some(s),
                        _ => None,
                    };

                    if let Some(ref flag_name) = preset_flag {
                        if let Some(preset) = loaded.get(flag_name) {
                            apply_preset(&pi, &state, flag_name, preset, &ctx).await?;
                            ctx.ui()?
                                .notify(&format!("Preset \"{flag_name}\" activated"), Some(NotifyType::Info));
                        } else {
                            let available = if loaded.is_empty() {
                                "(none defined)".to_string()
                            } else {
                                loaded.keys().cloned().collect::<Vec<_>>().join(", ")
                            };
                            ctx.ui()?.notify(
                                &format!("Unknown preset \"{flag_name}\". Available: {available}"),
                                Some(NotifyType::Warning),
                            );
                        }
                    }

                    if preset_flag.is_none() {
                        let entries = ctx.session_manager()?.get_entries();
                        if let Some(data_val) = entries.iter().rev().find_map(|e| match e {
                            pi_coding_agent::core::session_manager::SessionEntry::Custom(custom)
                                if custom.custom_type == "preset-state" =>
                            {
                                custom.data.as_ref()
                            }
                            _ => None,
                        }) && let Ok(parsed) = serde_json::from_value::<PresetStateEntry>(data_val.clone())
                            && let Some(preset) = loaded.get(&parsed.name)
                        {
                            let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                            guard.active_preset_name = Some(parsed.name);
                            guard.active_preset = Some(preset.clone());
                        }
                    }

                    let active = {
                        state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .active_preset_name
                            .clone()
                    };
                    update_status(&ctx, active.as_deref())?;
                    Ok(None)
                })
            });
        }

        {
            let pi_ts = pi.clone();
            let state_ts = Arc::clone(&state);
            pi.on::<TurnStartEvent>(move |_event, _ctx| {
                let pi = pi_ts.clone();
                let state = Arc::clone(&state_ts);
                Box::pin(async move {
                    let active = {
                        state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .active_preset_name
                            .clone()
                    };
                    if let Some(name) = active {
                        let val = serde_json::to_value(PresetStateEntry { name })
                            .map_err(|e| pi_js::Error::msg(e.to_string()))?;
                        pi.append_entry("preset-state", Some(val))?;
                    }
                    Ok(None)
                })
            });
        }

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(PresetExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
