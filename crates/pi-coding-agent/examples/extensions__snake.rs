//! Port of packages/coding-agent/examples/extensions/snake.ts

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, ExtensionMode, InlineExtension, NotifyType, RegisteredCommandOptions,
};
use pi_js::crypto::math_random;
use pi_js::time::{Interval, set_interval};
use pi_tui::{Component, TUI, matches_key, visible_width};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const GAME_WIDTH: isize = 40;
const GAME_HEIGHT: isize = 15;
const TICK_MS: u64 = 100;
const SNAKE_SAVE_TYPE: &str = "snake-save";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Direction {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Point {
    x: isize,
    y: isize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GameState {
    snake: Vec<Point>,
    food: Point,
    direction: Direction,
    next_direction: Direction,
    score: u64,
    game_over: bool,
    high_score: u64,
}

fn spawn_food(snake: &[Point]) -> Point {
    loop {
        let candidate = Point {
            x: (math_random() * GAME_WIDTH as f64).floor() as isize,
            y: (math_random() * GAME_HEIGHT as f64).floor() as isize,
        };
        if !snake.contains(&candidate) {
            return candidate;
        }
    }
}

fn create_initial_state() -> GameState {
    let start_x = GAME_WIDTH / 2;
    let start_y = GAME_HEIGHT / 2;
    let initial_head = Point { x: start_x, y: start_y };
    GameState {
        snake: vec![
            initial_head,
            Point {
                x: start_x - 1,
                y: start_y,
            },
            Point {
                x: start_x - 2,
                y: start_y,
            },
        ],
        food: spawn_food(&[initial_head]),
        direction: Direction::Right,
        next_direction: Direction::Right,
        score: 0,
        game_over: false,
        high_score: 0,
    }
}

struct SnakeInnerState {
    game: GameState,
    paused: bool,
    version: u64,
    cached_width: usize,
    cached_version: u64,
    cached_lines: Vec<String>,
    interval: Option<Interval>,
}

struct SnakeComponent {
    tui: Arc<dyn TUI>,
    on_close: Arc<dyn Fn() + Send + Sync>,
    on_save: Arc<dyn Fn(Option<GameState>) + Send + Sync>,
    inner: Arc<Mutex<SnakeInnerState>>,
}

impl SnakeComponent {
    fn new(
        tui: Arc<dyn TUI>,
        on_close: Arc<dyn Fn() + Send + Sync>,
        on_save: Arc<dyn Fn(Option<GameState>) + Send + Sync>,
        saved_state: Option<GameState>,
    ) -> Self {
        let (game, paused, should_start) = match saved_state {
            Some(saved) if !saved.game_over => (saved, true, false),
            Some(saved) => {
                let mut g = create_initial_state();
                g.high_score = saved.high_score;
                (g, false, true)
            }
            None => (create_initial_state(), false, true),
        };

        let inner = Arc::new(Mutex::new(SnakeInnerState {
            game,
            paused,
            version: 0,
            cached_width: 0,
            cached_version: u64::MAX,
            cached_lines: Vec::new(),
            interval: None,
        }));

        let comp = Self {
            tui,
            on_close,
            on_save,
            inner,
        };
        if should_start {
            comp.start_game();
        }
        comp
    }

    fn start_game(&self) {
        let inner_ref = Arc::clone(&self.inner);
        let tui_ref = Arc::clone(&self.tui);
        let iv = set_interval(TICK_MS, move || {
            let rendered = {
                let mut guard = inner_ref.lock().unwrap_or_else(|e| e.into_inner());
                if guard.game.game_over {
                    false
                } else {
                    Self::tick_state(&mut guard.game);
                    guard.version += 1;
                    true
                }
            };
            if rendered {
                tui_ref.request_render(None);
            }
        });
        let old = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .interval
            .replace(iv);
        if let Some(prev) = old {
            prev.clear();
        }
    }

    fn stop_interval(&self) {
        let old = self.inner.lock().unwrap_or_else(|e| e.into_inner()).interval.take();
        if let Some(prev) = old {
            prev.clear();
        }
    }

    fn tick_state(game: &mut GameState) {
        game.direction = game.next_direction;
        let Some(&head) = game.snake.first() else {
            return;
        };
        let new_head = match game.direction {
            Direction::Up => Point {
                x: head.x,
                y: head.y - 1,
            },
            Direction::Down => Point {
                x: head.x,
                y: head.y + 1,
            },
            Direction::Left => Point {
                x: head.x - 1,
                y: head.y,
            },
            Direction::Right => Point {
                x: head.x + 1,
                y: head.y,
            },
        };

        if new_head.x < 0 || new_head.x >= GAME_WIDTH || new_head.y < 0 || new_head.y >= GAME_HEIGHT {
            game.game_over = true;
            return;
        }
        if game.snake.contains(&new_head) {
            game.game_over = true;
            return;
        }

        game.snake.insert(0, new_head);
        if new_head == game.food {
            game.score += 10;
            if game.score > game.high_score {
                game.high_score = game.score;
            }
            game.food = spawn_food(&game.snake);
        } else {
            game.snake.pop();
        }
    }

    fn pad_line(line: &str, width: usize) -> String {
        let vis = visible_width(line).max(0) as usize;
        let padding = width.saturating_sub(vis);
        format!("{line}{}", " ".repeat(padding))
    }
}

impl Drop for SnakeComponent {
    fn drop(&mut self) {
        self.stop_interval();
    }
}

impl Component for SnakeComponent {
    fn render(&self, width: usize) -> Vec<String> {
        let (game, paused, version) = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if width == guard.cached_width && guard.cached_version == guard.version {
                return guard.cached_lines.clone();
            }
            (guard.game.clone(), guard.paused, guard.version)
        };

        let dim = |s: &str| format!("\x1b[2m{s}\x1b[22m");
        let green = |s: &str| format!("\x1b[32m{s}\x1b[0m");
        let red = |s: &str| format!("\x1b[31m{s}\x1b[0m");
        let yellow = |s: &str| format!("\x1b[33m{s}\x1b[0m");
        let bold = |s: &str| format!("\x1b[1m{s}\x1b[22m");

        let cell_width = 2isize;
        let effective_width = GAME_WIDTH.min((width as isize - 4).div_euclid(cell_width)).max(0);
        let box_width = (effective_width * cell_width) as usize;

        let box_line = |content: &str| -> String {
            let content_len = visible_width(content).max(0) as usize;
            let padding = box_width.saturating_sub(content_len);
            format!("{}{content}{}{}", dim(" │"), " ".repeat(padding), dim("│"))
        };

        let mut lines = Vec::new();
        lines.push(Self::pad_line(&dim(&format!(" ╭{}╮", "─".repeat(box_width))), width));

        let score_text = format!("Score: {}", bold(&yellow(&game.score.to_string())));
        let high_text = format!("High: {}", bold(&yellow(&game.high_score.to_string())));
        let title = format!("{} │ {score_text} │ {high_text}", bold(&green("SNAKE")));
        lines.push(Self::pad_line(&box_line(&title), width));
        lines.push(Self::pad_line(&dim(&format!(" ├{}┤", "─".repeat(box_width))), width));

        for y in 0..GAME_HEIGHT {
            let mut row = String::new();
            for x in 0..effective_width {
                let p = Point { x, y };
                let is_head = game.snake.first().copied() == Some(p);
                let is_body = game.snake.get(1..).is_some_and(|rest| rest.contains(&p));
                let is_food = game.food == p;

                if is_head {
                    row.push_str(&green("██"));
                } else if is_body {
                    row.push_str(&green("▓▓"));
                } else if is_food {
                    row.push_str(&red("◆ "));
                } else {
                    row.push_str("  ");
                }
            }
            lines.push(Self::pad_line(&format!("{}{row}{}", dim(" │"), dim("│")), width));
        }

        lines.push(Self::pad_line(&dim(&format!(" ├{}┤", "─".repeat(box_width))), width));

        let footer = if paused {
            format!(
                "{} Press any key to continue, {} to quit",
                yellow(&bold("PAUSED")),
                bold("Q")
            )
        } else if game.game_over {
            format!(
                "{} Press {} to restart, {} to quit",
                red(&bold("GAME OVER!")),
                bold("R"),
                bold("Q")
            )
        } else {
            format!("↑↓←→ or WASD to move, {} pause, {} quit", bold("ESC"), bold("Q"))
        };
        lines.push(Self::pad_line(&box_line(&footer), width));
        lines.push(Self::pad_line(&dim(&format!(" ╰{}╯", "─".repeat(box_width))), width));

        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.cached_lines = lines.clone();
        guard.cached_width = width;
        guard.cached_version = version;
        lines
    }

    fn handle_input(&self, data: &str) {
        enum Action {
            None,
            StartGame,
            CloseOnly,
            SaveAndClose(Option<GameState>),
            ClearSaveAndRender,
        }

        let action = {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if guard.paused {
                if matches_key(data, "escape") || data == "q" || data == "Q" {
                    Action::CloseOnly
                } else {
                    guard.paused = false;
                    Action::StartGame
                }
            } else if matches_key(data, "escape") {
                Action::SaveAndClose(Some(guard.game.clone()))
            } else if data == "q" || data == "Q" {
                Action::SaveAndClose(None)
            } else {
                if matches_key(data, "up") || data == "w" || data == "W" {
                    if guard.game.direction != Direction::Down {
                        guard.game.next_direction = Direction::Up;
                    }
                } else if matches_key(data, "down") || data == "s" || data == "S" {
                    if guard.game.direction != Direction::Up {
                        guard.game.next_direction = Direction::Down;
                    }
                } else if matches_key(data, "right") || data == "d" || data == "D" {
                    if guard.game.direction != Direction::Left {
                        guard.game.next_direction = Direction::Right;
                    }
                } else if (matches_key(data, "left") || data == "a" || data == "A")
                    && guard.game.direction != Direction::Right
                {
                    guard.game.next_direction = Direction::Left;
                }

                if guard.game.game_over && (data == "r" || data == "R" || data == " ") {
                    let high_score = guard.game.high_score;
                    guard.game = create_initial_state();
                    guard.game.high_score = high_score;
                    guard.version += 1;
                    Action::ClearSaveAndRender
                } else {
                    Action::None
                }
            }
        };

        match action {
            Action::None => {}
            Action::StartGame => self.start_game(),
            Action::CloseOnly => {
                self.stop_interval();
                (self.on_close)();
            }
            Action::SaveAndClose(saved) => {
                self.stop_interval();
                (self.on_save)(saved);
                (self.on_close)();
            }
            Action::ClearSaveAndRender => {
                (self.on_save)(None);
                self.tui.request_render(None);
            }
        }
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn invalidate(&self) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.cached_width = 0;
    }
}

pub struct SnakeExtension;

#[async_trait]
impl Extension for SnakeExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let pi_cmd = pi.clone();
        pi.register_command(
            "snake",
            RegisteredCommandOptions {
                description: Some("Play Snake!".to_string()),
                get_argument_completions: None,
                handler: Arc::new(move |_args, ctx| {
                    let pi = pi_cmd.clone();
                    Box::pin(async move {
                        let base = ctx.as_context();
                        let ui = base.ui()?;
                        if base.mode()? != ExtensionMode::Tui {
                            ui.notify("Snake requires interactive mode", Some(NotifyType::Error));
                            return Ok(());
                        }

                        let entries = base.session_manager()?.get_entries();
                        let mut saved_state: Option<GameState> = None;
                        for entry in entries.iter().rev() {
                            if entry.r#type == "custom"
                                && entry.fields.get("customType").and_then(|v| v.as_str()) == Some(SNAKE_SAVE_TYPE)
                                && let Some(data_val) = entry.fields.get("data")
                                && let Ok(parsed) = serde_json::from_value::<GameState>(data_val.clone())
                            {
                                saved_state = Some(parsed);
                                break;
                            }
                        }

                        let pi_save = pi.clone();
                        let _ = ui
                            .custom(
                                Box::new(move |tui, _theme, _kb, done| {
                                    Box::pin(async move {
                                        let pi_cb = pi_save.clone();
                                        let on_save: Arc<dyn Fn(Option<GameState>) + Send + Sync> =
                                            Arc::new(move |state| {
                                                let val = state.and_then(|s| serde_json::to_value(s).ok());
                                                let _ = pi_cb.append_entry(SNAKE_SAVE_TYPE, val);
                                            });
                                        Ok(Arc::new(SnakeComponent::new(
                                            tui,
                                            Arc::new(move || done(Value::Null)),
                                            on_save,
                                            saved_state,
                                        )) as Arc<dyn Component>)
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
    Arc::new(SnakeExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
