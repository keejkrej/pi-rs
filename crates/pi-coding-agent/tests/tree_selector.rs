//! Port of packages/coding-agent/test/tree-selector.test.ts

use std::sync::{Arc, Mutex};

use chrono::{Datelike, Local, TimeZone};
use pi_agent_core::AgentMessage;
use pi_ai::{Message, StopReason};
use pi_coding_agent::modes::interactive::components::tree_selector::{
    ContextEditEntry, FilterMode, ModelChangeEntry, SessionEntry, SessionMessageEntry, SessionTreeNode,
    ThinkingLevelChangeEntry, TreeSelectorComponent,
};
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_tui::keybindings::KeyIdOrList;
use pi_tui::{
    CURSOR_MARKER, Component, Focusable, KeybindingDefinition, KeybindingsConfig, KeybindingsManager, TUI_KEYBINDINGS,
    set_keybindings, strip_terminal_sequences, visible_width,
};
use serde_json::json;
use serial_test::serial;

struct TestGuard;

impl Drop for TestGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

fn app_keybindings(user_bindings: Option<KeybindingsConfig>) -> KeybindingsManager {
    let mut defs = TUI_KEYBINDINGS.clone();
    // WIRE(@tui-w3): Replace local app.* definitions with crate::core::keybindings::KeybindingsManager once @mcp lands.
    defs.insert(
        "app.message.copy".to_string(),
        KeybindingDefinition {
            default_keys: KeyIdOrList::One("ctrl+x".to_string()),
            description: None,
        },
    );
    defs.insert(
        "app.tree.foldOrUp".to_string(),
        KeybindingDefinition {
            default_keys: KeyIdOrList::Many(vec!["alt+left".to_string(), "ctrl+left".to_string()]),
            description: None,
        },
    );
    defs.insert(
        "app.tree.unfoldOrDown".to_string(),
        KeybindingDefinition {
            default_keys: KeyIdOrList::Many(vec!["alt+right".to_string(), "ctrl+right".to_string()]),
            description: None,
        },
    );
    defs.insert(
        "app.tree.editLabel".to_string(),
        KeybindingDefinition {
            default_keys: KeyIdOrList::One("shift+l".to_string()),
            description: None,
        },
    );
    defs.insert(
        "app.tree.toggleLabelTimestamp".to_string(),
        KeybindingDefinition {
            default_keys: KeyIdOrList::One("shift+t".to_string()),
            description: None,
        },
    );
    defs.insert(
        "app.tree.filter.default".to_string(),
        KeybindingDefinition {
            default_keys: KeyIdOrList::One("ctrl+d".to_string()),
            description: None,
        },
    );
    defs.insert(
        "app.tree.filter.noTools".to_string(),
        KeybindingDefinition {
            default_keys: KeyIdOrList::One("ctrl+t".to_string()),
            description: None,
        },
    );
    defs.insert(
        "app.tree.filter.userOnly".to_string(),
        KeybindingDefinition {
            default_keys: KeyIdOrList::One("ctrl+u".to_string()),
            description: None,
        },
    );
    defs.insert(
        "app.tree.filter.labeledOnly".to_string(),
        KeybindingDefinition {
            default_keys: KeyIdOrList::One("ctrl+l".to_string()),
            description: None,
        },
    );
    defs.insert(
        "app.tree.filter.all".to_string(),
        KeybindingDefinition {
            default_keys: KeyIdOrList::One("ctrl+a".to_string()),
            description: None,
        },
    );
    defs.insert(
        "app.tree.filter.cycleForward".to_string(),
        KeybindingDefinition {
            default_keys: KeyIdOrList::One("ctrl+o".to_string()),
            description: None,
        },
    );
    defs.insert(
        "app.tree.filter.cycleBackward".to_string(),
        KeybindingDefinition {
            default_keys: KeyIdOrList::One("shift+ctrl+o".to_string()),
            description: None,
        },
    );
    KeybindingsManager::new(defs, user_bindings)
}

fn setup() -> TestGuard {
    init_theme(Some("dark"), None);
    set_keybindings(app_keybindings(None));
    TestGuard
}

