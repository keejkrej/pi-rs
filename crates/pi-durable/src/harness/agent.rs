//! Port of packages/durable/src/harness/agent.ts

use std::any::Any;
use std::sync::{Arc, LazyLock};

use indexmap::{IndexMap, IndexSet};
use pi_ai::types::ModelThinkingLevel;
use pi_chord::Context;

use crate::documents::{DocDefinitionInput, RewindableConversationDocToken, define_rewindable_conversation_doc};
use crate::harness::types::{
    Agent, AgentChange, AgentExtensionsChange, AgentExtensionsFilter, AgentState, AgentToolsChange, AgentToolsFilter,
    CompactionPolicy, ConversationRetryPolicy, ConversationStreamOptions, Extension, FieldChange, HarnessSettings,
    PromptInput, PromptSection, QueueMode, RegistrySnapshot, Settings, ToolExecutionMode, ToolRegistration, Wrap,
};
use crate::session::transaction::Tx;
use crate::types::{ConversationId, ConversationRecord, RewindableFork};

pub const DEFAULT_RETRY_POLICY: ConversationRetryPolicy = ConversationRetryPolicy {
    enabled: true,
    max_retries: 3,
    base_delay_ms: 2000,
    max_agent_delay_ms: Some(60_000),
};

pub const DEFAULT_COMPACTION_POLICY: CompactionPolicy = CompactionPolicy {
    enabled: true,
    reserve_tokens: 16_384,
    keep_recent_tokens: 20_000,
    background_tokens: 32_768,
};

/// The reserved section key of the agent's `instructions`.
pub const INSTRUCTIONS_KEY: &str = "instructions";

/// Built-in agent document; rewindable so forks start from the agent at their fork entry.
pub static AGENT_DOC: LazyLock<RewindableConversationDocToken<AgentState>> = LazyLock::new(|| {
    define_rewindable_conversation_doc(
        RewindableFork::AsOf,
        DocDefinitionInput {
            kind: "pi.agent",
            version: 1,
            initial: AgentState::default,
            migrate: None,
            checkpoint_when: Some(|_, _, _| true),
        },
    )
    .expect("valid built-in pi.agent doc definition")
});

/// Resolve the host settings: every field over its built-in default, object fields merged.
pub fn resolve_settings(settings: Option<&dyn HarnessSettings>) -> Settings {
    let Some(snapshot) = settings.map(|s| s.get()) else {
        return Settings {
            extensions: None,
            stream: ConversationStreamOptions::default(),
            retry: DEFAULT_RETRY_POLICY,
            compaction: DEFAULT_COMPACTION_POLICY,
            tool_execution: ToolExecutionMode::Parallel,
            steering_mode: QueueMode::OneAtATime,
            follow_up_mode: QueueMode::OneAtATime,
        };
    };

    let retry = match snapshot.retry {
        None => DEFAULT_RETRY_POLICY,
        Some(r) => ConversationRetryPolicy {
            enabled: r.enabled.unwrap_or(DEFAULT_RETRY_POLICY.enabled),
            max_retries: r.max_retries.unwrap_or(DEFAULT_RETRY_POLICY.max_retries),
            base_delay_ms: r.base_delay_ms.unwrap_or(DEFAULT_RETRY_POLICY.base_delay_ms),
            max_agent_delay_ms: r.max_agent_delay_ms.unwrap_or(DEFAULT_RETRY_POLICY.max_agent_delay_ms),
        },
    };

    let compaction = match snapshot.compaction {
        None => DEFAULT_COMPACTION_POLICY,
        Some(c) => CompactionPolicy {
            enabled: c.enabled.unwrap_or(DEFAULT_COMPACTION_POLICY.enabled),
            reserve_tokens: c.reserve_tokens.unwrap_or(DEFAULT_COMPACTION_POLICY.reserve_tokens),
            keep_recent_tokens: c
                .keep_recent_tokens
                .unwrap_or(DEFAULT_COMPACTION_POLICY.keep_recent_tokens),
            background_tokens: c
                .background_tokens
                .unwrap_or(DEFAULT_COMPACTION_POLICY.background_tokens),
        },
    };

    Settings {
        extensions: snapshot.extensions,
        stream: snapshot.stream.unwrap_or_default(),
        retry,
        compaction,
        tool_execution: snapshot.tool_execution.unwrap_or(ToolExecutionMode::Parallel),
        steering_mode: snapshot.steering_mode.unwrap_or(QueueMode::OneAtATime),
        follow_up_mode: snapshot.follow_up_mode.unwrap_or(QueueMode::OneAtATime),
    }
}

