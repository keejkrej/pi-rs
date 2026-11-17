//! Port of packages/coding-agent/examples/extensions/summarize.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_ai::compat::complete_simple;
use pi_ai::env_api_keys::get_env_api_key;
use pi_ai::types::{
    AssistantContentBlock, CacheRetention, Context, Message, SimpleStreamOptions, StreamOptions, TextContent,
    ThinkingLevel, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_ai::uuidv7;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionCommandContext, ExtensionFactory, ExtensionMode, InlineExtension, NotifyType,
    RegisteredCommandOptions, Theme,
};
use pi_tui::components::markdown::MarkdownTheme;
use pi_tui::{Component, Container, Markdown, Text, matches_key};
use serde_json::Value;

struct FallbackDynamicBorder {
    // WIRE(@tui-w3): use pi_coding_agent::modes::interactive::components::DynamicBorder once @tui merges.
    color: Arc<dyn Fn(&str) -> String + Send + Sync>,
}

impl Component for FallbackDynamicBorder {
    fn render(&self, width: usize) -> Vec<String> {
        vec![(self.color)(&"─".repeat(width.max(1)))]
    }

    fn invalidate(&self) {}
}

fn fallback_markdown_theme(theme: &Theme) -> MarkdownTheme {
    // WIRE(@tui-w3): use pi_coding_agent::modes::interactive::theme::get_markdown_theme() once @tui merges.
    let t_heading = theme.clone();
    let t_link = theme.clone();
    let t_link_url = theme.clone();
    let t_code = theme.clone();
    let t_code_block = theme.clone();
    let t_code_border = theme.clone();
    let t_quote = theme.clone();
    let t_quote_border = theme.clone();
    let t_hr = theme.clone();
    let t_bullet = theme.clone();
    let t_bold = theme.clone();
    let t_italic = theme.clone();
    let t_strike = theme.clone();
    let t_under = theme.clone();
    MarkdownTheme {
        heading: Arc::new(move |s| t_heading.fg("mdHeading", &t_heading.bold(s))),
        link: Arc::new(move |s| t_link.fg("mdLink", s)),
        link_url: Arc::new(move |s| t_link_url.fg("mdLinkUrl", s)),
        code: Arc::new(move |s| t_code.fg("mdCode", s)),
        code_block: Arc::new(move |s| t_code_block.fg("mdCodeBlock", s)),
        code_block_border: Arc::new(move |s| t_code_border.fg("mdCodeBlockBorder", s)),
        quote: Arc::new(move |s| t_quote.fg("mdQuote", s)),
        quote_border: Arc::new(move |s| t_quote_border.fg("mdQuoteBorder", s)),
        hr: Arc::new(move |s| t_hr.fg("mdHr", s)),
        list_bullet: Arc::new(move |s| t_bullet.fg("mdListBullet", s)),
        bold: Arc::new(move |s| t_bold.bold(s)),
        italic: Arc::new(move |s| t_italic.italic(s)),
        strikethrough: Arc::new(move |s| t_strike.strikethrough(s)),
        underline: Arc::new(move |s| t_under.underline(s)),
        highlight_code: None,
        code_block_indent: None,
    }
}

struct SummaryModal {
    container: Container,
    on_close: Arc<dyn Fn() + Send + Sync>,
}

