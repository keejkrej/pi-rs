//! Port of packages/tui/test/input.test.ts

use std::sync::{Arc, Mutex};

use pi_tui::components::input::InputOptions;
use pi_tui::{Input, strip_terminal_sequences, visible_width};

#[test]
fn submits_value_including_backslash_on_enter() {
    let input = Input::new(None);
    let submitted: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let submitted_clone = Arc::clone(&submitted);

    input.set_on_submit(Some(Arc::new(move |value| {
        *submitted_clone.lock().unwrap() = Some(value.to_string());
    })));

    // Type hello, then backslash, then Enter
    input.handle_input("h");
    input.handle_input("e");
    input.handle_input("l");
    input.handle_input("l");
    input.handle_input("o");
    input.handle_input("\\");
    input.handle_input("\r");

    // Input is single-line, no backslash+Enter workaround
    assert_eq!(submitted.lock().unwrap().as_deref(), Some("hello\\"));
}

#[test]
fn inserts_backslash_as_regular_character() {
    let input = Input::new(None);

    input.handle_input("\\");
    input.handle_input("x");

    assert_eq!(input.get_value(), "\\x");
}

#[test]
fn render_supports_a_custom_prompt_and_styled_placeholder() {
    let input = Input::new(Some(InputOptions {
        prompt: Some(String::new()),
        placeholder: Some("Find transcript".to_string()),
        placeholder_style: Some(Arc::new(|text| format!("\x1b[2m{text}\x1b[22m"))),
    }));
    input.set_focused(true);

    let empty_lines = input.render(20);
    let empty = empty_lines.first().map(String::as_str).unwrap_or("");
    assert!(empty.contains("\x1b[2m"));
    assert_eq!(strip_terminal_sequences(empty).trim_end(), "Find transcript");

    input.handle_input("n");
    let populated_lines = input.render(20);
    let populated = populated_lines.first().map(String::as_str).unwrap_or("");
    assert_eq!(strip_terminal_sequences(populated).trim_end(), "n");
}

#[test]
fn render_does_not_overflow_with_wide_cjk_and_fullwidth_text() {
    let width = 93;
    let cases = [
        "가나다라마바사아자차카타파하 한글 텍스트가 터미널 너비를 초과하면 크래시가 발생합니다 이것은 재현용 테스트입니다",
        "これはテスト文章です。日本語のテキストが正しく表示されるかどうかを確認するためのサンプルテキストです。あいうえお",
        "这是一段测试文本，用于验证中文字符在终端中的显示宽度是否被正确计算，如果不正确就会导致用户界面崩溃的问题",
        "ＡＢＣＤＥＦＧＨＩＪＫＬＭＮＯＰＱＲＳＴＵＶＷＸＹＺ０１２３４５６７８９ａｂｃｄｅｆｇｈｉｊｋｌｍ",
    ];
    let move_middle = |input: &Input| {
        for _ in 0..10 {
            input.handle_input("\x1b[C");
        }
    };
    let move_end = |input: &Input| {
        input.handle_input("\x05");
    };

    for text in cases {
        for (label, move_fn) in [
            ("start", (|_: &Input| {}) as fn(&Input)),
            ("middle", move_middle as fn(&Input)),
            ("end", move_end as fn(&Input)),
        ] {
            let input = Input::new(None);
            input.set_value(text);
            input.set_focused(true);
            move_fn(&input);

            let lines = input.render(width);
            let line = lines.first().expect("rendered line should exist");
            assert!(
                visible_width(line) <= width as i64,
                "rendered line overflowed for {text} at {label}"
            );
        }
    }
}

#[test]
fn render_keeps_the_cursor_visible_when_horizontally_scrolling_wide_text() {
    let input = Input::new(None);
    let width = 20;
    let text = "가나다라마바사아자차카타파하";
    input.set_value(text);
    input.set_focused(true);
    input.handle_input("\x01");
    for _ in 0..5 {
        input.handle_input("\x1b[C");
    }

    let lines = input.render(width);
    let line = lines.first().expect("rendered line should exist");
    assert!(visible_width(line) <= width as i64);
}

