//! Port of packages/coding-agent/examples/extensions/todo.ts

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_agent_core::types::AgentToolResult;
use pi_ai::string_enum;
use pi_ai::types::{TextContent, UserContentBlock};
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionContext, ExtensionFactory, ExtensionMode, InlineExtension, NotifyType,
    RegisteredCommandOptions, SessionStartEvent, SessionTreeEvent, Theme, ToolDefinition,
};
use pi_schema::t;
use pi_tui::{Component, Text, matches_key, truncate_to_width};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Todo {
    id: u64,
    text: String,
    done: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum TodoAction {
    List,
    Add,
    Toggle,
    Clear,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TodoDetails {
    action: TodoAction,
    todos: Vec<Todo>,
    next_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct TodoParams {
    action: String,
    text: Option<String>,
    id: Option<u64>,
}

struct SharedTodos {
    todos: Vec<Todo>,
    next_id: u64,
}

struct TodoListCache {
    cached_width: Option<usize>,
    cached_lines: Option<Vec<String>>,
}

struct TodoListComponent {
    todos: Vec<Todo>,
    theme: Theme,
    on_close: Arc<dyn Fn() + Send + Sync>,
    cache: Mutex<TodoListCache>,
}

impl TodoListComponent {
    fn new(todos: Vec<Todo>, theme: Theme, on_close: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            todos,
            theme,
            on_close,
            cache: Mutex::new(TodoListCache {
                cached_width: None,
                cached_lines: None,
            }),
        }
    }
}

impl Component for TodoListComponent {
    fn render(&self, width: usize) -> Vec<String> {
        {
            let guard = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if guard.cached_width == Some(width)
                && let Some(ref lines) = guard.cached_lines
            {
                return lines.clone();
            }
        }

        let th = &self.theme;
        let w = width as i64;
        let mut lines = Vec::new();

        lines.push(String::new());
        let title = th.fg("accent", " Todos ");
        let header_line = format!(
            "{}{title}{}",
            th.fg("borderMuted", &"─".repeat(3)),
            th.fg("borderMuted", &"─".repeat(width.saturating_sub(10)))
        );
        lines.push(truncate_to_width(&header_line, w, None, None));
        lines.push(String::new());

        if self.todos.is_empty() {
            lines.push(truncate_to_width(
                &format!("  {}", th.fg("dim", "No todos yet. Ask the agent to add some!")),
                w,
                None,
                None,
            ));
        } else {
            let done = self.todos.iter().filter(|t| t.done).count();
            let total = self.todos.len();
            lines.push(truncate_to_width(
                &format!("  {}", th.fg("muted", &format!("{done}/{total} completed"))),
                w,
                None,
                None,
            ));
            lines.push(String::new());

            for todo in &self.todos {
                let check = if todo.done {
                    th.fg("success", "✓")
                } else {
                    th.fg("dim", "○")
                };
                let id = th.fg("accent", &format!("#{}", todo.id));
                let text = if todo.done {
                    th.fg("dim", &todo.text)
                } else {
                    th.fg("text", &todo.text)
                };
                lines.push(truncate_to_width(&format!("  {check} {id} {text}"), w, None, None));
            }
        }

        lines.push(String::new());
        lines.push(truncate_to_width(
            &format!("  {}", th.fg("dim", "Press Escape to close")),
            w,
            None,
            None,
        ));
        lines.push(String::new());

        let mut guard = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        guard.cached_width = Some(width);
        guard.cached_lines = Some(lines.clone());
        lines
    }

    fn handle_input(&self, data: &str) {
        if matches_key(data, "escape") || matches_key(data, "ctrl+c") {
            (self.on_close)();
        }
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn invalidate(&self) {
        let mut guard = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        guard.cached_width = None;
        guard.cached_lines = None;
    }
}

fn reconstruct_state(state: &Mutex<SharedTodos>, ctx: &ExtensionContext) -> pi_js::Result<()> {
    let mut todos = Vec::new();
    let mut next_id = 1u64;

    for entry in ctx.session_manager()?.get_branch(None) {
        let pi_coding_agent::core::session_manager::SessionEntry::Message(msg_entry) = entry else {
            continue;
        };
        let pi_agent_core::types::AgentMessage::Message(pi_ai::types::Message::ToolResult(tr)) = msg_entry.message
        else {
            continue;
        };
        if tr.tool_name != "todo" {
            continue;
        }
        if let Some(details_val) = tr.details
            && let Ok(details) = serde_json::from_value::<TodoDetails>(details_val)
        {
            todos = details.todos;
            next_id = details.next_id;
        }
    }

    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
    guard.todos = todos;
    guard.next_id = next_id;
    Ok(())
}

pub struct TodoExtension;

#[async_trait]
impl Extension for TodoExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let state = Arc::new(Mutex::new(SharedTodos {
            todos: Vec::new(),
            next_id: 1,
        }));

        {
            let state_ss = Arc::clone(&state);
            pi.on::<SessionStartEvent>(move |_event, ctx| {
                let state = Arc::clone(&state_ss);
                Box::pin(async move {
                    reconstruct_state(&state, &ctx)?;
                    Ok(None)
                })
            });
        }

        {
            let state_st = Arc::clone(&state);
            pi.on::<SessionTreeEvent>(move |_event, ctx| {
                let state = Arc::clone(&state_st);
                Box::pin(async move {
                    reconstruct_state(&state, &ctx)?;
                    Ok(None)
                })
            });
        }

        let params_schema = t::object([
            ("action", string_enum(["list", "add", "toggle", "clear"], None)),
            (
                "text",
                t::string().with("description", "Todo text (for add)").optional(),
            ),
            ("id", t::number().with("description", "Todo ID (for toggle)").optional()),
        ]);

        {
            let state_tool = Arc::clone(&state);
            pi.register_tool(ToolDefinition {
                name: "todo".to_string(),
                label: "Todo".to_string(),
                description: "Manage a todo list. Actions: list, add (text), toggle (id), clear".to_string(),
                prompt_snippet: None,
                prompt_guidelines: None,
                parameters: params_schema.into(),
                constrained_sampling: None,
                render_shell: None,
                prepare_arguments: None,
                output_schema: None,
                exposure: None,
                namespace: None,
                annotations: None,
                default_active: None,
                prepare_loadout: None,
                execution_mode: None,
                execute: Arc::new(move |_tool_call_id, raw_params, _signal, _on_update, _ctx| {
                    let state = Arc::clone(&state_tool);
                    Box::pin(async move {
                        let params: TodoParams = serde_json::from_value(raw_params)
                            .map_err(|e| pi_js::Error::msg(format!("Invalid parameters: {e}")))?;

                        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                        let (text, details) = match params.action.as_str() {
                            "list" => {
                                let text = if guard.todos.is_empty() {
                                    "No todos".to_string()
                                } else {
                                    guard
                                        .todos
                                        .iter()
                                        .map(|t| format!("[{}] #{}: {}", if t.done { "x" } else { " " }, t.id, t.text))
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                };
                                (
                                    text,
                                    TodoDetails {
                                        action: TodoAction::List,
                                        todos: guard.todos.clone(),
                                        next_id: guard.next_id,
                                        error: None,
                                    },
                                )
                            }
                            "add" => match params.text.filter(|s| !s.is_empty()) {
                                None => (
                                    "Error: text required for add".to_string(),
                                    TodoDetails {
                                        action: TodoAction::Add,
                                        todos: guard.todos.clone(),
                                        next_id: guard.next_id,
                                        error: Some("text required".to_string()),
                                    },
                                ),
                                Some(t_str) => {
                                    let id = guard.next_id;
                                    guard.next_id += 1;
                                    let new_todo = Todo {
                                        id,
                                        text: t_str.clone(),
                                        done: false,
                                    };
                                    guard.todos.push(new_todo);
                                    (
                                        format!("Added todo #{id}: {t_str}"),
                                        TodoDetails {
                                            action: TodoAction::Add,
                                            todos: guard.todos.clone(),
                                            next_id: guard.next_id,
                                            error: None,
                                        },
                                    )
                                }
                            },
                            "toggle" => match params.id {
                                None => (
                                    "Error: id required for toggle".to_string(),
                                    TodoDetails {
                                        action: TodoAction::Toggle,
                                        todos: guard.todos.clone(),
                                        next_id: guard.next_id,
                                        error: Some("id required".to_string()),
                                    },
                                ),
                                Some(target_id) => {
                                    if let Some(todo) = guard.todos.iter_mut().find(|t| t.id == target_id) {
                                        todo.done = !todo.done;
                                        let id = todo.id;
                                        let done = todo.done;
                                        (
                                            format!("Todo #{id} {}", if done { "completed" } else { "uncompleted" }),
                                            TodoDetails {
                                                action: TodoAction::Toggle,
                                                todos: guard.todos.clone(),
                                                next_id: guard.next_id,
                                                error: None,
                                            },
                                        )
                                    } else {
                                        (
                                            format!("Todo #{target_id} not found"),
                                            TodoDetails {
                                                action: TodoAction::Toggle,
                                                todos: guard.todos.clone(),
                                                next_id: guard.next_id,
                                                error: Some(format!("#{target_id} not found")),
                                            },
                                        )
                                    }
                                }
                            },
                            "clear" => {
                                let count = guard.todos.len();
                                guard.todos.clear();
                                guard.next_id = 1;
                                (
                                    format!("Cleared {count} todos"),
                                    TodoDetails {
                                        action: TodoAction::Clear,
                                        todos: Vec::new(),
                                        next_id: 1,
                                        error: None,
                                    },
                                )
                            }
                            other => (
                                format!("Unknown action: {other}"),
                                TodoDetails {
                                    action: TodoAction::List,
                                    todos: guard.todos.clone(),
                                    next_id: guard.next_id,
                                    error: Some(format!("unknown action: {other}")),
                                },
                            ),
                        };

                        Ok(AgentToolResult {
                            content: vec![UserContentBlock::Text(TextContent::new(text))],
                            details: serde_json::to_value(details).ok(),
                            structured_content: None,
                            usage: None,
                            is_error: None,
                            terminate: None,
                            control: None,
                        })
                    })
                }),
                render_call: Some(Arc::new(|args, theme, _context| {
                    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
                    let mut text = format!(
                        "{}{}",
                        theme.fg("toolTitle", &theme.bold("todo ")),
                        theme.fg("muted", action)
                    );
                    if let Some(t) = args.get("text").and_then(|v| v.as_str())
                        && !t.is_empty()
                    {
                        text.push_str(&format!(" {}", theme.fg("dim", &format!("\"{t}\""))));
                    }
                    if let Some(id) = args.get("id").and_then(|v| v.as_u64()) {
                        text.push_str(&format!(" {}", theme.fg("accent", &format!("#{id}"))));
                    }
                    Arc::new(Text::new(Some(&text), Some(0), Some(0), None))
                })),
                render_result: Some(Arc::new(|result, opts, theme, _context| {
                    let details: Option<TodoDetails> =
                        result.details.clone().and_then(|v| serde_json::from_value(v).ok());
                    let Some(details) = details else {
                        let text = match result.content.first() {
                            Some(UserContentBlock::Text(t)) => t.text.as_str(),
                            _ => "",
                        };
                        return Arc::new(Text::new(Some(text), Some(0), Some(0), None));
                    };

                    if let Some(ref err) = details.error {
                        return Arc::new(Text::new(
                            Some(&theme.fg("error", &format!("Error: {err}"))),
                            Some(0),
                            Some(0),
                            None,
                        ));
                    }

                    let todo_list = &details.todos;
                    match details.action {
                        TodoAction::List => {
                            if todo_list.is_empty() {
                                return Arc::new(Text::new(Some(&theme.fg("dim", "No todos")), Some(0), Some(0), None));
                            }
                            let mut list_text = theme.fg("muted", &format!("{} todo(s):", todo_list.len()));
                            let display_len = if opts.expanded {
                                todo_list.len()
                            } else {
                                todo_list.len().min(5)
                            };
                            for t in &todo_list[..display_len] {
                                let check = if t.done {
                                    theme.fg("success", "✓")
                                } else {
                                    theme.fg("dim", "○")
                                };
                                let item_text = if t.done {
                                    theme.fg("dim", &t.text)
                                } else {
                                    theme.fg("muted", &t.text)
                                };
                                list_text.push_str(&format!(
                                    "\n{check} {} {item_text}",
                                    theme.fg("accent", &format!("#{}", t.id))
                                ));
                            }
                            if !opts.expanded && todo_list.len() > 5 {
                                list_text.push_str(&format!(
                                    "\n{}",
                                    theme.fg("dim", &format!("... {} more", todo_list.len() - 5))
                                ));
                            }
                            Arc::new(Text::new(Some(&list_text), Some(0), Some(0), None))
                        }
                        TodoAction::Add => {
                            let text = if let Some(added) = todo_list.last() {
                                format!(
                                    "{}{} {}",
                                    theme.fg("success", "✓ Added "),
                                    theme.fg("accent", &format!("#{}", added.id)),
                                    theme.fg("muted", &added.text)
                                )
                            } else {
                                theme.fg("success", "✓ Added")
                            };
                            Arc::new(Text::new(Some(&text), Some(0), Some(0), None))
                        }
                        TodoAction::Toggle => {
                            let msg = match result.content.first() {
                                Some(UserContentBlock::Text(t)) => t.text.as_str(),
                                _ => "",
                            };
                            Arc::new(Text::new(
                                Some(&format!("{}{}", theme.fg("success", "✓ "), theme.fg("muted", msg))),
                                Some(0),
                                Some(0),
                                None,
                            ))
                        }
                        TodoAction::Clear => Arc::new(Text::new(
                            Some(&format!(
                                "{}{}",
                                theme.fg("success", "✓ "),
                                theme.fg("muted", "Cleared all todos")
                            )),
                            Some(0),
                            Some(0),
                            None,
                        )),
                    }
                })),
            })?;
        }

        {
            let state_cmd = Arc::clone(&state);
            pi.register_command(
                "todos",
                RegisteredCommandOptions {
                    description: Some("Show all todos on the current branch".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |_args, ctx| {
                        let state = Arc::clone(&state_cmd);
                        Box::pin(async move {
                            let base = ctx.as_context();
                            let ui = base.ui()?;
                            if base.mode()? != ExtensionMode::Tui {
                                ui.notify("/todos requires interactive mode", Some(NotifyType::Error));
                                return Ok(());
                            }
                            let todos_snapshot = state.lock().unwrap_or_else(|e| e.into_inner()).todos.clone();
                            let _ = ui
                                .custom(
                                    Box::new(move |_tui, theme, _kb, done| {
                                        Box::pin(async move {
                                            Ok(Arc::new(TodoListComponent::new(
                                                todos_snapshot,
                                                theme,
                                                Arc::new(move || done(Value::Null)),
                                            )) as Arc<dyn Component>)
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

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(TodoExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
