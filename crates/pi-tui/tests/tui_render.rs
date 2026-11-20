//! Port of packages/tui/test/tui-render.test.ts

mod support;

use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_js::env::testing::EnvGuard;
use pi_tui::components::image::{Image, ImageOptions, ImageTheme};
use pi_tui::components::input::Input;
use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
use pi_tui::terminal_image::{
    CellDimensions, EncodeKittyOptions, ImageDimensions, ImageProtocol, TerminalCapabilities, delete_kitty_image,
    encode_kitty, reset_capabilities_cache, set_capabilities, set_cell_dimensions,
};
use pi_tui::tui::{CURSOR_MARKER, Component, OverlayOptions, SizeValue, render_fake_cursor};
use pi_tui::tui_main_screen::TuiMainScreen;
use pi_tui::utils::slice_by_column;
use serial_test::serial;

use support::virtual_terminal::VirtualTerminal;

const MAX_RENDER_WRITE_CHARS: usize = 1024 * 1024;

struct TestComponent {
    lines: Mutex<Vec<String>>,
}

impl TestComponent {
    fn new(lines: Vec<String>) -> Self {
        Self {
            lines: Mutex::new(lines),
        }
    }

    fn set_lines(&self, lines: Vec<String>) {
        *self.lines.lock().unwrap() = lines;
    }
}

impl Component for TestComponent {
    fn render(&self, _width: usize) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }

    fn invalidate(&self) {}
}

struct InputComponent {
    lines: Mutex<Vec<String>>,
    render_count: Mutex<usize>,
}

impl InputComponent {
    fn new(lines: Vec<String>) -> Self {
        Self {
            lines: Mutex::new(lines),
            render_count: Mutex::new(0),
        }
    }

    fn set_lines(&self, lines: Vec<String>) {
        *self.lines.lock().unwrap() = lines;
    }

    fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }

    fn render_count(&self) -> usize {
        *self.render_count.lock().unwrap()
    }
}

impl Component for InputComponent {
    fn render(&self, _width: usize) -> Vec<String> {
        *self.render_count.lock().unwrap() += 1;
        self.lines.lock().unwrap().clone()
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn handle_input(&self, data: &str) {
        *self.lines.lock().unwrap() = vec![data.to_string()];
    }

    fn invalidate(&self) {}
}

struct BoundedWriteTerminal {
    writes: Mutex<Vec<String>>,
    rows: AtomicI64,
}

impl BoundedWriteTerminal {
    fn new() -> Self {
        Self {
            writes: Mutex::new(Vec::new()),
            rows: AtomicI64::new(24),
        }
    }

    fn set_rows(&self, rows: i64) {
        self.rows.store(rows, Ordering::SeqCst);
    }

    fn writes(&self) -> Vec<String> {
        self.writes.lock().unwrap().clone()
    }

    fn clear_writes(&self) {
        self.writes.lock().unwrap().clear();
    }
}

#[async_trait]
impl Terminal for BoundedWriteTerminal {
    fn start(&self, _on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {}

    fn stop(&self) {}

    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}

    fn write(&self, data: &str) {
        self.writes.lock().unwrap().push(data.to_string());
    }

    fn columns(&self) -> i64 {
        80
    }

    fn rows(&self) -> i64 {
        self.rows.load(Ordering::SeqCst)
    }

    fn kitty_protocol_active(&self) -> bool {
        false
    }

    fn move_by(&self, _lines: i64) {}

    fn hide_cursor(&self) {}

    fn show_cursor(&self) {}

    fn clear_line(&self) {}

    fn clear_from_cursor(&self) {}

    fn clear_screen(&self) {}

    fn set_title(&self, _title: &str) {}

    fn set_progress(&self, _active: bool) {}
}

struct LoggingVirtualTerminal {
    inner: VirtualTerminal,
    writes: Mutex<Vec<String>>,
}

impl LoggingVirtualTerminal {
    fn new(columns: i64, rows: i64) -> Self {
        Self {
            inner: VirtualTerminal::new(columns, rows),
            writes: Mutex::new(Vec::new()),
        }
    }

    fn get_writes(&self) -> String {
        self.writes.lock().unwrap().concat()
    }

    fn clear_writes(&self) {
        self.writes.lock().unwrap().clear();
    }

    async fn wait_for_render(&self) {
        self.inner.wait_for_render().await;
    }

    fn get_viewport(&self) -> Vec<String> {
        self.inner.get_viewport()
    }

    fn resize(&self, columns: i64, rows: i64) {
        self.inner.resize(columns, rows);
    }
}

#[async_trait]
impl Terminal for LoggingVirtualTerminal {
    fn start(&self, on_input: TerminalInputHandler, on_resize: TerminalResizeHandler) {
        self.inner.start(on_input, on_resize);
    }

    fn stop(&self) {
        self.inner.stop();
    }

    async fn drain_input(&self, max_ms: Option<i64>, idle_ms: Option<i64>) {
        self.inner.drain_input(max_ms, idle_ms).await;
    }

    fn write(&self, data: &str) {
        self.writes.lock().unwrap().push(data.to_string());
        self.inner.write(data);
    }

    fn columns(&self) -> i64 {
        self.inner.columns()
    }

    fn rows(&self) -> i64 {
        self.inner.rows()
    }

    fn kitty_protocol_active(&self) -> bool {
        self.inner.kitty_protocol_active()
    }

    fn move_by(&self, lines: i64) {
        self.inner.move_by(lines);
    }

    fn hide_cursor(&self) {
        self.inner.hide_cursor();
    }

    fn show_cursor(&self) {
        self.inner.show_cursor();
    }

