//! Port of packages/coding-agent/examples/extensions/dirty-repo-guard.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionContext, ExtensionFactory, InlineExtension, NotifyType, SessionBeforeForkEvent,
    SessionBeforeForkResult, SessionBeforeSwitchEvent, SessionBeforeSwitchReason, SessionBeforeSwitchResult,
};

async fn check_dirty_repo(pi: &ExtensionApi, ctx: &ExtensionContext, action: &str) -> pi_js::Result<bool> {
    let exec_res = pi
        .exec("git", &["status".to_string(), "--porcelain".to_string()], None)
        .await?;
    if exec_res.code != 0 {
        return Ok(false);
    }

    let trimmed = exec_res.stdout.trim();
    if trimmed.is_empty() {
        return Ok(false);
    }

    if !ctx.has_ui()? {
        return Ok(true);
    }

    let changed_files = trimmed.lines().filter(|l| !l.is_empty()).count();
    let ui = ctx.ui()?;
    let choice = ui
        .select(
            &format!("You have {changed_files} uncommitted file(s). {action} anyway?"),
            vec!["Yes, proceed anyway".to_string(), "No, let me commit first".to_string()],
            None,
        )
        .await?;

    if choice.as_deref() != Some("Yes, proceed anyway") {
        ui.notify("Commit your changes first", Some(NotifyType::Warning));
        return Ok(true);
    }

    Ok(false)
}

pub struct DirtyRepoGuardExtension;

#[async_trait]
impl Extension for DirtyRepoGuardExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        {
            let pi_sw = pi.clone();
            pi.on::<SessionBeforeSwitchEvent>(Arc::new(move |event, ctx| {
                let pi = pi_sw.clone();
                Box::pin(async move {
                    let action = if event.reason == SessionBeforeSwitchReason::New {
                        "new session"
                    } else {
                        "switch session"
                    };
                    if check_dirty_repo(&pi, &ctx, action).await? {
                        return Ok(Some(SessionBeforeSwitchResult { cancel: Some(true) }));
                    }
                    Ok(None)
                })
            }));
        }

        {
            let pi_fk = pi.clone();
            pi.on::<SessionBeforeForkEvent>(Arc::new(move |_event, ctx| {
                let pi = pi_fk.clone();
                Box::pin(async move {
                    if check_dirty_repo(&pi, &ctx, "fork").await? {
                        return Ok(Some(SessionBeforeForkResult {
                            cancel: Some(true),
                            skip_conversation_restore: None,
                        }));
                    }
                    Ok(None)
                })
            }));
        }

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(DirtyRepoGuardExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
