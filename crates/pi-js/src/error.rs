//! pi_js::error (Rust-only; API contract: PORTING.md Appendix A).
//!
//! [`Error`] models a JS `throw`: every variant's `Display` is the JS `error.message`, and
//! [`Error::to_js_string`] is `String(error)` (`"<name>: <message>"`).
//!
//! - `throw new Error(m)` is `Err(Error::msg(m))`; `new TypeError(m)` is `Error::js("TypeError", m)`.
//! - `err.code === "ENOENT"` is `err.code() == Some("ENOENT")`.
//! - `err.name === "AbortError"` is `err.is_abort()`.
//! - `err instanceof MyError` is `err.downcast_ref::<MyError>()` for `Error::typed("MyError", e)`.

use std::error::Error as StdError;
use std::fmt;

use crate::abort::AbortReason;

/// `Result<T, pi_js::Error>`.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A thrown JS value.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `Error` / `TypeError` / `RangeError` / `SyntaxError` (and other plain JS errors).
    #[error("{0}")]
    Js(JsError),
    /// The reason of an aborted `AbortSignal` (`AbortError`, `TimeoutError` or a custom reason).
    #[error("{}", .0.message)]
    Abort(AbortReason),
    /// A Node system error (fs, net, child_process), e.g.
    /// `ENOENT: no such file or directory, open '/x'`.
    #[error("{0}")]
    Node(NodeError),
    /// An instance of a TS `Error` subclass: the class name plus the Rust error struct.
    #[error("{1}")]
    Typed(&'static str, Box<dyn StdError + Send + Sync>),
    /// `process.exit(code)` below the entry point; `run()` maps it to the exit code.
    #[error("process.exit({0})")]
    Exit(i32),
}

/// A plain JS error object: `name`, `message`, optional `code` and `cause`.
#[derive(Debug)]
pub struct JsError {
    pub name: String,
    pub message: String,
    pub code: Option<String>,
    pub cause: Option<Box<Error>>,
}

impl fmt::Display for JsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl StdError for JsError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.cause.as_deref().map(|e| e as &(dyn StdError + 'static))
    }
}

/// A Node system error (`err.code`, `err.errno`, `err.syscall`, `err.path`, `err.dest`).
/// `message` is the full Node message, e.g. `EACCES: permission denied, mkdir '/y'`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeError {
    pub code: String,
    pub errno: i32,
    pub syscall: String,
    pub path: Option<String>,
    pub dest: Option<String>,
    pub message: String,
}

impl fmt::Display for NodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl StdError for NodeError {}

impl Error {
    /// `new Error(m)`.
    pub fn msg(m: impl Into<String>) -> Error {
        Error::js("Error", m)
    }

    /// `new <name>(m)`, e.g. `Error::js("TypeError", "x is not a function")`.
    pub fn js(name: &str, m: impl Into<String>) -> Error {
        Error::Js(JsError {
            name: name.to_string(),
            message: m.into(),
            code: None,
            cause: None,
        })
    }

    /// `Object.assign(new <name>(m), { code })`.
    pub fn js_with_code(name: &str, m: impl Into<String>, code: &str) -> Error {
        Error::Js(JsError {
            name: name.to_string(),
            message: m.into(),
            code: Some(code.to_string()),
            cause: None,
        })
    }

    /// `new <name>(m, { cause })`.
    pub fn js_with_cause(name: &str, m: impl Into<String>, cause: Error) -> Error {
        Error::Js(JsError {
            name: name.to_string(),
            message: m.into(),
            code: None,
            cause: Some(Box::new(cause)),
        })
    }