#[test]
fn kill_ring_ctrl_w_saves_deleted_text_to_kill_ring_and_ctrl_y_yanks_it() {
    let input = Input::new(None);

    input.set_value("foo bar baz");
    input.handle_input("\x05"); // Ctrl+E

    input.handle_input("\x17"); // Ctrl+W - deletes "baz"
    assert_eq!(input.get_value(), "foo bar ");

    // Move to beginning and yank
    input.handle_input("\x01"); // Ctrl+A
    input.handle_input("\x19"); // Ctrl+Y
    assert_eq!(input.get_value(), "bazfoo bar ");
}

#[test]
fn kill_ring_ctrl_w_preserves_ascii_punctuation_boundaries() {
    let input = Input::new(None);

    input.set_value("foo.bar");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "bar"
    assert_eq!(input.get_value(), "foo.");

    input.set_value("foo:bar");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "bar"
    assert_eq!(input.get_value(), "foo:");
}

#[test]
fn kill_ring_ctrl_w_handles_unicode_word_boundaries() {
    let input = Input::new(None);

    // "你好世界。你好，世界" segments as: 你好|世界|。|你好|，|世界
    input.set_value("你好世界。你好，世界");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "世界"
    assert_eq!(input.get_value(), "你好世界。你好，");
    input.handle_input("\x17"); // Ctrl+W - deletes "，"
    assert_eq!(input.get_value(), "你好世界。你好");
    input.handle_input("\x17"); // Ctrl+W - deletes "你好"
    assert_eq!(input.get_value(), "你好世界。");
    input.handle_input("\x17"); // Ctrl+W - deletes "。"
    assert_eq!(input.get_value(), "你好世界");
    input.handle_input("\x17"); // Ctrl+W - deletes "世界"
    assert_eq!(input.get_value(), "你好");
    input.handle_input("\x17"); // Ctrl+W - deletes "你好"
    assert_eq!(input.get_value(), "");
}

#[test]
fn kill_ring_ctrl_u_saves_deleted_text_to_kill_ring() {
    let input = Input::new(None);

    input.set_value("hello world");
    // Move cursor to after "hello "
    input.handle_input("\x01"); // Ctrl+A
    for _ in 0..6 {
        input.handle_input("\x1b[C");
    }

    input.handle_input("\x15"); // Ctrl+U - deletes "hello "
    assert_eq!(input.get_value(), "world");

    input.handle_input("\x19"); // Ctrl+Y
    assert_eq!(input.get_value(), "hello world");
}

#[test]
fn kill_ring_ctrl_k_saves_deleted_text_to_kill_ring() {
    let input = Input::new(None);

    input.set_value("hello world");
    input.handle_input("\x01"); // Ctrl+A
    input.handle_input("\x0b"); // Ctrl+K - deletes "hello world"

    assert_eq!(input.get_value(), "");

    input.handle_input("\x19"); // Ctrl+Y
    assert_eq!(input.get_value(), "hello world");
}

#[test]
fn kill_ring_ctrl_y_does_nothing_when_kill_ring_is_empty() {
    let input = Input::new(None);

    input.set_value("test");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x19"); // Ctrl+Y
    assert_eq!(input.get_value(), "test");
}

#[test]
fn kill_ring_alt_y_cycles_through_kill_ring_after_ctrl_y() {
    let input = Input::new(None);

    input.set_value("first");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "first"
    input.set_value("second");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "second"
    input.set_value("third");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "third"

    assert_eq!(input.get_value(), "");

    input.handle_input("\x19"); // Ctrl+Y - yanks "third"
    assert_eq!(input.get_value(), "third");

    input.handle_input("\x1by"); // Alt+Y - cycles to "second"
    assert_eq!(input.get_value(), "second");

    input.handle_input("\x1by"); // Alt+Y - cycles to "first"
    assert_eq!(input.get_value(), "first");

    input.handle_input("\x1by"); // Alt+Y - cycles back to "third"
    assert_eq!(input.get_value(), "third");
}