fn user_message(id: &str, parent_id: Option<&str>, content: &str) -> SessionEntry {
    let msg: Message = serde_json::from_value(json!({
        "role": "user",
        "content": content,
        "timestamp": 1000
    }))
    .expect("valid user message");
    SessionEntry::Message(SessionMessageEntry {
        id: id.to_string(),
        parent_id: parent_id.map(str::to_string),
        timestamp: "2026-01-01T00:00:00.000Z".to_string(),
        message: AgentMessage::Message(msg),
    })
}

fn assistant_message(id: &str, parent_id: Option<&str>, text: &str) -> SessionEntry {
    let msg: Message = serde_json::from_value(json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": text }],
        "api": "anthropic-messages",
        "provider": "anthropic",
        "model": "claude-sonnet-4",
        "usage": {
            "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
        },
        "stopReason": "stop",
        "timestamp": 1000
    }))
    .expect("valid assistant message");
    SessionEntry::Message(SessionMessageEntry {
        id: id.to_string(),
        parent_id: parent_id.map(str::to_string),
        timestamp: "2026-01-01T00:00:00.000Z".to_string(),
        message: AgentMessage::Message(msg),
    })
}

fn tool_call_only_assistant(id: &str, parent_id: Option<&str>) -> SessionEntry {
    let msg: Message = serde_json::from_value(json!({
        "role": "assistant",
        "content": [{ "type": "toolCall", "id": format!("tc-{id}"), "name": "read", "arguments": { "path": "test.ts" } }],
        "api": "anthropic-messages",
        "provider": "anthropic",
        "model": "claude-sonnet-4",
        "usage": {
            "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
        },
        "stopReason": StopReason::ToolUse,
        "timestamp": 1000
    }))
    .expect("valid tool call assistant");
    SessionEntry::Message(SessionMessageEntry {
        id: id.to_string(),
        parent_id: parent_id.map(str::to_string),
        timestamp: "2026-01-01T00:00:00.000Z".to_string(),
        message: AgentMessage::Message(msg),
    })
}

fn model_change(id: &str, parent_id: Option<&str>) -> SessionEntry {
    SessionEntry::ModelChange(ModelChangeEntry {
        id: id.to_string(),
        parent_id: parent_id.map(str::to_string),
        timestamp: "2026-01-01T00:00:00.000Z".to_string(),
        provider: "anthropic".to_string(),
        model_id: "claude-sonnet-4".to_string(),
    })
}

fn build_tree(entries: Vec<SessionEntry>) -> Vec<SessionTreeNode> {
    if entries.is_empty() {
        return Vec::new();
    }
    let mut nodes: Vec<SessionTreeNode> = entries
        .into_iter()
        .map(|entry| SessionTreeNode {
            entry,
            children: Vec::new(),
            label: None,
            label_timestamp: None,
        })
        .collect();
    // Attach children bottom-up by parent_id.
    let len = nodes.len();
    for i in (0..len).rev() {
        let node = nodes.remove(i);
        if let Some(pid) = node.entry.parent_id().map(str::to_string)
            && let Some(parent_idx) = nodes.iter().position(|n| n.entry.id() == pid)
        {
            nodes[parent_idx].children.insert(0, node);
        } else {
            nodes.insert(i, node);
        }
    }
    nodes
}

fn build_branching_tree() -> Vec<SessionTreeNode> {
    build_tree(vec![
        user_message("user-1", None, "first message"),
        assistant_message("asst-1", Some("user-1"), "response 1"),
        user_message("user-2", Some("asst-1"), "second message"),
        assistant_message("asst-2", Some("user-2"), "response 2"),
        // Branch A (active)
        user_message("user-3a", Some("asst-2"), "branch A start"),
        assistant_message("asst-3a", Some("user-3a"), "branch A response"),
        user_message("user-4a", Some("asst-3a"), "branch A deep"),
        assistant_message("asst-4a", Some("user-4a"), "branch A leaf"),
        // Branch B
        user_message("user-3b", Some("asst-2"), "branch B start"),
        assistant_message("asst-3b", Some("user-3b"), "branch B response"),
        user_message("user-4b", Some("asst-3b"), "branch B deep"),
    ])
}

