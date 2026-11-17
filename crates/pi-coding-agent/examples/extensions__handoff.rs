//! Port of packages/coding-agent/examples/extensions/handoff.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_agent_core::types::AgentMessage;
use pi_ai::compat::complete_simple;
use pi_ai::types::{
    AssistantContentBlock, CacheRetention, Context, Message, SimpleStreamOptions, StopReason, StreamOptions,
    TextContent, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_coding_agent::core::compaction::serialize_conversation;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, ExtensionMode, InlineExtension, NewSessionOptions, NotifyType,
    RegisteredCommandOptions, Theme,
};
use pi_coding_agent::core::messages::convert_to_llm;
use pi_coding_agent::core::model_registry::ResolvedRequestAuth;
use pi_coding_agent::core::session_manager::SessionEntry;
use pi_tui::components::cancellable_loader::CancellableLoader;
use pi_tui::{Component, Container, Text};
use serde_json::{Value, json};

const SYSTEM_PROMPT: &str = "You are a context transfer assistant. Given a conversation history and the user's goal for a new thread, generate a focused prompt that:\n\n\
1. Summarizes relevant context from the conversation (decisions made, approaches taken, key findings)\n\
2. Lists any relevant files that were discussed or modified\n\
3. Clearly states the next task based on the user's goal\n\
4. Is self-contained - the new thread should be able to proceed without the old conversation\n\n\
Format your response as a prompt the user can send to start the new thread. Be concise but include all necessary context. Do not include any preamble like \"Here's the prompt\" - just output the prompt itself.\n\n\
Example output format:\n\
## Context\n\
We've been working on X. Key decisions:\n\
- Decision 1\n\
- Decision 2\n\n\
Files involved:\n\
- path/to/file1.ts\n\
- path/to/file2.ts\n\n\
## Task\n\
[Clear description of what to do next based on user's goal]";

struct FallbackDynamicBorder {
    color: Arc<dyn Fn(&str) -> String + Send + Sync>,
}

impl Component for FallbackDynamicBorder {
    fn render(&self, width: usize) -> Vec<String> {
        vec![(self.color)(&"─".repeat(width.max(1)))]
    }

    fn invalidate(&self) {}
}

struct FallbackBorderedLoader {
    // WIRE(@tui-w3): use pi_coding_agent::modes::interactive::components::BorderedLoader once @tui merges.
    container: Container,
    loader: CancellableLoader,
}

impl FallbackBorderedLoader {
    fn new(tui: Arc<dyn pi_tui::TUI>, theme: &Theme, message: &str) -> Self {
        let container = Container::new();
        let t_border1 = theme.clone();
        container.add_child(Arc::new(FallbackDynamicBorder {
            color: Arc::new(move |s| t_border1.fg("border", s)),
        }));
        let t_spin = theme.clone();
        let t_msg = theme.clone();
        let loader = CancellableLoader::new(
            tui,
            Arc::new(move |s| t_spin.fg("accent", s)),
            Arc::new(move |s| t_msg.fg("muted", s)),
            Some(message),
            None,
        );
        container.add_child(Arc::new(loader.clone()));
        container.add_child(Arc::new(Text::new(
            Some(&theme.fg("dim", "esc cancel")),
            Some(1),
            Some(0),
            None,
        )));
        let t_border2 = theme.clone();
        container.add_child(Arc::new(FallbackDynamicBorder {
            color: Arc::new(move |s| t_border2.fg("border", s)),
        }));
        Self { container, loader }
    }
}

impl Drop for FallbackBorderedLoader {
    fn drop(&mut self) {
        self.loader.dispose();
    }
}

impl Component for FallbackBorderedLoader {
    fn has_handle_input(&self) -> bool {
        true
    }

    fn handle_input(&self, data: &str) {
        self.loader.handle_input(data);
    }