    fn clear_line(&self) {
        self.inner.clear_line();
    }

    fn clear_from_cursor(&self) {
        self.inner.clear_from_cursor();
    }

    fn clear_screen(&self) {
        self.inner.clear_screen();
    }

    fn set_title(&self, title: &str) {
        self.inner.set_title(title);
    }

    fn set_progress(&self, active: bool) {
        self.inner.set_progress(active);
    }
}

struct KittyCapsGuard;

impl KittyCapsGuard {
    fn new(width_px: i64, height_px: i64) -> Self {
        set_capabilities(TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        });
        set_cell_dimensions(CellDimensions { width_px, height_px });
        Self
    }
}

impl Drop for KittyCapsGuard {
    fn drop(&mut self) {
        reset_capabilities_cache();
        set_cell_dimensions(CellDimensions {
            width_px: 9,
            height_px: 18,
        });
    }
}

fn identity_image_theme() -> ImageTheme {
    ImageTheme {
        fallback_color: Arc::new(|value: &str| value.to_string()),
    }
}

fn panic_payload_to_string(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        String::new()
    }
}

mod tui_render_scheduling {
    use super::*;

    #[tokio::test]
    async fn renders_keyboard_input_without_waiting_for_a_throttled_frame() {
        let terminal = Arc::new(VirtualTerminal::new(40, 10));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let component = Arc::new(InputComponent::new(vec!["initial".to_string()]));
        tui.add_child(component.clone());
        tui.set_focus(Some(component.clone()));
        tui.start();
        tui.render_now(None);
        let render_count_before_input = component.render_count();

        component.set_lines(vec!["pending".to_string()]);
        tui.request_render(None);
        terminal.send_input("first");
        terminal.send_input("second");
        terminal.send_input("typed");
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        tokio::task::yield_now().await;

        assert_eq!(component.render_count(), render_count_before_input + 1);
        assert_eq!(component.lines(), vec!["typed".to_string()]);
        tui.stop(None);
    }
}

mod tui_debug_logging {
    use super::*;

    #[tokio::test]
    #[serial]
    async fn writes_redraw_logs_to_the_provided_directory() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let log_dir = temp_dir.path().to_str().expect("utf-8 temp dir");
        let _env = EnvGuard::set("PI_TUI_DEBUG_REDRAW", Some("1"));
        let terminal = Arc::new(VirtualTerminal::new(40, 10));
        let tui = TuiMainScreen::new(terminal.clone(), None, Some(log_dir));
        let component = Arc::new(TestComponent::new(vec!["test".to_string()]));
        tui.add_child(component);
        tui.start();
        terminal.wait_for_render().await;

        let log_content = fs::read_to_string(temp_dir.path().join("pi-tui-debug.log")).expect("debug log");
        assert!(log_content.contains("fullRender: first render"));
        tui.stop(None);
    }
}

mod tui_bounded_render_output {
    use super::*;

    #[test]
    fn does_not_write_duplicate_cursor_markers_to_the_terminal() {
        let terminal = Arc::new(BoundedWriteTerminal::new());
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        // Regression for #9257: terminals may render the payload of a leaked APC marker.
        let component = Arc::new(TestComponent::new(vec![format!(
            "\x1b[31m界🙂\x1b[0m{CURSOR_MARKER} world{CURSOR_MARKER}"
        )]));
        tui.add_child(component);

        tui.render_now(None);

        let output = terminal.writes().concat();
        assert!(
            output.contains("\x1b[31m界🙂\x1b[0m world"),
            "visible styled text should be preserved"
        );
        assert!(
            !output.contains(CURSOR_MARKER),
            "internal cursor markers must not reach the terminal"
        );
        assert!(
            output.contains("\x1b[5G"),
            "the first marker should keep the cursor at visible column 4"
        );
    }

    #[test]
    fn does_not_write_cursor_markers_from_other_visible_or_scrollback_lines() {
        let terminal = Arc::new(BoundedWriteTerminal::new());
        terminal.set_rows(2);
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        // Regression for #9257: cursor extraction must not leave markers on other rendered lines.
        let component = Arc::new(TestComponent::new(vec![
            format!("offscreen{CURSOR_MARKER}"),
            format!("a{CURSOR_MARKER}"),
            format!("visible-b{CURSOR_MARKER}"),
        ]));
        tui.add_child(component);

        tui.render_now(None);

        let output = terminal.writes().concat();
        assert!(
            output.contains("offscreen"),
            "full renders should preserve scrollback content"
        );
        assert!(output.contains("a"), "full renders should preserve other visible lines");
        assert!(
            !output.contains(CURSOR_MARKER),
            "internal cursor markers must not reach the terminal"
        );
        assert!(
            output.contains("\x1b[10G"),
            "the bottom visible marker should determine the cursor column"
        );
    }

    #[test]
    fn splits_a_large_full_render_without_changing_its_output() {
        let terminal = Arc::new(BoundedWriteTerminal::new());
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let kitty_line = format!("\x1b_Ga=T,f=100;{}\x1b\\", "A".repeat(1_200_000));
        let component = Arc::new(TestComponent::new(vec![kitty_line.clone(), kitty_line.clone()]));
        tui.add_child(component);

        tui.render_now(None);

        let writes = terminal.writes();
        assert!(writes.len() > 2, "large output should be split across terminal writes");
        assert!(
            writes.iter().all(|w| w.chars().count() <= MAX_RENDER_WRITE_CHARS),
            "each terminal write should stay below the configured limit"
        );
        assert_eq!(
            writes.concat(),
            format!("\x1b[?2026h{kitty_line}\r\n{kitty_line}\x1b[?2026l"),
            "chunking must preserve the synchronized render output"
        );
    }

