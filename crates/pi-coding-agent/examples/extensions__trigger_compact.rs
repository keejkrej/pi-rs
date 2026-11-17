//! Port of packages/coding-agent/examples/extensions/trigger-compact.ts

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    CompactOptions, Extension, ExtensionApi, ExtensionContext, ExtensionFactory, InlineExtension, NotifyType,
    RegisteredCommandOptions, TurnEndEvent,
};

const COMPACT_THRESHOLD_TOKENS: i64 = 100_000;

fn trigger_compaction(ctx: &ExtensionContext, custom_instructions: Option<String>) {
    if ctx.has_ui().unwrap_or(false)
        && let Ok(ui) = ctx.ui()
    {
        ui.notify("Compaction started", Some(NotifyType::Info));
    }
    let ctx_ok = ctx.clone();
    let ctx_err = ctx.clone();
    let _ = ctx.compact(Some(CompactOptions {
        custom_instructions,
        on_complete: Some(Arc::new(move |_result| {
            if ctx_ok.has_ui().unwrap_or(false)
                && let Ok(ui) = ctx_ok.ui()
            {
                ui.notify("Compaction completed", Some(NotifyType::Info));
            }
        })),
        on_error: Some(Arc::new(move |error| {
            if ctx_err.has_ui().unwrap_or(false)
                && let Ok(ui) = ctx_err.ui()
            {
                ui.notify(&format!("Compaction failed: {error}"), Some(NotifyType::Error));
            }
        })),
    }));
}

pub struct TriggerCompactExtension;

#[async_trait]
impl Extension for TriggerCompactExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let previous_tokens: Arc<Mutex<Option<i64>>> = Arc::new(Mutex::new(None));

        {
            let prev_te = Arc::clone(&previous_tokens);
            pi.on::<TurnEndEvent>(Arc::new(move |_event, ctx| {
                let prev_ref = Arc::clone(&prev_te);
                Box::pin(async move {
                    let usage = ctx.get_context_usage()?;
                    let Some(current_tokens) = usage.and_then(|u| u.tokens) else {
                        return Ok(None);
                    };

                    let should_compact = {
                        let mut guard = prev_ref.lock().unwrap_or_else(|e| e.into_inner());
                        let crossed = guard.is_some_and(|prev| prev <= COMPACT_THRESHOLD_TOKENS);
                        *guard = Some(current_tokens);
                        crossed && current_tokens > COMPACT_THRESHOLD_TOKENS
                    };

                    if should_compact {
                        trigger_compaction(&ctx, None);
                    }
                    Ok(None)
                })
            }));
        }

        pi.register_command(
            "trigger-compact",
            RegisteredCommandOptions {
                description: Some("Trigger compaction immediately".to_string()),
                get_argument_completions: None,
                handler: Arc::new(|args, ctx| {
                    Box::pin(async move {
                        let trimmed = args.trim().to_string();
                        let instructions = if trimmed.is_empty() { None } else { Some(trimmed) };
                        trigger_compaction(ctx.as_context(), instructions);
                        Ok(())
                    })
                }),
            },
        )?;

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(TriggerCompactExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
