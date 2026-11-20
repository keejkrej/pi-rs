//! Port of packages/coding-agent/src/cli/auth-terminal.ts

use std::io::IsTerminal;
use std::sync::Arc;

use pi_js::abort::AbortSignal;
use pi_js::text::Utf8StreamDecoder;
use pi_js::{Error, Result};
use tokio::io::{AsyncBufRead, AsyncReadExt};

use crate::utils::open_browser::open_browser;
use crate::utils::terminal_question::{question_terminal, question_terminal_with_reader};

/// Output shared by the terminal authentication commands.
pub trait AuthOutput: Send + Sync {
    fn write(&self, line: &str);
    fn error(&self, line: &str);
}

/// Line-based terminal I/O for `pi auth login`. Prompts fail when `signal` aborts.
#[async_trait::async_trait]
pub trait LoginTerminal: AuthOutput {
    /// Whether stdin and stdout are a terminal; login prompts need one.
    fn interactive(&self) -> bool;
    async fn question(&self, message: &str, signal: &AbortSignal) -> Result<String>;
    /// Read a line without echoing it.
    async fn secret(&self, message: &str, signal: &AbortSignal) -> Result<String>;
    fn open_url(&self, url: &str);
}

pub struct ProcessAuthOutput;

impl AuthOutput for ProcessAuthOutput {
    fn write(&self, line: &str) {
        pi_js::console::log(line);
    }

    fn error(&self, line: &str) {
        pi_js::console::error(line);
    }
}

pub struct ProcessLoginTerminal {
    interactive: bool,
    on_interrupt: Arc<dyn Fn() + Send + Sync>,
}

/// Terminal I/O on stdin and stdout. `on_interrupt` runs on Ctrl+C or EOF in a question, or Ctrl+C in a secret.
pub fn create_process_login_terminal(on_interrupt: Arc<dyn Fn() + Send + Sync>) -> ProcessLoginTerminal {
    ProcessLoginTerminal {
        interactive: std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        on_interrupt,
    }
}

#[async_trait::async_trait]
impl AuthOutput for ProcessLoginTerminal {
    fn write(&self, line: &str) {
        pi_js::console::log(line);
    }

    fn error(&self, line: &str) {
        pi_js::console::error(line);
    }
}

#[async_trait::async_trait]
impl LoginTerminal for ProcessLoginTerminal {
    fn interactive(&self) -> bool {
        self.interactive
    }

    async fn question(&self, message: &str, signal: &AbortSignal) -> Result<String> {
        question_terminal(message, Some(signal), Some(&*self.on_interrupt)).await
    }

    async fn secret(&self, message: &str, signal: &AbortSignal) -> Result<String> {
        read_secret(message, signal, &*self.on_interrupt).await
    }

    fn open_url(&self, url: &str) {
        open_browser(url);
    }
}

impl ProcessLoginTerminal {
    #[doc(hidden)]
    pub async fn question_with_reader<R: AsyncBufRead + Unpin>(
        &self,
        message: &str,
        reader: R,
        signal: &AbortSignal,
    ) -> Result<String> {
        question_terminal_with_reader(message, reader, Some(signal), Some(&*self.on_interrupt)).await
    }
}

struct RawModeGuard;

impl RawModeGuard {
    fn enable() -> Self {
        let _ = crossterm::terminal::enable_raw_mode();
        Self
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
        pi_js::console::stdout_write("\n");
    }
}

async fn read_secret(message: &str, signal: &AbortSignal, on_interrupt: &(dyn Fn() + Send + Sync)) -> Result<String> {
    if signal.aborted() {
        return Err(Error::msg("Login cancelled"));
    }

    pi_js::console::stdout_write(message);
    let _guard = RawModeGuard::enable();

    let mut stdin = tokio::io::stdin();
    let mut decoder = Utf8StreamDecoder::new();
    let mut value = String::new();
    let mut buf = [0u8; 64];
    let token = signal.token();

    loop {
        tokio::select! {
            _ = token.cancelled() => {
                return Err(Error::msg("Login cancelled"));
            }
            res = stdin.read(&mut buf) => {
                match res {
                    Ok(0) => {
                        on_interrupt();
                        return Err(Error::msg("Login cancelled"));
                    }
                    Ok(n) => {
                        let chunk = decoder.decode(&buf[..n], true);
                        // Ignore escape sequences such as arrow keys.
                        if chunk.starts_with('\x1b') {
                            continue;
                        }
                        for ch in chunk.chars() {
                            if ch == '\r' || ch == '\n' {
                                return Ok(value);
                            }
                            if ch == '\u{0003}' {
                                on_interrupt();
                                return Err(Error::msg("Login cancelled"));
                            }
                            if ch == '\u{007f}' || ch == '\u{0008}' {
                                value.pop();
                            } else if ch >= ' ' {
                                value.push(ch);
                            }
                        }
                    }
                    Err(err) => return Err(Error::msg(err.to_string())),
                }
            }
        }
    }
}
