//! Port of packages/coding-agent/examples/extensions/plan-mode/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod utils;
// @generated-mods end

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexSet;
use pi_agent_core::types::AgentMessage;
use pi_ai::types::{AssistantContentBlock, AssistantMessage, Message, UserContentBlock, UserMessageContent};
use pi_coding_agent::core::extensions::types::{
    AgentEndEvent, BeforeAgentStartEvent, BeforeAgentStartEventResult, ContextEvent, ContextEventResult,
    CustomMessageContent, CustomMessageDraft, DeliverAs, Extension, ExtensionApi, ExtensionContext, ExtensionFactory,
    FlagOptions, FlagType, FlagValue, InlineExtension, InputDeliveryBehavior, NotifyType, RegisteredCommandOptions,
    SendMessageOptions, SendUserMessageOptions, SessionStartEvent, ShortcutOptions, ToolCallEvent, ToolCallEventResult,
    TurnEndEvent,
};
use pi_tui::Key;
use serde::{Deserialize, Serialize};

use self::utils::{TodoItem, extract_todo_items, is_safe_command, mark_completed_steps};

const PLAN_MODE_TOOLS: &[&str] = &["read", "bash", "grep", "find", "ls", "questionnaire"];
const NORMAL_MODE_TOOLS: &[&str] = &["read", "bash", "edit", "write"];
const PLAN_MODE_DISABLED_TOOLS: &[&str] = &["edit", "write"];
const PLAN_MANAGED_TOOLS: &[&str] = &["read", "bash", "grep", "find", "ls", "questionnaire", "edit", "write"];

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanModeState {
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    todos: Option<Vec<TodoItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    executing: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools_before_plan_mode: Option<Vec<String>>,
}

struct SharedState {
    plan_mode_enabled: bool,
    execution_mode: bool,
    todo_items: Vec<TodoItem>,
    tools_before_plan_mode: Option<Vec<String>>,
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            plan_mode_enabled: false,
            execution_mode: false,
            todo_items: Vec::new(),
            tools_before_plan_mode: None,
        }
    }
}

fn assistant_message_from_agent(m: &AgentMessage) -> Option<&AssistantMessage> {
    match m {
        AgentMessage::Message(Message::Assistant(msg)) => Some(msg),
        _ => None,
    }
}

fn get_assistant_text(message: &AssistantMessage) -> String {
    let parts: Vec<&str> = message
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    parts.join("\n")
}

fn unique_tool_names(tool_names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = IndexSet::new();
    let mut out = Vec::new();
    for name in tool_names {
        if seen.insert(name.clone()) {
            out.push(name);
        }
    }
    out
}

fn get_plan_mode_tools(active_tool_names: &[String]) -> Vec<String> {
    let filtered = active_tool_names
        .iter()
        .filter(|name| !PLAN_MODE_DISABLED_TOOLS.contains(&name.as_str()))
        .cloned();
    unique_tool_names(filtered.chain(PLAN_MODE_TOOLS.iter().map(|s| (*s).to_string())))
}

fn get_normal_mode_tools(active_tool_names: &[String]) -> Vec<String> {
    let extras = active_tool_names
        .iter()
        .filter(|name| !PLAN_MANAGED_TOOLS.contains(&name.as_str()))
        .cloned();
    unique_tool_names(NORMAL_MODE_TOOLS.iter().map(|s| (*s).to_string()).chain(extras))
}

fn update_status(ctx: &ExtensionContext, state: &Mutex<SharedState>) -> pi_js::Result<()> {
    let (plan_enabled, exec_mode, items) = {
        let guard = state.lock().unwrap_or_else(|e| e.into_inner());
        (guard.plan_mode_enabled, guard.execution_mode, guard.todo_items.clone())
    };
    let ui = ctx.ui()?;
    let theme = ui.theme();

    if exec_mode && !items.is_empty() {
        let completed = items.iter().filter(|t| t.completed).count();
        ui.set_status(
            "plan-mode",
            Some(&theme.fg("accent", &format!("📋 {completed}/{}", items.len()))),
        );
    } else if plan_enabled {
        ui.set_status("plan-mode", Some(&theme.fg("warning", "⏸ plan")));
    } else {
        ui.set_status("plan-mode", None);
    }

    if exec_mode && !items.is_empty() {
        let lines: Vec<String> = items
            .iter()
            .map(|item| {
                if item.completed {
                    format!(
                        "{}{}",
                        theme.fg("success", "☑ "),
                        theme.fg("muted", &theme.strikethrough(&item.text))
                    )
                } else {
                    format!("{}{}", theme.fg("muted", "☐ "), item.text)
                }
            })
            .collect();
        ui.set_widget_lines("plan-todos", Some(lines), None);
    } else {
        ui.set_widget_lines("plan-todos", None, None);
    }
    Ok(())
}