    #[test]
    fn splits_large_differential_updates_without_a_full_redraw() {
        let terminal = Arc::new(BoundedWriteTerminal::new());
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let component = Arc::new(TestComponent::new(vec!["before".to_string()]));
        tui.add_child(component.clone());
        tui.render_now(None);
        terminal.clear_writes();

        let kitty_line = format!("\x1b_Ga=T,f=100;{}\x1b\\", "A".repeat(1_200_000));
        component.set_lines(vec!["before".to_string(), kitty_line.clone(), kitty_line]);
        tui.render_now(None);

        let writes = terminal.writes();
        assert!(writes.len() > 2, "large output should be split across terminal writes");
        assert!(writes.iter().all(|w| w.chars().count() <= MAX_RENDER_WRITE_CHARS));
        let output = writes.concat();
        assert!(output.starts_with("\x1b[?2026h"));
        assert!(output.ends_with("\x1b[?2026l"));
        assert!(
            !output.contains("\x1b[2J"),
            "the update should stay on the differential render path"
        );
    }
}

mod tui_crash_dump_without_configured_log_directory {
    use super::*;

    #[tokio::test]
    #[serial]
    async fn writes_the_crash_dump_to_the_os_temp_directory_instead_of_a_home_directory_default() {
        let crash_dir = tempfile::tempdir().expect("tempdir");
        let crash_dir_str = crash_dir.path().to_str().expect("utf-8 temp dir");
        let crash_log_path = crash_dir.path().join("pi-tui-crash.log");
        let _env = EnvGuard::set_many(&[
            ("TMPDIR", Some(crash_dir_str)),
            ("TEMP", Some(crash_dir_str)),
            ("TMP", Some(crash_dir_str)),
        ]);

        let terminal = Arc::new(VirtualTerminal::new(40, 10));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let component = Arc::new(TestComponent::new(vec!["ok".to_string()]));
        tui.add_child(component.clone());
        tui.start();
        terminal.wait_for_render().await;

        component.set_lines(vec!["ok".to_string(), "x".repeat(60)]);
        let panic_res = catch_unwind(AssertUnwindSafe(|| {
            tui.render_now(None);
        }));
        let err = panic_res.expect_err("width overflow should panic");
        let msg = panic_payload_to_string(err);
        let expected_path = crash_log_path.to_str().expect("utf-8 crash log path");
        assert!(
            msg.contains(expected_path),
            "error message should reference {expected_path}, got: {msg}"
        );

        let log_content = fs::read_to_string(&crash_log_path).expect("crash log file");
        assert!(log_content.contains("Terminal width: 40"));
    }
}

mod tui_kitty_image_cleanup {
    use super::*;

    #[tokio::test]
    #[serial]
    async fn clears_reserved_kitty_image_rows_before_drawing_appended_image_placements() {
        let _caps = KittyCapsGuard::new(10, 10);
        let terminal = Arc::new(LoggingVirtualTerminal::new(40, 10));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let component = Arc::new(TestComponent::new(vec!["before".to_string()]));
        tui.add_child(component.clone());

        tui.start();
        terminal.wait_for_render().await;
        terminal.clear_writes();

        let image = Image::new(
            "AAAA",
            "image/png",
            identity_image_theme(),
            Some(ImageOptions {
                max_width_cells: Some(2),
                ..Default::default()
            }),
            Some(ImageDimensions {
                width_px: 20,
                height_px: 20,
            }),
        );
        let image_lines = image.render(40);
        let image_sequence = image_lines[0].clone();
        let mut next_lines = vec!["before".to_string()];
        next_lines.extend(image_lines);
        next_lines.push("after".to_string());
        component.set_lines(next_lines);
        tui.request_render(None);
        terminal.wait_for_render().await;

        let writes = terminal.get_writes();
        assert!(
            writes.contains(&format!("\x1b[2K\r\n\x1b[2K\x1b[1A{image_sequence}\x1b[1B")),
            "reserved rows should be cleared before the image placement is drawn"
        );
        assert!(
            !writes.contains(&format!("{image_sequence}\r\n\x1b[2K")),
            "reserved row clears must not run after the image placement is drawn"
        );

        tui.stop(None);
    }

    #[tokio::test]
    #[serial]
    async fn falls_back_to_full_redraw_when_kitty_image_pre_clear_would_scroll() {
        let _caps = KittyCapsGuard::new(10, 10);
        let terminal = Arc::new(LoggingVirtualTerminal::new(40, 2));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let component = Arc::new(TestComponent::new(vec!["before".to_string()]));
        tui.add_child(component.clone());

        tui.start();
        terminal.wait_for_render().await;
        let redraws_before_image = tui.full_redraws();
        terminal.clear_writes();

        let image = Image::new(
            "AAAA",
            "image/png",
            identity_image_theme(),
            Some(ImageOptions {
                max_width_cells: Some(3),
                ..Default::default()
            }),
            Some(ImageDimensions {
                width_px: 30,
                height_px: 30,
            }),
        );
        let mut next_lines = vec!["before".to_string()];
        next_lines.extend(image.render(40));
        next_lines.push("after".to_string());
        component.set_lines(next_lines);
        tui.request_render(None);
        terminal.wait_for_render().await;

        assert!(
            tui.full_redraws() > redraws_before_image,
            "unsafe image pre-clear should force a full redraw"
        );
        assert!(
            terminal.get_writes().contains("\x1b[2J"),
            "fallback should clear and fully redraw"
        );

        tui.stop(None);
    }

