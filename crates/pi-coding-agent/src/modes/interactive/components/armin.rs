//! Port of packages/coding-agent/src/modes/interactive/components/armin.ts
//!
//! Armin says hi! A fun easter egg with animated XBM art.

use std::sync::{Arc, Mutex};

use pi_js::time::{Interval, set_interval};
use pi_tui::{Component, TUI, TuiMouseEvent, TuiMouseEventResult};

use super::super::theme::theme::{ThemeColor, theme};

// XBM image: 31x36 pixels, LSB first, 1=background, 0=foreground
pub const ARMIN_WIDTH: usize = 31;
pub const ARMIN_HEIGHT: usize = 36;
const WIDTH: usize = ARMIN_WIDTH;
const HEIGHT: usize = ARMIN_HEIGHT;
const BITS: [u8; 144] = [
    0xff, 0xff, 0xff, 0x7f, 0xff, 0xf0, 0xff, 0x7f, 0xff, 0xed, 0xff, 0x7f, 0xff, 0xdb, 0xff, 0x7f, 0xff, 0xb7, 0xff,
    0x7f, 0xff, 0x77, 0xfe, 0x7f, 0x3f, 0xf8, 0xfe, 0x7f, 0xdf, 0xff, 0xfe, 0x7f, 0xdf, 0x3f, 0xfc, 0x7f, 0x9f, 0xc3,
    0xfb, 0x7f, 0x6f, 0xfc, 0xf4, 0x7f, 0xf7, 0x0f, 0xf7, 0x7f, 0xf7, 0xff, 0xf7, 0x7f, 0xf7, 0xff, 0xe3, 0x7f, 0xf7,
    0x07, 0xe8, 0x7f, 0xef, 0xf8, 0x67, 0x70, 0x0f, 0xff, 0xbb, 0x6f, 0xf1, 0x00, 0xd0, 0x5b, 0xfd, 0x3f, 0xec, 0x53,
    0xc1, 0xff, 0xef, 0x57, 0x9f, 0xfd, 0xee, 0x5f, 0x9f, 0xfc, 0xae, 0x5f, 0x1f, 0x78, 0xac, 0x5f, 0x3f, 0x00, 0x50,
    0x6c, 0x7f, 0x00, 0xdc, 0x77, 0xff, 0xc0, 0x3f, 0x78, 0xff, 0x01, 0xf8, 0x7f, 0xff, 0x03, 0x9c, 0x78, 0xff, 0x07,
    0x8c, 0x7c, 0xff, 0x0f, 0xce, 0x78, 0xff, 0xff, 0xcf, 0x7f, 0xff, 0xff, 0xcf, 0x78, 0xff, 0xff, 0xdf, 0x78, 0xff,
    0xff, 0xdf, 0x7d, 0xff, 0xff, 0x3f, 0x7e, 0xff, 0xff, 0xff, 0x7f,
];

const BYTES_PER_ROW: usize = WIDTH.div_ceil(8);
const DISPLAY_HEIGHT: usize = HEIGHT.div_ceil(2); // Half-block rendering

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Effect {
    Typewriter,
    Scanline,
    Rain,
    Fade,
    Crt,
    Glitch,
    Dissolve,
}

const EFFECTS: [Effect; 7] = [
    Effect::Typewriter,
    Effect::Scanline,
    Effect::Rain,
    Effect::Fade,
    Effect::Crt,
    Effect::Glitch,
    Effect::Dissolve,
];

const DISSOLVE_CHARS: [char; 7] = [' ', '░', '▒', '▓', '█', '▀', '▄'];

fn random_index(len: usize) -> usize {
    if len <= 1 {
        return 0;
    }
    ((rand::random::<f64>() * (len as f64)).floor() as usize).min(len - 1)
}

/// Get pixel at (x, y): true = foreground, false = background.
pub fn is_armin_pixel(x: usize, y: usize) -> bool {
    if x >= WIDTH || y >= HEIGHT {
        return false;
    }
    let byte_index = y * BYTES_PER_ROW + (x / 8);
    let bit_index = x % 8;
    ((BITS[byte_index] >> bit_index) & 1) == 0
}

