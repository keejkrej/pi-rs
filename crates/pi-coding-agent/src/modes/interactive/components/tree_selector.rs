//! Port of packages/coding-agent/src/modes/interactive/components/tree-selector.ts

use std::sync::{Arc, Mutex};

pub use crate::core::messages::CustomMessageContent;
pub use crate::core::session_manager::{
    BranchSummaryEntry, CompactionEntry, ContextEditEntry, ContextEditReplacement, ContextEditableContent, CustomEntry,
    CustomMessageEntry, LabelEntry, ModelChangeEntry, SessionEntry, SessionInfoEntry, SessionMessageEntry,
    SessionTreeNode, ThinkingLevelChangeEntry, UsageEntry,
};
use crate::utils::paths::shorten_home_path;
use chrono::{DateTime, Datelike, Local, Timelike};
use indexmap::{IndexMap, IndexSet};
use pi_agent_core::AgentMessage;
use pi_ai::{
    AssistantContentBlock, JsonObject, Message, StopReason, SystemMessageContent, UserContentBlock, UserMessageContent,
};
use pi_tui::{
    CURSOR_MARKER, Component, Container, Focusable, Input, Keybinding, Spacer, Text, TuiMouseEvent,
    TuiMouseEventResult, get_keybindings, slice_by_column, truncate_to_width, visible_width, wrap_text_with_ansi,
};
use serde_json::Value;

use super::super::theme::theme::{ThemeBg, ThemeColor, theme};
use super::dynamic_border::DynamicBorder;
use super::keybinding_hints::{KeyTextFormatOptions, format_key_text, key_hint};
use crate::core::tools::render_utils::format_read_line_range;

#[derive(Clone, Debug)]
struct GutterInfo {
    position: usize,
    show: bool,
}

#[derive(Clone, Debug)]
struct FlatNode {
    node: SessionTreeNode,
    indent: usize,
    show_connector: bool,
    is_last: bool,
    gutters: Vec<GutterInfo>,
    is_virtual_root_child: bool,
}

struct HorizontalViewportRow {
    gutter: String,
    body: String,
    anchor_col: usize,
    body_width: usize,
    is_selected: bool,
}

const TREE_GUTTER_WIDTH: usize = 2;
const MIN_VISIBLE_ANCHOR_CONTENT_WIDTH: usize = 4;
const MAX_VISIBLE_ANCHOR_CONTENT_WIDTH: usize = 20;
const MIN_ANCHOR_CONTEXT_WIDTH: usize = 2;
const MAX_ANCHOR_CONTEXT_WIDTH: usize = 12;