    #[tokio::test]
    #[serial]
    async fn reserves_kitty_image_rows_before_drawing_during_full_redraw_fallbacks() {
        let _caps = KittyCapsGuard::new(10, 10);
        let terminal = Arc::new(LoggingVirtualTerminal::new(40, 5));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let component = Arc::new(TestComponent::new(vec![
            "l0".into(),
            "l1".into(),
            "l2".into(),
            "l3".into(),
            "l4".into(),
        ]));
        tui.add_child(component.clone());

        tui.start();
        terminal.wait_for_render().await;
        let redraws_before_image = tui.full_redraws();
        terminal.clear_writes();

        let image = Image::new(
            "AAAA",
            "image/png",
            identity_image_theme(),
            Some(ImageOptions {
                max_width_cells: Some(3),
                ..Default::default()
            }),
            Some(ImageDimensions {
                width_px: 30,
                height_px: 30,
            }),
        );
        let image_lines = image.render(40);
        let image_sequence = image_lines[0].clone();
        let mut next_lines: Vec<String> = vec!["l0".into(), "l1".into(), "l2".into(), "l3".into(), "l4".into()];
        next_lines.extend(image_lines);
        next_lines.push("after".into());
        component.set_lines(next_lines);
        tui.request_render(None);
        terminal.wait_for_render().await;

        let writes = terminal.get_writes();
        assert!(
            tui.full_redraws() > redraws_before_image,
            "scrolling image append should force a full redraw"
        );
        assert!(
            writes.contains(&format!("\r\n\r\n\x1b[2A{image_sequence}\x1b[2B")),
            "full redraw should reserve visible image rows before drawing the placement"
        );
        assert!(
            !writes.contains(&format!("{image_sequence}\r\n\x1b[0m")),
            "full redraw must not write reserved padding rows after drawing the placement"
        );

        tui.stop(None);
    }

    #[tokio::test]
    #[serial]
    async fn does_not_use_cursor_up_placement_for_kitty_images_taller_than_the_viewport() {
        let _caps = KittyCapsGuard::new(10, 10);
        let terminal = Arc::new(LoggingVirtualTerminal::new(40, 5));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let component = Arc::new(TestComponent::new(vec!["before".to_string()]));
        tui.add_child(component.clone());

        tui.start();
        terminal.wait_for_render().await;
        terminal.clear_writes();

        let image = Image::new(
            "AAAA",
            "image/png",
            identity_image_theme(),
            Some(ImageOptions {
                max_width_cells: Some(6),
                ..Default::default()
            }),
            Some(ImageDimensions {
                width_px: 60,
                height_px: 60,
            }),
        );
        let image_lines = image.render(40);
        let image_sequence = image_lines[0].clone();
        assert!(
            image_lines.len() as i64 > terminal.rows(),
            "test image should exceed the viewport height"
        );

        let image_len = image_lines.len();
        let mut next_lines = vec!["before".to_string()];
        next_lines.extend(image_lines);
        next_lines.push("after".to_string());
        component.set_lines(next_lines);
        tui.request_render(Some(true));
        terminal.wait_for_render().await;

        let writes = terminal.get_writes();
        assert!(writes.contains(&image_sequence), "image placement should be drawn");
        assert!(
            !writes.contains(&format!("\x1b[{}A{image_sequence}", image_len - 1)),
            "taller-than-viewport images must keep the #4461 first-row placement path"
        );

        tui.stop(None);
    }

    #[tokio::test]
    async fn deletes_changed_image_ids_before_drawing_moved_placements() {
        let terminal = Arc::new(LoggingVirtualTerminal::new(40, 10));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let old_image = encode_kitty(
            "AAAA",
            Some(EncodeKittyOptions {
                columns: Some(2),
                rows: Some(2),
                image_id: Some(42),
                move_cursor: Some(false),
            }),
        );
        let component = Arc::new(TestComponent::new(vec!["top".to_string(), old_image]));
        tui.add_child(component.clone());
        tui.start();
        terminal.wait_for_render().await;
        terminal.clear_writes();

        let new_image = encode_kitty(
            "BBBB",
            Some(EncodeKittyOptions {
                columns: Some(2),
                rows: Some(1),
                image_id: Some(42),
                move_cursor: Some(false),
            }),
        );
        component.set_lines(vec![new_image.clone(), String::new()]);
        tui.request_render(None);
        terminal.wait_for_render().await;

        let writes = terminal.get_writes();
        let delete_seq = delete_kitty_image(42);
        let delete_index = writes.find(&delete_seq);
        let draw_index = writes.find(&new_image);
        assert!(delete_index.is_some(), "changed old image should be deleted");
        assert!(draw_index.is_some(), "new image should be drawn");
        assert!(
            delete_index.unwrap() < draw_index.unwrap(),
            "old image must be deleted before the new placement is drawn"
        );

        tui.stop(None);
    }

    #[tokio::test]
    async fn redraws_image_lines_when_an_earlier_reserved_image_row_changes() {
        let terminal = Arc::new(LoggingVirtualTerminal::new(40, 10));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let image = encode_kitty(
            "AAAA",
            Some(EncodeKittyOptions {
                columns: Some(2),
                rows: Some(2),
                image_id: Some(88),
                move_cursor: Some(false),
            }),
        );
        let component = Arc::new(TestComponent::new(vec![String::new(), image.clone()]));
        tui.add_child(component.clone());
        tui.start();
        terminal.wait_for_render().await;
        terminal.clear_writes();

        component.set_lines(vec!["covered".to_string(), image.clone()]);
        tui.request_render(None);
        terminal.wait_for_render().await;

        let writes = terminal.get_writes();
        let delete_seq = delete_kitty_image(88);
        let delete_index = writes.find(&delete_seq);
        let draw_index = writes.find(&image);
        assert!(
            delete_index.is_some(),
            "image should be deleted when a reserved row changes"
        );
        assert!(
            draw_index.is_some(),
            "unchanged image line should be redrawn after deleting the placement"
        );
        assert!(
            delete_index.unwrap() < draw_index.unwrap(),
            "old placement must be deleted before the image line is redrawn"
        );
        assert!(
            !writes.contains("\x1b[2J"),
            "reserved row changes should not force a full redraw"
        );

        tui.stop(None);
    }

