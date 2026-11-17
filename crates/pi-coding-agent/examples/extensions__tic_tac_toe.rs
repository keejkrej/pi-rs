//! Port of packages/coding-agent/examples/extensions/tic-tac-toe.ts

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexSet;
use pi_agent_core::types::{AgentMessage, AgentToolResult, ToolExecutionMode};
use pi_ai::types::{Message, TextContent, UserContentBlock};
use pi_ai::utils::typebox_helpers::{StringEnumOptions, string_enum};
use pi_coding_agent::core::extensions::types::{
    BeforeAgentStartEvent, BeforeAgentStartEventResult, CustomMessageContent, CustomMessageDraft, Extension,
    ExtensionApi, ExtensionContext, ExtensionFactory, ExtensionMode, InlineExtension, NotifyType,
    RegisteredCommandOptions, SendMessageOptions, SessionStartEvent, SessionTreeEvent, Theme, ToolDefinition,
    ToolRenderResultOptions,
};
use pi_coding_agent::core::session_manager::SessionEntry;
use pi_js::vendor::typebox::t;
use pi_tui::{Component, Key, TUI, Text, matches_key, truncate_to_width, visible_width};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Cell {
    #[serde(rename = " ")]
    Empty,
    #[serde(rename = "X")]
    X,
    #[serde(rename = "O")]
    O,
}

impl Cell {
    fn as_str(self) -> &'static str {
        match self {
            Self::Empty => " ",
            Self::X => "X",
            Self::O => "O",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum GameStatus {
    #[serde(rename = "playing")]
    Playing,
    #[serde(rename = "win_X")]
    WinX,
    #[serde(rename = "win_O")]
    WinO,
    #[serde(rename = "draw")]
    Draw,
}

impl GameStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Playing => "playing",
            Self::WinX => "win_X",
            Self::WinO => "win_O",
            Self::Draw => "draw",
        }
    }
}

#[derive(Clone, Debug)]
struct GameState {
    board: [[Cell; 3]; 3],
    user_cursor_row: usize,
    user_cursor_col: usize,
    agent_cursor_row: usize,
    agent_cursor_col: usize,
    status: GameStatus,
    user_mark: Cell,
    agent_mark: Cell,
    current_turn: Cell,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BoardDetails {
    board: [[Cell; 3]; 3],
    agent_cursor_row: usize,
    agent_cursor_col: usize,
    status: GameStatus,
    current_turn: Cell,
}

const AGENT_CURSOR_HOME_ROW: usize = 0;
const AGENT_CURSOR_HOME_COL: usize = 0;
const CELL_WIDTH: usize = 7;
const CELL_HEIGHT: usize = 3;
const FG_CODE_X: &str = "34";
const FG_CODE_O: &str = "33";
const FG_CODE_WIN: &str = "32";
const GLYPH_X: &str = "\u{2573}";
const GLYPH_O: &str = "\u{25ef}";
const RESET: &str = "\x1b[0m";

const SAVE_TYPE: &str = "tic-tac-toe-save";
const MOVE_MESSAGE_TYPE: &str = "tic-tac-toe-move";
const GAME_OVER_MESSAGE_TYPE: &str = "tic-tac-toe-game-over";

fn create_initial_state() -> GameState {
    GameState {
        board: [[Cell::Empty; 3]; 3],
        user_cursor_row: 1,
        user_cursor_col: 1,
        agent_cursor_row: AGENT_CURSOR_HOME_ROW,
        agent_cursor_col: AGENT_CURSOR_HOME_COL,
        status: GameStatus::Playing,
        user_mark: Cell::X,
        agent_mark: Cell::O,
        current_turn: Cell::X,
    }
}

fn get_win_line(board: &[[Cell; 3]; 3]) -> Option<[(usize, usize); 3]> {
    const LINES: [[(usize, usize); 3]; 8] = [
        [(0, 0), (0, 1), (0, 2)],
        [(1, 0), (1, 1), (1, 2)],
        [(2, 0), (2, 1), (2, 2)],
        [(0, 0), (1, 0), (2, 0)],
        [(0, 1), (1, 1), (2, 1)],
        [(0, 2), (1, 2), (2, 2)],
        [(0, 0), (1, 1), (2, 2)],
        [(0, 2), (1, 1), (2, 0)],
    ];
    for line in LINES {
        let a = board[line[0].0][line[0].1];
        let b = board[line[1].0][line[1].1];
        let c = board[line[2].0][line[2].1];
        if a != Cell::Empty && a == b && b == c {
            return Some(line);
        }
    }
    None
}

fn check_win(board: &[[Cell; 3]; 3]) -> GameStatus {
    if let Some(win_line) = get_win_line(board) {
        let (r, c) = win_line[0];
        return if board[r][c] == Cell::X {
            GameStatus::WinX
        } else {
            GameStatus::WinO
        };
    }
    if board.iter().all(|row| row.iter().all(|&c| c != Cell::Empty)) {
        return GameStatus::Draw;
    }
    GameStatus::Playing
}

fn board_to_ascii(board: &[[Cell; 3]; 3], agent_cursor_row: usize, agent_cursor_col: usize) -> String {
    let separator = "---------+---------+---------";
    board
        .iter()
        .enumerate()
        .map(|(r, row)| {
            row.iter()
                .enumerate()
                .map(|(c_idx, &cell)| {
                    let on_cursor = r == agent_cursor_row && c_idx == agent_cursor_col;
                    if cell == Cell::Empty {
                        if on_cursor {
                            format!("<[{r},{c_idx}]>")
                        } else {
                            format!(" [{r},{c_idx}] ")
                        }
                    } else if on_cursor {
                        format!("   <{}>   ", cell.as_str())
                    } else {
                        format!("    {}    ", cell.as_str())
                    }
                })
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect::<Vec<_>>()
        .join(&format!("\n{separator}\n"))
}

fn dim_sgr(s: &str) -> String {
    format!("\x1b[2m{s}\x1b[22m")
}

fn center_pad(content: &str, width: usize) -> String {
    let content_len = visible_width(content).max(0) as usize;
    if content_len >= width {
        return truncate_to_width(content, width as i64, Some("..."), None);
    }
    let pad = width - content_len;
    let left = pad / 2;
    format!("{}{content}{}", " ".repeat(left), " ".repeat(pad - left))
}

fn cell_fg_code(cell: Cell, is_win: bool) -> Option<&'static str> {
    if cell == Cell::Empty {
        return None;
    }
    if is_win {
        return Some(FG_CODE_WIN);
    }
    Some(if cell == Cell::X { FG_CODE_X } else { FG_CODE_O })
}

fn build_cell_content(mark: Cell, line_idx: usize, is_win: bool) -> String {
    if mark == Cell::Empty || line_idx != CELL_HEIGHT / 2 {
        return " ".repeat(CELL_WIDTH);
    }
    let glyph = if mark == Cell::X { GLYPH_X } else { GLYPH_O };
    let fg = cell_fg_code(mark, is_win).unwrap_or(FG_CODE_X);
    let glyph_w = visible_width(glyph).max(0) as usize;
    let pad_len = CELL_WIDTH.saturating_sub(glyph_w);
    let left_pad = pad_len / 2;
    let right_pad = pad_len - left_pad;
    format!(
        "{}\x1b[{fg};1m{glyph}{RESET}{}",
        " ".repeat(left_pad),
        " ".repeat(right_pad)
    )
}

fn border_fg_code(adjacent: &[(Cell, bool)]) -> Option<&'static str> {
    let mut fgs = adjacent.iter().filter_map(|&(c, win)| cell_fg_code(c, win));
    let first = fgs.next()?;
    if fgs.all(|f| f == first) { Some(first) } else { None }
}

#[derive(Clone, Copy)]
enum CursorOwner {
    User,
    Agent,
}

struct CursorOverlay {
    row: usize,
    col: usize,
    owner: CursorOwner,
}

fn render_board(board: &[[Cell; 3]; 3], max_width: usize, cursor: Option<CursorOverlay>) -> Vec<String> {
    let show_cursor = cursor.is_some();
    let cr = cursor.as_ref().map(|c| c.row as isize).unwrap_or(-1);
    let cc = cursor.as_ref().map(|c| c.col as isize).unwrap_or(-1);
    let cursor_sgr = match cursor.as_ref().map(|c| c.owner) {
        Some(CursorOwner::Agent) => "\x1b[33;1m",
        _ => "\x1b[32;1m",
    };

    let mut win_cells: IndexSet<(usize, usize)> = IndexSet::new();
    if let Some(line) = get_win_line(board) {
        for cell in line {
            win_cells.insert(cell);
        }
    }

    let cell_at = |r: usize, c: usize| -> (Cell, bool) { (board[r][c], win_cells.contains(&(r, c))) };

    let is_cursor_corner = |grid_r: isize, grid_c: isize| -> bool {
        show_cursor && (grid_r == cr || grid_r == cr + 1) && (grid_c == cc || grid_c == cc + 1)
    };
    let is_cursor_h_segment =
        |grid_r: isize, c: isize| -> bool { show_cursor && c == cc && (grid_r == cr || grid_r == cr + 1) };
    let is_cursor_v_border =
        |r: isize, grid_c: isize| -> bool { show_cursor && r == cr && (grid_c == cc || grid_c == cc + 1) };

    let paint_border = |ch: &str, highlighted: bool, fg_code: Option<&'static str>| -> String {
        if highlighted {
            return format!("{cursor_sgr}{ch}{RESET}");
        }
        if let Some(code) = fg_code {
            return format!("\x1b[{code};1m{ch}{RESET}");
        }
        dim_sgr(ch)
    };