fn render_horizontal_viewport(rows: &[HorizontalViewportRow], width: usize) -> Vec<String> {
    let viewport_width = width.saturating_sub(TREE_GUTTER_WIDTH);
    let max_body_width = rows.iter().map(|r| r.body_width).max().unwrap_or(0);
    let max_horizontal_scroll = max_body_width.saturating_sub(viewport_width);
    let selected_row = rows.iter().find(|r| r.is_selected);

    let mut horizontal_scroll = 0usize;
    if let Some(selected) = selected_row
        && max_horizontal_scroll > 0
    {
        let min_visible_anchor_content_width =
            (viewport_width / 3).clamp(MIN_VISIBLE_ANCHOR_CONTENT_WIDTH, MAX_VISIBLE_ANCHOR_CONTENT_WIDTH);
        if selected.anchor_col > viewport_width.saturating_sub(min_visible_anchor_content_width) {
            let anchor_context_width = (viewport_width / 4).clamp(MIN_ANCHOR_CONTEXT_WIDTH, MAX_ANCHOR_CONTEXT_WIDTH);
            horizontal_scroll = max_horizontal_scroll.min(selected.anchor_col.saturating_sub(anchor_context_width));
        }
    }

    rows.iter()
        .map(|row| {
            let line = if horizontal_scroll > 0 {
                let sliced = slice_by_column(&row.body, horizontal_scroll as i64, viewport_width as i64, Some(true));
                format!("{}{sliced}\x1b[0m", row.gutter)
            } else {
                format!("{}{}", row.gutter, row.body)
            };
            truncate_to_width(&line, width as i64, Some(""), None)
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FilterMode {
    #[default]
    Default,
    NoTools,
    UserOnly,
    LabeledOnly,
    All,
}

impl FilterMode {
    const CYCLE: [Self; 5] = [
        Self::Default,
        Self::NoTools,
        Self::UserOnly,
        Self::LabeledOnly,
        Self::All,
    ];
}

#[derive(Clone, Debug)]
struct ToolCallInfo {
    name: String,
    arguments: JsonObject,
}

struct TreeListState {
    flat_nodes: Vec<FlatNode>,
    filtered_nodes: Vec<FlatNode>,
    selected_index: usize,
    current_leaf_id: Option<String>,
    max_visible_lines: usize,
    filter_mode: FilterMode,
    search_query: String,
    tool_call_map: IndexMap<String, ToolCallInfo>,
    multiple_roots: bool,
    show_label_timestamps: bool,
    active_path_ids: IndexSet<String>,
    visible_parent_map: IndexMap<String, Option<String>>,
    visible_children_map: IndexMap<Option<String>, Vec<String>>,
    last_selected_id: Option<String>,
    folded_nodes: IndexSet<String>,
    on_select: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    on_cancel: Option<Arc<dyn Fn() + Send + Sync>>,
    on_copy: Option<Arc<dyn Fn(Option<&str>) + Send + Sync>>,
    on_label_edit: Option<Arc<dyn Fn(&str, Option<&str>) + Send + Sync>>,
}

#[derive(Clone)]
pub struct TreeList {
    state: Arc<Mutex<TreeListState>>,
}

impl TreeList {
    fn new(
        tree: Vec<SessionTreeNode>,
        current_leaf_id: Option<String>,
        max_visible_lines: usize,
        initial_selected_id: Option<String>,
        initial_filter_mode: Option<FilterMode>,
    ) -> Self {
        let mut state = TreeListState {
            flat_nodes: Vec::new(),
            filtered_nodes: Vec::new(),
            selected_index: 0,
            current_leaf_id: current_leaf_id.clone(),
            max_visible_lines,
            filter_mode: initial_filter_mode.unwrap_or(FilterMode::Default),
            search_query: String::new(),
            tool_call_map: IndexMap::new(),
            multiple_roots: tree.len() > 1,
            show_label_timestamps: false,
            active_path_ids: IndexSet::new(),
            visible_parent_map: IndexMap::new(),
            visible_children_map: IndexMap::new(),
            last_selected_id: None,
            folded_nodes: IndexSet::new(),
            on_select: None,
            on_cancel: None,
            on_copy: None,
            on_label_edit: None,
        };

        Self::flatten_tree_locked(&mut state, &tree);
        Self::build_active_path_locked(&mut state);
        Self::apply_filter_locked(&mut state);

        let target_id = initial_selected_id.or(current_leaf_id);
        state.selected_index = Self::find_nearest_visible_index_locked(&state, target_id.as_deref());
        state.last_selected_id = state
            .filtered_nodes
            .get(state.selected_index)
            .map(|n| n.node.entry.id().to_string());

        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }

    fn set_on_select(&self, cb: Option<Arc<dyn Fn(&str) + Send + Sync>>) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).on_select = cb;
    }

    fn set_on_cancel(&self, cb: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).on_cancel = cb;
    }

    fn set_on_copy(&self, cb: Option<Arc<dyn Fn(Option<&str>) + Send + Sync>>) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).on_copy = cb;
    }

    fn set_on_label_edit(&self, cb: Option<Arc<dyn Fn(&str, Option<&str>) + Send + Sync>>) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).on_label_edit = cb;
    }

    fn find_nearest_visible_index_locked(state: &TreeListState, entry_id: Option<&str>) -> usize {
        if state.filtered_nodes.is_empty() {
            return 0;
        }
        let mut parent_by_id: IndexMap<&str, Option<&str>> = IndexMap::new();
        for flat_node in &state.flat_nodes {
            parent_by_id.insert(flat_node.node.entry.id(), flat_node.node.entry.parent_id());
        }
        let mut visible_id_to_index: IndexMap<&str, usize> = IndexMap::new();
        for (i, flat_node) in state.filtered_nodes.iter().enumerate() {
            visible_id_to_index.insert(flat_node.node.entry.id(), i);
        }

        let mut current = entry_id;
        while let Some(id) = current {
            if let Some(&idx) = visible_id_to_index.get(id) {
                return idx;
            }
            match parent_by_id.get(id) {
                Some(&next_parent) => current = next_parent,
                None => break,
            }
        }
        state.filtered_nodes.len() - 1
    }

    fn build_active_path_locked(state: &mut TreeListState) {
        state.active_path_ids.clear();
        let Some(ref leaf_id) = state.current_leaf_id else {
            return;
        };
        let mut parent_by_id: IndexMap<&str, Option<&str>> = IndexMap::new();
        for flat_node in &state.flat_nodes {
            parent_by_id.insert(flat_node.node.entry.id(), flat_node.node.entry.parent_id());
        }
        let mut current: Option<&str> = Some(leaf_id.as_str());
        while let Some(id) = current {
            state.active_path_ids.insert(id.to_string());
            let Some(&parent) = parent_by_id.get(id) else {
                break;
            };
            current = parent;
        }
    }

    fn flatten_tree_locked(state: &mut TreeListState, roots: &[SessionTreeNode]) {
        state.tool_call_map.clear();

        let leaf_id = state.current_leaf_id.as_deref();
        let mut contains_active: IndexSet<String> = IndexSet::new();
        {
            let mut all_nodes: Vec<&SessionTreeNode> = Vec::new();
            let mut pre_order: Vec<&SessionTreeNode> = roots.iter().collect();
            while let Some(node) = pre_order.pop() {
                all_nodes.push(node);
                for child in node.children.iter().rev() {
                    pre_order.push(child);
                }
            }
            for node in all_nodes.into_iter().rev() {
                let id = node.entry.id();
                let mut has = leaf_id.is_some_and(|l| id == l);
                if !has {
                    for child in &node.children {
                        if contains_active.contains(child.entry.id()) {
                            has = true;
                            break;
                        }
                    }
                }
                if has {
                    contains_active.insert(id.to_string());
                }
            }
        }

        struct StackItem<'a> {
            node: &'a SessionTreeNode,
            indent: usize,
            just_branched: bool,
            show_connector: bool,
            is_last: bool,
            gutters: Vec<GutterInfo>,
            is_virtual_root_child: bool,
        }

        let mut stack: Vec<StackItem<'_>> = Vec::new();
        let multiple_roots = roots.len() > 1;
        let mut ordered_roots: Vec<&SessionTreeNode> = roots.iter().collect();
        ordered_roots.sort_by_key(|r| !contains_active.contains(r.entry.id()));
        for (i, root) in ordered_roots.iter().enumerate().rev() {
            let is_last = i == ordered_roots.len() - 1;
            stack.push(StackItem {
                node: root,
                indent: usize::from(multiple_roots),
                just_branched: multiple_roots,
                show_connector: multiple_roots,
                is_last,
                gutters: Vec::new(),
                is_virtual_root_child: multiple_roots,
            });
        }

        let mut result = Vec::new();
        while let Some(item) = stack.pop() {
            let entry = &item.node.entry;
            if let SessionEntry::Message(msg_entry) = entry
                && let AgentMessage::Message(Message::Assistant(asst)) = &msg_entry.message
            {
                for block in &asst.content {
                    if let AssistantContentBlock::ToolCall(tc) = block {
                        state.tool_call_map.insert(
                            tc.id.clone(),
                            ToolCallInfo {
                                name: tc.name.clone(),
                                arguments: tc.arguments.clone(),
                            },
                        );
                    }
                }
            }

            result.push(FlatNode {
                node: item.node.clone(),
                indent: item.indent,
                show_connector: item.show_connector,
                is_last: item.is_last,
                gutters: item.gutters.clone(),
                is_virtual_root_child: item.is_virtual_root_child,
            });

            let children = &item.node.children;
            let multiple_children = children.len() > 1;
            let mut prioritized: Vec<&SessionTreeNode> = Vec::new();
            let mut rest: Vec<&SessionTreeNode> = Vec::new();
            for child in children {
                if contains_active.contains(child.entry.id()) {
                    prioritized.push(child);
                } else {
                    rest.push(child);
                }
            }
            prioritized.extend(rest);

            let child_indent = if multiple_children || (item.just_branched && item.indent > 0) {
                item.indent + 1
            } else {
                item.indent
            };

            let connector_displayed = item.show_connector && !item.is_virtual_root_child;
            let current_display_indent = if state.multiple_roots {
                item.indent.saturating_sub(1)
            } else {
                item.indent
            };
            let connector_position = current_display_indent.saturating_sub(1);
            let mut child_gutters = item.gutters;
            if connector_displayed {
                child_gutters.push(GutterInfo {
                    position: connector_position,
                    show: !item.is_last,
                });
            }

            for (i, child) in prioritized.iter().enumerate().rev() {
                let child_is_last = i == prioritized.len() - 1;
                stack.push(StackItem {
                    node: child,
                    indent: child_indent,
                    just_branched: multiple_children,
                    show_connector: multiple_children,
                    is_last: child_is_last,
                    gutters: child_gutters.clone(),
                    is_virtual_root_child: false,
                });
            }
        }

        state.flat_nodes = result;
    }

    fn apply_filter_locked(state: &mut TreeListState) {
        if !state.filtered_nodes.is_empty()
            && let Some(selected) = state.filtered_nodes.get(state.selected_index)
        {
            state.last_selected_id = Some(selected.node.entry.id().to_string());
        }

        let search_tokens: Vec<String> = state
            .search_query
            .to_lowercase()
            .split_whitespace()
            .map(str::to_string)
            .collect();

        let current_leaf_id = state.current_leaf_id.as_deref();
        let filter_mode = state.filter_mode;

        let mut next_filtered: Vec<FlatNode> = state
            .flat_nodes
            .iter()
            .filter(|flat_node| {
                let entry = &flat_node.node.entry;
                if matches!(entry, SessionEntry::Usage(_)) {
                    return false;
                }
                let is_current_leaf = current_leaf_id == Some(entry.id());

                if let SessionEntry::Message(msg_entry) = entry
                    && msg_entry.message.role() == Some("assistant")
                    && !is_current_leaf
                {
                    let has_text = Self::message_has_text(&msg_entry.message);
                    let is_error_or_aborted = match &msg_entry.message {
                        AgentMessage::Message(Message::Assistant(a)) => {
                            a.stop_reason != StopReason::Stop && a.stop_reason != StopReason::ToolUse
                        }
                        AgentMessage::Custom(v) => v
                            .get("stopReason")
                            .and_then(|s| s.as_str())
                            .is_some_and(|s| s != "stop" && s != "toolUse"),
                        _ => false,
                    };
                    if !has_text && !is_error_or_aborted {
                        return false;
                    }
                }

                let is_settings_entry = matches!(
                    entry,
                    SessionEntry::Label(_)
                        | SessionEntry::ContextEdit(_)
                        | SessionEntry::Custom(_)
                        | SessionEntry::ModelChange(_)
                        | SessionEntry::ThinkingLevelChange(_)
                        | SessionEntry::SessionInfo(_)
                );

                let passes_filter = match filter_mode {
                    FilterMode::UserOnly => {
                        matches!(entry, SessionEntry::Message(m) if m.message.role() == Some("user"))
                    }
                    FilterMode::NoTools => {
                        !is_settings_entry
                            && !matches!(entry, SessionEntry::Message(m) if m.message.role() == Some("toolResult"))
                    }
                    FilterMode::LabeledOnly => flat_node.node.label.is_some(),
                    FilterMode::All => true,
                    FilterMode::Default => !is_settings_entry,
                };

                if !passes_filter {
                    return false;
                }

                if !search_tokens.is_empty() {
                    let node_text = Self::get_searchable_text(&flat_node.node).to_lowercase();
                    return search_tokens.iter().all(|tok| node_text.contains(tok));
                }

                true
            })
            .cloned()
            .collect();

        if !state.folded_nodes.is_empty() {
            let mut skip_set: IndexSet<String> = IndexSet::new();
            for flat_node in &state.flat_nodes {
                let id = flat_node.node.entry.id();
                if let Some(parent_id) = flat_node.node.entry.parent_id()
                    && (state.folded_nodes.contains(parent_id) || skip_set.contains(parent_id))
                {
                    skip_set.insert(id.to_string());
                }
            }
            next_filtered.retain(|flat_node| !skip_set.contains(flat_node.node.entry.id()));
        }

        state.filtered_nodes = next_filtered;
        Self::recalculate_visual_structure_locked(state);

        if let Some(last_id) = state.last_selected_id.clone() {
            state.selected_index = Self::find_nearest_visible_index_locked(state, Some(&last_id));
        } else if state.selected_index >= state.filtered_nodes.len() {
            state.selected_index = state.filtered_nodes.len().saturating_sub(1);
        }

        if let Some(selected) = state.filtered_nodes.get(state.selected_index) {
            state.last_selected_id = Some(selected.node.entry.id().to_string());
        }
    }

    fn recalculate_visual_structure_locked(state: &mut TreeListState) {
        if state.filtered_nodes.is_empty() {
            return;
        }

        let visible_ids: IndexSet<String> = state
            .filtered_nodes
            .iter()
            .map(|n| n.node.entry.id().to_string())
            .collect();
        let mut parent_by_id: IndexMap<String, Option<String>> = IndexMap::new();
        for flat_node in &state.flat_nodes {
            parent_by_id.insert(
                flat_node.node.entry.id().to_string(),
                flat_node.node.entry.parent_id().map(str::to_string),
            );
        }

        let find_visible_ancestor = |node_id: &str| -> Option<String> {
            let mut current = parent_by_id.get(node_id).and_then(|p| p.as_deref());
            while let Some(id) = current {
                if visible_ids.contains(id) {
                    return Some(id.to_string());
                }
                current = parent_by_id.get(id).and_then(|p| p.as_deref());
            }
            None
        };

        let mut visible_parent: IndexMap<String, Option<String>> = IndexMap::new();
        let mut visible_children: IndexMap<Option<String>, Vec<String>> = IndexMap::new();
        visible_children.insert(None, Vec::new());

        for flat_node in &state.filtered_nodes {
            let node_id = flat_node.node.entry.id().to_string();
            let ancestor_id = find_visible_ancestor(&node_id);
            visible_parent.insert(node_id.clone(), ancestor_id.clone());
            visible_children.entry(ancestor_id).or_default().push(node_id);
        }

        let visible_root_ids = visible_children.get(&None).cloned().unwrap_or_default();
        state.multiple_roots = visible_root_ids.len() > 1;

        let mut index_by_id: IndexMap<String, usize> = IndexMap::new();
        for (i, flat_node) in state.filtered_nodes.iter().enumerate() {
            index_by_id.insert(flat_node.node.entry.id().to_string(), i);
        }

        struct RecalcItem {
            node_id: String,
            indent: usize,
            just_branched: bool,
            show_connector: bool,
            is_last: bool,
            gutters: Vec<GutterInfo>,
            is_virtual_root_child: bool,
        }

        let mut stack = Vec::new();
        for (i, root_id) in visible_root_ids.iter().enumerate().rev() {
            let is_last = i == visible_root_ids.len() - 1;
            stack.push(RecalcItem {
                node_id: root_id.clone(),
                indent: usize::from(state.multiple_roots),
                just_branched: state.multiple_roots,
                show_connector: state.multiple_roots,
                is_last,
                gutters: Vec::new(),
                is_virtual_root_child: state.multiple_roots,
            });
        }

        while let Some(item) = stack.pop() {
            if let Some(&idx) = index_by_id.get(&item.node_id) {
                let flat_node = &mut state.filtered_nodes[idx];
                flat_node.indent = item.indent;
                flat_node.show_connector = item.show_connector;
                flat_node.is_last = item.is_last;
                flat_node.gutters = item.gutters.clone();
                flat_node.is_virtual_root_child = item.is_virtual_root_child;
            }

            let children = visible_children.get(&Some(item.node_id)).cloned().unwrap_or_default();
            let multiple_children = children.len() > 1;
            let child_indent = if multiple_children || (item.just_branched && item.indent > 0) {
                item.indent + 1
            } else {
                item.indent
            };

            let connector_displayed = item.show_connector && !item.is_virtual_root_child;
            let current_display_indent = if state.multiple_roots {
                item.indent.saturating_sub(1)
            } else {
                item.indent
            };
            let connector_position = current_display_indent.saturating_sub(1);
            let mut child_gutters = item.gutters;
            if connector_displayed {
                child_gutters.push(GutterInfo {
                    position: connector_position,
                    show: !item.is_last,
                });
            }

            for (i, child_id) in children.iter().enumerate().rev() {
                let child_is_last = i == children.len() - 1;
                stack.push(RecalcItem {
                    node_id: child_id.clone(),
                    indent: child_indent,
                    just_branched: multiple_children,
                    show_connector: multiple_children,
                    is_last: child_is_last,
                    gutters: child_gutters.clone(),
                    is_virtual_root_child: false,
                });
            }
        }

        state.visible_parent_map = visible_parent;
        state.visible_children_map = visible_children;
    }

    fn extract_full_message_text(msg: &AgentMessage) -> String {
        match msg {
            AgentMessage::Message(Message::User(u)) => match &u.content {
                UserMessageContent::Text(s) => s.clone(),
                UserMessageContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|b| match b {
                        UserContentBlock::Text(t) => Some(t.text.as_str()),
                        UserContentBlock::Image(_) => None,
                    })
                    .collect::<String>(),
            },
            AgentMessage::Message(Message::Assistant(a)) => a
                .content
                .iter()
                .filter_map(|b| match b {
                    AssistantContentBlock::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect::<String>(),
            AgentMessage::Message(Message::ToolResult(tr)) => tr
                .content
                .iter()
                .filter_map(|b| match b {
                    UserContentBlock::Text(t) => Some(t.text.as_str()),
                    UserContentBlock::Image(_) => None,
                })
                .collect::<String>(),
            AgentMessage::Message(Message::System(s)) => match &s.content {
                SystemMessageContent::Text(t) => t.clone(),
                SystemMessageContent::Blocks(blocks) => blocks.iter().map(|b| b.text.as_str()).collect::<String>(),
            },
            AgentMessage::Custom(v) => Self::extract_full_value_content(v.get("content")),
        }
    }

    fn extract_full_value_content(content: Option<&Value>) -> String {
        let Some(content) = content else {
            return String::new();
        };
        if let Some(s) = content.as_str() {
            return s.to_string();
        }
        let Some(arr) = content.as_array() else {
            return String::new();
        };
        let mut result = String::new();
        for block in arr {
            if block.get("type").and_then(|t| t.as_str()) == Some("text")
                && let Some(text) = block.get("text").and_then(|t| t.as_str())
            {
                result.push_str(text);
            }
        }
        result
    }

    fn extract_custom_message_content(content: &CustomMessageContent) -> String {
        match content {
            CustomMessageContent::Text(s) => s.clone(),
            CustomMessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    UserContentBlock::Text(t) => Some(t.text.as_str()),
                    UserContentBlock::Image(_) => None,
                })
                .collect::<String>(),
        }
    }

    fn message_has_text(msg: &AgentMessage) -> bool {
        match msg {
            AgentMessage::Message(Message::Assistant(a)) => a.content.iter().any(|b| match b {
                AssistantContentBlock::Text(t) => !t.text.trim().is_empty(),
                _ => false,
            }),
            AgentMessage::Message(Message::User(u)) => match &u.content {
                UserMessageContent::Text(s) => !s.trim().is_empty(),
                UserMessageContent::Blocks(blocks) => blocks.iter().any(|b| match b {
                    UserContentBlock::Text(t) => !t.text.trim().is_empty(),
                    UserContentBlock::Image(_) => false,
                }),
            },
            AgentMessage::Custom(v) => {
                let Some(content) = v.get("content") else {
                    return false;
                };
                if let Some(s) = content.as_str() {
                    return !s.trim().is_empty();
                }
                if let Some(arr) = content.as_array() {
                    return arr.iter().any(|b| {
                        b.get("type").and_then(|t| t.as_str()) == Some("text")
                            && b.get("text")
                                .and_then(|t| t.as_str())
                                .is_some_and(|s| !s.trim().is_empty())
                    });
                }
                false
            }
            _ => false,
        }
    }

    fn slice_200(text: &str) -> String {
        text.chars().take(200).collect()
    }

    fn get_searchable_text(node: &SessionTreeNode) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(ref label) = node.label {
            parts.push(label.clone());
        }
        match &node.entry {
            SessionEntry::Message(m) => {
                if let Some(role) = m.message.role() {
                    parts.push(role.to_string());
                }
                let extracted = Self::slice_200(&Self::extract_full_message_text(&m.message));
                if !extracted.is_empty() {
                    parts.push(extracted);
                }
                if m.message.role() == Some("bashExecution")
                    && let AgentMessage::Custom(v) = &m.message
                    && let Some(cmd) = v.get("command").and_then(|c| c.as_str())
                {
                    parts.push(cmd.to_string());
                }
            }
            SessionEntry::CustomMessage(cm) => {
                parts.push(cm.custom_type.clone());
                let full = Self::extract_custom_message_content(&cm.content);
                parts.push(match &cm.content {
                    CustomMessageContent::Text(_) => full,
                    CustomMessageContent::Blocks(_) => Self::slice_200(&full),
                });
            }
            SessionEntry::Compaction(_) => parts.push("compaction".to_string()),
            SessionEntry::BranchSummary(b) => {
                parts.push("branch summary".to_string());
                parts.push(b.summary.clone());
            }
            SessionEntry::SessionInfo(s) => {
                parts.push("title".to_string());
                if let Some(ref name) = s.name {
                    parts.push(name.clone());
                }
            }
            SessionEntry::ModelChange(mc) => {
                parts.push("model".to_string());
                parts.push(mc.model_id.clone());
            }
            SessionEntry::ThinkingLevelChange(tl) => {
                parts.push("thinking".to_string());
                parts.push(tl.thinking_level.clone());
            }
            SessionEntry::Custom(c) => {
                parts.push("custom".to_string());
                parts.push(c.custom_type.clone());
            }
            SessionEntry::ContextEdit(ce) => {
                parts.push("context edit".to_string());
                parts.push(if ce.replacement.is_none() { "omit" } else { "replace" }.to_string());
                parts.push(ce.target_id.clone());
            }
            SessionEntry::Label(l) => {
                parts.push("label".to_string());
                parts.push(l.label.clone().unwrap_or_default());
            }
            SessionEntry::Usage(_) => {}
        }
        parts.join(" ")
    }

    pub fn get_search_query(&self) -> String {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .search_query
            .clone()
    }

    pub fn get_selected_node(&self) -> Option<SessionTreeNode> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.filtered_nodes.get(state.selected_index).map(|n| n.node.clone())
    }

    pub fn copy_selected(&self) {
        let (text, on_copy) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let text = state
                .filtered_nodes
                .get(state.selected_index)
                .and_then(|n| Self::get_entry_copy_text(&n.node));
            (text, state.on_copy.clone())
        };
        if let Some(cb) = on_copy {
            cb(text.as_deref());
        }
    }

    pub fn update_node_label(&self, entry_id: &str, label: Option<&str>, label_timestamp: Option<&str>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let ts = label.map(|_| {
            label_timestamp
                .map(str::to_string)
                .unwrap_or_else(|| pi_js::time::iso_now())
        });
        for flat_node in &mut state.flat_nodes {
            if flat_node.node.entry.id() == entry_id {
                flat_node.node.label = label.map(str::to_string);
                flat_node.node.label_timestamp = ts.clone();
                break;
            }
        }
        for flat_node in &mut state.filtered_nodes {
            if flat_node.node.entry.id() == entry_id {
                flat_node.node.label = label.map(str::to_string);
                flat_node.node.label_timestamp = ts;
                break;
            }
        }
    }

    fn get_entry_copy_text(node: &SessionTreeNode) -> Option<String> {
        let text: Option<String> = match &node.entry {
            SessionEntry::Message(m) => {
                if m.message.role() == Some("bashExecution") {
                    match &m.message {
                        AgentMessage::Custom(v) => v.get("command").and_then(|c| c.as_str()).map(str::to_string),
                        _ => None,
                    }
                } else {
                    let extracted = Self::extract_full_message_text(&m.message);
                    if extracted.is_empty() {
                        match &m.message {
                            AgentMessage::Message(Message::Assistant(a)) => a.error_message.clone(),
                            AgentMessage::Custom(v) if v.get("role").and_then(|r| r.as_str()) == Some("assistant") => {
                                v.get("errorMessage").and_then(|e| e.as_str()).map(str::to_string)
                            }
                            _ => None,
                        }
                    } else {
                        Some(extracted)
                    }
                }
            }
            SessionEntry::CustomMessage(cm) => Some(Self::extract_custom_message_content(&cm.content)),
            SessionEntry::Compaction(c) => Some(c.summary.clone()),
            SessionEntry::BranchSummary(b) => Some(b.summary.clone()),
            _ => None,
        };
        text.filter(|t| !t.trim().is_empty())
    }

    fn get_status_labels(state: &TreeListState) -> String {
        let mut labels = String::new();
        match state.filter_mode {
            FilterMode::NoTools => labels.push_str(" [no-tools]"),
            FilterMode::UserOnly => labels.push_str(" [user]"),
            FilterMode::LabeledOnly => labels.push_str(" [labeled]"),
            FilterMode::All => labels.push_str(" [all]"),
            FilterMode::Default => {}
        }
        if state.show_label_timestamps {
            labels.push_str(" [+label time]");
        }
        labels
    }

    fn format_label_timestamp(timestamp: &str) -> String {
        let Some(ms) = pi_js::time::parse_date(timestamp) else {
            return String::new();
        };
        let Some(dt_utc) = DateTime::from_timestamp_millis(ms) else {
            return String::new();
        };
        let date = dt_utc.with_timezone(&Local);
        let now_ms = pi_js::time::now_ms();
        let now = DateTime::from_timestamp_millis(now_ms)
            .map(|d| d.with_timezone(&Local))
            .unwrap_or_else(Local::now);

        let time = format!("{:02}:{:02}", date.hour(), date.minute());
        if date.year() == now.year() && date.month() == now.month() && date.day() == now.day() {
            return time;
        }
        if date.year() == now.year() {
            return format!("{}/{} {time}", date.month(), date.day());
        }
        let yy = format!("{:02}", date.year().rem_euclid(100));
        format!("{yy}/{}/{} {time}", date.month(), date.day())
    }

    fn format_tool_call(name: &str, args: &JsonObject) -> String {
        let home = pi_js::env::home_dir();
        let shorten = |p: &str| shorten_home_path(p, Some(&home));
        let str_arg = |k: &str| -> String {
            args.get(k)
                .and_then(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Null => None,
                    other => Some(other.to_string()),
                })
                .unwrap_or_default()
        };
        let path_arg = || {
            args.get("path")
                .or_else(|| args.get("file_path"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
        };

        match name {
            "read" => {
                let path = shorten(path_arg());
                format!(
                    "[read: {path}{}]",
                    format_read_line_range(args.get("offset"), args.get("limit"))
                )
            }
            "write" => format!("[write: {}]", shorten(path_arg())),
            "edit" => format!("[edit: {}]", shorten(path_arg())),
            "bash" => {
                let raw_cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
                let normalized: String = raw_cmd
                    .chars()
                    .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
                    .collect();
                let trimmed = normalized.trim();
                let preview: String = trimmed.chars().take(50).collect();
                let ellipsis = if raw_cmd.chars().count() > 50 { "..." } else { "" };
                format!("[bash: {preview}{ellipsis}]")
            }
            "grep" => {
                let pattern = str_arg("pattern");
                let raw_path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                format!("[grep: /{pattern}/ in {}]", shorten(raw_path))
            }
            "find" => {
                let pattern = str_arg("pattern");
                let raw_path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                format!("[find: {pattern} in {}]", shorten(raw_path))
            }
            "ls" => {
                let raw_path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                format!("[ls: {}]", shorten(raw_path))
            }
            _ => {
                let full = pi_js::json::stringify(args);
                let preview: String = full.chars().take(40).collect();
                let ellipsis = if full.chars().count() > 40 { "..." } else { "" };
                format!("[{name}: {preview}{ellipsis}]")
            }
        }
    }

    fn get_entry_display_text(state: &TreeListState, node: &SessionTreeNode, is_selected: bool) -> String {
        let t = theme();
        let normalize = |s: &str| -> String {
            s.chars()
                .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
                .collect::<String>()
                .trim()
                .to_string()
        };

        let result = match &node.entry {
            SessionEntry::Message(m) => match m.message.role() {
                Some("user") => {
                    let content = normalize(&Self::slice_200(&Self::extract_full_message_text(&m.message)));
                    format!("{}{}", t.fg(ThemeColor::Accent, "user: "), content)
                }
                Some("assistant") => {
                    let text_content = normalize(&Self::slice_200(&Self::extract_full_message_text(&m.message)));
                    if !text_content.is_empty() {
                        format!("{}{}", t.fg(ThemeColor::Success, "assistant: "), text_content)
                    } else {
                        let (is_aborted, err_msg) = match &m.message {
                            AgentMessage::Message(Message::Assistant(a)) => {
                                (a.stop_reason == StopReason::Aborted, a.error_message.clone())
                            }
                            AgentMessage::Custom(v) => (
                                v.get("stopReason").and_then(|r| r.as_str()) == Some("aborted"),
                                v.get("errorMessage").and_then(|e| e.as_str()).map(str::to_string),
                            ),
                            _ => (false, None),
                        };
                        if is_aborted {
                            format!(
                                "{}{}",
                                t.fg(ThemeColor::Success, "assistant: "),
                                t.fg(ThemeColor::Muted, "(aborted)")
                            )
                        } else if let Some(err) = err_msg.filter(|s| !s.is_empty()) {
                            let err_preview: String = normalize(&err).chars().take(80).collect();
                            format!(
                                "{}{}",
                                t.fg(ThemeColor::Success, "assistant: "),
                                t.fg(ThemeColor::Error, &err_preview)
                            )
                        } else {
                            format!(
                                "{}{}",
                                t.fg(ThemeColor::Success, "assistant: "),
                                t.fg(ThemeColor::Muted, "(no content)")
                            )
                        }
                    }
                }
                Some("toolResult") => {
                    let (tool_call_id, tool_name) = match &m.message {
                        AgentMessage::Message(Message::ToolResult(tr)) => {
                            (Some(tr.tool_call_id.as_str()), Some(tr.tool_name.as_str()))
                        }
                        AgentMessage::Custom(v) => (
                            v.get("toolCallId").and_then(|s| s.as_str()),
                            v.get("toolName").and_then(|s| s.as_str()),
                        ),
                        _ => (None, None),
                    };
                    if let Some(tc) = tool_call_id.and_then(|id| state.tool_call_map.get(id)) {
                        t.fg(ThemeColor::Muted, &Self::format_tool_call(&tc.name, &tc.arguments))
                    } else {
                        t.fg(ThemeColor::Muted, &format!("[{}]", tool_name.unwrap_or("tool")))
                    }
                }
                Some("bashExecution") => {
                    let cmd = match &m.message {
                        AgentMessage::Custom(v) => v.get("command").and_then(|c| c.as_str()).unwrap_or(""),
                        _ => "",
                    };
                    t.fg(ThemeColor::Dim, &format!("[bash]: {}", normalize(cmd)))
                }
                Some(role) => t.fg(ThemeColor::Dim, &format!("[{role}]")),
                None => String::new(),
            },
            SessionEntry::CustomMessage(cm) => {
                let content = Self::extract_custom_message_content(&cm.content);
                format!(
                    "{}{}",
                    t.fg(ThemeColor::CustomMessageLabel, &format!("[{}]: ", cm.custom_type)),
                    normalize(&content)
                )
            }
            SessionEntry::Compaction(c) => {
                let tokens = (c.tokens_before as f64 / 1000.0).round() as i64;
                t.fg(ThemeColor::BorderAccent, &format!("[compaction: {tokens}k tokens]"))
            }
            SessionEntry::BranchSummary(b) => {
                format!(
                    "{}{}",
                    t.fg(ThemeColor::Warning, "[branch summary]: "),
                    normalize(&b.summary)
                )
            }
            SessionEntry::ModelChange(mc) => t.fg(ThemeColor::Dim, &format!("[model: {}]", mc.model_id)),
            SessionEntry::ThinkingLevelChange(tl) => {
                t.fg(ThemeColor::Dim, &format!("[thinking: {}]", tl.thinking_level))
            }
            SessionEntry::Custom(c) => t.fg(ThemeColor::Dim, &format!("[custom: {}]", c.custom_type)),
            SessionEntry::ContextEdit(ce) => {
                let kind = if ce.replacement.is_none() { "omit" } else { "replace" };
                t.fg(ThemeColor::Dim, &format!("[context {kind}: {}]", ce.target_id))
            }
            SessionEntry::Label(l) => t.fg(
                ThemeColor::Dim,
                &format!("[label: {}]", l.label.as_deref().unwrap_or("(cleared)")),
            ),
            SessionEntry::SessionInfo(s) => match &s.name {
                Some(name) if !name.is_empty() => format!(
                    "{}{}{}",
                    t.fg(ThemeColor::Dim, "[title: "),
                    t.fg(ThemeColor::Dim, name),
                    t.fg(ThemeColor::Dim, "]")
                ),
                _ => format!(
                    "{}{}{}",
                    t.fg(ThemeColor::Dim, "[title: "),
                    t.italic(&t.fg(ThemeColor::Dim, "empty")),
                    t.fg(ThemeColor::Dim, "]")
                ),
            },
            SessionEntry::Usage(_) => String::new(),
        };

        if is_selected { t.bold(&result) } else { result }
    }

    fn is_foldable_locked(state: &TreeListState, entry_id: &str) -> bool {
        let Some(children) = state.visible_children_map.get(&Some(entry_id.to_string())) else {
            return false;
        };
        if children.is_empty() {
            return false;
        }
        let parent_id = state.visible_parent_map.get(entry_id).cloned().unwrap_or(None);
        let Some(ref pid) = parent_id else {
            return true;
        };
        state
            .visible_children_map
            .get(&Some(pid.clone()))
            .is_some_and(|siblings| siblings.len() > 1)
    }

    fn find_branch_segment_start_locked(state: &TreeListState, direction_up: bool) -> usize {
        let Some(selected) = state.filtered_nodes.get(state.selected_index) else {
            return state.selected_index;
        };
        let mut index_by_entry_id: IndexMap<&str, usize> = IndexMap::new();
        for (i, n) in state.filtered_nodes.iter().enumerate() {
            index_by_entry_id.insert(n.node.entry.id(), i);
        }

        let mut current_id = selected.node.entry.id().to_string();
        if !direction_up {
            loop {
                let children = state
                    .visible_children_map
                    .get(&Some(current_id.clone()))
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                if children.is_empty() {
                    return index_by_entry_id
                        .get(current_id.as_str())
                        .copied()
                        .unwrap_or(state.selected_index);
                }
                if children.len() > 1 {
                    return index_by_entry_id
                        .get(children[0].as_str())
                        .copied()
                        .unwrap_or(state.selected_index);
                }
                current_id = children[0].clone();
            }
        }

        loop {
            let parent_id = state.visible_parent_map.get(&current_id).cloned().unwrap_or(None);
            let Some(pid) = parent_id else {
                return index_by_entry_id
                    .get(current_id.as_str())
                    .copied()
                    .unwrap_or(state.selected_index);
            };
            let children = state
                .visible_children_map
                .get(&Some(pid.clone()))
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            if children.len() > 1
                && let Some(&segment_start) = index_by_entry_id.get(current_id.as_str())
                && segment_start < state.selected_index
            {
                return segment_start;
            }
            current_id = pid;
        }
    }
}

