//! Port of packages/tui/test/editor.test.ts
#![allow(clippy::too_many_lines)]

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;
use tempfile::TempDir;

use pi_js::str16;
use pi_tui::autocomplete::{
    ApplyCompletionResult, AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions,
    CombinedAutocompleteProvider, CombinedCommand, GetSuggestionsOptions, SlashCommand,
};
use pi_tui::components::editor::{EditorCursor, EditorOptions, EditorTheme, SegmentData, word_wrap_line};
use pi_tui::{
    Component, Editor, FAKE_CURSOR_START, TUI, TuiMainScreen, render_fake_cursor, strip_terminal_sequences,
    visible_width,
};
use support::test_themes::default_editor_theme;
use support::virtual_terminal::VirtualTerminal;

fn create_test_tui() -> Arc<dyn TUI + Send + Sync> {
    create_test_tui_with_size(80, 24)
}

fn create_test_tui_with_width(cols: i64) -> Arc<dyn TUI + Send + Sync> {
    create_test_tui_with_size(cols, 24)
}

fn create_test_tui_with_size(cols: i64, rows: i64) -> Arc<dyn TUI + Send + Sync> {
    let terminal = Arc::new(VirtualTerminal::new(cols, rows));
    Arc::new(TuiMainScreen::new(terminal, None, None))
}

fn standard_apply_completion(
    lines: &[String],
    cursor_line: i64,
    cursor_col: i64,
    item: &AutocompleteItem,
    prefix: &str,
) -> ApplyCompletionResult {
    let idx = usize::try_from(cursor_line).ok();
    let line = idx.and_then(|i| lines.get(i)).map(String::as_str).unwrap_or("");
    let prefix_len = i64::try_from(str16::len(prefix)).unwrap_or(0);
    let before_end = (cursor_col - prefix_len).max(0);
    let before = str16::slice(line, 0, Some(before_end));
    let after = str16::slice(line, cursor_col.max(0), None);
    let mut new_lines = lines.to_vec();
    if let Some(i) = idx
        && i < new_lines.len()
    {
        new_lines[i] = format!("{before}{}{after}", item.value);
    }
    let value_len = i64::try_from(str16::len(&item.value)).unwrap_or(0);
    ApplyCompletionResult {
        lines: new_lines,
        cursor_line,
        cursor_col: cursor_col - prefix_len + value_len,
    }
}

async fn flush_autocomplete() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

mod prompt_history_navigation {
    use super::*;

    #[test]
    fn does_nothing_on_up_arrow_when_history_is_empty() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.handle_input("\x1b[A"); // Up arrow

        assert_eq!(editor.get_text(), "");
    }

    #[test]
    fn shows_most_recent_history_entry_on_up_arrow_when_editor_is_empty() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.add_to_history("first prompt");
        editor.add_to_history("second prompt");

        editor.handle_input("\x1b[A"); // Up arrow

        assert_eq!(editor.get_text(), "second prompt");
    }

    #[test]
    fn cycles_through_history_entries_on_repeated_up_arrow() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.add_to_history("first");
        editor.add_to_history("second");
        editor.add_to_history("third");

        editor.handle_input("\x1b[A"); // Up - shows "third"
        assert_eq!(editor.get_text(), "third");

        editor.handle_input("\x1b[A"); // Up - shows "second"
        assert_eq!(editor.get_text(), "second");

        editor.handle_input("\x1b[A"); // Up - shows "first"
        assert_eq!(editor.get_text(), "first");

        editor.handle_input("\x1b[A"); // Up - stays at "first" (oldest)
        assert_eq!(editor.get_text(), "first");
    }

    #[test]
    fn jumps_to_start_before_entering_history_from_a_non_empty_draft() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.add_to_history("prompt");
        editor.set_text("draft");
        editor.handle_input("\x1b[D");
        editor.handle_input("\x1b[D");

        editor.handle_input("\x1b[A"); // Up - jumps to start before history browsing
        assert_eq!(editor.get_text(), "draft");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 });

        editor.handle_input("\x1b[A"); // Up at start - shows "prompt"
        assert_eq!(editor.get_text(), "prompt");

        editor.handle_input("\x1b[B"); // Down - restores draft
        assert_eq!(editor.get_text(), "draft");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 });
    }

    #[test]
    fn navigates_forward_through_history_with_down_arrow() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.add_to_history("first");
        editor.add_to_history("second");
        editor.add_to_history("third");
        editor.set_text("draft");

        // Go to oldest
        editor.handle_input("\x1b[A"); // start of draft
        editor.handle_input("\x1b[A"); // third
        editor.handle_input("\x1b[A"); // second
        editor.handle_input("\x1b[A"); // first

        // Navigate back
        editor.handle_input("\x1b[B"); // second
        assert_eq!(editor.get_text(), "second");

        editor.handle_input("\x1b[B"); // third
        assert_eq!(editor.get_text(), "third");

        editor.handle_input("\x1b[B"); // draft
        assert_eq!(editor.get_text(), "draft");
    }

    #[test]
    fn exits_history_mode_when_typing_a_character() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.add_to_history("old prompt");

        editor.handle_input("\x1b[A"); // Up - shows "old prompt"
        editor.handle_input("x"); // Type a character - exits history mode

        assert_eq!(editor.get_text(), "xold prompt");
    }

    #[test]
    fn exits_history_mode_on_set_text() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.add_to_history("first");
        editor.add_to_history("second");

        editor.handle_input("\x1b[A"); // Up - shows "second"
        editor.set_text(""); // External clear

        // Up should start fresh from most recent
        editor.handle_input("\x1b[A");
        assert_eq!(editor.get_text(), "second");
    }

    #[test]
    fn does_not_add_empty_strings_to_history() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.add_to_history("");
        editor.add_to_history("   ");
        editor.add_to_history("valid");

        editor.handle_input("\x1b[A");
        assert_eq!(editor.get_text(), "valid");

        // Should not have more entries
        editor.handle_input("\x1b[A");
        assert_eq!(editor.get_text(), "valid");
    }

    #[test]
    fn does_not_add_consecutive_duplicates_to_history() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.add_to_history("same");
        editor.add_to_history("same");
        editor.add_to_history("same");

        editor.handle_input("\x1b[A"); // "same"
        assert_eq!(editor.get_text(), "same");

        editor.handle_input("\x1b[A"); // stays at "same" (only one entry)
        assert_eq!(editor.get_text(), "same");
    }

    #[test]
    fn allows_non_consecutive_duplicates_in_history() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.add_to_history("first");
        editor.add_to_history("second");
        editor.add_to_history("first"); // Not consecutive, should be added

        editor.handle_input("\x1b[A"); // "first"
        assert_eq!(editor.get_text(), "first");

        editor.handle_input("\x1b[A"); // "second"
        assert_eq!(editor.get_text(), "second");

        editor.handle_input("\x1b[A"); // "first" (older one)
        assert_eq!(editor.get_text(), "first");
    }

    #[test]
    fn uses_cursor_movement_instead_of_history_when_editor_has_content() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.add_to_history("history item");
        editor.set_text("line1\nline2");

        // Cursor is at end of line2, Up should move to line1
        editor.handle_input("\x1b[A"); // Up - cursor movement

        // Insert character to verify cursor position
        editor.handle_input("X");

        // X should be inserted in line1, not replace with history
        assert_eq!(editor.get_text(), "line1X\nline2");
    }

    #[test]
    fn limits_history_to_100_entries() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        // Add 105 entries
        for i in 0..105 {
            editor.add_to_history(&format!("prompt {i}"));
        }

        // Navigate to oldest
        for _ in 0..100 {
            editor.handle_input("\x1b[A");
        }

        // Should be at entry 5 (oldest kept), not entry 0
        assert_eq!(editor.get_text(), "prompt 5");

        // One more Up should not change anything
        editor.handle_input("\x1b[A");
        assert_eq!(editor.get_text(), "prompt 5");
    }

    #[test]
    fn places_cursor_at_start_after_browsing_history_upward() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.add_to_history("older entry");
        editor.add_to_history("line1\nline2\nline3");

        editor.handle_input("\x1b[A"); // Up - shows multi-line entry at start
        assert_eq!(editor.get_text(), "line1\nline2\nline3");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 });

        editor.handle_input("\x1b[A"); // Up again - immediately navigates to older entry
        assert_eq!(editor.get_text(), "older entry");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 });
    }

    #[test]
    fn places_cursor_at_end_after_browsing_history_downward() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.add_to_history("older entry");
        editor.add_to_history("line1\nline2\nline3");
        editor.add_to_history("newer entry");

        editor.handle_input("\x1b[A"); // newer entry
        editor.handle_input("\x1b[A"); // multi-line entry
        editor.handle_input("\x1b[A"); // older entry

        editor.handle_input("\x1b[B"); // Down - shows multi-line entry at end
        assert_eq!(editor.get_text(), "line1\nline2\nline3");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 5 });

        editor.handle_input("\x1b[B"); // Down again - immediately navigates to newer entry
        assert_eq!(editor.get_text(), "newer entry");
    }

    #[test]
    fn allows_opposite_direction_cursor_movement_within_multi_line_history_entry() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.add_to_history("line1\nline2\nline3");

        editor.handle_input("\x1b[A"); // Up - shows entry at start
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 });

        editor.handle_input("\x1b[B"); // Down - cursor moves to line2
        assert_eq!(editor.get_text(), "line1\nline2\nline3");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 1, col: 0 });

        editor.handle_input("\x1b[A"); // Up - cursor moves back to line1
        assert_eq!(editor.get_text(), "line1\nline2\nline3");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 });
    }
}

mod public_state_accessors {
    use super::*;

    #[test]
    fn returns_cursor_position() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 });

        editor.handle_input("a");
        editor.handle_input("b");
        editor.handle_input("c");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 3 });

        editor.handle_input("\x1b[D"); // Left
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 2 });
    }

    #[test]
    fn returns_lines_as_a_defensive_copy() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        editor.set_text("a\nb");

        let mut lines = editor.get_lines();
        assert_eq!(lines, vec!["a", "b"]);

        lines[0] = "mutated".to_string();
        assert_eq!(editor.get_lines(), vec!["a", "b"]);
    }
}

mod backslash_enter_newline_workaround {
    use super::*;

    #[test]
    fn inserts_backslash_immediately_no_buffering() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.handle_input("\\");

        // Backslash should be visible immediately, not buffered
        assert_eq!(editor.get_text(), "\\");
    }

    #[test]
    fn converts_standalone_backslash_to_newline_on_enter() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.handle_input("\\");
        editor.handle_input("\r");

        assert_eq!(editor.get_text(), "\n");
    }

    #[test]
    fn inserts_backslash_normally_when_followed_by_other_characters() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.handle_input("\\");
        editor.handle_input("x");

        assert_eq!(editor.get_text(), "\\x");
    }

    #[test]
    fn does_not_trigger_newline_when_backslash_is_not_immediately_before_cursor() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let submitted = Arc::new(Mutex::new(false));
        let submitted_flag = Arc::clone(&submitted);
        editor.set_on_submit(Some(Arc::new(move |_| {
            *submitted_flag.lock().unwrap() = true;
        })));

        editor.handle_input("\\");
        editor.handle_input("x");
        editor.handle_input("\r");

        // Should submit, not insert newline (backslash not at cursor)
        assert!(*submitted.lock().unwrap());
    }

    #[test]
    fn only_removes_one_backslash_when_multiple_are_present() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.handle_input("\\");
        editor.handle_input("\\");
        editor.handle_input("\\");
        assert_eq!(editor.get_text(), "\\\\\\");

        editor.handle_input("\r");
        // Only the last backslash is removed, newline inserted
        assert_eq!(editor.get_text(), "\\\\\n");
    }
}

mod kitty_csi_u_handling {
    use super::*;

    #[test]
    fn ignores_printable_csi_u_sequences_with_unsupported_modifiers() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.handle_input("\x1b[99;9u");

        assert_eq!(editor.get_text(), "");
    }

    #[test]
    fn inserts_shifted_csi_u_letters_as_text() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.handle_input("\x1b[69;2u");

        assert_eq!(editor.get_text(), "E");
    }

    #[test]
    fn inserts_shifted_xterm_modify_other_keys_letters_as_text() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.handle_input("\x1b[27;2;69~");

        assert_eq!(editor.get_text(), "E");
    }
}

mod unicode_text_editing_behavior {
    use super::*;

    #[test]
    fn inserts_mixed_ascii_umlauts_and_emojis_as_literal_text() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        for ch in ["H", "e", "l", "l", "o", " ", "ä", "ö", "ü", " ", "😀"] {
            editor.handle_input(ch);
        }

        let text = editor.get_text();
        assert_eq!(text, "Hello äöü 😀");
    }

    #[test]
    fn deletes_single_code_unit_unicode_characters_umlauts_with_backspace() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.handle_input("ä");
        editor.handle_input("ö");
        editor.handle_input("ü");

        // Delete the last character (ü)
        editor.handle_input("\x7f"); // Backspace

        let text = editor.get_text();
        assert_eq!(text, "äö");
    }

    #[test]
    fn deletes_multi_code_unit_emojis_with_single_backspace() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.handle_input("😀");
        editor.handle_input("👍");

        // Delete the last emoji (👍) - single backspace deletes whole grapheme cluster
        editor.handle_input("\x7f"); // Backspace

        let text = editor.get_text();
        assert_eq!(text, "😀");
    }

    #[test]
    fn inserts_characters_at_the_correct_position_after_cursor_movement_over_umlauts() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.handle_input("ä");
        editor.handle_input("ö");
        editor.handle_input("ü");

        // Move cursor left twice
        editor.handle_input("\x1b[D"); // Left arrow
        editor.handle_input("\x1b[D"); // Left arrow

        // Insert 'x' in the middle
        editor.handle_input("x");

        let text = editor.get_text();
        assert_eq!(text, "äxöü");
    }

    #[test]
    fn moves_cursor_across_multi_code_unit_emojis_with_single_arrow_key() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.handle_input("😀");
        editor.handle_input("👍");
        editor.handle_input("🎉");

        // Move cursor left over last emoji (🎉) - single arrow moves over whole grapheme
        editor.handle_input("\x1b[D"); // Left arrow

        // Move cursor left over second emoji (👍)
        editor.handle_input("\x1b[D");

        // Insert 'x' between first and second emoji
        editor.handle_input("x");

        let text = editor.get_text();
        assert_eq!(text, "😀x👍🎉");
    }

    #[test]
    fn preserves_umlauts_across_line_breaks() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.handle_input("ä");
        editor.handle_input("ö");
        editor.handle_input("ü");
        editor.handle_input("\n"); // new line
        editor.handle_input("Ä");
        editor.handle_input("Ö");
        editor.handle_input("Ü");

        let text = editor.get_text();
        assert_eq!(text, "äöü\nÄÖÜ");
    }

    #[test]
    fn replaces_the_entire_document_with_unicode_text_via_set_text_paste_simulation() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        // Simulate bracketed paste / programmatic replacement
        editor.set_text("Hällö Wörld! 😀 äöüÄÖÜß");

        let text = editor.get_text();
        assert_eq!(text, "Hällö Wörld! 😀 äöüÄÖÜß");
    }

    #[test]
    fn moves_cursor_to_document_start_on_ctrl_a_and_inserts_at_the_beginning() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.handle_input("a");
        editor.handle_input("b");
        editor.handle_input("\x01"); // Ctrl+A (move to start)
        editor.handle_input("x"); // Insert at start

        let text = editor.get_text();
        assert_eq!(text, "xab");
    }

    #[test]
    fn deletes_words_correctly_with_ctrl_w_and_alt_backspace() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        // Basic word deletion
        editor.set_text("foo bar baz");
        editor.handle_input("\x17"); // Ctrl+W
        assert_eq!(editor.get_text(), "foo bar ");

        // Trailing whitespace
        editor.set_text("foo bar   ");
        editor.handle_input("\x17");
        assert_eq!(editor.get_text(), "foo ");

        // Punctuation run
        editor.set_text("foo bar...");
        editor.handle_input("\x17");
        assert_eq!(editor.get_text(), "foo bar");

        // ASCII punctuation inside Intl word-like segments preserves old boundaries
        editor.set_text("foo.bar");
        editor.handle_input("\x17");
        assert_eq!(editor.get_text(), "foo.");

        editor.set_text("foo:bar");
        editor.handle_input("\x17");
        assert_eq!(editor.get_text(), "foo:");

        // Delete across multiple lines
        editor.set_text("line one\nline two");
        editor.handle_input("\x17");
        assert_eq!(editor.get_text(), "line one\nline ");

        // Delete empty line (merge)
        editor.set_text("line one\n");
        editor.handle_input("\x17");
        assert_eq!(editor.get_text(), "line one");

        // Grapheme safety (emoji as a word)
        editor.set_text("foo 😀😀 bar");
        editor.handle_input("\x17");
        assert_eq!(editor.get_text(), "foo 😀😀 ");
        editor.handle_input("\x17");
        assert_eq!(editor.get_text(), "foo ");

        // Alt+Backspace
        editor.set_text("foo bar");
        editor.handle_input("\x1b\x7f"); // Alt+Backspace (legacy)
        assert_eq!(editor.get_text(), "foo ");
    }

    #[test]
    fn navigates_words_correctly_with_ctrl_left_right() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("foo bar... baz");
        // Cursor at end

        // Move left over baz
        editor.handle_input("\x1b[1;5D"); // Ctrl+Left
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 11 }); // after '...'

        // Move left over punctuation
        editor.handle_input("\x1b[1;5D"); // Ctrl+Left
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 7 }); // after 'bar'

        // Move left over bar
        editor.handle_input("\x1b[1;5D"); // Ctrl+Left
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 4 }); // after 'foo '

        // Move right over bar
        editor.handle_input("\x1b[1;5C"); // Ctrl+Right
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 7 }); // at end of 'bar'

        // Move right over punctuation run
        editor.handle_input("\x1b[1;5C"); // Ctrl+Right
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 10 }); // after '...'

        // Move right skips space and lands after baz
        editor.handle_input("\x1b[1;5C"); // Ctrl+Right
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 14 }); // end of line

        // Test forward from start with leading whitespace
        editor.set_text("   foo bar");
        editor.handle_input("\x01"); // Ctrl+A to go to start
        editor.handle_input("\x1b[1;5C"); // Ctrl+Right
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 6 }); // after 'foo'

        // ASCII punctuation inside Intl word-like segments preserves old boundaries
        editor.set_text("foo.bar baz");
        editor.handle_input("\x1b[1;5D"); // Ctrl+Left over baz
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 8 });
        editor.handle_input("\x1b[1;5D"); // Ctrl+Left over bar
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 4 });
        editor.handle_input("\x1b[1;5D"); // Ctrl+Left over .
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 3 });

        editor.handle_input("\x01"); // Ctrl+A
        editor.handle_input("\x1b[1;5C"); // Ctrl+Right over foo
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 3 });
        editor.handle_input("\x1b[1;5C"); // Ctrl+Right over .
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 4 });
        editor.handle_input("\x1b[1;5C"); // Ctrl+Right over bar
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 7 });
    }

    #[test]
    fn stops_at_fullwidth_chinese_punctuation_issue_4972() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        // 你好，世界 = 你好(0-2) ，(2-3) 世界(3-5)
        editor.set_text("你好，世界");
        // Cursor at end (col 5)

        // Move left over 世界
        editor.handle_input("\x1b[1;5D"); // Ctrl+Left
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 3 }); // after ，

        // Move left over ，
        editor.handle_input("\x1b[1;5D"); // Ctrl+Left
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 2 }); // after 你好

        // Move left over 你好
        editor.handle_input("\x1b[1;5D"); // Ctrl+Left
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 }); // start

        // Move right over 你好
        editor.handle_input("\x1b[1;5C"); // Ctrl+Right
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 2 }); // after 你好

        // Move right over ，
        editor.handle_input("\x1b[1;5C"); // Ctrl+Right
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 3 }); // after ，

        // Move right over 世界
        editor.handle_input("\x1b[1;5C"); // Ctrl+Right
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 5 }); // end
    }

    #[test]
    fn handles_mixed_cjk_and_ascii_word_movement() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        // "hello你好，world世界" = hello(0-5) 你好(5-7) ，(7-8) world(8-13) 世界(13-15)
        editor.set_text("hello你好，world世界");
        // Cursor at end (col 15)

        // Move left over 世界
        editor.handle_input("\x1b[1;5D"); // Ctrl+Left
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 13 }); // after 'world'

        // Move left over world
        editor.handle_input("\x1b[1;5D"); // Ctrl+Left
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 8 }); // after ，

        // Move left over ，
        editor.handle_input("\x1b[1;5D"); // Ctrl+Left
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 7 }); // after 你好

        // Move left over 你好
        editor.handle_input("\x1b[1;5D"); // Ctrl+Left
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 5 }); // after 'hello'

        // Move left over hello
        editor.handle_input("\x1b[1;5D"); // Ctrl+Left
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 }); // start

        // Forward from start
        editor.handle_input("\x1b[1;5C"); // Ctrl+Right
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 5 }); // after 'hello'

        editor.handle_input("\x1b[1;5C"); // Ctrl+Right
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 7 }); // after 你好

        editor.handle_input("\x1b[1;5C"); // Ctrl+Right
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 8 }); // after ，

        editor.handle_input("\x1b[1;5C"); // Ctrl+Right
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 13 }); // after 'world'

        editor.handle_input("\x1b[1;5C"); // Ctrl+Right
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 15 }); // end
    }
}

