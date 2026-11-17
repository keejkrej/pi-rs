//! Port of packages/coding-agent/src/modes/interactive/components/daxnuts.ts
//!
//! POWERED BY DAXNUTS - Easter egg for OpenCode + Kimi K2.5
//!
//! A heartfelt tribute to dax (@thdxr) for providing free Kimi K2.5 access via OpenCode.

use std::sync::{Arc, LazyLock, Mutex};

use pi_js::time::{Interval, set_interval};
use pi_tui::{Component, TUI, TuiMouseEvent, TuiMouseEventResult};
use regex::Regex;

use super::super::theme::theme::{ThemeColor, theme};

// 32x32 RGB image of dax, hex encoded (3 bytes per pixel)
const DAX_HEX: &str = concat!(
    "bbbab8b9b9b6b9b8b5bcbbb8b8b7b4b7b5b2b6b5b2b8b7b4b7b6b3b6b4b1bdbcb8bab8b6bbb8b5b8b5b1bbb8b4c2bebbc1bebac0bdbabfbcb9c1bebabfbebbc0",
    "bfbcc0bdbabbb8b5c1bfbcbfbcb8bbb9b6bfbcb8c2bfbcc1bfbcbfbbb8bdb9b6b8b7b5b9b8b5b8b8b5b5b5b2b6b5b2b8b7b4b9b8b5b9b8b5b6b5b3bab8b5bcba",
    "b7bbb9b6bbb8b5bfb9b5bdb2abbcb0a8beb2aabeb5afbfbab6bebab7c0bfbcbebdbabebbb8c0bdbabfbebbc2bebbbdbab7c3c0bdc3c0bdc1bebbc2bebabfbcb8",
    "bab9b6b7b6b3b2b1aeb6b5b2b5b4b1b5b4b2b6b5b2b7b6b4b9b8b6b7b6b3bbbab7b2afaba5988fb49e90b09481b79a88b39683b09583b7a395bfb6b0c0bdbabd",
    "bbb8bebcb9c1bfbcc0bebbbdbab7bebbb8c2bfbcc0bdbac0bcb9bdb9b6c0bcb8b5b4b2b4b3b0bab9b6b9b9b6b5b4b1b5b4b1b6b5b3b9b8b5b9b8b6b9b8b6b2ae",
    "aa968174a6836eaa856eab846eaf8973ac8973b08f79b18f7ab39786b7a89dbbb3aebfbab6c2c0bdbebcb9bfbdbac3c1bdc2bebbc0bcb9bdb9b6c1bdbabfbb",
    "b8b4b3b0b9b8b5b8b7b5b4b3b1b5b4b1b8b7b4b8b7b5bab9b6bbbab7b1afad8c7a719d735ca47860a87d65a98069ae8972ae8c75af8d77aa826ba98067aa8974",
    "b39e90b6a79dbbb2adc0bdbac1bfbdbfbbb8c1bdb9bebab6c0bdb9bfbbb8c1bdbab4b2b0b7b6b4b7b6b3b4b2b0bab9b7b6b5b2b6b5b2bab9b6bab9b6958c8797",
    "7663aa836bac8772b08f7aad8c77b2917db0917db0907cac8971a77d64a87f67ac8972b29887b8a89dbfbab5bfbdbac1bebac0bcb9c0bcb9c0bcb9c1bebabeba",
    "b7b8b7b4b7b6b4b5b4b1b5b4b2b7b6b3b5b4b2bab9b7bab9b6b4b1ada88f7fad8973ae8d78b19684b19685b29786b69a89b29582b1917daa856ea87e66a97e",
    "66ad866ea9826baf9280b8ada6bdbbb8bebab7bfbbb8c1bdbabfbbb8bcb8b4bcb8b5b6b4b2b7b5b3b6b5b2b8b7b4b3b2afb8b7b4b6b5b2b3b2b0b3a59aab856f",
    "ad8d78b0917eb19886b49b8bb49a89b39785b0917eaf8f7cab866fa77d65a77a61a87d64a9816ab08f79b5a296c1bcb8c3bfbcc2bebbbebab7bfbbb7bdbab6c2",
    "bebab8b7b4b7b6b4b6b5b3b7b6b3b6b5b2b9b8b6b4b3b1b6b1acac8f7ca9826bae8f7aaf9583b49c8cb49c8bb79d8cb59987b19380ad8e79ae8c77af8e78ac87",
    "71a3775faa826bae8972b39888bbb6b2bebbb8bfbbb8bfbbb8c0bdb9bebbb7c0bdb9b6b5b2b9b8b5b4b3b1b8b7b5b4b3b0b7b6b4b6b5b3b1a7a0aa8772a77d65",
    "a88570b49887b19b8d9c887c907a6d987f71aa907faf917daf8e7aad8c78ac8b77a8836ca9836cac8770b49b8abdb6b2c0bcb9c0bdb9bfbbb8bebab7bfbcb9be",
    "bab7b9b8b6b5b4b2b9b8b5b8b7b5b8b7b4b7b6b4b5b4b2b3a9a2ad8973a1755da9856fb398858c776a65544b776358725d526e594d9c7f6eb1907ba68672ad8e",
    "7aab8771ac856db18f79b3a092beb9b5c1bdbabdb9b5bebab7bfbbb7bebab7bcb9b6b7b6b4b6b6b3b8b7b4b5b4b2b8b6b4b7b6b3b4b3b0b4aba4a6826ba3775f",
    "b08e79b19584a88e7daa8e7db29481ad8f7c997e6da38674ac8d79ac8e7aae917f9a7c6a896a599a7c6ab3a398c1bdbabdb9b6bcb8b5bebab6bebab7bdb9b5bd",
    "b9b6b5b4b1b7b5b3b5b4b2b7b6b3b7b6b4b3b3b0b3b2b0b4aca5a7846fa97f68ae8f7bae9383b59c8bb2937fae8e79ac8b76af927eaf927eb29683b39885b2",
    "988891786a72594c6e594d978d86bdbab7bab7b3c0bcb9c0bcb9bebab7bebbb7bdb9b6b3b2b0b4b3b0b5b4b2b4b4b1b4b3b1b4b3b1b4b3b0b6ada5aa8670a5",
    "7a62ad8e7ab29b8cb69d8dab856fa9826aa88069ab8771af907db49987b19684b29886b59987b39480b09787b5a9a1bcb8b5bebab7bdb9b5bebab7bfbbb8bfbb",
    "b7bbb7b4b3b2afb8b7b5b8b7b5b3b2b0b5b4b2b6b5b3b6b4b1afa299a98975a9826baf907cb39988b49a89af8e7aac8973aa856eaf8c74b1917dae907dac90",
    "7db39988b29785b49785b7a090b9aca3bfbab7bcb8b5bdb9b6bcb8b4bcb8b5bdb9b5bcb8b4b5b4b2b6b5b3b4b3b0b4b3b0b9b8b5b8b6b4908b88887467aa8f",
    "7ea78976ad8973b08b74b59885b69e8eb29888b1917cb1917db1937fae907cb19686b39a8ab29886b59b8ab8a192b6aaa3b7b2afbcb8b4bcb8b5bbb7b4c0bc",
    "b9bebab7c0bcb9b6b5b2b6b5b3b4b3b0bab9b7b7b6b4b1b0ae7b716ba083709b806f716158967764b08870b29481b69b8ab69f8fb39a89b69f90b49d8db39a89",
    "b29988b49c8cb6a090b8a496baa49593867f8f8986bfbbb7bdb9b5bcb7b4bab6b3b9b5b2bab6b2b4b3b1b3b3b0b6b5b3b8b7b5b4b2b0a7a5a38f837dae917ea0",
    "84725a504c63544da28370b39784b59e8db2a093a698909b918b998e8790857e95877dad998bb39c8cb5a091b9a2938d827c95908dbebab6bbb7b3bdbab7bbb7",
    "b4bdb9b6bbb7b4b4b3b0b5b4b1b8b7b5b6b5b3b8b8b5b4b2af968f8ab29a8bab9485544b483a323073655d96887f70655f61595547403e453e3c453f3d57504f",
    "655e5b90847db39c8db7a090b6a09189807aaba6a3bdb9b6c0bcb9bebab7bcb7b4bebab7bbb7b4b3b2b0b6b5b3b2b1afb7b6b4b8b7b4b5b4b1aeaba8b5a89fac",
    "998d4d44412d25244d46444e4744322b293a3230423937433a37352d2a59504c534b48524a48988a81b59f8fb19c8d827974b2afacbdb9b5bcb8b4bdb9b5bcb8",
    "b5bdb9b6bab6b2b8b7b5b5b4b2b6b6b3b9b8b5b7b6b3b6b5b2b8b6b3b9b4b1b2a9a26c64612d25242d2625312a28352d2c453d3a78675c8d7a6ea09792aea6a0",
    "615854332b29524a479f8e82b09d90a49b96c1bdb9bebab7bfbbb8bbb8b4b9b5b1b8b4b0b9b4b0b7b6b4b8b7b5b8b7b4b6b5b3b8b6b3bab9b6b9b8b5b4b3b0b7",
    "b5b2a5a29f453d3b261e1d261f1e2e2625413936857268977865b19482b5a69caca5a07c7572453d3b746963a0948cc5bfbbc0bbb8beb9b6bbb7b3bbb6b3b7b3",
    "afb8b4b0b9b5b1b7b6b3b6b5b3b5b4b2b5b4b2b7b6b3b7b6b3b8b6b3b4b2afb7b6b3b3b1ae6d6765251f1e1e18172a22212d2523443b3971625ab19888b09482",
    "a89182877e792c25243e3634766d6abeb9b5bfbbb7bebab6bcb7b3bbb6b3b9b5b1b7b3afb8b4b0b4b3b0b5b4b1b5b4b1b4b3b1b5b4b2b8b6b4b5b3b0b9b6b4b5",
    "b4b1b6b4b27f79762a2322221c1b2d2524221b1a443e3c47413f6f676281766f867971675e5a3e37352a222166605dbab7b3bdb9b5beb9b5bcb7b3bcb7b3b9b4",
    "b0bab6b2bab6b2b5b3b0b6b4b2b3b2afb7b6b3b4b4b1b4b3b0b6b4b1b5b4b1b4b3b0b9b6b29a8c8252474230292828201f181212322c2c231e1d1c16162c2625",
    "2923222d26252d2523332b2a8e8885bcb8b5bcb7b3bbb6b2bcb7b3b9b4b1b9b5b1b7b2afb7b2ae7a838e9b9b9caeadacb3b2b0b3b2afb7b7b4b6b5b3b6b6b3b7",
    "b6b3b9ada4a991808e7b6f50453f2b24231a14142923221f19181d17161f18182620201d17162a22215d5654b7b3b0bbb7b3bbb6b2b8b4b0bab5b1bbb6b2bab5",
    "b1b8b4b0bab6b22c496b4c5d735f68766e727a828285929090adaba8b7b2aeb6a59ab39682a28470a387748e76674e403a1a14141d1716181211221c1c1f1918",
    "221c1b2f2827342d2c8d8884bab6b3b9b5b2bab5b1bab5b1b9b4b0bab6b2b8b4b0b9b4b0b7b2ae325e8b365f8a3a5d833f5b7a545f70646469706b6aa08f84b0",
    "8e78b18e769f7e689e7f6b9e816d907766584940362d2a1c1615201b1a1a1413201a1a251e1d393331a39e9bbab5b1bcb7b3bab6b2b8b3afb8b4b0b9b4b0b9b4",
    "b1bab5b2b5b0ac3d6c9843729d44719c426e98415f805a64716f6a699d8677b1927eb3947faa89749d7a649f7f6ba487749e837186716454463f2c25231e1818",
    "37302e3a33317a7471beb9b6bcb8b4bbb6b2b6b2aebab5b1b9b5b1b8b3afbab6b2b6b1adb5aeaa4877a14c7aa44e7ba345719a3a5d80586b7f767475927b6eb1",
    "927faf8e79b08e78a78169a07861a17f6aa58570a688749b83738270666f66618a8480a49e99b7b2aebab6b2bcb8b4b9b5b1b7b2aebab5b1b9b4b0b6b1aeb6b1",
    "adb2aca8b2aca84876a04a78a2517fa74771973a5d80405c7a6161677c695fac8a75b08d77b4917aaf8971ad876fa5816aa6846ea78670a98a76ac9484ab9f96",
    "b2aca8bdb8b4bcb7b3bcb8b4bcb8b4b8b3afb7b2aeb9b4b0b8b3afb8b2aeb6afabb3aeaab2aeaa4878a14b7aa34c7ba44a759b3d63873b5f825b67766f5f569c",
    "7e6caf8c77b18f79b28f78b5927caf8e78a98872aa8a76a98a76ac917fada199b7b0acb9b3afbfb9b5c1bab6bdb6b2b8b3afbab5b1b9b4b0b6afabb7b1adb3ad",
    "a9b3aeaab0aba8"
);