impl Component for TreeList {
    fn render(&self, width: usize) -> Vec<String> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let t = theme();

        if state.filtered_nodes.is_empty() {
            return vec![
                truncate_to_width(&t.fg(ThemeColor::Muted, "  No entries found"), width as i64, None, None),
                truncate_to_width(
                    &t.fg(
                        ThemeColor::Muted,
                        &format!("  (0/0){}", Self::get_status_labels(&state)),
                    ),
                    width as i64,
                    None,
                    None,
                ),
            ];
        }

        let len = state.filtered_nodes.len();
        let max_vis = state.max_visible_lines;
        let max_start = len.saturating_sub(max_vis);
        let start_index = state.selected_index.saturating_sub(max_vis / 2).min(max_start);
        let end_index = (start_index + max_vis).min(len);

        let mut rendered_rows: Vec<HorizontalViewportRow> = Vec::with_capacity(end_index - start_index);
        for (i, flat_node) in state
            .filtered_nodes
            .iter()
            .enumerate()
            .take(end_index)
            .skip(start_index)
        {
            let entry = &flat_node.node.entry;
            let entry_id = entry.id();
            let is_selected = i == state.selected_index;
            let cursor = if is_selected {
                t.fg(ThemeColor::Accent, "› ")
            } else {
                "  ".to_string()
            };

            let display_indent = if state.multiple_roots {
                flat_node.indent.saturating_sub(1)
            } else {
                flat_node.indent
            };
            let has_connector = flat_node.show_connector && !flat_node.is_virtual_root_child;
            let connector_position = if has_connector && display_indent > 0 {
                display_indent as isize - 1
            } else {
                -1
            };

            let total_chars = display_indent * 3;
            let mut prefix = String::with_capacity(total_chars);
            let is_folded = state.folded_nodes.contains(entry_id);
            for c_idx in 0..total_chars {
                let level = c_idx / 3;
                let pos_in_level = c_idx % 3;
                if let Some(gutter) = flat_node.gutters.iter().find(|g| g.position == level) {
                    if pos_in_level == 0 {
                        prefix.push(if gutter.show { '│' } else { ' ' });
                    } else {
                        prefix.push(' ');
                    }
                } else if has_connector && level as isize == connector_position {
                    if pos_in_level == 0 {
                        prefix.push(if flat_node.is_last { '└' } else { '├' });
                    } else if pos_in_level == 1 {
                        let foldable = Self::is_foldable_locked(&state, entry_id);
                        prefix.push(if is_folded {
                            '⊞'
                        } else if foldable {
                            '⊟'
                        } else {
                            '─'
                        });
                    } else {
                        prefix.push(' ');
                    }
                } else {
                    prefix.push(' ');
                }
            }

            let shows_fold_in_connector = flat_node.show_connector && !flat_node.is_virtual_root_child;
            let fold_marker = if is_folded && !shows_fold_in_connector {
                t.fg(ThemeColor::Accent, "⊞ ")
            } else {
                String::new()
            };
            let path_marker = if state.active_path_ids.contains(entry_id) {
                t.fg(ThemeColor::Accent, "• ")
            } else {
                String::new()
            };
            let label = flat_node
                .node
                .label
                .as_deref()
                .map(|l| t.fg(ThemeColor::Warning, &format!("[{l}] ")))
                .unwrap_or_default();
            let label_timestamp = if state.show_label_timestamps && flat_node.node.label.is_some() {
                flat_node
                    .node
                    .label_timestamp
                    .as_deref()
                    .map(|ts| {
                        let fmt = Self::format_label_timestamp(ts);
                        if fmt.is_empty() {
                            String::new()
                        } else {
                            t.fg(ThemeColor::Muted, &format!("{fmt} "))
                        }
                    })
                    .unwrap_or_default()
            } else {
                String::new()
            };

            let content = Self::get_entry_display_text(&state, &flat_node.node, is_selected);
            let prefix_part = format!("{}{}{}", t.fg(ThemeColor::Dim, &prefix), fold_marker, path_marker);
            let anchor_col = visible_width(&prefix_part).max(0) as usize;
            let mut gutter = cursor;
            let mut body = format!("{prefix_part}{label}{label_timestamp}{content}");
            if is_selected {
                gutter = t.bg(ThemeBg::SelectedBg, &gutter);
                body = t.bg(ThemeBg::SelectedBg, &body);
            }
            let body_width = visible_width(&body).max(0) as usize;
            rendered_rows.push(HorizontalViewportRow {
                gutter,
                body,
                anchor_col,
                body_width,
                is_selected,
            });
        }

