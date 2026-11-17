//! Port of packages/coding-agent/test/assistant-message.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::{
    AssistantContentBlock, AssistantMessage, StopReason, TextContent, ThinkingContent, ToolCall, Usage, UsageCost,
};
use pi_coding_agent::modes::interactive::components::assistant_message::AssistantMessageComponent;
use pi_coding_agent::modes::interactive::components::markdown_transform::{
    MarkdownMessageType, MarkdownTransformContext, MarkdownTransformer,
};
use pi_coding_agent::modes::interactive::components::user_message::UserMessageComponent;
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_tui::{Component, TuiMouseButton, TuiMouseEvent, TuiMouseEventType, strip_terminal_sequences};
use serial_test::serial;

const OSC133_ZONE_START: &str = "\x1b]133;A\x07";
const OSC133_ZONE_END: &str = "\x1b]133;B\x07";
const OSC133_ZONE_FINAL: &str = "\x1b]133;C\x07";

fn create_assistant_message(content: Vec<AssistantContentBlock>, stop_reason: StopReason) -> AssistantMessage {
    AssistantMessage {
        content,
        api: "openai-responses".into(),
        provider: "openai".into(),
        model: "gpt-4o-mini".to_string(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 0,
            cost: UsageCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                total: 0.0,
            },
        },
        stop_reason,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: pi_js::time::now_ms(),
    }
}

#[test]
#[serial]
fn adds_osc_133_zone_markers_to_assistant_messages_without_tool_calls() {
    init_theme(Some("dark"), None);

    let component = AssistantMessageComponent::new(
        Some(create_assistant_message(
            vec![AssistantContentBlock::Text(TextContent::new("hello"))],
            StopReason::Stop,
        )),
        None,
        None,
        None,
        None,
        None,
    );
    let lines = component.render(40);

    assert!(!lines.is_empty());
    assert!(lines[0].contains(OSC133_ZONE_START));
    assert!(
        lines
            .last()
            .expect("last line")
            .starts_with(&format!("{OSC133_ZONE_END}{OSC133_ZONE_FINAL}"))
    );
}

#[test]
#[serial]
fn does_not_add_osc_133_zone_markers_when_assistant_message_contains_tool_calls() {
    init_theme(Some("dark"), None);

    let mut args = IndexMap::new();
    args.insert("path".to_string(), serde_json::json!("file.txt"));
    let component = AssistantMessageComponent::new(
        Some(create_assistant_message(
            vec![
                AssistantContentBlock::Text(TextContent::new("calling tool")),
                AssistantContentBlock::ToolCall(ToolCall::new("tool-1", "read", args)),
            ],
            StopReason::Stop,
        )),
        None,
        None,
        None,
        None,
        None,
    );
    let rendered = component.render(60).join("\n");

    assert!(!rendered.contains(OSC133_ZONE_START));
    assert!(!rendered.contains(OSC133_ZONE_END));
    assert!(!rendered.contains(OSC133_ZONE_FINAL));
}

#[test]
#[serial]
fn renders_length_stops_with_neutral_truncation_wording() {
    init_theme(Some("dark"), None);

    let component = AssistantMessageComponent::new(
        Some(create_assistant_message(
            vec![AssistantContentBlock::Thinking(ThinkingContent::new(
                "private reasoning",
            ))],
            StopReason::Length,
        )),
        Some(true),
        None,
        None,
        None,
        None,
    );
    let rendered = component.render(80).join("\n");

    assert!(rendered.contains("Thinking..."));
    assert!(rendered.contains("Response was truncated before completion."));
}

#[test]
#[serial]
fn coalesces_adjacent_thinking_blocks_into_one_hidden_thinking_label() {
    init_theme(Some("dark"), None);

    let component = AssistantMessageComponent::new(
        Some(create_assistant_message(
            vec![
                AssistantContentBlock::Thinking(ThinkingContent::new("first thought")),
                AssistantContentBlock::Thinking(ThinkingContent::new("")),
                AssistantContentBlock::Thinking(ThinkingContent::new("second thought")),
                AssistantContentBlock::Text(TextContent::new("answer")),
            ],
            StopReason::Stop,
        )),
        Some(true),
        None,
        None,
        None,
        None,
    );
    let rendered = strip_terminal_sequences(&component.render(80).join("\n"));

    assert_eq!(rendered.matches("Thinking...").count(), 1);
    assert!(rendered.contains("answer"));
}

