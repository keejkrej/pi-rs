//! Port of packages/coding-agent/examples/extensions/send-user-message.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_ai::types::{TextContent, UserContentBlock};
use pi_coding_agent::core::extensions::types::{
    CustomMessageContent, Extension, ExtensionApi, ExtensionFactory, InlineExtension, InputDeliveryBehavior,
    NotifyType, RegisteredCommandOptions, SendUserMessageOptions,
};

pub struct SendUserMessageExtension;

#[async_trait]
impl Extension for SendUserMessageExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        {
            let pi_ask = pi.clone();
            pi.register_command(
                "ask",
                RegisteredCommandOptions {
                    description: Some("Send a user message to the agent".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |args, ctx| {
                        let pi = pi_ask.clone();
                        Box::pin(async move {
                            let base = ctx.as_context();
                            let ui = base.ui()?;
                            if args.trim().is_empty() {
                                ui.notify("Usage: /ask <message>", Some(NotifyType::Warning));
                                return Ok(());
                            }
                            if !base.is_idle()? {
                                ui.notify(
                                    "Agent is busy. Use /steer or /followup instead.",
                                    Some(NotifyType::Warning),
                                );
                                return Ok(());
                            }
                            pi.send_user_message(CustomMessageContent::Text(args), None)
                        })
                    }),
                },
            )?;
        }

        {
            let pi_steer = pi.clone();
            pi.register_command(
                "steer",
                RegisteredCommandOptions {
                    description: Some("Send a steering message (interrupts current processing)".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |args, ctx| {
                        let pi = pi_steer.clone();
                        Box::pin(async move {
                            let base = ctx.as_context();
                            if args.trim().is_empty() {
                                base.ui()?.notify("Usage: /steer <message>", Some(NotifyType::Warning));
                                return Ok(());
                            }
                            if base.is_idle()? {
                                pi.send_user_message(CustomMessageContent::Text(args), None)
                            } else {
                                pi.send_user_message(
                                    CustomMessageContent::Text(args),
                                    Some(SendUserMessageOptions {
                                        deliver_as: Some(InputDeliveryBehavior::Steer),
                                        expand_prompt_templates: None,
                                    }),
                                )
                            }
                        })
                    }),
                },
            )?;
        }

        {
            let pi_fu = pi.clone();
            pi.register_command(
                "followup",
                RegisteredCommandOptions {
                    description: Some("Queue a follow-up message (waits for current processing)".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |args, ctx| {
                        let pi = pi_fu.clone();
                        Box::pin(async move {
                            let base = ctx.as_context();
                            let ui = base.ui()?;
                            if args.trim().is_empty() {
                                ui.notify("Usage: /followup <message>", Some(NotifyType::Warning));
                                return Ok(());
                            }
                            if base.is_idle()? {
                                pi.send_user_message(CustomMessageContent::Text(args), None)
                            } else {
                                pi.send_user_message(
                                    CustomMessageContent::Text(args),
                                    Some(SendUserMessageOptions {
                                        deliver_as: Some(InputDeliveryBehavior::FollowUp),
                                        expand_prompt_templates: None,
                                    }),
                                )?;
                                ui.notify("Follow-up queued", Some(NotifyType::Info));
                                Ok(())
                            }
                        })
                    }),
                },
            )?;
        }

        {
            let pi_aw = pi.clone();
            pi.register_command(
                "askwith",
                RegisteredCommandOptions {
                    description: Some("Send a user message with structured content".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |args, ctx| {
                        let pi = pi_aw.clone();
                        Box::pin(async move {
                            let base = ctx.as_context();
                            let ui = base.ui()?;
                            if args.trim().is_empty() {
                                ui.notify("Usage: /askwith <message>", Some(NotifyType::Warning));
                                return Ok(());
                            }
                            if !base.is_idle()? {
                                ui.notify("Agent is busy", Some(NotifyType::Warning));
                                return Ok(());
                            }
                            pi.send_user_message(
                                CustomMessageContent::Blocks(vec![
                                    UserContentBlock::Text(TextContent::new(format!("User request: {args}"))),
                                    UserContentBlock::Text(TextContent::new("Please respond concisely.")),
                                ]),
                                None,
                            )
                        })
                    }),
                },
            )?;
        }

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(SendUserMessageExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