mod scroll_indicators {
    use super::*;

    #[test]
    fn centers_scroll_indicators_on_wide_borders() {
        let width = 40;
        let editor = Editor::new(create_test_tui_with_width(width as i64), default_editor_theme(), None);
        let text = (0..20)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        editor.set_text(&text);

        editor.render(width);
        for _ in 0..10 {
            editor.handle_input("\x1b[A");
        }

        let lines = editor.render(width);
        assert_eq!(
            strip_terminal_sequences(&lines[0]),
            format!("{} ↑ 9 more {}", "─".repeat(15), "─".repeat(15))
        );
        assert_eq!(
            strip_terminal_sequences(lines.last().unwrap()),
            format!("{} ↓ 4 more {}", "─".repeat(15), "─".repeat(15))
        );
    }

    #[test]
    fn keeps_truncated_scroll_indicators_within_width_and_preserves_their_color_issue_6962() {
        let width = 10;
        let border_color: Arc<dyn Fn(&str) -> String + Send + Sync> =
            Arc::new(|text| format!("\x1b[35m{text}\x1b[39m"));
        let mut theme: EditorTheme = default_editor_theme();
        theme.border_color = Arc::clone(&border_color);
        let editor = Editor::new(create_test_tui_with_width(width as i64), theme, None);
        let text = (0..20)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        editor.set_text(&text);

        // Render once to initialize wrapping, then move the cursor so content remains above and below the viewport.
        editor.render(width);
        for _ in 0..10 {
            editor.handle_input("\x1b[A");
        }

        let lines = editor.render(width);
        let top_border = &lines[0];
        let bottom_border = lines.last().unwrap();

        let top_stripped = strip_terminal_sequences(top_border);
        let bottom_stripped = strip_terminal_sequences(bottom_border);
        assert!(Regex::new(r"^─── ↑").unwrap().is_match(&top_stripped));
        assert!(Regex::new(r"^─── ↓").unwrap().is_match(&bottom_stripped));
        assert_eq!(*top_border, border_color(&top_stripped));
        assert_eq!(*bottom_border, border_color(&bottom_stripped));
        for line in &lines {
            assert_eq!(
                visible_width(line),
                width as i64,
                "line exceeds width {width}: {line:?}"
            );
        }
    }
}

mod grapheme_aware_text_wrapping {
    use super::*;

    #[test]
    fn wraps_lines_correctly_when_text_contains_wide_emojis() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let width = 20;

        // ✅ is 2 columns wide, so "Hello ✅ World" is 14 columns
        editor.set_text("Hello ✅ World");
        let lines = editor.render(width);

        // All content lines (between borders) should fit within width
        for (i, line) in lines[1..lines.len() - 1].iter().enumerate() {
            let line_width = visible_width(line);
            assert_eq!(
                line_width,
                width as i64,
                "Line {} has width {line_width}, expected {width}",
                i + 1
            );
        }
    }

    #[test]
    fn wraps_long_text_with_emojis_at_correct_positions() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let width = 10;

        // Each ✅ is 2 columns. "✅✅✅✅✅" = 10 columns, fits exactly
        // "✅✅✅✅✅✅" = 12 columns, needs wrap
        editor.set_text("✅✅✅✅✅✅");
        let lines = editor.render(width);

        // Should have 2 content lines (plus 2 border lines)
        // First line: 5 emojis (10 cols), second line: 1 emoji (2 cols) + padding
        for (i, line) in lines[1..lines.len() - 1].iter().enumerate() {
            let line_width = visible_width(line);
            assert_eq!(
                line_width,
                width as i64,
                "Line {} has width {line_width}, expected {width}",
                i + 1
            );
        }
    }

    #[test]
    fn renders_isolated_thai_and_lao_am_clusters_without_width_drift() {
        for text in ["ำabc", "ຳabc"] {
            let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
            let width = 8;
            editor.set_text(text);

            for line in editor.render(width) {
                assert_eq!(
                    visible_width(&line),
                    width as i64,
                    "line width drift for {text:?}: {line}"
                );
            }
        }
    }

    #[test]
    fn wraps_cjk_characters_correctly_each_is_2_columns_wide() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let width = 10 + 1; // +1 col reserved for cursor

        // Each CJK char is 2 columns. "日本語テスト" = 6 chars = 12 columns
        editor.set_text("日本語テスト");
        let lines = editor.render(width);

        for (i, line) in lines[1..lines.len() - 1].iter().enumerate() {
            let line_width = visible_width(line);
            assert_eq!(
                line_width,
                width as i64,
                "Line {} has width {line_width}, expected {width}",
                i + 1
            );
        }

        // Verify content split correctly
        let content_lines: Vec<String> = lines[1..lines.len() - 1]
            .iter()
            .map(|l| str16::trim(&strip_terminal_sequences(l)).to_string())
            .collect();
        assert_eq!(content_lines.len(), 2);
        assert_eq!(content_lines[0], "日本語テス"); // 5 chars = 10 columns
        assert_eq!(content_lines[1], "ト"); // 1 char = 2 columns (+ padding)
    }

    #[test]
    fn handles_mixed_ascii_and_wide_characters_in_wrapping() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let width = 15 + 1; // +1 col reserved for cursor

        // "Test ✅ OK 日本" = 4 + 1 + 2 + 1 + 2 + 1 + 4 = 15 columns (fits in width-1=15)
        editor.set_text("Test ✅ OK 日本");
        let lines = editor.render(width);

        // Should fit in one content line
        let content_lines = &lines[1..lines.len() - 1];
        assert_eq!(content_lines.len(), 1);

        let line_width = visible_width(&content_lines[0]);
        assert_eq!(line_width, width as i64);
    }

    #[test]
    fn renders_cursor_correctly_on_wide_characters() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let width = 20;

        editor.set_text("A✅B");
        // Cursor should be at end (after B)
        let lines = editor.render(width);

        let content_line = &lines[1];
        assert!(content_line.contains(FAKE_CURSOR_START), "Should have fake cursor");

        // Line should still be correct width
        assert_eq!(visible_width(content_line), width as i64);
    }

    #[test]
    fn does_not_exceed_terminal_width_with_emoji_at_wrap_boundary() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let width = 11;

        // "0123456789✅" = 10 ASCII + 2-wide emoji = 12 columns
        // Should wrap before the emoji since it would exceed width
        editor.set_text("0123456789✅");
        let lines = editor.render(width);

        for (i, line) in lines[1..lines.len() - 1].iter().enumerate() {
            let line_width = visible_width(line);
            assert!(
                line_width <= width as i64,
                "Line {} has width {line_width}, exceeds max {width}",
                i + 1
            );
        }
    }

    #[test]
    fn shows_cursor_at_end_of_line_before_wrap_wraps_on_next_char() {
        let width = 10usize;
        for padding_x in [0usize, 1usize] {
            let editor = Editor::new(
                create_test_tui_with_width((width + padding_x) as i64),
                default_editor_theme(),
                Some(EditorOptions {
                    padding_x: Some(padding_x as f64),
                    autocomplete_max_visible: None,
                }),
            );

            // Type 9 chars -> fills layoutWidth exactly, cursor at end on same line
            for _ in 0..9 {
                editor.handle_input("a");
            }
            let mut lines = editor.render(width + padding_x);
            let mut content_lines = &lines[1..lines.len() - 1];
            assert_eq!(content_lines.len(), 1, "Should be 1 content line before wrap");
            let fake_space = render_fake_cursor(" ");
            assert!(
                content_lines[0].ends_with(&fake_space),
                "Cursor should be at end of line"
            );

            // Type 1 more -> text wraps to second line
            editor.handle_input("a");
            lines = editor.render(width + padding_x);
            content_lines = &lines[1..lines.len() - 1];
            assert_eq!(content_lines.len(), 2, "Should wrap to 2 content lines");
        }
    }
}

mod word_wrapping {
    use super::*;

    #[test]
    fn wraps_at_word_boundaries_instead_of_mid_word() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let width = 40;

        editor.set_text("Hello world this is a test of word wrapping functionality");
        let lines = editor.render(width);

        // Get content lines (between borders)
        let content_lines: Vec<String> = lines[1..lines.len() - 1]
            .iter()
            .map(|l| str16::trim(&strip_terminal_sequences(l)).to_string())
            .collect();

        // Should NOT break mid-word
        // Line 1 should end with a complete word
        assert!(
            !content_lines[0].ends_with('-'),
            "Line should not end with hyphen (mid-word break)"
        );

        // Each content line should be complete words
        let re = Regex::new(r"^[\w.,!?;:]$").unwrap();
        for line in &content_lines {
            // Words at end of line should be complete (no partial words)
            let trimmed_end = str16::trim_end(line);
            let last_char = str16::slice(trimmed_end, -1, None);
            assert!(
                last_char.is_empty() || re.is_match(&last_char),
                "Line ends unexpectedly with: \"{last_char}\""
            );
        }
    }

    #[test]
    fn does_not_start_lines_with_leading_whitespace_after_word_wrap() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let width = 20;

        editor.set_text("Word1 Word2 Word3 Word4 Word5 Word6");
        let lines = editor.render(width);

        // Get content lines (between borders)
        let content_lines = &lines[1..lines.len() - 1];
        let re = Regex::new(r"^\s+\S").unwrap();

        // No line should start with whitespace (except for padding at the end)
        for (i, raw_line) in content_lines.iter().enumerate() {
            let line = strip_terminal_sequences(raw_line);
            let trimmed_start = str16::trim_start(&line);
            // The line should either be all padding or start with a word character
            if !trimmed_start.is_empty() {
                assert!(
                    !re.is_match(str16::trim_end(&line)),
                    "Line {i} starts with unexpected whitespace before content"
                );
            }
        }
    }

    #[test]
    fn breaks_long_words_urls_at_character_level() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let width = 30;

        editor.set_text("Check https://example.com/very/long/path/that/exceeds/width here");
        let lines = editor.render(width);

        // All lines should fit within width
        for (i, line) in lines[1..lines.len() - 1].iter().enumerate() {
            let line_width = visible_width(line);
            assert_eq!(
                line_width,
                width as i64,
                "Line {} has width {line_width}, expected {width}",
                i + 1
            );
        }
    }

    #[test]
    fn preserves_multiple_spaces_within_words_on_same_line() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let width = 50;

        editor.set_text("Word1   Word2    Word3");
        let lines = editor.render(width);

        let content_line = str16::trim(&strip_terminal_sequences(&lines[1])).to_string();
        // Multiple spaces should be preserved
        assert!(
            content_line.contains("Word1   Word2"),
            "Multiple spaces should be preserved"
        );
    }

    #[test]
    fn handles_empty_string() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let width = 40;

        editor.set_text("");
        let lines = editor.render(width);

        // Should have border + empty content + border
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn handles_single_word_that_fits_exactly() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let width = 10 + 1; // +1 col reserved for cursor

        editor.set_text("1234567890");
        let lines = editor.render(width);

        // Should have exactly 3 lines (top border, content, bottom border)
        assert_eq!(lines.len(), 3);
        let content_line = strip_terminal_sequences(&lines[1]);
        assert!(content_line.contains("1234567890"), "Content should contain the word");
    }

    #[test]
    fn wraps_word_to_next_line_when_it_ends_exactly_at_terminal_width() {
        // "hello " (6) + "world" (5) = 11, but "world" is non-whitespace ending at width.
        // Thus, wrap it to next line. The trailing space stays with "hello" on line 1
        let chunks = word_wrap_line("hello world test", 11, None);

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, "hello ");
        assert_eq!(chunks[1].text, "world test");
    }

    #[test]
    fn keeps_whitespace_at_terminal_width_boundary_on_same_line() {
        // "hello world " is exactly 12 chars (including trailing space)
        // The space at position 12 should stay on the first line
        let chunks = word_wrap_line("hello world test", 12, None);

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, "hello world ");
        assert_eq!(chunks[1].text, "test");
    }

    #[test]
    fn handles_unbreakable_word_filling_width_exactly_followed_by_space() {
        let chunks = word_wrap_line("aaaaaaaaaaaa aaaa", 12, None);

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, "aaaaaaaaaaaa");
        assert_eq!(chunks[1].text, " aaaa");
    }

    #[test]
    fn wraps_word_to_next_line_when_it_fits_width_but_not_remaining_space() {
        let chunks = word_wrap_line("      aaaaaaaaaaaa", 12, None);

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, "      ");
        assert_eq!(chunks[1].text, "aaaaaaaaaaaa");
    }

    #[test]
    fn keeps_word_with_multi_space_and_following_word_together_when_they_fit() {
        let chunks = word_wrap_line("Lorem ipsum dolor sit amet,    consectetur", 30, None);

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, "Lorem ipsum dolor sit ");
        assert_eq!(chunks[1].text, "amet,    consectetur");
    }

    #[test]
    fn keeps_word_with_multi_space_and_following_word_when_they_fill_width_exactly() {
        let chunks = word_wrap_line("Lorem ipsum dolor sit amet,              consectetur", 30, None);

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, "Lorem ipsum dolor sit ");
        assert_eq!(chunks[1].text, "amet,              consectetur");
    }

    #[test]
    fn splits_when_word_plus_multi_space_plus_word_exceeds_width() {
        let chunks = word_wrap_line("Lorem ipsum dolor sit amet,               consectetur", 30, None);

        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].text, "Lorem ipsum dolor sit ");
        assert_eq!(chunks[1].text, "amet,               ");
        assert_eq!(chunks[2].text, "consectetur");
    }

    #[test]
    fn breaks_long_whitespace_at_line_boundary() {
        let chunks = word_wrap_line(
            "Lorem ipsum dolor sit amet,                         consectetur",
            30,
            None,
        );

        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].text, "Lorem ipsum dolor sit ");
        assert_eq!(chunks[1].text, "amet,                         ");
        assert_eq!(chunks[2].text, "consectetur");
    }

    #[test]
    fn breaks_long_whitespace_at_line_boundary_2() {
        let chunks = word_wrap_line(
            "Lorem ipsum dolor sit amet,                          consectetur",
            30,
            None,
        );

        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].text, "Lorem ipsum dolor sit ");
        assert_eq!(chunks[1].text, "amet,                         ");
        assert_eq!(chunks[2].text, " consectetur");
    }

    #[test]
    fn breaks_whitespace_spanning_full_lines() {
        let chunks = word_wrap_line(
            "Lorem ipsum dolor sit amet,                                     consectetur",
            30,
            None,
        );

        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].text, "Lorem ipsum dolor sit ");
        assert_eq!(chunks[1].text, "amet,                         ");
        assert_eq!(chunks[2].text, "            consectetur");
    }

    #[test]
    fn force_breaks_when_wide_char_after_word_boundary_wrap_still_overflows() {
        // " " (1) + "a"*186 (186) + "你" (2) = 189 visible width
        // maxWidth = 187: backtracking to the space would leave 186 + 2 = 188 > 187,
        // so the algorithm must force-break before the wide char instead.
        let line = format!(" {}你", "a".repeat(186));
        let chunks = word_wrap_line(&line, 187, None);

        for chunk in &chunks {
            let preview = str16::slice(&chunk.text, 0, Some(20));
            assert!(
                visible_width(&chunk.text) <= 187,
                "chunk \"{preview}...\" has visible width {}, expected <= 187",
                visible_width(&chunk.text)
            );
        }
        // Verify no content is lost
        let reconstructed: String = chunks
            .iter()
            .map(|c| str16::slice(&line, c.start_index, Some(c.end_index)))
            .collect();
        assert_eq!(reconstructed, line);
    }

    #[test]
    fn splits_oversized_atomic_segment_across_multiple_chunks() {
        // Simulate a paste marker wider than maxWidth by passing pre-segmented data
        let marker = "[paste #1 +20 lines]"; // 21 chars
        let line = format!("A{marker}B");
        let marker_len = i64::try_from(str16::len(marker)).unwrap();
        let segments = vec![
            SegmentData {
                segment: "A".to_string(),
                index: 0,
                input: line.clone(),
                is_word_like: None,
            },
            SegmentData {
                segment: marker.to_string(),
                index: 1,
                input: line.clone(),
                is_word_like: None,
            },
            SegmentData {
                segment: "B".to_string(),
                index: 1 + marker_len,
                input: line.clone(),
                is_word_like: None,
            },
        ];

        let chunks = word_wrap_line(&line, 10, Some(&segments));

        // Every chunk must fit within maxWidth
        for chunk in &chunks {
            assert!(
                visible_width(&chunk.text) <= 10,
                "chunk \"{}\" has visible width {}, expected <= 10",
                chunk.text,
                visible_width(&chunk.text)
            );
        }

        // Verify no content is lost
        let reconstructed: String = chunks
            .iter()
            .map(|c| str16::slice(&line, c.start_index, Some(c.end_index)))
            .collect();
        assert_eq!(reconstructed, line);
    }

    #[test]
    fn splits_oversized_atomic_segment_at_start_of_line() {
        let marker = "[paste #1 +20 lines]"; // 21 chars
        let line = format!("{marker}B");
        let marker_len = i64::try_from(str16::len(marker)).unwrap();
        let segments = vec![
            SegmentData {
                segment: marker.to_string(),
                index: 0,
                input: line.clone(),
                is_word_like: None,
            },
            SegmentData {
                segment: "B".to_string(),
                index: marker_len,
                input: line.clone(),
                is_word_like: None,
            },
        ];

        let chunks = word_wrap_line(&line, 10, Some(&segments));

        for chunk in &chunks {
            assert!(visible_width(&chunk.text) <= 10);
        }
        // "B" ends up on the last line (either alone or with the marker tail)
        assert!(chunks.last().unwrap().text.contains('B'));

        let reconstructed: String = chunks
            .iter()
            .map(|c| str16::slice(&line, c.start_index, Some(c.end_index)))
            .collect();
        assert_eq!(reconstructed, line);
    }

    #[test]
    fn splits_oversized_atomic_segment_at_end_of_line() {
        let marker = "[paste #1 +20 lines]"; // 21 chars
        let line = format!("A{marker}");
        let segments = vec![
            SegmentData {
                segment: "A".to_string(),
                index: 0,
                input: line.clone(),
                is_word_like: None,
            },
            SegmentData {
                segment: marker.to_string(),
                index: 1,
                input: line.clone(),
                is_word_like: None,
            },
        ];

        let chunks = word_wrap_line(&line, 10, Some(&segments));

        for chunk in &chunks {
            assert!(visible_width(&chunk.text) <= 10);
        }
        assert_eq!(chunks[0].text, "A");

        let reconstructed: String = chunks
            .iter()
            .map(|c| str16::slice(&line, c.start_index, Some(c.end_index)))
            .collect();
        assert_eq!(reconstructed, line);
    }

    #[test]
    fn splits_consecutive_oversized_atomic_segments() {
        let m1 = "[paste #1 +20 lines]"; // 21 chars
        let m2 = "[paste #2 +30 lines]"; // 21 chars
        let line = format!("{m1}{m2}");
        let m1_len = i64::try_from(str16::len(m1)).unwrap();
        let segments = vec![
            SegmentData {
                segment: m1.to_string(),
                index: 0,
                input: line.clone(),
                is_word_like: None,
            },
            SegmentData {
                segment: m2.to_string(),
                index: m1_len,
                input: line.clone(),
                is_word_like: None,
            },
        ];

        let chunks = word_wrap_line(&line, 10, Some(&segments));

        for chunk in &chunks {
            assert!(
                visible_width(&chunk.text) <= 10,
                "chunk \"{}\" has visible width {}, expected <= 10",
                chunk.text,
                visible_width(&chunk.text)
            );
        }

        let reconstructed: String = chunks
            .iter()
            .map(|c| str16::slice(&line, c.start_index, Some(c.end_index)))
            .collect();
        assert_eq!(reconstructed, line);
    }

    #[test]
    fn wraps_normally_after_oversized_atomic_segment() {
        let marker = "[paste #1 +20 lines]"; // 21 chars
        let line = format!("{marker} hello world");
        let marker_len = i64::try_from(str16::len(marker)).unwrap();
        let mut segments = vec![SegmentData {
            segment: marker.to_string(),
            index: 0,
            input: line.clone(),
            is_word_like: None,
        }];
        for (offset, ch) in " hello world".chars().enumerate() {
            segments.push(SegmentData {
                segment: ch.to_string(),
                index: marker_len + i64::try_from(offset).unwrap(),
                input: line.clone(),
                is_word_like: None,
            });
        }

        let chunks = word_wrap_line(&line, 10, Some(&segments));

        // All chunks must fit
        for chunk in &chunks {
            assert!(
                visible_width(&chunk.text) <= 10,
                "chunk \"{}\" has visible width {}, expected <= 10",
                chunk.text,
                visible_width(&chunk.text)
            );
        }

        // Last chunk should contain "world" (normal wrapping resumes)
        assert_eq!(chunks.last().unwrap().text, "world");

        let reconstructed: String = chunks
            .iter()
            .map(|c| str16::slice(&line, c.start_index, Some(c.end_index)))
            .collect();
        assert_eq!(reconstructed, line);
    }
}

