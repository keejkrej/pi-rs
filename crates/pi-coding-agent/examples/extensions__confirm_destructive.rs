//! Port of packages/coding-agent/examples/extensions/confirm-destructive.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, NotifyType, SessionBeforeForkEvent,
    SessionBeforeForkResult, SessionBeforeSwitchEvent, SessionBeforeSwitchReason, SessionBeforeSwitchResult,
};

pub struct ConfirmDestructiveExtension;

#[async_trait]
impl Extension for ConfirmDestructiveExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.on::<SessionBeforeSwitchEvent>(Arc::new(|event, ctx| {
            Box::pin(async move {
                if !ctx.has_ui()? {
                    return Ok(None);
                }
                let ui = ctx.ui()?;

                if event.reason == SessionBeforeSwitchReason::New {
                    let confirmed = ui
                        .confirm(
                            "Clear session?",
                            "This will delete all messages in the current session.",
                            None,
                        )
                        .await?;
                    if !confirmed {
                        ui.notify("Clear cancelled", Some(NotifyType::Info));
                        return Ok(Some(SessionBeforeSwitchResult { cancel: Some(true) }));
                    }
                    return Ok(None);
                }

                let entries = ctx.session_manager()?.get_entries();
                let has_unsaved_work = entries.iter().any(|e| {
                    e.r#type == "message"
                        && e.fields
                            .get("message")
                            .and_then(|m| m.get("role"))
                            .and_then(|v| v.as_str())
                            == Some("user")
                });

                if has_unsaved_work {
                    let confirmed = ui
                        .confirm(
                            "Switch session?",
                            "You have messages in the current session. Switch anyway?",
                            None,
                        )
                        .await?;
                    if !confirmed {
                        ui.notify("Switch cancelled", Some(NotifyType::Info));
                        return Ok(Some(SessionBeforeSwitchResult { cancel: Some(true) }));
                    }
                }

                Ok(None)
            })
        }));

        pi.on::<SessionBeforeForkEvent>(Arc::new(|event, ctx| {
            Box::pin(async move {
                if !ctx.has_ui()? {
                    return Ok(None);
                }
                let ui = ctx.ui()?;
                let short_id: String = event.entry_id.chars().take(8).collect();
                let choice = ui
                    .select(
                        &format!("Fork from entry {short_id}?"),
                        vec![
                            "Yes, create fork".to_string(),
                            "No, stay in current session".to_string(),
                        ],
                        None,
                    )
                    .await?;

                if choice.as_deref() != Some("Yes, create fork") {
                    ui.notify("Fork cancelled", Some(NotifyType::Info));
                    return Ok(Some(SessionBeforeForkResult {
                        cancel: Some(true),
                        skip_conversation_restore: None,
                    }));
                }

                Ok(None)
            })
        }));

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(ConfirmDestructiveExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