        let mut lines = render_horizontal_viewport(&rendered_rows, width);
        lines.push(truncate_to_width(
            &t.fg(
                ThemeColor::Muted,
                &format!(
                    "  ({}/{}){}",
                    state.selected_index + 1,
                    len,
                    Self::get_status_labels(&state)
                ),
            ),
            width as i64,
            None,
            None,
        ));
        lines
    }

    fn handle_input(&self, key_data: &str) {
        let kb = get_keybindings();

        enum PostAction {
            None,
            Select(String),
            Copy(Option<String>),
            Cancel,
            LabelEdit(String, Option<String>),
        }

        let (action, on_select, on_copy, on_cancel, on_label_edit) = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let mut post = PostAction::None;

            if kb.matches(key_data, "tui.select.up") {
                if !state.filtered_nodes.is_empty() {
                    state.selected_index = if state.selected_index == 0 {
                        state.filtered_nodes.len() - 1
                    } else {
                        state.selected_index - 1
                    };
                }
            } else if kb.matches(key_data, "tui.select.down") {
                if !state.filtered_nodes.is_empty() {
                    state.selected_index = if state.selected_index == state.filtered_nodes.len() - 1 {
                        0
                    } else {
                        state.selected_index + 1
                    };
                }
            } else if kb.matches(key_data, "app.tree.foldOrUp") {
                let current_id = state
                    .filtered_nodes
                    .get(state.selected_index)
                    .map(|n| n.node.entry.id().to_string());
                if let Some(ref cid) = current_id
                    && Self::is_foldable_locked(&state, cid)
                    && !state.folded_nodes.contains(cid)
                {
                    state.folded_nodes.insert(cid.clone());
                    Self::apply_filter_locked(&mut state);
                } else {
                    state.selected_index = Self::find_branch_segment_start_locked(&state, true);
                }
            } else if kb.matches(key_data, "app.tree.unfoldOrDown") {
                let current_id = state
                    .filtered_nodes
                    .get(state.selected_index)
                    .map(|n| n.node.entry.id().to_string());
                if let Some(ref cid) = current_id
                    && state.folded_nodes.contains(cid)
                {
                    state.folded_nodes.shift_remove(cid);
                    Self::apply_filter_locked(&mut state);
                } else {
                    state.selected_index = Self::find_branch_segment_start_locked(&state, false);
                }
            } else if kb.matches(key_data, "tui.editor.cursorLeft") || kb.matches(key_data, "tui.select.pageUp") {
                state.selected_index = state.selected_index.saturating_sub(state.max_visible_lines);
            } else if kb.matches(key_data, "tui.editor.cursorRight") || kb.matches(key_data, "tui.select.pageDown") {
                if !state.filtered_nodes.is_empty() {
                    state.selected_index =
                        (state.selected_index + state.max_visible_lines).min(state.filtered_nodes.len() - 1);
                }
            } else if kb.matches(key_data, "tui.select.confirm") {
                if let Some(selected) = state.filtered_nodes.get(state.selected_index) {
                    post = PostAction::Select(selected.node.entry.id().to_string());
                }
            } else if kb.matches(key_data, "app.message.copy") {
                let text = state
                    .filtered_nodes
                    .get(state.selected_index)
                    .and_then(|n| Self::get_entry_copy_text(&n.node));
                post = PostAction::Copy(text);
            } else if kb.matches(key_data, "tui.select.cancel") {
                if !state.search_query.is_empty() {
                    state.search_query.clear();
                    state.folded_nodes.clear();
                    Self::apply_filter_locked(&mut state);
                } else {
                    post = PostAction::Cancel;
                }
            } else if kb.matches(key_data, "app.tree.filter.default") {
                state.filter_mode = FilterMode::Default;
                state.folded_nodes.clear();
                Self::apply_filter_locked(&mut state);
            } else if kb.matches(key_data, "app.tree.filter.noTools") {
                state.filter_mode = if state.filter_mode == FilterMode::NoTools {
                    FilterMode::Default
                } else {
                    FilterMode::NoTools
                };
                state.folded_nodes.clear();
                Self::apply_filter_locked(&mut state);
            } else if kb.matches(key_data, "app.tree.filter.userOnly") {
                state.filter_mode = if state.filter_mode == FilterMode::UserOnly {
                    FilterMode::Default
                } else {
                    FilterMode::UserOnly
                };
                state.folded_nodes.clear();
                Self::apply_filter_locked(&mut state);
            } else if kb.matches(key_data, "app.tree.filter.labeledOnly") {
                state.filter_mode = if state.filter_mode == FilterMode::LabeledOnly {
                    FilterMode::Default
                } else {
                    FilterMode::LabeledOnly
                };
                state.folded_nodes.clear();
                Self::apply_filter_locked(&mut state);
            } else if kb.matches(key_data, "app.tree.filter.all") {
                state.filter_mode = if state.filter_mode == FilterMode::All {
                    FilterMode::Default
                } else {
                    FilterMode::All
                };
                state.folded_nodes.clear();
                Self::apply_filter_locked(&mut state);
            } else if kb.matches(key_data, "app.tree.filter.cycleBackward") {
                let cur = FilterMode::CYCLE
                    .iter()
                    .position(|&m| m == state.filter_mode)
                    .unwrap_or(0);
                state.filter_mode = FilterMode::CYCLE[(cur + FilterMode::CYCLE.len() - 1) % FilterMode::CYCLE.len()];
                state.folded_nodes.clear();
                Self::apply_filter_locked(&mut state);
            } else if kb.matches(key_data, "app.tree.filter.cycleForward") {
                let cur = FilterMode::CYCLE
                    .iter()
                    .position(|&m| m == state.filter_mode)
                    .unwrap_or(0);
                state.filter_mode = FilterMode::CYCLE[(cur + 1) % FilterMode::CYCLE.len()];
                state.folded_nodes.clear();
                Self::apply_filter_locked(&mut state);
            } else if kb.matches(key_data, "tui.editor.deleteCharBackward") {
                if !state.search_query.is_empty() {
                    let next_len = pi_js::str16::len(&state.search_query).saturating_sub(1);
                    state.search_query = pi_js::str16::slice(&state.search_query, 0, Some(next_len as i64));
                    state.folded_nodes.clear();
                    Self::apply_filter_locked(&mut state);
                }
            } else if kb.matches(key_data, "app.tree.editLabel") {
                if let Some(selected) = state.filtered_nodes.get(state.selected_index) {
                    post = PostAction::LabelEdit(selected.node.entry.id().to_string(), selected.node.label.clone());
                }
            } else if kb.matches(key_data, "app.tree.toggleLabelTimestamp") {
                state.show_label_timestamps = !state.show_label_timestamps;
            } else {
                let has_control_chars = key_data.chars().any(|ch| {
                    let code = ch as u32;
                    code < 32 || code == 0x7f || (0x80..=0x9f).contains(&code)
                });
                if !has_control_chars && !key_data.is_empty() {
                    state.search_query.push_str(key_data);
                    state.folded_nodes.clear();
                    Self::apply_filter_locked(&mut state);
                }
            }

            (
                post,
                state.on_select.clone(),
                state.on_copy.clone(),
                state.on_cancel.clone(),
                state.on_label_edit.clone(),
            )
        };

        match action {
            PostAction::None => {}
            PostAction::Select(id) => {
                if let Some(cb) = on_select {
                    cb(&id);
                }
            }
            PostAction::Copy(text) => {
                if let Some(cb) = on_copy {
                    cb(text.as_deref());
                }
            }
            PostAction::Cancel => {
                if let Some(cb) = on_cancel {
                    cb();
                }
            }
            PostAction::LabelEdit(id, label) => {
                if let Some(cb) = on_label_edit {
                    cb(&id, label.as_deref());
                }
            }
        }
    }

    fn invalidate(&self) {}
}