mod kill_ring {
    use super::*;

    #[test]
    fn ctrl_w_saves_deleted_text_to_kill_ring_and_ctrl_y_yanks_it() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("foo bar baz");
        editor.handle_input("\x17"); // Ctrl+W - deletes "baz"
        assert_eq!(editor.get_text(), "foo bar ");

        // Move to beginning and yank
        editor.handle_input("\x01"); // Ctrl+A
        editor.handle_input("\x19"); // Ctrl+Y
        assert_eq!(editor.get_text(), "bazfoo bar ");
    }

    #[test]
    fn ctrl_u_saves_deleted_text_to_kill_ring() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("hello world");
        // Move cursor to middle
        editor.handle_input("\x01"); // Ctrl+A (start)
        for _ in 0..6 {
            editor.handle_input("\x1b[C"); // After "hello "
        }

        editor.handle_input("\x15"); // Ctrl+U - deletes "hello "
        assert_eq!(editor.get_text(), "world");

        editor.handle_input("\x19"); // Ctrl+Y
        assert_eq!(editor.get_text(), "hello world");
    }

    #[test]
    fn ctrl_k_saves_deleted_text_to_kill_ring() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("hello world");
        editor.handle_input("\x01"); // Ctrl+A (start)
        editor.handle_input("\x0b"); // Ctrl+K - deletes "hello world"

        assert_eq!(editor.get_text(), "");

        editor.handle_input("\x19"); // Ctrl+Y
        assert_eq!(editor.get_text(), "hello world");
    }

    #[test]
    fn ctrl_y_does_nothing_when_kill_ring_is_empty() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("test");
        editor.handle_input("\x19"); // Ctrl+Y
        assert_eq!(editor.get_text(), "test");
    }

    #[test]
    fn alt_y_cycles_through_kill_ring_after_ctrl_y() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        // Create kill ring with multiple entries
        editor.set_text("first");
        editor.handle_input("\x17"); // Ctrl+W - deletes "first"
        editor.set_text("second");
        editor.handle_input("\x17"); // Ctrl+W - deletes "second"
        editor.set_text("third");
        editor.handle_input("\x17"); // Ctrl+W - deletes "third"

        // Kill ring now has: [first, second, third]
        assert_eq!(editor.get_text(), "");

        editor.handle_input("\x19"); // Ctrl+Y - yanks "third" (most recent)
        assert_eq!(editor.get_text(), "third");

        editor.handle_input("\x1by"); // Alt+Y - cycles to "second"
        assert_eq!(editor.get_text(), "second");

        editor.handle_input("\x1by"); // Alt+Y - cycles to "first"
        assert_eq!(editor.get_text(), "first");

        editor.handle_input("\x1by"); // Alt+Y - cycles back to "third"
        assert_eq!(editor.get_text(), "third");
    }

    #[test]
    fn alt_y_does_nothing_if_not_preceded_by_yank() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("test");
        editor.handle_input("\x17"); // Ctrl+W - deletes "test"
        editor.set_text("other");

        // Type something to break the yank chain
        editor.handle_input("x");
        assert_eq!(editor.get_text(), "otherx");

        // Alt+Y should do nothing
        editor.handle_input("\x1by"); // Alt+Y
        assert_eq!(editor.get_text(), "otherx");
    }

    #[test]
    fn alt_y_does_nothing_if_kill_ring_has_le_1_entry() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("only");
        editor.handle_input("\x17"); // Ctrl+W - deletes "only"

        editor.handle_input("\x19"); // Ctrl+Y - yanks "only"
        assert_eq!(editor.get_text(), "only");

        editor.handle_input("\x1by"); // Alt+Y - should do nothing (only 1 entry)
        assert_eq!(editor.get_text(), "only");
    }

    #[test]
    fn consecutive_ctrl_w_accumulates_into_one_kill_ring_entry() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("one two three");
        editor.handle_input("\x17"); // Ctrl+W - deletes "three"
        editor.handle_input("\x17"); // Ctrl+W - deletes "two " (prepended)
        editor.handle_input("\x17"); // Ctrl+W - deletes "one " (prepended)

        assert_eq!(editor.get_text(), "");

        // Should be one combined entry
        editor.handle_input("\x19"); // Ctrl+Y
        assert_eq!(editor.get_text(), "one two three");
    }

    #[test]
    fn ctrl_u_accumulates_multiline_deletes_including_newlines() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        // Start with multiline text, cursor at end
        editor.set_text("line1\nline2\nline3");
        // Cursor is at end of line3 (line 2, col 5)

        // Delete "line3"
        editor.handle_input("\x15"); // Ctrl+U
        assert_eq!(editor.get_text(), "line1\nline2\n");

        // Delete newline (at start of empty line 2, merges with line1)
        editor.handle_input("\x15"); // Ctrl+U
        assert_eq!(editor.get_text(), "line1\nline2");

        // Delete "line2"
        editor.handle_input("\x15"); // Ctrl+U
        assert_eq!(editor.get_text(), "line1\n");

        // Delete newline
        editor.handle_input("\x15"); // Ctrl+U
        assert_eq!(editor.get_text(), "line1");

        // Delete "line1"
        editor.handle_input("\x15"); // Ctrl+U
        assert_eq!(editor.get_text(), "");

        // All deletions accumulated into one entry: "line1\nline2\nline3"
        editor.handle_input("\x19"); // Ctrl+Y
        assert_eq!(editor.get_text(), "line1\nline2\nline3");
    }

    #[test]
    fn backward_deletions_prepend_forward_deletions_append_during_accumulation() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("prefix|suffix");
        // Position cursor at |
        editor.handle_input("\x01"); // Ctrl+A
        for _ in 0..6 {
            editor.handle_input("\x1b[C"); // Move right 6 times
        }

        editor.handle_input("\x0b"); // Ctrl+K - deletes "suffix" (forward)
        editor.handle_input("\x0b"); // Ctrl+K - deletes "|" (forward, appended)
        assert_eq!(editor.get_text(), "prefix");

        editor.handle_input("\x19"); // Ctrl+Y
        assert_eq!(editor.get_text(), "prefix|suffix");
    }

    #[test]
    fn non_delete_actions_break_kill_accumulation() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        // Delete "baz", then type "x" to break accumulation, then delete "x"
        editor.set_text("foo bar baz");
        editor.handle_input("\x17"); // Ctrl+W - deletes "baz"
        assert_eq!(editor.get_text(), "foo bar ");

        editor.handle_input("x"); // Typing breaks accumulation
        assert_eq!(editor.get_text(), "foo bar x");

        editor.handle_input("\x17"); // Ctrl+W - deletes "x" (separate entry, not accumulated)
        assert_eq!(editor.get_text(), "foo bar ");

        // Yank most recent - should be "x", not "xbaz"
        editor.handle_input("\x19"); // Ctrl+Y
        assert_eq!(editor.get_text(), "foo bar x");

        // Cycle to previous - should be "baz" (separate entry)
        editor.handle_input("\x1by"); // Alt+Y
        assert_eq!(editor.get_text(), "foo bar baz");
    }

    #[test]
    fn non_yank_actions_break_alt_y_chain() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("first");
        editor.handle_input("\x17"); // Ctrl+W
        editor.set_text("second");
        editor.handle_input("\x17"); // Ctrl+W
        editor.set_text("");

        editor.handle_input("\x19"); // Ctrl+Y - yanks "second"
        assert_eq!(editor.get_text(), "second");

        editor.handle_input("x"); // Type breaks yank chain
        assert_eq!(editor.get_text(), "secondx");

        editor.handle_input("\x1by"); // Alt+Y - should do nothing
        assert_eq!(editor.get_text(), "secondx");
    }

    #[test]
    fn kill_ring_rotation_persists_after_cycling() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("first");
        editor.handle_input("\x17"); // deletes "first"
        editor.set_text("second");
        editor.handle_input("\x17"); // deletes "second"
        editor.set_text("third");
        editor.handle_input("\x17"); // deletes "third"
        editor.set_text("");

        // Ring: [first, second, third]

        editor.handle_input("\x19"); // Ctrl+Y - yanks "third"
        editor.handle_input("\x1by"); // Alt+Y - cycles to "second", ring rotates

        // Now ring is: [third, first, second]
        assert_eq!(editor.get_text(), "second");

        // Do something else
        editor.handle_input("x");
        editor.set_text("");

        // New yank should get "second" (now at end after rotation)
        editor.handle_input("\x19"); // Ctrl+Y
        assert_eq!(editor.get_text(), "second");
    }

    #[test]
    fn consecutive_deletions_across_lines_coalesce_into_one_entry() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        // "1\n2\n3" with cursor at end, delete everything with Ctrl+W
        editor.set_text("1\n2\n3");
        editor.handle_input("\x17"); // Ctrl+W - deletes "3"
        assert_eq!(editor.get_text(), "1\n2\n");

        editor.handle_input("\x17"); // Ctrl+W - deletes newline (merge with prev line)
        assert_eq!(editor.get_text(), "1\n2");

        editor.handle_input("\x17"); // Ctrl+W - deletes "2"
        assert_eq!(editor.get_text(), "1\n");

        editor.handle_input("\x17"); // Ctrl+W - deletes newline
        assert_eq!(editor.get_text(), "1");

        editor.handle_input("\x17"); // Ctrl+W - deletes "1"
        assert_eq!(editor.get_text(), "");

        // All deletions should have accumulated into one entry
        editor.handle_input("\x19"); // Ctrl+Y
        assert_eq!(editor.get_text(), "1\n2\n3");
    }

    #[test]
    fn ctrl_k_at_line_end_deletes_newline_and_coalesces() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        // "ab" on line 1, "cd" on line 2, cursor at end of line 1
        editor.set_text("");
        editor.handle_input("a");
        editor.handle_input("b");
        editor.handle_input("\n");
        editor.handle_input("c");
        editor.handle_input("d");
        // Move to end of first line
        editor.handle_input("\x1b[A"); // Up arrow
        editor.handle_input("\x05"); // Ctrl+E - end of line

        // Now at end of "ab", Ctrl+K should delete newline (merge with "cd")
        editor.handle_input("\x0b"); // Ctrl+K - deletes newline
        assert_eq!(editor.get_text(), "abcd");

        // Continue deleting
        editor.handle_input("\x0b"); // Ctrl+K - deletes "cd"
        assert_eq!(editor.get_text(), "ab");

        // Both deletions should accumulate
        editor.handle_input("\x19"); // Ctrl+Y
        assert_eq!(editor.get_text(), "ab\ncd");
    }

    #[test]
    fn handles_yank_in_middle_of_text() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("word");
        editor.handle_input("\x17"); // Ctrl+W - deletes "word"
        editor.set_text("hello world");

        // Move to middle (after "hello ")
        editor.handle_input("\x01"); // Ctrl+A
        for _ in 0..6 {
            editor.handle_input("\x1b[C");
        }

        editor.handle_input("\x19"); // Ctrl+Y
        assert_eq!(editor.get_text(), "hello wordworld");
    }

    #[test]
    fn handles_yank_pop_in_middle_of_text() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        // Create two kill ring entries
        editor.set_text("FIRST");
        editor.handle_input("\x17"); // Ctrl+W - deletes "FIRST"
        editor.set_text("SECOND");
        editor.handle_input("\x17"); // Ctrl+W - deletes "SECOND"

        // Ring: ["FIRST", "SECOND"]

        // Set up "hello world" and position cursor after "hello "
        editor.set_text("hello world");
        editor.handle_input("\x01"); // Ctrl+A - go to start of line
        for _ in 0..6 {
            editor.handle_input("\x1b[C"); // Move right 6
        }

        // Yank "SECOND" in the middle
        editor.handle_input("\x19"); // Ctrl+Y
        assert_eq!(editor.get_text(), "hello SECONDworld");

        // Yank-pop replaces "SECOND" with "FIRST"
        editor.handle_input("\x1by"); // Alt+Y
        assert_eq!(editor.get_text(), "hello FIRSTworld");
    }

    #[test]
    fn multiline_yank_and_yank_pop_in_middle_of_text() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        // Create single-line entry
        editor.set_text("SINGLE");
        editor.handle_input("\x17"); // Ctrl+W - deletes "SINGLE"

        // Create multiline entry via consecutive Ctrl+U
        editor.set_text("A\nB");
        editor.handle_input("\x15"); // Ctrl+U - deletes "B"
        editor.handle_input("\x15"); // Ctrl+U - deletes newline
        editor.handle_input("\x15"); // Ctrl+U - deletes "A"
        // Ring: ["SINGLE", "A\nB"]

        // Insert in middle of "hello world"
        editor.set_text("hello world");
        editor.handle_input("\x01"); // Ctrl+A
        for _ in 0..6 {
            editor.handle_input("\x1b[C");
        }

        // Yank multiline "A\nB"
        editor.handle_input("\x19"); // Ctrl+Y
        assert_eq!(editor.get_text(), "hello A\nBworld");

        // Yank-pop replaces with "SINGLE"
        editor.handle_input("\x1by"); // Alt+Y
        assert_eq!(editor.get_text(), "hello SINGLEworld");
    }

    #[test]
    fn alt_d_deletes_word_forward_and_saves_to_kill_ring() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("hello world test");
        editor.handle_input("\x01"); // Ctrl+A - go to start

        editor.handle_input("\x1bd"); // Alt+D - deletes "hello"
        assert_eq!(editor.get_text(), " world test");

        editor.handle_input("\x1bd"); // Alt+D - deletes " world" (skips whitespace, then word)
        assert_eq!(editor.get_text(), " test");

        // Yank should get accumulated text
        editor.handle_input("\x19"); // Ctrl+Y
        assert_eq!(editor.get_text(), "hello world test");
    }

    #[test]
    fn alt_d_at_end_of_line_deletes_newline() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("line1\nline2");
        // Move to start of document, then to end of first line
        editor.handle_input("\x1b[A"); // Up arrow - go to first line
        editor.handle_input("\x05"); // Ctrl+E - end of line

        editor.handle_input("\x1bd"); // Alt+D - deletes newline (merges lines)
        assert_eq!(editor.get_text(), "line1line2");

        editor.handle_input("\x19"); // Ctrl+Y
        assert_eq!(editor.get_text(), "line1\nline2");
    }
}

struct TestAutocompleteProvider {
    triggers: Mutex<Option<Vec<String>>>,
    get_suggestions_fn: Arc<
        dyn Fn(&[String], i64, i64, GetSuggestionsOptions) -> pi_js::BoxFuture<Option<AutocompleteSuggestions>>
            + Send
            + Sync,
    >,
    apply_completion_fn:
        Option<Arc<dyn Fn(&[String], i64, i64, &AutocompleteItem, &str) -> ApplyCompletionResult + Send + Sync>>,
}

impl TestAutocompleteProvider {
    fn new(
        triggers: Option<Vec<String>>,
        get_suggestions_fn: impl Fn(
            &[String],
            i64,
            i64,
            GetSuggestionsOptions,
        ) -> pi_js::BoxFuture<Option<AutocompleteSuggestions>>
        + Send
        + Sync
        + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            triggers: Mutex::new(triggers),
            get_suggestions_fn: Arc::new(get_suggestions_fn),
            apply_completion_fn: None,
        })
    }

    fn with_apply(
        triggers: Option<Vec<String>>,
        get_suggestions_fn: impl Fn(
            &[String],
            i64,
            i64,
            GetSuggestionsOptions,
        ) -> pi_js::BoxFuture<Option<AutocompleteSuggestions>>
        + Send
        + Sync
        + 'static,
        apply_completion_fn: impl Fn(&[String], i64, i64, &AutocompleteItem, &str) -> ApplyCompletionResult
        + Send
        + Sync
        + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            triggers: Mutex::new(triggers),
            get_suggestions_fn: Arc::new(get_suggestions_fn),
            apply_completion_fn: Some(Arc::new(apply_completion_fn)),
        })
    }
}

#[async_trait]
impl AutocompleteProvider for TestAutocompleteProvider {
    fn trigger_characters(&self) -> Option<Vec<String>> {
        self.triggers.lock().unwrap().clone()
    }

    fn set_trigger_characters(&self, trigger_characters: Option<Vec<String>>) {
        *self.triggers.lock().unwrap() = trigger_characters;
    }

    async fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: i64,
        cursor_col: i64,
        options: GetSuggestionsOptions,
    ) -> Option<AutocompleteSuggestions> {
        (self.get_suggestions_fn)(lines, cursor_line, cursor_col, options).await
    }

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: i64,
        cursor_col: i64,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> ApplyCompletionResult {
        if let Some(apply_fn) = &self.apply_completion_fn {
            apply_fn(lines, cursor_line, cursor_col, item, prefix)
        } else {
            standard_apply_completion(lines, cursor_line, cursor_col, item, prefix)
        }
    }
}

mod undo {
    use super::*;

