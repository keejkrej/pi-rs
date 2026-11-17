//! Port of packages/coding-agent/examples/extensions/qna.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_agent_core::types::AgentMessage;
use pi_ai::compat::complete_simple;
use pi_ai::types::{
    AssistantContentBlock, Context, Message, SimpleStreamOptions, StopReason, StreamOptions, TextContent,
    UserContentBlock, UserMessage, UserMessageContent,
};
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, ExtensionMode, InlineExtension, NotifyType, RegisteredCommandOptions,
};
use pi_coding_agent::modes::interactive::components::bordered_loader::BorderedLoader;
use pi_coding_agent::modes::interactive::theme::theme::theme as current_interactive_theme;
use pi_tui::Component;
use serde_json::Value;

const SYSTEM_PROMPT: &str = "You are a question extractor. Given text from a conversation, extract any questions that need answering and format them for the user to fill in.\n\n\
Output format:\n\
- List each question on its own line, prefixed with \"Q: \"\n\
- After each question, add a blank line for the answer prefixed with \"A: \"\n\
- If no questions are found, output \"No questions found in the last message.\"\n\n\
Example output:\n\
Q: What is your preferred database?\n\
A: \n\n\
Q: Should we use TypeScript or JavaScript?\n\
A: \n\n\
Keep questions in the order they appeared. Be concise.";

fn stop_reason_name(reason: StopReason) -> &'static str {
    match reason {
        StopReason::Pending => "pending",
        StopReason::Stop => "stop",
        StopReason::Length => "length",
        StopReason::ToolUse => "toolUse",
        StopReason::Error => "error",
        StopReason::Aborted => "aborted",
        _ => "end",
    }
}

pub struct QnaExtension;

#[async_trait]
impl Extension for QnaExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.register_command(
            "qna",
            RegisteredCommandOptions {
                description: Some("Extract questions from last assistant message into editor".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| {
                    Box::pin(async move {
                        let base = ctx.as_context();
                        let ui = base.ui()?;

                        if base.mode()? != ExtensionMode::Tui {
                            ui.notify("qna requires interactive mode", Some(NotifyType::Error));
                            return Ok(());
                        }

                        let Some(model) = base.model()? else {
                            ui.notify("No model selected", Some(NotifyType::Error));
                            return Ok(());
                        };

                        let branch = base.session_manager()?.get_branch(None);
                        let mut last_assistant_text: Option<String> = None;

                        for entry in branch.iter().rev() {
                            if let pi_coding_agent::core::session_manager::SessionEntry::Message(msg_entry) = entry
                                && let AgentMessage::Message(Message::Assistant(assistant)) = &msg_entry.message
                            {
                                if assistant.stop_reason != StopReason::Stop {
                                    ui.notify(
                                        &format!(
                                            "Last assistant message incomplete ({})",
                                            stop_reason_name(assistant.stop_reason)
                                        ),
                                        Some(NotifyType::Error),
                                    );
                                    return Ok(());
                                }
                                let text_parts: Vec<&str> = assistant
                                    .content
                                    .iter()
                                    .filter_map(|c| match c {
                                        AssistantContentBlock::Text(t) => Some(t.text.as_str()),
                                        _ => None,
                                    })
                                    .collect();
                                if !text_parts.is_empty() {
                                    last_assistant_text = Some(text_parts.join("\n"));
                                    break;
                                }
                            }
                        }

                        let Some(last_text) = last_assistant_text else {
                            ui.notify("No assistant messages found", Some(NotifyType::Error));
                            return Ok(());
                        };

                        let model_registry = base.model_registry()?;
                        let model_for_ui = model.clone();
                        let result = ui
                            .custom(
                                Box::new(move |tui, theme, _kb, done| {
                                    Box::pin(async move {
                                        let interactive_theme =
                                            theme.0.to_interactive_theme().unwrap_or_else(current_interactive_theme);
                                        let loader = BorderedLoader::new(
                                            tui,
                                            &interactive_theme,
                                            &format!("Extracting questions using {}...", model_for_ui.id),
                                            None,
                                        );
                                        let signal = loader.signal();
                                        let done_abort = Arc::clone(&done);
                                        loader.set_on_abort(Some(Arc::new(move || {
                                            done_abort(Value::Null);
                                        })));

                                        let done_task = Arc::clone(&done);
                                        tokio::spawn(async move {
                                            let api_key = match model_registry
                                                .get_api_key_and_headers(&model_for_ui)
                                                .await
                                            {
                                                pi_coding_agent::core::model_registry::ResolvedRequestAuth::Ok {
                                                    api_key,
                                                    ..
                                                } => api_key,
                                                pi_coding_agent::core::model_registry::ResolvedRequestAuth::Err {
                                                    ..
                                                } => None,
                                            };
                                            let user_message = Message::User(UserMessage {
                                                content: UserMessageContent::Blocks(vec![UserContentBlock::Text(
                                                    TextContent::new(last_text),
                                                )]),
                                                timestamp: pi_js::time::now_ms(),
                                            });
                                            let context = Context {
                                                system_prompt: Some(SYSTEM_PROMPT.to_string()),
                                                messages: vec![user_message],
                                                tools: None,
                                            };
                                            let opts = SimpleStreamOptions {
                                                base: StreamOptions {
                                                    api_key,
                                                    signal: Some(signal),
                                                    ..Default::default()
                                                },
                                                ..Default::default()
                                            };
                                            let response = complete_simple(&model_for_ui, &context, Some(opts)).await;
                                            if response.stop_reason == StopReason::Aborted {
                                                done_task(Value::Null);
                                                return;
                                            }
                                            let extracted = response
                                                .content
                                                .iter()
                                                .filter_map(|c| match c {
                                                    AssistantContentBlock::Text(t) => Some(t.text.as_str()),
                                                    _ => None,
                                                })
                                                .collect::<Vec<_>>()
                                                .join("\n");
                                            done_task(Value::String(extracted));
                                        });

                                        Ok(Arc::new(loader) as Arc<dyn Component>)
                                    })
                                }),
                                None,
                            )
                            .await?;

                        let Some(text) = result.as_str() else {
                            ui.notify("Cancelled", Some(NotifyType::Info));
                            return Ok(());
                        };

                        ui.set_editor_text(text);
                        ui.notify("Questions loaded. Edit and submit when ready.", Some(NotifyType::Info));
                        Ok(())
                    })
                }),
            },
        )?;

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(QnaExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