#[derive(Clone)]
struct SearchLine {
    tree_list: TreeList,
    focused: Arc<Mutex<bool>>,
}

impl SearchLine {
    fn new(tree_list: TreeList) -> Self {
        Self {
            tree_list,
            focused: Arc::new(Mutex::new(false)),
        }
    }
}

impl Focusable for SearchLine {
    fn focused(&self) -> bool {
        *self.focused.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_focused(&self, value: bool) {
        *self.focused.lock().unwrap_or_else(|e| e.into_inner()) = value;
    }
}

impl Component for SearchLine {
    fn render(&self, width: usize) -> Vec<String> {
        if width == 0 {
            return vec![String::new()];
        }
        let t = theme();
        let query = self.tree_list.get_search_query();
        let line = if !query.is_empty() {
            format!(
                "  {} {}",
                t.fg(ThemeColor::Muted, "Type to search:"),
                t.fg(ThemeColor::Accent, &query)
            )
        } else {
            format!("  {} ", t.fg(ThemeColor::Muted, "Type to search:"))
        };
        if !self.focused() {
            return vec![truncate_to_width(&line, width as i64, None, None)];
        }
        let content = truncate_to_width(&line, (width.saturating_sub(1)) as i64, None, None);
        vec![format!("{content}{CURSOR_MARKER}\x1b[7m \x1b[27m")]
    }

