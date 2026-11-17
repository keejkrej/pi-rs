//! Port of packages/coding-agent/examples/extensions/question.ts

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_agent_core::types::{AgentToolResult, ToolExecutionMode};
use pi_ai::types::{TextContent, UserContentBlock};
use pi_coding_agent::core::extensions::types::{
    DoneFn, Extension, ExtensionApi, ExtensionFactory, ExtensionMode, InlineExtension, Theme, ToolDefinition,
    ToolRenderResultOptions,
};
use pi_js::vendor::typebox::t;
use pi_tui::{
    Component, Editor, EditorTheme, Key, SelectListTheme, TUI, Text, matches_key, visible_width, wrap_text_with_ansi,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Deserialize)]
struct OptionWithDesc {
    label: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Clone, Debug)]
struct DisplayOption {
    label: String,
    description: Option<String>,
    is_other: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct QuestionParams {
    question: String,
    #[serde(default)]
    options: Vec<OptionWithDesc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuestionDetails {
    question: String,
    options: Vec<String>,
    answer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    was_custom: Option<bool>,
}

struct SelectorState {
    option_index: usize,
    edit_mode: bool,
    cached_lines: Option<Vec<String>>,
}

struct QuestionSelector {
    question: String,
    all_options: Vec<DisplayOption>,
    tui: Arc<dyn TUI + Send + Sync>,
    theme: Theme,
    done: DoneFn,
    editor: Editor,
    state: Arc<Mutex<SelectorState>>,
}

impl QuestionSelector {
    fn new(
        question: String,
        all_options: Vec<DisplayOption>,
        tui: Arc<dyn TUI + Send + Sync>,
        theme: Theme,
        done: DoneFn,
    ) -> Self {
        let t_border = theme.clone();
        let t_prefix = theme.clone();
        let t_text = theme.clone();
        let t_desc = theme.clone();
        let t_scroll = theme.clone();
        let t_nomatch = theme.clone();
        let editor_theme = EditorTheme {
            border_color: Arc::new(move |s| t_border.fg("accent", s)),
            select_list: SelectListTheme {
                selected_prefix: Arc::new(move |s| t_prefix.fg("accent", s)),
                selected_text: Arc::new(move |s| t_text.fg("accent", s)),
                description: Arc::new(move |s| t_desc.fg("muted", s)),
                scroll_info: Arc::new(move |s| t_scroll.fg("dim", s)),
                no_match: Arc::new(move |s| t_nomatch.fg("warning", s)),
            },
        };
        let editor = Editor::new(Arc::clone(&tui), editor_theme, None);
        let state = Arc::new(Mutex::new(SelectorState {
            option_index: 0,
            edit_mode: false,
            cached_lines: None,
        }));

        let editor_ref = editor.clone();
        let state_ref = Arc::clone(&state);
        let tui_ref = Arc::clone(&tui);
        let done_ref = Arc::clone(&done);
        editor.set_on_submit(Some(Arc::new(move |value| {
            let trimmed = value.trim().to_string();
            if !trimmed.is_empty() {
                done_ref(json!({ "answer": trimmed, "wasCustom": true }));
            } else {
                {
                    let mut guard = state_ref.lock().unwrap_or_else(|e| e.into_inner());
                    guard.edit_mode = false;
                    guard.cached_lines = None;
                }
                editor_ref.set_text("");
                tui_ref.request_render(Some(false));
            }
        })));

        Self {
            question,
            all_options,
            tui,
            theme,
            done,
            editor,
            state,
        }
    }

    fn refresh(&self) {
        {
            let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.cached_lines = None;
        }
        self.tui.request_render(Some(false));
    }
}

impl Component for QuestionSelector {
    fn has_handle_input(&self) -> bool {
        true
    }

    fn handle_input(&self, data: &str) {
        let is_edit = self.state.lock().unwrap_or_else(|e| e.into_inner()).edit_mode;
        if is_edit {
            if matches_key(data, Key::escape) {
                {
                    let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    guard.edit_mode = false;
                    guard.cached_lines = None;
                }
                self.editor.set_text("");
                self.tui.request_render(Some(false));
                return;
            }
            self.editor.handle_input(data);
            self.refresh();
            return;
        }

        if matches_key(data, Key::up) {
            {
                let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
                guard.option_index = guard.option_index.saturating_sub(1);
                guard.cached_lines = None;
            }
            self.tui.request_render(Some(false));
            return;
        }

        if matches_key(data, Key::down) {
            {
                let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let max_idx = self.all_options.len().saturating_sub(1);
                guard.option_index = (guard.option_index + 1).min(max_idx);
                guard.cached_lines = None;
            }
            self.tui.request_render(Some(false));
            return;
        }

        if matches_key(data, Key::enter) {
            let idx = self.state.lock().unwrap_or_else(|e| e.into_inner()).option_index;
            if let Some(selected) = self.all_options.get(idx) {
                if selected.is_other {
                    {
                        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        guard.edit_mode = true;
                        guard.cached_lines = None;
                    }
                    self.tui.request_render(Some(false));
                } else {
                    (self.done)(json!({
                        "answer": selected.label,
                        "wasCustom": false,
                        "index": idx + 1,
                    }));
                }
            }
            return;
        }

        if matches_key(data, Key::escape) {
            (self.done)(Value::Null);
        }
    }

    fn invalidate(&self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).cached_lines = None;
    }

    fn render(&self, width: usize) -> Vec<String> {
        let (cached, option_index, edit_mode) = {
            let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (guard.cached_lines.clone(), guard.option_index, guard.edit_mode)
        };
        if let Some(lines) = cached {
            return lines;
        }

        let render_width = width.max(1) as i64;
        let mut lines: Vec<String> = Vec::new();

        let add_wrapped_with_prefix = |out: &mut Vec<String>, prefix: &str, text: &str| {
            let prefix_width = visible_width(prefix).max(0);
            if prefix_width >= render_width {
                out.extend(wrap_text_with_ansi(&format!("{prefix}{text}"), render_width));
                return;
            }
            let wrapped = wrap_text_with_ansi(text, render_width - prefix_width);
            let continuation = " ".repeat(prefix_width as usize);
            for (i, part) in wrapped.into_iter().enumerate() {
                if i == 0 {
                    out.push(format!("{prefix}{part}"));
                } else {
                    out.push(format!("{continuation}{part}"));
                }
            }
        };

        lines.push(self.theme.fg("accent", &"─".repeat(render_width as usize)));
        add_wrapped_with_prefix(&mut lines, " ", &self.theme.fg("text", &self.question));
        lines.push(String::new());

        for (i, opt) in self.all_options.iter().enumerate() {
            let selected = i == option_index;
            let prefix = if selected {
                self.theme.fg("accent", "> ")
            } else {
                "  ".to_string()
            };
            let edit_suffix = if opt.is_other && edit_mode { " ✎" } else { "" };
            let label = format!("{}. {}{edit_suffix}", i + 1, opt.label);
            let color = if selected || (opt.is_other && edit_mode) {
                "accent"
            } else {
                "text"
            };
            add_wrapped_with_prefix(&mut lines, &prefix, &self.theme.fg(color, &label));

            if let Some(desc) = &opt.description {
                add_wrapped_with_prefix(&mut lines, "     ", &self.theme.fg("muted", desc));
            }
        }

        if edit_mode {
            lines.push(String::new());
            add_wrapped_with_prefix(&mut lines, " ", &self.theme.fg("muted", "Your answer:"));
            for line in self.editor.render((render_width - 2).max(1) as usize) {
                lines.push(format!(" {line}"));
            }
        }

        lines.push(String::new());
        if edit_mode {
            add_wrapped_with_prefix(
                &mut lines,
                " ",
                &self.theme.fg("dim", "Enter to submit • Esc to go back"),
            );
        } else {
            add_wrapped_with_prefix(
                &mut lines,
                " ",
                &self.theme.fg("dim", "↑↓ navigate • Enter to select • Esc to cancel"),
            );
        }
        lines.push(self.theme.fg("accent", &"─".repeat(render_width as usize)));

        self.state.lock().unwrap_or_else(|e| e.into_inner()).cached_lines = Some(lines.clone());
        lines
    }
}

pub struct QuestionExtension;

#[async_trait]
impl Extension for QuestionExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let option_schema = t::object([
            ("label", t::string().with("description", "Display label for the option")),
            (
                "description",
                t::string()
                    .optional()
                    .with("description", "Optional description shown below label"),
            ),
        ]);