    #[test]
    fn does_nothing_when_undo_stack_is_empty() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "");
    }

    #[test]
    fn coalesces_consecutive_word_characters_into_one_undo_unit() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        for ch in ["h", "e", "l", "l", "o", " ", "w", "o", "r", "l", "d"] {
            editor.handle_input(ch);
        }
        assert_eq!(editor.get_text(), "hello world");

        // Undo removes " world" (space captured state before it, so we restore to "hello")
        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "hello");

        // Undo removes "hello"
        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "");
    }

    #[test]
    fn undoes_spaces_one_at_a_time() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        for ch in ["h", "e", "l", "l", "o", " ", " "] {
            editor.handle_input(ch);
        }
        assert_eq!(editor.get_text(), "hello  ");

        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo) - removes second " "
        assert_eq!(editor.get_text(), "hello ");

        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo) - removes first " "
        assert_eq!(editor.get_text(), "hello");

        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo) - removes "hello"
        assert_eq!(editor.get_text(), "");
    }

    #[test]
    fn undoes_newlines_and_signals_next_word_to_capture_state() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        for ch in ["h", "e", "l", "l", "o", "\n", "w", "o", "r", "l", "d"] {
            editor.handle_input(ch);
        }
        assert_eq!(editor.get_text(), "hello\nworld");

        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "hello\n");

        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "hello");

        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "");
    }

    #[test]
    fn undoes_backspace() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        for ch in ["h", "e", "l", "l", "o"] {
            editor.handle_input(ch);
        }
        editor.handle_input("\x7f"); // Backspace
        assert_eq!(editor.get_text(), "hell");

        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "hello");
    }

    #[test]
    fn undoes_forward_delete() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        for ch in ["h", "e", "l", "l", "o"] {
            editor.handle_input(ch);
        }
        editor.handle_input("\x01"); // Ctrl+A - go to start
        editor.handle_input("\x1b[C"); // Right arrow
        editor.handle_input("\x1b[3~"); // Delete key
        assert_eq!(editor.get_text(), "hllo");

        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "hello");
    }

    #[test]
    fn undoes_ctrl_w_delete_word_backward() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        for ch in ["h", "e", "l", "l", "o", " ", "w", "o", "r", "l", "d"] {
            editor.handle_input(ch);
        }
        assert_eq!(editor.get_text(), "hello world");

        editor.handle_input("\x17"); // Ctrl+W
        assert_eq!(editor.get_text(), "hello ");

        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "hello world");
    }

    #[test]
    fn undoes_ctrl_k_delete_to_line_end() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        for ch in ["h", "e", "l", "l", "o", " ", "w", "o", "r", "l", "d"] {
            editor.handle_input(ch);
        }
        editor.handle_input("\x01"); // Ctrl+A - go to start
        for _ in 0..6 {
            editor.handle_input("\x1b[C"); // Move right 6 times
        }

        editor.handle_input("\x0b"); // Ctrl+K
        assert_eq!(editor.get_text(), "hello ");

        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "hello world");

        editor.handle_input("|");
        assert_eq!(editor.get_text(), "hello |world");
    }

    #[test]
    fn undoes_ctrl_u_delete_to_line_start() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        for ch in ["h", "e", "l", "l", "o", " ", "w", "o", "r", "l", "d"] {
            editor.handle_input(ch);
        }
        editor.handle_input("\x01"); // Ctrl+A - go to start
        for _ in 0..6 {
            editor.handle_input("\x1b[C"); // Move right 6 times
        }

        editor.handle_input("\x15"); // Ctrl+U
        assert_eq!(editor.get_text(), "world");

        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "hello world");
    }

    #[test]
    fn undoes_yank() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        for ch in ["h", "e", "l", "l", "o", " "] {
            editor.handle_input(ch);
        }
        editor.handle_input("\x17"); // Ctrl+W - delete "hello "
        editor.handle_input("\x19"); // Ctrl+Y - yank
        assert_eq!(editor.get_text(), "hello ");

        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "");
    }

    #[test]
    fn undoes_single_line_paste_atomically() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("hello world");
        editor.handle_input("\x01"); // Ctrl+A - go to start
        for _ in 0..5 {
            editor.handle_input("\x1b[C"); // Move right 5 (after "hello", before space)
        }

        // Simulate bracketed paste of "beep boop"
        editor.handle_input("\x1b[200~beep boop\x1b[201~");
        assert_eq!(editor.get_text(), "hellobeep boop world");

        // Single undo should restore entire pre-paste state
        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "hello world");

        editor.handle_input("|");
        assert_eq!(editor.get_text(), "hello| world");
    }

    #[test]
    fn does_not_trigger_autocomplete_during_single_line_paste() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let suggestion_calls = Arc::new(Mutex::new(0i64));
        let calls_ref = Arc::clone(&suggestion_calls);

        let mock_provider = TestAutocompleteProvider::new(None, move |_, _, _, _| {
            let calls_ref = Arc::clone(&calls_ref);
            Box::pin(async move {
                *calls_ref.lock().unwrap() += 1;
                None
            })
        });

        editor.set_autocomplete_provider(mock_provider);
        editor.handle_input("\x1b[200~look at @node_modules/react/index.js please\x1b[201~");

        assert_eq!(editor.get_text(), "look at @node_modules/react/index.js please");
        assert_eq!(*suggestion_calls.lock().unwrap(), 0);
        assert!(!editor.is_showing_autocomplete());
    }

    #[test]
    fn decodes_csi_u_ctrl_letter_sequences_inside_bracketed_paste_tmux_popup() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        // tmux popups with extended-keys-format=csi-u re-encode \n in pastes as
        // \x1b[106;5u (Ctrl+J). Without decoding, the per-char filter strips ESC
        // and leaks "[106;5u" between lines. See issue #3599.
        editor.handle_input("\x1b[200~line1\x1b[106;5uline2\x1b[106;5uline3\x1b[201~");
        assert_eq!(editor.get_text(), "line1\nline2\nline3");
    }

    #[test]
    fn undoes_multi_line_paste_atomically() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("hello world");
        editor.handle_input("\x01"); // Ctrl+A - go to start
        for _ in 0..5 {
            editor.handle_input("\x1b[C"); // Move right 5 (after "hello", before space)
        }

        // Simulate bracketed paste of multi-line text
        editor.handle_input("\x1b[200~line1\nline2\nline3\x1b[201~");
        assert_eq!(editor.get_text(), "helloline1\nline2\nline3 world");

        // Single undo should restore entire pre-paste state
        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "hello world");

        editor.handle_input("|");
        assert_eq!(editor.get_text(), "hello| world");
    }

    #[test]
    fn undoes_insert_text_at_cursor_atomically() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("hello world");
        editor.handle_input("\x01"); // Ctrl+A - go to start
        for _ in 0..5 {
            editor.handle_input("\x1b[C"); // Move right 5 (after "hello", before space)
        }

        // Programmatic insertion (e.g., clipboard image path)
        editor.insert_text_at_cursor("/tmp/image.png");
        assert_eq!(editor.get_text(), "hello/tmp/image.png world");

        // Single undo should restore entire pre-insert state
        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "hello world");

        editor.handle_input("|");
        assert_eq!(editor.get_text(), "hello| world");
    }

    #[test]
    fn insert_text_at_cursor_handles_multiline_text() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("hello world");
        editor.handle_input("\x01"); // Ctrl+A - go to start
        for _ in 0..5 {
            editor.handle_input("\x1b[C"); // Move right 5 (after "hello", before space)
        }

        // Insert multiline text
        editor.insert_text_at_cursor("line1\nline2\nline3");
        assert_eq!(editor.get_text(), "helloline1\nline2\nline3 world");

        // Cursor should be at end of inserted text (after "line3", before " world")
        let cursor = editor.get_cursor();
        assert_eq!(cursor.line, 2);
        assert_eq!(cursor.col, 5); // "line3".length

        // Single undo should restore entire pre-insert state
        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "hello world");
    }

    #[test]
    fn insert_text_at_cursor_normalizes_crlf_and_cr_line_endings() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("");

        // Insert text with CRLF
        editor.insert_text_at_cursor("a\r\nb\r\nc");
        assert_eq!(editor.get_text(), "a\nb\nc");

        editor.handle_input("\x1b[45;5u"); // Undo
        assert_eq!(editor.get_text(), "");

        // Insert text with CR only
        editor.insert_text_at_cursor("x\ry\rz");
        assert_eq!(editor.get_text(), "x\ny\nz");
    }

    #[test]
    fn undoes_set_text_to_empty_string() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        for ch in ["h", "e", "l", "l", "o", " ", "w", "o", "r", "l", "d"] {
            editor.handle_input(ch);
        }
        assert_eq!(editor.get_text(), "hello world");

        editor.set_text("");
        assert_eq!(editor.get_text(), "");

        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "hello world");
    }

    #[test]
    fn clears_undo_stack_on_submit() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let submitted = Arc::new(Mutex::new(String::new()));
        let submitted_ref = Arc::clone(&submitted);
        editor.set_on_submit(Some(Arc::new(move |text| {
            *submitted_ref.lock().unwrap() = text.to_string();
        })));

        for ch in ["h", "e", "l", "l", "o"] {
            editor.handle_input(ch);
        }
        editor.handle_input("\r"); // Enter - submit

        assert_eq!(*submitted.lock().unwrap(), "hello");
        assert_eq!(editor.get_text(), "");

        // Undo should do nothing - stack was cleared
        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "");
    }

    #[test]
    fn exits_history_browsing_mode_on_undo() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        // Add "hello" to history
        editor.add_to_history("hello");
        assert_eq!(editor.get_text(), "");

        // Type "world"
        for ch in ["w", "o", "r", "l", "d"] {
            editor.handle_input(ch);
        }
        assert_eq!(editor.get_text(), "world");

        // Ctrl+W - delete word
        editor.handle_input("\x17"); // Ctrl+W
        assert_eq!(editor.get_text(), "");

        // Press Up - enter history browsing, shows "hello"
        editor.handle_input("\x1b[A"); // Up arrow
        assert_eq!(editor.get_text(), "hello");

        // Undo should restore to "" (state before entering history browsing)
        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "");

        // Undo again should restore to "world" (state before Ctrl+W)
        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "world");
    }

    #[test]
    fn undo_restores_to_pre_history_state_even_after_multiple_history_navigations() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        // Add history entries
        editor.add_to_history("first");
        editor.add_to_history("second");
        editor.add_to_history("third");

        // Type something
        for ch in ["c", "u", "r", "r", "e", "n", "t"] {
            editor.handle_input(ch);
        }
        assert_eq!(editor.get_text(), "current");

        // Clear editor
        editor.handle_input("\x17"); // Ctrl+W
        assert_eq!(editor.get_text(), "");

        // Navigate through history multiple times
        editor.handle_input("\x1b[A"); // Up - "third"
        assert_eq!(editor.get_text(), "third");
        editor.handle_input("\x1b[A"); // Up - "second"
        assert_eq!(editor.get_text(), "second");
        editor.handle_input("\x1b[A"); // Up - "first"
        assert_eq!(editor.get_text(), "first");

        // Undo should go back to "" (state before we started browsing), not intermediate states
        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "");

        // Another undo goes back to "current"
        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "current");
    }

    #[test]
    fn cursor_movement_starts_new_undo_unit() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        for ch in ["h", "e", "l", "l", "o", " ", "w", "o", "r", "l", "d"] {
            editor.handle_input(ch);
        }
        assert_eq!(editor.get_text(), "hello world");

        // Move cursor left 5 (to after "hello ")
        for _ in 0..5 {
            editor.handle_input("\x1b[D");
        }

        // Type "lol" in the middle
        editor.handle_input("l");
        editor.handle_input("o");
        editor.handle_input("l");
        assert_eq!(editor.get_text(), "hello lolworld");

        // Undo should restore to "hello world" (before inserting "lol")
        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "hello world");

        editor.handle_input("|");
        assert_eq!(editor.get_text(), "hello |world");
    }

    #[test]
    fn no_op_delete_operations_do_not_push_undo_snapshots() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        for ch in ["h", "e", "l", "l", "o"] {
            editor.handle_input(ch);
        }
        assert_eq!(editor.get_text(), "hello");

        // Delete word on empty - multiple times (should be no-ops)
        editor.handle_input("\x17"); // Ctrl+W - deletes "hello"
        assert_eq!(editor.get_text(), "");
        editor.handle_input("\x17"); // Ctrl+W - no-op (nothing to delete)
        editor.handle_input("\x17"); // Ctrl+W - no-op

        // Single undo should restore "hello"
        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "hello");
    }

    #[tokio::test]
    async fn undoes_autocomplete() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        // Create a mock autocomplete provider
        let mock_provider = TestAutocompleteProvider::new(None, |lines, _cursor_line, cursor_col, _| {
            let text = lines.first().map(String::as_str).unwrap_or("");
            let prefix = str16::slice(text, 0, Some(cursor_col.max(0)));
            Box::pin(async move {
                if prefix == "di" {
                    Some(AutocompleteSuggestions {
                        items: vec![AutocompleteItem {
                            value: "dist/".to_string(),
                            label: "dist/".to_string(),
                            description: None,
                        }],
                        prefix: "di".to_string(),
                    })
                } else {
                    None
                }
            })
        });

        editor.set_autocomplete_provider(mock_provider);

        // Type "di"
        editor.handle_input("d");
        editor.handle_input("i");
        assert_eq!(editor.get_text(), "di");

        // Press Tab to trigger autocomplete
        editor.handle_input("\t");
        flush_autocomplete().await;
        assert_eq!(editor.get_text(), "dist/");
        assert!(!editor.is_showing_autocomplete());

        // Undo should restore to "di"
        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "di");
    }
}

mod autocomplete {
    use super::*;

    #[tokio::test]
    async fn moves_the_autocomplete_selection_with_selection_page_keys_7629() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let submitted = Arc::new(Mutex::new(String::new()));
        let submitted_ref = Arc::clone(&submitted);
        editor.set_on_submit(Some(Arc::new(move |text| {
            *submitted_ref.lock().unwrap() = text.to_string();
        })));
        let mock_provider = TestAutocompleteProvider::new(None, |_, _, _, _| {
            Box::pin(async move {
                let items = (0..12)
                    .map(|index| AutocompleteItem {
                        value: format!("/command-{index}"),
                        label: format!("command-{index}"),
                        description: None,
                    })
                    .collect();
                Some(AutocompleteSuggestions {
                    items,
                    prefix: "/".to_string(),
                })
            })
        });
        editor.set_autocomplete_provider(mock_provider);

        assert!(!editor.captures_viewport_input());
        editor.handle_input("/");
        flush_autocomplete().await;
        assert!(editor.captures_viewport_input());

        editor.handle_input("\x1b[6~");
        editor.handle_input("\x1b[6~");
        editor.handle_input("\x1b[5~");
        editor.handle_input("\r");