#[test]
#[serial]
fn focuses_nearest_visible_ancestor_when_current_leaf_is_model_change_with_sibling_branch() {
    let _g = setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), "hi"),
        user_message("user-2", Some("asst-1"), "active branch"),
        model_change("model-1", Some("user-2")),
        user_message("user-3", Some("asst-1"), "sibling branch"),
    ]);

    let selector = TreeSelectorComponent::new(
        tree,
        Some("model-1".to_string()),
        24,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );
    let selected = selector.get_tree_list().get_selected_node().unwrap();
    assert_eq!(selected.entry.id(), "user-2");
}

#[test]
#[serial]
fn hides_context_edits_by_default_and_labels_them_in_all_mode() {
    let _g = setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), "hi"),
        SessionEntry::ContextEdit(ContextEditEntry {
            id: "edit-1".to_string(),
            parent_id: Some("asst-1".to_string()),
            timestamp: "2026-01-01T00:00:00.000Z".to_string(),
            target_id: "asst-1".to_string(),
            replacement: None,
        }),
    ]);

    let default_selector = TreeSelectorComponent::new(
        tree.clone(),
        Some("edit-1".to_string()),
        24,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );
    assert_eq!(
        default_selector.get_tree_list().get_selected_node().unwrap().entry.id(),
        "asst-1"
    );

    let all_selector = TreeSelectorComponent::new(
        tree,
        Some("edit-1".to_string()),
        24,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        Some(FilterMode::All),
    );
    let rendered = all_selector
        .get_tree_list()
        .render(200)
        .into_iter()
        .map(|l| strip_terminal_sequences(&l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("[context omit: asst-1]"));
}

#[test]
#[serial]
fn focuses_nearest_visible_ancestor_when_current_leaf_is_thinking_level_change() {
    let _g = setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), "hi"),
        user_message("user-2", Some("asst-1"), "active branch"),
        SessionEntry::ThinkingLevelChange(ThinkingLevelChangeEntry {
            id: "thinking-1".to_string(),
            parent_id: Some("user-2".to_string()),
            timestamp: "2026-01-01T00:00:00.000Z".to_string(),
            thinking_level: "high".to_string(),
        }),
        user_message("user-3", Some("asst-1"), "sibling branch"),
    ]);

    let selector = TreeSelectorComponent::new(
        tree,
        Some("thinking-1".to_string()),
        24,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );
    assert_eq!(
        selector.get_tree_list().get_selected_node().unwrap().entry.id(),
        "user-2"
    );
}

#[test]
#[serial]
fn switches_to_nearest_visible_user_message_when_changing_to_user_only_filter() {
    let _g = setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), "hi"),
        user_message("user-2", Some("asst-1"), "active branch"),
        assistant_message("asst-2", Some("user-2"), "response"),
        user_message("user-3", Some("asst-1"), "sibling branch"),
    ]);

    let selector = TreeSelectorComponent::new(
        tree,
        Some("asst-2".to_string()),
        24,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );
    let list = selector.get_tree_list();
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "asst-2");

    selector.handle_input("\x15");
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-2");
}

#[test]
#[serial]
fn returns_to_nearest_visible_ancestor_when_switching_back_to_default_filter() {
    let _g = setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), "hi"),
        user_message("user-2", Some("asst-1"), "active branch"),
        assistant_message("asst-2", Some("user-2"), "response"),
        user_message("user-3", Some("asst-1"), "sibling branch"),
    ]);

    let selector = TreeSelectorComponent::new(
        tree,
        Some("asst-2".to_string()),
        24,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );
    let list = selector.get_tree_list();
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "asst-2");

    selector.handle_input("\x15");
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-2");

    selector.handle_input("\x04");
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-2");
}

#[test]
#[serial]
fn renders_semantic_help_rows_without_truncating_narrow_terminal_controls() {
    let _g = setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), "hi"),
    ]);
    let selector = TreeSelectorComponent::new(
        tree,
        Some("asst-1".to_string()),
        24,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );

    let plain_lines: Vec<String> = selector
        .render(30)
        .into_iter()
        .map(|l| strip_terminal_sequences(&l))
        .collect();
    let plain = plain_lines.join("\n");
    assert!(plain.contains("branch"));
    assert!(plain.contains("copy"));
    assert!(plain.contains("filters"));
    assert!(plain.contains("cycle"));
    assert!(plain.contains("label time"));
    assert!(!plain.contains("..."));
    assert!(plain_lines.iter().all(|line| visible_width(line) <= 30));
}

