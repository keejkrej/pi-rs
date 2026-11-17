//! Port of packages/coding-agent/examples/extensions/working-message-test.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, SessionStartEvent, WorkingIndicatorOptions,
};

const CUSTOM_MESSAGE: &str = "\x1b[38;2;155;86;63mWorking... (custom)\x1b[39m";

pub struct WorkingMessageTestExtension;

#[async_trait]
impl Extension for WorkingMessageTestExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.on::<SessionStartEvent>(|_event, ctx| {
            Box::pin(async move {
                let ui = ctx.ui()?;
                ui.set_working_message(Some(CUSTOM_MESSAGE));
                ui.set_working_indicator(Some(WorkingIndicatorOptions {
                    frames: Some(vec!["\x1b[38;2;155;86;63m●\x1b[39m".to_string()]),
                    interval_ms: None,
                }));
                Ok(None)
            })
        });

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(WorkingMessageTestExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
