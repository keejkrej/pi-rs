//! Port of packages/coding-agent/examples/extensions/working-indicator.ts

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionContext, ExtensionFactory, InlineExtension, NotifyType, RegisteredCommandOptions,
    SessionStartEvent, WorkingIndicatorOptions,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkingIndicatorMode {
    Dot,
    None,
    Pulse,
    Spinner,
    Default,
}

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const PASTEL_RAINBOW: &[&str] = &[
    "\x1b[38;2;255;179;186m",
    "\x1b[38;2;255;223;186m",
    "\x1b[38;2;255;255;186m",
    "\x1b[38;2;186;255;201m",
    "\x1b[38;2;186;225;255m",
    "\x1b[38;2;218;186;255m",
];
const RESET_FG: &str = "\x1b[39m";

fn colorize(text: &str, color: &str) -> String {
    format!("{color}{text}{RESET_FG}")
}

fn get_indicator(mode: WorkingIndicatorMode) -> Option<WorkingIndicatorOptions> {
    match mode {
        WorkingIndicatorMode::Dot => Some(WorkingIndicatorOptions {
            frames: Some(vec![colorize("●", PASTEL_RAINBOW[0])]),
            interval_ms: None,
        }),
        WorkingIndicatorMode::None => Some(WorkingIndicatorOptions {
            frames: Some(Vec::new()),
            interval_ms: None,
        }),
        WorkingIndicatorMode::Pulse => Some(WorkingIndicatorOptions {
            frames: Some(vec![
                colorize("·", PASTEL_RAINBOW[0]),
                colorize("•", PASTEL_RAINBOW[2]),
                colorize("●", PASTEL_RAINBOW[4]),
                colorize("•", PASTEL_RAINBOW[5]),
            ]),
            interval_ms: Some(120),
        }),
        WorkingIndicatorMode::Spinner => Some(WorkingIndicatorOptions {
            frames: Some(
                SPINNER_FRAMES
                    .iter()
                    .enumerate()
                    .map(|(idx, frame)| colorize(frame, PASTEL_RAINBOW[idx % PASTEL_RAINBOW.len()]))
                    .collect(),
            ),
            interval_ms: Some(80),
        }),
        WorkingIndicatorMode::Default => None,
    }
}

fn describe_mode(mode: WorkingIndicatorMode) -> &'static str {
    match mode {
        WorkingIndicatorMode::Dot => "static dot",
        WorkingIndicatorMode::None => "hidden",
        WorkingIndicatorMode::Pulse => "custom pulse",
        WorkingIndicatorMode::Spinner => "custom spinner",
        WorkingIndicatorMode::Default => "pi default spinner",
    }
}

fn apply_indicator(ctx: &ExtensionContext, mode: WorkingIndicatorMode) -> pi_js::Result<()> {
    let ui = ctx.ui()?;
    ui.set_working_indicator(get_indicator(mode));
    let theme = ui.theme();
    ui.set_status(
        "working-indicator",
        Some(&theme.fg("dim", &format!("Indicator: {}", describe_mode(mode)))),
    );
    Ok(())
}

pub struct WorkingIndicatorExtension;

#[async_trait]
impl Extension for WorkingIndicatorExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let mode = Arc::new(Mutex::new(WorkingIndicatorMode::Spinner));

        {
            let mode_ss = Arc::clone(&mode);
            pi.on::<SessionStartEvent>(move |_event, ctx| {
                let mode_ref = Arc::clone(&mode_ss);
                Box::pin(async move {
                    let current = *mode_ref.lock().unwrap_or_else(|e| e.into_inner());
                    apply_indicator(&ctx, current)?;
                    Ok(None)
                })
            });
        }

        {
            let mode_cmd = Arc::clone(&mode);
            pi.register_command(
                "working-indicator",
                RegisteredCommandOptions {
                    description: Some(
                        "Set the streaming working indicator: dot, pulse, none, spinner, or reset.".to_string(),
                    ),
                    get_argument_completions: None,
                    handler: Arc::new(move |args, ctx| {
                        let mode_ref = Arc::clone(&mode_cmd);
                        Box::pin(async move {
                            let ui = ctx.as_context().ui()?;
                            let next_str = args.trim().to_lowercase();
                            if next_str.is_empty() {
                                let current = *mode_ref.lock().unwrap_or_else(|e| e.into_inner());
                                ui.notify(
                                    &format!("Working indicator: {}", describe_mode(current)),
                                    Some(NotifyType::Info),
                                );
                                return Ok(());
                            }

                            let next_mode = match next_str.as_str() {
                                "dot" => WorkingIndicatorMode::Dot,
                                "none" => WorkingIndicatorMode::None,
                                "pulse" => WorkingIndicatorMode::Pulse,
                                "spinner" => WorkingIndicatorMode::Spinner,
                                "reset" => WorkingIndicatorMode::Default,
                                _ => {
                                    ui.notify(
                                        "Usage: /working-indicator [dot|pulse|none|spinner|reset]",
                                        Some(NotifyType::Error),
                                    );
                                    return Ok(());
                                }
                            };

                            {
                                let mut guard = mode_ref.lock().unwrap_or_else(|e| e.into_inner());
                                *guard = next_mode;
                            }
                            apply_indicator(ctx.as_context(), next_mode)?;
                            ui.notify(
                                &format!("Working indicator set to: {}", describe_mode(next_mode)),
                                Some(NotifyType::Info),
                            );
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
    Arc::new(WorkingIndicatorExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