/// Apply one change to `pi.agent`: `Set` replaces, `Clear` deletes, `Unchanged` changes nothing.
pub async fn configure(tx: &Tx, conversation_id: ConversationId, change: &AgentChange) -> pi_js::Result<()> {
    let draft = tx.doc_conversation(&*AGENT_DOC, conversation_id).await?;
    draft.update(|state| apply_change(state, change))
}

/// `addTools` of a tool round: a list filter gets each missing name appended, `Remove` drops the
/// added names, and unset tools already offer every tool so nothing is written.
pub async fn add_tools(tx: &Tx, conversation_id: ConversationId, added: &[String]) -> pi_js::Result<()> {
    let draft = tx.doc_conversation(&*AGENT_DOC, conversation_id).await?;
    let current = draft.get()?;
    let Some(tools) = current.tools else {
        return Ok(());
    };
    match tools {
        AgentToolsFilter::List(mut list) => {
            let mut changed = false;
            for name in added {
                if !list.contains(name) {
                    list.push(name.clone());
                    changed = true;
                }
            }
            if changed {
                draft.update(|state| {
                    state.tools = Some(AgentToolsFilter::List(list));
                })?;
            }
        }
        AgentToolsFilter::Remove { mut remove } => {
            if remove.iter().any(|name| added.contains(name)) {
                remove.retain(|name| !added.contains(name));
                draft.update(|state| {
                    state.tools = Some(AgentToolsFilter::Remove { remove });
                })?;
            }
        }
    }
    Ok(())
}

fn apply_field<T>(slot: &mut Option<T>, change: &FieldChange<T>)
where
    T: Clone,
{
    match change {
        FieldChange::Unchanged => {}
        FieldChange::Clear => *slot = None,
        FieldChange::Set(value) => *slot = Some(value.clone()),
    }
}

fn apply_change(state: &mut AgentState, change: &AgentChange) {
    apply_field(&mut state.model, &change.model);
    apply_field(&mut state.thinking_level, &change.thinking_level);

    match &change.extensions {
        FieldChange::Unchanged => {}
        FieldChange::Clear => state.extensions = None,
        FieldChange::Set(AgentExtensionsChange::Replace(names)) => {
            state.extensions = Some(AgentExtensionsFilter::List(names.clone()));
        }
        FieldChange::Set(AgentExtensionsChange::Patch { add, remove }) => {
            state.extensions = Some(AgentExtensionsFilter::Patch {
                add: add.clone(),
                remove: remove.clone(),
            });
        }
    }

    match &change.tools {
        FieldChange::Unchanged => {}
        FieldChange::Clear => state.tools = None,
        FieldChange::Set(AgentToolsChange::Replace(names)) => {
            state.tools = Some(AgentToolsFilter::List(names.clone()));
        }
        FieldChange::Set(AgentToolsChange::Remove(names)) => {
            state.tools = Some(AgentToolsFilter::Remove { remove: names.clone() });
        }
    }

    apply_field(&mut state.instructions, &change.instructions);
    apply_field(&mut state.cwd, &change.cwd);
}

/// Built-in part of every Harness commit that creates or forks a conversation for `pi.agent`:
/// a fork keeps its `asOf` copy; a new task-owned conversation copies the stored agent of its
/// owner task's conversation; a new ownerless one starts empty.
pub async fn create_agent(tx: &Tx, conversation: &ConversationRecord) -> pi_js::Result<()> {
    if conversation.parent.is_some() {
        return Ok(());
    }
    let agent = tx.doc_conversation(&*AGENT_DOC, conversation.id).await?;
    let Some(owner) = conversation.owner else {
        return Ok(());
    };
    let owner_doc = tx.doc_conversation(&*AGENT_DOC, owner.conversation_id).await?;
    let owner_state = owner_doc.get()?;
    agent.replace(owner_state)
}

/// Handlers of the selected extensions' hooks for a task name, in extension order.
pub fn agent_hooks(agent: &Agent, task_name: &str) -> Vec<Arc<dyn Any + Send + Sync>> {
    let mut handlers = Vec::new();
    for extension in &agent.extensions {
        for hook in &extension.hooks {
            if hook.task == task_name {
                handlers.push(Arc::clone(&hook.handlers));
            }
        }
    }
    handlers
}