#[test]
fn kill_ring_alt_y_does_nothing_if_not_preceded_by_yank() {
    let input = Input::new(None);

    input.set_value("test");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "test"
    input.set_value("other");
    input.handle_input("\x05"); // Ctrl+E

    // Type something to break the yank chain
    input.handle_input("x");
    assert_eq!(input.get_value(), "otherx");

    input.handle_input("\x1by"); // Alt+Y - should do nothing
    assert_eq!(input.get_value(), "otherx");
}

#[test]
fn kill_ring_alt_y_does_nothing_if_kill_ring_has_one_entry() {
    let input = Input::new(None);

    input.set_value("only");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "only"

    input.handle_input("\x19"); // Ctrl+Y - yanks "only"
    assert_eq!(input.get_value(), "only");

    input.handle_input("\x1by"); // Alt+Y - should do nothing
    assert_eq!(input.get_value(), "only");
}

#[test]
fn kill_ring_consecutive_ctrl_w_accumulates_into_one_kill_ring_entry() {
    let input = Input::new(None);

    input.set_value("one two three");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "three"
    input.handle_input("\x17"); // Ctrl+W - deletes "two "
    input.handle_input("\x17"); // Ctrl+W - deletes "one "

    assert_eq!(input.get_value(), "");

    input.handle_input("\x19"); // Ctrl+Y
    assert_eq!(input.get_value(), "one two three");
}

#[test]
fn kill_ring_non_delete_actions_break_kill_accumulation() {
    let input = Input::new(None);

    input.set_value("foo bar baz");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "baz"
    assert_eq!(input.get_value(), "foo bar ");

    input.handle_input("x"); // Typing breaks accumulation
    assert_eq!(input.get_value(), "foo bar x");

    input.handle_input("\x17"); // Ctrl+W - deletes "x" (separate entry)
    assert_eq!(input.get_value(), "foo bar ");

    input.handle_input("\x19"); // Ctrl+Y - most recent is "x"
    assert_eq!(input.get_value(), "foo bar x");

    input.handle_input("\x1by"); // Alt+Y - cycle to "baz"
    assert_eq!(input.get_value(), "foo bar baz");
}

#[test]
fn kill_ring_non_yank_actions_break_alt_y_chain() {
    let input = Input::new(None);

    input.set_value("first");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W
    input.set_value("second");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W
    input.set_value("");

    input.handle_input("\x19"); // Ctrl+Y - yanks "second"
    assert_eq!(input.get_value(), "second");

    input.handle_input("x"); // Breaks yank chain
    assert_eq!(input.get_value(), "secondx");

    input.handle_input("\x1by"); // Alt+Y - should do nothing
    assert_eq!(input.get_value(), "secondx");
}

#[test]
fn kill_ring_rotation_persists_after_cycling() {
    let input = Input::new(None);

    input.set_value("first");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // deletes "first"
    input.set_value("second");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // deletes "second"
    input.set_value("third");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // deletes "third"
    input.set_value("");

    input.handle_input("\x19"); // Ctrl+Y - yanks "third"
    input.handle_input("\x1by"); // Alt+Y - cycles to "second"
    assert_eq!(input.get_value(), "second");

    // Break chain and start fresh
    input.handle_input("x");
    input.set_value("");

    // New yank should get "second" (now at end after rotation)
    input.handle_input("\x19"); // Ctrl+Y
    assert_eq!(input.get_value(), "second");
}

#[test]
fn kill_ring_backward_deletions_prepend_forward_deletions_append_during_accumulation() {
    let input = Input::new(None);

    input.set_value("prefix|suffix");
    // Position cursor at "|"
    input.handle_input("\x01"); // Ctrl+A
    for _ in 0..6 {
        input.handle_input("\x1b[C"); // Move right 6
    }

    input.handle_input("\x0b"); // Ctrl+K - deletes "|suffix" (forward)
    assert_eq!(input.get_value(), "prefix");

    input.handle_input("\x19"); // Ctrl+Y
    assert_eq!(input.get_value(), "prefix|suffix");
}