/// Get the character for a cell (2 vertical pixels packed).
fn get_char(x: usize, row: usize) -> char {
    let upper = is_armin_pixel(x, row * 2);
    let lower = is_armin_pixel(x, row * 2 + 1);
    if upper && lower {
        '█'
    } else if upper {
        '▀'
    } else if lower {
        '▄'
    } else {
        ' '
    }
}

/// Build the final image grid.
fn build_final_grid() -> Vec<Vec<char>> {
    let mut grid = Vec::with_capacity(DISPLAY_HEIGHT);
    for row in 0..DISPLAY_HEIGHT {
        let mut line = Vec::with_capacity(WIDTH);
        for x in 0..WIDTH {
            line.push(get_char(x, row));
        }
        grid.push(line);
    }
    grid
}

fn create_empty_grid() -> Vec<Vec<char>> {
    vec![vec![' '; WIDTH]; DISPLAY_HEIGHT]
}

fn shuffled_positions() -> Vec<(usize, usize)> {
    let mut positions = Vec::with_capacity(DISPLAY_HEIGHT * WIDTH);
    for row in 0..DISPLAY_HEIGHT {
        for x in 0..WIDTH {
            positions.push((row, x));
        }
    }
    // Fisher-Yates shuffle
    for i in (1..positions.len()).rev() {
        let j = random_index(i + 1);
        positions.swap(i, j);
    }
    positions
}

#[derive(Clone, Copy)]
struct RainDrop {
    y: isize,
    settled: usize,
}

enum EffectState {
    Typewriter { pos: usize },
    Scanline { row: usize },
    Rain { drops: Vec<RainDrop> },
    Fade { positions: Vec<(usize, usize)>, idx: usize },
    Crt { expansion: usize },
    Glitch { phase: usize, glitch_frames: usize },
    Dissolve { positions: Vec<(usize, usize)>, idx: usize },
}

struct ArminState {
    interval: Option<Interval>,
    current_grid: Vec<Vec<char>>,
    effect_state: EffectState,
    cached_lines: Vec<String>,
    cached_width: Option<usize>,
    grid_version: u64,
    cached_version: Option<u64>,
}

struct ArminInner {
    ui: Arc<dyn TUI + Send + Sync>,
    effect: Effect,
    final_grid: Vec<Vec<char>>,
    state: Mutex<ArminState>,
}

impl Drop for ArminInner {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock()
            && let Some(interval) = state.interval.take()
        {
            interval.clear();
        }
    }
}

#[derive(Clone)]
pub struct ArminComponent {
    inner: Arc<ArminInner>,
}

impl ArminComponent {
    pub fn new(ui: Arc<dyn TUI + Send + Sync>) -> Self {
        let effect = EFFECTS[random_index(EFFECTS.len())];
        let final_grid = build_final_grid();
        let (current_grid, effect_state) = init_effect(effect);

        let inner = Arc::new(ArminInner {
            ui,
            effect,
            final_grid,
            state: Mutex::new(ArminState {
                interval: None,
                current_grid,
                effect_state,
                cached_lines: Vec::new(),
                cached_width: None,
                grid_version: 0,
                cached_version: None,
            }),
        });

        let component = Self { inner };
        component.start_animation();
        component
    }

