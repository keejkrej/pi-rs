//! Port of packages/coding-agent/examples/extensions/permission-gate.ts

use std::process::ExitCode;
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, ToolCallEvent, ToolCallEventResult,
};

static DANGEROUS_PATTERNS: LazyLock<Vec<regex::Regex>> = LazyLock::new(|| {
    [
        r"(?i)\brm\s+(-rf?|--recursive)",
        r"(?i)\bsudo\b",
        r"(?i)\b(chmod|chown)\b.*777",
    ]
    .into_iter()
    .filter_map(|p| regex::Regex::new(p).ok())
    .collect()
});

pub struct PermissionGateExtension;

#[async_trait]
impl Extension for PermissionGateExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.on::<ToolCallEvent>(|event, ctx| {
            Box::pin(async move {
                let ToolCallEvent::Bash(bash_evt) = event else {
                    return Ok(None);
                };
                let command = bash_evt.input.command;
                let is_dangerous = DANGEROUS_PATTERNS.iter().any(|p| p.is_match(&command));
                if !is_dangerous {
                    return Ok(None);
                }

                if !ctx.has_ui()? {
                    return Ok(Some(ToolCallEventResult {
                        block: Some(true),
                        reason: Some("Dangerous command blocked (no UI for confirmation)".to_string()),
                        terminate: None,
                    }));
                }

                let choice = ctx
                    .ui()?
                    .select(
                        &format!("⚠️ Dangerous command:\n\n  {command}\n\nAllow?"),
                        vec!["Yes".to_string(), "No".to_string()],
                        None,
                    )
                    .await?;

                if choice.as_deref() != Some("Yes") {
                    return Ok(Some(ToolCallEventResult {
                        block: Some(true),
                        reason: Some("Blocked by user".to_string()),
                        terminate: None,
                    }));
                }

                Ok(None)
            })
        });

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(PermissionGateExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
