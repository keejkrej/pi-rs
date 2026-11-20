//! Port of packages/coding-agent/test/user-message.test.ts

use std::sync::{Arc, Mutex};

use pi_coding_agent::modes::interactive::components::markdown_transform::{
    MarkdownMessageType, MarkdownTransformContext, MarkdownTransformer,
};
use pi_coding_agent::modes::interactive::components::user_message::UserMessageComponent;
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_tui::strip_terminal_sequences;
use serial_test::serial;

const OSC133_ZONE_START: &str = "\x1b]133;A\x07";
const OSC133_ZONE_END: &str = "\x1b]133;B\x07";
const OSC133_ZONE_FINAL: &str = "\x1b]133;C\x07";
const BG_RESET: &str = "\x1b[49m";

#[test]
#[serial]
fn keeps_user_message_height_stable_while_moving_closing_osc_markers_off_line_end() {
    init_theme(Some("dark"), None);

    let component = UserMessageComponent::new("hello", None, None, None);
    let lines = component.render(20);

    assert_eq!(lines.len(), 3, "lines={lines:?}");
    assert!(lines[0].contains(OSC133_ZONE_START));
    assert!(lines[0].ends_with(BG_RESET));
    assert!(!lines[0].contains(OSC133_ZONE_END));
    assert!(lines[1].contains("hello"));
    assert!(lines[2].starts_with(&format!("{OSC133_ZONE_END}{OSC133_ZONE_FINAL}")));
    assert!(lines[2].ends_with(BG_RESET));
}

#[test]
#[serial]
fn chains_markdown_transformers_with_user_message_context() {
    init_theme(Some("dark"), None);
    let calls: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let calls_1 = Arc::clone(&calls);
    let calls_2 = Arc::clone(&calls);

    let transformers: Vec<MarkdownTransformer> = vec![
        Arc::new(move |markdown: &str, context: MarkdownTransformContext| {
            calls_1.lock().unwrap().push("formula");
            assert_eq!(
                context,
                MarkdownTransformContext {
                    message_type: MarkdownMessageType::User,
                    is_streaming: false,
                    available_width: 78,
                    message_id: None,
                    timestamp: None,
                }
            );
            markdown.replace("$x^2$", "x²")
        }),
        Arc::new(move |markdown: &str, _context: MarkdownTransformContext| {
            calls_2.lock().unwrap().push("suffix");
            format!("{markdown} Done.")
        }),
    ];

    let component = UserMessageComponent::new("The input is $x^2$.", None, Some(1), Some(transformers));
    let rendered = strip_terminal_sequences(&component.render(80).join("\n"));
    assert!(rendered.contains("The input is x². Done."), "rendered={rendered}");
    assert_eq!(*calls.lock().unwrap(), vec!["formula", "suffix"]);
}

#[test]
#[serial]
fn reapplies_markdown_transformers_when_invalidated() {
    init_theme(Some("dark"), None);
    let suffix = Arc::new(Mutex::new("before".to_string()));
    let suffix_c = Arc::clone(&suffix);

    let transformers: Vec<MarkdownTransformer> = vec![Arc::new(move |markdown: &str, _| {
        format!("{markdown} {}", *suffix_c.lock().unwrap())
    })];
    let component = UserMessageComponent::new("Message", None, Some(1), Some(transformers));

    assert!(strip_terminal_sequences(&component.render(80).join("\n")).contains("Message before"));

    *suffix.lock().unwrap() = "after".to_string();
    component.invalidate();

    assert!(strip_terminal_sequences(&component.render(80).join("\n")).contains("Message after"));
}