#[test]
#[serial]
fn renders_search_cursor_only_while_selector_is_focused() {
    let _g = setup();
    let tree = build_tree(vec![user_message("user-1", None, "hello")]);
    let selector = TreeSelectorComponent::new(
        tree,
        Some("user-1".to_string()),
        24,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );

    assert!(!selector.render(80).join("\n").contains(CURSOR_MARKER));

    selector.set_focused(true);
    let focused_lines = selector.render(80);
    let cursor_lines: Vec<String> = focused_lines
        .into_iter()
        .filter(|line| line.contains(CURSOR_MARKER))
        .collect();
    assert_eq!(cursor_lines.len(), 1);
    assert_eq!(
        strip_terminal_sequences(&cursor_lines[0].replace(CURSOR_MARKER, "")),
        "  Type to search:  "
    );
    assert!(cursor_lines[0].contains(&format!("{CURSOR_MARKER}\x1b[7m \x1b[27m")));

    selector.set_focused(false);
    assert!(!selector.render(80).join("\n").contains(CURSOR_MARKER));
}

#[test]
#[serial]
fn moves_focus_to_label_input_and_restores_it_after_cancellation() {
    let _g = setup();
    let tree = build_tree(vec![user_message("user-1", None, "hello")]);
    let selector = TreeSelectorComponent::new(
        tree,
        Some("user-1".to_string()),
        24,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );
    selector.set_focused(true);

    selector.handle_input("L");
    let rendered = selector.render(80);
    let cursor_line_idx = rendered.iter().position(|line| line.contains(CURSOR_MARKER)).unwrap();
    assert!(cursor_line_idx > 0);
    assert!(strip_terminal_sequences(&rendered[cursor_line_idx - 1]).contains("Label (empty to remove):"));
    assert_eq!(rendered.iter().filter(|line| line.contains(CURSOR_MARKER)).count(), 1);

    selector.handle_input("\x1b");
    let rendered = selector.render(80);
    let cursor_line_idx = rendered.iter().position(|line| line.contains(CURSOR_MARKER)).unwrap();
    assert!(strip_terminal_sequences(&rendered[cursor_line_idx]).contains("Type to search:"));
    assert_eq!(rendered.iter().filter(|line| line.contains(CURSOR_MARKER)).count(), 1);
}

#[test]
#[serial]
fn keeps_search_cursor_within_narrow_terminal_widths() {
    let _g = setup();
    let tree = build_tree(vec![user_message("user-1", None, "hello")]);
    let selector = TreeSelectorComponent::new(
        tree,
        Some("user-1".to_string()),
        24,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );
    selector.set_focused(true);
    selector.handle_input("a long search query");

    let rendered = selector.render(10);
    let cursor_line = rendered.iter().find(|line| line.contains(CURSOR_MARKER)).unwrap();
    assert!(visible_width(cursor_line) <= 10);
}

#[test]
#[serial]
fn copies_full_selected_message_with_ctrl_x() {
    let _g = setup();
    let message = format!("{}\nsecond line", "long message ".repeat(30));
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), &message),
    ]);
    let selector = TreeSelectorComponent::new(
        tree,
        Some("asst-1".to_string()),
        24,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );
    let copied: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let copied_clone = Arc::clone(&copied);
    selector.set_on_copy(Some(Arc::new(move |text| {
        *copied_clone.lock().unwrap() = text.map(str::to_string);
    })));

    selector.handle_input("\x18");
    assert_eq!(copied.lock().unwrap().as_deref(), Some(message.as_str()));
}