fn enable_plan_mode_tools(pi: &ExtensionApi, state: &Mutex<SharedState>) -> pi_js::Result<()> {
    let existing_before = {
        state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tools_before_plan_mode
            .clone()
    };
    let base = match existing_before {
        Some(tools) => tools,
        None => {
            let active = pi.get_active_tools()?;
            let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
            if guard.tools_before_plan_mode.is_none() {
                guard.tools_before_plan_mode = Some(active.clone());
            }
            guard.tools_before_plan_mode.clone().unwrap_or(active)
        }
    };
    pi.set_active_tools(get_plan_mode_tools(&base))
}

fn restore_normal_mode_tools(pi: &ExtensionApi, state: &Mutex<SharedState>) -> pi_js::Result<()> {
    let saved = {
        state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tools_before_plan_mode
            .take()
    };
    let target = match saved {
        Some(tools) => tools,
        None => get_normal_mode_tools(&pi.get_active_tools()?),
    };
    pi.set_active_tools(target)
}

fn persist_state(pi: &ExtensionApi, state: &Mutex<SharedState>) -> pi_js::Result<()> {
    let snapshot = {
        let guard = state.lock().unwrap_or_else(|e| e.into_inner());
        PlanModeState {
            enabled: guard.plan_mode_enabled,
            todos: Some(guard.todo_items.clone()),
            executing: Some(guard.execution_mode),
            tools_before_plan_mode: guard.tools_before_plan_mode.clone(),
        }
    };
    let val = serde_json::to_value(snapshot).map_err(|e| pi_js::Error::msg(e.to_string()))?;
    pi.append_entry("plan-mode", Some(val))
}

fn toggle_plan_mode(pi: &ExtensionApi, state: &Mutex<SharedState>, ctx: &ExtensionContext) -> pi_js::Result<()> {
    let now_enabled = {
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        guard.plan_mode_enabled = !guard.plan_mode_enabled;
        guard.execution_mode = false;
        guard.todo_items.clear();
        guard.plan_mode_enabled
    };

    if now_enabled {
        enable_plan_mode_tools(pi, state)?;
        ctx.ui()?.notify(
            "Plan mode enabled. Built-in write tools disabled.",
            Some(NotifyType::Info),
        );
    } else {
        restore_normal_mode_tools(pi, state)?;
        ctx.ui()?
            .notify("Plan mode disabled. Full access restored.", Some(NotifyType::Info));
    }
    update_status(ctx, state)?;
    persist_state(pi, state)
}

pub struct PlanModeExtension;

