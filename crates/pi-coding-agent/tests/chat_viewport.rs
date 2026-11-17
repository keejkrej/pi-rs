//! Port of packages/coding-agent/test/chat-viewport.test.ts

use std::sync::Arc;

use pi_coding_agent::modes::interactive::chat_viewport::{ChatViewportOptions, create_chat_viewport};
use pi_tui::{Container, ScrollViewScrollbar};
use pretty_assertions::assert_eq;

#[test]
fn defaults_the_transcript_scrollbar_to_auto_and_accepts_overrides() {
    let automatic = create_chat_viewport(ChatViewportOptions {
        document: Arc::new(Container::new()),
        pending_messages: Arc::new(Container::new()),
        status: Arc::new(Container::new()),
        editor: Arc::new(Container::new()),
        footer: Arc::new(Container::new()),
        widgets_above: None,
        widgets_below: None,
        scrollbar: None,
        scrollbar_track_style: None,
        scrollbar_thumb_style: None,
    });
    let hidden = create_chat_viewport(ChatViewportOptions {
        document: Arc::new(Container::new()),
        pending_messages: Arc::new(Container::new()),
        status: Arc::new(Container::new()),
        editor: Arc::new(Container::new()),
        footer: Arc::new(Container::new()),
        widgets_above: None,
        widgets_below: None,
        scrollbar: Some(ScrollViewScrollbar::Hidden),
        scrollbar_track_style: None,
        scrollbar_thumb_style: None,
    });

    assert_eq!(automatic.transcript.scrollbar(), ScrollViewScrollbar::Auto);
    assert_eq!(hidden.transcript.scrollbar(), ScrollViewScrollbar::Hidden);
}