    #[tokio::test]
    async fn deletes_previously_rendered_image_ids_during_full_redraws() {
        let terminal = Arc::new(LoggingVirtualTerminal::new(40, 10));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let image = encode_kitty(
            "AAAA",
            Some(EncodeKittyOptions {
                columns: Some(2),
                rows: Some(2),
                image_id: Some(77),
                move_cursor: Some(false),
            }),
        );
        let component = Arc::new(TestComponent::new(vec![image]));
        tui.add_child(component.clone());
        tui.start();
        terminal.wait_for_render().await;
        terminal.clear_writes();

        component.set_lines(vec!["plain text".to_string()]);
        tui.request_render(Some(true));
        terminal.wait_for_render().await;

        let writes = terminal.get_writes();
        let delete_seq = delete_kitty_image(77);
        let delete_index = writes.find(&delete_seq);
        let clear_index = writes.find("\x1b[2J");
        assert!(
            delete_index.is_some(),
            "previous image should be deleted during full redraw"
        );
        assert!(clear_index.is_some(), "full redraw should clear the screen");
        assert!(
            delete_index.unwrap() < clear_index.unwrap(),
            "old image should be deleted before the screen is cleared"
        );

        tui.stop(None);
    }
}

mod tui_resize_handling {
    use super::*;

    #[tokio::test]
    #[serial]
    async fn triggers_full_re_render_when_terminal_height_changes() {
        let _env = EnvGuard::set("TERMUX_VERSION", None);
        let terminal = Arc::new(VirtualTerminal::new(40, 10));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let component = Arc::new(TestComponent::new(vec![
            "Line 0".into(),
            "Line 1".into(),
            "Line 2".into(),
        ]));
        tui.add_child(component);
        tui.start();
        terminal.wait_for_render().await;

        let initial_redraws = tui.full_redraws();
        terminal.resize(40, 15);
        terminal.wait_for_render().await;

        assert!(
            tui.full_redraws() > initial_redraws,
            "Height change should trigger full redraw"
        );
        let viewport = terminal.get_viewport();
        assert!(viewport[0].contains("Line 0"), "Content preserved after height change");
        tui.stop(None);
    }

    #[tokio::test]
    #[serial]
    async fn skips_full_re_render_on_height_changes_in_termux() {
        let _env = EnvGuard::set("TERMUX_VERSION", Some("1"));
        let terminal = Arc::new(LoggingVirtualTerminal::new(40, 10));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let component = Arc::new(TestComponent::new((0..20).map(|i| format!("Line {i}")).collect()));
        tui.add_child(component);
        tui.start();
        terminal.wait_for_render().await;
        terminal.clear_writes();

        let initial_redraws = tui.full_redraws();
        for height in [15, 8, 14, 11] {
            terminal.resize(40, height);
            terminal.wait_for_render().await;
        }

        assert_eq!(
            tui.full_redraws(),
            initial_redraws,
            "Height change should not trigger full redraw"
        );
        assert!(
            !terminal.get_writes().contains("\x1b[2J"),
            "Height change should not clear the screen"
        );
        assert!(
            !terminal.get_writes().contains("\x1b[3J"),
            "Height change should not clear scrollback"
        );
        let viewport = terminal.get_viewport();
        assert!(
            viewport.join("\n").contains("Line 19"),
            "Latest content remains visible after resize"
        );
        tui.stop(None);
    }

    #[tokio::test]
    async fn triggers_full_re_render_when_terminal_width_changes() {
        let terminal = Arc::new(VirtualTerminal::new(40, 10));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let component = Arc::new(TestComponent::new(vec![
            "Line 0".into(),
            "Line 1".into(),
            "Line 2".into(),
        ]));
        tui.add_child(component);
        tui.start();
        terminal.wait_for_render().await;

        let initial_redraws = tui.full_redraws();
        terminal.resize(60, 10);
        terminal.wait_for_render().await;

        assert!(
            tui.full_redraws() > initial_redraws,
            "Width change should trigger full redraw"
        );
        tui.stop(None);
    }
}

mod tui_content_shrinkage {
    use super::*;

    #[tokio::test]
    async fn clears_empty_rows_when_content_shrinks_significantly() {
        let terminal = Arc::new(VirtualTerminal::new(40, 10));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        tui.set_clear_on_shrink(true);
        let component = Arc::new(TestComponent::new((0..=5).map(|i| format!("Line {i}")).collect()));
        tui.add_child(component.clone());
        tui.start();
        terminal.wait_for_render().await;

        let initial_redraws = tui.full_redraws();
        component.set_lines(vec!["Line 0".into(), "Line 1".into()]);
        tui.request_render(None);
        terminal.wait_for_render().await;

        assert!(
            tui.full_redraws() > initial_redraws,
            "Content shrinkage should trigger full redraw"
        );
        let viewport = terminal.get_viewport();
        assert!(viewport[0].contains("Line 0"), "First line preserved");
        assert!(viewport[1].contains("Line 1"), "Second line preserved");
        assert_eq!(viewport[2].trim(), "", "Line 2 should be cleared");
        assert_eq!(viewport[3].trim(), "", "Line 3 should be cleared");
        tui.stop(None);
    }