/// Resolve an agent from its stored state (absent: every field unset), a registry snapshot, and
/// resolved settings. A wrapper that renames drops its target and is reported; a wrapper without
/// a target does nothing.
pub fn resolve_agent(
    state: Option<&AgentState>,
    snapshot: &dyn RegistrySnapshot,
    settings: &Settings,
    report: &dyn Fn(pi_js::Error),
) -> Agent {
    let extensions = select_extensions(state.and_then(|s| s.extensions.as_ref()), snapshot, settings);

    let mut composed_tools: IndexMap<String, ToolRegistration> = IndexMap::new();
    for extension in &extensions {
        for tool in &extension.tools {
            composed_tools.insert(tool.name.clone(), tool.clone());
        }
    }

    let mut sections: IndexMap<String, PromptSection> = IndexMap::new();
    for extension in &extensions {
        for section in &extension.sections {
            sections.insert(section.key.clone(), section.clone());
        }
    }

    for extension in &extensions {
        for wrap in &extension.wraps {
            match wrap {
                Wrap::Tool { tool, wrap } => {
                    apply_wrap(&mut composed_tools, tool, |item| wrap(item), |item| &item.name, report);
                }
                Wrap::Section { section, wrap } => {
                    apply_wrap(&mut sections, section, |item| wrap(item), |item| &item.key, report);
                }
            }
        }
    }

    let tools = match state.and_then(|s| s.tools.as_ref()) {
        None => composed_tools.into_values().collect(),
        Some(AgentToolsFilter::List(filter)) => {
            let unique: IndexSet<&str> = filter.iter().map(String::as_str).collect();
            unique
                .into_iter()
                .filter_map(|name| composed_tools.get(name).cloned())
                .collect()
        }
        Some(AgentToolsFilter::Remove { remove }) => {
            let removed: IndexSet<&str> = remove.iter().map(String::as_str).collect();
            composed_tools
                .into_values()
                .filter(|tool| !removed.contains(tool.name.as_str()))
                .collect()
        }
    };

    let instructions = state.and_then(|s| s.instructions.clone());
    let mut agent_sections: Vec<PromptSection> = sections.into_values().collect();
    if let Some(ref text) = instructions {
        let text = text.clone();
        agent_sections.push(PromptSection {
            key: INSTRUCTIONS_KEY.to_owned(),
            render: Arc::new(move |_input: PromptInput, _context: Context| {
                let text = text.clone();
                Box::pin(async move { Ok(Some(text)) })
            }),
            tag: false,
        });
    }

    Agent {
        model: state.and_then(|s| s.model.clone()),
        thinking_level: state.and_then(|s| s.thinking_level).unwrap_or(ModelThinkingLevel::Off),
        extensions,
        tools,
        sections: agent_sections,
        instructions,
        cwd: state.and_then(|s| s.cwd.clone()),
    }
}

/// Selected installed extensions: the stored list, or the default selection edited by `{ add, remove }`.
fn select_extensions(
    stored: Option<&AgentExtensionsFilter>,
    snapshot: &dyn RegistrySnapshot,
    settings: &Settings,
) -> Vec<Extension> {
    let selected: Vec<String> = match stored {
        Some(AgentExtensionsFilter::List(list)) => list.clone(),
        _ => {
            let base: Vec<String> = match &settings.extensions {
                Some(exts) => exts.iter().map(|ext| ext.name.clone()).collect(),
                None => snapshot.installed().iter().map(|ext| ext.name.clone()).collect(),
            };
            let (add, remove) = match stored {
                Some(AgentExtensionsFilter::Patch { add, remove }) => {
                    (add.as_deref().unwrap_or(&[]), remove.as_deref().unwrap_or(&[]))
                }
                _ => (&[][..], &[][..]),
            };
            let removed: IndexSet<&str> = remove.iter().map(String::as_str).collect();
            base.into_iter()
                .chain(add.iter().cloned())
                .filter(|name| !removed.contains(name.as_str()))
                .collect()
        }
    };

    let mut seen: IndexSet<String> = IndexSet::new();
    let mut extensions = Vec::new();
    for name in selected {
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Some(extension) = snapshot.extension(&name) {
            extensions.push(extension.clone());
        }
    }
    extensions
}

fn apply_wrap<T>(
    items: &mut IndexMap<String, T>,
    target: &str,
    wrap: impl FnOnce(T) -> T,
    name_of: impl Fn(&T) -> &str,
    report: &dyn Fn(pi_js::Error),
) where
    T: Clone,
{
    let Some(existing) = items.get(target).cloned() else {
        return;
    };
    let wrapped = wrap(existing);
    let actual = name_of(&wrapped);
    if actual != target {
        items.shift_remove(target);
        report(pi_js::Error::msg(format!("Wrapper renamed {target} to {actual}")));
        return;
    }
    if let Some(slot) = items.get_mut(target) {
        *slot = wrapped;
    }
}
