//! Port of packages/mcp/src/transports/pipe.ts

#![allow(dead_code, unused_variables)]

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use pi_js::{Error, Unsubscribe};

use crate::protocol::jsonrpc::{JsonRpcMessage, McpConnectionClosedError};
use crate::transports::lines::{JsonRpcLineReader, JsonRpcLineReaderOptions};
use crate::transports::transport::{
    McpTransport, McpTransportCloseListener, McpTransportErrorListener, McpTransportMessageListener, TransportEvents,
};

const DEFAULT_CLOSE_TIMEOUT_MS: i64 = 2_000;

/// A running program's stdio, as `@earendil-works/pi-env`'s `RemoteProcess` provides it.
#[async_trait]
pub trait McpPipe: Send + Sync {
    fn write(&self, data: &[u8]);
    fn close_stdin(&self);
    /// Kill the program (and what it started). The pipe owns cleaning up what the program started.
    fn close(&self);
    /// Settles when the program has ended, whatever the reason; it must also settle after `close()`.
    async fn exited(&self) -> pi_js::Result<()>;
}

/// Handlers `PipeTransportOptions.open` connects to the program's output.
#[derive(Clone)]
pub struct McpPipeHandlers {
    pub on_stdout: Arc<dyn Fn(&[u8]) + Send + Sync>,
    pub on_stderr: Arc<dyn Fn(&[u8]) + Send + Sync>,
}

pub type PipeOpenFn =
    Arc<dyn Fn(McpPipeHandlers) -> Pin<Box<dyn Future<Output = pi_js::Result<Arc<dyn McpPipe>>> + Send>> + Send + Sync>;

#[derive(Clone)]
pub struct PipeTransportOptions {
    /// Start the program; `on_stdout`/`on_stderr` receive its output from then on.
    pub open: PipeOpenFn,
    pub max_message_bytes: Option<i64>,
    pub max_stderr_bytes: Option<i64>,
    pub on_stderr: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    /// After closing stdin, how long the program may take to exit before `close()` kills it. Default 2000.
    pub close_timeout_ms: Option<i64>,
}

struct PipeTransportState {
    pipe: Option<Arc<dyn McpPipe>>,
    started: bool,
    closed: bool,
    ended: bool,
}

struct PipeTransportInner {
    options: PipeTransportOptions,
    events: TransportEvents,
    lines: JsonRpcLineReader,
    state: Mutex<PipeTransportState>,
    exit_notify: tokio::sync::Notify,
}

/// MCP's stdio transport over a program that something else runs, for example on another machine through pi-env.
/// The transport closes when the program ends; a rejected `exited` that `close()` did not cause is reported as an
/// error first.
#[derive(Clone)]
pub struct PipeTransport {
    inner: Arc<PipeTransportInner>,
}

impl PipeTransport {
    pub fn new(options: PipeTransportOptions) -> Self {
        let events = TransportEvents::new();
        let events_msg = events.clone();
        let events_err = events.clone();
        let lines = JsonRpcLineReader::new(JsonRpcLineReaderOptions {
            max_message_bytes: options.max_message_bytes,
            max_stderr_bytes: options.max_stderr_bytes,
            on_stderr: options.on_stderr.clone(),
            on_message: Arc::new(move |message| events_msg.emit_message(message)),
            on_error: Arc::new(move |error| events_err.emit_error(error)),
        });
        Self {
            inner: Arc::new(PipeTransportInner {
                options,
                events,
                lines,
                state: Mutex::new(PipeTransportState {
                    pipe: None,
                    started: false,
                    closed: false,
                    ended: false,
                }),
                exit_notify: tokio::sync::Notify::new(),
            }),
        }
    }

    pub fn options(&self) -> &PipeTransportOptions {
        &self.inner.options
    }

    /// The last `max_stderr_bytes` (default 64 KiB) of the program's stderr.
    pub fn stderr(&self) -> String {
        self.inner.lines.stderr()
    }