    fn handle_input(&self, _data: &str) {}

    fn invalidate(&self) {}
}

struct TreeHelpItem {
    keys: &'static [&'static str],
    label: &'static str,
    label_first: bool,
}

const TREE_HELP_ITEMS: &[TreeHelpItem] = &[
    TreeHelpItem {
        keys: &["tui.select.up", "tui.select.down"],
        label: "move",
        label_first: false,
    },
    TreeHelpItem {
        keys: &["tui.editor.cursorLeft", "tui.editor.cursorRight"],
        label: "page",
        label_first: false,
    },
    TreeHelpItem {
        keys: &["app.tree.foldOrUp", "app.tree.unfoldOrDown"],
        label: "branch",
        label_first: false,
    },
    TreeHelpItem {
        keys: &["app.message.copy"],
        label: "copy",
        label_first: false,
    },
    TreeHelpItem {
        keys: &["app.tree.editLabel"],
        label: "label",
        label_first: false,
    },
    TreeHelpItem {
        keys: &["app.tree.toggleLabelTimestamp"],
        label: "label time",
        label_first: false,
    },
    TreeHelpItem {
        keys: &[
            "app.tree.filter.default",
            "app.tree.filter.noTools",
            "app.tree.filter.userOnly",
            "app.tree.filter.labeledOnly",
            "app.tree.filter.all",
        ],
        label: "filters",
        label_first: true,
    },
    TreeHelpItem {
        keys: &["app.tree.filter.cycleForward", "app.tree.filter.cycleBackward"],
        label: "cycle",
        label_first: true,
    },
];

