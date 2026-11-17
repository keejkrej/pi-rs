//! Port of packages/tui/test/terminal.test.ts

use std::sync::{Arc, Mutex};
use std::time::Duration;

use indexmap::IndexMap;
use pi_js::env::testing::EnvGuard;
use pi_js::seam::Guard;
use pi_tui::keys::set_kitty_protocol_active;
use pi_tui::terminal::testing::{feed_stdin_data, override_stdout_size, override_stdout_write, start_negotiation_test};
use pi_tui::terminal::{
    ProcessTerminal, Terminal, normalize_apple_terminal_input, normalize_native_shift_enter_input,
    resolve_escape_timeout_ms,
};
use serial_test::serial;

fn env_map(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
    let mut map = IndexMap::new();
    for (k, v) in pairs {
        map.insert((*k).to_string(), (*v).to_string());
    }
    map
}

async fn wait_ms(ms: u64) {
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(ms)).await;
    tokio::task::yield_now().await;
}

struct NegotiationHarness {
    terminal: ProcessTerminal,
    writes: Arc<Mutex<Vec<String>>>,
    input: Arc<Mutex<Option<String>>>,
    cleaned: bool,
    stdout_guard: Option<Guard>,
}

impl NegotiationHarness {
    fn new() -> Self {
        let terminal = ProcessTerminal::new();
        let writes = Arc::new(Mutex::new(Vec::new()));
        let writes_clone = Arc::clone(&writes);
        let stdout_guard = override_stdout_write(move |chunk| {
            writes_clone.lock().unwrap().push(chunk.to_string());
        });

        let input = Arc::new(Mutex::new(None));
        let input_clone = Arc::clone(&input);
        start_negotiation_test(
            &terminal,
            Arc::new(move |data| {
                *input_clone.lock().unwrap() = Some(data.to_string());
            }),
        );

        Self {
            terminal,
            writes,
            input,
            cleaned: false,
            stdout_guard: Some(stdout_guard),
        }
    }

    fn send(&self, data: &str) {
        feed_stdin_data(&self.terminal, data);
    }

    fn get_input(&self) -> Option<String> {
        self.input.lock().unwrap().clone()
    }

    fn writes(&self) -> Vec<String> {
        self.writes.lock().unwrap().clone()
    }

    fn cleanup(&mut self) {
        if self.cleaned {
            return;
        }
        self.cleaned = true;
        self.terminal.stop();
        set_kitty_protocol_active(false);
        self.stdout_guard.take();
    }
}

impl Drop for NegotiationHarness {
    fn drop(&mut self) {
        self.cleanup();
    }
}

// ============================================================================
// resolveEscapeTimeoutMs
// ============================================================================

#[test]
fn uses_pi_tui_esc_timeout_when_configured() {
    assert_eq!(
        resolve_escape_timeout_ms(Some(&env_map(&[("PI_TUI_ESC_TIMEOUT", "80")]))),
        80.0
    );
    assert_eq!(
        resolve_escape_timeout_ms(Some(&env_map(&[
            ("PI_TUI_ESC_TIMEOUT", "80"),
            ("SSH_TTY", "/dev/pts/1"),
        ]))),
        80.0
    );
}

#[test]
fn ignores_invalid_pi_tui_esc_timeout_values() {
    assert_eq!(
        resolve_escape_timeout_ms(Some(&env_map(&[("PI_TUI_ESC_TIMEOUT", "abc")]))),
        10.0
    );
    assert_eq!(
        resolve_escape_timeout_ms(Some(&env_map(&[("PI_TUI_ESC_TIMEOUT", "0")]))),
        10.0
    );
    assert_eq!(
        resolve_escape_timeout_ms(Some(&env_map(&[("PI_TUI_ESC_TIMEOUT", "-5")]))),
        10.0
    );
    assert_eq!(
        resolve_escape_timeout_ms(Some(&env_map(&[("PI_TUI_ESC_TIMEOUT", "")]))),
        10.0
    );
}