    #[tokio::test]
    async fn handles_shrink_to_single_line() {
        let terminal = Arc::new(VirtualTerminal::new(40, 10));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        tui.set_clear_on_shrink(true);
        let component = Arc::new(TestComponent::new((0..=3).map(|i| format!("Line {i}")).collect()));
        tui.add_child(component.clone());
        tui.start();
        terminal.wait_for_render().await;

        component.set_lines(vec!["Only line".into()]);
        tui.request_render(None);
        terminal.wait_for_render().await;

        let viewport = terminal.get_viewport();
        assert!(viewport[0].contains("Only line"), "Single line rendered");
        assert_eq!(viewport[1].trim(), "", "Line 1 should be cleared");
        tui.stop(None);
    }

    #[tokio::test]
    async fn handles_shrink_to_empty() {
        let terminal = Arc::new(VirtualTerminal::new(40, 10));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        tui.set_clear_on_shrink(true);
        let component = Arc::new(TestComponent::new((0..=2).map(|i| format!("Line {i}")).collect()));
        tui.add_child(component.clone());
        tui.start();
        terminal.wait_for_render().await;

        component.set_lines(Vec::new());
        tui.request_render(None);
        terminal.wait_for_render().await;

        let viewport = terminal.get_viewport();
        assert_eq!(viewport[0].trim(), "", "Line 0 should be cleared");
        assert_eq!(viewport[1].trim(), "", "Line 1 should be cleared");
        tui.stop(None);
    }
}

mod tui_differential_rendering {
    use super::*;

    #[tokio::test]
    async fn tracks_cursor_correctly_when_content_shrinks_with_unchanged_remaining_lines() {
        let terminal = Arc::new(VirtualTerminal::new(40, 10));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let component = Arc::new(TestComponent::new((0..=4).map(|i| format!("Line {i}")).collect()));
        tui.add_child(component.clone());
        tui.start();
        terminal.wait_for_render().await;

        component.set_lines(vec!["Line 0".into(), "Line 1".into(), "Line 2".into()]);
        tui.request_render(None);
        terminal.wait_for_render().await;

        component.set_lines(vec!["Line 0".into(), "CHANGED".into(), "Line 2".into()]);
        tui.request_render(None);
        terminal.wait_for_render().await;

        let viewport = terminal.get_viewport();
        assert!(
            viewport[1].contains("CHANGED"),
            "Expected \"CHANGED\" on line 1, got: {}",
            viewport[1]
        );
        tui.stop(None);
    }

    #[tokio::test]
    async fn renders_correctly_when_only_a_middle_line_changes_spinner_case() {
        let terminal = Arc::new(VirtualTerminal::new(40, 10));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let component = Arc::new(TestComponent::new(vec![
            "Header".into(),
            "Working...".into(),
            "Footer".into(),
        ]));
        tui.add_child(component.clone());
        tui.start();
        terminal.wait_for_render().await;

        for frame in ["|", "/", "-", "\\"] {
            component.set_lines(vec!["Header".into(), format!("Working {frame}"), "Footer".into()]);
            tui.request_render(None);
            terminal.wait_for_render().await;

            let viewport = terminal.get_viewport();
            assert!(viewport[0].contains("Header"));
            assert!(viewport[1].contains(&format!("Working {frame}")));
            assert!(viewport[2].contains("Footer"));
        }
        tui.stop(None);
    }

    #[tokio::test]
    async fn resets_styles_after_each_rendered_line() {
        let terminal = Arc::new(VirtualTerminal::new(20, 6));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let component = Arc::new(TestComponent::new(vec!["\x1b[3mItalic".into(), "Plain".into()]));
        tui.add_child(component);
        tui.start();
        terminal.wait_for_render().await;

        assert_eq!(terminal.get_cell(1, 0).expect("cell (1,0)").is_italic(), 0);
        tui.stop(None);
    }

    #[tokio::test]
    async fn renders_correctly_when_first_line_changes_but_rest_stays_same() {
        let terminal = Arc::new(VirtualTerminal::new(40, 10));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let component = Arc::new(TestComponent::new((0..=3).map(|i| format!("Line {i}")).collect()));
        tui.add_child(component.clone());
        tui.start();
        terminal.wait_for_render().await;

        component.set_lines(vec![
            "CHANGED".into(),
            "Line 1".into(),
            "Line 2".into(),
            "Line 3".into(),
        ]);
        tui.request_render(None);
        terminal.wait_for_render().await;

        let viewport = terminal.get_viewport();
        assert!(viewport[0].contains("CHANGED"));
        assert!(viewport[1].contains("Line 1"));
        assert!(viewport[2].contains("Line 2"));
        assert!(viewport[3].contains("Line 3"));
        tui.stop(None);
    }

    #[tokio::test]
    async fn renders_correctly_when_last_line_changes_but_rest_stays_same() {
        let terminal = Arc::new(VirtualTerminal::new(40, 10));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let component = Arc::new(TestComponent::new((0..=3).map(|i| format!("Line {i}")).collect()));
        tui.add_child(component.clone());
        tui.start();
        terminal.wait_for_render().await;

        component.set_lines(vec![
            "Line 0".into(),
            "Line 1".into(),
            "Line 2".into(),
            "CHANGED".into(),
        ]);
        tui.request_render(None);
        terminal.wait_for_render().await;

        let viewport = terminal.get_viewport();
        assert!(viewport[0].contains("Line 0"));
        assert!(viewport[1].contains("Line 1"));
        assert!(viewport[2].contains("Line 2"));
        assert!(viewport[3].contains("CHANGED"));
        tui.stop(None);
    }