#[test]
#[serial]
fn toggles_label_timestamps_for_labeled_nodes() {
    let _g = setup();
    let mut tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), "hi"),
    ]);
    let now = Local::now();
    let target_month = if now.month() == 3 && now.day() == 28 { 4 } else { 3 };
    let label_dt = Local
        .with_ymd_and_hms(now.year(), target_month, 28, 14, 32, 0)
        .single()
        .unwrap();
    let expected_stamp = format!("{target_month}/28 14:32");
    tree[0].label = Some("checkpoint".to_string());
    tree[0].label_timestamp = Some(label_dt.to_rfc3339());

    let selector = TreeSelectorComponent::new(
        tree,
        Some("asst-1".to_string()),
        24,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );

    let list = selector.get_tree_list();
    let render_before = list.render(200).join("\n");
    assert!(render_before.contains("[checkpoint]"));
    assert!(!render_before.contains(&expected_stamp));
    assert!(!render_before.contains("[+label time]"));

    selector.handle_input("T");

    let render_after = list.render(200).join("\n");
    assert!(render_after.contains(&expected_stamp));
    assert!(render_after.contains("[+label time]"));
}

#[test]
#[serial]
fn preserves_selection_when_switching_to_empty_labeled_filter_and_back() {
    let _g = setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), "hi"),
        user_message("user-2", Some("asst-1"), "bye"),
        assistant_message("asst-2", Some("user-2"), "goodbye"),
    ]);
    let selector = TreeSelectorComponent::new(
        tree,
        Some("asst-2".to_string()),
        24,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );
    let list = selector.get_tree_list();
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "asst-2");

    selector.handle_input("\x0c");
    assert!(list.get_selected_node().is_none());

    selector.handle_input("\x04");
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "asst-2");
}

#[test]
#[serial]
fn preserves_selection_through_multiple_empty_filter_switches() {
    let _g = setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), "hi"),
    ]);
    let selector = TreeSelectorComponent::new(
        tree,
        Some("asst-1".to_string()),
        24,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );
    let list = selector.get_tree_list();
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "asst-1");

    selector.handle_input("\x0c");
    assert!(list.get_selected_node().is_none());

    selector.handle_input("\x0c");
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "asst-1");

    selector.handle_input("\x0c");
    assert!(list.get_selected_node().is_none());

    selector.handle_input("\x04");
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "asst-1");
}

const UP: &str = "\x1b[A";
const DOWN: &str = "\x1b[B";
const CTRL_LEFT: &str = "\x1b[1;5D";
const CTRL_RIGHT: &str = "\x1b[1;5C";
const ALT_LEFT: &str = "\x1b[1;3D";
const ALT_RIGHT: &str = "\x1b[1;3C";