    /// The program ended: report an unfinished message, then close.
    fn exit(&self) {
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.ended = true;
        }
        self.inner.lines.end();
        self.inner.events.emit_close();
        self.inner.exit_notify.notify_waiters();
    }

    /// `exited` rejected: an error unless `close()` caused it, then the same as an exit.
    fn fail(&self, error: Error) {
        let closed = self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).closed;
        if !closed {
            self.inner.events.emit_error(&error);
        }
        self.exit();
    }
}

#[async_trait]
impl McpTransport for PipeTransport {
    async fn start(&self) -> pi_js::Result<()> {
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.started {
                return Err(Error::msg("MCP pipe transport already started"));
            }
            if state.closed {
                return Err(Error::typed(
                    "McpConnectionClosedError",
                    McpConnectionClosedError::new(None),
                ));
            }
            state.started = true;
        }

        let lines_stdout = self.inner.lines.clone();
        let lines_stderr = self.inner.lines.clone();
        let handlers = McpPipeHandlers {
            on_stdout: Arc::new(move |bytes| lines_stdout.read_stdout(bytes)),
            on_stderr: Arc::new(move |bytes| lines_stderr.read_stderr(bytes)),
        };

        let pipe = (self.inner.options.open)(handlers).await?;

        let closed_while_opening = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.closed {
                true
            } else {
                state.pipe = Some(pipe.clone());
                false
            }
        };

        if closed_while_opening {
            // close() ran while the program was starting and found nothing to shut down. Nobody else awaits
            // `exited`, and a pipe may reject it after a kill.
            let pipe_bg = pipe.clone();
            tokio::spawn(async move {
                let _ = pipe_bg.exited().await;
            });
            pipe.close();
            return Err(Error::typed(
                "McpConnectionClosedError",
                McpConnectionClosedError::new(None),
            ));
        }

        let this = self.clone();
        let pipe_wait = pipe;
        tokio::spawn(async move {
            match pipe_wait.exited().await {
                Ok(()) => this.exit(),
                Err(error) => this.fail(error),
            }
        });

        Ok(())
    }

    async fn send(&self, message: JsonRpcMessage) -> pi_js::Result<()> {
        let pipe = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.closed || state.ended {
                None
            } else {
                state.pipe.clone()
            }
        };
        let Some(pipe) = pipe else {
            return Err(Error::typed(
                "McpConnectionClosedError",
                McpConnectionClosedError::new(None),
            ));
        };
        let payload = format!("{}\n", pi_js::json::stringify(&message));
        pipe.write(payload.as_bytes());
        Ok(())
    }

    async fn close(&self) -> pi_js::Result<()> {
        let notified = self.inner.exit_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();

        let (pipe, ended) = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.closed {
                return Ok(());
            }
            state.closed = true;
            (state.pipe.clone(), state.ended)
        };

        let Some(pipe) = pipe else {
            self.inner.events.emit_close();
            return Ok(());
        };
        if ended {
            return Ok(());
        }

        // Shutdown per the spec: close stdin and let the program exit, then kill it.
        pipe.close_stdin();
        let close_timeout_ms = self
            .inner
            .options
            .close_timeout_ms
            .unwrap_or(DEFAULT_CLOSE_TIMEOUT_MS)
            .max(0) as u64;
        let exited_in_time = tokio::select! {
            () = &mut notified => true,
            _ = pi_js::time::sleep(close_timeout_ms, None) => false,
        };
        if exited_in_time {
            return Ok(());
        }
        pipe.close();
        notified.await;
        Ok(())
    }

    fn on_message(&self, listener: McpTransportMessageListener) -> Unsubscribe {
        self.inner.events.on_message(listener)
    }

    fn on_error(&self, listener: McpTransportErrorListener) -> Unsubscribe {
        self.inner.events.on_error(listener)
    }

    fn on_close(&self, listener: McpTransportCloseListener) -> Unsubscribe {
        self.inner.events.on_close(listener)
    }
}
