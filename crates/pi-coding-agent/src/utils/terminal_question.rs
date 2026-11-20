//! Port of packages/coding-agent/src/utils/terminal-question.ts

use pi_js::abort::AbortSignal;
use pi_js::{Error, Result};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, BufReader};

/// Read one line from `stdin`, failing on abort, Ctrl+C, or EOF.
pub async fn question_terminal(
    message: &str,
    signal: Option<&AbortSignal>,
    on_interrupt: Option<&(dyn Fn() + Send + Sync)>,
) -> Result<String> {
    let reader = BufReader::new(tokio::io::stdin());
    question_terminal_with_reader(message, reader, signal, on_interrupt).await
}

/// Reader-injected variant of [`question_terminal`] for deterministic offline tests.
pub async fn question_terminal_with_reader<R: AsyncBufRead + Unpin>(
    message: &str,
    mut reader: R,
    signal: Option<&AbortSignal>,
    on_interrupt: Option<&(dyn Fn() + Send + Sync)>,
) -> Result<String> {
    if let Some(sig) = signal {
        sig.throw_if_aborted()?;
    }

    pi_js::console::stderr_write(message);

    let mut line = String::new();

    let token = signal.map(AbortSignal::token);
    tokio::select! {
        res = reader.read_line(&mut line) => {
            match res {
                Ok(0) => {
                    if let Some(cb) = on_interrupt {
                        cb();
                    }
                    Err(Error::js("AbortError", "Input stream closed"))
                }
                Ok(_) => {
                    while line.ends_with('\n') || line.ends_with('\r') {
                        line.pop();
                    }
                    Ok(line)
                }
                Err(err) => Err(Error::msg(err.to_string())),
            }
        }
        ctrl_c = tokio::signal::ctrl_c() => {
            let _ = ctrl_c;
            if let Some(cb) = on_interrupt {
                cb();
            }
            Err(Error::js("AbortError", "Interrupted"))
        }
        () = async {
            if let Some(ref t) = token {
                t.cancelled().await;
            } else {
                std::future::pending::<()>().await;
            }
        } => {
            Err(Error::js("AbortError", "Request aborted"))
        }
    }
}