        assert_eq!(*submitted.lock().unwrap(), "/command-5");
        assert!(!editor.captures_viewport_input());
    }

    #[tokio::test(start_paused = true)]
    async fn triggers_and_debounces_symbol_completion_after_cjk_punctuation() {
        let mut befores: Vec<String> = vec!["查看，".to_string(), "\u{3000}".to_string()];
        for ch in "，．：；！？（）［］｛｝\u{201c}\u{201d}\u{2018}\u{2019}…—。、「」『』《》【】".chars()
        {
            befores.push(ch.to_string());
        }
        befores.extend(["(", "see (", "`", "["].into_iter().map(str::to_string));

        for before in befores {
            for trigger in ["@", "#", "$", "-"] {
                let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
                let requests = Arc::new(Mutex::new(Vec::<String>::new()));
                let requests_ref = Arc::clone(&requests);
                let mock_provider = TestAutocompleteProvider::new(
                    Some(vec!["$".to_string(), "-".to_string()]),
                    move |lines, cursor_line, cursor_col, _| {
                        let req = lines
                            .get(usize::try_from(cursor_line).unwrap_or(0))
                            .map(|l| str16::slice(l, 0, Some(cursor_col.max(0))))
                            .unwrap_or_default();
                        let requests_ref = Arc::clone(&requests_ref);
                        Box::pin(async move {
                            requests_ref.lock().unwrap().push(req);
                            None
                        })
                    },
                );
                editor.set_autocomplete_provider(mock_provider);
                editor.set_text(&before);
                editor.handle_input(trigger);
                tokio::time::advance(Duration::from_millis(19)).await;
                flush_autocomplete().await;
                assert!(requests.lock().unwrap().is_empty());
                tokio::time::advance(Duration::from_millis(1)).await;
                flush_autocomplete().await;
                assert_eq!(*requests.lock().unwrap(), vec![format!("{before}{trigger}")]);

                editor.handle_input("r");
                editor.handle_input("e");
                tokio::time::advance(Duration::from_millis(19)).await;
                flush_autocomplete().await;
                assert_eq!(requests.lock().unwrap().len(), 1);
                tokio::time::advance(Duration::from_millis(1)).await;
                flush_autocomplete().await;
                assert_eq!(
                    *requests.lock().unwrap(),
                    vec![format!("{before}{trigger}"), format!("{before}{trigger}re")]
                );
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn does_not_auto_trigger_after_cjk_letters_or_for_unprefixed_paths() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let requests = Arc::new(Mutex::new(0i64));
        let requests_ref = Arc::clone(&requests);
        let mock_provider = TestAutocompleteProvider::new(None, move |_, _, _, _| {
            let requests_ref = Arc::clone(&requests_ref);
            Box::pin(async move {
                *requests_ref.lock().unwrap() += 1;
                None
            })
        });
        editor.set_autocomplete_provider(mock_provider);
        for text in [
            "user@example.com",
            "张三@example.com",
            "查看@src",
            "あ@src",
            "カ@src",
            "한@src",
            "ㄅ@src",
            "𠮷@src",
            "か\u{3099}@src",
            "禰\u{e0100}@src",
            "々@src",
            "Ａ@src",
            "文档@备份",
            "prefix#123",
            "foo(@src",
            "问题#123",
            "查看，/path/",
            "查看，./文档/",
            "src/index.ts",
            "./文档/说明.md",
            "文档/说明.md",
            "查看src/index.ts",
        ] {
            editor.set_text("");
            for ch in text.chars() {
                let mut buf = [0u8; 4];
                editor.handle_input(ch.encode_utf8(&mut buf));
            }
            tokio::time::advance(Duration::from_millis(20)).await;
            flush_autocomplete().await;
            assert_eq!(*requests.lock().unwrap(), 0, "{text}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn requests_path_completion_after_cjk_punctuation_only_on_tab() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let requests = Arc::new(Mutex::new(Vec::<(String, Option<bool>)>::new()));
        let requests_ref = Arc::clone(&requests);
        let mock_provider = TestAutocompleteProvider::new(None, move |lines, cursor_line, cursor_col, options| {
            let text = lines
                .get(usize::try_from(cursor_line).unwrap_or(0))
                .map(|l| str16::slice(l, 0, Some(cursor_col.max(0))))
                .unwrap_or_default();
            let requests_ref = Arc::clone(&requests_ref);
            Box::pin(async move {
                requests_ref.lock().unwrap().push((text, options.force));
                None
            })
        });
        editor.set_autocomplete_provider(mock_provider);
        let text = "查看，/path/";
        for ch in text.chars() {
            let mut buf = [0u8; 4];
            editor.handle_input(ch.encode_utf8(&mut buf));
        }
        tokio::time::advance(Duration::from_millis(20)).await;
        flush_autocomplete().await;
        assert!(requests.lock().unwrap().is_empty());
        editor.handle_input("\t");
        flush_autocomplete().await;
        assert_eq!(*requests.lock().unwrap(), vec![(text.to_string(), Some(true))]);
    }

    #[tokio::test]
    async fn completes_chinese_path_prefixes_after_whitespace_or_cjk_punctuation_with_tab() {
        let base_dir = TempDir::new().unwrap();
        let doc_dir = base_dir.path().join("文档");
        std::fs::create_dir_all(&doc_dir).unwrap();
        std::fs::write(doc_dir.join("说明.md"), "text").unwrap();
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        editor.set_autocomplete_provider(Arc::new(CombinedAutocompleteProvider::new(
            Some(vec![]),
            base_dir.path().to_str().unwrap(),
            None,
        )));
        for separator in [" ", "\t", "\u{3000}", "\u{00a0}", "，", "。"] {
            editor.set_text(&format!("查看{separator}"));
            let before = editor.get_text();
            editor.handle_input("文");
            editor.handle_input("\t");
            flush_autocomplete().await;
            assert_eq!(editor.get_text(), format!("{before}文档/"));
            editor.handle_input("说");
            editor.handle_input("\t");
            flush_autocomplete().await;
            assert_eq!(editor.get_text(), format!("{before}文档/说明.md"));
            let expected_col = i64::try_from(str16::len(&editor.get_text())).unwrap();
            assert_eq!(
                editor.get_cursor(),
                EditorCursor {
                    line: 0,
                    col: expected_col
                }
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn ends_unquoted_trigger_and_debounce_contexts_at_whitespace_or_cjk_punctuation() {
        for separator in [" ", "\u{3000}", "，", "。"] {
            for trigger in ["@", "#", "$"] {
                let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
                let requests = Arc::new(Mutex::new(Vec::<String>::new()));
                let requests_ref = Arc::clone(&requests);
                let prefix = format!("{trigger}src");
                let prefix_ref = prefix.clone();
                let mock_provider = TestAutocompleteProvider::new(
                    Some(vec!["$".to_string()]),
                    move |lines, cursor_line, cursor_col, _| {
                        let text = lines
                            .get(usize::try_from(cursor_line).unwrap_or(0))
                            .map(|l| str16::slice(l, 0, Some(cursor_col.max(0))))
                            .unwrap_or_default();
                        let requests_ref = Arc::clone(&requests_ref);
                        let prefix_ref = prefix_ref.clone();
                        Box::pin(async move {
                            requests_ref.lock().unwrap().push(text.clone());
                            if text == prefix_ref {
                                Some(AutocompleteSuggestions {
                                    prefix: prefix_ref.clone(),
                                    items: vec![AutocompleteItem {
                                        value: format!("{prefix_ref}/"),
                                        label: "src/".to_string(),
                                        description: None,
                                    }],
                                })
                            } else {
                                None
                            }
                        })
                    },
                );
                editor.set_autocomplete_provider(mock_provider);
                editor.set_text(&format!("{trigger}sr"));
                editor.handle_input("c");
                tokio::time::advance(Duration::from_millis(20)).await;
                flush_autocomplete().await;
                assert!(editor.is_showing_autocomplete());
                editor.handle_input(separator);
                flush_autocomplete().await;
                assert_eq!(
                    *requests.lock().unwrap(),
                    vec![prefix.clone(), format!("{prefix}{separator}")]
                );
                assert!(!editor.is_showing_autocomplete());
                editor.handle_input("文");
                tokio::time::advance(Duration::from_millis(20)).await;
                flush_autocomplete().await;
                assert_eq!(
                    *requests.lock().unwrap(),
                    vec![prefix.clone(), format!("{prefix}{separator}")]
                );
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn re_triggers_cjk_path_completion_after_accepting_directories_and_deleting() {
        let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
        let provider = Arc::new(CombinedAutocompleteProvider::new(Some(vec![]), &cwd, None));
        for directory in ["文档", "我的 文档", "资料，归档"] {
            let quoted = directory != "文档";
            let initial = if quoted {
                format!("@\"{}", str16::slice(directory, 0, Some(2)))
            } else {
                "@文".to_string()
            };
            let directory_value = if quoted {
                format!("@\"{directory}/\"")
            } else {
                format!("@{directory}/")
            };
            let file_value = if quoted {
                format!("@\"{directory}/说明.md\"")
            } else {
                format!("@{directory}/说明.md")
            };
            let file_prefix = if quoted {
                format!("@\"{directory}/说")
            } else {
                format!("@{directory}/说")
            };
            let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
            let initial_ref = initial.clone();
            let directory_value_ref = directory_value.clone();
            let file_prefix_ref = file_prefix.clone();
            let file_value_ref = file_value.clone();
            let directory_owned = directory.to_string();
            let provider_for_apply = Arc::clone(&provider);
            let mock_provider = TestAutocompleteProvider::with_apply(
                None,
                move |lines, cursor_line, cursor_col, _| {
                    let before = lines
                        .get(usize::try_from(cursor_line).unwrap_or(0))
                        .map(|l| str16::slice(l, 0, Some(cursor_col.max(0))))
                        .unwrap_or_default();
                    let at_idx = str16::index_of(&before, "@", 0)
                        .and_then(|idx| i64::try_from(idx).ok())
                        .unwrap_or(0);
                    let prefix = str16::slice(&before, at_idx, None);
                    let initial_ref = initial_ref.clone();
                    let directory_value_ref = directory_value_ref.clone();
                    let file_prefix_ref = file_prefix_ref.clone();
                    let file_value_ref = file_value_ref.clone();
                    let directory_owned = directory_owned.clone();
                    Box::pin(async move {
                        if prefix == initial_ref {
                            Some(AutocompleteSuggestions {
                                prefix,
                                items: vec![AutocompleteItem {
                                    value: directory_value_ref,
                                    label: format!("{directory_owned}/"),
                                    description: None,
                                }],
                            })
                        } else if prefix == file_prefix_ref {
                            Some(AutocompleteSuggestions {
                                prefix,
                                items: vec![AutocompleteItem {
                                    value: file_value_ref,
                                    label: "说明.md".to_string(),
                                    description: None,
                                }],
                            })
                        } else {
                            None
                        }
                    })
                },
                move |lines, cursor_line, cursor_col, item, prefix| {
                    provider_for_apply.apply_completion(lines, cursor_line, cursor_col, item, prefix)
                },
            );
            editor.set_autocomplete_provider(mock_provider);
            editor.set_text(&format!("查看：{initial}"));
            editor.handle_input("\t");
            flush_autocomplete().await;
            assert_eq!(editor.get_text(), format!("查看：{directory_value}"));
            assert!(!editor.is_showing_autocomplete());

            editor.handle_input("说");
            tokio::time::advance(Duration::from_millis(20)).await;
            flush_autocomplete().await;
            assert!(editor.is_showing_autocomplete());

            for deletion in ["\x7f", "\x1b[3~"] {
                editor.handle_input("错");
                tokio::time::advance(Duration::from_millis(20)).await;
                flush_autocomplete().await;
                assert!(!editor.is_showing_autocomplete());
                if deletion == "\x1b[3~" {
                    editor.handle_input("\x1b[D");
                }
                editor.handle_input(deletion);
                tokio::time::advance(Duration::from_millis(20)).await;
                flush_autocomplete().await;
                assert!(editor.is_showing_autocomplete());
            }

            editor.handle_input("\t");
            assert_eq!(editor.get_text(), format!("查看：{file_value} "));
            let expected_col = i64::try_from(str16::len(&editor.get_text())).unwrap();
            assert_eq!(
                editor.get_cursor(),
                EditorCursor {
                    line: 0,
                    col: expected_col
                }
            );
        }
    }

    #[tokio::test]
    async fn auto_applies_single_force_file_suggestion_without_showing_menu() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        let mock_provider = TestAutocompleteProvider::new(None, |lines, _cursor_line, cursor_col, options| {
            let text = lines.first().map(String::as_str).unwrap_or("");
            let prefix = str16::slice(text, 0, Some(cursor_col.max(0)));
            Box::pin(async move {
                if !options.force.unwrap_or(false) {
                    return None;
                }
                if prefix == "Work" {
                    Some(AutocompleteSuggestions {
                        items: vec![AutocompleteItem {
                            value: "Workspace/".to_string(),
                            label: "Workspace/".to_string(),
                            description: None,
                        }],
                        prefix: "Work".to_string(),
                    })
                } else {
                    None
                }
            })
        });

        editor.set_autocomplete_provider(mock_provider);

        // Type "Work"
        for ch in ["W", "o", "r", "k"] {
            editor.handle_input(ch);
        }
        assert_eq!(editor.get_text(), "Work");

        // Press Tab - should auto-apply without showing menu
        editor.handle_input("\t");
        flush_autocomplete().await;
        assert_eq!(editor.get_text(), "Workspace/");
        assert!(!editor.is_showing_autocomplete());

        // Undo should restore to "Work"
        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "Work");
    }

    #[tokio::test]
    async fn shows_menu_when_force_file_has_multiple_suggestions() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        let mock_provider = TestAutocompleteProvider::new(None, |lines, _cursor_line, cursor_col, options| {
            let text = lines.first().map(String::as_str).unwrap_or("");
            let prefix = str16::slice(text, 0, Some(cursor_col.max(0)));
            Box::pin(async move {
                if !options.force.unwrap_or(false) {
                    return None;
                }
                if prefix == "src" {
                    Some(AutocompleteSuggestions {
                        items: vec![
                            AutocompleteItem {
                                value: "src/".to_string(),
                                label: "src/".to_string(),
                                description: None,
                            },
                            AutocompleteItem {
                                value: "src.txt".to_string(),
                                label: "src.txt".to_string(),
                                description: None,
                            },
                        ],
                        prefix: "src".to_string(),
                    })
                } else {
                    None
                }
            })
        });

        editor.set_autocomplete_provider(mock_provider);

        // Type "src"
        for ch in ["s", "r", "c"] {
            editor.handle_input(ch);
        }
        assert_eq!(editor.get_text(), "src");

        // Press Tab - should show menu because there are multiple suggestions
        editor.handle_input("\t");
        flush_autocomplete().await;
        assert_eq!(editor.get_text(), "src");
        assert!(editor.is_showing_autocomplete());

        // Press Tab again to accept first suggestion
        editor.handle_input("\t");
        assert_eq!(editor.get_text(), "src/");
        assert!(!editor.is_showing_autocomplete());
    }

    #[tokio::test]
    async fn keeps_suggestions_open_when_typing_in_force_mode_tab_triggered() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        let all_files = Arc::new(vec![
            AutocompleteItem {
                value: "readme.md".to_string(),
                label: "readme.md".to_string(),
                description: None,
            },
            AutocompleteItem {
                value: "package.json".to_string(),
                label: "package.json".to_string(),
                description: None,
            },
            AutocompleteItem {
                value: "src/".to_string(),
                label: "src/".to_string(),
                description: None,
            },
            AutocompleteItem {
                value: "dist/".to_string(),
                label: "dist/".to_string(),
                description: None,
            },
        ]);
        let all_files_ref = Arc::clone(&all_files);

        let mock_provider = TestAutocompleteProvider::new(None, move |lines, _cursor_line, cursor_col, options| {
            let text = lines.first().map(String::as_str).unwrap_or("");
            let prefix = str16::slice(text, 0, Some(cursor_col.max(0)));
            let all_files_ref = Arc::clone(&all_files_ref);
            Box::pin(async move {
                let should_match = options.force.unwrap_or(false) || prefix.contains('/') || prefix.starts_with('.');
                if !should_match {
                    return None;
                }
                let lower_prefix = prefix.to_lowercase();
                let filtered: Vec<AutocompleteItem> = all_files_ref
                    .iter()
                    .filter(|f| f.value.to_lowercase().starts_with(&lower_prefix))
                    .cloned()
                    .collect();
                if !filtered.is_empty() {
                    Some(AutocompleteSuggestions {
                        items: filtered,
                        prefix,
                    })
                } else {
                    None
                }
            })
        });

        editor.set_autocomplete_provider(mock_provider);

        // Press Tab on empty prompt - should show all files (force mode)
        editor.handle_input("\t");
        flush_autocomplete().await;
        assert!(editor.is_showing_autocomplete());

        // Type "r" - should narrow to "readme.md" (force mode keeps suggestions open)
        editor.handle_input("r");
        flush_autocomplete().await;
        assert_eq!(editor.get_text(), "r");
        assert!(editor.is_showing_autocomplete());

        // Type "e" - should still show "readme.md"
        editor.handle_input("e");
        flush_autocomplete().await;
        assert_eq!(editor.get_text(), "re");
        assert!(editor.is_showing_autocomplete());

        // Accept with Tab
        editor.handle_input("\t");
        assert_eq!(editor.get_text(), "readme.md");
        assert!(!editor.is_showing_autocomplete());
    }

    #[tokio::test(start_paused = true)]
    async fn debounces_at_autocomplete_while_typing() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let suggestion_calls = Arc::new(Mutex::new(0i64));
        let calls_ref = Arc::clone(&suggestion_calls);

        let mock_provider = TestAutocompleteProvider::new(None, move |lines, _cursor_line, cursor_col, _| {
            let text = str16::slice(
                lines.first().map(String::as_str).unwrap_or(""),
                0,
                Some(cursor_col.max(0)),
            );
            let calls_ref = Arc::clone(&calls_ref);
            Box::pin(async move {
                *calls_ref.lock().unwrap() += 1;
                Some(AutocompleteSuggestions {
                    items: vec![AutocompleteItem {
                        value: "@main.ts".to_string(),
                        label: "main.ts".to_string(),
                        description: None,
                    }],
                    prefix: text,
                })
            })
        });

        editor.set_autocomplete_provider(mock_provider);

        for ch in ["@", "m", "a", "i"] {
            editor.handle_input(ch);
        }

        assert_eq!(*suggestion_calls.lock().unwrap(), 0);
        assert!(!editor.is_showing_autocomplete());

        tokio::time::advance(Duration::from_millis(50)).await;
        flush_autocomplete().await;

        assert_eq!(*suggestion_calls.lock().unwrap(), 1);
        assert!(editor.is_showing_autocomplete());
    }

    #[tokio::test]
    async fn re_queries_the_autocomplete_picker_when_the_cursor_moves_back_into_the_command_name() {
        // Regression for earendil-works/pi#5496: arrowing left out of a slash
        // command's argument region must re-query the picker, not leave the
        // stale argument list showing. Before the fix, moveCursor() never
        // called updateAutocomplete(), so `/cmd ` (argument menu) + Left kept
        // displaying the arguments against a `/cmd` prefix — and a Tab there
        // would concatenate the stale suggestion onto the partial command name.
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        let mock_provider = TestAutocompleteProvider::new(None, |lines, _cursor_line, cursor_col, _| {
            let before = str16::slice(
                lines.first().map(String::as_str).unwrap_or(""),
                0,
                Some(cursor_col.max(0)),
            );
            Box::pin(async move {
                if !before.starts_with('/') {
                    return None;
                }
                // Past the command name (a space before the cursor): offer arguments.
                if let Some(space_idx) = str16::index_of(&before, " ", 0) {
                    let arg_prefix = str16::slice(&before, i64::try_from(space_idx + 1).unwrap(), None);
                    return Some(AutocompleteSuggestions {
                        items: vec![
                            AutocompleteItem {
                                value: "repo".to_string(),
                                label: "repo".to_string(),
                                description: None,
                            },
                            AutocompleteItem {
                                value: "message".to_string(),
                                label: "message".to_string(),
                                description: None,
                            },
                            AutocompleteItem {
                                value: "help".to_string(),
                                label: "help".to_string(),
                                description: None,
                            },
                        ],
                        prefix: arg_prefix,
                    });
                }
                // Inside the command name: offer the command name only.
                Some(AutocompleteSuggestions {
                    items: vec![AutocompleteItem {
                        value: "cmd".to_string(),
                        label: "cmd".to_string(),
                        description: None,
                    }],
                    prefix: before,
                })
            })
        });

        editor.set_autocomplete_provider(mock_provider);

        // Type `/cmd ` so the picker ends up showing the argument list.
        for ch in ["/", "c", "m", "d", " "] {
            editor.handle_input(ch);
            flush_autocomplete().await;
        }
        assert_eq!(editor.get_text(), "/cmd ");
        assert!(editor.is_showing_autocomplete());
        let at_arg = editor
            .render(80)
            .into_iter()
            .map(|l| strip_terminal_sequences(&l))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(at_arg.contains("repo"), "argument menu should be visible at `/cmd `");

        // Arrow Left back into the command name (`/cmd`).
        editor.handle_input("\x1b[D");
        flush_autocomplete().await;

        // The picker must have re-queried: the stale argument items are gone
        // (replaced by the command-name suggestion, or the picker closed).
        let after_move = editor
            .render(80)
            .into_iter()
            .map(|l| strip_terminal_sequences(&l))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !after_move.contains("repo"),
            "stale argument menu must not survive the cursor move"
        );
        assert!(
            !after_move.contains("message"),
            "stale argument menu must not survive the cursor move"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn debounces_hash_autocomplete_while_typing() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let suggestion_calls = Arc::new(Mutex::new(0i64));
        let calls_ref = Arc::clone(&suggestion_calls);

        let mock_provider = TestAutocompleteProvider::new(None, move |lines, _cursor_line, cursor_col, _| {
            let text = str16::slice(
                lines.first().map(String::as_str).unwrap_or(""),
                0,
                Some(cursor_col.max(0)),
            );
            let calls_ref = Arc::clone(&calls_ref);
            Box::pin(async move {
                *calls_ref.lock().unwrap() += 1;
                Some(AutocompleteSuggestions {
                    items: vec![AutocompleteItem {
                        value: "#2983".to_string(),
                        label: "#2983".to_string(),
                        description: None,
                    }],
                    prefix: text,
                })
            })
        });

        editor.set_autocomplete_provider(mock_provider);

        for ch in ["#", "2", "9", "8"] {
            editor.handle_input(ch);
        }

        assert_eq!(*suggestion_calls.lock().unwrap(), 0);
        assert!(!editor.is_showing_autocomplete());

        tokio::time::advance(Duration::from_millis(50)).await;
        flush_autocomplete().await;

        assert_eq!(*suggestion_calls.lock().unwrap(), 1);
        assert!(editor.is_showing_autocomplete());
    }

    #[tokio::test(start_paused = true)]
    async fn debounces_custom_trigger_characters_autocomplete_while_typing() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let suggestion_calls = Arc::new(Mutex::new(0i64));
        let calls_ref = Arc::clone(&suggestion_calls);

        let mock_provider = TestAutocompleteProvider::new(
            Some(vec!["$".to_string()]),
            move |lines, _cursor_line, cursor_col, _| {
                let prefix = str16::slice(
                    lines.first().map(String::as_str).unwrap_or(""),
                    0,
                    Some(cursor_col.max(0)),
                );
                let calls_ref = Arc::clone(&calls_ref);
                Box::pin(async move {
                    *calls_ref.lock().unwrap() += 1;
                    Some(AutocompleteSuggestions {
                        items: vec![AutocompleteItem {
                            value: "$skill-name".to_string(),
                            label: "skill-name".to_string(),
                            description: None,
                        }],
                        prefix,
                    })
                })
            },
        );
        editor.set_autocomplete_provider(mock_provider);

        for ch in ["$", "s", "k"] {
            editor.handle_input(ch);
        }

        assert_eq!(*suggestion_calls.lock().unwrap(), 0);
        tokio::time::advance(Duration::from_millis(50)).await;
        flush_autocomplete().await;

        assert_eq!(*suggestion_calls.lock().unwrap(), 1);
        assert!(editor.is_showing_autocomplete());
    }

    #[tokio::test(start_paused = true)]
    async fn resets_custom_trigger_characters_when_provider_changes() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let suggestion_calls = Arc::new(Mutex::new(0i64));
        let calls_ref = Arc::clone(&suggestion_calls);

        editor.set_autocomplete_provider(TestAutocompleteProvider::new(
            Some(vec!["$".to_string()]),
            |_, _, _, _| {
                Box::pin(async move {
                    Some(AutocompleteSuggestions {
                        items: vec![AutocompleteItem {
                            value: "$skill-name".to_string(),
                            label: "skill-name".to_string(),
                            description: None,
                        }],
                        prefix: "$".to_string(),
                    })
                })
            },
        ));
        editor.set_autocomplete_provider(TestAutocompleteProvider::new(None, move |_, _, _, _| {
            let calls_ref = Arc::clone(&calls_ref);
            Box::pin(async move {
                *calls_ref.lock().unwrap() += 1;
                Some(AutocompleteSuggestions {
                    items: vec![AutocompleteItem {
                        value: "$skill-name".to_string(),
                        label: "skill-name".to_string(),
                        description: None,
                    }],
                    prefix: "$".to_string(),
                })
            })
        }));

        editor.handle_input("$");
        editor.handle_input("s");
        tokio::time::advance(Duration::from_millis(50)).await;
        flush_autocomplete().await;

        assert_eq!(*suggestion_calls.lock().unwrap(), 0);
        assert!(!editor.is_showing_autocomplete());
    }

    #[tokio::test(start_paused = true)]
    async fn aborts_active_at_autocomplete_when_typing_continues() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let aborts = Arc::new(Mutex::new(0i64));
        let aborts_ref = Arc::clone(&aborts);

        let mock_provider = TestAutocompleteProvider::new(None, move |_lines, _cursor_line, _cursor_col, options| {
            let aborts_ref = Arc::clone(&aborts_ref);
            Box::pin(async move {
                let (tx, rx) = tokio::sync::oneshot::channel::<Option<AutocompleteSuggestions>>();
                let tx = Arc::new(Mutex::new(Some(tx)));
                let tx_timer = Arc::clone(&tx);
                let timer = Arc::new(Mutex::new(Some(pi_js::time::set_timeout(500, move || {
                    if let Some(sender) = tx_timer.lock().unwrap().take() {
                        let _ = sender.send(Some(AutocompleteSuggestions {
                            items: vec![AutocompleteItem {
                                value: "@main.ts".to_string(),
                                label: "main.ts".to_string(),
                                description: None,
                            }],
                            prefix: "@main".to_string(),
                        }));
                    }
                }))));
                let timer_abort = Arc::clone(&timer);
                let tx_abort = Arc::clone(&tx);
                let _guard = options.signal.on_abort(move |_| {
                    *aborts_ref.lock().unwrap() += 1;
                    if let Some(t) = timer_abort.lock().unwrap().take() {
                        t.clear();
                    }
                    if let Some(sender) = tx_abort.lock().unwrap().take() {
                        let _ = sender.send(None);
                    }
                });
                rx.await.unwrap_or(None)
            })
        });

        editor.set_autocomplete_provider(mock_provider);

        for ch in ["@", "m", "a", "i"] {
            editor.handle_input(ch);
        }
        tokio::time::advance(Duration::from_millis(250)).await;
        flush_autocomplete().await;
        editor.handle_input("n");
        tokio::time::advance(Duration::from_millis(50)).await;
        flush_autocomplete().await;

        assert_eq!(*aborts.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn hides_autocomplete_when_backspacing_slash_command_to_empty() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        // Mock provider with slash commands
        let mock_provider = TestAutocompleteProvider::new(None, |lines, _cursor_line, cursor_col, _| {
            let text = lines.first().map(String::as_str).unwrap_or("");
            let prefix = str16::slice(text, 0, Some(cursor_col.max(0)));
            Box::pin(async move {
                // Only return slash command suggestions when line starts with /
                if prefix.starts_with('/') {
                    let commands = [
                        AutocompleteItem {
                            value: "/model".to_string(),
                            label: "model".to_string(),
                            description: Some("Change model".to_string()),
                        },
                        AutocompleteItem {
                            value: "/help".to_string(),
                            label: "help".to_string(),
                            description: Some("Show help".to_string()),
                        },
                    ];
                    let query = str16::slice(&prefix, 1, None); // Remove leading /
                    let filtered: Vec<AutocompleteItem> =
                        commands.into_iter().filter(|c| c.value.starts_with(&query)).collect();
                    if !filtered.is_empty() {
                        return Some(AutocompleteSuggestions {
                            items: filtered,
                            prefix,
                        });
                    }
                }
                None
            })
        });

        editor.set_autocomplete_provider(mock_provider);

        // Type "/" - should show slash command suggestions
        editor.handle_input("/");
        flush_autocomplete().await;
        assert_eq!(editor.get_text(), "/");
        assert!(editor.is_showing_autocomplete());

        // Backspace to delete "/" - should hide autocomplete completely
        editor.handle_input("\x7f"); // Backspace
        flush_autocomplete().await;
        assert_eq!(editor.get_text(), "");
        assert!(!editor.is_showing_autocomplete());
    }

    #[tokio::test]
    async fn applies_exact_typed_slash_argument_value_on_enter_even_when_first_item_is_highlighted() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let argtest_re = Regex::new(r"^/argtest\s+(\S+)$").unwrap();

        // Mock provider for /argtest command with argument completions
        let mock_provider = TestAutocompleteProvider::new(None, move |lines, _cursor_line, cursor_col, _| {
            let text = lines.first().map(String::as_str).unwrap_or("");
            let before_cursor = str16::slice(text, 0, Some(cursor_col.max(0)));
            let captured = argtest_re
                .captures(&before_cursor)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string());
            Box::pin(async move {
                // Check if we're in argument completion context: "/argtest <prefix>"
                if let Some(argument_text) = captured {
                    let all_arguments = ["one", "two", "three"].into_iter().map(|s| AutocompleteItem {
                        value: s.to_string(),
                        label: s.to_string(),
                        description: None,
                    });
                    // Return all arguments that start with the typed prefix
                    let filtered: Vec<AutocompleteItem> = all_arguments
                        .filter(|arg| arg.value.starts_with(&argument_text))
                        .collect();
                    if !filtered.is_empty() {
                        return Some(AutocompleteSuggestions {
                            items: filtered,
                            prefix: argument_text,
                        });
                    }
                }
                None
            })
        });

        editor.set_autocomplete_provider(mock_provider);

        // Type "/argtest two"
        for ch in ["/", "a", "r", "g", "t", "e", "s", "t", " ", "t", "w", "o"] {
            editor.handle_input(ch);
        }

        assert_eq!(editor.get_text(), "/argtest two");
        flush_autocomplete().await;
        assert!(editor.is_showing_autocomplete());

        // Press Enter - should apply the exact typed value "two", not the first item
        editor.handle_input("\r");

        // The exact typed value "two" should be retained
        assert_eq!(editor.get_text(), "/argtest two");
    }

    #[tokio::test]
    async fn selects_first_prefix_match_on_enter_when_typed_arg_is_not_exact_match() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let argtest_re = Regex::new(r"^/argtest\s+(\S+)$").unwrap();

        // Mock provider for /argtest command with argument completions
        let mock_provider = TestAutocompleteProvider::new(None, move |lines, _cursor_line, cursor_col, _| {
            let text = lines.first().map(String::as_str).unwrap_or("");
            let before_cursor = str16::slice(text, 0, Some(cursor_col.max(0)));
            let captured = argtest_re
                .captures(&before_cursor)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string());
            Box::pin(async move {
                // Check if we're in argument completion context
                if let Some(argument_text) = captured {
                    let all_arguments = ["two", "three", "twelve"].into_iter().map(|s| AutocompleteItem {
                        value: s.to_string(),
                        label: s.to_string(),
                        description: None,
                    });
                    // Return all items that start with the typed prefix
                    let filtered: Vec<AutocompleteItem> = all_arguments
                        .filter(|arg| arg.value.starts_with(&argument_text))
                        .collect();
                    if !filtered.is_empty() {
                        return Some(AutocompleteSuggestions {
                            items: filtered,
                            prefix: argument_text,
                        });
                    }
                }
                None
            })
        });

        editor.set_autocomplete_provider(mock_provider);

        // Type "/argtest t" - filtered to [two, three, twelve], prefix "t" matches "two" first
        for ch in ["/", "a", "r", "g", "t", "e", "s", "t", " ", "t"] {
            editor.handle_input(ch);
        }

        flush_autocomplete().await;
        assert!(editor.is_showing_autocomplete());

        // Press Enter - "t" prefix matches "two" (first in list), so "two" is applied
        editor.handle_input("\r");
        assert_eq!(editor.get_text(), "/argtest two");
    }

    #[tokio::test]
    async fn highlights_unique_prefix_match_as_user_types_before_full_exact_match() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let argtest_re = Regex::new(r"^/argtest\s+(\S+)$").unwrap();

        // Mock provider that returns all items unfiltered (like real extensions do)
        let mock_provider = TestAutocompleteProvider::new(None, move |lines, _cursor_line, cursor_col, _| {
            let text = lines.first().map(String::as_str).unwrap_or("");
            let before_cursor = str16::slice(text, 0, Some(cursor_col.max(0)));
            let captured = argtest_re
                .captures(&before_cursor)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string());
            Box::pin(async move {
                if let Some(argument_text) = captured {
                    // Return all items - provider does not filter
                    let all_arguments = ["one", "two", "three"]
                        .into_iter()
                        .map(|s| AutocompleteItem {
                            value: s.to_string(),
                            label: s.to_string(),
                            description: None,
                        })
                        .collect();
                    return Some(AutocompleteSuggestions {
                        items: all_arguments,
                        prefix: argument_text,
                    });
                }
                None
            })
        });

        editor.set_autocomplete_provider(mock_provider);

        // Type "/argtest tw" - "tw" is a prefix of only "two"
        for ch in ["/", "a", "r", "g", "t", "e", "s", "t", " ", "t", "w"] {
            editor.handle_input(ch);
        }

        assert_eq!(editor.get_text(), "/argtest tw");
        flush_autocomplete().await;
        assert!(editor.is_showing_autocomplete());

        // Press Enter - "tw" uniquely matches "two", so "two" should be applied
        editor.handle_input("\r");
        assert_eq!(editor.get_text(), "/argtest two");
    }

    #[tokio::test]
    async fn selects_first_prefix_match_when_multiple_items_match() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let argtest_re = Regex::new(r"^/argtest\s+(\S+)$").unwrap();

        // Mock provider that returns all items unfiltered
        let mock_provider = TestAutocompleteProvider::new(None, move |lines, _cursor_line, cursor_col, _| {
            let text = lines.first().map(String::as_str).unwrap_or("");
            let before_cursor = str16::slice(text, 0, Some(cursor_col.max(0)));
            let captured = argtest_re
                .captures(&before_cursor)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string());
            Box::pin(async move {
                if let Some(argument_text) = captured {
                    let all_arguments = ["one", "two", "three"]
                        .into_iter()
                        .map(|s| AutocompleteItem {
                            value: s.to_string(),
                            label: s.to_string(),
                            description: None,
                        })
                        .collect();
                    return Some(AutocompleteSuggestions {
                        items: all_arguments,
                        prefix: argument_text,
                    });
                }
                None
            })
        });

        editor.set_autocomplete_provider(mock_provider);

        // Type "/argtest t" - "t" is a prefix of both "two" and "three"
        for ch in ["/", "a", "r", "g", "t", "e", "s", "t", " ", "t"] {
            editor.handle_input(ch);
        }

        flush_autocomplete().await;
        assert!(editor.is_showing_autocomplete());

        // Press Enter - "t" matches "two" first, so "two" is selected
        editor.handle_input("\r");
        assert_eq!(editor.get_text(), "/argtest two");
    }

    #[tokio::test]
    async fn works_for_built_in_style_command_argument_completion_path_model_like() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let model_re = Regex::new(r"^/model\s+(\S+)$").unwrap();

        // Mock provider for /model command with model completions
        let mock_provider = TestAutocompleteProvider::new(None, move |lines, _cursor_line, cursor_col, _| {
            let text = lines.first().map(String::as_str).unwrap_or("");
            let before_cursor = str16::slice(text, 0, Some(cursor_col.max(0)));
            let captured = model_re
                .captures(&before_cursor)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string());
            Box::pin(async move {
                // Check if we're in /model argument completion context
                if let Some(model_text) = captured {
                    let all_models = ["gpt-4o", "gpt-4o-mini", "claude-sonnet"]
                        .into_iter()
                        .map(|s| AutocompleteItem {
                            value: s.to_string(),
                            label: s.to_string(),
                            description: None,
                        });
                    // Return all models that start with the typed prefix
                    let filtered: Vec<AutocompleteItem> =
                        all_models.filter(|m| m.value.starts_with(&model_text)).collect();
                    if !filtered.is_empty() {
                        return Some(AutocompleteSuggestions {
                            items: filtered,
                            prefix: model_text,
                        });
                    }
                }
                None
            })
        });

        editor.set_autocomplete_provider(mock_provider);

        // Type "/model gpt-4o-mini" - exact match for second item in list
        for ch in [
            "/", "m", "o", "d", "e", "l", " ", "g", "p", "t", "-", "4", "o", "-", "m", "i", "n", "i",
        ] {
            editor.handle_input(ch);
        }

        assert_eq!(editor.get_text(), "/model gpt-4o-mini");
        flush_autocomplete().await;
        assert!(editor.is_showing_autocomplete());

        // Press Enter - should retain exact typed value, not apply first highlighted item
        editor.handle_input("\r");

        // The exact typed value should be retained
        assert_eq!(editor.get_text(), "/model gpt-4o-mini");
    }

    #[tokio::test]
    async fn awaits_async_slash_command_argument_completions() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
        let provider = Arc::new(CombinedAutocompleteProvider::new(
            Some(vec![CombinedCommand::SlashCommand(SlashCommand {
                name: "load-skills".to_string(),
                description: Some("Load skills".to_string()),
                argument_hint: None,
                get_argument_completions: Some(Arc::new(|prefix| {
                    let prefix = prefix.to_string();
                    Box::pin(async move {
                        if prefix.starts_with('s') {
                            Some(vec![AutocompleteItem {
                                value: "skill-a".to_string(),
                                label: "skill-a".to_string(),
                                description: None,
                            }])
                        } else {
                            None
                        }
                    })
                })),
            })]),
            &cwd,
            None,
        ));
        editor.set_autocomplete_provider(provider);
        editor.set_text("/load-skills ");

        editor.handle_input("s");
        flush_autocomplete().await;
        assert!(editor.is_showing_autocomplete());

        editor.handle_input("\t");
        assert_eq!(editor.get_text(), "/load-skills skill-a");
        assert!(!editor.is_showing_autocomplete());
    }

    #[tokio::test]
    async fn ignores_invalid_slash_command_argument_completion_results() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
        let provider = Arc::new(CombinedAutocompleteProvider::new(
            Some(vec![CombinedCommand::SlashCommand(SlashCommand {
                name: "load-skills".to_string(),
                description: Some("Load skills".to_string()),
                argument_hint: None,
                // PORT: TS casts a non-array return value via `as unknown as ...`;
                // Rust enforces `Option<Vec<AutocompleteItem>>`, so returning `None` exercises the rejected-result path.
                get_argument_completions: Some(Arc::new(|_| Box::pin(async move { None }))),
            })]),
            &cwd,
            None,
        ));
        editor.set_autocomplete_provider(provider);
        editor.set_text("/load-skills ");

        editor.handle_input("s");
        flush_autocomplete().await;
        assert!(!editor.is_showing_autocomplete());
        assert_eq!(editor.get_text(), "/load-skills s");
    }

    #[tokio::test]
    async fn does_not_show_argument_completions_when_command_has_no_argument_completer() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
        let provider = Arc::new(CombinedAutocompleteProvider::new(
            Some(vec![
                CombinedCommand::SlashCommand(SlashCommand {
                    name: "help".to_string(),
                    description: Some("Show help".to_string()),
                    argument_hint: None,
                    get_argument_completions: None,
                }),
                CombinedCommand::SlashCommand(SlashCommand {
                    name: "model".to_string(),
                    description: Some("Switch model".to_string()),
                    argument_hint: None,
                    get_argument_completions: Some(Arc::new(|_| {
                        Box::pin(async move {
                            Some(vec![AutocompleteItem {
                                value: "claude-opus".to_string(),
                                label: "claude-opus".to_string(),
                                description: None,
                            }])
                        })
                    })),
                }),
            ]),
            &cwd,
            None,
        ));
        editor.set_autocomplete_provider(provider);

        editor.handle_input("/");
        editor.handle_input("h");
        editor.handle_input("e");
        flush_autocomplete().await;
        assert!(editor.is_showing_autocomplete());

        editor.handle_input("\t");
        assert_eq!(editor.get_text(), "/help ");
        assert!(!editor.is_showing_autocomplete());
    }
}

