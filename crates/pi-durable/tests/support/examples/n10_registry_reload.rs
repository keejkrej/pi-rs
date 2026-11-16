//! Port of packages/durable/test/examples/10-registry-reload.ts
#![allow(dead_code)]

// Reload extension code through the registry.
// Run via `spec_usage::examples_smoke`.

use std::sync::Arc;

use pi_ai::types::TextContent;
use pi_ai::{Type, create_models};
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::harness::types::{ToolExecutionResult, ToolResultContentBlock};
use pi_durable::{
    Conversation, Extension, Harness, HarnessOptions, MemoryStorage, Registry, RegistryReader, ToolRegistration,
    create_registry, define_extension, define_tool, wrap_tool,
};

fn example_tool(name: &str, description: &str) -> ToolRegistration {
    let tool_name = name.to_owned();
    define_tool(ToolRegistration {
        name: name.to_owned(),
        description: description.to_owned(),
        parameters: Type::object([("path", Type::string())]).to_value(),
        replay: None,
        execution_mode: None,
        prepare_arguments: None,
        output_limits: None,
        execute: Arc::new(move |args, _api, _ctx| {
            let tool_name = tool_name.clone();
            Box::pin(async move {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                Ok(ToolExecutionResult {
                    content: Some(vec![ToolResultContentBlock::Text(TextContent::new(format!(
                        "{tool_name} {path}"
                    )))]),
                    ..Default::default()
                })
            })
        }),
    })
}

async fn tools_summary(conv: &Conversation, context: pi_chord::Context) -> pi_js::Result<Vec<String>> {
    let agent = conv.agent(context).await?;
    Ok(agent
        .tools
        .into_iter()
        .map(|tool| format!("{}: {}", tool.name, tool.description))
        .collect())
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();

    let read = example_tool("read", "Read a file");
    let registry = create_registry();
    registry.install(define_extension(Extension {
        name: "files".to_owned(),
        tools: vec![read.clone(), example_tool("grep", "Search files")],
        ..Default::default()
    }))?;

    let harness = Harness::open(
        Arc::new(MemoryStorage::new()),
        HarnessOptions {
            models: create_models(None),
            registry: Arc::clone(&registry) as Arc<dyn RegistryReader>,
            settings: None,
            env: None,
            conversation_created: None,
            now: None,
            on_report: None,
        },
        context.clone(),
    )
    .await?;
    let root = harness.root(context.clone(), None).await?;

    // Reloading is installing a new object with the same name: it replaces the old
    // one in place, in one publication, so no conversation ever sees it missing.
    // A task phase that already started keeps the snapshot it took; the next phase
    // uses the new code.
    registry.install(define_extension(Extension {
        name: "files".to_owned(),
        tools: vec![read.clone(), example_tool("grep", "Search files, faster")],
        ..Default::default()
    }))?;
    pi_js::console::log(&format!(
        "after reload: {:?}",
        tools_summary(&root, context.clone()).await?
    ));

    // Another extension may wrap a tool by name. The wrap applies where the
    // wrapping extension is selected, and survives reloads of the wrapped tool.
    let audit = define_extension(Extension {
        name: "audit".to_owned(),
        wraps: vec![wrap_tool(&read, |mut tool| {
            tool.description = format!("{} (audited)", tool.description);
            tool
        })],
        ..Default::default()
    });
    registry.install(audit.clone())?;
    pi_js::console::log(&format!(
        "with audit: {:?}",
        tools_summary(&root, context.clone()).await?
    ));

    // Uninstall removes an extension by name; conversations that select it stop
    // getting its code.
    registry.uninstall(&audit);
    pi_js::console::log(&format!(
        "after uninstall: {:?}",
        tools_summary(&root, context.clone()).await?
    ));

    harness.close(context).await?;
    Ok(())
}