    let corner_char = |grid_r: usize, grid_c: usize| -> &'static str {
        match (grid_r, grid_c) {
            (0, 0) => "\u{250c}",
            (0, 3) => "\u{2510}",
            (3, 0) => "\u{2514}",
            (3, 3) => "\u{2518}",
            (0, _) => "\u{252c}",
            (3, _) => "\u{2534}",
            (_, 0) => "\u{251c}",
            (_, 3) => "\u{2524}",
            _ => "\u{253c}",
        }
    };

    let corner_adjacent = |grid_r: usize, grid_c: usize| -> Vec<(Cell, bool)> {
        let mut out = Vec::with_capacity(4);
        for (dr, dc) in [(-1isize, -1isize), (-1, 0), (0, -1), (0, 0)] {
            let r = (grid_r as isize) + dr;
            let c = (grid_c as isize) + dc;
            if (0..3).contains(&r) && (0..3).contains(&c) {
                out.push(cell_at(r as usize, c as usize));
            }
        }
        out
    };

    let mut lines = Vec::new();
    for grid_r in 0..=3 {
        let mut row = String::new();
        for grid_c in 0..=3 {
            let corner_color = border_fg_code(&corner_adjacent(grid_r, grid_c));
            row.push_str(&paint_border(
                corner_char(grid_r, grid_c),
                is_cursor_corner(grid_r as isize, grid_c as isize),
                corner_color,
            ));
            if grid_c < 3 {
                let mut adj = Vec::with_capacity(2);
                if grid_r > 0 {
                    adj.push(cell_at(grid_r - 1, grid_c));
                }
                if grid_r < 3 {
                    adj.push(cell_at(grid_r, grid_c));
                }
                let seg_color = border_fg_code(&adj);
                row.push_str(&paint_border(
                    &"\u{2500}".repeat(CELL_WIDTH),
                    is_cursor_h_segment(grid_r as isize, grid_c as isize),
                    seg_color,
                ));
            }
        }
        lines.push(center_pad(&row, max_width));

        if grid_r == 3 {
            break;
        }

        for line_idx in 0..CELL_HEIGHT {
            let mut content_row = String::new();
            for grid_c in 0..=3 {
                let mut adj = Vec::with_capacity(2);
                if grid_c > 0 {
                    adj.push(cell_at(grid_r, grid_c - 1));
                }
                if grid_c < 3 {
                    adj.push(cell_at(grid_r, grid_c));
                }
                let v_color = border_fg_code(&adj);
                content_row.push_str(&paint_border(
                    "\u{2502}",
                    is_cursor_v_border(grid_r as isize, grid_c as isize),
                    v_color,
                ));
                if grid_c < 3 {
                    content_row.push_str(&build_cell_content(
                        board[grid_r][grid_c],
                        line_idx,
                        win_cells.contains(&(grid_r, grid_c)),
                    ));
                }
            }
            lines.push(center_pad(&content_row, max_width));
        }
    }

    lines
}

