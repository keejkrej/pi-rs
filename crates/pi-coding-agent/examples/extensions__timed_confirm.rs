//! Port of packages/coding-agent/examples/extensions/timed-confirm.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, ExtensionUIDialogOptions, InlineExtension, NotifyType,
    RegisteredCommandOptions,
};
use pi_js::abort::AbortController;
use pi_js::time::set_timeout;

pub struct TimedConfirmExtension;

#[async_trait]
impl Extension for TimedConfirmExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.register_command(
            "timed",
            RegisteredCommandOptions {
                description: Some("Show a timed confirmation dialog (auto-cancels in 5s with countdown)".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| {
                    Box::pin(async move {
                        let ui = ctx.as_context().ui()?;
                        let confirmed = ui
                            .confirm(
                                "Timed Confirmation",
                                "This dialog will auto-cancel in 5 seconds. Confirm?",
                                Some(ExtensionUIDialogOptions {
                                    signal: None,
                                    timeout: Some(5000),
                                }),
                            )
                            .await?;

                        if confirmed {
                            ui.notify("Confirmed by user!", Some(NotifyType::Info));
                        } else {
                            ui.notify("Cancelled or timed out", Some(NotifyType::Info));
                        }
                        Ok(())
                    })
                }),
            },
        )?;

        pi.register_command(
            "timed-select",
            RegisteredCommandOptions {
                description: Some("Show a timed select dialog (auto-cancels in 10s with countdown)".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| {
                    Box::pin(async move {
                        let ui = ctx.as_context().ui()?;
                        let choice = ui
                            .select(
                                "Pick an option",
                                vec!["Option A".to_string(), "Option B".to_string(), "Option C".to_string()],
                                Some(ExtensionUIDialogOptions {
                                    signal: None,
                                    timeout: Some(10000),
                                }),
                            )
                            .await?;

                        match choice {
                            Some(c) => ui.notify(&format!("Selected: {c}"), Some(NotifyType::Info)),
                            None => ui.notify("Selection cancelled or timed out", Some(NotifyType::Info)),
                        }
                        Ok(())
                    })
                }),
            },
        )?;

        pi.register_command(
            "timed-signal",
            RegisteredCommandOptions {
                description: Some("Show a timed confirm using AbortSignal (manual approach)".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| {
                    Box::pin(async move {
                        let ui = ctx.as_context().ui()?;
                        let controller = Arc::new(AbortController::new());
                        let ctrl_abort = Arc::clone(&controller);
                        let timeout = set_timeout(5000, move || {
                            ctrl_abort.abort(None);
                        });
                        let signal = controller.signal().clone();

                        ui.notify("Dialog will auto-cancel in 5 seconds...", Some(NotifyType::Info));

                        let confirmed = ui
                            .confirm(
                                "Timed Confirmation",
                                "This dialog will auto-cancel in 5 seconds. Confirm?",
                                Some(ExtensionUIDialogOptions {
                                    signal: Some(signal.clone()),
                                    timeout: None,
                                }),
                            )
                            .await?;

                        timeout.clear();

                        if confirmed {
                            ui.notify("Confirmed by user!", Some(NotifyType::Info));
                        } else if signal.aborted() {
                            ui.notify("Dialog timed out (auto-cancelled)", Some(NotifyType::Warning));
                        } else {
                            ui.notify("Cancelled by user", Some(NotifyType::Info));
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
    Arc::new(TimedConfirmExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
