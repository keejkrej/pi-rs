//! Port of packages/tui/test/terminal-colors.test.ts

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
use pi_tui::terminal_colors::{
    OscColorResponse, OscColorTarget, RgbColor, TerminalColorScheme, TerminalColors, parse_osc_color_response,
    parse_terminal_color_scheme_report,
};
use pi_tui::tui::QueryTerminalColorsOptions;
use pi_tui::tui_main_screen::TuiMainScreen;

struct TestTerminal {
    writes: Mutex<Vec<String>>,
    on_input: Mutex<Option<TerminalInputHandler>>,
}

impl TestTerminal {
    fn new() -> Self {
        Self {
            writes: Mutex::new(Vec::new()),
            on_input: Mutex::new(None),
        }
    }

    fn writes(&self) -> Vec<String> {
        self.writes.lock().unwrap().clone()
    }

    fn emit_input(&self, data: &str) {
        let cb = self.on_input.lock().unwrap().clone();
        if let Some(handler) = cb {
            handler(data);
        }
    }
}

#[async_trait]
impl Terminal for TestTerminal {
    fn start(&self, on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {
        *self.on_input.lock().unwrap() = Some(on_input);
    }

    fn stop(&self) {
        *self.on_input.lock().unwrap() = None;
    }

    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}

    fn write(&self, data: &str) {
        self.writes.lock().unwrap().push(data.to_string());
    }

    fn columns(&self) -> i64 {
        80
    }