fn render_visual_board(state: &GameState, max_width: usize) -> Vec<String> {
    let is_user_turn = state.current_turn == state.user_mark;
    let cursor = if state.status != GameStatus::Playing {
        None
    } else if is_user_turn {
        Some(CursorOverlay {
            row: state.user_cursor_row,
            col: state.user_cursor_col,
            owner: CursorOwner::User,
        })
    } else {
        Some(CursorOverlay {
            row: state.agent_cursor_row,
            col: state.agent_cursor_col,
            owner: CursorOwner::Agent,
        })
    };
    render_board(&state.board, max_width, cursor)
}

fn render_board_snapshot(board: &[[Cell; 3]; 3], max_width: usize) -> Vec<String> {
    render_board(board, max_width, None)
}

struct ComponentMutable {
    state: GameState,
    cached_lines: Vec<String>,
    cached_width: usize,
    version: u64,
    cached_version: i64,
}

struct TicTacToeComponent {
    tui: Arc<dyn TUI + Send + Sync>,
    on_close: Arc<dyn Fn() + Send + Sync>,
    on_user_play: Arc<dyn Fn(usize, usize) + Send + Sync>,
    mutable: Mutex<ComponentMutable>,
}

impl TicTacToeComponent {
    fn new(
        tui: Arc<dyn TUI + Send + Sync>,
        on_close: Arc<dyn Fn() + Send + Sync>,
        on_user_play: Arc<dyn Fn(usize, usize) + Send + Sync>,
        state: GameState,
    ) -> Self {
        Self {
            tui,
            on_close,
            on_user_play,
            mutable: Mutex::new(ComponentMutable {
                state,
                cached_lines: Vec::new(),
                cached_width: 0,
                version: 0,
                cached_version: -1,
            }),
        }
    }

    fn update_state(&self, state: GameState) {
        {
            let mut m = self.mutable.lock().unwrap_or_else(|e| e.into_inner());
            m.state = state;
            m.version += 1;
        }
        self.tui.request_render(Some(false));
    }
}

impl Component for TicTacToeComponent {
    fn has_handle_input(&self) -> bool {
        true
    }

    fn handle_input(&self, data: &str) {
        if matches_key(data, Key::escape) || data == "q" || data == "Q" {
            (self.on_close)();
            return;
        }

        enum ActionOutcome {
            None,
            Close,
            Render,
            UserPlay(usize, usize),
        }

        let outcome = {
            let mut m = self.mutable.lock().unwrap_or_else(|e| e.into_inner());
            if m.state.status != GameStatus::Playing {
                if data == "r" || data == "R" {
                    ActionOutcome::Close
                } else {
                    ActionOutcome::None
                }
            } else if m.state.current_turn != m.state.user_mark {
                ActionOutcome::None
            } else if matches_key(data, Key::up) && m.state.user_cursor_row > 0 {
                m.state.user_cursor_row -= 1;
                m.version += 1;
                ActionOutcome::Render
            } else if matches_key(data, Key::down) && m.state.user_cursor_row < 2 {
                m.state.user_cursor_row += 1;
                m.version += 1;
                ActionOutcome::Render
            } else if matches_key(data, Key::left) && m.state.user_cursor_col > 0 {
                m.state.user_cursor_col -= 1;
                m.version += 1;
                ActionOutcome::Render
            } else if matches_key(data, Key::right) && m.state.user_cursor_col < 2 {
                m.state.user_cursor_col += 1;
                m.version += 1;
                ActionOutcome::Render
            } else if matches_key(data, Key::r#return) || data == " " {
                let r = m.state.user_cursor_row;
                let c = m.state.user_cursor_col;
                if m.state.board[r][c] == Cell::Empty {
                    ActionOutcome::UserPlay(r, c)
                } else {
                    ActionOutcome::None
                }
            } else {
                ActionOutcome::None
            }
        };

        match outcome {
            ActionOutcome::None => {}
            ActionOutcome::Close => (self.on_close)(),
            ActionOutcome::Render => self.tui.request_render(Some(false)),
            ActionOutcome::UserPlay(r, c) => (self.on_user_play)(r, c),
        }
    }

    fn invalidate(&self) {
        self.mutable.lock().unwrap_or_else(|e| e.into_inner()).cached_width = 0;
    }

