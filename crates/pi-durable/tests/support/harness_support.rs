//! Port of packages/durable/test/harness-support.ts
#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use indexmap::IndexMap;
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, Message, StopReason, SystemMessage, SystemMessageContent, TextContent,
    ToolCall, ToolResultMessage, Usage, UsageCost, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_ai::{Type, create_models};

use pi_durable::harness::define::{SectionOptions, define_extension, define_tool, hook, section_with_options};
use pi_durable::harness::harness::Harness;
use pi_durable::harness::registry::{Registry, RegistryImpl, create_registry};
use pi_durable::harness::types::{
    Extension, HarnessOptions, OnReportFn, PromptRenderFn, RegistryReader, ToolExecutionResult, ToolRegistration,
};
use pi_durable::tasks::{AnyTask, Task};
use pi_durable::types::Storage;

use super::session_support::context;

pub fn tool(name: &str, description: Option<&str>) -> ToolRegistration {
    define_tool(ToolRegistration {
        name: name.to_owned(),
        description: description
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("{name} tool")),
        parameters: Type::object::<_, &str>([]).to_value(),
        replay: None,
        execution_mode: None,
        prepare_arguments: None,
        output_limits: None,
        execute: Arc::new(|_, _, _| Box::pin(async { Ok(ToolExecutionResult::default()) })),
    })
}

#[derive(Clone, Default)]
pub struct OpenHarnessOptions {
    pub registry: Option<Arc<RegistryImpl>>,
    pub on_report: Option<OnReportFn>,
}

pub struct OpenedHarness {
    pub harness: Harness,
    pub registry: Arc<RegistryImpl>,
}

/// Open a Harness with a fresh registry holding the named tools.
pub async fn open_harness(
    storage: Arc<dyn Storage>,
    tool_names: &[&str],
    options: OpenHarnessOptions,
) -> pi_js::Result<OpenedHarness> {
    let registry = options.registry.unwrap_or_else(create_registry);
    if !tool_names.is_empty() {
        registry.install(define_extension(Extension {
            name: "tools".to_owned(),
            tools: tool_names.iter().map(|name| tool(name, None)).collect(),
            ..Default::default()
        }))?;
    }
    let harness = Harness::open(
        storage,
        HarnessOptions {
            models: create_models(None),
            registry: Arc::clone(&registry) as Arc<dyn RegistryReader>,
            settings: None,
            env: None,
            conversation_created: None,
            now: None,
            on_report: options.on_report,
        },
        context(),
    )
    .await?;
    Ok(OpenedHarness { harness, registry })
}

pub fn user(text: &str) -> UserMessage {
    UserMessage {
        content: UserMessageContent::Text(text.to_owned()),
        timestamp: 1,
    }
}

pub fn assistant(text: &str, calls: &[&str], stop_reason: Option<StopReason>) -> AssistantMessage {
    let tool_calls: Vec<ToolCall> = calls
        .iter()
        .map(|id| ToolCall::new(*id, format!("tool-{id}"), IndexMap::new()))
        .collect();
    let default_stop = if tool_calls.is_empty() {
        StopReason::Stop
    } else {
        StopReason::ToolUse
    };
    let mut content: Vec<AssistantContentBlock> = vec![AssistantContentBlock::Text(TextContent::new(text))];
    content.extend(tool_calls.into_iter().map(AssistantContentBlock::ToolCall));
    AssistantMessage {
        content,
        api: "faux".to_owned(),
        provider: "faux".to_owned(),
        model: "faux".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            cost: UsageCost::default(),
            ..Default::default()
        },
        stop_reason: stop_reason.unwrap_or(default_stop),
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 2,
        duration_ms: None,
    }
}

pub fn tool_result(id: &str, text: Option<&str>) -> ToolResultMessage {
    let body = text.map(ToOwned::to_owned).unwrap_or_else(|| format!("result {id}"));
    ToolResultMessage {
        tool_call_id: id.to_owned(),
        tool_name: format!("tool-{id}"),
        content: vec![UserContentBlock::Text(TextContent::new(body))],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 3,
        duration_ms: None,
    }
}

