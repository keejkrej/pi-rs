//! Port of packages/coding-agent/src/modes/interactive/chat-viewport.ts

use std::sync::Arc;

use pi_tui::components::scroll_view::ScrollViewFollow;
use pi_tui::layout_node::{ScrollOverscroll, StackBasis};
use pi_tui::{Component, ScrollView, ScrollViewOptions, ScrollViewScrollbar, StackChild, StackEntry, VStack};

type StyleFn = Arc<dyn Fn(&str) -> String + Send + Sync>;

pub struct ChatViewportOptions {
    pub document: Arc<dyn Component>,
    pub pending_messages: Arc<dyn Component>,
    pub status: Arc<dyn Component>,
    pub editor: Arc<dyn Component>,
    pub footer: Arc<dyn Component>,
    pub widgets_above: Option<Arc<dyn Component>>,
    pub widgets_below: Option<Arc<dyn Component>>,
    pub scrollbar: Option<ScrollViewScrollbar>,
    pub scrollbar_track_style: Option<StyleFn>,
    pub scrollbar_thumb_style: Option<StyleFn>,
}

#[derive(Clone)]
pub struct ChatViewport {
    pub root: Arc<dyn Component>,
    pub transcript: ScrollView,
}

fn dock_entry(component: Arc<dyn Component>, min_size: i64) -> StackChild {
    StackChild::Entry(StackEntry {
        component,
        basis: None,
        grow: None,
        shrink: Some(1),
        min_size: Some(min_size),
        max_size: None,
        visible: None,
    })
}

/// Shared fullscreen transcript and fixed input-dock layout.
pub fn create_chat_viewport(options: ChatViewportOptions) -> ChatViewport {
    let transcript = ScrollView::new(
        options.document,
        Some(ScrollViewOptions {
            axis: None,
            follow: Some(ScrollViewFollow::End),
            primary: Some(true),
            overscroll: Some(ScrollOverscroll::Chain),
            scrollbar: Some(options.scrollbar.unwrap_or(ScrollViewScrollbar::Auto)),
            scrollbar_track_style: options.scrollbar_track_style,
            scrollbar_thumb_style: options.scrollbar_thumb_style,
            scrollbar_hide_delay_ms: None,
        }),
    );

    let mut dock_children = vec![dock_entry(options.pending_messages, 0), dock_entry(options.status, 0)];
    if let Some(above) = options.widgets_above {
        dock_children.push(dock_entry(above, 0));
    }
    dock_children.push(dock_entry(options.editor, 3));
    if let Some(below) = options.widgets_below {
        dock_children.push(dock_entry(below, 0));
    }
    dock_children.push(dock_entry(options.footer, 0));

    let dock = VStack::new(Some(dock_children), None);
    let root = VStack::new(
        Some(vec![
            StackChild::Entry(StackEntry {
                component: Arc::new(transcript.clone()),
                basis: Some(StackBasis::Fixed(0.0)),
                grow: Some(1),
                shrink: Some(1),
                min_size: Some(1),
                max_size: None,
                visible: None,
            }),
            StackChild::Entry(StackEntry {
                component: Arc::new(dock),
                basis: Some(StackBasis::Auto),
                grow: Some(0),
                shrink: Some(1),
                min_size: Some(1),
                max_size: None,
                visible: None,
            }),
        ]),
        None,
    );

    ChatViewport {
        root: Arc::new(root),
        transcript,
    }
}
