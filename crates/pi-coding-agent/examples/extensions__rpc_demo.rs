//! Port of packages/coding-agent/examples/extensions/rpc-demo.ts

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, NotifyType, RegisteredCommandOptions,
    SessionBeforeSwitchEvent, SessionBeforeSwitchReason, SessionBeforeSwitchResult, SessionStartEvent,
    SessionStartReason, ToolCallEvent, ToolCallEventResult, TurnEndEvent, TurnStartEvent,
};

pub struct RpcDemoExtension;

#[async_trait]
impl Extension for RpcDemoExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let turn_count = Arc::new(AtomicUsize::new(0));

        {
            let tc = Arc::clone(&turn_count);
            pi.on::<SessionStartEvent>(move |event, ctx| {
                let tc = Arc::clone(&tc);
                Box::pin(async move {
                    let ui = ctx.ui()?;
                    let title = if event.reason == SessionStartReason::New {
                        "pi RPC Demo (new session)"
                    } else {
                        "pi RPC Demo"
                    };
                    ui.set_title(title);
                    ui.set_widget_lines(
                        "rpc-demo",
                        Some(vec![
                            "--- RPC Extension UI Demo ---".to_string(),
                            "Loaded and ready.".to_string(),
                        ]),
                        None,
                    );
                    let count = tc.load(Ordering::SeqCst);
                    ui.set_status("rpc-demo", Some(&format!("Turns: {count}")));
                    Ok(None)
                })
            });
        }

        {
            let tc = Arc::clone(&turn_count);
            pi.on::<TurnStartEvent>(move |_event, ctx| {
                let tc = Arc::clone(&tc);
                Box::pin(async move {
                    let next = tc.fetch_add(1, Ordering::SeqCst) + 1;
                    ctx.ui()?
                        .set_status("rpc-demo", Some(&format!("Turn {next} running...")));
                    Ok(None)
                })
            });
        }

        {
            let tc = Arc::clone(&turn_count);
            pi.on::<TurnEndEvent>(move |_event, ctx| {
                let tc = Arc::clone(&tc);
                Box::pin(async move {
                    let count = tc.load(Ordering::SeqCst);
                    ctx.ui()?.set_status("rpc-demo", Some(&format!("Turn {count} done")));
                    Ok(None)
                })
            });
        }

        {
            let rm_re = regex::Regex::new(r"(?i)\brm\s+(-rf?|--recursive)").ok();
            let sudo_re = regex::Regex::new(r"(?i)\bsudo\b").ok();
            pi.on::<ToolCallEvent>(move |event, ctx| {
                let rm_re = rm_re.clone();
                let sudo_re = sudo_re.clone();
                Box::pin(async move {
                    let ToolCallEvent::Bash(bash_evt) = event else {
                        return Ok(None);
                    };
                    let command = bash_evt.input.command;
                    let is_dangerous = rm_re.as_ref().is_some_and(|r| r.is_match(&command))
                        || sudo_re.as_ref().is_some_and(|r| r.is_match(&command));
                    if is_dangerous {
                        if !ctx.has_ui()? {
                            return Ok(Some(ToolCallEventResult {
                                block: Some(true),
                                reason: Some("Dangerous command blocked (no UI)".to_string()),
                                terminate: None,
                            }));
                        }
                        let ui = ctx.ui()?;
                        let choice = ui
                            .select(
                                &format!("Dangerous command: {command}"),
                                vec!["Allow".to_string(), "Block".to_string()],
                                None,
                            )
                            .await?;
                        if choice.as_deref() != Some("Allow") {
                            ui.notify("Command blocked by user", Some(NotifyType::Warning));
                            return Ok(Some(ToolCallEventResult {
                                block: Some(true),
                                reason: Some("Blocked by user".to_string()),
                                terminate: None,
                            }));
                        }
                        ui.notify("Command allowed", Some(NotifyType::Info));
                    }
                    Ok(None)
                })
            });
        }

        pi.on::<SessionBeforeSwitchEvent>(|event, ctx| {
            Box::pin(async move {
                if event.reason != SessionBeforeSwitchReason::New || !ctx.has_ui()? {
                    return Ok(None);
                }
                let ui = ctx.ui()?;
                let confirmed = ui.confirm("Clear session?", "All messages will be lost.", None).await?;
                if !confirmed {
                    ui.notify("Clear cancelled", Some(NotifyType::Info));
                    return Ok(Some(SessionBeforeSwitchResult { cancel: Some(true) }));
                }
                Ok(None)
            })
        });

        pi.register_command(
            "rpc-input",
            RegisteredCommandOptions {
                description: Some("Prompt for text input (demonstrates ctx.ui.input in RPC)".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| {
                    Box::pin(async move {
                        let ui = ctx.as_context().ui()?;
                        let value = ui.input("Enter a value", Some("type something..."), None).await?;
                        match value {
                            Some(v) if !v.is_empty() => ui.notify(&format!("You entered: {v}"), Some(NotifyType::Info)),
                            _ => ui.notify("Input cancelled", Some(NotifyType::Info)),
                        }
                        Ok(())
                    })
                }),
            },
        )?;

        pi.register_command(
            "rpc-editor",
            RegisteredCommandOptions {
                description: Some("Open multi-line editor (demonstrates ctx.ui.editor in RPC)".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| {
                    Box::pin(async move {
                        let ui = ctx.as_context().ui()?;
                        let text = ui.editor("Edit some text", Some("Line 1\nLine 2\nLine 3")).await?;
                        match text {
                            Some(t) if !t.is_empty() => {
                                let lines = t.split('\n').count();
                                ui.notify(&format!("Editor submitted ({lines} lines)"), Some(NotifyType::Info));
                            }
                            _ => ui.notify("Editor cancelled", Some(NotifyType::Info)),
                        }
                        Ok(())
                    })
                }),
            },
        )?;

        pi.register_command(
            "rpc-prefill",
            RegisteredCommandOptions {
                description: Some("Prefill the input editor (demonstrates ctx.ui.setEditorText in RPC)".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|_args, ctx| {
                    Box::pin(async move {
                        let ui = ctx.as_context().ui()?;
                        ui.set_editor_text("This text was set by the rpc-demo extension.");
                        ui.notify("Editor prefilled", Some(NotifyType::Info));
                        Ok(())
                    })
                }),
            },
        )?;

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(RpcDemoExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
