//! Port of packages/tui/test/bug-regression-isimageline-startswith-bug.test.ts
//!
//! Bug regression test for isImageLine() crash scenario.

use pi_tui::terminal_image::is_image_line;

#[test]
fn old_implementation_would_return_false_causing_crash() {
    let old_is_image_line = |line: &str, image_escape_prefix: Option<&str>| -> bool {
        image_escape_prefix.is_some_and(|prefix| line.starts_with(prefix))
    };

    let terminal_without_image_support: Option<&str> = None;
    let line_with_image_sequence = "Read image file [image/jpeg]\x1b]1337;File=size=800,600;inline=1:base64data...\x07";

    let old_result = old_is_image_line(line_with_image_sequence, terminal_without_image_support);
    assert!(
        !old_result,
        "Bug: old implementation returns false for line containing image sequence when terminal has no image support"
    );
}

#[test]
fn new_implementation_returns_true_correctly() {
    let line_with_image_sequence = "Read image file [image/jpeg]\x1b]1337;File=size=800,600;inline=1:base64data...\x07";
    assert!(
        is_image_line(line_with_image_sequence),
        "Fix: new implementation returns true for line containing image sequence"
    );
}

#[test]
fn new_implementation_detects_kitty_sequences_in_any_position() {
    let long_a = "A".repeat(300_000);
    let very_long = format!("Text before \x1b_Ga=T,f=100{long_a} text after");
    let scenarios = [
        "At start: \x1b_Ga=T,f=100,data...\x1b\\",
        "Prefix \x1b_Ga=T,data...\x1b\\",
        "Suffix text \x1b_Ga=T,data...\x1b\\ suffix",
        "Middle \x1b_Ga=T,data...\x1b\\ more text",
        very_long.as_str(),
    ];

    for line in scenarios {
        let preview = &line[..line.len().min(50)];
        assert!(is_image_line(line), "Should detect Kitty sequence in: {preview}...");
    }
}

#[test]
fn new_implementation_detects_iterm2_sequences_in_any_position() {
    let long_b = "B".repeat(300_000);
    let very_long = format!("Text before \x1b]1337;File=size=800,600;inline=1:{long_b} text after");
    let scenarios = [
        "At start: \x1b]1337;File=size=100,100:base64...\x07",
        "Prefix \x1b]1337;File=inline=1:data==\x07",
        "Suffix text \x1b]1337;File=inline=1:data==\x07 suffix",
        "Middle \x1b]1337;File=inline=1:data==\x07 more text",
        very_long.as_str(),
    ];

    for line in scenarios {
        let preview = &line[..line.len().min(50)];
        assert!(is_image_line(line), "Should detect iTerm2 sequence in: {preview}...");
    }
}

#[test]
fn detects_image_sequences_in_read_tool_output() {
    let tool_output_line = "Read image file [image/jpeg]\x1b]1337;File=size=800,600;inline=1:base64image...\x07";
    assert!(
        is_image_line(tool_output_line),
        "Should detect image sequence in tool output line"
    );
}

#[test]
fn detects_kitty_sequences_from_image_component() {
    let kitty_line = "\x1b_Ga=T,f=100,t=f,d=base64data...\x1b\\\x1b_Gm=i=1;\x1b\\";
    assert!(is_image_line(kitty_line), "Should detect Kitty image component output");
}

#[test]
fn handles_ansi_codes_before_image_sequences() {
    let lines = [
        "\x1b[31mError\x1b[0m: \x1b]1337;File=inline=1:base64==\x07",
        "\x1b[33mWarning\x1b[0m: \x1b_Ga=T,data...\x1b\\",
        "\x1b[1mBold\x1b[0m \x1b]1337;File=:base64==\x07\x1b[0m",
    ];

    for line in lines {
        assert!(
            is_image_line(line),
            "Should detect image sequence after ANSI codes: {}...",
            &line[..line.len().min(30)]
        );
    }
}

#[test]
fn does_not_crash_on_very_long_lines_with_image_sequences() {
    let base64_char = "A".repeat(100);
    let iterm2_sequence = "\x1b]1337;File=size=800,600;inline=1:";
    let crash_line = format!("Output: {iterm2_sequence}{} end of output", base64_char.repeat(3040));

    assert!(crash_line.len() > 300_000, "Test line should be > 300KB");
    assert!(
        is_image_line(&crash_line),
        "Should detect image sequence in very long line, preventing TUI crash"
    );
}

#[test]
fn handles_lines_exactly_matching_crash_log_dimensions() {
    let target_width = 58_649usize;
    let prefix = "Text";
    let sequence = "\x1b_Ga=T,f=100";
    let suffix = "End";
    let padding = "A".repeat(target_width - prefix.len() - sequence.len() - suffix.len());
    let line = format!("{prefix}{sequence}{padding}{suffix}");

    assert_eq!(line.len(), 58_649);
    assert!(is_image_line(&line), "Should detect image sequence in 58649-char line");
}

#[test]
fn does_not_detect_images_in_regular_long_text() {
    let long_text = "A".repeat(100_000);
    assert!(
        !is_image_line(&long_text),
        "Should not detect images in plain long text"
    );
}

#[test]
fn does_not_detect_images_in_lines_with_file_paths() {
    let file_paths = [
        "/path/to/1337/image.jpg",
        "/usr/local/bin/File_converter",
        "~/Documents/1337File_backup.png",
        "./_G_test_file.txt",
    ];

    for path in file_paths {
        assert!(
            !is_image_line(path),
            "Should not falsely detect image sequence in path: {path}"
        );
    }
}