#[test]
fn kill_ring_alt_d_deletes_word_forward_and_saves_to_kill_ring() {
    let input = Input::new(None);

    input.set_value("hello world test");
    input.handle_input("\x01"); // Ctrl+A

    input.handle_input("\x1bd"); // Alt+D - deletes "hello"
    assert_eq!(input.get_value(), " world test");

    input.handle_input("\x1bd"); // Alt+D - deletes " world"
    assert_eq!(input.get_value(), " test");

    // Yank should get accumulated text
    input.handle_input("\x19"); // Ctrl+Y
    assert_eq!(input.get_value(), "hello world test");
}

#[test]
fn kill_ring_alt_d_preserves_ascii_punctuation_boundaries() {
    let input = Input::new(None);

    input.set_value("foo.bar baz");
    input.handle_input("\x01"); // Ctrl+A
    input.handle_input("\x1bd"); // Alt+D - deletes "foo"
    assert_eq!(input.get_value(), ".bar baz");
    input.handle_input("\x1bd"); // Alt+D - deletes "."
    assert_eq!(input.get_value(), "bar baz");
    input.handle_input("\x1bd"); // Alt+D - deletes "bar"
    assert_eq!(input.get_value(), " baz");
}

#[test]
fn kill_ring_alt_d_handles_unicode_word_boundaries() {
    let input = Input::new(None);

    // "你好世界。你好，世界" segments as: 你好|世界|。|你好|，|世界
    input.set_value("你好世界。你好，世界");
    input.handle_input("\x01"); // Ctrl+A
    input.handle_input("\x1bd"); // Alt+D - deletes "你好"
    assert_eq!(input.get_value(), "世界。你好，世界");
    input.handle_input("\x1bd"); // Alt+D - deletes "世界"
    assert_eq!(input.get_value(), "。你好，世界");
    input.handle_input("\x1bd"); // Alt+D - deletes "。"
    assert_eq!(input.get_value(), "你好，世界");
    input.handle_input("\x1bd"); // Alt+D - deletes "你好"
    assert_eq!(input.get_value(), "，世界");
    input.handle_input("\x1bd"); // Alt+D - deletes "，"
    assert_eq!(input.get_value(), "世界");
    input.handle_input("\x1bd"); // Alt+D - deletes "世界"
    assert_eq!(input.get_value(), "");
}

#[test]
fn kill_ring_handles_yank_in_middle_of_text() {
    let input = Input::new(None);

    input.set_value("word");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "word"
    input.set_value("hello world");
    // Move to middle (after "hello ")
    input.handle_input("\x01"); // Ctrl+A
    for _ in 0..6 {
        input.handle_input("\x1b[C");
    }

    input.handle_input("\x19"); // Ctrl+Y
    assert_eq!(input.get_value(), "hello wordworld");
}

#[test]
fn kill_ring_handles_yank_pop_in_middle_of_text() {
    let input = Input::new(None);

    // Create two kill ring entries
    input.set_value("FIRST");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "FIRST"
    input.set_value("SECOND");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "SECOND"

    // Set up "hello world" and position cursor after "hello "
    input.set_value("hello world");
    input.handle_input("\x01"); // Ctrl+A
    for _ in 0..6 {
        input.handle_input("\x1b[C");
    }

    input.handle_input("\x19"); // Ctrl+Y - yanks "SECOND"
    assert_eq!(input.get_value(), "hello SECONDworld");

    input.handle_input("\x1by"); // Alt+Y - replaces with "FIRST"
    assert_eq!(input.get_value(), "hello FIRSTworld");
}

#[test]
fn undo_does_nothing_when_undo_stack_is_empty() {
    let input = Input::new(None);

    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "");
}

#[test]
fn undo_coalesces_consecutive_word_characters_into_one_undo_unit() {
    let input = Input::new(None);

    for ch in ["h", "e", "l", "l", "o", " ", "w", "o", "r", "l", "d"] {
        input.handle_input(ch);
    }
    assert_eq!(input.get_value(), "hello world");

    // Undo removes " world"
    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "hello");

    // Undo removes "hello"
    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "");
}

#[test]
fn undo_undoes_spaces_one_at_a_time() {
    let input = Input::new(None);

    for ch in ["h", "e", "l", "l", "o", " ", " "] {
        input.handle_input(ch);
    }
    assert_eq!(input.get_value(), "hello  ");

    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo) - removes second " "
    assert_eq!(input.get_value(), "hello ");

    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo) - removes first " "
    assert_eq!(input.get_value(), "hello");

    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo) - removes "hello"
    assert_eq!(input.get_value(), "");
}