    fn render(&self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn invalidate(&self) {
        self.container.invalidate();
    }
}

fn entry_to_message(entry: &SessionEntry) -> Option<AgentMessage> {
    match entry {
        SessionEntry::Message(m) => Some(m.message.clone()),
        SessionEntry::Compaction(c) => {
            let timestamp = chrono::DateTime::parse_from_rfc3339(&c.timestamp)
                .map(|dt| dt.timestamp_millis())
                .unwrap_or_else(|_| pi_js::time::now_ms() as i64);
            Some(AgentMessage::Custom(json!({
                "role": "compactionSummary",
                "summary": c.summary,
                "tokensBefore": c.tokens_before,
                "timestamp": timestamp,
            })))
        }
        _ => None,
    }
}

fn get_handoff_messages(branch: &[SessionEntry]) -> Vec<AgentMessage> {
    let compaction_index = branch.iter().rposition(|e| matches!(e, SessionEntry::Compaction(_)));
    let Some(compaction_idx) = compaction_index else {
        return branch.iter().filter_map(entry_to_message).collect();
    };

    let compaction = &branch[compaction_idx];
    let first_kept_id = match compaction {
        SessionEntry::Compaction(c) => c.first_kept_entry_id.as_str(),
        _ => "",
    };
    let first_kept_idx = branch.iter().position(|e| e.id() == first_kept_id);

    let mut compacted: Vec<&SessionEntry> = vec![compaction];
    if let Some(fk_idx) = first_kept_idx
        && fk_idx <= compaction_idx
    {
        compacted.extend(branch[fk_idx..compaction_idx].iter());
    }
    compacted.extend(branch[compaction_idx + 1..].iter());
    compacted.into_iter().filter_map(entry_to_message).collect()
}

pub struct HandoffExtension;

#[async_trait]
impl Extension for HandoffExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.register_command(
            "handoff",
            RegisteredCommandOptions {
                description: Some("Transfer context to a new focused session".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|args, ctx| {
                    Box::pin(async move {
                        let base = ctx.as_context();
                        let ui = base.ui()?;
                        if base.mode()? != ExtensionMode::Tui {
                            ui.notify("handoff requires interactive mode", Some(NotifyType::Error));
                            return Ok(());
                        }

                        let Some(model) = base.model()? else {
                            ui.notify("No model selected", Some(NotifyType::Error));
                            return Ok(());
                        };

                        let goal = args.trim().to_string();
                        if goal.is_empty() {
                            ui.notify("Usage: /handoff <goal for new thread>", Some(NotifyType::Error));
                            return Ok(());
                        }

                        let sm = base.session_manager()?;
                        let messages = get_handoff_messages(&sm.get_branch(None));
                        if messages.is_empty() {
                            ui.notify("No conversation to hand off", Some(NotifyType::Error));
                            return Ok(());
                        }

                        let llm_messages = convert_to_llm(&messages);
                        let conversation_text = serialize_conversation(&llm_messages);
                        let current_session_file = sm.get_session_file();
                        let registry = base.model_registry()?;

                        let result_val = ui
                            .custom(
                                Box::new(move |tui, theme, _kb, done| {
                                    Box::pin(async move {
                                        let bordered =
                                            FallbackBorderedLoader::new(tui, &theme, "Generating handoff prompt...");
                                        let done_abort = Arc::clone(&done);
                                        bordered.loader.set_on_abort(Some(Arc::new(move || {
                                            done_abort(Value::Null);
                                        })));

                                        let signal = bordered.loader.signal();
                                        let done_task = Arc::clone(&done);
                                        let reg = registry.clone();
                                        let model_clone = model.clone();

                                        tokio::spawn(async move {
                                            let user_message = Message::User(UserMessage {
                                                content: UserMessageContent::Blocks(vec![UserContentBlock::Text(
                                                    TextContent::new(format!(
                                                        "## Conversation History\n\n{conversation_text}\n\n## User's Goal for New Thread\n\n{goal}"
                                                    )),
                                                )]),
                                                timestamp: pi_js::time::now_ms() as i64,
                                            });

                                            let api_key = match reg.get_api_key_and_headers(&model_clone).await {
                                                ResolvedRequestAuth::Ok { api_key, .. } => api_key,
                                                ResolvedRequestAuth::Err { .. } => None,
                                            };
                                            let response = complete_simple(
                                                &model_clone,
                                                &Context {
                                                    system_prompt: Some(SYSTEM_PROMPT.to_string()),
                                                    messages: vec![user_message],
                                                    tools: None,
                                                },
                                                Some(SimpleStreamOptions {
                                                    base: StreamOptions {
                                                        api_key,
                                                        signal: Some(signal),
                                                        cache_retention: Some(CacheRetention::None),
                                                        session_id: pi_ai::uuidv7(None).ok(),
                                                        ..Default::default()
                                                    },
                                                    ..Default::default()
                                                }),
                                            )
                                            .await;

                                            if response.stop_reason == StopReason::Aborted {
                                                done_task(Value::Null);
                                                return;
                                            }
                                            if response.stop_reason == StopReason::Error {
                                                let msg = response
                                                    .error_message
                                                    .unwrap_or_else(|| "Unknown error".to_string());
                                                pi_js::console::error(&format!("Handoff generation failed: {msg}"));
                                                done_task(Value::Null);
                                                return;
                                            }

                                            let text = response
                                                .content
                                                .iter()
                                                .filter_map(|b| match b {
                                                    AssistantContentBlock::Text(tc) => Some(tc.text.as_str()),
                                                    _ => None,
                                                })
                                                .collect::<Vec<_>>()
                                                .join("\n");
                                            done_task(Value::String(text));
                                        });

                                        Ok(Arc::new(bordered) as Arc<dyn Component>)
                                    })
                                }),
                                None,
                            )
                            .await?;

                        let Value::String(result) = result_val else {
                            ui.notify("Cancelled", Some(NotifyType::Info));
                            return Ok(());
                        };

                        let Some(edited_prompt) = ui.editor("Edit handoff prompt", Some(&result)).await? else {
                            ui.notify("Cancelled", Some(NotifyType::Info));
                            return Ok(());
                        };

                        let new_session_res = ctx
                            .new_session(Some(NewSessionOptions {
                                parent_session: current_session_file,
                                setup: None,
                                with_session: Some(Box::new(move |replacement_ctx| {
                                    Box::pin(async move {
                                        let r_ui = replacement_ctx.as_command_context().as_context().ui()?;
                                        r_ui.set_editor_text(&edited_prompt);
                                        r_ui.notify("Handoff ready. Submit when ready.", Some(NotifyType::Info));
                                        Ok(())
                                    })
                                })),
                            }))
                            .await?;

                        if new_session_res.cancelled {
                            ui.notify("New session cancelled", Some(NotifyType::Info));
                        }

                        Ok(())
                    })
                }),
            },
        )?;

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(HandoffExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
