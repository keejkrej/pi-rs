//! Port of packages/coding-agent/test/custom-message.test.ts

use std::sync::{Arc, Mutex};

use pi_coding_agent::modes::interactive::components::custom_message::{
    CustomMessage, CustomMessageComponent, CustomMessageContent, MessageRenderOptions, MessageRenderer,
};
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_tui::{Component, Text, strip_terminal_sequences};
use serial_test::serial;

#[test]
#[serial]
fn provides_output_padding_to_custom_renderers_and_updates_it() {
    init_theme(Some("dark"), None);
    let options_seen: Arc<Mutex<Vec<MessageRenderOptions>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_c = Arc::clone(&options_seen);

    let renderer: MessageRenderer = Arc::new(move |_message, options, _theme| {
        seen_c.lock().unwrap().push(options);
        Some(Arc::new(Text::new(Some("custom"), Some(options.output_pad), Some(0), None)) as Arc<dyn Component>)
    });

    let message = CustomMessage {
        role: "custom".to_string(),
        custom_type: "test".to_string(),
        content: CustomMessageContent::Text("custom".to_string()),
        display: true,
        details: None,
        timestamp: pi_js::time::now_ms(),
    };
    let component = CustomMessageComponent::new(message, Some(renderer), None, Some(1));

    assert_eq!(
        *options_seen.lock().unwrap(),
        vec![MessageRenderOptions {
            expanded: false,
            output_pad: 1,
        }]
    );
    assert!(
        component
            .render(40)
            .into_iter()
            .map(|l| strip_terminal_sequences(&l))
            .any(|line| line.starts_with(" custom"))
    );

    component.set_output_pad(0);

    assert_eq!(
        options_seen.lock().unwrap().last().copied(),
        Some(MessageRenderOptions {
            expanded: false,
            output_pad: 0,
        })
    );
    assert!(
        component
            .render(40)
            .into_iter()
            .map(|l| strip_terminal_sequences(&l))
            .any(|line| line.starts_with("custom"))
    );
}
