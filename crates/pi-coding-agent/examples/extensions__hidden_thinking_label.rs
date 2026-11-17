//! Port of packages/coding-agent/examples/extensions/hidden-thinking-label.ts

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, RegisteredCommandOptions, SessionStartEvent,
};

const DEFAULT_LABEL: &str = "Pondering...";

pub struct HiddenThinkingLabelExtension;

#[async_trait]
impl Extension for HiddenThinkingLabelExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let label = Arc::new(Mutex::new(DEFAULT_LABEL.to_string()));

        {
            let label_ss = Arc::clone(&label);
            pi.on::<SessionStartEvent>(move |_event, ctx| {
                let label = Arc::clone(&label_ss);
                Box::pin(async move {
                    let current = label.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    ctx.ui()?.set_hidden_thinking_label(Some(&current));
                    Ok(None)
                })
            });
        }

        {
            let label_cmd = Arc::clone(&label);
            pi.register_command(
                "thinking-label",
                RegisteredCommandOptions {
                    description: Some("Set the hidden thinking label. Use without args to reset.".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |args, ctx| {
                        let label = Arc::clone(&label_cmd);
                        Box::pin(async move {
                            let next_label = args.trim().to_string();
                            let ui = ctx.as_context().ui()?;

                            if next_label.is_empty() {
                                {
                                    let mut guard = label.lock().unwrap_or_else(|e| e.into_inner());
                                    *guard = DEFAULT_LABEL.to_string();
                                }
                                ui.set_hidden_thinking_label(None);
                                ui.notify(&format!("Hidden thinking label reset to: {DEFAULT_LABEL}"), None);
                                return Ok(());
                            }

                            {
                                let mut guard = label.lock().unwrap_or_else(|e| e.into_inner());
                                *guard = next_label.clone();
                            }
                            ui.set_hidden_thinking_label(Some(&next_label));
                            ui.notify(&format!("Hidden thinking label set to: {next_label}"), None);
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
    Arc::new(HiddenThinkingLabelExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
