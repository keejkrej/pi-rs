//! Port of packages/coding-agent/examples/extensions/prompt-customizer.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    BeforeAgentStartEvent, Extension, ExtensionApi, ExtensionFactory, InlineExtension,
    NormalizedBuildSystemPromptOptions,
};

fn build_tool_guidance(options: &NormalizedBuildSystemPromptOptions) -> String {
    let has_tool = |name: &str| options.selected_tools.iter().any(|t| t == name);
    let mut rules = Vec::new();

    if has_tool("read") {
        rules.push("- Use `read` for file contents; it supports text and images.");
        rules.push("- For large files, use `offset` and `limit` to read in chunks.");
    }
    if has_tool("bash") {
        rules.push("- Use `bash` for file operations such as `ls`, `find`, and `grep`.");
    }
    if has_tool("edit") {
        rules.push("- Use `edit` for precise text replacements that match existing content exactly.");
    }
    if has_tool("write") {
        rules.push("- Use `write` to create new files or replace existing files completely.");
    }
    // Note: NormalizedBuildSystemPromptOptions in mcp_wire does not expose loaded skills metadata.

    rules.join("\n")
}

pub struct PromptCustomizerExtension;

#[async_trait]
impl Extension for PromptCustomizerExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.on::<BeforeAgentStartEvent>(Arc::new(|event, _ctx| {
            Box::pin(async move {
                let mut opts = event.system_prompt_options.lock().unwrap_or_else(|e| e.into_inner());
                let guidance = build_tool_guidance(&opts);
                if guidance.is_empty() {
                    opts.sections.shift_remove("tool_guidance");
                } else {
                    opts.sections.insert("tool_guidance".to_string(), guidance);
                }
                Ok(None)
            })
        }));

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(PromptCustomizerExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