mod character_jump_ctrl_bracket {
    use super::*;

    #[test]
    fn jumps_forward_to_first_occurrence_of_character_on_same_line() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("hello world");
        editor.handle_input("\x01"); // Ctrl+A - go to start
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 });

        editor.handle_input("\x1d"); // Ctrl+] (legacy sequence for ctrl+])
        editor.handle_input("o"); // Jump to first 'o'

        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 4 }); // 'o' in "hello"
    }

    #[test]
    fn jumps_forward_to_next_occurrence_after_cursor() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("hello world");
        editor.handle_input("\x01"); // Ctrl+A - go to start
        // Move cursor to the 'o' in "hello" (col 4)
        for _ in 0..4 {
            editor.handle_input("\x1b[C");
        }
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 4 });

        editor.handle_input("\x1d"); // Ctrl+]
        editor.handle_input("o"); // Jump to next 'o' (in "world")

        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 7 }); // 'o' in "world"
    }

    #[test]
    fn jumps_forward_across_multiple_lines() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("abc\ndef\nghi");
        // Cursor is at end (line 2, col 3). Move to line 0 via up arrows, then Ctrl+A
        editor.handle_input("\x1b[A"); // Up
        editor.handle_input("\x1b[A"); // Up - now on line 0
        editor.handle_input("\x01"); // Ctrl+A - go to start of line
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 });

        editor.handle_input("\x1d"); // Ctrl+]
        editor.handle_input("g"); // Jump to 'g' on line 3

        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 0 });
    }

    #[test]
    fn jumps_backward_to_first_occurrence_before_cursor_on_same_line() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("hello world");
        // Cursor at end (col 11)
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 11 });

        editor.handle_input("\x1b\x1d"); // Ctrl+Alt+] (ESC followed by Ctrl+])
        editor.handle_input("o"); // Jump to last 'o' before cursor

        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 7 }); // 'o' in "world"
    }

    #[test]
    fn jumps_backward_across_multiple_lines() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("abc\ndef\nghi");
        // Cursor at end of line 3
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 3 });

        editor.handle_input("\x1b\x1d"); // Ctrl+Alt+]
        editor.handle_input("a"); // Jump to 'a' on line 1

        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 });
    }

    #[test]
    fn does_nothing_when_character_is_not_found_forward() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("hello world");
        editor.handle_input("\x01"); // Ctrl+A - go to start
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 });

        editor.handle_input("\x1d"); // Ctrl+]
        editor.handle_input("z"); // 'z' doesn't exist

        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 }); // Cursor unchanged
    }

    #[test]
    fn does_nothing_when_character_is_not_found_backward() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("hello world");
        // Cursor at end
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 11 });

        editor.handle_input("\x1b\x1d"); // Ctrl+Alt+]
        editor.handle_input("z"); // 'z' doesn't exist

        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 11 }); // Cursor unchanged
    }

    #[test]
    fn is_case_sensitive() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("Hello World");
        editor.handle_input("\x01"); // Ctrl+A - go to start
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 });

        // Search for lowercase 'h' - should not find it (only 'H' exists)
        editor.handle_input("\x1d"); // Ctrl+]
        editor.handle_input("h");

        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 }); // Cursor unchanged

        // Search for uppercase 'W' - should find it
        editor.handle_input("\x1d"); // Ctrl+]
        editor.handle_input("W");

        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 6 }); // 'W' in "World"
    }

    #[test]
    fn cancels_jump_mode_when_ctrl_bracket_is_pressed_again() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("hello world");
        editor.handle_input("\x01"); // Ctrl+A - go to start
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 });

        editor.handle_input("\x1d"); // Ctrl+] - enter jump mode
        editor.handle_input("\x1d"); // Ctrl+] again - cancel

        // Type 'o' normally - should insert, not jump
        editor.handle_input("o");
        assert_eq!(editor.get_text(), "ohello world");
    }

    #[test]
    fn cancels_jump_mode_on_escape_and_processes_the_escape() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("hello world");
        editor.handle_input("\x01"); // Ctrl+A - go to start
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 });

        editor.handle_input("\x1d"); // Ctrl+] - enter jump mode
        editor.handle_input("\x1b"); // Escape - cancel jump mode

        // Cursor should be unchanged (Escape itself doesn't move cursor in editor)
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 });

        // Type 'o' normally - should insert, not jump
        editor.handle_input("o");
        assert_eq!(editor.get_text(), "ohello world");
    }

    #[test]
    fn cancels_backward_jump_mode_when_ctrl_alt_bracket_is_pressed_again() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("hello world");
        // Cursor at end
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 11 });

        editor.handle_input("\x1b\x1d"); // Ctrl+Alt+] - enter backward jump mode
        editor.handle_input("\x1b\x1d"); // Ctrl+Alt+] again - cancel

        // Type 'o' normally - should insert, not jump
        editor.handle_input("o");
        assert_eq!(editor.get_text(), "hello worldo");
    }

    #[test]
    fn searches_for_special_characters() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("foo(bar) = baz;");
        editor.handle_input("\x01"); // Ctrl+A - go to start
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 });

        // Jump to '('
        editor.handle_input("\x1d"); // Ctrl+]
        editor.handle_input("(");

        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 3 });

        // Jump to '='
        editor.handle_input("\x1d"); // Ctrl+]
        editor.handle_input("=");

        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 9 });
    }

    #[test]
    fn handles_empty_text_gracefully() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 });

        editor.handle_input("\x1d"); // Ctrl+]
        editor.handle_input("x");

        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 }); // Cursor unchanged
    }

    #[test]
    fn resets_last_action_when_jumping() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("hello world");
        editor.handle_input("\x01"); // Ctrl+A - go to start

        // Type to set lastAction to "type-word"
        editor.handle_input("x");
        assert_eq!(editor.get_text(), "xhello world");

        // Jump forward
        editor.handle_input("\x1d"); // Ctrl+]
        editor.handle_input("o");

        // Type more - should start a new undo unit (lastAction was reset)
        editor.handle_input("Y");
        assert_eq!(editor.get_text(), "xhellYo world");

        // Undo should only undo "Y", not "x" as well
        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "xhello world");
    }
}

mod sticky_column {
    use super::*;

    /// Helper: position cursor at a specific line and column
    fn position_cursor(editor: &Editor, line: usize, col: usize) {
        // Go to line 0 first
        for _ in 0..20 {
            editor.handle_input("\x1b[A");
        }
        // Go to target line
        for _ in 0..line {
            editor.handle_input("\x1b[B");
        }
        // Go to target col
        editor.handle_input("\x01"); // Ctrl+A
        for _ in 0..col {
            editor.handle_input("\x1b[C");
        }
    }

    #[test]
    fn preserves_target_column_when_moving_up_through_a_shorter_line() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        // Line 0: "2222222222x222" (x at col 10)
        // Line 1: "" (empty)
        // Line 2: "1111111111_111111111111" (_ at col 10)
        editor.set_text("2222222222x222\n\n1111111111_111111111111");