    /// An instance of the TS `Error` subclass `name`, carried as the Rust error `e`.
    pub fn typed<E: StdError + Send + Sync + 'static>(name: &'static str, e: E) -> Error {
        Error::Typed(name, Box::new(e))
    }

    /// `err.name`.
    pub fn name(&self) -> &str {
        match self {
            Error::Js(j) => &j.name,
            Error::Abort(r) => &r.name,
            Error::Node(_) => "Error",
            Error::Typed(name, _) => name,
            Error::Exit(_) => "Error",
        }
    }

    /// `err.message`.
    pub fn message(&self) -> String {
        self.to_string()
    }

    /// `err.code` when it is a string (`"ENOENT"`, `"ERR_INVALID_ARG_TYPE"`, ...).
    pub fn code(&self) -> Option<&str> {
        match self {
            Error::Js(j) => j.code.as_deref(),
            Error::Node(n) => Some(&n.code),
            Error::Abort(_) | Error::Typed(..) | Error::Exit(_) => None,
        }
    }

    /// `err.cause` for errors created with a cause.
    pub fn cause(&self) -> Option<&Error> {
        match self {
            Error::Js(j) => j.cause.as_deref(),
            _ => None,
        }
    }

    /// The code of `Error::Exit`.
    pub fn exit_code(&self) -> Option<i32> {
        match self {
            Error::Exit(c) => Some(*c),
            _ => None,
        }
    }

    /// `err.name === "AbortError"`.
    pub fn is_abort(&self) -> bool {
        self.name() == "AbortError"
    }

    /// `String(err)`: `Error.prototype.toString`, i.e. `"<name>: <message>"`, or just the
    /// non-empty one of the two when the other is empty.
    pub fn to_js_string(&self) -> String {
        let name = self.name();
        let message = self.message();
        if name.is_empty() {
            message
        } else if message.is_empty() {
            name.to_string()
        } else {
            format!("{name}: {message}")
        }
    }

    /// `err instanceof E`: the wrapped Rust error of an [`Error::Typed`] (or the [`JsError`] /
    /// [`NodeError`] payload) if it is an `E`.
    pub fn downcast_ref<E: StdError + 'static>(&self) -> Option<&E> {
        match self {
            Error::Typed(_, e) => (**e).downcast_ref::<E>(),
            Error::Js(j) => (j as &(dyn StdError + 'static)).downcast_ref::<E>(),
            Error::Node(n) => (n as &(dyn StdError + 'static)).downcast_ref::<E>(),
            Error::Abort(_) | Error::Exit(_) => None,
        }
    }

    /// The Node system error for a failed OS call: `CODE: description, syscall 'path' -> 'dest'`
    /// with libuv's `code`, `errno` (negative, platform-specific as in Node) and description.
    pub fn from_io(e: std::io::Error, syscall: &str, path: Option<&str>, dest: Option<&str>) -> Error {
        let (code, errno, desc) = crate::fs::uv_error_info(&e);
        let mut message = format!("{code}: {desc}, {syscall}");
        if let Some(p) = path {
            message.push_str(&format!(" '{p}'"));
        }
        if let Some(d) = dest {
            message.push_str(&format!(" -> '{d}'"));
        }
        Error::Node(NodeError {
            code,
            errno,
            syscall: syscall.to_string(),
            path: path.map(str::to_string),
            dest: dest.map(str::to_string),
            message,
        })
    }
}

impl From<JsError> for Error {
    fn from(e: JsError) -> Error {
        Error::Js(e)
    }
}

impl From<NodeError> for Error {
    fn from(e: NodeError) -> Error {
        Error::Node(e)
    }
}

impl From<AbortReason> for Error {
    fn from(r: AbortReason) -> Error {
        Error::Abort(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abort::AbortReason;

    #[derive(Debug)]
    struct Boom;

    impl fmt::Display for Boom {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("boom-inner")
        }
    }

    impl StdError for Boom {}

    #[test]
    fn display_name_message_and_js_string() {
        let err = Error::msg("boom");
        assert_eq!(err.to_string(), "boom");
        assert_eq!(err.name(), "Error");
        assert_eq!(err.message(), "boom");
        assert_eq!(err.to_js_string(), "Error: boom");
        assert!(!err.is_abort());
        assert_eq!(err.code(), None);

        let err = Error::js("TypeError", "x is not a function");
        assert_eq!(err.to_string(), "x is not a function");
        assert_eq!(err.name(), "TypeError");
        assert_eq!(err.message(), "x is not a function");
        assert_eq!(err.to_js_string(), "TypeError: x is not a function");

        let err = Error::Abort(AbortReason::abort());
        assert_eq!(err.to_string(), "This operation was aborted");
        assert_eq!(err.name(), "AbortError");
        assert_eq!(err.message(), "This operation was aborted");
        assert!(err.is_abort());
        assert_eq!(err.to_js_string(), "AbortError: This operation was aborted");

        let err = Error::Abort(AbortReason::timeout());
        assert_eq!(err.to_string(), "The operation was aborted due to timeout");
        assert_eq!(err.name(), "TimeoutError");
        assert!(!err.is_abort());
        assert_eq!(
            err.to_js_string(),
            "TimeoutError: The operation was aborted due to timeout"
        );

        let err = Error::typed("Boom", Boom);
        assert_eq!(err.to_string(), "boom-inner");
        assert_eq!(err.name(), "Boom");
        assert_eq!(err.message(), "boom-inner");
        assert_eq!(err.to_js_string(), "Boom: boom-inner");
        assert!(err.downcast_ref::<Boom>().is_some());
        assert!(Error::msg("x").downcast_ref::<Boom>().is_none());

        let err = Error::Exit(3);
        assert_eq!(err.to_string(), "process.exit(3)");
        assert_eq!(err.name(), "Error");
        assert_eq!(err.message(), "process.exit(3)");
        assert_eq!(err.to_js_string(), "Error: process.exit(3)");
    }

    #[test]
    fn node_error_text_is_code_description_syscall_and_path() {
        let err = Error::from_io(
            std::io::Error::new(std::io::ErrorKind::NotFound, "missing"),
            "open",
            Some("/x"),
            None,
        );
        assert_eq!(err.to_string(), "ENOENT: no such file or directory, open '/x'");
        assert_eq!(err.message(), "ENOENT: no such file or directory, open '/x'");
        assert_eq!(err.name(), "Error");
        assert_eq!(err.code(), Some("ENOENT"));
        assert_eq!(
            err.to_js_string(),
            "Error: ENOENT: no such file or directory, open '/x'"
        );
        let Error::Node(node) = &err else {
            panic!("expected Node, got {err:?}");
        };
        assert_eq!(node.to_string(), "ENOENT: no such file or directory, open '/x'");
        assert_eq!(node.code, "ENOENT");
        assert_eq!(node.syscall, "open");
        assert_eq!(node.path.as_deref(), Some("/x"));
        assert_eq!(node.dest, None);
        assert!(node.errno < 0);
        assert!(err.downcast_ref::<NodeError>().is_some());

        let renamed = Error::from_io(
            std::io::Error::new(std::io::ErrorKind::NotFound, "missing"),
            "rename",
            Some("/a"),
            Some("/b"),
        );
        assert_eq!(
            renamed.to_string(),
            "ENOENT: no such file or directory, rename '/a' -> '/b'"
        );

        let denied = Error::from_io(
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "nope"),
            "mkdir",
            Some("/y"),
            None,
        );
        assert_eq!(denied.to_string(), "EACCES: permission denied, mkdir '/y'");
        assert_eq!(denied.code(), Some("EACCES"));
        assert_eq!(denied.message(), denied.to_string());
    }
}