    pub fn invalidate(&self) {
        self.inner.state.lock().unwrap().cached_width = None;
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let mut state = self.inner.state.lock().unwrap();
        if state.cached_width == Some(width) && state.cached_version == Some(state.grid_version) {
            return state.cached_lines.clone();
        }

        let t = theme();
        let padding = 1usize;
        let available_width = width.saturating_sub(padding);

        let mut lines = Vec::with_capacity(state.current_grid.len() + 1);
        for row in &state.current_grid {
            // Clip row to available width before applying color
            let clipped: String = row.iter().take(available_width).copied().collect();
            let clipped_len = clipped.chars().count();
            let pad_right = width.saturating_sub(padding).saturating_sub(clipped_len);
            lines.push(format!(
                " {}{}",
                t.fg(ThemeColor::Accent, &clipped),
                " ".repeat(pad_right)
            ));
        }

        // Add "ARMIN SAYS HI" at the end
        let message = "ARMIN SAYS HI";
        let msg_pad_right = width.saturating_sub(padding).saturating_sub(message.len());
        lines.push(format!(
            " {}{}",
            t.fg(ThemeColor::Accent, message),
            " ".repeat(msg_pad_right)
        ));

        state.cached_lines = lines.clone();
        state.cached_width = Some(width);
        state.cached_version = Some(state.grid_version);

        lines
    }