#[test]
fn defaults_to_100ms_over_ssh() {
    assert_eq!(
        resolve_escape_timeout_ms(Some(&env_map(&[("SSH_CONNECTION", "10.0.0.1 22")]))),
        100.0
    );
    assert_eq!(
        resolve_escape_timeout_ms(Some(&env_map(&[("SSH_TTY", "/dev/pts/1")]))),
        100.0
    );
}

#[test]
fn defaults_to_10ms_otherwise() {
    assert_eq!(resolve_escape_timeout_ms(Some(&IndexMap::new())), 10.0);
}

// ============================================================================
// normalizeNativeShiftEnterInput
// ============================================================================

#[test]
fn rewrites_return_to_csi_u_shift_enter_when_native_shift_detection_is_enabled_and_shift_is_pressed() {
    assert_eq!(normalize_native_shift_enter_input("\r", true, true), "\x1b[13;2u");
}

#[test]
fn leaves_return_unchanged_when_native_shift_detection_is_disabled() {
    assert_eq!(normalize_native_shift_enter_input("\r", false, true), "\r");
}

#[test]
fn leaves_return_unchanged_when_shift_is_not_pressed() {
    assert_eq!(normalize_native_shift_enter_input("\r", true, false), "\r");
}

#[test]
fn leaves_non_return_input_unchanged_native() {
    assert_eq!(
        normalize_native_shift_enter_input("\x1b[13;2u", true, true),
        "\x1b[13;2u"
    );
    assert_eq!(normalize_native_shift_enter_input("a", true, true), "a");
}

// ============================================================================
// normalizeAppleTerminalInput
// ============================================================================

#[test]
fn rewrites_apple_terminal_return_to_csi_u_shift_enter_when_shift_is_pressed() {
    assert_eq!(normalize_apple_terminal_input("\r", true, true), "\x1b[13;2u");
}

#[test]
fn leaves_apple_terminal_return_unchanged_when_shift_is_not_pressed() {
    assert_eq!(normalize_apple_terminal_input("\r", true, false), "\r");
}

#[test]
fn leaves_non_apple_terminal_return_unchanged_when_shift_is_pressed() {
    assert_eq!(normalize_apple_terminal_input("\r", false, true), "\r");
}

#[test]
fn leaves_non_return_input_unchanged_apple() {
    assert_eq!(normalize_apple_terminal_input("\x1b[13;2u", true, true), "\x1b[13;2u");
    assert_eq!(normalize_apple_terminal_input("a", true, true), "a");
}

// ============================================================================
// ProcessTerminal Kitty keyboard protocol negotiation
// ============================================================================

#[test]
#[serial]
fn queries_kitty_mode_before_enabling_modify_other_keys_fallback() {
    let mut harness = NegotiationHarness::new();
    let writes = harness.writes();
    assert_eq!(writes[0], "\x1b[>7u\x1b[?u\x1b[c");
    assert!(!writes.iter().any(|w| w == "\x1b[>4;2m"));
    assert!(!harness.terminal.kitty_protocol_active());
    harness.cleanup();
}

#[test]
#[serial]
fn activates_kitty_mode_for_non_zero_negotiated_flags() {
    let mut harness = NegotiationHarness::new();
    harness.send("\x1b[?7u");

    assert_eq!(harness.get_input(), None);
    assert!(harness.terminal.kitty_protocol_active());
    let writes = harness.writes();
    assert!(!writes.iter().any(|w| w == "\x1b[>4;2m"));
    assert!(!writes.iter().any(|w| w == "\x1b[>4;0m"));

    harness.cleanup();
    let writes = harness.writes();
    assert_eq!(writes.iter().filter(|w| w.as_str() == "\x1b[<u").count(), 1);
    assert!(!writes.iter().any(|w| w == "\x1b[>4;0m"));
}

