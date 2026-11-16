//! Port of packages/durable/src/harness/define.ts

use std::any::Any;
use std::sync::Arc;

use crate::harness::types::{Extension, HookRegistration, PromptRenderFn, PromptSection, ToolRegistration, Wrap};
use crate::tasks::{AnyTask, Task};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SectionOptions {
    pub tag: Option<bool>,
}

/// Identity function that types an extension.
pub fn define_extension(extension: Extension) -> Extension {
    extension
}

/// Identity function that types a tool.
pub fn define_tool(tool: ToolRegistration) -> ToolRegistration {
    tool
}

/// A prompt section; tagged unless `tag` is false.
pub fn section(key: impl Into<String>, render: PromptRenderFn) -> PromptSection {
    section_with_options(key, render, None)
}

/// A prompt section with explicit options (`tag: Some(false)` omits `<key>...</key>`).
pub fn section_with_options(
    key: impl Into<String>,
    render: PromptRenderFn,
    options: Option<SectionOptions>,
) -> PromptSection {
    PromptSection {
        key: key.into(),
        render,
        tag: options.and_then(|opt| opt.tag).unwrap_or(true),
    }
}

/// Hook handlers for tasks with `task`'s name.
pub fn hook<I, S, R, H: Send + Sync + 'static>(task: &Task<I, S, R, H>, handlers: H) -> HookRegistration {
    HookRegistration {
        task: task.definition.name.clone(),
        handlers: Arc::new(handlers),
    }
}

/// Erased-task variant of [`hook`].
pub fn hook_any(task: &AnyTask, handlers: Arc<dyn Any + Send + Sync>) -> HookRegistration {
    HookRegistration {
        task: task.name().to_owned(),
        handlers,
    }
}

/// Wrap the tool named like `tool` wherever the wrapping extension is selected.
pub fn wrap_tool(
    tool: &ToolRegistration,
    wrapper: impl Fn(ToolRegistration) -> ToolRegistration + Send + Sync + 'static,
) -> Wrap {
    wrap_tool_by_name(tool.name.clone(), wrapper)
}

/// Wrap the tool with the given name wherever the wrapping extension is selected.
pub fn wrap_tool_by_name(
    name: impl Into<String>,
    wrapper: impl Fn(ToolRegistration) -> ToolRegistration + Send + Sync + 'static,
) -> Wrap {
    Wrap::Tool {
        tool: name.into(),
        wrap: Arc::new(wrapper),
    }
}

/// Wrap the section `key` wherever the wrapping extension is selected.
pub fn wrap_section(
    key: impl Into<String>,
    wrapper: impl Fn(PromptSection) -> PromptSection + Send + Sync + 'static,
) -> Wrap {
    Wrap::Section {
        section: key.into(),
        wrap: Arc::new(wrapper),
    }
}
