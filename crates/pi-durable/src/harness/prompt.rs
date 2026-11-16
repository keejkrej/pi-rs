//! Port of packages/durable/src/harness/prompt.ts

use indexmap::{IndexMap, IndexSet};
use pi_ai::types::{Message, SystemMessage, SystemMessageContent, Tool, ToolReference};
use pi_ai::utils::transcript::{declarations_equal, get_current_tools, to_tool_declaration};
use pi_chord::Context;

use crate::entries::{SYSTEM_ENTRY, TypedEntryDraft};
use crate::harness::types::{ContextView, PromptInput, PromptSection};
use crate::types::ContextEdit;

type SystemDraft = TypedEntryDraft<()>;

struct ToolChanges {
    tools_removed: Vec<ToolReference>,
    tools_added: Vec<Tool>,
}

/// Sections in effect after replaying system messages in order: set in place, `None` deletes, re-adding appends.
pub fn replay_sections(messages: &[Message]) -> IndexMap<String, String> {
    let mut shown = IndexMap::new();
    for message in messages {
        let Message::System(sys) = message else {
            continue;
        };
        let Some(sections) = &sys.sections else {
            continue;
        };
        for (key, value) in sections {
            match value {
                None => {
                    shown.shift_remove(key);
                }
                Some(text) => {
                    shown.insert(key.clone(), text.clone());
                }
            }
        }
    }
    shown
}

/// Render the agent's sections in order. `None` omits a section; tagged text is wrapped as `<key>\n...\n</key>`.
/// A section that errors keeps its shown text, if any, and is reported; errors after `context` is aborted propagate.
pub async fn render_sections(
    sections: &[PromptSection],
    input: &PromptInput,
    shown: &IndexMap<String, String>,
    report: &dyn Fn(pi_js::Error),
    context: Context,
) -> pi_js::Result<IndexMap<String, String>> {
    let mut desired = IndexMap::new();
    for section in sections {
        let rendered = match (section.render)(input.clone(), context.clone()).await {
            Ok(text) => text,
            Err(error) => {
                if context.abort_signal().is_some_and(|signal| signal.aborted()) {
                    return Err(error);
                }
                report(error);
                if let Some(kept) = shown.get(&section.key) {
                    desired.insert(section.key.clone(), kept.clone());
                }
                continue;
            }
        };
        let Some(text) = rendered else {
            continue;
        };
        let formatted = if !section.tag {
            text
        } else {
            format!("<{}>\n{}\n</{}>", section.key, text, section.key)
        };
        desired.insert(section.key.clone(), formatted);
    }
    Ok(desired)
}

/// Plan the `pi.system` entries that make the replayed sections and tools of `view` equal `desired` and `tools`
/// in values and order.
///
/// - A head marker with no later `pi.system` entry in context: one complete baseline that omits every retained earlier
///   `pi.system` entry, written even when it restates the replayed values.
/// - Otherwise, when a minimal section patch would leave a different order: remove every shown section, then re-add
///   every desired section in order.
/// - Otherwise the minimal patch of changed values and `None` removals, or nothing.
///
/// Tool changes ride on the last planned entry, or on one entry of their own.
pub fn plan_system_entries(
    view: &ContextView,
    desired: &IndexMap<String, String>,
    tools: &[Tool],
    timestamp: i64,
) -> Vec<SystemDraft> {
    if let Some(head) = &view.head
        && !view
            .entries
            .iter()
            .any(|entry| SYSTEM_ENTRY.matches(Some(entry)) && entry.id > head.id)
    {
        let edits: Vec<ContextEdit> = view
            .entries
            .iter()
            .filter(|entry| SYSTEM_ENTRY.matches(Some(entry)))
            .map(|entry| ContextEdit::Omit { target: entry.id })
            .collect();
        let baseline_sections: IndexMap<String, Option<String>> =
            desired.iter().map(|(k, v)| (k.clone(), Some(v.clone()))).collect();
        let changes = ToolChanges {
            tools_removed: Vec::new(),
            tools_added: tools.iter().map(to_tool_declaration).collect(),
        };
        let mut draft = system_entry(Some(baseline_sections), Some(changes), timestamp);
        if !edits.is_empty() {
            draft.edits = Some(edits);
        }
        return vec![draft];
    }

    let sections = plan_sections(&replay_sections(&view.messages), desired);
    let current_tools = get_current_tools(&view.messages);
    let changes = plan_tools(&current_tools, tools);
    let has_tool_changes = !changes.tools_removed.is_empty() || !changes.tools_added.is_empty();
    if !has_tool_changes {
        return sections
            .into_iter()
            .map(|patch| system_entry(Some(patch), None, timestamp))
            .collect();
    }
    if sections.is_empty() {
        return vec![system_entry(None, Some(changes), timestamp)];
    }
    let last_idx = sections.len() - 1;
    let mut changes = Some(changes);
    sections
        .into_iter()
        .enumerate()
        .map(|(index, patch)| {
            let entry_changes = if index == last_idx { changes.take() } else { None };
            system_entry(Some(patch), entry_changes, timestamp)
        })
        .collect()
}