    fn render(&self, width: usize) -> Vec<String> {
        let mut m = self.mutable.lock().unwrap_or_else(|e| e.into_inner());
        if width == m.cached_width && m.cached_version == (m.version as i64) {
            return m.cached_lines.clone();
        }

        let bold = |s: &str| format!("\x1b[1m{s}{RESET}");
        let dim = |s: &str| format!("\x1b[2m{s}{RESET}");
        let blue = |s: &str| format!("\x1b[34m{s}{RESET}");
        let yellow = |s: &str| format!("\x1b[33m{s}{RESET}");
        let green = |s: &str| format!("\x1b[32m{s}{RESET}");

        let mut lines = Vec::new();
        let title_text = " Tic-Tac-Toe ";
        let title_len = visible_width(title_text).max(0) as usize;
        let border_len = width.saturating_sub(title_len);
        let left_border = border_len / 2;
        let right_border = border_len - left_border;
        lines.push(format!(
            "{}{}{}",
            dim(&"\u{2500}".repeat(left_border)),
            bold(&blue(title_text)),
            dim(&"\u{2500}".repeat(right_border))
        ));
        lines.push(String::new());

        if m.state.status != GameStatus::Playing {
            let status_text = match m.state.status {
                GameStatus::Draw => bold(&yellow("Draw!")),
                GameStatus::WinX => bold(&green("X wins!")),
                _ => bold(&yellow("O wins!")),
            };
            lines.push(center_pad(&status_text, width));
        } else if m.state.current_turn == Cell::X {
            lines.push(center_pad(
                &format!(
                    "Turn: {} (You)  {}  {} (Agent)",
                    bold(&blue("X")),
                    dim("|"),
                    bold(&yellow("O"))
                ),
                width,
            ));
        } else {
            lines.push(center_pad(
                &format!(
                    "{} (You)  {}  Turn: {} (Agent)",
                    blue("X"),
                    dim("|"),
                    bold(&yellow("O"))
                ),
                width,
            ));
        }

        lines.push(String::new());
        lines.push(String::new());
        lines.extend(render_visual_board(&m.state, width));
        lines.push(String::new());
        lines.push(String::new());

        let footer = if m.state.status != GameStatus::Playing {
            format!(
                "{} restart  {}  {}/{} quit",
                bold("R"),
                dim("|"),
                bold("Q"),
                bold("ESC")
            )
        } else if m.state.current_turn != m.state.user_mark {
            dim("Agent is thinking...")
        } else {
            format!(
                "{} move  {}  {} play  {}  {} quit",
                bold("\u{2190}\u{2191}\u{2193}\u{2192}"),
                dim("|"),
                bold("ENTER"),
                dim("|"),
                bold("ESC")
            )
        };
        lines.push(center_pad(&footer, width));
        lines.push(String::new());
        lines.push(dim(&"\u{2500}".repeat(width)));

        m.cached_lines = lines.clone();
        m.cached_width = width;
        m.cached_version = m.version as i64;
        lines
    }
}

struct BannerMessageComponent {
    title: String,
    details: Option<BoardDetails>,
    expanded: bool,
    theme: Theme,
}

impl Component for BannerMessageComponent {
    fn render(&self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        let title_len = visible_width(&self.title).max(0) as usize;
        let fill_len = width.saturating_sub(title_len + 2);
        let left_fill = fill_len / 2;
        let right_fill = fill_len - left_fill;
        lines.push(format!(
            "{} {} {}",
            self.theme.fg("dim", &"\u{2500}".repeat(left_fill)),
            self.title,
            self.theme.fg("dim", &"\u{2500}".repeat(right_fill))
        ));

        if self.expanded
            && let Some(details) = &self.details
        {
            lines.push(String::new());
            lines.extend(render_board_snapshot(&details.board, width));
        }
        lines
    }

    fn invalidate(&self) {}
}

struct GameOverMessageComponent {
    status: GameStatus,
    details: Option<BoardDetails>,
    theme: Theme,
}

impl Component for GameOverMessageComponent {
    fn render(&self, width: usize) -> Vec<String> {
        let hr = self.theme.fg("dim", &"\u{2500}".repeat(width));
        let mut lines = vec![hr.clone(), String::new()];

        let (title, sub) = match self.status {
            GameStatus::WinX => (
                self.theme
                    .bold(&self.theme.fg("accent", "\u{2605} Player X wins \u{2605}")),
                "You beat the agent.",
            ),
            GameStatus::WinO => (
                self.theme
                    .bold(&self.theme.fg("warning", "\u{2605} Player O wins \u{2605}")),
                "The agent beat you.",
            ),
            GameStatus::Draw => (
                self.theme.bold(&self.theme.fg("muted", "\u{2014} Draw \u{2014}")),
                "No winner.",
            ),
            GameStatus::Playing => (self.theme.bold("Game over"), ""),
        };

        for line in [title, self.theme.fg("dim", sub)] {
            let pad = width.saturating_sub(visible_width(&line).max(0) as usize);
            lines.push(format!("{}{line}", " ".repeat(pad / 2)));
        }

        lines.push(String::new());
        if let Some(details) = &self.details {
            lines.extend(render_board_snapshot(&details.board, width));
            lines.push(String::new());
        }
        lines.push(hr);
        lines
    }

    fn invalidate(&self) {}
}

struct TicTacToeShared {
    game_state: GameState,
    component: Option<Arc<TicTacToeComponent>>,
    game_active: bool,
}

impl TicTacToeShared {
    fn new() -> Self {
        Self {
            game_state: create_initial_state(),
            component: None,
            game_active: false,
        }
    }

    fn board_details(&self) -> BoardDetails {
        BoardDetails {
            board: self.game_state.board,
            agent_cursor_row: self.game_state.agent_cursor_row,
            agent_cursor_col: self.game_state.agent_cursor_col,
            status: self.game_state.status,
            current_turn: self.game_state.current_turn,
        }
    }
}

fn reconstruct_state(shared: &Mutex<TicTacToeShared>, ctx: &ExtensionContext) {
    let Ok(sm) = ctx.session_manager() else {
        return;
    };
    let branch = sm.get_branch(None);
    let mut guard = shared.lock().unwrap_or_else(|e| e.into_inner());
    guard.game_state = create_initial_state();
    guard.game_active = false;

    for entry in branch {
        let SessionEntry::Message(msg_entry) = entry else {
            continue;
        };
        let AgentMessage::Message(Message::ToolResult(tr)) = msg_entry.message else {
            continue;
        };
        if tr.tool_name != "tic_tac_toe" && tr.tool_name != "tic_tac_toe_see_board" {
            continue;
        }
        if let Some(details_val) = tr.details
            && let Ok(details) = serde_json::from_value::<BoardDetails>(details_val)
        {
            guard.game_state.board = details.board;
            guard.game_state.agent_cursor_row = details.agent_cursor_row;
            guard.game_state.agent_cursor_col = details.agent_cursor_col;
            guard.game_state.status = details.status;
            guard.game_state.current_turn = details.current_turn;
        }
    }
}

