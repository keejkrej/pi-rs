//! Port of packages/coding-agent/examples/extensions/project-trust.ts

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, ExtensionMode, InlineExtension, NotifyType, ProjectTrustEventDecision,
    ProjectTrustEventResult, SessionStartEvent,
};

fn mode_str(mode: ExtensionMode) -> &'static str {
    match mode {
        ExtensionMode::Tui => "tui",
        ExtensionMode::Rpc => "rpc",
        ExtensionMode::Json => "json",
        ExtensionMode::Print => "print",
    }
}

pub struct ProjectTrustExtension;

#[async_trait]
impl Extension for ProjectTrustExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let load_counter = Arc::new(AtomicUsize::new(0));
        let load_count = load_counter.fetch_add(1, Ordering::SeqCst) + 1;

        pi.on_project_trust(Arc::new(move |event, ctx| {
            Box::pin(async move {
                ctx.ui.notify(
                    &format!(
                        "project_trust fired for {} (mode: {}, load: {load_count})",
                        event.cwd,
                        mode_str(ctx.mode)
                    ),
                    Some(NotifyType::Info),
                );

                if !ctx.has_ui {
                    return Ok(ProjectTrustEventResult {
                        trusted: ProjectTrustEventDecision::Undecided,
                        remember: None,
                    });
                }

                let choice = ctx
                    .ui
                    .select(
                        &format!("Project trust for:\n{}", event.cwd),
                        vec![
                            "Trust and remember".to_string(),
                            "Trust with note and remember".to_string(),
                            "Trust this session".to_string(),
                            "Do not trust this session".to_string(),
                            "Let built-in prompt decide".to_string(),
                        ],
                        None,
                    )
                    .await?;

                match choice.as_deref() {
                    Some("Trust with note and remember") => {
                        let note = ctx
                            .ui
                            .input("Project trust note", Some("Optional note for this demo"), None)
                            .await?;
                        let msg = match note {
                            Some(n) if !n.is_empty() => format!("Recorded demo note: {n}"),
                            _ => "No demo note entered".to_string(),
                        };
                        ctx.ui.notify(&msg, Some(NotifyType::Info));
                        Ok(ProjectTrustEventResult {
                            trusted: ProjectTrustEventDecision::Yes,
                            remember: Some(true),
                        })
                    }
                    Some("Trust and remember") => Ok(ProjectTrustEventResult {
                        trusted: ProjectTrustEventDecision::Yes,
                        remember: Some(true),
                    }),
                    Some("Trust this session") => Ok(ProjectTrustEventResult {
                        trusted: ProjectTrustEventDecision::Yes,
                        remember: None,
                    }),
                    Some("Do not trust this session") => Ok(ProjectTrustEventResult {
                        trusted: ProjectTrustEventDecision::No,
                        remember: None,
                    }),
                    _ => Ok(ProjectTrustEventResult {
                        trusted: ProjectTrustEventDecision::Undecided,
                        remember: None,
                    }),
                }
            })
        }));

        pi.on::<SessionStartEvent>(|_event, ctx| {
            Box::pin(async move {
                ctx.ui()?.notify(
                    &format!("project-trust example loaded after trust resolution in {}", ctx.cwd()?),
                    Some(NotifyType::Info),
                );
                Ok(None)
            })
        });

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(ProjectTrustExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
