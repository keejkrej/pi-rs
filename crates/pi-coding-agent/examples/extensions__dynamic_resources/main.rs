//! Port of packages/coding-agent/examples/extensions/dynamic-resources/index.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, ResourcesDiscoverEvent, ResourcesDiscoverResult,
};

pub struct DynamicResourcesExtension;

#[async_trait]
impl Extension for DynamicResourcesExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let base_dir = pi_js::path::join(&[env!("CARGO_MANIFEST_DIR"), "examples", "extensions__dynamic_resources"]);
        let skill_path = pi_js::path::join(&[&base_dir, "SKILL.md"]);
        let prompt_path = pi_js::path::join(&[&base_dir, "dynamic.md"]);
        let theme_path = pi_js::path::join(&[&base_dir, "dynamic.json"]);

        pi.on::<ResourcesDiscoverEvent>(Arc::new(move |_event, _ctx| {
            let skill_path = skill_path.clone();
            let prompt_path = prompt_path.clone();
            let theme_path = theme_path.clone();
            Box::pin(async move {
                Ok(Some(ResourcesDiscoverResult {
                    skill_paths: Some(vec![skill_path]),
                    prompt_paths: Some(vec![prompt_path]),
                    theme_paths: Some(vec![theme_path]),
                }))
            })
        }));

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(DynamicResourcesExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
