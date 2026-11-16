//! Port of packages/durable/test/examples/07-configuration.ts
#![allow(dead_code)]

// Per-conversation agent choices and Harness-wide settings.
// Run via `spec_usage::examples_smoke`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use indexmap::IndexMap;
use pi_ai::types::{ModelThinkingLevel, TextContent};
use pi_ai::{Type, create_models};
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::harness::types::{ToolExecutionMode, ToolExecutionResult, ToolResultContentBlock};
use pi_durable::{
    AGENT_DOC, AgentChange, AgentExtensionsChange, AgentToolsChange, Conversation, ConversationStreamOptions,
    Extension, FieldChange, Harness, HarnessOptions, HarnessSettings, HarnessSettingsSnapshot, MemoryStorage, ModelRef,
    PartialRetryPolicy, Registry, RegistryReader, ToolRegistration, create_registry, define_extension, define_tool,
    section,
};

struct ExampleSettings {
    timeout_ms: Arc<AtomicU64>,
}

impl HarnessSettings for ExampleSettings {
    fn get(&self) -> HarnessSettingsSnapshot {
        HarnessSettingsSnapshot {
            stream: Some(ConversationStreamOptions {
                timeout_ms: Some(self.timeout_ms.load(Ordering::Relaxed)),
                ..Default::default()
            }),
            retry: Some(PartialRetryPolicy {
                max_retries: Some(5),
                ..Default::default()
            }),
            tool_execution: Some(ToolExecutionMode::Sequential),
            ..Default::default()
        }
    }
}

fn example_tool(name: &str, description: &str) -> (ToolRegistration, String) {
    let tool_name = name.to_owned();
    let tool = define_tool(ToolRegistration {
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
    });
    let snippet = format!("Use {name} for files.");
    (tool, snippet)
}

async fn tool_names(conv: &Conversation, context: pi_chord::Context) -> pi_js::Result<Vec<String>> {
    let agent = conv.agent(context).await?;
    Ok(agent.tools.into_iter().map(|tool| tool.name).collect())
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();

    // PORT: TS extends ToolRegistration with `& { snippet?: string }`; in Rust
    // `ToolRegistration` is a fixed struct, so per-tool snippets live in a lookup map.
    let (read, read_snippet) = example_tool("read", "Read a file");
    let (write, write_snippet) = example_tool("write", "Write a file");
    let (grep, grep_snippet) = example_tool("grep", "Search files");

    let mut snippets = IndexMap::new();
    snippets.insert(read.name.clone(), read_snippet);
    snippets.insert(write.name.clone(), write_snippet);
    snippets.insert(grep.name.clone(), grep_snippet);
    let snippets = Arc::new(snippets);

    let files = define_extension(Extension {
        name: "files".to_owned(),
        tools: vec![read.clone(), write.clone()],
        ..Default::default()
    });
    let search = define_extension(Extension {
        name: "search".to_owned(),
        tools: vec![grep],
        ..Default::default()
    });
    // Sections see the offered tools and can render their snippets.
    let snippets_c = Arc::clone(&snippets);
    let snippets_ext = define_extension(Extension {
        name: "snippets".to_owned(),
        sections: vec![section(
            "tool_snippets",
            Arc::new(move |input, _ctx| {
                let snippets = Arc::clone(&snippets_c);
                Box::pin(async move {
                    let rendered = input
                        .agent
                        .tools
                        .iter()
                        .filter_map(|tool| snippets.get(&tool.name).cloned())
                        .collect::<Vec<_>>()
                        .join("\n");
                    Ok(Some(rendered))
                })
            }),
        )],
        ..Default::default()
    });

    let registry = create_registry();
    registry.install(files.clone())?;
    registry.install(search.clone())?;
    registry.install(snippets_ext)?;

    // Settings are Harness-wide run policy, read at every use and never stored.
    // A trait impl makes a value live, such as one backed by the app's settings file.
    let timeout_ms = Arc::new(AtomicU64::new(60_000));
    let settings: Arc<dyn HarnessSettings> = Arc::new(ExampleSettings {
        timeout_ms: Arc::clone(&timeout_ms),
    });

    let harness = Harness::open(
        Arc::new(MemoryStorage::new()),
        HarnessOptions {
            models: create_models(None),
            registry: Arc::clone(&registry) as Arc<dyn RegistryReader>,
            settings: Some(settings),
            env: None,
            conversation_created: None,
            now: None,
            on_report: None,
        },
        context.clone(),
    )
    .await?;
    let root = harness.root(context.clone(), None).await?;

    // A new conversation stores no choices and follows the host: every installed
    // extension, all of their tools.
    pi_js::console::log(&format!(
        "default tools: {:?}",
        tool_names(&root, context.clone()).await?
    ));

    // configure() stores choices in the conversation's pi.agent document, one
    // commit per call. Extensions and tools are passed as values and stored by
    // name, so a typo cannot slip in.
    root.configure(
        AgentChange {
            model: FieldChange::Set(ModelRef {
                provider: "anthropic".to_owned(),
                model_id: "claude-sonnet-4-5".to_owned(),
            }),
            thinking_level: FieldChange::Set(ModelThinkingLevel::High),
            tools: FieldChange::Set(AgentToolsChange::replace(&[write, read])),
            ..Default::default()
        },
        context.clone(),
    )
    .await?;

    let stored = harness
        .snapshot_conversation(&*AGENT_DOC, root.id(), context.clone())
        .await?;
    pi_js::console::log(&format!("stored: {stored:?}"));
    let agent = root.agent(context.clone()).await?;
    pi_js::console::log(&format!(
        "model: {:?} thinking: {:?} tools: {:?}",
        agent.model,
        agent.thinking_level,
        tool_names(&root, context.clone()).await?
    ));

    // Deselect an extension; FieldChange::Clear clears a stored field back to the host default.
    root.configure(
        AgentChange {
            extensions: FieldChange::Set(AgentExtensionsChange::Patch {
                add: None,
                remove: Some(vec![search.name.clone()]),
            }),
            tools: FieldChange::Clear,
            ..Default::default()
        },
        context.clone(),
    )
    .await?;
    pi_js::console::log(&format!(
        "without search: {:?}",
        tool_names(&root, context.clone()).await?
    ));

    // Stored names outlive the code. The conversation selects exactly these two
    // extensions; uninstalling "files" leaves it selected, and requests just stop
    // offering its tools until it is back.
    root.configure(
        AgentChange {
            extensions: FieldChange::Set(AgentExtensionsChange::replace(&[files.clone(), search])),
            ..Default::default()
        },
        context.clone(),
    )
    .await?;
    registry.uninstall(&files);
    pi_js::console::log(&format!(
        "files uninstalled: {:?}",
        tool_names(&root, context.clone()).await?
    ));
    registry.install(files)?;
    pi_js::console::log(&format!(
        "files reinstalled: {:?}",
        tool_names(&root, context.clone()).await?
    ));

    // Settings changes need no commit; the next request uses the new timeout.
    timeout_ms.store(120_000, Ordering::Relaxed);

    harness.close(context).await?;
    Ok(())
}