const WIDTH: usize = 32;
const HEIGHT: usize = 32;
const MAX_TICKS: usize = 25; // ~2 seconds at 80ms
const RESET: &str = "\x1b[0m";

static SGR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*m").expect("SGR_RE compiles"));

fn parse_image() -> Vec<Vec<[u8; 3]>> {
    let mut pixels = Vec::with_capacity(HEIGHT);
    for y in 0..HEIGHT {
        let mut row = Vec::with_capacity(WIDTH);
        for x in 0..WIDTH {
            let idx = (y * WIDTH + x) * 6;
            let r = u8::from_str_radix(&DAX_HEX[idx..idx + 2], 16).unwrap_or(0);
            let g = u8::from_str_radix(&DAX_HEX[idx + 2..idx + 4], 16).unwrap_or(0);
            let b = u8::from_str_radix(&DAX_HEX[idx + 4..idx + 6], 16).unwrap_or(0);
            row.push([r, g, b]);
        }
        pixels.push(row);
    }
    pixels
}

fn rgb(r: u8, g: u8, b: u8, bg: bool) -> String {
    format!("\x1b[{};2;{r};{g};{b}m", if bg { 48 } else { 38 })
}

fn build_image() -> Vec<String> {
    let pixels = parse_image();
    let mut lines = Vec::with_capacity(HEIGHT / 2);

    // Use half-block chars: ▄ with bg=top pixel, fg=bottom pixel
    for row in (0..HEIGHT).step_by(2) {
        let mut line = String::new();
        for x in 0..WIDTH {
            let top = pixels[row][x];
            let bottom = pixels.get(row + 1).map_or(top, |r| r[x]);
            line.push_str(&rgb(bottom[0], bottom[1], bottom[2], false));
            line.push_str(&rgb(top[0], top[1], top[2], true));
            line.push('▄');
        }
        line.push_str(RESET);
        lines.push(line);
    }
    lines
}