fn compact_raw_keys(keys: &[String]) -> String {
    if keys.len() == 1 {
        return keys[0].clone();
    }
    let parts: Vec<(&str, &str)> = keys
        .iter()
        .map(|key| match key.rfind('+') {
            None => ("", key.as_str()),
            Some(idx) => (&key[..=idx], &key[idx + 1..]),
        })
        .collect();
    let prefix = parts[0].0;
    if !prefix.is_empty() && parts.iter().all(|p| p.0 == prefix) {
        let suffixes: Vec<&str> = parts.iter().map(|p| p.1).collect();
        format!("{prefix}{}", suffixes.join("/"))
    } else {
        keys.join("/")
    }
}

fn replace_word(input: &str, word: &str, replacement: &str) -> String {
    let bytes = input.as_bytes();
    let wbytes = word.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(wbytes) {
            let before_ok = i == 0 || !is_word(bytes[i - 1]);
            let after_idx = i + wbytes.len();
            let after_ok = after_idx == bytes.len() || !is_word(bytes[after_idx]);
            if before_ok && after_ok {
                out.push_str(replacement);
                i = after_idx;
                continue;
            }
        }
        let ch = input[i..].chars().next().unwrap_or('\0');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn format_help_keys(keybindings: &[&str]) -> String {
    let kb = get_keybindings();
    let mut keys = Vec::new();
    for &keybinding in keybindings {
        let kb_id: Keybinding = keybinding.to_string();
        if let Some(first) = kb.get_keys(&kb_id).into_iter().next() {
            keys.push(first);
        }
    }
    if keys.is_empty() {
        return String::new();
    }
    let text = format_key_text(&compact_raw_keys(&keys), KeyTextFormatOptions::default());
    let text = replace_word(&text, "pageUp", "pgup");
    let text = replace_word(&text, "pageDown", "pgdn");
    let text = replace_word(&text, "up", "↑");
    let text = replace_word(&text, "down", "↓");
    let text = replace_word(&text, "left", "←");
    replace_word(&text, "right", "→")
}

struct TreeHelp;

impl Component for TreeHelp {
    fn render(&self, width: usize) -> Vec<String> {
        let items: Vec<String> = TREE_HELP_ITEMS
            .iter()
            .map(|item| {
                let text = format_help_keys(item.keys);
                if text.is_empty() {
                    item.label.to_string()
                } else if item.label_first {
                    format!("{} {text}", item.label)
                } else {
                    format!("{text} {}", item.label)
                }
            })
            .collect();

        let available_width = width.max(1) as i64;
        let indent = "  ";
        let separator = " · ";
        let mut lines: Vec<String> = Vec::new();
        let mut current_line = String::new();

        for item in items {
            let candidate = if !current_line.is_empty() {
                format!("{current_line}{separator}{item}")
            } else {
                let indented = format!("{indent}{item}");
                if visible_width(&indented) <= available_width {
                    indented
                } else {
                    item.clone()
                }
            };
            if current_line.is_empty() || visible_width(&candidate) <= available_width {
                current_line = candidate;
                continue;
            }
            lines.extend(wrap_text_with_ansi(current_line.trim_end(), available_width));
            let indented = format!("{indent}{item}");
            current_line = if visible_width(&indented) <= available_width {
                indented
            } else {
                item
            };
        }

        if !current_line.is_empty() {
            lines.extend(wrap_text_with_ansi(current_line.trim_end(), available_width));
        }

        let t = theme();
        lines.into_iter().map(|line| t.fg(ThemeColor::Muted, &line)).collect()
    }

    fn handle_input(&self, _data: &str) {}

    fn invalidate(&self) {}
}

struct LabelInputState {
    focused: bool,
    on_submit: Option<Arc<dyn Fn(&str, Option<&str>) + Send + Sync>>,
    on_cancel: Option<Arc<dyn Fn() + Send + Sync>>,
}

#[derive(Clone)]
struct LabelInput {
    input: Input,
    entry_id: String,
    state: Arc<Mutex<LabelInputState>>,
}

impl LabelInput {
    fn new(entry_id: &str, current_label: Option<&str>) -> Self {
        let input = Input::new(None);
        if let Some(label) = current_label {
            input.set_value(label);
        }
        Self {
            input,
            entry_id: entry_id.to_string(),
            state: Arc::new(Mutex::new(LabelInputState {
                focused: false,
                on_submit: None,
                on_cancel: None,
            })),
        }
    }

    fn set_on_submit(&self, cb: Option<Arc<dyn Fn(&str, Option<&str>) + Send + Sync>>) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).on_submit = cb;
    }

    fn set_on_cancel(&self, cb: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).on_cancel = cb;
    }
}

