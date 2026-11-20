//! Port of packages/coding-agent/examples/extensions/debug-provider.ts

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_agent_core::types::AgentMessage;
use pi_ai::types::Message;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, MessageEndEvent, NotifyType, ProviderStreamEvent,
    RegisteredCommandOptions, TurnEndEvent, TurnStartEvent,
};
use pi_tui::{Box as TuiBox, Component, Text};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const ENTRY_TYPE: &str = "debug-provider-events";
const STATUS_KEY: &str = "debug-provider";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ProviderDebugEntry {
    provider: String,
    api: String,
    model: String,
    events: Vec<Value>,
}

struct DebugState {
    enabled: bool,
    active_events: Option<Vec<Value>>,
    completed_entry: Option<ProviderDebugEntry>,
}

pub struct DebugProviderExtension;

#[async_trait]
impl Extension for DebugProviderExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let state = Arc::new(Mutex::new(DebugState {
            enabled: false,
            active_events: None,
            completed_entry: None,
        }));

        pi.register_entry_renderer(
            ENTRY_TYPE,
            Arc::new(|entry, opts, theme| {
                let data = entry
                    .data
                    .and_then(|v| serde_json::from_value::<ProviderDebugEntry>(v).ok());
                let Some(data) = data else {
                    let comp: Arc<dyn Component> = Arc::new(Text::new(
                        Some(&theme.fg("warning", "[provider debug] Missing event data")),
                        Some(0),
                        Some(0),
                        None,
                    ));
                    return Some(comp);
                };

                let theme_bg = theme.clone();
                let tui_box = TuiBox::new(
                    Some(1),
                    Some(1),
                    Some(Arc::new(move |text| theme_bg.bg("customMessageBg", text))),
                );
                let count_word = if data.events.len() == 1 { "event" } else { "events" };
                let count = format!("{} {count_word}", data.events.len());
                let expand_hint = if opts.expanded {
                    String::new()
                } else {
                    format!(" ({})", opts.key_hint("app.tools.expand", "to view events"))
                };
                tui_box.add_child(Arc::new(Text::new(
                    Some(&format!(
                        "{} {}/{} ({}) · {count}{expand_hint}",
                        theme.fg("accent", "[provider debug]"),
                        data.provider,
                        data.model,
                        data.api
                    )),
                    Some(0),
                    Some(0),
                    None,
                )));
                if opts.expanded {
                    let pretty = pi_js::json::stringify_pretty(&data.events, "  ");
                    tui_box.add_child(Arc::new(Text::new(Some(&pretty), Some(0), Some(0), None)));
                }
                let comp: Arc<dyn Component> = Arc::new(tui_box);
                Some(comp)
            }),
        );

        {
            let state_cmd = Arc::clone(&state);
            pi.register_command(
                "debug-provider",
                RegisteredCommandOptions {
                    description: Some("Toggle capture of raw provider stream events".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |args, ctx| {
                        let state = Arc::clone(&state_cmd);
                        Box::pin(async move {
                            let requested = args.trim().to_lowercase();
                            let ui = ctx.as_context().ui()?;
                            if !requested.is_empty() && requested != "on" && requested != "off" {
                                ui.notify("Usage: /debug-provider [on|off]", Some(NotifyType::Warning));
                                return Ok(());
                            }

                            let now_enabled = {
                                let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                                guard.enabled = if requested.is_empty() {
                                    !guard.enabled
                                } else {
                                    requested == "on"
                                };
                                if !guard.enabled {
                                    guard.active_events = None;
                                    guard.completed_entry = None;
                                }
                                guard.enabled
                            };

                            ui.set_status(STATUS_KEY, if now_enabled { Some("provider debug") } else { None });
                            ui.notify(
                                &format!(
                                    "Provider event capture {}",
                                    if now_enabled { "enabled" } else { "disabled" }
                                ),
                                Some(NotifyType::Info),
                            );
                            Ok(())
                        })
                    }),
                },
            )?;
        }

        {
            let state_ts = Arc::clone(&state);
            pi.on::<TurnStartEvent>(move |_event, _ctx| {
                let state = Arc::clone(&state_ts);
                Box::pin(async move {
                    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                    guard.active_events = if guard.enabled { Some(Vec::new()) } else { None };
                    Ok(None)
                })
            });
        }

        {
            let state_ps = Arc::clone(&state);
            pi.on::<ProviderStreamEvent>(move |event, _ctx| {
                let state = Arc::clone(&state_ps);
                Box::pin(async move {
                    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(ref mut events) = guard.active_events {
                        events.push(event.data);
                    }
                    Ok(None)
                })
            });
        }

        {
            let state_me = Arc::clone(&state);
            pi.on::<MessageEndEvent>(move |event, _ctx| {
                let state = Arc::clone(&state_me);
                Box::pin(async move {
                    let AgentMessage::Message(Message::Assistant(assistant)) = event.message else {
                        return Ok(None);
                    };
                    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(events) = guard.active_events.take() {
                        guard.completed_entry = Some(ProviderDebugEntry {
                            provider: assistant.provider,
                            api: assistant.api,
                            model: assistant.model,
                            events,
                        });
                    }
                    Ok(None)
                })
            });
        }

        {
            let pi_te = pi.clone();
            let state_te = Arc::clone(&state);
            pi.on::<TurnEndEvent>(move |_event, _ctx| {
                let pi = pi_te.clone();
                let state = Arc::clone(&state_te);
                Box::pin(async move {
                    let entry = { state.lock().unwrap_or_else(|e| e.into_inner()).completed_entry.take() };
                    if let Some(entry) = entry {
                        let val = serde_json::to_value(entry).map_err(|e| pi_js::Error::msg(e.to_string()))?;
                        pi.append_entry(ENTRY_TYPE, Some(val))?;
                    }
                    Ok(None)
                })
            });
        }

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(DebugProviderExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
