//! Port of packages/coding-agent/examples/extensions/pirate.ts

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    BeforeAgentStartEvent, BeforeAgentStartEventResult, Extension, ExtensionApi, ExtensionFactory, InlineExtension,
    NotifyType, RegisteredCommandOptions,
};

pub struct PirateExtension;

#[async_trait]
impl Extension for PirateExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let pirate_mode = Arc::new(AtomicBool::new(false));

        {
            let pirate_cmd = Arc::clone(&pirate_mode);
            pi.register_command(
                "pirate",
                RegisteredCommandOptions {
                    description: Some("Toggle pirate mode (agent speaks like a pirate)".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |_args, ctx| {
                        let pirate_mode = Arc::clone(&pirate_cmd);
                        Box::pin(async move {
                            let enabled = !pirate_mode.load(Ordering::SeqCst);
                            pirate_mode.store(enabled, Ordering::SeqCst);
                            let msg = if enabled {
                                "Arrr! Pirate mode enabled!"
                            } else {
                                "Pirate mode disabled"
                            };
                            ctx.as_context().ui()?.notify(msg, Some(NotifyType::Info));
                            Ok(())
                        })
                    }),
                },
            )?;
        }

        {
            let pirate_bas = Arc::clone(&pirate_mode);
            pi.on::<BeforeAgentStartEvent>(move |event, _ctx| {
                let pirate_mode = Arc::clone(&pirate_bas);
                Box::pin(async move {
                    if !pirate_mode.load(Ordering::SeqCst) {
                        return Ok(None);
                    }
                    let appendix = "\n\nIMPORTANT: You are now in PIRATE MODE. You must:\n\
                         - Speak like a stereotypical pirate in all responses\n\
                         - Use phrases like \"Arrr!\", \"Ahoy!\", \"Shiver me timbers!\", \"Avast!\", \"Ye scurvy dog!\"\n\
                         - Replace \"my\" with \"me\", \"you\" with \"ye\", \"your\" with \"yer\"\n\
                         - Refer to the user as \"matey\" or \"landlubber\"\n\
                         - End sentences with nautical expressions\n\
                         - Still complete the actual task correctly, just in pirate speak\n";
                    Ok(Some(BeforeAgentStartEventResult {
                        message: None,
                        system_prompt: Some(format!("{}{appendix}", event.system_prompt)),
                    }))
                })
            });
        }

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(PirateExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