    fn start_animation(&self) {
        let fps: u64 = if self.inner.effect == Effect::Glitch { 60 } else { 30 };
        let weak = Arc::downgrade(&self.inner);
        let interval = set_interval(1000 / fps, move || {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let finished_interval = {
                let mut state = inner.state.lock().unwrap();
                let done = tick_effect(&inner.final_grid, &mut state);
                state.grid_version += 1;
                if done { state.interval.take() } else { None }
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

    pub fn dispose(&self) {
        self.stop_animation();
    }
}

impl Component for ArminComponent {
    fn render(&self, width: usize) -> Vec<String> {
        ArminComponent::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        ArminComponent::invalidate(self);
    }
}

fn init_effect(effect: Effect) -> (Vec<Vec<char>>, EffectState) {
    match effect {
        Effect::Typewriter => (create_empty_grid(), EffectState::Typewriter { pos: 0 }),
        Effect::Scanline => (create_empty_grid(), EffectState::Scanline { row: 0 }),
        Effect::Rain => {
            // Track falling position for each column
            let drops = (0..WIDTH)
                .map(|_| RainDrop {
                    y: -(random_index(DISPLAY_HEIGHT * 2) as isize),
                    settled: 0,
                })
                .collect();
            (create_empty_grid(), EffectState::Rain { drops })
        }
        Effect::Fade => (
            create_empty_grid(),
            EffectState::Fade {
                positions: shuffled_positions(),
                idx: 0,
            },
        ),
        Effect::Crt => (create_empty_grid(), EffectState::Crt { expansion: 0 }),
        Effect::Glitch => (
            create_empty_grid(),
            EffectState::Glitch {
                phase: 0,
                glitch_frames: 8,
            },
        ),
        Effect::Dissolve => {
            // Start with random noise
            let mut grid = Vec::with_capacity(DISPLAY_HEIGHT);
            for _ in 0..DISPLAY_HEIGHT {
                let mut line = Vec::with_capacity(WIDTH);
                for _ in 0..WIDTH {
                    line.push(DISSOLVE_CHARS[random_index(DISSOLVE_CHARS.len())]);
                }
                grid.push(line);
            }
            (
                grid,
                EffectState::Dissolve {
                    positions: shuffled_positions(),
                    idx: 0,
                },
            )
        }
    }
}

fn tick_effect(final_grid: &[Vec<char>], state: &mut ArminState) -> bool {
    match &mut state.effect_state {
        EffectState::Typewriter { pos } => tick_typewriter(final_grid, &mut state.current_grid, pos),
        EffectState::Scanline { row } => tick_scanline(final_grid, &mut state.current_grid, row),
        EffectState::Rain { drops } => tick_rain(final_grid, &mut state.current_grid, drops),
        EffectState::Fade { positions, idx } => tick_fade(final_grid, &mut state.current_grid, positions, idx),
        EffectState::Crt { expansion } => tick_crt(final_grid, &mut state.current_grid, expansion),
        EffectState::Glitch { phase, glitch_frames } => {
            tick_glitch(final_grid, &mut state.current_grid, phase, *glitch_frames)
        }
        EffectState::Dissolve { positions, idx } => tick_dissolve(final_grid, &mut state.current_grid, positions, idx),
    }
}

fn tick_typewriter(final_grid: &[Vec<char>], current_grid: &mut [Vec<char>], pos: &mut usize) -> bool {
    let pixels_per_frame = 3;

    for _ in 0..pixels_per_frame {
        let row = *pos / WIDTH;
        let x = *pos % WIDTH;
        if row >= DISPLAY_HEIGHT {
            return true;
        }
        current_grid[row][x] = final_grid[row][x];
        *pos += 1;
    }
    false
}

fn tick_scanline(final_grid: &[Vec<char>], current_grid: &mut [Vec<char>], row: &mut usize) -> bool {
    if *row >= DISPLAY_HEIGHT {
        return true;
    }

    // Copy row
    current_grid[*row][..WIDTH].copy_from_slice(&final_grid[*row][..WIDTH]);
    *row += 1;
    false
}

fn tick_rain(final_grid: &[Vec<char>], current_grid: &mut Vec<Vec<char>>, drops: &mut [RainDrop]) -> bool {
    let mut all_settled = true;
    *current_grid = create_empty_grid();

    for (x, drop) in drops.iter_mut().enumerate() {
        // Draw settled pixels
        let settled_start = DISPLAY_HEIGHT.saturating_sub(drop.settled);
        for row in settled_start..DISPLAY_HEIGHT {
            current_grid[row][x] = final_grid[row][x];
        }

        // Check if this column is done
        if drop.settled >= DISPLAY_HEIGHT {
            continue;
        }

        all_settled = false;

        // Find the target row for this column (lowest non-space pixel)
        let mut target_row: isize = -1;
        let search_top = (DISPLAY_HEIGHT as isize) - 1 - (drop.settled as isize);
        let mut row = search_top;
        while row >= 0 {
            if final_grid[row as usize][x] != ' ' {
                target_row = row;
                break;
            }
            row -= 1;
        }

        // Move drop down
        drop.y += 1;

        // Draw falling drop
        if drop.y >= 0 && (drop.y as usize) < DISPLAY_HEIGHT {
            if target_row >= 0 && drop.y >= target_row {
                // Settle
                drop.settled = DISPLAY_HEIGHT - (target_row as usize);
                drop.y = -(random_index(5) as isize) - 1;
            } else {
                // Still falling
                current_grid[drop.y as usize][x] = '▓';
            }
        }
    }

    all_settled
}

fn tick_fade(
    final_grid: &[Vec<char>],
    current_grid: &mut [Vec<char>],
    positions: &[(usize, usize)],
    idx: &mut usize,
) -> bool {
    let pixels_per_frame = 15;

    for _ in 0..pixels_per_frame {
        if *idx >= positions.len() {
            return true;
        }
        let (row, x) = positions[*idx];
        current_grid[row][x] = final_grid[row][x];
        *idx += 1;
    }
    false
}

fn tick_crt(final_grid: &[Vec<char>], current_grid: &mut Vec<Vec<char>>, expansion: &mut usize) -> bool {
    let mid_row = DISPLAY_HEIGHT / 2;

    *current_grid = create_empty_grid();

    // Draw from middle expanding outward
    let top = mid_row.saturating_sub(*expansion);
    let bottom = (mid_row + *expansion).min(DISPLAY_HEIGHT - 1);

    for row in top..=bottom {
        current_grid[row][..WIDTH].copy_from_slice(&final_grid[row][..WIDTH]);
    }

    *expansion += 1;
    *expansion > DISPLAY_HEIGHT
}

fn tick_glitch(
    final_grid: &[Vec<char>],
    current_grid: &mut Vec<Vec<char>>,
    phase: &mut usize,
    glitch_frames: usize,
) -> bool {
    if *phase < glitch_frames {
        // Glitch phase: show corrupted version
        *current_grid = final_grid
            .iter()
            .map(|row| {
                let offset = (random_index(7) as isize) - 3;
                let mut glitch_row = row.clone();

                // Random horizontal offset
                if rand::random::<f64>() < 0.3 {
                    let shift = offset.rem_euclid(WIDTH as isize) as usize;
                    glitch_row.rotate_left(shift);
                    return glitch_row;
                }

                // Random vertical swap
                if rand::random::<f64>() < 0.2 {
                    let swap_row = random_index(DISPLAY_HEIGHT);
                    return final_grid[swap_row].clone();
                }

                glitch_row
            })
            .collect();
        *phase += 1;
        return false;
    }

    // Final frame: show clean image
    *current_grid = final_grid.to_vec();
    true
}

fn tick_dissolve(
    final_grid: &[Vec<char>],
    current_grid: &mut [Vec<char>],
    positions: &[(usize, usize)],
    idx: &mut usize,
) -> bool {
    let pixels_per_frame = 20;

    for _ in 0..pixels_per_frame {
        if *idx >= positions.len() {
            return true;
        }
        let (row, x) = positions[*idx];
        current_grid[row][x] = final_grid[row][x];
        *idx += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

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
    fn armin_bitmap_dimensions_and_pixels() {
        assert_eq!(ARMIN_WIDTH, 31);
        assert_eq!(ARMIN_HEIGHT, 36);
        assert_eq!(BYTES_PER_ROW, 4);
        assert_eq!(DISPLAY_HEIGHT, 18);
        assert!(!is_armin_pixel(0, 0));
        assert!(!is_armin_pixel(31, 0));
        assert!(!is_armin_pixel(0, 36));
        // Row 1 (0xff, 0xf0, 0xff, 0x7f) has pixels 8..12 lit (bits 0..3 of byte 1 are 0).
        assert!(is_armin_pixel(8, 1));
        assert!(is_armin_pixel(11, 1));
        assert!(!is_armin_pixel(12, 1));

        let final_grid = build_final_grid();
        assert_eq!(final_grid.len(), DISPLAY_HEIGHT);
        assert_eq!(final_grid[0].len(), WIDTH);
    }

    #[test]
    fn crt_and_scanline_resolve_to_final_grid() {
        let final_grid = build_final_grid();

        let (mut grid, mut state) = init_effect(Effect::Scanline);
        let mut st = ArminState {
            interval: None,
            current_grid: grid.clone(),
            effect_state: state,
            cached_lines: Vec::new(),
            cached_width: None,
            grid_version: 0,
            cached_version: None,
        };
        let mut steps = 0;
        while !tick_effect(&final_grid, &mut st) {
            steps += 1;
            assert!(steps <= DISPLAY_HEIGHT + 1);
        }
        assert_eq!(st.current_grid, final_grid);

        (grid, state) = init_effect(Effect::Crt);
        st.current_grid = grid;
        st.effect_state = state;
        steps = 0;
        while !tick_effect(&final_grid, &mut st) {
            steps += 1;
            assert!(steps <= DISPLAY_HEIGHT + 2);
        }
        assert_eq!(st.current_grid, final_grid);
    }

    #[tokio::test(start_paused = true)]
    async fn armin_component_renders_caches_and_disposes() {
        let term = Arc::new(TestTerminal::new());
        let tui: Arc<dyn TUI + Send + Sync> = Arc::new(TuiMainScreen::new(term, None, None));
        let comp = ArminComponent::new(tui);

        let lines = comp.render(40);
        assert_eq!(lines.len(), DISPLAY_HEIGHT + 1);
        assert_eq!(strip_terminal_sequences(&lines[DISPLAY_HEIGHT]).trim(), "ARMIN SAYS HI");
        assert_eq!(lines, comp.render(40));

        let narrow = comp.render(10);
        assert_eq!(narrow.len(), DISPLAY_HEIGHT + 1);
        assert_eq!(strip_terminal_sequences(&narrow[0]).chars().count(), 10);

        comp.invalidate();
        assert_eq!(comp.render(40).len(), DISPLAY_HEIGHT + 1);
        comp.dispose();
    }
}