impl Component for SummaryModal {
    fn render(&self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn handle_input(&self, data: &str) {
        if matches_key(data, "enter") || matches_key(data, "escape") {
            (self.on_close)();
        }
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn invalidate(&self) {
        self.container.invalidate();
    }
}

fn extract_text_parts(content: Option<&Value>) -> Vec<String> {
    let Some(val) = content else {
        return Vec::new();
    };
    if let Some(s) = val.as_str() {
        return vec![s.to_string()];
    }
    let Some(arr) = val.as_array() else {
        return Vec::new();
    };
    let mut parts = Vec::new();
    for part in arr {
        if part.get("type").and_then(|v| v.as_str()) == Some("text")
            && let Some(text) = part.get("text").and_then(|v| v.as_str())
        {
            parts.push(text.to_string());
        }
    }
    parts
}

fn extract_tool_call_lines(content: Option<&Value>) -> Vec<String> {
    let Some(arr) = content.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut calls = Vec::new();
    for part in arr {
        if part.get("type").and_then(|v| v.as_str()) == Some("toolCall")
            && let Some(name) = part.get("name").and_then(|v| v.as_str())
        {
            let args_json = part
                .get("arguments")
                .map(pi_js::json::stringify)
                .unwrap_or_else(|| "{}".to_string());
            calls.push(format!("Tool {name} was called with args {args_json}"));
        }
    }
    calls
}

fn build_conversation_text(entries: &[pi_coding_agent::core::session_manager::SessionEntry]) -> String {
    let mut sections = Vec::new();
    for entry in entries {
        let pi_coding_agent::core::session_manager::SessionEntry::Message(msg_entry) = entry else {
            continue;
        };
        let Ok(msg) = serde_json::to_value(&msg_entry.message) else {
            continue;
        };
        let Some(role) = msg.get("role").and_then(|v| v.as_str()) else {
            continue;
        };
        let is_user = role == "user";
        let is_assistant = role == "assistant";
        if !is_user && !is_assistant {
            continue;
        }

        let mut entry_lines = Vec::new();
        let content = msg.get("content");
        let text_parts = extract_text_parts(content);
        if !text_parts.is_empty() {
            let role_label = if is_user { "User" } else { "Assistant" };
            let message_text = text_parts.join("\n").trim().to_string();
            if !message_text.is_empty() {
                entry_lines.push(format!("{role_label}: {message_text}"));
            }
        }
        if is_assistant {
            entry_lines.extend(extract_tool_call_lines(content));
        }
        if !entry_lines.is_empty() {
            sections.push(entry_lines.join("\n"));
        }
    }
    sections.join("\n\n")
}

fn build_summary_prompt(conversation_text: &str) -> String {
    format!(
        "Summarize this conversation so I can resume it later.\n\
         Include goals, key decisions, progress, open questions, and next steps.\n\
         Keep it concise and structured with headings.\n\n\
         <conversation>\n\
         {conversation_text}\n\
         </conversation>"
    )
}

async fn show_summary_ui(summary: String, ctx: &ExtensionCommandContext) -> pi_js::Result<()> {
    let base = ctx.as_context();
    if base.mode()? != ExtensionMode::Tui {
        return Ok(());
    }
    let _ = base
        .ui()?
        .custom(
            Box::new(move |_tui, theme: Theme, _kb, done| {
                Box::pin(async move {
                    let container = Container::new();
                    let t_border = theme.clone();
                    let border = Arc::new(FallbackDynamicBorder {
                        color: Arc::new(move |s| t_border.fg("accent", s)),
                    });
                    let md_theme = fallback_markdown_theme(&theme);

                    container.add_child(border.clone());
                    container.add_child(Arc::new(Text::new(
                        Some(&theme.fg("accent", &theme.bold("Conversation Summary"))),
                        Some(1),
                        Some(0),
                        None,
                    )));
                    container.add_child(Arc::new(Markdown::new(&summary, 1, 1, md_theme, None, None)));
                    container.add_child(Arc::new(Text::new(
                        Some(&theme.fg("dim", "Press Enter or Esc to close")),
                        Some(1),
                        Some(0),
                        None,
                    )));
                    container.add_child(border);

                    Ok(Arc::new(SummaryModal {
                        container,
                        on_close: Arc::new(move || done(Value::Null)),
                    }) as Arc<dyn Component>)
                })
            }),
            None,
        )
        .await?;
    Ok(())
}

pub struct SummarizeExtension;

#[async_trait]
impl Extension for SummarizeExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.register_command(
            "summarize",
            RegisteredCommandOptions {
                description: Some("Summarize the current conversation in a custom UI".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| {
                    Box::pin(async move {
                        let base = ctx.as_context();
                        let has_ui = base.has_ui()?;
                        let ui = base.ui()?;
                        let branch = base.session_manager()?.get_branch(None);
                        let conversation_text = build_conversation_text(&branch);

                        if conversation_text.trim().is_empty() {
                            if has_ui {
                                ui.notify("No conversation text found", Some(NotifyType::Warning));
                            }
                            return Ok(());
                        }

                        if has_ui {
                            ui.notify("Preparing summary...", Some(NotifyType::Info));
                        }

                        let registry = base.model_registry()?;
                        let Some(model) = registry
                            .get_available()
                            .into_iter()
                            .find(|m| m.provider == "openai" && m.id == "gpt-5.2")
                        else {
                            if has_ui {
                                ui.notify("Model openai/gpt-5.2 not found", Some(NotifyType::Warning));
                            }
                            return Ok(());
                        };

                        let api_key = match registry.get_api_key_and_headers(&model).await {
                            pi_coding_agent::core::model_registry::ResolvedRequestAuth::Ok { api_key, .. } => api_key,
                            pi_coding_agent::core::model_registry::ResolvedRequestAuth::Err { .. } => None,
                        };
                        let has_auth = api_key.as_ref().is_some_and(|k| !k.is_empty())
                            || get_env_api_key(&model.provider, None).is_some();
                        if !has_auth {
                            if has_ui {
                                ui.notify(
                                    "No authentication configured for openai/gpt-5.2",
                                    Some(NotifyType::Warning),
                                );
                            }
                            return Ok(());
                        }

                        let user_message = Message::User(UserMessage {
                            content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new(
                                build_summary_prompt(&conversation_text),
                            ))]),
                            timestamp: pi_js::time::now_ms(),
                        });
                        let context = Context {
                            system_prompt: None,
                            messages: vec![user_message],
                            tools: None,
                        };
                        let opts = SimpleStreamOptions {
                            base: StreamOptions {
                                api_key,
                                cache_retention: Some(CacheRetention::None),
                                session_id: uuidv7(None).ok(),
                                ..Default::default()
                            },
                            reasoning: Some(ThinkingLevel::High),
                            ..Default::default()
                        };
                        let response = complete_simple(&model, &context, Some(opts)).await;
                        let summary = response
                            .content
                            .iter()
                            .filter_map(|c| match c {
                                AssistantContentBlock::Text(t) => Some(t.text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n");

                        show_summary_ui(summary, &ctx).await
                    })
                }),
            },
        )?;

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(SummarizeExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
