//! Port of packages/coding-agent/examples/extensions/tools.ts

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexSet;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionContext, ExtensionFactory, ExtensionMode, InlineExtension, NotifyType,
    RegisteredCommandOptions, SessionStartEvent, SessionTreeEvent, Theme, ToolInfo,
};
use pi_coding_agent::modes::interactive::theme::theme::get_settings_list_theme;
use pi_tui::components::settings_list::{SettingItem, SettingsList};
use pi_tui::{Component, Container};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolsState {
    enabled_tools: Vec<String>,
}

struct HeaderLine {
    theme: Theme,
}

impl Component for HeaderLine {
    fn render(&self, _width: usize) -> Vec<String> {
        vec![
            self.theme.fg("accent", &self.theme.bold("Tool Configuration")),
            String::new(),
        ]
    }

    fn invalidate(&self) {}
}

struct ToolsModal {
    container: Container,
    settings_list: Arc<SettingsList>,
    tui: Arc<dyn pi_tui::TUI>,
}

impl Component for ToolsModal {
    fn render(&self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn handle_input(&self, data: &str) {
        self.settings_list.handle_input(data);
        self.tui.request_render(None);
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn invalidate(&self) {
        self.container.invalidate();
    }

    fn captures_viewport_input(&self) -> bool {
        self.settings_list.captures_viewport_input()
    }
}

struct SharedState {
    enabled_tools: IndexSet<String>,
    all_tools: Vec<ToolInfo>,
}

fn persist_state(pi: &ExtensionApi, state: &Mutex<SharedState>) -> pi_js::Result<()> {
    let enabled: Vec<String> = {
        state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .enabled_tools
            .iter()
            .cloned()
            .collect()
    };
    let val =
        serde_json::to_value(ToolsState { enabled_tools: enabled }).map_err(|e| pi_js::Error::msg(e.to_string()))?;
    pi.append_entry("tools-config", Some(val))
}

fn apply_tools(pi: &ExtensionApi, state: &Mutex<SharedState>) -> pi_js::Result<()> {
    let enabled: Vec<String> = {
        state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .enabled_tools
            .iter()
            .cloned()
            .collect()
    };
    pi.set_active_tools(enabled)
}

fn restore_from_branch(pi: &ExtensionApi, state: &Mutex<SharedState>, ctx: &ExtensionContext) -> pi_js::Result<()> {
    let all_tools = pi.get_all_tools()?;
    let branch_entries = ctx.session_manager()?.get_branch(None);
    let mut saved_tools: Option<Vec<String>> = None;

    for entry in branch_entries {
        if let pi_coding_agent::core::session_manager::SessionEntry::Custom(custom) = entry
            && custom.custom_type == "tools-config"
            && let Some(data_val) = custom.data.as_ref()
            && let Ok(parsed) = serde_json::from_value::<ToolsState>(data_val.clone())
        {
            saved_tools = Some(parsed.enabled_tools);
        }
    }

    if let Some(saved) = saved_tools {
        let all_names: IndexSet<String> = all_tools.iter().map(|t| t.name.clone()).collect();
        let filtered: IndexSet<String> = saved.into_iter().filter(|name| all_names.contains(name)).collect();
        {
            let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
            guard.all_tools = all_tools;
            guard.enabled_tools = filtered;
        }
        apply_tools(pi, state)?;
    } else {
        let active: IndexSet<String> = pi.get_active_tools()?.into_iter().collect();
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        guard.all_tools = all_tools;
        guard.enabled_tools = active;
    }

    Ok(())
}

pub struct ToolsExtension;

#[async_trait]
impl Extension for ToolsExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let state = Arc::new(Mutex::new(SharedState {
            enabled_tools: IndexSet::new(),
            all_tools: Vec::new(),
        }));

        {
            let pi_cmd = pi.clone();
            let state_cmd = Arc::clone(&state);
            pi.register_command(
                "tools",
                RegisteredCommandOptions {
                    description: Some("Enable/disable tools".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |_args, ctx| {
                        let pi = pi_cmd.clone();
                        let state = Arc::clone(&state_cmd);
                        Box::pin(async move {
                            let base = ctx.as_context();
                            if base.mode()? != ExtensionMode::Tui {
                                base.ui()?.notify("/tools requires TUI mode", Some(NotifyType::Error));
                                return Ok(());
                            }

                            let all_tools = pi.get_all_tools()?;
                            let enabled_snapshot = {
                                let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                                guard.all_tools = all_tools.clone();
                                guard.enabled_tools.clone()
                            };

                            let pi_ui = pi.clone();
                            let state_ui = Arc::clone(&state);
                            let _ = base
                                .ui()?
                                .custom(
                                    Box::new(move |tui, theme, _kb, done| {
                                        Box::pin(async move {
                                            let items: Vec<SettingItem> = all_tools
                                                .iter()
                                                .map(|tool| SettingItem {
                                                    id: tool.name.clone(),
                                                    label: tool.name.clone(),
                                                    description: None,
                                                    current_value: if enabled_snapshot.contains(&tool.name) {
                                                        "enabled".to_string()
                                                    } else {
                                                        "disabled".to_string()
                                                    },
                                                    values: Some(vec!["enabled".to_string(), "disabled".to_string()]),
                                                    submenu: None,
                                                })
                                                .collect();

                                            let max_visible = (items.len() + 2).min(15) as i64;
                                            let container = Container::new();
                                            container.add_child(Arc::new(HeaderLine { theme: theme.clone() }));

                                            let pi_cb = pi_ui.clone();
                                            let state_cb = Arc::clone(&state_ui);
                                            let settings_list = Arc::new(SettingsList::new(
                                                items,
                                                max_visible,
                                                get_settings_list_theme(),
                                                Arc::new(move |id, new_value| {
                                                    {
                                                        let mut guard =
                                                            state_cb.lock().unwrap_or_else(|e| e.into_inner());
                                                        if new_value == "enabled" {
                                                            guard.enabled_tools.insert(id.to_string());
                                                        } else {
                                                            guard.enabled_tools.shift_remove(id);
                                                        }
                                                    }
                                                    let _ = apply_tools(&pi_cb, &state_cb);
                                                    let _ = persist_state(&pi_cb, &state_cb);
                                                }),
                                                Arc::new(move || {
                                                    done(serde_json::Value::Null);
                                                }),
                                                None,
                                            ));

                                            container.add_child(settings_list.clone());
                                            Ok(Arc::new(ToolsModal {
                                                container,
                                                settings_list,
                                                tui,
                                            }) as Arc<dyn Component>)
                                        })
                                    }),
                                    None,
                                )
                                .await?;

                            Ok(())
                        })
                    }),
                },
            )?;
        }

        {
            let pi_ss = pi.clone();
            let state_ss = Arc::clone(&state);
            pi.on::<SessionStartEvent>(move |_event, ctx| {
                let pi = pi_ss.clone();
                let state = Arc::clone(&state_ss);
                Box::pin(async move {
                    restore_from_branch(&pi, &state, &ctx)?;
                    Ok(None)
                })
            });
        }

        {
            let pi_st = pi.clone();
            let state_st = Arc::clone(&state);
            pi.on::<SessionTreeEvent>(move |_event, ctx| {
                let pi = pi_st.clone();
                let state = Arc::clone(&state_st);
                Box::pin(async move {
                    restore_from_branch(&pi, &state, &ctx)?;
                    Ok(None)
                })
            });
        }

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(ToolsExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