fn emit_game_over_message(pi: &ExtensionApi, status: GameStatus, details: &BoardDetails) {
    let label = match status {
        GameStatus::WinX => "Player X (human) wins",
        GameStatus::WinO => "Player O (agent) wins",
        GameStatus::Draw => "Draw",
        GameStatus::Playing => "Game over",
    };
    let _ = pi.send_message(
        CustomMessageDraft {
            custom_type: GAME_OVER_MESSAGE_TYPE.to_string(),
            content: CustomMessageContent::Text(format!("Game over: {label}.")),
            display: true,
            details: serde_json::to_value(details).ok(),
        },
        None,
    );
}

pub struct TicTacToeExtension;

#[async_trait]
impl Extension for TicTacToeExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let shared = Arc::new(Mutex::new(TicTacToeShared::new()));

        let s_start = Arc::clone(&shared);
        pi.on::<SessionStartEvent>(move |_event, ctx| {
            let shared_ref = Arc::clone(&s_start);
            Box::pin(async move {
                reconstruct_state(&shared_ref, &ctx);
                Ok(None)
            })
        });

        let s_tree = Arc::clone(&shared);
        pi.on::<SessionTreeEvent>(move |_event, ctx| {
            let shared_ref = Arc::clone(&s_tree);
            Box::pin(async move {
                reconstruct_state(&shared_ref, &ctx);
                Ok(None)
            })
        });

        pi.register_message_renderer(
            MOVE_MESSAGE_TYPE,
            Arc::new(|message, options, theme| {
                let details = message
                    .details
                    .as_ref()
                    .and_then(|d| serde_json::from_value::<BoardDetails>(d.clone()).ok());
                let turn_label = if details.as_ref().is_some_and(|d| d.current_turn == Cell::O) {
                    format!("{} (Agent)", theme.fg("warning", &theme.bold("O")))
                } else {
                    format!("{} (You)", theme.fg("accent", &theme.bold("X")))
                };
                let title = format!(
                    "{}  {}  next: {turn_label}",
                    theme.fg("accent", &theme.bold("Player X played")),
                    theme.fg("dim", "\u{2192}")
                );
                Some(Arc::new(BannerMessageComponent {
                    title,
                    details,
                    expanded: options.expanded,
                    theme,
                }))
            }),
        )?;

        pi.register_message_renderer(
            GAME_OVER_MESSAGE_TYPE,
            Arc::new(|message, _options, theme| {
                let details = message
                    .details
                    .as_ref()
                    .and_then(|d| serde_json::from_value::<BoardDetails>(d.clone()).ok());
                let status = details.as_ref().map(|d| d.status).unwrap_or(GameStatus::Draw);
                Some(Arc::new(GameOverMessageComponent { status, details, theme }))
            }),
        )?;

        let s_prompt = Arc::clone(&shared);
        pi.on::<BeforeAgentStartEvent>(move |event, _ctx| {
            let active = s_prompt.lock().unwrap_or_else(|e| e.into_inner()).game_active;
            Box::pin(async move {
                if !active {
                    return Ok(None);
                }
                let instructions = format!(
                    "\n\n## Tic-Tac-Toe (you are Player O)\n\n\
A tic-tac-toe game is in progress. The human is Player X. You are Player O.\n\
The human plays through a TUI; you play through the `tic_tac_toe` tool.\n\n\
### Turn protocol\n\n\
When the human plays, you receive a message that contains the cell X marked,\n\
the full board, and YOUR cursor position (Player O's cursor). The message is\n\
the source of truth for the board.\n\n\
Player O's cursor persists between O turns. It is reset to (row={AGENT_CURSOR_HOME_ROW}, col={AGENT_CURSOR_HOME_COL})\n\
only after a successful `play`. If a `play` fails (cell already taken), the\n\
cursor stays where it was, so you can move and retry.\n\n\
You may also call `tic_tac_toe_see_board` if you want the current board and\n\
your cursor position restated at any point. The user's cursor is private and\n\
is never shown to you.\n\n\
### The tool\n\n\
`tic_tac_toe` takes ONE action per call:\n\
- `move_up` / `move_down` / `move_left` / `move_right`: move YOUR cursor one cell (clamped at edges)\n\
- `play`: place O on the cell under YOUR cursor. Errors if the cell is not empty.\n\n\
There is no batched form. One call = one action.\n\n\
### CRITICAL: emit the whole turn in a single response\n\n\
To play at (r, c) from your cursor (r0, c0) emit, in order:\n\
- `move_down` (r - r0) times (or `move_up` (r0 - r) times if r < r0)\n\
- `move_right` (c - c0) times (or `move_left` (c0 - c) times if c < c0)\n\
- one call of `play`\n\n\
All of these tool calls MUST be emitted in the SAME assistant response, as\n\
separate tool_use blocks, before you stop. Do not:\n\
- split the sequence across multiple assistant responses,\n\
- wait for a move result before emitting the next move or `play`,\n\
- write any explanation or text between the tool calls,\n\
- call any other tool during your turn (except `tic_tac_toe_see_board` when you\n\
  explicitly need the state restated).\n\n\
Decide the target cell first, then dump every action for the turn in one go.\n\n\
### Examples (cursor starts at ({AGENT_CURSOR_HOME_ROW}, {AGENT_CURSOR_HOME_COL}))\n\n\
- Target (0,0): one call, `play`.\n\
- Target (0,2): `move_right`, `move_right`, `play`. Three calls, one response.\n\
- Target (1,1): `move_down`, `move_right`, `play`. Three calls, one response.\n\
- Target (2,2): `move_down`, `move_down`, `move_right`, `move_right`, `play`. Five calls, one response.\n\n\
### Strategy\n\n\
1. If you have two O's in a line with the third cell empty, win by playing there.\n\
2. Otherwise, if X has two in a line with the third cell empty, block there.\n\
3. Otherwise, prefer center, then corners, then edges.\n"
                );
                Ok(Some(BeforeAgentStartEventResult {
                    message: None,
                    system_prompt: Some(format!("{}{}", event.system_prompt, instructions)),
                }))
            })
        });

        let pi_cmd = pi.clone();
        let s_cmd = Arc::clone(&shared);
        pi.register_command(
            "tic-tac-toe",
            RegisteredCommandOptions {
                description: Some("Play tic-tac-toe against the agent".to_string()),
                get_argument_completions: None,
                handler: Arc::new(move |_args, ctx| {
                    let pi_outer = pi_cmd.clone();
                    let shared_outer = Arc::clone(&s_cmd);
                    Box::pin(async move {
                        let base = ctx.as_context();
                        let ui = base.ui()?;
                        if base.mode()? != ExtensionMode::Tui {
                            ui.notify("Tic-tac-toe requires interactive mode", Some(NotifyType::Error));
                            return Ok(());
                        }

                        reconstruct_state(&shared_outer, base);
                        let initial_state = {
                            let mut guard = shared_outer.lock().unwrap_or_else(|e| e.into_inner());
                            if guard.game_state.status != GameStatus::Playing {
                                guard.game_state = create_initial_state();
                            }
                            guard.game_active = true;
                            guard.game_state.clone()
                        };
                        let _ = pi_outer.set_session_name("Tic-Tac-Toe");

                        let pi_ui = pi_outer.clone();
                        let shared_ui = Arc::clone(&shared_outer);
                        let _ = ui
                            .custom(
                                Box::new(move |tui, _theme, _kb, done| {
                                    Box::pin(async move {
                                        let shared_close = Arc::clone(&shared_ui);
                                        let done_close = Arc::clone(&done);
                                        let on_close: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                                            {
                                                let mut guard = shared_close.lock().unwrap_or_else(|e| e.into_inner());
                                                guard.component = None;
                                                guard.game_active = false;
                                            }
                                            done_close(Value::Null);
                                        });

                                        let shared_play = Arc::clone(&shared_ui);
                                        let pi_play = pi_ui.clone();
                                        let on_user_play: Arc<dyn Fn(usize, usize) + Send + Sync> =
                                            Arc::new(move |row, col| {
                                                let (comp, snapshot, details) = {
                                                    let mut guard =
                                                        shared_play.lock().unwrap_or_else(|e| e.into_inner());
                                                    let user_mark = guard.game_state.user_mark;
                                                    guard.game_state.board[row][col] = user_mark;
                                                    guard.game_state.status = check_win(&guard.game_state.board);
                                                    if guard.game_state.status == GameStatus::Playing {
                                                        guard.game_state.current_turn = guard.game_state.agent_mark;
                                                    } else {
                                                        guard.game_active = false;
                                                    }
                                                    (
                                                        guard.component.clone(),
                                                        guard.game_state.clone(),
                                                        guard.board_details(),
                                                    )
                                                };

                                                if let Some(c) = comp {
                                                    c.update_state(snapshot.clone());
                                                }
                                                let _ = pi_play
                                                    .append_entry(SAVE_TYPE, serde_json::to_value(&details).ok());

                                                if snapshot.status == GameStatus::Playing {
                                                    let board_ascii = board_to_ascii(
                                                        &snapshot.board,
                                                        snapshot.agent_cursor_row,
                                                        snapshot.agent_cursor_col,
                                                    );
                                                    let _ = pi_play.send_message(
                                                        CustomMessageDraft {
                                                            custom_type: MOVE_MESSAGE_TYPE.to_string(),
                                                            content: CustomMessageContent::Text(format!(
                                                                "Player X played at (row={row}, col={col}). It is now Player O's turn.\n\n\
Board (your cursor marked with <>):\n{board_ascii}\n\n\
Your cursor is at (row={}, col={}). \
Decide your target cell, then emit every move_* and the final play \
as separate tic_tac_toe tool calls in THIS response.",
                                                                snapshot.agent_cursor_row, snapshot.agent_cursor_col
                                                            )),
                                                            display: true,
                                                            details: serde_json::to_value(&details).ok(),
                                                        },
                                                        Some(SendMessageOptions {
                                                            trigger_turn: Some(true),
                                                            deliver_as: None,
                                                        }),
                                                    );
                                                } else {
                                                    emit_game_over_message(&pi_play, snapshot.status, &details);
                                                }
                                            });

                                        let comp = Arc::new(TicTacToeComponent::new(
                                            tui,
                                            on_close,
                                            on_user_play,
                                            initial_state,
                                        ));
                                        shared_ui.lock().unwrap_or_else(|e| e.into_inner()).component =
                                            Some(Arc::clone(&comp));
                                        Ok(comp as Arc<dyn Component>)
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

        let action_schema = string_enum(
            ["move_up", "move_down", "move_left", "move_right", "play"],
            Some(StringEnumOptions {
                description: Some(
                    "The single action to perform this call. Emit multiple tic_tac_toe calls in one response to string actions together."
                        .to_string(),
                ),
                r#default: None,
            }),
        );

        let pi_tool = pi.clone();
        let s_tool = Arc::clone(&shared);
        pi.register_tool(ToolDefinition {
            name: "tic_tac_toe".to_string(),
            label: "Tic-Tac-Toe".to_string(),
            description: "Execute ONE tic-tac-toe action as Player O. `action` is exactly one of: move_up, move_down, move_left, move_right (move YOUR cursor one cell, clamped at edges), or play (place O under YOUR cursor; errors if the cell is not empty). There is no batched form. To play at (r, c) from your current cursor (r0, c0), emit the required move_down/move_up and move_right/move_left calls, then play, all as separate tool_use blocks in the SAME assistant response. Do not split the sequence across responses and do not wait for a result before emitting the next call. Your cursor position persists between turns and is reset to (0,0) only after a successful play."
                .to_string(),
            prompt_snippet: Some("Play a tic-tac-toe action (move_up/down/left/right or play) as Player O".to_string()),
            prompt_guidelines: Some(vec![
                "When it is your tic-tac-toe turn, decide the target cell first, then emit every move_* plus the final play as separate tic_tac_toe tool calls in a SINGLE assistant response. Never split them across responses or wait for intermediate results."
                    .to_string(),
                "Never ask the user for the board. The board and your cursor position are included in the user's move message; use tic_tac_toe_see_board if you need them restated."
                    .to_string(),
            ]),
            parameters: t::object([("action", action_schema)]),
            constrained_sampling: None,
            render_shell: None,
            prepare_arguments: None,
            output_schema: None,
            exposure: None,
            namespace: None,
            annotations: None,
            default_active: None,
            prepare_loadout: None,
            execution_mode: Some(ToolExecutionMode::Sequential),
            execute: Arc::new(move |_tool_call_id, params, _signal, _on_update, _ctx| {
                let pi_exec = pi_tool.clone();
                let shared_exec = Arc::clone(&s_tool);
                Box::pin(async move {
                    let action = params
                        .get("action")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| pi_js::Error::msg("Missing action parameter"))?
                        .to_string();

                    let delay_ms = match action.as_str() {
                        "move_up" | "move_down" | "move_left" | "move_right" => 300,
                        "play" => 0,
                        other => return Err(pi_js::Error::msg(format!("Unknown action: {other}"))),
                    };
                    if delay_ms > 0 {
                        pi_js::time::sleep(delay_ms, None).await;
                    }

                    enum StepOutcome {
                        Success {
                            result: String,
                            comp: Option<Arc<TicTacToeComponent>>,
                            snapshot: GameState,
                            details: BoardDetails,
                            game_over_status: Option<GameStatus>,
                        },
                        CellTaken {
                            err_msg: String,
                            comp: Option<Arc<TicTacToeComponent>>,
                            snapshot: GameState,
                            details: BoardDetails,
                        },
                        Error(String),
                    }

                    let outcome = {
                        let mut guard = shared_exec.lock().unwrap_or_else(|e| e.into_inner());
                        match action.as_str() {
                            "move_up" => {
                                if guard.game_state.agent_cursor_row > 0 {
                                    guard.game_state.agent_cursor_row -= 1;
                                }
                                let msg = format!(
                                    "Moved up. Cursor: ({}, {})",
                                    guard.game_state.agent_cursor_row, guard.game_state.agent_cursor_col
                                );
                                StepOutcome::Success {
                                    result: msg,
                                    comp: guard.component.clone(),
                                    snapshot: guard.game_state.clone(),
                                    details: guard.board_details(),
                                    game_over_status: None,
                                }
                            }
                            "move_down" => {
                                if guard.game_state.agent_cursor_row < 2 {
                                    guard.game_state.agent_cursor_row += 1;
                                }
                                let msg = format!(
                                    "Moved down. Cursor: ({}, {})",
                                    guard.game_state.agent_cursor_row, guard.game_state.agent_cursor_col
                                );
                                StepOutcome::Success {
                                    result: msg,
                                    comp: guard.component.clone(),
                                    snapshot: guard.game_state.clone(),
                                    details: guard.board_details(),
                                    game_over_status: None,
                                }
                            }
                            "move_left" => {
                                if guard.game_state.agent_cursor_col > 0 {
                                    guard.game_state.agent_cursor_col -= 1;
                                }
                                let msg = format!(
                                    "Moved left. Cursor: ({}, {})",
                                    guard.game_state.agent_cursor_row, guard.game_state.agent_cursor_col
                                );
                                StepOutcome::Success {
                                    result: msg,
                                    comp: guard.component.clone(),
                                    snapshot: guard.game_state.clone(),
                                    details: guard.board_details(),
                                    game_over_status: None,
                                }
                            }
                            "move_right" => {
                                if guard.game_state.agent_cursor_col < 2 {
                                    guard.game_state.agent_cursor_col += 1;
                                }
                                let msg = format!(
                                    "Moved right. Cursor: ({}, {})",
                                    guard.game_state.agent_cursor_row, guard.game_state.agent_cursor_col
                                );
                                StepOutcome::Success {
                                    result: msg,
                                    comp: guard.component.clone(),
                                    snapshot: guard.game_state.clone(),
                                    details: guard.board_details(),
                                    game_over_status: None,
                                }
                            }
                            _ => {
                                if guard.game_state.status != GameStatus::Playing {
                                    StepOutcome::Error(format!("Game is over ({}).", guard.game_state.status.as_str()))
                                } else if guard.game_state.current_turn != guard.game_state.agent_mark {
                                    StepOutcome::Error("It is not your turn.".to_string())
                                } else {
                                    let r = guard.game_state.agent_cursor_row;
                                    let c = guard.game_state.agent_cursor_col;
                                    if guard.game_state.board[r][c] != Cell::Empty {
                                        let existing = guard.game_state.board[r][c].as_str();
                                        StepOutcome::CellTaken {
                                            err_msg: format!(
                                                "Cell ({r},{c}) is already {existing}. Your cursor is still at ({r},{c}). Move to an empty cell and retry play."
                                            ),
                                            comp: guard.component.clone(),
                                            snapshot: guard.game_state.clone(),
                                            details: guard.board_details(),
                                        }
                                    } else {
                                        let agent_mark = guard.game_state.agent_mark;
                                        guard.game_state.board[r][c] = agent_mark;
                                        guard.game_state.status = check_win(&guard.game_state.board);
                                        guard.game_state.agent_cursor_row = AGENT_CURSOR_HOME_ROW;
                                        guard.game_state.agent_cursor_col = AGENT_CURSOR_HOME_COL;

                                        let mut game_over_status = None;
                                        let msg = match guard.game_state.status {
                                            GameStatus::Playing => {
                                                guard.game_state.current_turn = guard.game_state.user_mark;
                                                format!(
                                                    "Placed O at ({r},{c}). Cursor reset to ({AGENT_CURSOR_HOME_ROW},{AGENT_CURSOR_HOME_COL}). Your turn, X!"
                                                )
                                            }
                                            GameStatus::WinO => {
                                                guard.game_active = false;
                                                game_over_status = Some(GameStatus::WinO);
                                                format!("Placed O at ({r},{c}). Player O wins!")
                                            }
                                            GameStatus::Draw => {
                                                guard.game_active = false;
                                                game_over_status = Some(GameStatus::Draw);
                                                format!("Placed O at ({r},{c}). It's a draw!")
                                            }
                                            GameStatus::WinX => format!("Placed O at ({r},{c})."),
                                        };
                                        StepOutcome::Success {
                                            result: msg,
                                            comp: guard.component.clone(),
                                            snapshot: guard.game_state.clone(),
                                            details: guard.board_details(),
                                            game_over_status,
                                        }
                                    }
                                }
                            }
                        }
                    };

                    match outcome {
                        StepOutcome::Error(msg) => Err(pi_js::Error::msg(msg)),
                        StepOutcome::CellTaken {
                            err_msg,
                            comp,
                            snapshot,
                            details,
                        } => {
                            if let Some(c) = comp {
                                c.update_state(snapshot);
                            }
                            let _ = pi_exec.append_entry(SAVE_TYPE, serde_json::to_value(&details).ok());
                            Err(pi_js::Error::msg(err_msg))
                        }
                        StepOutcome::Success {
                            result,
                            comp,
                            snapshot,
                            details,
                            game_over_status,
                        } => {
                            if let Some(status) = game_over_status {
                                emit_game_over_message(&pi_exec, status, &details);
                            }
                            if let Some(c) = comp {
                                c.update_state(snapshot);
                            }
                            let details_val = serde_json::to_value(&details).ok();
                            let _ = pi_exec.append_entry(SAVE_TYPE, details_val.clone());
                            Ok(AgentToolResult {
                                content: vec![UserContentBlock::Text(TextContent::new(result))],
                                details: details_val,
                                structured_content: None,
                                usage: None,
                                is_error: None,
                                terminate: None,
                                control: None,
                            })
                        }
                    }
                })
            }),
            render_call: Some(Arc::new(|args, theme, _context| {
                let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
                let msg = format!("{}{}", theme.fg("toolTitle", &theme.bold("tic_tac_toe ")), theme.fg("muted", action));
                Arc::new(Text::new(Some(&msg), Some(0), Some(0), None))
            })),
            render_result: Some(Arc::new(
                |result, options: ToolRenderResultOptions, theme, context| {
                    let details = result
                        .details
                        .as_ref()
                        .and_then(|d| serde_json::from_value::<BoardDetails>(d.clone()).ok());
                    let msg = match result.content.first() {
                        Some(UserContentBlock::Text(tc)) => tc.text.as_str(),
                        _ => "",
                    };
                    let prefix = if context.is_error {
                        theme.fg("error", "\u{2717} ")
                    } else {
                        theme.fg("success", "\u{2713} ")
                    };
                    let summary = format!("{prefix}{}", theme.fg("muted", msg));
                    if options.expanded
                        && let Some(details) = details
                    {
                        return Arc::new(BannerMessageComponent {
                            title: summary,
                            details: Some(details),
                            expanded: true,
                            theme,
                        });
                    }
                    Arc::new(Text::new(Some(&summary), Some(0), Some(0), None))
                },
            )),
        })?;

        let s_see = Arc::clone(&shared);
        pi.register_tool(ToolDefinition {
            name: "tic_tac_toe_see_board".to_string(),
            label: "See Board".to_string(),
            description: "Return the current tic-tac-toe board state and YOUR cursor position (Player O). Takes no arguments. Use this if you need the current state restated mid-turn (for example after a failed play). The user's cursor is never exposed."
                .to_string(),
            prompt_snippet: Some("Inspect the tic-tac-toe board and your cursor".to_string()),
            prompt_guidelines: None,
            parameters: t::object([] as [(&str, pi_js::vendor::typebox::Schema); 0]),
            constrained_sampling: None,
            render_shell: None,
            prepare_arguments: None,
            output_schema: None,
            exposure: None,
            namespace: None,
            annotations: None,
            default_active: None,
            prepare_loadout: None,
            execution_mode: None,
            execute: Arc::new(move |_tool_call_id, _params, _signal, _on_update, _ctx| {
                let shared_exec = Arc::clone(&s_see);
                Box::pin(async move {
                    let (snapshot, details) = {
                        let guard = shared_exec.lock().unwrap_or_else(|e| e.into_inner());
                        (guard.game_state.clone(), guard.board_details())
                    };
                    let board_ascii =
                        board_to_ascii(&snapshot.board, snapshot.agent_cursor_row, snapshot.agent_cursor_col);
                    let turn = if snapshot.current_turn == snapshot.agent_mark {
                        "Player O (you)"
                    } else {
                        "Player X"
                    };
                    let text = format!(
                        "Board (your cursor marked with <>):\n{board_ascii}\n\n\
Your cursor: (row={}, col={})\n\
Status: {}\n\
Turn: {turn}",
                        snapshot.agent_cursor_row,
                        snapshot.agent_cursor_col,
                        snapshot.status.as_str()
                    );
                    Ok(AgentToolResult {
                        content: vec![UserContentBlock::Text(TextContent::new(text))],
                        details: serde_json::to_value(&details).ok(),
                        structured_content: None,
                        usage: None,
                        is_error: None,
                        terminate: None,
                        control: None,
                    })
                })
            }),
            render_call: Some(Arc::new(|_args, theme, _context| {
                Arc::new(Text::new(
                    Some(&theme.fg("toolTitle", &theme.bold("tic_tac_toe_see_board"))),
                    Some(0),
                    Some(0),
                    None,
                ))
            })),
            render_result: Some(Arc::new(
                |result, options: ToolRenderResultOptions, theme, _context| {
                    let details = result
                        .details
                        .as_ref()
                        .and_then(|d| serde_json::from_value::<BoardDetails>(d.clone()).ok());
                    let r = details.as_ref().map(|d| d.agent_cursor_row).unwrap_or(0);
                    let c = details.as_ref().map(|d| d.agent_cursor_col).unwrap_or(0);
                    let summary = format!(
                        "{}{}",
                        theme.fg("success", "\u{2713} "),
                        theme.fg("muted", &format!("cursor ({r},{c})"))
                    );
                    if options.expanded
                        && let Some(details) = details
                    {
                        return Arc::new(BannerMessageComponent {
                            title: summary,
                            details: Some(details),
                            expanded: true,
                            theme,
                        });
                    }
                    Arc::new(Text::new(Some(&summary), Some(0), Some(0), None))
                },
            )),
        })?;

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(TicTacToeExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