pub fn system(sections: IndexMap<String, Option<String>>) -> SystemMessage {
    SystemMessage {
        content: SystemMessageContent::Text(String::new()),
        sections: Some(sections),
        tools_added: None,
        tools_removed: None,
        timestamp: 4,
    }
}

/// Compact message rendering for assertions.
pub fn describe_message(message: &Message) -> String {
    match message {
        Message::User(u) => {
            let text = match &u.content {
                UserMessageContent::Text(s) => s.as_str(),
                UserMessageContent::Blocks(blocks) => blocks
                    .iter()
                    .find_map(|b| match b {
                        UserContentBlock::Text(t) => Some(t.text.as_str()),
                        UserContentBlock::Image(_) => None,
                    })
                    .unwrap_or(""),
            };
            format!("user:{text}")
        }
        Message::Assistant(a) => {
            let text = a
                .content
                .iter()
                .find_map(|c| match c {
                    AssistantContentBlock::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .unwrap_or("");
            format!("assistant:{text}")
        }
        Message::ToolResult(r) => {
            let body = if r.is_error {
                "error"
            } else {
                r.content
                    .iter()
                    .find_map(|c| match c {
                        UserContentBlock::Text(t) => Some(t.text.as_str()),
                        UserContentBlock::Image(_) => None,
                    })
                    .unwrap_or("")
            };
            format!("result:{}:{body}", r.tool_call_id)
        }
        Message::System(s) => {
            let keys = s
                .sections
                .as_ref()
                .map(|sec| sec.keys().map(String::as_str).collect::<Vec<_>>().join(","))
                .unwrap_or_default();
            format!("system:{keys}")
        }
    }
}

/// Uninstalls what one of the helpers below installed.
#[derive(Clone)]
pub struct Installed {
    registry: Arc<RegistryImpl>,
    extension: Extension,
}

impl Installed {
    pub fn dispose(&self) {
        self.registry.uninstall(&self.extension);
    }
}

fn install_one(registry: &Arc<RegistryImpl>, extension: Extension) -> pi_js::Result<Installed> {
    registry.install(extension.clone())?;
    Ok(Installed {
        registry: Arc::clone(registry),
        extension,
    })
}

/// Install a one-tool extension named after the tool.
pub fn add_tool(registry: &Arc<RegistryImpl>, tool: ToolRegistration, name: Option<&str>) -> pi_js::Result<Installed> {
    let ext_name = name
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("tool:{}", tool.name));
    install_one(
        registry,
        define_extension(Extension {
            name: ext_name,
            tools: vec![tool],
            ..Default::default()
        }),
    )
}

/// Install a one-task extension named after the task.
pub fn add_task(registry: &Arc<RegistryImpl>, task: AnyTask, name: Option<&str>) -> pi_js::Result<Installed> {
    let ext_name = name
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("task:{}", task.name()));
    install_one(
        registry,
        define_extension(Extension {
            name: ext_name,
            tasks: vec![task],
            ..Default::default()
        }),
    )
}

static HOOK_EXTENSIONS: AtomicUsize = AtomicUsize::new(0);

/// Install an extension with one hook registration for `task`.
pub fn add_hooks<I, S, R, H: Send + Sync + 'static>(
    registry: &Arc<RegistryImpl>,
    task: &Task<I, S, R, H>,
    handlers: H,
    name: Option<&str>,
) -> pi_js::Result<Installed> {
    let ext_name = name.map(ToOwned::to_owned).unwrap_or_else(|| {
        let id = HOOK_EXTENSIONS.fetch_add(1, Ordering::SeqCst) + 1;
        format!("hooks:{id}")
    });
    install_one(
        registry,
        define_extension(Extension {
            name: ext_name,
            hooks: vec![hook(task, handlers)],
            ..Default::default()
        }),
    )
}

/// Install a one-section extension named after the section.
pub fn add_section(
    registry: &Arc<RegistryImpl>,
    key: &str,
    render: PromptRenderFn,
    options: Option<SectionOptions>,
    name: Option<&str>,
) -> pi_js::Result<Installed> {
    let ext_name = name.map(ToOwned::to_owned).unwrap_or_else(|| format!("section:{key}"));
    install_one(
        registry,
        define_extension(Extension {
            name: ext_name,
            sections: vec![section_with_options(key, render, options)],
            ..Default::default()
        }),
    )
}