struct DaxnutsState {
    interval: Option<Interval>,
    tick: usize,
    cached_lines: Vec<String>,
    cached_width: Option<usize>,
    cached_tick: Option<usize>,
}

struct DaxnutsInner {
    ui: Arc<dyn TUI + Send + Sync>,
    image: Vec<String>,
    max_ticks: usize,
    state: Mutex<DaxnutsState>,
}

impl Drop for DaxnutsInner {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock()
            && let Some(interval) = state.interval.take()
        {
            interval.clear();
        }
    }
}

#[derive(Clone)]
pub struct DaxnutsComponent {
    inner: Arc<DaxnutsInner>,
}

impl DaxnutsComponent {
    pub fn new(ui: Arc<dyn TUI + Send + Sync>) -> Self {
        let inner = Arc::new(DaxnutsInner {
            ui,
            image: build_image(),
            max_ticks: MAX_TICKS,
            state: Mutex::new(DaxnutsState {
                interval: None,
                tick: 0,
                cached_lines: Vec::new(),
                cached_width: None,
                cached_tick: None,
            }),
        });

        let component = Self { inner };
        component.start_animation();
        component
    }

    pub fn invalidate(&self) {
        self.inner.state.lock().unwrap().cached_width = None;
    }

    fn start_animation(&self) {
        let weak = Arc::downgrade(&self.inner);
        let interval = set_interval(80, move || {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let finished_interval = {
                let mut state = inner.state.lock().unwrap();
                state.tick += 1;
                state.cached_width = None;
                if state.tick >= inner.max_ticks {
                    state.interval.take()
                } else {
                    None
                }
            };
            if let Some(interval) = finished_interval {
                interval.clear();
            }
            inner.ui.request_render(None);
        });
        self.inner.state.lock().unwrap().interval = Some(interval);
    }

