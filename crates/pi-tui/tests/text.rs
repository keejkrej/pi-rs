//! Port of packages/tui/test/text.test.ts

use pi_tui::Text;

#[test]
fn keeps_its_wrapped_lines_when_the_same_text_is_set_again() {
    let text = Text::new(Some("hello world"), Some(0), Some(0), None);
    let lines = text.render(5);
    text.set_text("hello world");
    assert_eq!(text.render(5), lines);
    text.set_text("other");
    assert_ne!(text.render(5), lines);
}
