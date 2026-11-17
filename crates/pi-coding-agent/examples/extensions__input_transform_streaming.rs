//! Port of packages/coding-agent/examples/extensions/input-transform-streaming.ts

use std::process::ExitCode;
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, InputDeliveryBehavior, InputEvent, InputEventResult,
};

static TRIGGER_RE: LazyLock<Option<regex::Regex>> =
    LazyLock::new(|| regex::Regex::new(r"(?i)\b(changes?|diff|modified)\b").ok());

pub struct InputTransformStreamingExtension;

#[async_trait]
impl Extension for InputTransformStreamingExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let pi_in = pi.clone();
        pi.on::<InputEvent>(Arc::new(move |event, _ctx| {
            let pi = pi_in.clone();
            Box::pin(async move {
                if event.streaming_behavior == Some(InputDeliveryBehavior::Steer) {
                    return Ok(Some(InputEventResult::Continue));
                }

                let matches = TRIGGER_RE.as_ref().is_some_and(|re| re.is_match(&event.text));
                if !matches {
                    return Ok(Some(InputEventResult::Continue));
                }

                let res = pi
                    .exec("git", &["diff".to_string(), "--stat".to_string()], None)
                    .await?;
                if res.code != 0 || res.stdout.trim().is_empty() {
                    return Ok(Some(InputEventResult::Continue));
                }

                Ok(Some(InputEventResult::Transform {
                    text: format!(
                        "{}\n\nCurrent uncommitted changes:\n```\n{}\n```",
                        event.text,
                        res.stdout.trim()
                    ),
                    images: None,
                }))
            })
        }));

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(InputTransformStreamingExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
