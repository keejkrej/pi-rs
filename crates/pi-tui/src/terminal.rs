use anyhow::Result;
use std::io::{self, Write};

use crate::keys::set_kitty_protocol_active;

pub trait Terminal {
    fn start(&mut self) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    fn write(&mut self, data: &str) -> Result<()>;
    fn columns(&self) -> usize;
    fn rows(&self) -> usize;

    fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) -> Result<()> {
        Ok(())
    }

    fn kitty_protocol_active(&self) -> bool {
        false
    }

    fn move_by(&mut self, lines: isize) -> Result<()> {
        if lines < 0 {
            self.write(&format!("\u{1b}[{}A", lines.unsigned_abs()))
        } else if lines > 0 {
            self.write(&format!("\u{1b}[{}B", lines))
        } else {
            Ok(())
        }
    }

    fn hide_cursor(&mut self) -> Result<()> {
        self.write("\u{1b}[?25l")
    }

    fn show_cursor(&mut self) -> Result<()> {
        self.write("\u{1b}[?25h")
    }

    fn clear_line(&mut self) -> Result<()> {
        self.write("\u{1b}[K")
    }

    fn clear_from_cursor(&mut self) -> Result<()> {
        self.write("\u{1b}[J")
    }

    fn clear_screen(&mut self) -> Result<()> {
        self.write("\u{1b}[2J\u{1b}[H")
    }

    fn set_title(&mut self, title: &str) -> Result<()> {
        self.write(&format!("\u{1b}]0;{}\u{7}", title))
    }

    fn set_progress(&mut self, active: bool) -> Result<()> {
        if active {
            self.write("\u{1b}]9;4;3\u{7}")
        } else {
            self.write("\u{1b}]9;4;0;\u{7}")
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProcessTerminal {
    columns: usize,
    rows: usize,
    raw_enabled: bool,
    kitty_protocol_active: bool,
    modify_other_keys_active: bool,
    progress_active: bool,
}

impl Default for ProcessTerminal {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessTerminal {
    pub fn new() -> Self {
        let (columns, rows) = current_terminal_size();
        Self {
            columns,
            rows,
            raw_enabled: false,
            kitty_protocol_active: false,
            modify_other_keys_active: false,
            progress_active: false,
        }
    }
}

impl Terminal for ProcessTerminal {
    fn start(&mut self) -> Result<()> {
        if crossterm::terminal::enable_raw_mode().is_ok() {
            self.raw_enabled = true;
        }
        let (columns, rows) = current_terminal_size();
        self.columns = columns;
        self.rows = rows;
        self.hide_cursor()?;
        self.write("\u{1b}[?2004h")?;
        self.write("\u{1b}[?u")?;
        // TypeScript waits briefly for a Kitty response before enabling this
        // fallback. The Rust port has no async stdin loop here, so enable the
        // fallback eagerly; terminals that support it accept this xterm mode.
        self.write("\u{1b}[>4;2m")?;
        self.modify_other_keys_active = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if self.progress_active {
            self.write("\u{1b}]9;4;0;\u{7}")?;
            self.progress_active = false;
        }
        self.write("\u{1b}[?2004l")?;
        if self.kitty_protocol_active {
            self.write("\u{1b}[<u")?;
            self.kitty_protocol_active = false;
            set_kitty_protocol_active(false);
        }
        if self.modify_other_keys_active {
            self.write("\u{1b}[>4;0m")?;
            self.modify_other_keys_active = false;
        }
        self.show_cursor()?;
        if self.raw_enabled {
            let _ = crossterm::terminal::disable_raw_mode();
            self.raw_enabled = false;
        }
        io::stdout().flush()?;
        Ok(())
    }

    fn write(&mut self, data: &str) -> Result<()> {
        let mut stdout = io::stdout();
        stdout.write_all(data.as_bytes())?;
        stdout.flush()?;
        Ok(())
    }

    fn columns(&self) -> usize {
        current_terminal_size().0
    }

    fn rows(&self) -> usize {
        current_terminal_size().1
    }

    fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) -> Result<()> {
        if self.kitty_protocol_active {
            self.write("\u{1b}[<u")?;
            self.kitty_protocol_active = false;
            set_kitty_protocol_active(false);
        }
        if self.modify_other_keys_active {
            self.write("\u{1b}[>4;0m")?;
            self.modify_other_keys_active = false;
        }
        Ok(())
    }

    fn kitty_protocol_active(&self) -> bool {
        self.kitty_protocol_active
    }

    fn set_progress(&mut self, active: bool) -> Result<()> {
        self.progress_active = active;
        if active {
            self.write("\u{1b}]9;4;3\u{7}")
        } else {
            self.write("\u{1b}]9;4;0;\u{7}")
        }
    }
}

fn current_terminal_size() -> (usize, usize) {
    crossterm::terminal::size()
        .map(|(columns, rows)| (columns as usize, rows as usize))
        .unwrap_or_else(|_| {
            let columns = std::env::var("COLUMNS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(80);
            let rows = std::env::var("LINES")
                .or_else(|_| std::env::var("ROWS"))
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(24);
            (columns, rows)
        })
}
