//! Port of packages/coding-agent/test/diff.test.ts

use pi_coding_agent::modes::interactive::components::diff::render_diff;
use pi_coding_agent::modes::interactive::theme::theme::{init_theme, theme};
use pi_coding_agent::utils::ansi::strip_ansi;
use serial_test::serial;

#[test]
#[serial]
fn preserves_indentation_for_leading_insertion_and_indented_replacement() {
    init_theme(Some("dark"), Some(false));

    let cases = [
        (
            "-83   await commitRecordingChunk({\n+83   const result = await commitRecordingChunk({",
            vec![
                "-83   await commitRecordingChunk({",
                "+83   const result = await commitRecordingChunk({",
            ],
        ),
        (
            "-12     const value = oldValue\n+12     const value = newValue",
            vec!["-12     const value = oldValue", "+12     const value = newValue"],
        ),
    ];

    for (diff, expected) in cases {
        let rendered = render_diff(diff, None);
        let stripped = strip_ansi(&rendered);
        let lines: Vec<&str> = stripped.split('\n').collect();
        assert_eq!(lines, expected);
    }
}

#[test]
#[serial]
fn does_not_highlight_shared_indentation_as_insertion() {
    init_theme(Some("dark"), Some(false));

    let rendered = render_diff(
        "-83   await commitRecordingChunk({\n+83   const result = await commitRecordingChunk({",
        None,
    );
    let inverse_insertion = theme().inverse("const result = ");
    let prefix_idx = rendered.find("+83   ").expect("prefix index");
    let insertion_idx = rendered.find(&inverse_insertion).expect("insertion index");
    assert!(prefix_idx < insertion_idx);
}