#[async_trait]
impl Extension for PlanModeExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let state = Arc::new(Mutex::new(SharedState::default()));

        pi.register_flag(
            "plan",
            FlagOptions {
                description: Some("Start in plan mode (read-only exploration)".to_string()),
                r#type: Some(FlagType::Boolean),
                default: Some(FlagValue::Bool(false)),
            },
        )?;

        {
            let pi_cmd = pi.clone();
            let state_cmd = Arc::clone(&state);
            pi.register_command(
                "plan",
                RegisteredCommandOptions {
                    description: Some("Toggle plan mode (read-only exploration)".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |_args, ctx| {
                        let pi = pi_cmd.clone();
                        let state = Arc::clone(&state_cmd);
                        Box::pin(async move { toggle_plan_mode(&pi, &state, ctx.as_context()) })
                    }),
                },
            )?;
        }

        {
            let state_todos = Arc::clone(&state);
            pi.register_command(
                "todos",
                RegisteredCommandOptions {
                    description: Some("Show current plan todo list".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |_args, ctx| {
                        let state = Arc::clone(&state_todos);
                        Box::pin(async move {
                            let items = { state.lock().unwrap_or_else(|e| e.into_inner()).todo_items.clone() };
                            let ui = ctx.as_context().ui()?;
                            if items.is_empty() {
                                ui.notify("No todos. Create a plan first with /plan", Some(NotifyType::Info));
                                return Ok(());
                            }
                            let list: Vec<String> = items
                                .iter()
                                .enumerate()
                                .map(|(i, item)| {
                                    format!("{}. {} {}", i + 1, if item.completed { "✓" } else { "○" }, item.text)
                                })
                                .collect();
                            ui.notify(&format!("Plan Progress:\n{}", list.join("\n")), Some(NotifyType::Info));
                            Ok(())
                        })
                    }),
                },
            )?;
        }

        {
            let pi_sc = pi.clone();
            let state_sc = Arc::clone(&state);
            pi.register_shortcut(
                Key::ctrl_alt("p"),
                ShortcutOptions {
                    description: Some("Toggle plan mode".to_string()),
                    handler: Arc::new(move |ctx| {
                        let pi = pi_sc.clone();
                        let state = Arc::clone(&state_sc);
                        Box::pin(async move { toggle_plan_mode(&pi, &state, &ctx) })
                    }),
                },
            )?;
        }

        {
            let state_tc = Arc::clone(&state);
            pi.on::<ToolCallEvent>(move |event, _ctx| {
                let state = Arc::clone(&state_tc);
                Box::pin(async move {
                    let enabled = state.lock().unwrap_or_else(|e| e.into_inner()).plan_mode_enabled;
                    if !enabled {
                        return Ok(None);
                    }
                    if let ToolCallEvent::Bash(bash_evt) = event
                        && !is_safe_command(&bash_evt.input.command)
                    {
                        return Ok(Some(ToolCallEventResult {
                            block: Some(true),
                            reason: Some(format!(
                                "Plan mode: command blocked (not allowlisted). Use /plan to disable plan mode first.\nCommand: {}",
                                bash_evt.input.command
                            )),
                            terminate: None,
                        }));
                    }
                    Ok(None)
                })
            });
        }

        {
            let state_ctx = Arc::clone(&state);
            pi.on::<ContextEvent>(move |event, _ctx| {
                let state = Arc::clone(&state_ctx);
                Box::pin(async move {
                    let enabled = state.lock().unwrap_or_else(|e| e.into_inner()).plan_mode_enabled;
                    if enabled {
                        return Ok(None);
                    }
                    let messages = event
                        .messages
                        .into_iter()
                        .filter(|m| match m {
                            AgentMessage::Custom(val) => {
                                val.get("customType").and_then(|v| v.as_str()) != Some("plan-mode-context")
                            }
                            AgentMessage::Message(Message::User(user)) => match &user.content {
                                UserMessageContent::Text(t) => !t.contains("[PLAN MODE ACTIVE]"),
                                UserMessageContent::Blocks(blocks) => !blocks.iter().any(|b| match b {
                                    UserContentBlock::Text(t) => t.text.contains("[PLAN MODE ACTIVE]"),
                                    _ => false,
                                }),
                            },
                            _ => true,
                        })
                        .collect();
                    Ok(Some(ContextEventResult {
                        messages: Some(messages),
                    }))
                })
            });
        }

        {
            let state_bas = Arc::clone(&state);
            pi.on::<BeforeAgentStartEvent>(move |_event, _ctx| {
                let state = Arc::clone(&state_bas);
                Box::pin(async move {
                    let (plan_enabled, exec_mode, items) = {
                        let guard = state.lock().unwrap_or_else(|e| e.into_inner());
                        (guard.plan_mode_enabled, guard.execution_mode, guard.todo_items.clone())
                    };
                    if plan_enabled {
                        return Ok(Some(BeforeAgentStartEventResult {
                            message: Some(CustomMessageDraft {
                                custom_type: "plan-mode-context".to_string(),
                                content: CustomMessageContent::Text(
                                    "[PLAN MODE ACTIVE]\n\
                                     You are in plan mode - a read-only exploration mode for safe code analysis.\n\n\
                                     Restrictions:\n\
                                     - Built-in edit and write tools are disabled\n\
                                     - Other currently active tools remain available\n\
                                     - Bash is restricted to an allowlist of read-only commands\n\n\
                                     Ask clarifying questions using the questionnaire tool.\n\
                                     Use brave-search skill via bash for web research.\n\n\
                                     Create a detailed numbered plan under a \"Plan:\" header:\n\n\
                                     Plan:\n\
                                     1. First step description\n\
                                     2. Second step description\n\
                                     ...\n\n\
                                     Do NOT attempt to make changes - just describe what you would do."
                                        .to_string(),
                                ),
                                display: false,
                                details: None,
                            }),
                            system_prompt: None,
                        }));
                    }

                    if exec_mode && !items.is_empty() {
                        let todo_list: Vec<String> = items
                            .iter()
                            .filter(|t| !t.completed)
                            .map(|t| format!("{}. {}", t.step, t.text))
                            .collect();
                        return Ok(Some(BeforeAgentStartEventResult {
                            message: Some(CustomMessageDraft {
                                custom_type: "plan-execution-context".to_string(),
                                content: CustomMessageContent::Text(format!(
                                    "[EXECUTING PLAN - Full tool access enabled]\n\n\
                                     Remaining steps:\n\
                                     {}\n\n\
                                     Execute each step in order.\n\
                                     After completing a step, include a [DONE:n] tag in your response.",
                                    todo_list.join("\n")
                                )),
                                display: false,
                                details: None,
                            }),
                            system_prompt: None,
                        }));
                    }
                    Ok(None)
                })
            });
        }

        {
            let pi_te = pi.clone();
            let state_te = Arc::clone(&state);
            pi.on::<TurnEndEvent>(move |event, ctx| {
                let pi = pi_te.clone();
                let state = Arc::clone(&state_te);
                Box::pin(async move {
                    let (should_check, mut items) = {
                        let guard = state.lock().unwrap_or_else(|e| e.into_inner());
                        (
                            guard.execution_mode && !guard.todo_items.is_empty(),
                            guard.todo_items.clone(),
                        )
                    };
                    if !should_check {
                        return Ok(None);
                    }
                    let Some(msg) = assistant_message_from_agent(&event.message) else {
                        return Ok(None);
                    };
                    let text = get_assistant_text(msg);
                    let updated = mark_completed_steps(&text, &mut items) > 0;
                    if updated {
                        {
                            let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                            guard.todo_items = items;
                        }
                        update_status(&ctx, &state)?;
                    }
                    persist_state(&pi, &state)?;
                    Ok(None)
                })
            });
        }

        {
            let pi_ae = pi.clone();
            let state_ae = Arc::clone(&state);
            pi.on::<AgentEndEvent>(move |event, ctx| {
                let pi = pi_ae.clone();
                let state = Arc::clone(&state_ae);
                Box::pin(async move {
                    let (exec_mode, plan_enabled, mut items) = {
                        let guard = state.lock().unwrap_or_else(|e| e.into_inner());
                        (guard.execution_mode, guard.plan_mode_enabled, guard.todo_items.clone())
                    };

                    if exec_mode && !items.is_empty() {
                        if items.iter().all(|t| t.completed) {
                            let completed_list: Vec<String> =
                                items.iter().map(|t| format!("~~{}~~", t.text)).collect();
                            pi.send_message(
                                CustomMessageDraft {
                                    custom_type: "plan-complete".to_string(),
                                    content: CustomMessageContent::Text(format!(
                                        "**Plan Complete!** ✓\n\n{}",
                                        completed_list.join("\n")
                                    )),
                                    display: true,
                                    details: None,
                                },
                                Some(SendMessageOptions {
                                    trigger_turn: Some(false),
                                    deliver_as: None,
                                }),
                            )?;
                            {
                                let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                                guard.execution_mode = false;
                                guard.todo_items.clear();
                            }
                            update_status(&ctx, &state)?;
                            persist_state(&pi, &state)?;
                        }
                        return Ok(None);
                    }

                    if !plan_enabled || !ctx.has_ui()? {
                        return Ok(None);
                    }

                    if let Some(last_assistant) = event.messages.iter().rev().find_map(assistant_message_from_agent) {
                        let extracted = extract_todo_items(&get_assistant_text(last_assistant));
                        if !extracted.is_empty() {
                            items = extracted;
                            let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                            guard.todo_items = items.clone();
                        }
                    }

                    if items.is_empty() {
                        return Ok(None);
                    }
                    persist_state(&pi, &state)?;

                    let todo_list_text: Vec<String> = items
                        .iter()
                        .enumerate()
                        .map(|(i, t)| format!("{}. ☐ {}", i + 1, t.text))
                        .collect();
                    let plan_todo_list_message = CustomMessageDraft {
                        custom_type: "plan-todo-list".to_string(),
                        content: CustomMessageContent::Text(format!(
                            "**Plan Steps ({}):**\n\n{}",
                            items.len(),
                            todo_list_text.join("\n")
                        )),
                        display: true,
                        details: None,
                    };

                    let ui = ctx.ui()?;
                    let choice = ui
                        .select(
                            "Plan mode - what next?",
                            vec![
                                "Execute the plan (track progress)".to_string(),
                                "Stay in plan mode".to_string(),
                                "Refine the plan".to_string(),
                            ],
                            None,
                        )
                        .await?;

                    if let Some(c) = choice.as_deref()
                        && c.starts_with("Execute")
                    {
                        let Some(first_item) = items.first().cloned() else {
                            return Ok(None);
                        };
                        {
                            let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                            guard.plan_mode_enabled = false;
                            guard.execution_mode = true;
                        }
                        restore_normal_mode_tools(&pi, &state)?;
                        update_status(&ctx, &state)?;
                        persist_state(&pi, &state)?;

                        let remaining_list: Vec<String> =
                            items.iter().map(|t| format!("{}. {}", t.step, t.text)).collect();
                        let exec_message = format!(
                            "Execute the plan.\n\nRemaining steps:\n{}\n\nStart with: {}\nAfter completing a step, include a [DONE:n] tag in your response.",
                            remaining_list.join("\n"),
                            first_item.text
                        );
                        pi.send_message(
                            plan_todo_list_message,
                            Some(SendMessageOptions {
                                trigger_turn: None,
                                deliver_as: Some(DeliverAs::FollowUp),
                            }),
                        )?;
                        pi.send_message(
                            CustomMessageDraft {
                                custom_type: "plan-mode-execute".to_string(),
                                content: CustomMessageContent::Text(exec_message),
                                display: true,
                                details: None,
                            },
                            Some(SendMessageOptions {
                                trigger_turn: Some(true),
                                deliver_as: Some(DeliverAs::FollowUp),
                            }),
                        )?;
                    } else if choice.as_deref() == Some("Refine the plan") {
                        let refinement = ui.editor("Refine the plan:", None).await?;
                        if let Some(text) = refinement
                            && !text.trim().is_empty()
                        {
                            pi.send_message(
                                plan_todo_list_message,
                                Some(SendMessageOptions {
                                    trigger_turn: None,
                                    deliver_as: Some(DeliverAs::FollowUp),
                                }),
                            )?;
                            pi.send_user_message(
                                CustomMessageContent::Text(text.trim().to_string()),
                                Some(SendUserMessageOptions {
                                    deliver_as: Some(InputDeliveryBehavior::FollowUp),
                                    expand_prompt_templates: None,
                                }),
                            )?;
                        }
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
                    if pi.get_flag("plan")? == Some(FlagValue::Bool(true)) {
                        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                        guard.plan_mode_enabled = true;
                    }

                    let entries = ctx.session_manager()?.get_entries();
                    let plan_mode_entry = entries.iter().rev().find_map(|e| match e {
                        pi_coding_agent::core::session_manager::SessionEntry::Custom(custom)
                            if custom.custom_type == "plan-mode" =>
                        {
                            Some(custom)
                        }
                        _ => None,
                    });

                    if let Some(entry) = plan_mode_entry
                        && let Some(data_val) = entry.data.as_ref()
                        && let Ok(parsed) = serde_json::from_value::<PlanModeState>(data_val.clone())
                    {
                        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                        guard.plan_mode_enabled = parsed.enabled;
                        if let Some(todos) = parsed.todos {
                            guard.todo_items = todos;
                        }
                        if let Some(executing) = parsed.executing {
                            guard.execution_mode = executing;
                        }
                        if let Some(tools) = parsed.tools_before_plan_mode {
                            guard.tools_before_plan_mode = Some(tools);
                        }
                    }

                    let is_resume = plan_mode_entry.is_some();
                    let should_scan = {
                        let guard = state.lock().unwrap_or_else(|e| e.into_inner());
                        is_resume && guard.execution_mode && !guard.todo_items.is_empty()
                    };

                    if should_scan {
                        let execute_index = entries
                            .iter()
                            .rposition(|e| {
                                matches!(
                                    e,
                                    pi_coding_agent::core::session_manager::SessionEntry::Custom(custom)
                                        if custom.custom_type == "plan-mode-execute"
                                )
                            })
                            .map(|idx| idx + 1)
                            .unwrap_or(0);

                        let mut texts = Vec::new();
                        for entry in &entries[execute_index..] {
                            if let pi_coding_agent::core::session_manager::SessionEntry::Message(msg_entry) = entry
                                && let Some(assistant) = assistant_message_from_agent(&msg_entry.message)
                            {
                                texts.push(get_assistant_text(assistant));
                            }
                        }
                        let all_text = texts.join("\n");
                        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                        mark_completed_steps(&all_text, &mut guard.todo_items);
                    }

                    let enabled = state.lock().unwrap_or_else(|e| e.into_inner()).plan_mode_enabled;
                    if enabled {
                        enable_plan_mode_tools(&pi, &state)?;
                    }
                    update_status(&ctx, &state)?;
                    Ok(None)
                })
            });
        }

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(PlanModeExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