#[test]
fn undo_undoes_backspace() {
    let input = Input::new(None);

    for ch in ["h", "e", "l", "l", "o"] {
        input.handle_input(ch);
    }
    input.handle_input("\x7f"); // Backspace
    assert_eq!(input.get_value(), "hell");

    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "hello");
}

#[test]
fn undo_undoes_forward_delete() {
    let input = Input::new(None);

    for ch in ["h", "e", "l", "l", "o"] {
        input.handle_input(ch);
    }
    input.handle_input("\x01"); // Ctrl+A - go to start
    input.handle_input("\x1b[C"); // Right arrow
    input.handle_input("\x1b[3~"); // Delete key
    assert_eq!(input.get_value(), "hllo");

    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "hello");
}

#[test]
fn undo_undoes_ctrl_w_delete_word_backward() {
    let input = Input::new(None);

    for ch in ["h", "e", "l", "l", "o", " ", "w", "o", "r", "l", "d"] {
        input.handle_input(ch);
    }
    assert_eq!(input.get_value(), "hello world");

    input.handle_input("\x17"); // Ctrl+W
    assert_eq!(input.get_value(), "hello ");

    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "hello world");
}

#[test]
fn undo_undoes_ctrl_k_delete_to_line_end() {
    let input = Input::new(None);

    for ch in ["h", "e", "l", "l", "o", " ", "w", "o", "r", "l", "d"] {
        input.handle_input(ch);
    }
    input.handle_input("\x01"); // Ctrl+A
    for _ in 0..6 {
        input.handle_input("\x1b[C");
    }

    input.handle_input("\x0b"); // Ctrl+K
    assert_eq!(input.get_value(), "hello ");

    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "hello world");
}

#[test]
fn undo_undoes_ctrl_u_delete_to_line_start() {
    let input = Input::new(None);

    for ch in ["h", "e", "l", "l", "o", " ", "w", "o", "r", "l", "d"] {
        input.handle_input(ch);
    }
    input.handle_input("\x01"); // Ctrl+A
    for _ in 0..6 {
        input.handle_input("\x1b[C");
    }

    input.handle_input("\x15"); // Ctrl+U
    assert_eq!(input.get_value(), "world");

    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "hello world");
}

#[test]
fn undo_undoes_yank() {
    let input = Input::new(None);

    for ch in ["h", "e", "l", "l", "o", " "] {
        input.handle_input(ch);
    }
    input.handle_input("\x17"); // Ctrl+W - delete "hello "
    input.handle_input("\x19"); // Ctrl+Y - yank
    assert_eq!(input.get_value(), "hello ");

    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "");
}

#[test]
fn undo_undoes_paste_atomically() {
    let input = Input::new(None);

    input.set_value("hello world");
    input.handle_input("\x01"); // Ctrl+A
    for _ in 0..5 {
        input.handle_input("\x1b[C");
    }

    // Simulate bracketed paste
    input.handle_input("\x1b[200~beep boop\x1b[201~");
    assert_eq!(input.get_value(), "hellobeep boop world");

    // Single undo should restore entire pre-paste state
    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "hello world");
}

#[test]
fn undo_undoes_alt_d_delete_word_forward() {
    let input = Input::new(None);

    input.set_value("hello world");
    input.handle_input("\x01"); // Ctrl+A

    input.handle_input("\x1bd"); // Alt+D - deletes "hello"
    assert_eq!(input.get_value(), " world");

    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "hello world");
}

#[test]
fn undo_cursor_movement_starts_new_undo_unit() {
    let input = Input::new(None);

    input.handle_input("a");
    input.handle_input("b");
    input.handle_input("c");
    input.handle_input("\x01"); // Ctrl+A - movement breaks coalescing
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("d");
    input.handle_input("e");
    assert_eq!(input.get_value(), "abcde");

    // Undo removes "de" (typed after movement)
    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "abc");

    // Undo removes "abc"
    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "");
}