    fn stop_animation(&self) {
        let interval = self.inner.state.lock().unwrap().interval.take();
        if let Some(interval) = interval {
            interval.clear();
        }
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let mut state = self.inner.state.lock().unwrap();
        if state.cached_width == Some(width) && state.cached_tick == Some(state.tick) {
            return state.cached_lines.clone();
        }

        let t = theme();
        let mut lines = Vec::new();
        let tick = state.tick;
        let max_ticks = self.inner.max_ticks;

        let center = |s: &str| -> String {
            let stripped = SGR_RE.replace_all(s, "");
            let visible = pi_js::str16::len(&stripped);
            let left = width.saturating_sub(visible) / 2;
            format!("{}{s}", " ".repeat(left))
        };

        lines.push(String::new());

        // Scanline reveal effect: show rows progressively
        let image_len = self.inner.image.len();
        let revealed_rows =
            ((((tick as f64) / (max_ticks as f64)) * ((image_len + 3) as f64)).floor() as usize).min(image_len);

        for i in 0..image_len {
            if i < revealed_rows {
                lines.push(center(&self.inner.image[i]));
            } else if i == revealed_rows {
                // Show scan line
                let scanline = "▓".repeat(WIDTH);
                lines.push(center(&format!("{}{scanline}{RESET}", rgb(100, 200, 255, false))));
            } else {
                lines.push(center(&" ".repeat(WIDTH)));
            }
        }

        lines.push(String::new());

        // Fade in text after image is revealed
        let text_phase = ((tick as f64) - (max_ticks as f64) * 0.6).max(0.0);
        if text_phase > 0.0 || tick >= max_ticks {
            lines.push(center(&t.fg(ThemeColor::Accent, "Free Kimi K2.5 via OpenCode Zen")));
            lines.push(center(&t.fg(ThemeColor::Success, "\"Powered by daxnuts\"")));
            lines.push(center(&t.fg(ThemeColor::Muted, "— @thdxr")));
        } else {
            lines.push(String::new());
            lines.push(String::new());
            lines.push(String::new());
        }

        lines.push(String::new());
        if text_phase > 2.0 || tick >= max_ticks {
            lines.push(center(&t.fg(ThemeColor::Dim, "Try OpenCode")));
            lines.push(center(
                &t.fg(ThemeColor::MdLink, "https://mistral.ai/news/mistral-vibe-2-0"),
            ));
        } else {
            lines.push(String::new());
            lines.push(String::new());
        }
        lines.push(String::new());

        state.cached_lines = lines.clone();
        state.cached_width = Some(width);
        state.cached_tick = Some(tick);
        lines
    }

