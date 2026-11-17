//! Port of packages/coding-agent/examples/extensions/widget-placement.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, ExtensionWidgetOptions, InlineExtension, SessionStartEvent,
    WidgetPlacement,
};

pub struct WidgetPlacementExtension;

#[async_trait]
impl Extension for WidgetPlacementExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.on::<SessionStartEvent>(|_event, ctx| {
            Box::pin(async move {
                if !ctx.has_ui()? {
                    return Ok(None);
                }
                let ui = ctx.ui()?;
                ui.set_widget_lines("widget-above", Some(vec!["Above editor widget".to_string()]), None);
                ui.set_widget_lines(
                    "widget-below",
                    Some(vec!["Below editor widget".to_string()]),
                    Some(ExtensionWidgetOptions {
                        placement: Some(WidgetPlacement::BelowEditor),
                    }),
                );
                Ok(None)
            })
        });

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(WidgetPlacementExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
