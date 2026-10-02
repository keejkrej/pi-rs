//! pi_js::console (Rust-only; API contract: PORTING.md Appendix A).
//!
//! `console.log` / `console.error` / `console.warn` and `process.stdout.write` /
//! `process.stderr.write`. Each call writes its bytes and flushes, so output interleaves in call
//! order like Node's synchronous TTY/file writes. Write errors (e.g. a closed pipe) are ignored.
//!
//! Tests capture output with [`testing::capture`]: while the returned [`testing::Capture`] is
//! alive, writes made on the current thread go to it instead of the real streams (thread-local,
//! like `fetch::testing::mock_fetch`, so parallel tests do not see each other's output).

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

#[derive(Default)]
struct Buffers {
    stdout: String,
    stderr: String,
}

thread_local! {
    static CAPTURES: RefCell<Vec<(u64, Rc<RefCell<Buffers>>)>> = const { RefCell::new(Vec::new()) };
    static NEXT_CAPTURE_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[derive(Clone, Copy)]
enum Stream {
    Stdout,
    Stderr,
}

fn write(stream: Stream, s: &str) {
    let captured = CAPTURES.with(|c| {
        let captures = c.borrow();
        match captures.last() {
            Some((_, buffers)) => {
                let mut b = buffers.borrow_mut();
                match stream {
                    Stream::Stdout => b.stdout.push_str(s),
                    Stream::Stderr => b.stderr.push_str(s),
                }
                true
            }
            None => false,
        }
    });
    if captured {
        return;
    }
    match stream {
        Stream::Stdout => {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(s.as_bytes());
            let _ = out.flush();
        }
        Stream::Stderr => {
            let mut err = std::io::stderr().lock();
            let _ = err.write_all(s.as_bytes());
            let _ = err.flush();
        }
    }
}

/// `console.log(s)`: `s` plus `\n` on stdout.
pub fn log(s: &str) {
    write(Stream::Stdout, &format!("{s}\n"));
}

/// `console.info(s)`: same as [`log`].
pub fn info(s: &str) {
    log(s);
}

/// `console.debug(s)`: same as [`log`].
pub fn debug(s: &str) {
    log(s);
}

/// `console.error(s)`: `s` plus `\n` on stderr.
pub fn error(s: &str) {
    write(Stream::Stderr, &format!("{s}\n"));
}

/// `console.warn(s)`: same as [`error`].
pub fn warn(s: &str) {
    error(s);
}

/// `process.stdout.write(s)`.
pub fn stdout_write(s: &str) {
    write(Stream::Stdout, s);
}

/// `process.stderr.write(s)`.
pub fn stderr_write(s: &str) {
    write(Stream::Stderr, s);
}

pub mod testing {
    use super::*;

    /// Captured console output of the current thread; capturing stops when it drops. Nested
    /// captures stack: the most recent live one receives the output.
    pub struct Capture {
        id: u64,
        buffers: Rc<RefCell<Buffers>>,
    }

    /// Starts capturing `stdout` / `stderr` writes made on this thread
    /// (`vi.spyOn(console, "log")`, `vi.spyOn(process.stdout, "write")`).
    pub fn capture() -> Capture {
        let id = NEXT_CAPTURE_ID.with(|n| {
            let id = n.get();
            n.set(id + 1);
            id
        });
        let buffers = Rc::new(RefCell::new(Buffers::default()));
        CAPTURES.with(|c| c.borrow_mut().push((id, buffers.clone())));
        Capture { id, buffers }
    }

    impl Capture {
        /// Everything written to stdout so far.
        pub fn stdout(&self) -> String {
            self.buffers.borrow().stdout.clone()
        }

        /// Everything written to stderr so far.
        pub fn stderr(&self) -> String {
            self.buffers.borrow().stderr.clone()
        }

        /// Discards the output captured so far.
        pub fn clear(&self) {
            let mut b = self.buffers.borrow_mut();
            b.stdout.clear();
            b.stderr.clear();
        }
    }

    impl Drop for Capture {
        fn drop(&mut self) {
            // `try_with`: the thread-local may already be gone during thread teardown.
            let _ = CAPTURES.try_with(|c| c.borrow_mut().retain(|(id, _)| *id != self.id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_splits_stdout_and_stderr_and_nests() {
        let outer = testing::capture();
        log("out");
        info("info");
        debug("debug");
        error("err");
        warn("warn");
        stdout_write("raw-out");
        stderr_write("raw-err");
        assert_eq!(outer.stdout(), "out\ninfo\ndebug\nraw-out");
        assert_eq!(outer.stderr(), "err\nwarn\nraw-err");

        {
            let inner = testing::capture();
            log("in");
            stdout_write("more");
            error("inner-err");
            assert_eq!(inner.stdout(), "in\nmore");
            assert_eq!(inner.stderr(), "inner-err\n");
            assert_eq!(outer.stdout(), "out\ninfo\ndebug\nraw-out");
            assert_eq!(outer.stderr(), "err\nwarn\nraw-err");
        }

        log("after");
        assert_eq!(outer.stdout(), "out\ninfo\ndebug\nraw-outafter\n");
        outer.clear();
        assert_eq!(outer.stdout(), "");
        assert_eq!(outer.stderr(), "");
        stdout_write("only");
        assert_eq!(outer.stdout(), "only");
    }
}
