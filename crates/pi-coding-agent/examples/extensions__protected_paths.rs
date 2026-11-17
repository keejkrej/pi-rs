//! Port of packages/coding-agent/examples/extensions/protected-paths.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, NotifyType, ToolCallEvent, ToolCallEventResult,
};

const PROTECTED_PATHS: &[&str] = &[".env", ".git/", "node_modules/"];

pub struct ProtectedPathsExtension;

#[async_trait]
impl Extension for ProtectedPathsExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.on::<ToolCallEvent>(|event, ctx| {
            Box::pin(async move {
                let path = match event {
                    ToolCallEvent::Write(w) => w.input.path,
                    ToolCallEvent::Edit(e) => e.input.path,
                    _ => return Ok(None),
                };

                if PROTECTED_PATHS.iter().any(|p| path.contains(p)) {
                    if ctx.has_ui()? {
                        ctx.ui()?.notify(
                            &format!("Blocked write to protected path: {path}"),
                            Some(NotifyType::Warning),
                        );
                    }
                    return Ok(Some(ToolCallEventResult {
                        block: Some(true),
                        reason: Some(format!("Path \"{path}\" is protected")),
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
    Arc::new(ProtectedPathsExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