/// Tool changes from `offered` to `desired`. A changed declaration is removed and re-added.
/// Replay keeps retained tools in place and appends additions; when that would not yield the desired order,
/// every offered tool is removed and every desired tool re-added in order.
fn plan_tools(offered: &[Tool], desired: &[Tool]) -> ToolChanges {
    let wanted: IndexMap<&str, &Tool> = desired.iter().map(|tool| (tool.name.as_str(), tool)).collect();
    let kept: Vec<&Tool> = offered
        .iter()
        .filter(|tool| {
            wanted
                .get(tool.name.as_str())
                .is_some_and(|next| declarations_equal(tool, next))
        })
        .collect();
    let kept_names: IndexSet<&str> = kept.iter().map(|tool| tool.name.as_str()).collect();
    let added: Vec<&Tool> = desired
        .iter()
        .filter(|tool| !kept_names.contains(tool.name.as_str()))
        .collect();

    let order_differs = kept
        .iter()
        .chain(added.iter())
        .enumerate()
        .any(|(index, tool)| tool.name != desired[index].name);

    if order_differs {
        return ToolChanges {
            tools_removed: offered
                .iter()
                .map(|tool| ToolReference {
                    name: tool.name.clone(),
                })
                .collect(),
            tools_added: desired.iter().map(to_tool_declaration).collect(),
        };
    }

    ToolChanges {
        tools_removed: offered
            .iter()
            .filter(|tool| !kept_names.contains(tool.name.as_str()))
            .map(|tool| ToolReference {
                name: tool.name.clone(),
            })
            .collect(),
        tools_added: added.into_iter().map(to_tool_declaration).collect(),
    }
}

/// Section patches: none, the minimal patch, or a remove-all/re-add-all pair when the order would differ.
fn plan_sections(
    shown: &IndexMap<String, String>,
    desired: &IndexMap<String, String>,
) -> Vec<IndexMap<String, Option<String>>> {
    let patched_order: Vec<&str> = shown
        .keys()
        .filter(|key| desired.contains_key(*key))
        .chain(desired.keys().filter(|key| !shown.contains_key(*key)))
        .map(String::as_str)
        .collect();
    let desired_order: Vec<&str> = desired.keys().map(String::as_str).collect();
    if patched_order != desired_order {
        let remove_all: IndexMap<String, Option<String>> = shown.keys().map(|k| (k.clone(), None)).collect();
        let readd_all: IndexMap<String, Option<String>> =
            desired.iter().map(|(k, v)| (k.clone(), Some(v.clone()))).collect();
        return vec![remove_all, readd_all];
    }

    let mut patch: IndexMap<String, Option<String>> = IndexMap::new();
    for (key, value) in shown {
        let next = desired.get(key);
        if next != Some(value) {
            patch.insert(key.clone(), next.cloned());
        }
    }
    for (key, value) in desired {
        if !shown.contains_key(key) {
            patch.insert(key.clone(), Some(value.clone()));
        }
    }
    if patch.is_empty() { Vec::new() } else { vec![patch] }
}

fn system_entry(
    sections: Option<IndexMap<String, Option<String>>>,
    tools: Option<ToolChanges>,
    timestamp: i64,
) -> SystemDraft {
    let (tools_removed, tools_added) = match tools {
        None => (None, None),
        Some(changes) => (
            (!changes.tools_removed.is_empty()).then_some(changes.tools_removed),
            (!changes.tools_added.is_empty()).then_some(changes.tools_added),
        ),
    };
    let message = Message::System(SystemMessage {
        content: SystemMessageContent::Text(String::new()),
        sections,
        tools_added,
        tools_removed,
        timestamp,
    });
    TypedEntryDraft {
        model: Some(vec![message]),
        data: (),
        head: None,
        edits: None,
    }
}