    pub fn dispose(&self) {
        self.stop_animation();
    }
}

impl Component for DaxnutsComponent {
    fn render(&self, width: usize) -> Vec<String> {
        DaxnutsComponent::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        DaxnutsComponent::invalidate(self);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use async_trait::async_trait;
    use pi_tui::strip_terminal_sequences;
    use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
    use pi_tui::tui_main_screen::TuiMainScreen;

    use super::*;

    struct TestTerminal {
        on_input: Mutex<Option<TerminalInputHandler>>,
    }

    impl TestTerminal {
        fn new() -> Self {
            Self {
                on_input: Mutex::new(None),
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
        fn write(&self, _data: &str) {}
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
    fn dax_hex_and_image_dimensions() {
        assert_eq!(DAX_HEX.len(), WIDTH * HEIGHT * 6);
        let pixels = parse_image();
        assert_eq!(pixels.len(), HEIGHT);
        assert_eq!(pixels[0].len(), WIDTH);
        assert_eq!(pixels[0][0], [0xbb, 0xba, 0xb8]);

        let image = build_image();
        assert_eq!(image.len(), HEIGHT / 2);
        assert!(image[0].ends_with(RESET));
    }

    #[tokio::test(start_paused = true)]
    async fn daxnuts_component_reveals_scanlines_and_credits() {
        let term = Arc::new(TestTerminal::new());
        let tui: Arc<dyn TUI + Send + Sync> = Arc::new(TuiMainScreen::new(term, None, None));
        let comp = DaxnutsComponent::new(tui);

        let initial = comp.render(80);
        assert_eq!(initial.len(), 25);
        assert!(strip_terminal_sequences(&initial[1]).contains("▓▓▓▓"));
        assert_eq!(initial[18], "");

        tokio::time::advance(Duration::from_millis(80 * 25 + 10)).await;
        tokio::task::yield_now().await;

        let finished = comp.render(80);
        assert_eq!(finished.len(), 25);
        assert!(strip_terminal_sequences(&finished[1]).contains("▄▄▄▄"));
        assert!(strip_terminal_sequences(&finished[18]).contains("Free Kimi K2.5 via OpenCode Zen"));
        assert!(strip_terminal_sequences(&finished[19]).contains("\"Powered by daxnuts\""));
        assert!(strip_terminal_sequences(&finished[20]).contains("— @thdxr"));
        assert!(strip_terminal_sequences(&finished[22]).contains("Try OpenCode"));
        assert!(strip_terminal_sequences(&finished[23]).contains("https://mistral.ai/news/mistral-vibe-2-0"));

        comp.dispose();
    }
}
