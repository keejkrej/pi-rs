//! Port of packages/coding-agent/examples/extensions/provider-payload.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_coding_agent::config::CONFIG_DIR_NAME;
use pi_coding_agent::core::extensions::types::{
    AfterProviderResponseEvent, BeforeProviderRequestEvent, Extension, ExtensionApi, ExtensionFactory, InlineExtension,
};

pub struct ProviderPayloadExtension;

#[async_trait]
impl Extension for ProviderPayloadExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.on::<BeforeProviderRequestEvent>(Arc::new(|event, ctx| {
            Box::pin(async move {
                let log_file = pi_js::path::join(&[&ctx.cwd()?, CONFIG_DIR_NAME, "provider-payload.log"]);
                let formatted = pi_js::json::stringify_pretty(&event.payload, 2)?;
                let _ = pi_js::fs::append_file(&log_file, format!("{formatted}\n\n"));
                Ok(None)
            })
        }));

        pi.on::<AfterProviderResponseEvent>(Arc::new(|event, ctx| {
            Box::pin(async move {
                let log_file = pi_js::path::join(&[&ctx.cwd()?, CONFIG_DIR_NAME, "provider-payload.log"]);
                let headers_json = pi_js::json::stringify(&event.headers)?;
                let _ = pi_js::fs::append_file(&log_file, format!("[{}] {headers_json}\n\n", event.status));
                Ok(None)
            })
        }));

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(ProviderPayloadExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