        // Position cursor on _ (line 2, col 10)
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 23 }); // At end
        editor.handle_input("\x01"); // Ctrl+A - go to start of line
        for _ in 0..10 {
            editor.handle_input("\x1b[C"); // Move right to col 10
        }
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 10 });

        // Press Up - should move to empty line (col clamped to 0)
        editor.handle_input("\x1b[A"); // Up arrow
        assert_eq!(editor.get_cursor(), EditorCursor { line: 1, col: 0 });

        // Press Up again - should move to line 0 at col 10 (on 'x')
        editor.handle_input("\x1b[A"); // Up arrow
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 10 });
    }

    #[test]
    fn preserves_target_column_when_moving_down_through_a_shorter_line() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("1111111111_111\n\n2222222222x222222222222");

        // Position cursor on _ (line 0, col 10)
        editor.handle_input("\x1b[A"); // Up to line 1
        editor.handle_input("\x1b[A"); // Up to line 0
        editor.handle_input("\x01"); // Ctrl+A
        for _ in 0..10 {
            editor.handle_input("\x1b[C");
        }
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 10 });

        // Press Down - should move to empty line (col clamped to 0)
        editor.handle_input("\x1b[B"); // Down arrow
        assert_eq!(editor.get_cursor(), EditorCursor { line: 1, col: 0 });

        // Press Down again - should move to line 2 at col 10 (on 'x')
        editor.handle_input("\x1b[B"); // Down arrow
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 10 });
    }

    #[test]
    fn resets_sticky_column_on_horizontal_movement_left_arrow() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("1234567890\n\n1234567890");

        // Start at line 2, col 5
        editor.handle_input("\x01"); // Ctrl+A
        for _ in 0..5 {
            editor.handle_input("\x1b[C");
        }
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 5 });

        // Move up through empty line
        editor.handle_input("\x1b[A"); // Up - line 1, col 0
        editor.handle_input("\x1b[A"); // Up - line 0, col 5 (sticky)
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 5 });

        // Move left - resets sticky column
        editor.handle_input("\x1b[D"); // Left
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 4 });

        // Move down twice
        editor.handle_input("\x1b[B"); // Down - line 1, col 0
        editor.handle_input("\x1b[B"); // Down - line 2, col 4 (new sticky from col 4)
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 4 });
    }

    #[test]
    fn resets_sticky_column_on_horizontal_movement_right_arrow() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("1234567890\n\n1234567890");

        // Start at line 0, col 5
        editor.handle_input("\x1b[A"); // Up to line 1
        editor.handle_input("\x1b[A"); // Up to line 0
        editor.handle_input("\x01"); // Ctrl+A
        for _ in 0..5 {
            editor.handle_input("\x1b[C");
        }
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 5 });

        // Move down through empty line
        editor.handle_input("\x1b[B"); // Down - line 1, col 0
        editor.handle_input("\x1b[B"); // Down - line 2, col 5 (sticky)
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 5 });

        // Move right - resets sticky column
        editor.handle_input("\x1b[C"); // Right
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 6 });

        // Move up twice
        editor.handle_input("\x1b[A"); // Up - line 1, col 0
        editor.handle_input("\x1b[A"); // Up - line 0, col 6 (new sticky from col 6)
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 6 });
    }

    #[test]
    fn resets_sticky_column_on_typing() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("1234567890\n\n1234567890");

        // Start at line 2, col 8
        editor.handle_input("\x01"); // Ctrl+A
        for _ in 0..8 {
            editor.handle_input("\x1b[C");
        }

        // Move up through empty line
        editor.handle_input("\x1b[A"); // Up
        editor.handle_input("\x1b[A"); // Up - line 0, col 8
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 8 });

        // Type a character - resets sticky column
        editor.handle_input("X");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 9 });

        // Move down twice
        editor.handle_input("\x1b[B"); // Down - line 1, col 0
        editor.handle_input("\x1b[B"); // Down - line 2, col 9 (new sticky from col 9)
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 9 });
    }

    #[test]
    fn resets_sticky_column_on_backspace() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("1234567890\n\n1234567890");

        // Start at line 2, col 8
        editor.handle_input("\x01"); // Ctrl+A
        for _ in 0..8 {
            editor.handle_input("\x1b[C");
        }

        // Move up through empty line
        editor.handle_input("\x1b[A"); // Up
        editor.handle_input("\x1b[A"); // Up - line 0, col 8
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 8 });

        // Backspace - resets sticky column
        editor.handle_input("\x7f"); // Backspace
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 7 });

        // Move down twice
        editor.handle_input("\x1b[B"); // Down - line 1, col 0
        editor.handle_input("\x1b[B"); // Down - line 2, col 7 (new sticky from col 7)
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 7 });
    }

    #[test]
    fn resets_sticky_column_on_ctrl_a_move_to_line_start() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("1234567890\n\n1234567890");

        // Start at line 2, col 8
        editor.handle_input("\x01"); // Ctrl+A
        for _ in 0..8 {
            editor.handle_input("\x1b[C");
        }

        // Move up - establishes sticky col 8
        editor.handle_input("\x1b[A"); // Up - line 1, col 0

        // Ctrl+A - resets sticky column to 0
        editor.handle_input("\x01"); // Ctrl+A
        assert_eq!(editor.get_cursor(), EditorCursor { line: 1, col: 0 });

        // Move up
        editor.handle_input("\x1b[A"); // Up - line 0, col 0 (new sticky from col 0)
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 });
    }

    #[test]
    fn resets_sticky_column_on_ctrl_e_move_to_line_end() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("12345\n\n1234567890");

        // Start at line 2, col 3
        editor.handle_input("\x01"); // Ctrl+A
        for _ in 0..3 {
            editor.handle_input("\x1b[C");
        }

        // Move up through empty line - establishes sticky col 3
        editor.handle_input("\x1b[A"); // Up - line 1, col 0
        editor.handle_input("\x1b[A"); // Up - line 0, col 3
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 3 });

        // Ctrl+E - resets sticky column to end
        editor.handle_input("\x05"); // Ctrl+E
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 5 });

        // Move down twice
        editor.handle_input("\x1b[B"); // Down - line 1, col 0
        editor.handle_input("\x1b[B"); // Down - line 2, col 5 (new sticky from col 5)
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 5 });
    }

    #[test]
    fn resets_sticky_column_on_word_movement_ctrl_left() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("hello world\n\nhello world");

        // Start at end of line 2 (col 11)
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 11 });

        // Move up through empty line - establishes sticky col 11
        editor.handle_input("\x1b[A"); // Up - line 1, col 0
        editor.handle_input("\x1b[A"); // Up - line 0, col 11
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 11 });

        // Ctrl+Left - word movement resets sticky column
        editor.handle_input("\x1b[1;5D"); // Ctrl+Left
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 6 }); // Before "world"

        // Move down twice
        editor.handle_input("\x1b[B"); // Down - line 1, col 0
        editor.handle_input("\x1b[B"); // Down - line 2, col 6 (new sticky from col 6)
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 6 });
    }

    #[test]
    fn resets_sticky_column_on_word_movement_ctrl_right() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("hello world\n\nhello world");

        // Start at line 0, col 0
        editor.handle_input("\x1b[A"); // Up
        editor.handle_input("\x1b[A"); // Up
        editor.handle_input("\x01"); // Ctrl+A
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 });

        // Move down through empty line - establishes sticky col 0
        editor.handle_input("\x1b[B"); // Down - line 1, col 0
        editor.handle_input("\x1b[B"); // Down - line 2, col 0
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 0 });

        // Ctrl+Right - word movement resets sticky column
        editor.handle_input("\x1b[1;5C"); // Ctrl+Right
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 5 }); // After "hello"

        // Move up twice
        editor.handle_input("\x1b[A"); // Up - line 1, col 0
        editor.handle_input("\x1b[A"); // Up - line 0, col 5 (new sticky from col 5)
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 5 });
    }

    #[test]
    fn resets_sticky_column_on_undo() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("1234567890\n\n1234567890");

        // Go to line 0, col 8
        editor.handle_input("\x1b[A"); // Up to line 1
        editor.handle_input("\x1b[A"); // Up to line 0
        editor.handle_input("\x01"); // Ctrl+A
        for _ in 0..8 {
            editor.handle_input("\x1b[C");
        }
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 8 });

        // Move down through empty line - establishes sticky col 8
        editor.handle_input("\x1b[B"); // Down - line 1, col 0
        editor.handle_input("\x1b[B"); // Down - line 2, col 8 (sticky)
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 8 });

        // Type something to create undo state - this clears sticky and sets col to 9
        editor.handle_input("X");
        assert_eq!(editor.get_text(), "1234567890\n\n12345678X90");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 9 });

        // Move up - establishes new sticky col 9
        editor.handle_input("\x1b[A"); // Up - line 1, col 0
        editor.handle_input("\x1b[A"); // Up - line 0, col 9
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 9 });

        // Undo - resets sticky column and restores cursor to line 2, col 8
        editor.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
        assert_eq!(editor.get_text(), "1234567890\n\n1234567890");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 8 });

        // Move up - should capture new sticky from restored col 8, not old col 9
        editor.handle_input("\x1b[A"); // Up - line 1, col 0
        editor.handle_input("\x1b[A"); // Up - line 0, col 8 (new sticky from restored position)
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 8 });
    }

    #[test]
    fn handles_multiple_consecutive_up_down_movements() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("1234567890\nab\ncd\nef\n1234567890");

        // Start at line 4, col 7
        editor.handle_input("\x01"); // Ctrl+A
        for _ in 0..7 {
            editor.handle_input("\x1b[C");
        }
        assert_eq!(editor.get_cursor(), EditorCursor { line: 4, col: 7 });

        // Move up multiple times through short lines
        editor.handle_input("\x1b[A"); // Up - line 3, col 2 (clamped)
        editor.handle_input("\x1b[A"); // Up - line 2, col 2 (clamped)
        editor.handle_input("\x1b[A"); // Up - line 1, col 2 (clamped)
        editor.handle_input("\x1b[A"); // Up - line 0, col 7 (restored)
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 7 });

        // Move down multiple times - sticky should still be 7
        editor.handle_input("\x1b[B"); // Down - line 1, col 2
        editor.handle_input("\x1b[B"); // Down - line 2, col 2
        editor.handle_input("\x1b[B"); // Down - line 3, col 2
        editor.handle_input("\x1b[B"); // Down - line 4, col 7 (restored)
        assert_eq!(editor.get_cursor(), EditorCursor { line: 4, col: 7 });
    }

    #[test]
    fn moves_correctly_through_wrapped_visual_lines_without_getting_stuck() {
        let tui = create_test_tui_with_width(15); // Narrow terminal
        let editor = Editor::new(tui, default_editor_theme(), None);

        // Line 0: short
        // Line 1: 30 chars = wraps to 3 visual lines at width 10 (after padding)
        editor.set_text("short\n123456789012345678901234567890");
        editor.render(15); // This gives 14 layout width

        // Position at end of line 1 (col 30)
        assert_eq!(editor.get_cursor(), EditorCursor { line: 1, col: 30 });

        // Move up repeatedly - should traverse all visual lines of the wrapped text
        // and eventually reach line 0
        editor.handle_input("\x1b[A"); // Up - to previous visual line within line 1
        assert_eq!(editor.get_cursor().line, 1);

        editor.handle_input("\x1b[A"); // Up - another visual line
        assert_eq!(editor.get_cursor().line, 1);

        editor.handle_input("\x1b[A"); // Up - should reach line 0
        assert_eq!(editor.get_cursor().line, 0);
    }

    #[test]
    fn handles_set_text_resetting_sticky_column() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        editor.set_text("1234567890\n\n1234567890");

        // Establish sticky column
        editor.handle_input("\x01"); // Ctrl+A
        for _ in 0..8 {
            editor.handle_input("\x1b[C");
        }
        editor.handle_input("\x1b[A"); // Up

        // setText should reset sticky column
        editor.set_text("abcdefghij\n\nabcdefghij");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 10 }); // At end

        // Move up - should capture new sticky from current position (10)
        editor.handle_input("\x1b[A"); // Up - line 1, col 0
        editor.handle_input("\x1b[A"); // Up - line 0, col 10
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 10 });
    }

    #[test]
    fn sets_preferred_visual_col_when_pressing_right_at_end_of_prompt_last_line() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        // Line 0: 20 chars with 'x' at col 10
        // Line 1: empty
        // Line 2: 10 chars ending with '_'
        editor.set_text("111111111x1111111111\n\n333333333_");

        // Go to line 0, press Ctrl+E (end of line) - col 20
        editor.handle_input("\x1b[A"); // Up to line 1
        editor.handle_input("\x1b[A"); // Up to line 0
        editor.handle_input("\x05"); // Ctrl+E - move to end of line
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 20 });

        // Move down to line 2 - cursor clamped to col 10 (end of line)
        editor.handle_input("\x1b[B"); // Down to line 1, col 0
        editor.handle_input("\x1b[B"); // Down to line 2, col 10 (clamped)
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 10 });

        // Press Right at end of prompt - nothing visible happens, but sets preferredVisualCol to 10
        editor.handle_input("\x1b[C"); // Right - can't move, but sets preferredVisualCol
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 10 }); // Still at same position

        // Move up twice to line 0 - should use preferredVisualCol (10) to land on 'x'
        editor.handle_input("\x1b[A"); // Up to line 1, col 0
        editor.handle_input("\x1b[A"); // Up to line 0, col 10 (on 'x')
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 10 });
    }

    #[test]
    fn handles_editor_resizes_when_preferred_visual_col_is_on_the_same_line() {
        // Create editor with wider terminal
        let tui = create_test_tui();
        let editor = Editor::new(tui, default_editor_theme(), None);

        editor.set_text("12345678901234567890\n\n12345678901234567890");

        // Start at line 2, col 15
        editor.handle_input("\x01"); // Ctrl+A
        for _ in 0..15 {
            editor.handle_input("\x1b[C");
        }

        // Move up through empty line - establishes sticky col 15
        editor.handle_input("\x1b[A"); // Up
        editor.handle_input("\x1b[A"); // Up - line 0, col 15
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 15 });

        // Render with narrower width to simulate resize
        editor.render(12); // Width 12

        // Move down - sticky should be clamped to new width
        editor.handle_input("\x1b[B"); // Down - line 1
        editor.handle_input("\x1b[B"); // Down - line 2, col should be clamped
        assert_eq!(editor.get_cursor().col, 4);
    }

    #[test]
    fn handles_editor_resizes_when_preferred_visual_col_is_on_a_different_line() {
        let tui = create_test_tui();
        let editor = Editor::new(tui, default_editor_theme(), None);

        // Create a line that wraps into multiple visual lines at width 10
        // "12345678901234567890" = 20 chars, wraps to 2 visual lines at width 10
        editor.set_text("short\n12345678901234567890");

        // Go to line 1, col 15
        editor.handle_input("\x01"); // Ctrl+A
        for _ in 0..15 {
            editor.handle_input("\x1b[C");
        }
        assert_eq!(editor.get_cursor(), EditorCursor { line: 1, col: 15 });

        // Move up to establish sticky col 15
        editor.handle_input("\x1b[A"); // Up to line 0
        // Line 0 has only 5 chars, so cursor at col 5
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 5 });

        // Narrow the editor
        editor.render(10);

        // Move down - preferredVisualCol was 15, but width is 10
        // Should land on line 1, clamped to width (visual col 9, which is logical col 9)
        editor.handle_input("\x1b[B"); // Down to line 1
        assert_eq!(editor.get_cursor(), EditorCursor { line: 1, col: 8 });

        // Move up
        editor.handle_input("\x1b[A"); // Up - should go to line 0
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 5 }); // Line 0 only has 5 chars

        // Restore the original width
        editor.render(80);

        // Move down - preferredVisualCol was kept at 15
        editor.handle_input("\x1b[B"); // Down to line 1
        assert_eq!(editor.get_cursor(), EditorCursor { line: 1, col: 15 });
    }

    #[test]
    fn rewrapped_lines_target_fits_current_visual_column() {
        let tui = create_test_tui();
        let editor = Editor::new(tui, default_editor_theme(), None);
        editor.set_text("abcdefghijklmnopqr\n123456789012345678");

        position_cursor(&editor, 0, 18);
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 18 });

        // Narrow to width 10 (layoutWidth = 9).
        // Line 0 last segment has visual col max 9, line 1 first segment max 8
        editor.render(10);

        // Move down: cursor clamps to 8
        editor.handle_input("\x1b[B");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 1, col: 8 });

        // Widen back. Move up, the current visual col wins
        editor.render(80);
        editor.handle_input("\x1b[A");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 8 });

        // Preferred was cleared by the rewrapped branch
        editor.handle_input("\x1b[B");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 1, col: 8 });
    }

    #[test]
    fn rewrapped_lines_target_shorter_than_current_visual_column() {
        let tui = create_test_tui();
        let editor = Editor::new(tui, default_editor_theme(), None);
        editor.set_text("abcdefghijklmnopqr\n123456789012345678\nab");

        position_cursor(&editor, 0, 18);
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 18 });

        // Narrow to width 10 (layoutWidth = 9). Moving down clamps to col 8
        editor.render(10);
        editor.handle_input("\x1b[B");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 1, col: 8 });

        // Widen the editor
        editor.render(80);

        // Move down to short line "ab".
        // preferredVisualCol is replaced with current visual col (8), cursor clamps to 2
        editor.handle_input("\x1b[B");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 2 });

        // Moving up restores to preferred col 8
        editor.handle_input("\x1b[A");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 1, col: 8 });
    }
}

mod paste_marker_atomic_behavior {
    use super::*;

    /// Helper: simulate a large paste that creates a marker
    fn paste_with_marker(editor: &Editor) -> String {
        let big_content = "line\n".repeat(20);
        let big_content = big_content.trim_end_matches('\n'); // 20 lines
        editor.handle_input(&format!("\x1b[200~{big_content}\x1b[201~"));
        // The editor replaces large pastes with a marker like "[paste #1 +20 lines]"
        editor.get_text()
    }

    /// Helper: 12-line paste content with a distinguishing tag
    fn big_paste(tag: &str) -> String {
        (0..12).map(|i| format!("{tag}{i}")).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn creates_a_paste_marker_for_large_pastes() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let text = paste_with_marker(&editor);
        assert!(Regex::new(r"\[paste #\d+ \+\d+ lines\]").unwrap().is_match(&text));
    }

    #[test]
    fn treats_paste_marker_as_single_unit_for_right_arrow() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        editor.handle_input("A");
        paste_with_marker(&editor);
        editor.handle_input("B");
        // Text: "A[paste #1 +20 lines]B", cursor at end