#[test]
#[serial]
fn collapses_individual_thinking_runs_when_clicked() {
    init_theme(Some("dark"), None);

    let component = AssistantMessageComponent::new(
        Some(create_assistant_message(
            vec![
                AssistantContentBlock::Thinking(ThinkingContent::new("first reasoning")),
                AssistantContentBlock::Text(TextContent::new("answer")),
                AssistantContentBlock::Thinking(ThinkingContent::new("second reasoning")),
            ],
            StopReason::Stop,
        )),
        None,
        None,
        None,
        None,
        None,
    );

    let width = 80;
    let lines = component.render(width);
    let first_thinking_row = lines
        .iter()
        .position(|line| strip_terminal_sequences(line).contains("first reasoning"))
        .expect("first thinking row");
    let event = TuiMouseEvent {
        r#type: TuiMouseEventType::Click,
        button: TuiMouseButton::Left,
        x: 1,
        y: first_thinking_row as i64,
        screen_x: 1,
        screen_y: first_thinking_row as i64,
        width: width as i64,
        height: lines.len() as i64,
        shift: false,
        alt: false,
        ctrl: false,
        click_count: Some(1),
        wheel_delta: None,
    };
    assert_eq!(component.handle_mouse(&event).and_then(|r| r.handled), Some(true));

    let collapsed = strip_terminal_sequences(&component.render(width).join("\n"));
    assert!(!collapsed.contains("first reasoning"));
    assert!(collapsed.contains("Thinking..."));
    assert!(collapsed.contains("second reasoning"));
}

#[test]
#[serial]
fn uses_configured_output_padding_for_text_and_thinking() {
    init_theme(Some("dark"), None);

    let component = AssistantMessageComponent::new(
        Some(create_assistant_message(
            vec![
                AssistantContentBlock::Text(TextContent::new("hello")),
                AssistantContentBlock::Thinking(ThinkingContent::new("reasoning")),
            ],
            StopReason::Stop,
        )),
        Some(false),
        None,
        Some("Thinking..."),
        Some(1),
        None,
    );
    let lines: Vec<String> = component
        .render(80)
        .into_iter()
        .map(|l| strip_terminal_sequences(&l))
        .collect();
    assert!(lines.iter().any(|l| l.contains(" hello")));
    assert!(lines.iter().any(|l| l.contains(" reasoning")));

    component.set_output_pad(0);
    let updated_lines: Vec<String> = component
        .render(80)
        .into_iter()
        .map(|l| strip_terminal_sequences(&l))
        .collect();
    assert!(updated_lines.iter().any(|l| l.starts_with("hello")));
    assert!(updated_lines.iter().any(|l| l.starts_with("reasoning")));
}

#[test]
#[serial]
fn chains_markdown_transformers_in_registration_order() {
    init_theme(Some("dark"), None);
    let calls: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let c1 = Arc::clone(&calls);
    let c2 = Arc::clone(&calls);

    let transformers: Vec<MarkdownTransformer> = vec![
        Arc::new(move |markdown: &str, context: MarkdownTransformContext| {
            c1.lock().unwrap().push("formula");
            assert_eq!(
                context,
                MarkdownTransformContext {
                    message_type: MarkdownMessageType::Assistant,
                    is_streaming: false,
                    available_width: 78,
                }
            );
            markdown.replace("$x^2$", "x²")
        }),
        Arc::new(move |markdown: &str, _| {
            c2.lock().unwrap().push("suffix");
            format!("{markdown} Done.")
        }),
    ];

    let message = create_assistant_message(
        vec![AssistantContentBlock::Text(TextContent::new("The result is $x^2$."))],
        StopReason::Stop,
    );
    let component = AssistantMessageComponent::new(
        Some(message),
        Some(false),
        None,
        Some("Thinking..."),
        Some(1),
        Some(transformers),
    );

    assert!(strip_terminal_sequences(&component.render(80).join("\n")).contains("The result is x². Done."));
    assert_eq!(*calls.lock().unwrap(), vec!["formula", "suffix"]);
}

