//! Port of packages/coding-agent/examples/extensions/event-bus.ts

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionContext, ExtensionFactory, InlineExtension, NotifyType, RegisteredCommandOptions,
    SessionStartEvent,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct MyNotification {
    message: String,
    from: String,
}

pub struct EventBusExtension;

#[async_trait]
impl Extension for EventBusExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let current_ctx: Arc<Mutex<Option<ExtensionContext>>> = Arc::new(Mutex::new(None));

        {
            let ctx_ref = Arc::clone(&current_ctx);
            pi.on::<SessionStartEvent>(move |_event, ctx| {
                let ctx_ref = Arc::clone(&ctx_ref);
                Box::pin(async move {
                    let mut guard = ctx_ref.lock().unwrap_or_else(|e| e.into_inner());
                    *guard = Some(ctx);
                    Ok(None)
                })
            });
        }

        {
            let ctx_ref = Arc::clone(&current_ctx);
            let _unsub = pi.events().on_sync(
                "my:notification",
                Arc::new(move |data| {
                    let ctx_opt = ctx_ref.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    if let Some(ctx) = ctx_opt
                        && let Ok(notif) = serde_json::from_value::<MyNotification>(data)
                        && let Ok(ui) = ctx.ui()
                    {
                        ui.notify(
                            &format!("Event from {}: {}", notif.from, notif.message),
                            Some(NotifyType::Info),
                        );
                    }
                }),
            );
        }

        {
            let pi_cmd = pi.clone();
            pi.register_command(
                "emit",
                RegisteredCommandOptions {
                    description: Some("Emit my:notification event (usage: /emit message)".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |args, _ctx| {
                        let pi = pi_cmd.clone();
                        Box::pin(async move {
                            let trimmed = args.trim();
                            let message = if trimmed.is_empty() { "hello" } else { trimmed };
                            pi.events().emit(
                                "my:notification",
                                serde_json::json!({
                                    "message": message,
                                    "from": "/emit command",
                                }),
                            );
                            Ok(())
                        })
                    }),
                },
            )?;
        }

        {
            let pi_ss = pi.clone();
            pi.on::<SessionStartEvent>(move |_event, _ctx| {
                let pi = pi_ss.clone();
                Box::pin(async move {
                    pi.events().emit(
                        "my:notification",
                        serde_json::json!({
                            "message": "Session started",
                            "from": "event-bus-example",
                        }),
                    );
                    Ok(None)
                })
            });
        }

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(EventBusExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