impl Focusable for LabelInput {
    fn focused(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).focused
    }

    fn set_focused(&self, value: bool) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).focused = value;
        self.input.set_focused(value);
    }
}

impl Component for LabelInput {
    fn render(&self, width: usize) -> Vec<String> {
        let t = theme();
        let indent = "  ";
        let available_width = width.saturating_sub(indent.len());
        let mut lines = Vec::new();
        lines.push(truncate_to_width(
            &format!("{indent}{}", t.fg(ThemeColor::Muted, "Label (empty to remove):")),
            width as i64,
            None,
            None,
        ));
        for line in self.input.render(available_width) {
            lines.push(truncate_to_width(&format!("{indent}{line}"), width as i64, None, None));
        }
        lines.push(truncate_to_width(
            &format!(
                "{indent}{}  {}",
                key_hint(&"tui.select.confirm".to_string(), "save"),
                key_hint(&"tui.select.cancel".to_string(), "cancel")
            ),
            width as i64,
            None,
            None,
        ));
        lines
    }

    fn handle_input(&self, key_data: &str) {
        let kb = get_keybindings();
        if kb.matches(key_data, "tui.select.confirm") {
            let value = self.input.get_value();
            let trimmed = value.trim();
            let label = if trimmed.is_empty() { None } else { Some(trimmed) };
            let cb = self.state.lock().unwrap_or_else(|e| e.into_inner()).on_submit.clone();
            if let Some(cb) = cb {
                cb(&self.entry_id, label);
            }
        } else if kb.matches(key_data, "tui.select.cancel") {
            let cb = self.state.lock().unwrap_or_else(|e| e.into_inner()).on_cancel.clone();
            if let Some(cb) = cb {
                cb();
            }
        } else {
            self.input.handle_input(key_data);
        }
    }

    fn invalidate(&self) {}
}

struct TreeSelectorState {
    label_input: Option<LabelInput>,
    focused: bool,
    on_copy: Option<Arc<dyn Fn(Option<&str>) + Send + Sync>>,
    on_label_change: Option<Arc<dyn Fn(&str, Option<&str>) + Send + Sync>>,
}

struct TreeSelectorInner {
    container: Container,
    tree_list: TreeList,
    search_line: SearchLine,
    tree_container: Container,
    label_input_container: Container,
    state: Mutex<TreeSelectorState>,
}

#[derive(Clone)]
pub struct TreeSelectorComponent {
    inner: Arc<TreeSelectorInner>,
}

impl TreeSelectorComponent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tree: Vec<SessionTreeNode>,
        current_leaf_id: Option<String>,
        terminal_height: usize,
        on_select: Arc<dyn Fn(&str) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
        on_label_change: Option<Arc<dyn Fn(&str, Option<&str>) + Send + Sync>>,
        initial_selected_id: Option<String>,
        initial_filter_mode: Option<FilterMode>,
    ) -> Self {
        let is_empty = tree.is_empty();
        let max_visible_lines = (terminal_height / 2).max(5);
        let tree_list = TreeList::new(
            tree,
            current_leaf_id,
            max_visible_lines,
            initial_selected_id,
            initial_filter_mode,
        );
        tree_list.set_on_select(Some(on_select));
        tree_list.set_on_cancel(Some(Arc::clone(&on_cancel)));

        let search_line = SearchLine::new(tree_list.clone());
        let tree_container = Container::new();
        tree_container.add_child(Arc::new(tree_list.clone()));
        let label_input_container = Container::new();

        let container = Container::new();
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(DynamicBorder::default()));
        container.add_child(Arc::new(Text::new(
            Some(&theme().bold("  Session Tree")),
            Some(1),
            Some(0),
            None,
        )));
        container.add_child(Arc::new(TreeHelp));
        container.add_child(Arc::new(search_line.clone()));
        container.add_child(Arc::new(DynamicBorder::default()));
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(tree_container.clone()));
        container.add_child(Arc::new(label_input_container.clone()));
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(DynamicBorder::default()));

        let comp = Self {
            inner: Arc::new(TreeSelectorInner {
                container,
                tree_list: tree_list.clone(),
                search_line,
                tree_container,
                label_input_container,
                state: Mutex::new(TreeSelectorState {
                    label_input: None,
                    focused: false,
                    on_copy: None,
                    on_label_change,
                }),
            }),
        };

        let copy_comp = comp.clone();
        tree_list.set_on_copy(Some(Arc::new(move |text| {
            let cb = copy_comp
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .on_copy
                .clone();
            if let Some(cb) = cb {
                cb(text);
            }
        })));

        let label_comp = comp.clone();
        tree_list.set_on_label_edit(Some(Arc::new(move |entry_id, current_label| {
            label_comp.show_label_input(entry_id, current_label);
        })));

        if is_empty && tokio::runtime::Handle::try_current().is_ok() {
            pi_js::time::set_timeout(100, move || {
                on_cancel();
            })
            .unref();
        }

        comp
    }

    pub fn set_on_copy(&self, cb: Option<Arc<dyn Fn(Option<&str>) + Send + Sync>>) {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).on_copy = cb;
    }

    fn show_label_input(&self, entry_id: &str, current_label: Option<&str>) {
        let label_input = LabelInput::new(entry_id, current_label);
        let submit_self = self.clone();
        label_input.set_on_submit(Some(Arc::new(move |id, label| {
            submit_self.inner.tree_list.update_node_label(id, label, None);
            let cb = submit_self
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .on_label_change
                .clone();
            if let Some(cb) = cb {
                cb(id, label);
            }
            submit_self.hide_label_input();
        })));

        let cancel_self = self.clone();
        label_input.set_on_cancel(Some(Arc::new(move || {
            cancel_self.hide_label_input();
        })));

        let focused = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let focused = state.focused;
            label_input.set_focused(focused);
            state.label_input = Some(label_input.clone());
            focused
        };
        self.inner.search_line.set_focused(false);
        let _ = focused;

        self.inner.tree_container.clear();
        self.inner.label_input_container.clear();
        self.inner.label_input_container.add_child(Arc::new(label_input));
    }

    fn hide_label_input(&self) {
        let focused = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.label_input = None;
            state.focused
        };
        self.inner.search_line.set_focused(focused);
        self.inner.label_input_container.clear();
        self.inner.tree_container.clear();
        self.inner
            .tree_container
            .add_child(Arc::new(self.inner.tree_list.clone()));
    }

    pub fn get_tree_list(&self) -> &TreeList {
        &self.inner.tree_list
    }
}

impl Focusable for TreeSelectorComponent {
    fn focused(&self) -> bool {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).focused
    }

    fn set_focused(&self, value: bool) {
        let label_input = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.focused = value;
            state.label_input.clone()
        };
        self.inner.search_line.set_focused(value && label_input.is_none());
        if let Some(input) = label_input {
            input.set_focused(value);
        }
    }
}

impl Component for TreeSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        self.inner.container.render(width)
    }

    fn handle_input(&self, key_data: &str) {
        let label_input = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .label_input
            .clone();
        if let Some(input) = label_input {
            input.handle_input(key_data);
        } else {
            self.inner.tree_list.handle_input(key_data);
        }
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }

    fn invalidate(&self) {
        self.inner.container.invalidate();
    }
}