#[test]
#[serial]
fn falls_back_to_modify_other_keys_for_zero_kitty_flags() {
    let mut harness = NegotiationHarness::new();
    harness.send("\x1b[?0u");

    assert_eq!(harness.get_input(), None);
    assert!(!harness.terminal.kitty_protocol_active());
    let writes = harness.writes();
    assert_eq!(writes.iter().filter(|w| w.as_str() == "\x1b[>4;2m").count(), 1);

    harness.cleanup();
    let writes = harness.writes();
    assert_eq!(writes.iter().filter(|w| w.as_str() == "\x1b[>4;0m").count(), 1);
}

#[test]
#[serial]
fn falls_back_to_modify_other_keys_for_device_attributes_without_kitty_flags() {
    let mut harness = NegotiationHarness::new();
    harness.send("\x1b[?62;4;52c");

    assert_eq!(harness.get_input(), None);
    assert!(!harness.terminal.kitty_protocol_active());
    let writes = harness.writes();
    assert_eq!(writes.iter().filter(|w| w.as_str() == "\x1b[>4;2m").count(), 1);
    harness.cleanup();
}

#[test]
#[serial]
fn forwards_device_attributes_replies_that_answer_other_queries() {
    let mut harness = NegotiationHarness::new();
    harness.send("\x1b[?7u");
    harness.send("\x1b[?62;4;52c");
    assert_eq!(harness.get_input(), None);

    // The TUI's color query uses DA1 as its own sentinel.
    harness.send("\x1b[?62;4;52c");
    assert_eq!(harness.get_input().as_deref(), Some("\x1b[?62;4;52c"));
    harness.cleanup();
}

#[test]
#[serial]
fn forwards_normal_input_while_waiting_for_kitty_response() {
    let mut harness = NegotiationHarness::new();
    harness.send("a");

    assert_eq!(harness.get_input().as_deref(), Some("a"));
    assert!(!harness.terminal.kitty_protocol_active());
    harness.cleanup();
}

#[tokio::test(start_paused = true)]
#[serial]
async fn tracks_split_kitty_confirmation() {
    let mut harness = NegotiationHarness::new();
    harness.send("\x1b[?7");
    wait_ms(10).await;

    assert_eq!(harness.get_input(), None);

    harness.send("u");

    assert!(harness.terminal.kitty_protocol_active());
    assert!(!harness.writes().iter().any(|w| w == "\x1b[>4;2m"));
    harness.cleanup();
}

#[tokio::test(start_paused = true)]
#[serial]
async fn replays_buffered_csi_prefix_input_when_it_is_not_a_kitty_response() {
    let mut harness = NegotiationHarness::new();
    harness.send("\x1b[");
    wait_ms(50).await;

    assert_eq!(harness.get_input(), None);

    wait_ms(150).await;

    assert_eq!(harness.get_input().as_deref(), Some("\x1b["));
    harness.cleanup();
}

// ============================================================================
// ProcessTerminal progress & dimensions
// ============================================================================

#[test]
#[serial]
fn writes_a_valid_osc_9_4_clear_sequence() {
    let terminal = ProcessTerminal::new();
    let writes = Arc::new(Mutex::new(Vec::new()));
    let writes_clone = Arc::clone(&writes);
    let _write = override_stdout_write(move |chunk| {
        writes_clone.lock().unwrap().push(chunk.to_string());
    });

    terminal.set_progress(false);
    assert_eq!(*writes.lock().unwrap(), vec!["\x1b]9;4;0\x07"]);
}

#[test]
#[serial]
fn falls_back_to_columns_and_lines_before_default_dimensions() {
    let _size = override_stdout_size(|| None);
    let _env = EnvGuard::set_many(&[("COLUMNS", Some("123")), ("LINES", Some("45"))]);

    let terminal = ProcessTerminal::new();
    assert_eq!(terminal.columns(), 123);
    assert_eq!(terminal.rows(), 45);
}
