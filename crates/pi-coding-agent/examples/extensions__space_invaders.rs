//! Port of packages/coding-agent/examples/extensions/space-invaders.ts

use std::process::ExitCode;
use std::sync::{Arc, LazyLock, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, ExtensionMode, InlineExtension, NotifyType, RegisteredCommandOptions,
};
use pi_tui::{Component, Key, TUI, is_key_release, matches_key, visible_width};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const GAME_WIDTH: i64 = 60;
const GAME_HEIGHT: i64 = 24;
const TICK_MS: u64 = 50;
const PLAYER_Y: i64 = GAME_HEIGHT - 2;
const ALIEN_ROWS: usize = 5;
const ALIEN_COLS: usize = 11;
const ALIEN_START_Y: i64 = 2;
const INVADERS_SAVE_TYPE: &str = "space-invaders-save";

static ANSI_SGR_RE: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*m").ok());

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct Bullet {
    x: i64,
    y: i64,
    direction: i64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct Alien {
    x: i64,
    y: i64,
    #[serde(rename = "type")]
    kind: usize,
    alive: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Shield {
    x: i64,
    segments: Vec<Vec<bool>>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct Player {
    x: i64,
    lives: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GameState {
    player: Player,
    aliens: Vec<Alien>,
    alien_direction: i64,
    alien_move_counter: usize,
    alien_move_delay: usize,
    alien_dropping: bool,
    bullets: Vec<Bullet>,
    shields: Vec<Shield>,
    score: u64,
    high_score: u64,
    level: u64,
    game_over: bool,
    victory: bool,
    alien_shoot_counter: usize,
}

#[derive(Default)]
struct KeyState {
    left: bool,
    right: bool,
    fire: bool,
}

fn create_shields() -> Vec<Shield> {
    [8, 22, 36, 50]
        .into_iter()
        .map(|x| Shield {
            x,
            segments: vec![
                vec![true, true, true, true],
                vec![true, true, true, true],
                vec![true, false, false, true],
            ],
        })
        .collect()
}

fn create_aliens() -> Vec<Alien> {
    let mut aliens = Vec::with_capacity(ALIEN_ROWS * ALIEN_COLS);
    for row in 0..ALIEN_ROWS {
        let kind = if row == 0 {
            2
        } else if row < 3 {
            1
        } else {
            0
        };
        for col in 0..ALIEN_COLS {
            aliens.push(Alien {
                x: 4 + (col as i64) * 5,
                y: ALIEN_START_Y + (row as i64) * 2,
                kind,
                alive: true,
            });
        }
    }
    aliens
}

fn create_initial_state(high_score: u64, level: u64) -> GameState {
    let move_delay = (20usize.saturating_sub((level as usize) * 2)).max(5);
    GameState {
        player: Player {
            x: GAME_WIDTH / 2,
            lives: 3,
        },
        aliens: create_aliens(),
        alien_direction: 1,
        alien_move_counter: 0,
        alien_move_delay: move_delay,
        alien_dropping: false,
        bullets: Vec::new(),
        shields: create_shields(),
        score: 0,
        high_score,
        level,
        game_over: false,
        victory: false,
        alien_shoot_counter: 0,
    }
}

struct SpaceInvadersState {
    state: GameState,
    keys: KeyState,
    interval: Option<pi_js::time::Interval>,
    cached_lines: Vec<String>,
    cached_width: usize,
    version: u64,
    cached_version: i64,
    paused: bool,
    fire_cooldown: usize,
    player_move_counter: usize,
}

struct SpaceInvadersInner {
    tui: Arc<dyn TUI + Send + Sync>,
    on_close: Arc<dyn Fn() + Send + Sync>,
    on_save: Arc<dyn Fn(Option<GameState>) + Send + Sync>,
    mutable: Mutex<SpaceInvadersState>,
}

#[derive(Clone)]
struct SpaceInvadersComponent {
    inner: Arc<SpaceInvadersInner>,
}

impl SpaceInvadersState {
    fn move_aliens(&mut self) {
        let alive_indices: Vec<usize> = self
            .state
            .aliens
            .iter()
            .enumerate()
            .filter_map(|(i, a)| a.alive.then_some(i))
            .collect();
        if alive_indices.is_empty() {
            return;
        }

        if self.state.alien_dropping {
            for &idx in &alive_indices {
                self.state.aliens[idx].y += 1;
                if self.state.aliens[idx].y >= PLAYER_Y - 1 {
                    self.state.game_over = true;
                    return;
                }
            }
            self.state.alien_dropping = false;
        } else {
            let min_x = alive_indices.iter().map(|&i| self.state.aliens[i].x).min().unwrap_or(0);
            let max_x = alive_indices.iter().map(|&i| self.state.aliens[i].x).max().unwrap_or(0);

            if (self.state.alien_direction == 1 && max_x >= GAME_WIDTH - 3)
                || (self.state.alien_direction == -1 && min_x <= 2)
            {
                self.state.alien_direction *= -1;
                self.state.alien_dropping = true;
            } else {
                let dir = self.state.alien_direction;
                for &idx in &alive_indices {
                    self.state.aliens[idx].x += dir;
                }
            }
        }

        let alive_count = alive_indices.len();
        if alive_count <= 5 {
            self.state.alien_move_delay = 1;
        } else if alive_count <= 10 {
            self.state.alien_move_delay = 2;
        } else if alive_count <= 20 {
            self.state.alien_move_delay = 3;
        }
    }

    fn alien_shoot(&mut self) {
        let mut columns: IndexMap<i64, (i64, i64)> = IndexMap::new();
        for alien in &self.state.aliens {
            if !alien.alive {
                continue;
            }
            match columns.get_mut(&alien.x) {
                Some(entry) => {
                    if alien.y > entry.1 {
                        *entry = (alien.x, alien.y);
                    }
                }
                None => {
                    columns.insert(alien.x, (alien.x, alien.y));
                }
            }
        }

        let shooters: Vec<(i64, i64)> = columns.values().copied().collect();
        let alien_bullet_count = self.state.bullets.iter().filter(|b| b.direction == 1).count();
        if !shooters.is_empty() && alien_bullet_count < 3 {
            let idx =
                ((pi_js::crypto::math_random() * (shooters.len() as f64)).floor() as usize).min(shooters.len() - 1);
            let shooter = shooters[idx];
            self.state.bullets.push(Bullet {
                x: shooter.0,
                y: shooter.1 + 1,
                direction: 1,
            });
        }
    }

    fn check_collisions(&mut self) {
        let mut remove_flags = vec![false; self.state.bullets.len()];

        for (b_idx, bullet) in self.state.bullets.iter().enumerate() {
            if bullet.direction == -1 {
                for alien in &mut self.state.aliens {
                    if alien.alive && (bullet.x - alien.x).abs() <= 1 && bullet.y == alien.y {
                        alien.alive = false;
                        remove_flags[b_idx] = true;
                        let points = match alien.kind {
                            0 => 10,
                            1 => 20,
                            _ => 30,
                        };
                        self.state.score += points;
                        if self.state.score > self.state.high_score {
                            self.state.high_score = self.state.score;
                        }
                        break;
                    }
                }
            }

            if bullet.direction == 1 && (bullet.x - self.state.player.x).abs() <= 1 && bullet.y == PLAYER_Y {
                remove_flags[b_idx] = true;
                self.state.player.lives = self.state.player.lives.saturating_sub(1);
                if self.state.player.lives == 0 {
                    self.state.game_over = true;
                }
            }

            for shield in &mut self.state.shields {
                let rel_x = bullet.x - shield.x;
                let rel_y = bullet.y - (PLAYER_Y - 5);
                if (0..4).contains(&rel_x)
                    && (0..3).contains(&rel_y)
                    && let Some(row) = shield.segments.get_mut(rel_y as usize)
                    && let Some(cell) = row.get_mut(rel_x as usize)
                    && *cell
                {
                    *cell = false;
                    remove_flags[b_idx] = true;
                }
            }
        }

        let prev = std::mem::take(&mut self.state.bullets);
        self.state.bullets = prev
            .into_iter()
            .enumerate()
            .filter_map(|(i, b)| (!remove_flags.get(i).copied().unwrap_or(false)).then_some(b))
            .collect();
    }

    fn tick(&mut self) {
        self.player_move_counter += 1;
        if self.player_move_counter >= 2 {
            self.player_move_counter = 0;
            if self.keys.left && self.state.player.x > 2 {
                self.state.player.x -= 1;
            }
            if self.keys.right && self.state.player.x < GAME_WIDTH - 3 {
                self.state.player.x += 1;
            }
        }

        if self.fire_cooldown > 0 {
            self.fire_cooldown -= 1;
        }

        if self.keys.fire && self.fire_cooldown == 0 {
            let player_bullets = self.state.bullets.iter().filter(|b| b.direction == -1).count();
            if player_bullets < 2 {
                self.state.bullets.push(Bullet {
                    x: self.state.player.x,
                    y: PLAYER_Y - 1,
                    direction: -1,
                });
                self.fire_cooldown = 8;
            }
        }

        let prev_bullets = std::mem::take(&mut self.state.bullets);
        self.state.bullets = prev_bullets
            .into_iter()
            .filter_map(|mut bullet| {
                bullet.y += bullet.direction;
                (bullet.y >= 0 && bullet.y < GAME_HEIGHT).then_some(bullet)
            })
            .collect();

        self.state.alien_move_counter += 1;
        if self.state.alien_move_counter >= self.state.alien_move_delay {
            self.state.alien_move_counter = 0;
            self.move_aliens();
        }

        self.state.alien_shoot_counter += 1;
        if self.state.alien_shoot_counter >= 30 {
            self.state.alien_shoot_counter = 0;
            self.alien_shoot();
        }

        self.check_collisions();

        if self.state.aliens.iter().all(|a| !a.alive) {
            self.state.victory = true;
        }
    }
}

impl SpaceInvadersComponent {
    fn new(
        tui: Arc<dyn TUI + Send + Sync>,
        on_close: Arc<dyn Fn() + Send + Sync>,
        on_save: Arc<dyn Fn(Option<GameState>) + Send + Sync>,
        saved_state: Option<GameState>,
    ) -> Self {
        let (initial_state, paused) = match saved_state {
            Some(s) if !s.game_over && !s.victory => (s, true),
            Some(s) => (create_initial_state(s.high_score, 1), false),
            None => (create_initial_state(0, 1), false),
        };

        let inner = Arc::new(SpaceInvadersInner {
            tui,
            on_close,
            on_save,
            mutable: Mutex::new(SpaceInvadersState {
                state: initial_state,
                keys: KeyState::default(),
                interval: None,
                cached_lines: Vec::new(),
                cached_width: 0,
                version: 0,
                cached_version: -1,
                paused,
                fire_cooldown: 0,
                player_move_counter: 0,
            }),
        });

        let comp = Self { inner };
        if !paused {
            comp.start_game();
        }
        comp
    }

    fn start_game(&self) {
        let weak = Arc::downgrade(&self.inner);
        let handle = pi_js::time::set_interval(TICK_MS, move || {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let should_render = {
                let mut m = inner.mutable.lock().unwrap_or_else(|e| e.into_inner());
                if !m.state.game_over && !m.state.victory {
                    m.tick();
                    m.version += 1;
                    true
                } else {
                    false
                }
            };
            if should_render {
                inner.tui.request_render(Some(false));
            }
        });
        let old = {
            let mut m = self.inner.mutable.lock().unwrap_or_else(|e| e.into_inner());
            m.interval.replace(handle)
        };
        if let Some(prev) = old {
            prev.clear();
        }
    }

    fn dispose(&self) {
        let old = self
            .inner
            .mutable
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .interval
            .take();
        if let Some(handle) = old {
            handle.clear();
        }
    }

    fn pad_line(line: &str, width: usize) -> String {
        let visible_len = match ANSI_SGR_RE.as_ref() {
            Some(re) => re.replace_all(line, "").chars().count(),
            None => line.chars().count(),
        };
        let padding = width.saturating_sub(visible_len);
        format!("{line}{}", " ".repeat(padding))
    }
}

impl Drop for SpaceInvadersInner {
    fn drop(&mut self) {
        if let Some(handle) = self.mutable.lock().unwrap_or_else(|e| e.into_inner()).interval.take() {
            handle.clear();
        }
    }
}

impl Component for SpaceInvadersComponent {
    fn has_handle_input(&self) -> bool {
        true
    }

    fn wants_key_release(&self) -> bool {
        true
    }

    fn handle_input(&self, data: &str) {
        let released = is_key_release(data);

        let is_paused = self.inner.mutable.lock().unwrap_or_else(|e| e.into_inner()).paused;
        if is_paused && !released {
            if matches_key(data, Key::escape) || data == "q" || data == "Q" {
                self.dispose();
                (self.inner.on_close)();
                return;
            }
            self.inner.mutable.lock().unwrap_or_else(|e| e.into_inner()).paused = false;
            self.start_game();
            return;
        }

        if !released && matches_key(data, Key::escape) {
            self.dispose();
            let state_snapshot = self
                .inner
                .mutable
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .state
                .clone();
            (self.inner.on_save)(Some(state_snapshot));
            (self.inner.on_close)();
            return;
        }

        if !released && (data == "q" || data == "Q") {
            self.dispose();
            (self.inner.on_save)(None);
            (self.inner.on_close)();
            return;
        }

        let mut need_save_clear = false;
        let mut need_render = false;
        {
            let mut m = self.inner.mutable.lock().unwrap_or_else(|e| e.into_inner());
            if matches_key(data, Key::left) || data == "a" || data == "A" || matches_key(data, "a") {
                m.keys.left = !released;
            }
            if matches_key(data, Key::right) || data == "d" || data == "D" || matches_key(data, "d") {
                m.keys.right = !released;
            }
            if matches_key(data, Key::space) || data == " " || data == "f" || data == "F" || matches_key(data, "f") {
                m.keys.fire = !released;
            }

            if !released && (m.state.game_over || m.state.victory) && (data == "r" || data == "R" || data == " ") {
                let high_score = m.state.high_score;
                let next_level = if m.state.victory { m.state.level + 1 } else { 1 };
                m.state = create_initial_state(high_score, next_level);
                m.keys = KeyState::default();
                m.version += 1;
                need_save_clear = true;
                need_render = true;
            }
        }

        if need_save_clear {
            (self.inner.on_save)(None);
        }
        if need_render {
            self.inner.tui.request_render(Some(false));
        }
    }

    fn invalidate(&self) {
        self.inner
            .mutable
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cached_width = 0;
    }

    fn render(&self, width: usize) -> Vec<String> {
        let mut m = self.inner.mutable.lock().unwrap_or_else(|e| e.into_inner());
        if width == m.cached_width && m.cached_version == (m.version as i64) {
            return m.cached_lines.clone();
        }

        let dim = |s: &str| format!("\x1b[2m{s}\x1b[22m");
        let green = |s: &str| format!("\x1b[32m{s}\x1b[0m");
        let red = |s: &str| format!("\x1b[31m{s}\x1b[0m");
        let yellow = |s: &str| format!("\x1b[33m{s}\x1b[0m");
        let cyan = |s: &str| format!("\x1b[36m{s}\x1b[0m");
        let magenta = |s: &str| format!("\x1b[35m{s}\x1b[0m");
        let white = |s: &str| format!("\x1b[97m{s}\x1b[0m");
        let bold = |s: &str| format!("\x1b[1m{s}\x1b[22m");

        let box_width = GAME_WIDTH as usize;
        let box_line = |content: &str| -> String {
            let content_len = visible_width(content).max(0) as usize;
            let padding = box_width.saturating_sub(content_len);
            format!("{}{content}{}{}", dim(" │"), " ".repeat(padding), dim("│"))
        };

        let mut lines: Vec<String> = Vec::new();
        lines.push(Self::pad_line(&dim(&format!(" ╭{}╮", "─".repeat(box_width))), width));

        let title = bold(&green("SPACE INVADERS"));
        let score_text = format!("Score: {}", bold(&yellow(&m.state.score.to_string())));
        let high_text = format!("Hi: {}", bold(&yellow(&m.state.high_score.to_string())));
        let level_text = format!("Lv: {}", bold(&cyan(&m.state.level.to_string())));
        let lives_text = red(&"♥".repeat(m.state.player.lives));
        let header = format!("{title} │ {score_text} │ {high_text} │ {level_text} │ {lives_text}");
        lines.push(Self::pad_line(&box_line(&header), width));

        lines.push(Self::pad_line(&dim(&format!(" ├{}┤", "─".repeat(box_width))), width));

        for y in 0..GAME_HEIGHT {
            let mut row = String::new();
            for x in 0..GAME_WIDTH {
                let mut cell = String::from(" ");
                let mut colored = false;

                for alien in &m.state.aliens {
                    if alien.alive && alien.y == y && (alien.x - x).abs() <= 1 {
                        let sprite = match alien.kind {
                            0 => {
                                if x == alien.x {
                                    "▼"
                                } else if x < alien.x {
                                    "╲"
                                } else {
                                    "╱"
                                }
                            }
                            1 => {
                                if x == alien.x {
                                    "◆"
                                } else if x < alien.x {
                                    "╱"
                                } else {
                                    "╲"
                                }
                            }
                            _ => {
                                if x == alien.x {
                                    "☆"
                                } else if x < alien.x {
                                    "◄"
                                } else {
                                    "►"
                                }
                            }
                        };
                        cell = match alien.kind {
                            0 => green(sprite),
                            1 => cyan(sprite),
                            _ => magenta(sprite),
                        };
                        colored = true;
                        break;
                    }
                }

                if !colored {
                    for shield in &m.state.shields {
                        let rel_x = x - shield.x;
                        let rel_y = y - (PLAYER_Y - 5);
                        if (0..4).contains(&rel_x) && (0..3).contains(&rel_y) {
                            if shield
                                .segments
                                .get(rel_y as usize)
                                .and_then(|r| r.get(rel_x as usize))
                                .copied()
                                .unwrap_or(false)
                            {
                                cell = dim("█");
                                colored = true;
                            }
                            break;
                        }
                    }
                }

                if !colored && y == PLAYER_Y && (x - m.state.player.x).abs() <= 1 {
                    cell = if x == m.state.player.x {
                        white("▲")
                    } else {
                        white("═")
                    };
                    colored = true;
                }

                if !colored {
                    for bullet in &m.state.bullets {
                        if bullet.x == x && bullet.y == y {
                            cell = if bullet.direction == -1 {
                                yellow("│")
                            } else {
                                red("│")
                            };
                            break;
                        }
                    }
                }

                row.push_str(&cell);
            }
            lines.push(Self::pad_line(&format!("{}{row}{}", dim(" │"), dim("│")), width));
        }

        lines.push(Self::pad_line(&dim(&format!(" ├{}┤", "─".repeat(box_width))), width));

        let footer = if m.paused {
            format!(
                "{} Press any key to continue, {} to quit",
                yellow(&bold("PAUSED")),
                bold("Q")
            )
        } else if m.state.game_over {
            format!(
                "{} Press {} to restart, {} to quit",
                red(&bold("GAME OVER!")),
                bold("R"),
                bold("Q")
            )
        } else if m.state.victory {
            format!(
                "{} Press {} for level {}, {} to quit",
                green(&bold("VICTORY!")),
                bold("R"),
                m.state.level + 1,
                bold("Q")
            )
        } else {
            format!(
                "←→ or AD to move, {}/F to fire, {} pause, {} quit",
                bold("SPACE"),
                bold("ESC"),
                bold("Q")
            )
        };
        lines.push(Self::pad_line(&box_line(&footer), width));
        lines.push(Self::pad_line(&dim(&format!(" ╰{}╯", "─".repeat(box_width))), width));

        m.cached_lines = lines.clone();
        m.cached_width = width;
        m.cached_version = m.version as i64;
        lines
    }
}

pub struct SpaceInvadersExtension;

#[async_trait]
impl Extension for SpaceInvadersExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let pi_cmd = pi.clone();
        pi.register_command(
            "invaders",
            RegisteredCommandOptions {
                description: Some("Play Space Invaders!".to_string()),
                get_argument_completions: None,
                handler: Arc::new(move |_args, ctx| {
                    let pi_inner = pi_cmd.clone();
                    Box::pin(async move {
                        let base = ctx.as_context();
                        let ui = base.ui()?;
                        if base.mode()? != ExtensionMode::Tui {
                            ui.notify("Space Invaders requires interactive mode", Some(NotifyType::Error));
                            return Ok(());
                        }

                        let entries = base.session_manager()?.get_entries();
                        let mut saved_state: Option<GameState> = None;
                        for entry in entries.iter().rev() {
                            if let pi_coding_agent::core::session_manager::SessionEntry::Custom(custom) = entry
                                && custom.custom_type == INVADERS_SAVE_TYPE
                            {
                                saved_state = custom
                                    .data
                                    .as_ref()
                                    .and_then(|v| serde_json::from_value::<GameState>(v.clone()).ok());
                                break;
                            }
                        }

                        let pi_save = pi_inner.clone();
                        let _ = ui
                            .custom(
                                Box::new(move |tui, _theme, _kb, done| {
                                    Box::pin(async move {
                                        let done_cb: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                                            done(Value::Null);
                                        });
                                        let save_cb: Arc<dyn Fn(Option<GameState>) + Send + Sync> =
                                            Arc::new(move |state| {
                                                let val = state.and_then(|s| serde_json::to_value(s).ok());
                                                let _ = pi_save.append_entry(INVADERS_SAVE_TYPE, val);
                                            });
                                        let comp = SpaceInvadersComponent::new(tui, done_cb, save_cb, saved_state);
                                        Ok(Arc::new(comp) as Arc<dyn Component>)
                                    })
                                }),
                                None,
                            )
                            .await?;

                        Ok(())
                    })
                }),
            },
        )?;

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(SpaceInvadersExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