        // Go to start
        editor.handle_input("\x01"); // Ctrl+A
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 });

        // Right arrow: should move past "A"
        editor.handle_input("\x1b[C");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 1 });

        // Right arrow: should skip the entire marker
        editor.handle_input("\x1b[C");
        let text = editor.get_text();
        let marker = Regex::new(r"\[paste #\d+ \+\d+ lines\]")
            .unwrap()
            .find(&text)
            .unwrap()
            .as_str();
        let marker_len = i64::try_from(str16::len(marker)).unwrap();
        assert_eq!(
            editor.get_cursor(),
            EditorCursor {
                line: 0,
                col: 1 + marker_len
            }
        );

        // Right arrow: should move past "B"
        editor.handle_input("\x1b[C");
        assert_eq!(
            editor.get_cursor(),
            EditorCursor {
                line: 0,
                col: 1 + marker_len + 1
            }
        );
    }

    #[test]
    fn treats_paste_marker_as_single_unit_for_left_arrow() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        editor.handle_input("A");
        paste_with_marker(&editor);
        editor.handle_input("B");
        // Cursor at end

        // Left arrow: past "B"
        editor.handle_input("\x1b[D");
        let text = editor.get_text();
        let marker = Regex::new(r"\[paste #\d+ \+\d+ lines\]")
            .unwrap()
            .find(&text)
            .unwrap()
            .as_str();
        let marker_len = i64::try_from(str16::len(marker)).unwrap();
        assert_eq!(
            editor.get_cursor(),
            EditorCursor {
                line: 0,
                col: 1 + marker_len
            }
        );

        // Left arrow: skip the entire marker
        editor.handle_input("\x1b[D");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 1 });

        // Left arrow: past "A"
        editor.handle_input("\x1b[D");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 });
    }

    #[test]
    fn treats_paste_marker_as_single_unit_for_backspace() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        editor.handle_input("A");
        paste_with_marker(&editor);
        editor.handle_input("B");

        let text = editor.get_text();
        let marker = Regex::new(r"\[paste #\d+ \+\d+ lines\]")
            .unwrap()
            .find(&text)
            .unwrap()
            .as_str();
        let marker_len = i64::try_from(str16::len(marker)).unwrap();

        // Position cursor right after the marker (before "B")
        editor.handle_input("\x01"); // Ctrl+A
        // Move past "A" and the marker
        editor.handle_input("\x1b[C"); // past "A"
        editor.handle_input("\x1b[C"); // past marker
        assert_eq!(
            editor.get_cursor(),
            EditorCursor {
                line: 0,
                col: 1 + marker_len
            }
        );

        // Backspace: should delete the entire marker at once
        editor.handle_input("\x7f");
        assert_eq!(editor.get_text(), "AB");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 1 });
    }

    #[test]
    fn treats_paste_marker_as_single_unit_for_forward_delete() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        editor.handle_input("A");
        paste_with_marker(&editor);
        editor.handle_input("B");

        // Position cursor on "A" (col 0) then move right once to be just before marker
        editor.handle_input("\x01"); // Ctrl+A
        editor.handle_input("\x1b[C"); // past "A", now at col 1 (start of marker)

        // Forward delete: should delete the entire marker at once
        editor.handle_input("\x1b[3~"); // Delete key
        assert_eq!(editor.get_text(), "AB");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 1 });
    }

    #[test]
    fn treats_paste_marker_as_single_unit_for_word_movement() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        editor.handle_input("X");
        editor.handle_input(" ");
        paste_with_marker(&editor);
        editor.handle_input(" ");
        editor.handle_input("Y");
        // Text: "X [paste #1 +20 lines] Y"

        let text = editor.get_text();
        let marker = Regex::new(r"\[paste #\d+ \+\d+ lines\]")
            .unwrap()
            .find(&text)
            .unwrap()
            .as_str();
        let marker_len = i64::try_from(str16::len(marker)).unwrap();

        // Go to start
        editor.handle_input("\x01"); // Ctrl+A

        // Ctrl+Right: skip "X"
        editor.handle_input("\x1b[1;5C");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 1 });

        // Ctrl+Right: skip whitespace + marker (marker treated as single non-ws, non-punct unit)
        editor.handle_input("\x1b[1;5C");
        assert_eq!(
            editor.get_cursor(),
            EditorCursor {
                line: 0,
                col: 2 + marker_len
            }
        );
    }

    #[test]
    fn undo_restores_marker_after_backspace_deletion() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        editor.handle_input("A");
        paste_with_marker(&editor);
        editor.handle_input("B");

        let text_before = editor.get_text();

        // Position after marker
        editor.handle_input("\x01");
        editor.handle_input("\x1b[C"); // past A
        editor.handle_input("\x1b[C"); // past marker

        // Delete marker
        editor.handle_input("\x7f");
        assert_eq!(editor.get_text(), "AB");

        // Undo
        editor.handle_input("\x1b[45;5u");
        assert_eq!(editor.get_text(), text_before);
    }

    #[test]
    fn undo_after_paste_marker_deletion_restores_the_paste_registry() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let submitted = Arc::new(Mutex::new(String::new()));
        let submitted_ref = Arc::clone(&submitted);
        editor.set_on_submit(Some(Arc::new(move |t| {
            *submitted_ref.lock().unwrap() = t.to_string();
        })));

        let paste = big_paste("alpha");
        editor.handle_input(&format!("\x1b[200~{paste}\x1b[201~"));
        editor.handle_input("\x7f"); // delete the marker
        editor.handle_input("\x1b[45;5u"); // undo: restores marker text and registry
        editor.handle_input("\r");
        assert_eq!(*submitted.lock().unwrap(), paste);
    }

    #[test]
    fn undo_after_deleting_the_first_of_two_paste_markers_restores_both_registry_entries() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let submitted = Arc::new(Mutex::new(String::new()));
        let submitted_ref = Arc::clone(&submitted);
        editor.set_on_submit(Some(Arc::new(move |t| {
            *submitted_ref.lock().unwrap() = t.to_string();
        })));

        let paste_a = big_paste("alpha");
        let paste_b = big_paste("beta");
        editor.handle_input(&format!("\x1b[200~{paste_a}\x1b[201~")); // #1 = A
        editor.handle_input(&format!("\x1b[200~{paste_b}\x1b[201~")); // #2 = B, cursor at end
        editor.handle_input("\x01"); // Ctrl+A
        editor.handle_input("\x1b[C"); // right over marker #1
        editor.handle_input("\x7f"); // delete marker #1, renumbers #2 -> #1
        editor.handle_input("\x1b[45;5u"); // undo
        editor.handle_input("\r");
        assert_eq!(*submitted.lock().unwrap(), format!("{paste_a}{paste_b}"));
    }

    #[test]
    fn renumbers_the_paste_registry_in_ascending_id_order_when_markers_are_out_of_order_in_text() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let submitted = Arc::new(Mutex::new(String::new()));
        let submitted_ref = Arc::clone(&submitted);
        editor.set_on_submit(Some(Arc::new(move |t| {
            *submitted_ref.lock().unwrap() = t.to_string();
        })));

        let paste_a = big_paste("alpha");
        let paste_b = big_paste("beta");
        let paste_c = big_paste("gamma");
        editor.handle_input(&format!("\x1b[200~{paste_a}\x1b[201~")); // #1 = A
        editor.handle_input("\x01"); // Ctrl+A
        editor.handle_input(&format!("\x1b[200~{paste_b}\x1b[201~")); // #2 = B, text: [#2][#1]
        editor.handle_input("\x01"); // Ctrl+A
        editor.handle_input(&format!("\x1b[200~{paste_c}\x1b[201~")); // #3 = C, text: [#3][#2][#1]
        editor.handle_input("\x05"); // Ctrl+E
        editor.handle_input("\x7f"); // delete marker #1, renumber #3 -> #2 and #2 -> #1
        editor.handle_input("\r");
        assert_eq!(*submitted.lock().unwrap(), format!("{paste_c}{paste_b}"));
    }

    // Regression test for #11923
    #[test]
    fn renumbers_short_paste_markers_without_a_size_suffix() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let submitted = Arc::new(Mutex::new(String::new()));
        let submitted_ref = Arc::clone(&submitted);
        editor.set_on_submit(Some(Arc::new(move |t| {
            *submitted_ref.lock().unwrap() = t.to_string();
        })));

        let paste_a = big_paste("alpha");
        let paste_b = big_paste("beta");
        editor.handle_input(&format!("\x1b[200~{paste_a}\x1b[201~")); // #1 = A
        editor.handle_input(&format!("\x1b[200~{paste_b}\x1b[201~")); // #2 = B
        editor.handle_input("\x15"); // Ctrl+U clears the line, registry stays
        for ch in "[paste #1][paste #2]".chars() {
            let mut buf = [0u8; 4];
            editor.handle_input(ch.encode_utf8(&mut buf));
        }
        editor.handle_input("\x1b[D"); // left over marker #2
        editor.handle_input("\x7f"); // delete marker #1, renumber #2 -> #1
        assert_eq!(editor.get_text(), "[paste #1]");
        editor.handle_input("\r");
        assert_eq!(*submitted.lock().unwrap(), paste_b);
    }

    #[test]
    fn undo_after_set_text_restores_paste_markers_and_registry() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let submitted = Arc::new(Mutex::new(String::new()));
        let submitted_ref = Arc::clone(&submitted);
        editor.set_on_submit(Some(Arc::new(move |t| {
            *submitted_ref.lock().unwrap() = t.to_string();
        })));

        let paste = big_paste("alpha");
        editor.handle_input(&format!("\x1b[200~{paste}\x1b[201~"));
        editor.set_text("replacement");
        editor.handle_input("\x1b[45;5u"); // undo
        editor.handle_input("\r");
        assert_eq!(*submitted.lock().unwrap(), paste);
    }

    #[test]
    fn handles_multiple_paste_markers_in_same_line() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        paste_with_marker(&editor);
        editor.handle_input(" ");
        paste_with_marker(&editor);

        let text = editor.get_text();
        let markers: Vec<&str> = Regex::new(r"\[paste #\d+ \+\d+ lines\]")
            .unwrap()
            .find_iter(&text)
            .map(|m| m.as_str())
            .collect();
        assert_eq!(markers.len(), 2);
        let m0_len = i64::try_from(str16::len(markers[0])).unwrap();
        let m1_len = i64::try_from(str16::len(markers[1])).unwrap();

        // Go to start
        editor.handle_input("\x01");

        // Right arrow: should skip first marker atomically
        editor.handle_input("\x1b[C");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: m0_len });

        // Right arrow: past space
        editor.handle_input("\x1b[C");
        assert_eq!(
            editor.get_cursor(),
            EditorCursor {
                line: 0,
                col: m0_len + 1
            }
        );

        // Right arrow: should skip second marker atomically
        editor.handle_input("\x1b[C");
        assert_eq!(
            editor.get_cursor(),
            EditorCursor {
                line: 0,
                col: m0_len + 1 + m1_len
            }
        );
    }

    #[test]
    fn does_not_treat_manually_typed_marker_like_text_as_atomic_no_valid_paste_id() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        // Type text that matches the pattern but was typed manually (no paste entry)
        let fake_marker = "[paste #99 +5 lines]";
        for ch in fake_marker.chars() {
            let mut buf = [0u8; 4];
            editor.handle_input(ch.encode_utf8(&mut buf));
        }

        assert_eq!(editor.get_text(), fake_marker);

        // No paste with ID 99 exists, so the marker is NOT treated atomically.
        // Right arrow should move one grapheme at a time.
        editor.handle_input("\x01"); // Ctrl+A
        editor.handle_input("\x1b[C"); // Right
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 1 }); // Just past "["
    }

    #[test]
    fn does_not_crash_when_paste_marker_is_wider_than_terminal_width() {
        // Reproduce: terminal width 8, paste marker "[paste #1 +47 lines]" (21 chars)
        let tui = create_test_tui();
        let editor = Editor::new(tui, default_editor_theme(), None);
        let big_content = "line\n".repeat(47);
        let big_content = big_content.trim_end_matches('\n');
        editor.handle_input(&format!("\x1b[200~{big_content}\x1b[201~"));

        let text = editor.get_text();
        let marker = Regex::new(r"\[paste #\d+ \+\d+ lines\]").unwrap().find(&text);
        assert!(marker.is_some(), "paste marker should be created");
        assert!(
            visible_width(marker.unwrap().as_str()) > 8,
            "marker should be wider than render width"
        );

        // Render at very narrow width - should not throw
        let lines = editor.render(8);
        // Every rendered line must fit within the width (marker is split)
        for line in &lines {
            assert!(
                visible_width(line) <= 8,
                "line exceeds width 8: visible={} text={line:?}",
                visible_width(line)
            );
        }
    }

    #[test]
    fn does_not_crash_when_text_plus_paste_marker_exceeds_terminal_width_with_cursor_on_marker() {
        // Reproduce: terminal width 54, text "b".repeat(35) + "[paste #1 +27 lines]" + "bbbb"
        // Cursor lands on the paste marker after word-wrap, causing the rendered line
        // to be 55 visible chars (1 over the width).
        let tui = create_test_tui();
        let editor = Editor::new(tui, default_editor_theme(), None);

        // Type 35 'b' characters
        for _ in 0..35 {
            editor.handle_input("b");
        }

        // Paste 27 lines
        let big_content = "line\n".repeat(27);
        let big_content = big_content.trim_end_matches('\n');
        editor.handle_input(&format!("\x1b[200~{big_content}\x1b[201~"));

        // Type a few more characters
        for _ in 0..4 {
            editor.handle_input("b");
        }

        // Move cursor left to land on the paste marker
        editor.handle_input("\x1b[D"); // past last 'b'
        editor.handle_input("\x1b[D"); // past last 'b'
        editor.handle_input("\x1b[D"); // past last 'b'
        editor.handle_input("\x1b[D"); // past last 'b'
        editor.handle_input("\x1b[D"); // now on the paste marker

        // Render at width 54 - should not throw
        let render_width = 54;
        let lines = editor.render(render_width);
        for line in &lines {
            assert!(
                visible_width(line) <= render_width as i64,
                "line exceeds width {render_width}: visible={} text={line:?}",
                visible_width(line)
            );
        }
    }

    #[test]
    fn word_wrap_line_re_checks_overflow_after_backtracking_to_wrap_opportunity() {
        // Reproduce crash #2: " " + "b".repeat(35) + atomic_marker(20 chars) + "bbbb"
        // layoutWidth=53. After wrapping at the space, the remaining 35 b's + marker = 55
        // must trigger a second force-break instead of silently overflowing.
        let tui = create_test_tui();
        let editor = Editor::new(tui, default_editor_theme(), None);

        // Type a space, then 35 b's
        editor.handle_input(" ");
        for _ in 0..35 {
            editor.handle_input("b");
        }

        // Paste 27 lines to create marker
        let big_content = "line\n".repeat(27);
        let big_content = big_content.trim_end_matches('\n');
        editor.handle_input(&format!("\x1b[200~{big_content}\x1b[201~"));

        // Type trailing chars
        for _ in 0..4 {
            editor.handle_input("b");
        }

        // Render at width 54 (contentWidth=54, layoutWidth=53 with paddingX=0)
        let render_width = 54;
        let lines = editor.render(render_width);
        for line in &lines {
            assert!(
                visible_width(line) <= render_width as i64,
                "line exceeds width {render_width}: visible={} text={line:?}",
                visible_width(line)
            );
        }
    }

    #[test]
    fn expands_large_pasted_content_literally_in_get_expanded_text() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let pasted_text = [
            "line 1",
            "line 2",
            "line 3",
            "line 4",
            "line 5",
            "line 6",
            "line 7",
            "line 8",
            "line 9",
            "line 10",
            "tokens $1 $2 $& $$ $` $' end",
        ]
        .join("\n");

        editor.handle_input(&format!("\x1b[200~{pasted_text}\x1b[201~"));

        assert!(
            Regex::new(r"\[paste #\d+ \+\d+ lines\]")
                .unwrap()
                .is_match(&editor.get_text())
        );
        assert_eq!(editor.get_expanded_text(), pasted_text);
    }

    #[test]
    fn snaps_to_the_paste_marker_start_when_navigating_down_into_it() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);

        // Line 0: long enough text to establish a sticky column
        editor.set_text("12345678901234567890\n\nhello ");

        // Create a large paste to get a marker
        let big_content = "x".repeat(2000);
        editor.handle_input(&format!("\x1b[200~{big_content}\x1b[201~"));
        editor.render(80);

        let text = editor.get_text();
        let _marker = Regex::new(r"\[paste #\d+ \d+ chars\]")
            .unwrap()
            .find(&text)
            .unwrap()
            .as_str();
        // Line 0: "12345678901234567890"
        // Line 1: "" (empty)
        // Line 2: "hello [paste #1 2000 chars]"
        //         marker starts at col 6

        // Navigate to line 0, col 10
        editor.handle_input("\x1b[A"); // Up to line 1
        editor.handle_input("\x1b[A"); // Up to line 0
        editor.handle_input("\x01"); // Ctrl+A (start of line)
        for _ in 0..10 {
            editor.handle_input("\x1b[C"); // Right 10
        }
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 10 });

        // Down to empty line
        editor.handle_input("\x1b[B");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 1, col: 0 });

        // Down to paste marker line - sticky col 10 falls inside marker (starts at col 6).
        // Cursor should snap to start of marker (col 6), not end (col 6 + marker.length).
        editor.handle_input("\x1b[B");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 6 });
    }

    #[test]
    fn preserves_sticky_column_when_navigating_through_paste_marker_line() {
        let tui = create_test_tui_with_width(30);
        let editor = Editor::new(tui, default_editor_theme(), None);

        // Build:
        // Line 0: "1234567890123456" (16 chars)
        // Line 1: "" (empty)
        // Line 2: "[paste #1 2000 chars]" (22 chars, paste marker)
        // Line 3: "" (empty)
        // Line 4: "abcdefghijklmnop" (16 chars)
        for ch in "1234567890123456".chars() {
            let mut buf = [0u8; 4];
            editor.handle_input(ch.encode_utf8(&mut buf));
        }
        editor.handle_input("\n");
        editor.handle_input("\n");
        editor.handle_input(&format!("\x1b[200~{}\x1b[201~", "x".repeat(2000)));
        editor.handle_input("\n");
        editor.handle_input("\n");
        for ch in "abcdefghijklmnop".chars() {
            let mut buf = [0u8; 4];
            editor.handle_input(ch.encode_utf8(&mut buf));
        }
        editor.render(30);

        // Navigate to line 0, col 10
        for _ in 0..4 {
            editor.handle_input("\x1b[A"); // Up to line 0
        }
        editor.handle_input("\x01"); // Ctrl+A
        for _ in 0..10 {
            editor.handle_input("\x1b[C");
        }
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 10 });

        // Down to empty line - sticky col 10 established
        editor.handle_input("\x1b[B");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 1, col: 0 });

        // Down to paste marker - cursor snapped to col 0 (start of marker)
        editor.handle_input("\x1b[B");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 2, col: 0 });

        // Down to empty line
        editor.handle_input("\x1b[B");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 3, col: 0 });

        // Down to last line - should restore sticky col 10
        editor.handle_input("\x1b[B");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 4, col: 10 });
    }

    #[test]
    fn does_not_get_stuck_moving_down_from_a_multi_visual_line_paste_marker() {
        let tui = create_test_tui_with_width(20);
        let editor = Editor::new(tui, default_editor_theme(), None);

        // Build:
        // Logical line 0: "abcdefgh" + marker(21 chars) + "ijklmnopqr"
        // Logical line 1: "123456789012345678"
        //
        // Marker "[paste #1 +100 lines]" (21 chars) is wider than the
        // terminal (20). Word-wrap splits at the space before "lines",
        // producing:
        //   VL1: abcdefgh              (startCol 0,  len 8)
        //   VL2: [paste #1 +100        (startCol 8,  len 15) <- marker head
        //   VL3: lines]ijklmnopqr      (startCol 23, len 16) <- marker tail + content
        //   VL4: 123456789012345678    (line 1)
        //
        // On VL3 the marker tail "lines]" occupies visual cols 0-5.
        // Content ("i") starts at visual col 6 = logical col 29.
        for ch in "abcdefgh".chars() {
            let mut buf = [0u8; 4];
            editor.handle_input(ch.encode_utf8(&mut buf));
        }
        let big_content = "line\n".repeat(100);
        let big_content = big_content.trim_end_matches('\n');
        editor.handle_input(&format!("\x1b[200~{big_content}\x1b[201~"));
        for ch in "ijklmnopqr".chars() {
            let mut buf = [0u8; 4];
            editor.handle_input(ch.encode_utf8(&mut buf));
        }
        editor.handle_input("\n");
        for ch in "123456789012345678".chars() {
            let mut buf = [0u8; 4];
            editor.handle_input(ch.encode_utf8(&mut buf));
        }
        editor.render(20);

        let text = editor.get_text();
        let marker_match = Regex::new(r"\[paste #\d+ \+\d+ lines\]").unwrap().find(&text);
        assert!(marker_match.is_some(), "paste marker should be created");
        let marker_len = i64::try_from(str16::len(marker_match.unwrap().as_str())).unwrap(); // 21
        assert!(marker_len > 20, "marker should be wider than terminal");
        let marker_start = 8i64;
        let marker_end = marker_start + marker_len; // 29

        // Navigate to line 0, col 6 (on "g"). Preferred col 6 is past the
        // marker tail on VL3, so the cursor should land on content ("i" at
        // col 29) without snapping back.
        editor.handle_input("\x1b[A"); // Up to line 0
        editor.handle_input("\x01"); // Ctrl+A (start of line)
        for _ in 0..6 {
            editor.handle_input("\x1b[C"); // Right to col 6
        }
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 6 });

        // Down: cursor lands on paste marker start
        editor.handle_input("\x1b[B");
        assert_eq!(
            editor.get_cursor(),
            EditorCursor {
                line: 0,
                col: marker_start
            }
        );

        // Down again: preferred col 6 lands at VL3 col 29 ("i"), which is
        // past the marker. Cursor stays on line 0.
        editor.handle_input("\x1b[B");
        assert_eq!(editor.get_cursor().line, 0);
        assert_eq!(editor.get_cursor().col, marker_end); // col 29 = "i"

        // Up: back to paste marker
        editor.handle_input("\x1b[A");
        assert_eq!(
            editor.get_cursor(),
            EditorCursor {
                line: 0,
                col: marker_start
            }
        );

        // Up again: back to col 6 ("g")
        editor.handle_input("\x1b[A");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 6 });
    }

    #[test]
    fn skips_marker_continuation_vls_when_preferred_col_falls_in_marker_tail() {
        let tui = create_test_tui_with_width(20);
        let editor = Editor::new(tui, default_editor_theme(), None);

        // Same layout. Start at col 3 ("d"). Preferred col 3 maps to VL3
        // visual col 3 which is inside the "lines]" marker tail.
        // moveToVisualLine detects the continuation VL and skips to VL4
        // (line 1).
        //   VL1: abcdefgh              (startCol 0,  len 8)
        //   VL2: [paste #1 +100        (startCol 8,  len 15) <- marker head
        //   VL3: lines]ijklmnopqr      (startCol 23, len 16) <- marker tail + content
        //   VL4: 123456789012345678    (line 1)
        for ch in "abcdefgh".chars() {
            let mut buf = [0u8; 4];
            editor.handle_input(ch.encode_utf8(&mut buf));
        }
        let big_content = "line\n".repeat(100);
        let big_content = big_content.trim_end_matches('\n');
        editor.handle_input(&format!("\x1b[200~{big_content}\x1b[201~"));
        for ch in "ijklmnopqr".chars() {
            let mut buf = [0u8; 4];
            editor.handle_input(ch.encode_utf8(&mut buf));
        }
        editor.handle_input("\n");
        for ch in "123456789012345678".chars() {
            let mut buf = [0u8; 4];
            editor.handle_input(ch.encode_utf8(&mut buf));
        }
        editor.render(20);

        // Navigate to line 0, col 3 (on "d")
        editor.handle_input("\x1b[A"); // Up to line 0
        editor.handle_input("\x01"); // Ctrl+A
        for _ in 0..3 {
            editor.handle_input("\x1b[C");
        }
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 3 });

        // Down: marker
        editor.handle_input("\x1b[B");
        assert_eq!(editor.get_cursor().col, 8);

        // Down: skips VL3 (col 3 in marker tail) and lands on line 1
        editor.handle_input("\x1b[B");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 1, col: 3 });

        // Round-trip back
        editor.handle_input("\x1b[A");
        assert_eq!(editor.get_cursor().col, 8); // marker
        editor.handle_input("\x1b[A");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 3 });
    }

    #[test]
    fn submits_large_pasted_content_literally() {
        let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
        let pasted_text = [
            "line 1",
            "line 2",
            "line 3",
            "line 4",
            "line 5",
            "line 6",
            "line 7",
            "line 8",
            "line 9",
            "line 10",
            "tokens $1 $2 $& $$ $` $' end",
        ]
        .join("\n");
        let submitted = Arc::new(Mutex::new(String::new()));
        let submitted_ref = Arc::clone(&submitted);
        editor.set_on_submit(Some(Arc::new(move |text| {
            *submitted_ref.lock().unwrap() = text.to_string();
        })));

        editor.handle_input(&format!("\x1b[200~{pasted_text}\x1b[201~"));
        editor.handle_input("\r");

        assert_eq!(*submitted.lock().unwrap(), pasted_text);
    }
}
