//! Port of packages/coding-agent/examples/extensions/input-transform.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Local;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, InputEvent, InputEventResult, InputSource, NotifyType,
};

pub struct InputTransformExtension;

#[async_trait]
impl Extension for InputTransformExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.on::<InputEvent>(|event, ctx| {
            Box::pin(async move {
                if event.source == InputSource::Extension {
                    return Ok(Some(InputEventResult::Continue));
                }

                if let Some(rest) = event.text.strip_prefix("?quick ") {
                    let query = rest.trim();
                    if query.is_empty() {
                        ctx.ui()?.notify("Usage: ?quick <question>", Some(NotifyType::Warning));
                        return Ok(Some(InputEventResult::Handled));
                    }
                    return Ok(Some(InputEventResult::Transform {
                        text: format!("Respond briefly in 1-2 sentences: {query}"),
                        images: None,
                    }));
                }

                let lower = event.text.to_lowercase();
                if lower == "ping" {
                    ctx.ui()?.notify("pong", Some(NotifyType::Info));
                    return Ok(Some(InputEventResult::Handled));
                }
                if lower == "time" {
                    let now_str = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
                    ctx.ui()?.notify(&now_str, Some(NotifyType::Info));
                    return Ok(Some(InputEventResult::Handled));
                }

                Ok(Some(InputEventResult::Continue))
            })
        });

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(InputTransformExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