#[test]
#[serial]
fn ctrl_right_unfolds_folded_node_then_does_segment_jump_when_unfolded() {
    let _g = setup();
    let selector = TreeSelectorComponent::new(
        build_branching_tree(),
        Some("asst-4a".to_string()),
        24,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );
    let list = selector.get_tree_list();

    selector.handle_input(CTRL_LEFT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-3a");

    selector.handle_input(CTRL_LEFT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-3a");

    selector.handle_input(DOWN);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-3b");

    selector.handle_input(UP);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-3a");

    selector.handle_input(CTRL_RIGHT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-3a");

    selector.handle_input(DOWN);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "asst-3a");

    selector.handle_input(CTRL_LEFT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-3a");

    selector.handle_input(CTRL_RIGHT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "asst-4a");
}

#[test]
#[serial]
fn alt_left_right_are_aliases_for_fold_and_unfold_navigation() {
    let _g = setup();
    let selector = TreeSelectorComponent::new(
        build_branching_tree(),
        Some("asst-4a".to_string()),
        24,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );
    let list = selector.get_tree_list();

    selector.handle_input(ALT_LEFT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-3a");

    selector.handle_input(ALT_LEFT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-3a");

    selector.handle_input(ALT_RIGHT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-3a");

    selector.handle_input(ALT_RIGHT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "asst-4a");
}

#[test]
#[serial]
fn folding_root_hides_entire_subtree_nested_fold_preserved_on_unfold() {
    let _g = setup();
    let selector = TreeSelectorComponent::new(
        build_branching_tree(),
        Some("asst-4a".to_string()),
        24,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );
    let list = selector.get_tree_list();

    selector.handle_input(CTRL_LEFT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-3a");

    selector.handle_input(CTRL_LEFT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-3a");

    selector.handle_input(CTRL_LEFT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-1");

    selector.handle_input(CTRL_LEFT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-1");

    selector.handle_input(DOWN);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-1");

    selector.handle_input(CTRL_RIGHT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-1");

    selector.handle_input(CTRL_RIGHT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-3a");

    selector.handle_input(DOWN);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-3b");
}

#[test]
#[serial]
fn fold_and_navigate_on_non_active_branch() {
    let _g = setup();
    let selector = TreeSelectorComponent::new(
        build_branching_tree(),
        Some("asst-4a".to_string()),
        24,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );
    let list = selector.get_tree_list();

    let mut found = false;
    for _ in 0..20 {
        selector.handle_input(DOWN);
        if list.get_selected_node().is_some_and(|n| n.entry.id() == "user-3b") {
            found = true;
            break;
        }
    }
    assert!(found);

    selector.handle_input(CTRL_RIGHT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-4b");

    selector.handle_input(CTRL_LEFT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-3b");

    selector.handle_input(CTRL_LEFT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-3b");

    selector.handle_input(CTRL_LEFT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-1");
}

#[test]
#[serial]
fn fold_and_navigate_with_multiple_roots() {
    let _g = setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "first root"),
        assistant_message("asst-1", Some("user-1"), "response 1"),
        user_message("user-2", None, "second root"),
        assistant_message("asst-2", Some("user-2"), "response 2"),
    ]);
    let selector = TreeSelectorComponent::new(
        tree,
        Some("asst-1".to_string()),
        24,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );
    let list = selector.get_tree_list();
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "asst-1");

    selector.handle_input(CTRL_LEFT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-1");

    selector.handle_input(CTRL_LEFT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-1");

    selector.handle_input(DOWN);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-2");

    selector.handle_input(CTRL_RIGHT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "asst-2");

    selector.handle_input(CTRL_LEFT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-2");

    selector.handle_input(CTRL_LEFT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-2");

    selector.handle_input(CTRL_LEFT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-2");
}

#[test]
#[serial]
fn folding_root_hides_descendants_even_when_intermediate_nodes_filtered_out() {
    let _g = setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        tool_call_only_assistant("tool-asst-1", Some("user-1")),
        user_message("user-2", Some("tool-asst-1"), "follow up"),
        assistant_message("asst-2", Some("user-2"), "response"),
    ]);
    let selector = TreeSelectorComponent::new(
        tree,
        Some("asst-2".to_string()),
        24,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );
    let list = selector.get_tree_list();

    selector.handle_input(CTRL_LEFT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-1");

    selector.handle_input(CTRL_LEFT);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-1");

    selector.handle_input(DOWN);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-1");
}

#[test]
#[serial]
fn search_resets_fold_state() {
    let _g = setup();
    let selector = TreeSelectorComponent::new(
        build_branching_tree(),
        Some("asst-4a".to_string()),
        24,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );
    let list = selector.get_tree_list();

    selector.handle_input(CTRL_LEFT);
    selector.handle_input(CTRL_LEFT);

    selector.handle_input(DOWN);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "user-3b");

    selector.handle_input("b");
    selector.handle_input("\x1b");

    let mut current_id = String::new();
    for _ in 0..20 {
        selector.handle_input(DOWN);
        if let Some(n) = list.get_selected_node() {
            current_id = n.entry.id().to_string();
            if current_id == "user-3a" {
                break;
            }
        }
    }
    assert_eq!(current_id, "user-3a");

    selector.handle_input(DOWN);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "asst-3a");
}

#[test]
#[serial]
fn filter_mode_change_resets_fold_state() {
    let _g = setup();
    let selector = TreeSelectorComponent::new(
        build_branching_tree(),
        Some("asst-4a".to_string()),
        24,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );
    let list = selector.get_tree_list();

    selector.handle_input(CTRL_LEFT);
    selector.handle_input(CTRL_LEFT);

    selector.handle_input("\x15");
    selector.handle_input("\x04");

    let mut current_id = String::new();
    for _ in 0..20 {
        selector.handle_input(DOWN);
        if let Some(n) = list.get_selected_node() {
            current_id = n.entry.id().to_string();
            if current_id == "user-3a" {
                break;
            }
        }
    }
    assert_eq!(current_id, "user-3a");

    selector.handle_input(DOWN);
    assert_eq!(list.get_selected_node().unwrap().entry.id(), "asst-3a");
}