#[test]
#[serial]
fn identifies_partial_assistant_markdown_as_streaming() {
    init_theme(Some("dark"), None);
    let streaming_states: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
    let ss_c = Arc::clone(&streaming_states);

    let message = create_assistant_message(
        vec![AssistantContentBlock::Text(TextContent::new("partial"))],
        StopReason::Stop,
    );
    let component = AssistantMessageComponent::new(
        None,
        Some(false),
        None,
        Some("Thinking..."),
        Some(1),
        Some(vec![Arc::new(
            move |markdown: &str, context: MarkdownTransformContext| {
                ss_c.lock().unwrap().push(context.is_streaming);
                if context.is_streaming {
                    markdown.to_string()
                } else {
                    format!("{markdown} transformed")
                }
            },
        )]),
    );

    component.update_content(message.clone(), Some(true));
    assert!(!strip_terminal_sequences(&component.render(80).join("\n")).contains("transformed"));

    component.update_content(message, Some(false));
    assert!(strip_terminal_sequences(&component.render(80).join("\n")).contains("partial transformed"));
    assert_eq!(*streaming_states.lock().unwrap(), vec![true, false]);
}

#[test]
#[serial]
fn reapplies_markdown_transformers_when_available_width_changes() {
    init_theme(Some("dark"), None);
    let available_widths: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let aw_c = Arc::clone(&available_widths);

    let component = AssistantMessageComponent::new(
        Some(create_assistant_message(
            vec![AssistantContentBlock::Text(TextContent::new("answer"))],
            StopReason::Stop,
        )),
        Some(false),
        None,
        Some("Thinking..."),
        Some(1),
        Some(vec![Arc::new(
            move |markdown: &str, context: MarkdownTransformContext| {
                aw_c.lock().unwrap().push(context.available_width);
                format!("{markdown} ({})", context.available_width)
            },
        )]),
    );

    assert!(strip_terminal_sequences(&component.render(80).join("\n")).contains("answer (78)"));
    let _ = component.render(80);
    assert!(strip_terminal_sequences(&component.render(60).join("\n")).contains("answer (58)"));
    assert_eq!(*available_widths.lock().unwrap(), vec![78, 58]);
}

#[test]
#[serial]
fn transforms_text_and_thinking_markdown_without_mutating_the_original_message() {
    init_theme(Some("dark"), None);
    let message = create_assistant_message(
        vec![
            AssistantContentBlock::Text(TextContent::new("answer")),
            AssistantContentBlock::Thinking(ThinkingContent::new("reasoning")),
        ],
        StopReason::Stop,
    );
    let component = AssistantMessageComponent::new(
        Some(message.clone()),
        Some(false),
        None,
        Some("Thinking..."),
        Some(1),
        Some(vec![Arc::new(|markdown: &str, context: MarkdownTransformContext| {
            let prefix = match context.message_type {
                MarkdownMessageType::User => "user",
                MarkdownMessageType::Assistant => "assistant",
                MarkdownMessageType::AssistantThinking => "assistant-thinking",
            };
            format!("{prefix}:{markdown}")
        })]),
    );

    let rendered = strip_terminal_sequences(&component.render(80).join("\n"));
    assert!(rendered.contains("assistant:answer"));
    assert!(rendered.contains("assistant-thinking:reasoning"));
    assert_eq!(
        message.content,
        vec![
            AssistantContentBlock::Text(TextContent::new("answer")),
            AssistantContentBlock::Thinking(ThinkingContent::new("reasoning")),
        ]
    );
}

#[test]
#[serial]
fn uses_configured_output_padding_for_user_messages() {
    init_theme(Some("dark"), None);

    let padded = UserMessageComponent::new("hello", None, Some(1), None);
    let padded_lines: Vec<String> = padded
        .render(40)
        .into_iter()
        .map(|l| strip_terminal_sequences(&l))
        .collect();
    assert!(padded_lines.iter().any(|l| l.starts_with(" hello")));

    let unpadded = UserMessageComponent::new("hello", None, Some(0), None);
    let unpadded_lines: Vec<String> = unpadded
        .render(40)
        .into_iter()
        .map(|l| strip_terminal_sequences(&l))
        .collect();
    assert!(unpadded_lines.iter().any(|l| l.starts_with("hello")));
}