    fn rows(&self) -> i64 {
        24
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

#[test]
fn parses_dark_scheme_reports() {
    assert_eq!(
        parse_terminal_color_scheme_report("\x1b[?997;1n"),
        Some(TerminalColorScheme::Dark)
    );
}

#[test]
fn parses_light_scheme_reports() {
    assert_eq!(
        parse_terminal_color_scheme_report("\x1b[?997;2n"),
        Some(TerminalColorScheme::Light)
    );
}

#[test]
fn uses_the_final_scheme_when_multiple_reports_arrive_together() {
    assert_eq!(
        parse_terminal_color_scheme_report("\x1b[?997;1n\x1b[?997;2n"),
        Some(TerminalColorScheme::Light)
    );
}

#[test]
fn returns_none_for_unrelated_or_malformed_input() {
    assert_eq!(parse_terminal_color_scheme_report("\x1b[?997;3n"), None);
    assert_eq!(parse_terminal_color_scheme_report("hello"), None);
    assert_eq!(parse_terminal_color_scheme_report(""), None);
    assert_eq!(parse_terminal_color_scheme_report("\x1b[?997;1nextra"), None);
}

#[test]
fn parses_osc_10_and_11_with_rgb_format_and_bel_or_st_terminators() {
    assert_eq!(
        parse_osc_color_response("\x1b]10;rgb:ffff/0000/8080\x07"),
        Some(OscColorResponse {
            target: OscColorTarget::Foreground,
            rgb: Some(RgbColor {
                r: 255.0,
                g: 0.0,
                b: 128.0,
            }),
        })
    );
    assert_eq!(
        parse_osc_color_response("\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\"),
        Some(OscColorResponse {
            target: OscColorTarget::Background,
            rgb: Some(RgbColor {
                r: 30.0,
                g: 30.0,
                b: 46.0,
            }),
        })
    );
}

#[test]
fn parses_osc_4_palette_index_and_rgb_and_hash_color_formats() {
    assert_eq!(
        parse_osc_color_response("\x1b]4;12;rgb:80/40/ff\x07"),
        Some(OscColorResponse {
            target: OscColorTarget::Index(12),
            rgb: Some(RgbColor {
                r: 128.0,
                g: 64.0,
                b: 255.0,
            }),
        })
    );
    assert_eq!(
        parse_osc_color_response("\x1b]11;#123456\x07"),
        Some(OscColorResponse {
            target: OscColorTarget::Background,
            rgb: Some(RgbColor {
                r: 0x12 as f64,
                g: 0x34 as f64,
                b: 0x56 as f64,
            }),
        })
    );
}

#[test]
fn returns_none_rgb_when_color_payload_is_unparseable_and_none_for_other_input() {
    assert_eq!(
        parse_osc_color_response("\x1b]10;rgb:zzzz/0000/0000\x07"),
        Some(OscColorResponse {
            target: OscColorTarget::Foreground,
            rgb: None,
        })
    );
    assert_eq!(parse_osc_color_response("\x1b]99;hello\x07"), None);
}

#[tokio::test]
async fn queries_theme_colors_in_one_write_and_resolves_once_all_replies_arrive_without_da1() {
    let terminal = Arc::new(TestTerminal::new());
    let tui = TuiMainScreen::new(terminal.clone(), None, None);
    let forward = Arc::new(Mutex::new(Vec::<String>::new()));
    let forward_ref = Arc::clone(&forward);
    tui.add_input_listener(Arc::new(move |data: &str| {
        forward_ref.lock().unwrap().push(data.to_string());
        None
    }));

    tui.start();

    let tui_task = tui.clone();
    let handle = tokio::spawn(async move {
        tui_task
            .query_terminal_colors(QueryTerminalColorsOptions {
                timeout_ms: 1000,
                on_late_reply: None,
            })
            .await
    });
    tokio::task::yield_now().await;

    let writes = terminal.writes();
    let query_write = writes
        .iter()
        .find(|w| w.contains("\x1b]10;?\x07"))
        .expect("query write")
        .clone();
    assert!(query_write.contains("\x1b]10;?\x07\x1b]11;?\x07"));
    for i in 0..16 {
        assert!(query_write.contains(&format!("\x1b]4;{i};?\x07")));
    }
    assert!(query_write.ends_with("\x1b[c"));

    terminal.emit_input("\x1b]10;rgb:c0c0/c0c0/c0c0\x07");
    terminal.emit_input("\x1b]11;rgb:1010/1010/1010\x07");
    for i in 0..16 {
        let hex = format!("{:02x}", i * 16);
        terminal.emit_input(&format!("\x1b]4;{i};rgb:{hex}{hex}/0000/ffff\x07"));
    }

    let colors = handle.await.unwrap();
    let expected_palette: Vec<RgbColor> = (0..16)
        .map(|i| RgbColor {
            r: (i * 16) as f64,
            g: 0.0,
            b: 255.0,
        })
        .collect();
    assert_eq!(
        colors,
        TerminalColors {
            foreground: Some(RgbColor {
                r: 192.0,
                g: 192.0,
                b: 192.0,
            }),
            background: Some(RgbColor {
                r: 16.0,
                g: 16.0,
                b: 16.0,
            }),
            palette: Some(expected_palette),
        }
    );
    assert!(forward.lock().unwrap().is_empty());

    tui.stop(None);
}

#[tokio::test]
async fn queries_theme_colors_in_order_on_da1_and_drops_incomplete_palettes() {
    let terminal = Arc::new(TestTerminal::new());
    let tui = TuiMainScreen::new(terminal.clone(), None, None);
    tui.start();

    let tui1 = tui.clone();
    let q1 = tokio::spawn(async move {
        tui1.query_terminal_colors(QueryTerminalColorsOptions {
            timeout_ms: 1000,
            on_late_reply: None,
        })
        .await
    });
    let tui2 = tui.clone();
    let q2 = tokio::spawn(async move {
        tui2.query_terminal_colors(QueryTerminalColorsOptions {
            timeout_ms: 1000,
            on_late_reply: None,
        })
        .await
    });
    tokio::task::yield_now().await;

    terminal.emit_input("\x1b]11;rgb:2020/2020/2020\x07");
    terminal.emit_input("\x1b]4;0;rgb:0000/0000/0000\x07");
    terminal.emit_input("\x1b[?62;22c");

    let r1 = q1.await.unwrap();
    assert_eq!(
        r1,
        TerminalColors {
            foreground: None,
            background: Some(RgbColor {
                r: 32.0,
                g: 32.0,
                b: 32.0,
            }),
            palette: None,
        }
    );

    terminal.emit_input("\x1b]10;rgb:eeee/eeee/eeee\x07");
    terminal.emit_input("\x1b[?62;22c");

    let r2 = q2.await.unwrap();
    assert_eq!(
        r2,
        TerminalColors {
            foreground: Some(RgbColor {
                r: 238.0,
                g: 238.0,
                b: 238.0,
            }),
            background: None,
            palette: None,
        }
    );

    tui.stop(None);
}

#[tokio::test]
async fn delivers_late_replies_after_timeout_and_leaves_future_osc_10_11_4_unconsumed() {
    let terminal = Arc::new(TestTerminal::new());
    let tui = TuiMainScreen::new(terminal.clone(), None, None);
    let forwarded = Arc::new(Mutex::new(Vec::<String>::new()));
    let forwarded_ref = Arc::clone(&forwarded);
    tui.add_input_listener(Arc::new(move |data: &str| {
        forwarded_ref.lock().unwrap().push(data.to_string());
        None
    }));

    tui.start();

    let late_colors = Arc::new(Mutex::new(None::<TerminalColors>));
    let late_colors_ref = Arc::clone(&late_colors);
    let timed_out = tui
        .query_terminal_colors(QueryTerminalColorsOptions {
            timeout_ms: 10,
            on_late_reply: Some(Arc::new(move |colors| {
                *late_colors_ref.lock().unwrap() = Some(colors);
            })),
        })
        .await;

    assert_eq!(
        timed_out,
        TerminalColors {
            foreground: None,
            background: None,
            palette: None,
        }
    );

    terminal.emit_input("\x1b]11;rgb:2828/2828/2828\x07");
    terminal.emit_input("\x1b[?62;22c");

    assert_eq!(
        *late_colors.lock().unwrap(),
        Some(TerminalColors {
            foreground: None,
            background: Some(RgbColor {
                r: 40.0,
                g: 40.0,
                b: 40.0,
            }),
            palette: None,
        })
    );

    terminal.emit_input("\x1b]11;rgb:1111/2222/3333\x07");
    assert_eq!(
        *forwarded.lock().unwrap(),
        vec!["\x1b]11;rgb:1111/2222/3333\x07".to_string()]
    );

    tui.stop(None);
}