    #[tokio::test]
    async fn renders_correctly_when_multiple_non_adjacent_lines_change() {
        let terminal = Arc::new(VirtualTerminal::new(40, 10));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let component = Arc::new(TestComponent::new((0..=4).map(|i| format!("Line {i}")).collect()));
        tui.add_child(component.clone());
        tui.start();
        terminal.wait_for_render().await;

        component.set_lines(vec![
            "Line 0".into(),
            "CHANGED 1".into(),
            "Line 2".into(),
            "CHANGED 3".into(),
            "Line 4".into(),
        ]);
        tui.request_render(None);
        terminal.wait_for_render().await;

        let viewport = terminal.get_viewport();
        assert!(viewport[0].contains("Line 0"));
        assert!(viewport[1].contains("CHANGED 1"));
        assert!(viewport[2].contains("Line 2"));
        assert!(viewport[3].contains("CHANGED 3"));
        assert!(viewport[4].contains("Line 4"));
        tui.stop(None);
    }

    #[tokio::test]
    async fn handles_transition_from_content_to_empty_and_back_to_content() {
        let terminal = Arc::new(VirtualTerminal::new(40, 10));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let component = Arc::new(TestComponent::new((0..=2).map(|i| format!("Line {i}")).collect()));
        tui.add_child(component.clone());
        tui.start();
        terminal.wait_for_render().await;

        assert!(terminal.get_viewport()[0].contains("Line 0"));

        component.set_lines(Vec::new());
        tui.request_render(None);
        terminal.wait_for_render().await;

        component.set_lines(vec!["New Line 0".into(), "New Line 1".into()]);
        tui.request_render(None);
        terminal.wait_for_render().await;

        let viewport = terminal.get_viewport();
        assert!(viewport[0].contains("New Line 0"));
        assert!(viewport[1].contains("New Line 1"));
        tui.stop(None);
    }

    #[tokio::test]
    async fn full_re_renders_when_deleted_lines_move_the_viewport_upward() {
        let terminal = Arc::new(VirtualTerminal::new(20, 5));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let component = Arc::new(TestComponent::new((0..12).map(|i| format!("Line {i}")).collect()));
        tui.add_child(component.clone());
        tui.start();
        terminal.wait_for_render().await;

        let initial_redraws = tui.full_redraws();
        component.set_lines((0..7).map(|i| format!("Line {i}")).collect());
        tui.request_render(None);
        terminal.wait_for_render().await;

        assert!(
            tui.full_redraws() > initial_redraws,
            "Shrink should trigger a full redraw"
        );
        assert_eq!(
            terminal.get_viewport().iter().map(|s| s.trim_end()).collect::<Vec<_>>(),
            vec!["Line 2", "Line 3", "Line 4", "Line 5", "Line 6"]
        );
        tui.stop(None);
    }

    #[tokio::test]
    async fn appends_after_a_shrink_without_another_full_redraw_once_the_viewport_is_reset() {
        let terminal = Arc::new(VirtualTerminal::new(20, 5));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let component = Arc::new(TestComponent::new((0..8).map(|i| format!("Line {i}")).collect()));
        tui.add_child(component.clone());
        tui.start();
        terminal.wait_for_render().await;

        let initial_redraws = tui.full_redraws();
        component.set_lines(vec!["Line 0".into(), "Line 1".into()]);
        tui.request_render(None);
        terminal.wait_for_render().await;

        assert!(
            tui.full_redraws() > initial_redraws,
            "Shrink should reset the viewport with a full redraw"
        );
        let redraws_after_shrink = tui.full_redraws();

        component.set_lines(vec!["Line 0".into(), "Line 1".into(), "Line 2".into()]);
        tui.request_render(None);
        terminal.wait_for_render().await;

        assert_eq!(
            tui.full_redraws(),
            redraws_after_shrink,
            "Append should stay on the differential path"
        );
        assert_eq!(
            terminal.get_viewport().iter().map(|s| s.trim_end()).collect::<Vec<_>>(),
            vec!["Line 0", "Line 1", "Line 2", "", ""]
        );
        tui.stop(None);
    }

    #[tokio::test]
    async fn clears_stale_content_when_max_lines_rendered_was_inflated_by_a_transient_component() {
        let terminal = Arc::new(VirtualTerminal::new(40, 10));
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let long_chat: Vec<String> = (0..15).map(|i| format!("Chat {i}")).collect();
        let short_chat: Vec<String> = (0..12).map(|i| format!("Chat {i}")).collect();
        let editor_lines: Vec<String> = vec!["Editor 0".into(), "Editor 1".into(), "Editor 2".into()];
        let selector_lines: Vec<String> = (0..8).map(|i| format!("Selector {i}")).collect();

        let chat = Arc::new(TestComponent::new(long_chat));
        let editor = Arc::new(TestComponent::new(editor_lines.clone()));
        tui.add_child(chat.clone());
        tui.add_child(editor.clone());
        tui.start();
        terminal.wait_for_render().await;

        editor.set_lines(selector_lines);
        tui.request_render(None);
        terminal.wait_for_render().await;

        editor.set_lines(editor_lines);
        tui.request_render(None);
        terminal.wait_for_render().await;

        let redraws_before_switch = tui.full_redraws();
        chat.set_lines(short_chat);
        tui.request_render(None);
        terminal.wait_for_render().await;

        assert!(
            tui.full_redraws() > redraws_before_switch,
            "Branch switch should trigger a full redraw"
        );

        let viewport = terminal.get_viewport();
        for (i, line) in viewport.iter().enumerate().take(10) {
            assert!(!line.contains("Chat 12"), "Stale Chat 12 at row {i}");
            assert!(!line.contains("Chat 13"), "Stale Chat 13 at row {i}");
            assert!(!line.contains("Chat 14"), "Stale Chat 14 at row {i}");
        }
        assert_eq!(
            viewport.iter().map(|s| s.trim_end()).collect::<Vec<_>>(),
            vec![
                "Chat 5", "Chat 6", "Chat 7", "Chat 8", "Chat 9", "Chat 10", "Chat 11", "Editor 0", "Editor 1",
                "Editor 2",
            ]
        );
        tui.stop(None);
    }
}