        let params_schema = t::object([
            (
                "question",
                t::string().with("description", "The question to ask the user"),
            ),
            (
                "options",
                t::array(option_schema).with("description", "Options for the user to choose from"),
            ),
        ]);

        pi.register_tool(ToolDefinition {
            name: "question".to_string(),
            label: "Question".to_string(),
            description:
                "Ask the user a question and let them pick from options. Use when you need user input to proceed."
                    .to_string(),
            prompt_snippet: None,
            prompt_guidelines: None,
            parameters: params_schema,
            constrained_sampling: None,
            render_shell: None,
            prepare_arguments: None,
            output_schema: None,
            exposure: None,
            namespace: None,
            annotations: None,
            default_active: None,
            prepare_loadout: None,
            execution_mode: Some(ToolExecutionMode::Sequential),
            execute: Arc::new(|_tool_call_id, raw_params, _signal, _on_update, ctx| {
                Box::pin(async move {
                    let params: QuestionParams = serde_json::from_value(raw_params)
                        .map_err(|e| pi_js::Error::msg(format!("Invalid question parameters: {e}")))?;
                    let simple_options: Vec<String> = params.options.iter().map(|o| o.label.clone()).collect();

                    if ctx.as_context().mode()? != ExtensionMode::Tui {
                        let details = QuestionDetails {
                            question: params.question,
                            options: simple_options,
                            answer: None,
                            was_custom: None,
                        };
                        return Ok(AgentToolResult {
                            content: vec![UserContentBlock::Text(TextContent::new(
                                "Error: UI not available (running in non-interactive mode)",
                            ))],
                            details: serde_json::to_value(details).ok(),
                            structured_content: None,
                            usage: None,
                            is_error: None,
                            terminate: None,
                            control: None,
                        });
                    }

                    if params.options.is_empty() {
                        let details = QuestionDetails {
                            question: params.question,
                            options: Vec::new(),
                            answer: None,
                            was_custom: None,
                        };
                        return Ok(AgentToolResult {
                            content: vec![UserContentBlock::Text(TextContent::new("Error: No options provided"))],
                            details: serde_json::to_value(details).ok(),
                            structured_content: None,
                            usage: None,
                            is_error: None,
                            terminate: None,
                            control: None,
                        });
                    }

                    let mut all_options: Vec<DisplayOption> = params
                        .options
                        .iter()
                        .map(|o| DisplayOption {
                            label: o.label.clone(),
                            description: o.description.clone(),
                            is_other: false,
                        })
                        .collect();
                    all_options.push(DisplayOption {
                        label: "Type something.".to_string(),
                        description: None,
                        is_other: true,
                    });

                    let ui = ctx.ui()?;
                    let question_text = params.question.clone();
                    let raw_res = ui
                        .custom(
                            Box::new(move |tui, theme, _kb, done| {
                                Box::pin(async move {
                                    Ok(
                                        Arc::new(QuestionSelector::new(question_text, all_options, tui, theme, done))
                                            as Arc<dyn Component>,
                                    )
                                })
                            }),
                            None,
                        )
                        .await?;

                    let Value::Object(obj) = raw_res else {
                        let details = QuestionDetails {
                            question: params.question,
                            options: simple_options,
                            answer: None,
                            was_custom: None,
                        };
                        return Ok(AgentToolResult {
                            content: vec![UserContentBlock::Text(TextContent::new("User cancelled the selection"))],
                            details: serde_json::to_value(details).ok(),
                            structured_content: None,
                            usage: None,
                            is_error: None,
                            terminate: None,
                            control: None,
                        });
                    };

                    let answer = obj.get("answer").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let was_custom = obj.get("wasCustom").and_then(|v| v.as_bool()).unwrap_or(false);
                    let index = obj.get("index").and_then(|v| v.as_u64()).unwrap_or(1);

                    let msg = if was_custom {
                        format!("User wrote: {answer}")
                    } else {
                        format!("User selected: {index}. {answer}")
                    };
                    let details = QuestionDetails {
                        question: params.question,
                        options: simple_options,
                        answer: Some(answer),
                        was_custom: Some(was_custom),
                    };

                    Ok(AgentToolResult {
                        content: vec![UserContentBlock::Text(TextContent::new(msg))],
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
                let q = args.get("question").and_then(|v| v.as_str()).unwrap_or("");
                let mut text = format!(
                    "{}{}",
                    theme.fg("toolTitle", &theme.bold("question ")),
                    theme.fg("muted", q)
                );
                if let Some(opts) = args.get("options").and_then(|v| v.as_array())
                    && !opts.is_empty()
                {
                    let mut numbered: Vec<String> = opts
                        .iter()
                        .enumerate()
                        .map(|(i, o)| {
                            let label = o.get("label").and_then(|v| v.as_str()).unwrap_or("");
                            format!("{}. {label}", i + 1)
                        })
                        .collect();
                    numbered.push(format!("{}. Type something.", numbered.len() + 1));
                    text.push('\n');
                    text.push_str(&theme.fg("dim", &format!("  Options: {}", numbered.join(", "))));
                }
                Arc::new(Text::new(Some(&text), Some(0), Some(0), None))
            })),
            render_result: Some(Arc::new(
                |result, _options: ToolRenderResultOptions, theme, _context| {
                    let details = result
                        .details
                        .as_ref()
                        .and_then(|d| serde_json::from_value::<QuestionDetails>(d.clone()).ok());
                    let Some(details) = details else {
                        let msg = match result.content.first() {
                            Some(UserContentBlock::Text(tc)) => tc.text.as_str(),
                            _ => "",
                        };
                        return Arc::new(Text::new(Some(msg), Some(0), Some(0), None));
                    };

                    let Some(answer) = details.answer else {
                        return Arc::new(Text::new(
                            Some(&theme.fg("warning", "Cancelled")),
                            Some(0),
                            Some(0),
                            None,
                        ));
                    };

                    if details.was_custom.unwrap_or(false) {
                        let msg = format!(
                            "{}{}{}",
                            theme.fg("success", "✓ "),
                            theme.fg("muted", "(wrote) "),
                            theme.fg("accent", &answer)
                        );
                        return Arc::new(Text::new(Some(&msg), Some(0), Some(0), None));
                    }

                    let idx = details.options.iter().position(|o| o == &answer).map(|i| i + 1);
                    let display = match idx {
                        Some(n) => format!("{n}. {answer}"),
                        None => answer,
                    };
                    let msg = format!("{}{}", theme.fg("success", "✓ "), theme.fg("accent", &display));
                    Arc::new(Text::new(Some(&msg), Some(0), Some(0), None))
                },
            )),
        })?;

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(QuestionExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
