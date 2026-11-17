//! Port of packages/coding-agent/examples/extensions/model-status.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, ModelSelectEvent, ModelSelectSource, NotifyType,
};

fn source_label(source: ModelSelectSource) -> &'static str {
    match source {
        ModelSelectSource::Set => "set",
        ModelSelectSource::Cycle => "cycle",
        ModelSelectSource::Restore => "restore",
    }
}

pub struct ModelStatusExtension;

#[async_trait]
impl Extension for ModelStatusExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.on::<ModelSelectEvent>(Arc::new(|event, ctx| {
            Box::pin(async move {
                let next = format!("{}/{}", event.model.provider, event.model.id);
                let prev = event
                    .previous_model
                    .as_ref()
                    .map(|m| format!("{}/{}", m.provider, m.id))
                    .unwrap_or_else(|| "none".to_string());

                let ui = ctx.ui()?;
                if event.source != ModelSelectSource::Restore {
                    ui.notify(&format!("Model: {next}"), Some(NotifyType::Info));
                }

                ui.set_status("model", Some(&format!("🤖 {}", event.model.id)));

                pi_js::console::log(&format!(
                    "[model_select] {prev} → {next} ({})",
                    source_label(event.source)
                ));
                Ok(None)
            })
        }));

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(ModelStatusExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