mod tui_osc_133_markers {
    use super::*;

    #[test]
    fn does_not_write_osc_133_prompt_markers_to_the_terminal() {
        // Terminals read 133;A as a new shell prompt and drop the OSC 7501 program status.
        let terminal = Arc::new(BoundedWriteTerminal::new());
        let tui = TuiMainScreen::new(terminal.clone(), None, None);
        let component = Arc::new(TestComponent::new(vec![
            "\x1b]133;A\x07prompt".to_string(),
            "\x1b]133;B\x07\x1b]133;C\x07output".to_string(),
        ]));
        tui.add_child(component.clone());
        tui.render_now(None);
        component.set_lines(vec![
            "\x1b]133;A\x07prompt".to_string(),
            "\x1b]133;B\x07\x1b]133;C\x07output".to_string(),
            "\x1b]133;A\x1b\\reply".to_string(),
            "\x1b]133;B\x1b\\\x1b]133;C\x1b\\done".to_string(),
        ]);
        tui.render_now(None);

        let output = terminal.writes().concat();
        assert!(
            !output.contains("\x1b]133;"),
            "regular mode must not emit OSC 133 markers"
        );
        for text in ["prompt", "output", "reply", "done"] {
            assert!(output.contains(text), "expected output to include {text}");
        }
    }
}

mod tui_fake_cursor {
    use super::*;

    #[tokio::test]
    async fn draws_the_focused_fake_cursor_in_reverse_video_and_omits_it_when_the_hardware_cursor_is_shown() {
        let terminal = Arc::new(LoggingVirtualTerminal::new(20, 5));
        let tui = Arc::new(TuiMainScreen::new(terminal.clone(), None, None));
        let input = Arc::new(Input::new(None));
        input.set_value("abc");
        tui.add_child(input.clone());
        tui.set_focus(Some(input));
        tui.start();
        terminal.wait_for_render().await;
        assert!(terminal.get_writes().contains("> \x1b[7ma\x1b[27mbc"));

        terminal.clear_writes();
        tui.set_show_hardware_cursor(true);
        terminal.wait_for_render().await;
        assert!(!terminal.get_writes().contains("\x1b[7m"));
        assert!(!terminal.get_writes().contains("\x1b_pi:"));

        tui.stop(None);
    }

    #[tokio::test]
    async fn keeps_the_fake_cursor_of_an_unfocused_input_when_the_hardware_cursor_is_shown() {
        let terminal = Arc::new(LoggingVirtualTerminal::new(20, 5));
        let tui = Arc::new(TuiMainScreen::new(terminal.clone(), Some(true), None));
        let input = Arc::new(Input::new(None));
        input.set_value("abc");
        tui.add_child(input);
        tui.start();
        terminal.wait_for_render().await;
        assert!(terminal.get_writes().contains("> \x1b[7ma\x1b[27mbc"));
        tui.stop(None);
    }

    #[tokio::test]
    async fn omits_the_focused_fake_cursor_when_truncation_dropped_its_end_marker() {
        let terminal = Arc::new(LoggingVirtualTerminal::new(20, 5));
        let tui = Arc::new(TuiMainScreen::new(terminal.clone(), Some(true), None));
        let truncated = slice_by_column(
            &format!("ab{CURSOR_MARKER}{}def", render_fake_cursor("c")),
            0,
            3,
            Some(true),
        );
        let base = Arc::new(TestComponent::new(vec![truncated]));
        tui.add_child(base);
        tui.start();
        terminal.wait_for_render().await;
        assert!(!terminal.get_writes().contains("\x1b[7m"));
        assert!(terminal.get_writes().ends_with("\x1b[3G"));
        tui.stop(None);
    }

    #[tokio::test]
    async fn keeps_the_cursor_when_an_overlay_ends_directly_left_of_it() {
        for show_hardware_cursor in [false, true] {
            let terminal = Arc::new(LoggingVirtualTerminal::new(20, 5));
            let tui = Arc::new(TuiMainScreen::new(terminal.clone(), Some(show_hardware_cursor), None));
            let base = Arc::new(TestComponent::new(vec![format!(
                "abc{CURSOR_MARKER}{}ef",
                render_fake_cursor("d")
            )]));
            tui.add_child(base);
            let overlay = Arc::new(TestComponent::new(vec!["XXX".to_string()]));
            tui.show_overlay(
                overlay,
                Some(OverlayOptions {
                    row: Some(SizeValue::Absolute(0.0)),
                    col: Some(SizeValue::Absolute(0.0)),
                    width: Some(SizeValue::Absolute(3.0)),
                    ..Default::default()
                }),
            );
            tui.start();
            terminal.wait_for_render().await;

            let writes = terminal.get_writes();
            assert!(writes.contains("XXX"));
            assert_eq!(writes.contains("\x1b[7md"), !show_hardware_cursor);
            // The hardware cursor is positioned at column 4 in both modes
            assert!(writes.ends_with("\x1b[4G"));
            tui.stop(None);
        }
    }
}
