//! pi_js::fs (Rust-only; API contract: PORTING.md Appendix A).
//!
//! `node:fs` sync API with Node's error objects (`NodeError`, exact messages such as
//! `ENOENT: no such file or directory, open '/x'`), plus [`promises`], the async mirror
//! (`node:fs/promises`), which runs the same operations on tokio's blocking pool.
//!
//! Paths are `&str` exactly as in TS. Relative paths resolve against `process.cwd()`;
//! when a `pi_js::env::testing::CwdGuard` is active, against the guarded cwd. Error
//! messages always show the path as passed in.
//!
//! Behaviour notes (Node 24):
//! - `readdir` returns names sorted by byte order on unix (libuv `scandir` sorts with
//!   `strcmp`), OS order on Windows; non-UTF-8 names are decoded lossily.
//! - `rm` follows `fs.rmSync` (JS `lstat` pre-check, then the C++ `std::filesystem`
//!   removal with its `ERRNO, Message: path 'path'` errors); `promises::rm` follows
//!   `fsp.rm` (Node's bundled rimraf, errors from the failing child operation).
//! - `realpath` is the JS walk of `fs.realpathSync` (errors come from `lstat`/`stat` on the
//!   partial path); `promises::realpath` and [`realpath_native`] use realpath(3).
//! - `mkdtemp` errors show `<prefix>XXXXXX` (sync) or the attempted name (promises).

use std::ffi::OsString;
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::error::{Error, JsError, NodeError, Result};

/// `fs.constants` (the subset with platform-independent values used by pi).
pub mod constants {
    pub const F_OK: u32 = 0;
    pub const R_OK: u32 = 4;
    pub const W_OK: u32 = 2;
    pub const X_OK: u32 = 1;
    pub const COPYFILE_EXCL: u32 = 1;
    pub const COPYFILE_FICLONE: u32 = 2;
    pub const COPYFILE_FICLONE_FORCE: u32 = 4;
    pub const S_IFMT: u32 = 0o170000;
    pub const S_IFREG: u32 = 0o100000;
    pub const S_IFDIR: u32 = 0o040000;
    pub const S_IFCHR: u32 = 0o020000;
    pub const S_IFBLK: u32 = 0o060000;
    pub const S_IFIFO: u32 = 0o010000;
    pub const S_IFLNK: u32 = 0o120000;
    pub const S_IFSOCK: u32 = 0o140000;
}

use constants::*;

// ---------------------------------------------------------------------------------------
// libuv error names, numbers and messages

/// libuv error descriptions (`uv_strerror`), keyed by `uv_err_name`.
fn uv_desc(name: &str) -> &'static str {
    match name {
        "E2BIG" => "argument list too long",
        "EACCES" => "permission denied",
        "EADDRINUSE" => "address already in use",
        "EADDRNOTAVAIL" => "address not available",
        "EAFNOSUPPORT" => "address family not supported",
        "EAGAIN" => "resource temporarily unavailable",
        "EALREADY" => "connection already in progress",
        "EBADF" => "bad file descriptor",
        "EBUSY" => "resource busy or locked",
        "ECANCELED" => "operation canceled",
        "ECHARSET" => "invalid Unicode character",
        "ECONNABORTED" => "software caused connection abort",
        "ECONNREFUSED" => "connection refused",
        "ECONNRESET" => "connection reset by peer",
        "EDESTADDRREQ" => "destination address required",
        "EEXIST" => "file already exists",
        "EFAULT" => "bad address in system call argument",
        "EFBIG" => "file too large",
        "EHOSTUNREACH" => "host is unreachable",
        "EINTR" => "interrupted system call",
        "EINVAL" => "invalid argument",
        "EIO" => "i/o error",
        "EISCONN" => "socket is already connected",
        "EISDIR" => "illegal operation on a directory",
        "ELOOP" => "too many symbolic links encountered",
        "EMFILE" => "too many open files",
        "EMSGSIZE" => "message too long",
        "ENAMETOOLONG" => "name too long",
        "ENETDOWN" => "network is down",
        "ENETUNREACH" => "network is unreachable",
        "ENFILE" => "file table overflow",
        "ENOBUFS" => "no buffer space available",
        "ENODEV" => "no such device",
        "ENOENT" => "no such file or directory",
        "ENOMEM" => "not enough memory",
        "ENONET" => "machine is not on the network",
        "ENOPROTOOPT" => "protocol not available",
        "ENOSPC" => "no space left on device",
        "ENOSYS" => "function not implemented",
        "ENOTCONN" => "socket is not connected",
        "ENOTDIR" => "not a directory",
        "ENOTEMPTY" => "directory not empty",
        "ENOTSOCK" => "socket operation on non-socket",
        "ENOTSUP" => "operation not supported on socket",
        "EOVERFLOW" => "value too large for defined data type",
        "EPERM" => "operation not permitted",
        "EPIPE" => "broken pipe",
        "EPROTO" => "protocol error",
        "EPROTONOSUPPORT" => "protocol not supported",
        "EPROTOTYPE" => "protocol wrong type for socket",
        "ERANGE" => "result too large",
        "EROFS" => "read-only file system",
        "ESHUTDOWN" => "cannot send after transport endpoint shutdown",
        "ESPIPE" => "invalid seek",
        "ESRCH" => "no such process",
        "ETIMEDOUT" => "connection timed out",
        "ETXTBSY" => "text file is busy",
        "EXDEV" => "cross-device link not permitted",
        "EOF" => "end of file",
        "ENXIO" => "no such device or address",
        "EMLINK" => "too many links",
        "EHOSTDOWN" => "host is down",
        "EREMOTEIO" => "remote I/O error",
        "ENOTTY" => "inappropriate ioctl for device",
        "EFTYPE" => "inappropriate file type or format",
        "EILSEQ" => "illegal byte sequence",
        "ESOCKTNOSUPPORT" => "socket type not supported",
        "ENODATA" => "no data available",
        "EUNATCH" => "protocol driver not attached",
        "ENOEXEC" => "exec format error",
        _ => "unknown error",
    }
}

#[cfg(unix)]
fn unix_errno_name(errno: i32) -> Option<&'static str> {
    use libc::*;
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    ))]
    if errno == EFTYPE {
        return Some("EFTYPE");
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        if errno == EREMOTEIO {
            return Some("EREMOTEIO");
        }
        if errno == ENONET {
            return Some("ENONET");
        }
        if errno == EUNATCH {
            return Some("EUNATCH");
        }
    }
    Some(match errno {
        E2BIG => "E2BIG",
        EACCES => "EACCES",
        EADDRINUSE => "EADDRINUSE",
        EADDRNOTAVAIL => "EADDRNOTAVAIL",
        EAFNOSUPPORT => "EAFNOSUPPORT",
        EAGAIN => "EAGAIN",
        EALREADY => "EALREADY",
        EBADF => "EBADF",
        EBUSY => "EBUSY",
        ECANCELED => "ECANCELED",
        ECONNABORTED => "ECONNABORTED",
        ECONNREFUSED => "ECONNREFUSED",
        ECONNRESET => "ECONNRESET",
        EDESTADDRREQ => "EDESTADDRREQ",
        EEXIST => "EEXIST",
        EFAULT => "EFAULT",
        EFBIG => "EFBIG",
        EHOSTUNREACH => "EHOSTUNREACH",
        EINTR => "EINTR",
        EINVAL => "EINVAL",
        EIO => "EIO",
        EISCONN => "EISCONN",
        EISDIR => "EISDIR",
        ELOOP => "ELOOP",
        EMFILE => "EMFILE",
        EMSGSIZE => "EMSGSIZE",
        ENAMETOOLONG => "ENAMETOOLONG",
        ENETDOWN => "ENETDOWN",
        ENETUNREACH => "ENETUNREACH",
        ENFILE => "ENFILE",
        ENOBUFS => "ENOBUFS",
        ENODEV => "ENODEV",
        ENOENT => "ENOENT",
        ENOMEM => "ENOMEM",
        ENOPROTOOPT => "ENOPROTOOPT",
        ENOSPC => "ENOSPC",
        ENOSYS => "ENOSYS",
        ENOTCONN => "ENOTCONN",
        ENOTDIR => "ENOTDIR",
        ENOTEMPTY => "ENOTEMPTY",
        ENOTSOCK => "ENOTSOCK",
        ENOTSUP => "ENOTSUP",
        EOVERFLOW => "EOVERFLOW",
        EPERM => "EPERM",
        EPIPE => "EPIPE",
        EPROTO => "EPROTO",
        EPROTONOSUPPORT => "EPROTONOSUPPORT",
        EPROTOTYPE => "EPROTOTYPE",
        ERANGE => "ERANGE",
        EROFS => "EROFS",
        ESHUTDOWN => "ESHUTDOWN",
        ESPIPE => "ESPIPE",
        ESRCH => "ESRCH",
        ETIMEDOUT => "ETIMEDOUT",
        ETXTBSY => "ETXTBSY",
        EXDEV => "EXDEV",
        ENXIO => "ENXIO",
        EMLINK => "EMLINK",
        EHOSTDOWN => "EHOSTDOWN",
        ENOTTY => "ENOTTY",
        EILSEQ => "EILSEQ",
        ESOCKTNOSUPPORT => "ESOCKTNOSUPPORT",
        ENODATA => "ENODATA",
        ENOEXEC => "ENOEXEC",
        _ => return None,
    })
}

/// libuv's Windows error numbers (`UV__E*` in uv/errno.h).
#[cfg_attr(unix, allow(dead_code))]
fn win_uv_errno(name: &str) -> i32 {
    match name {
        "E2BIG" => -4093,
        "EACCES" => -4092,
        "EADDRINUSE" => -4091,
        "EADDRNOTAVAIL" => -4090,
        "EAFNOSUPPORT" => -4089,
        "EAGAIN" => -4088,
        "EALREADY" => -4084,
        "EBADF" => -4083,
        "EBUSY" => -4082,
        "ECANCELED" => -4081,
        "ECHARSET" => -4080,
        "ECONNABORTED" => -4079,
        "ECONNREFUSED" => -4078,
        "ECONNRESET" => -4077,
        "EDESTADDRREQ" => -4076,
        "EEXIST" => -4075,
        "EFAULT" => -4074,
        "EHOSTUNREACH" => -4073,
        "EINTR" => -4072,
        "EINVAL" => -4071,
        "EIO" => -4070,
        "EISCONN" => -4069,
        "EISDIR" => -4068,
        "ELOOP" => -4067,
        "EMFILE" => -4066,
        "EMSGSIZE" => -4065,
        "ENAMETOOLONG" => -4064,
        "ENETDOWN" => -4063,
        "ENETUNREACH" => -4062,
        "ENFILE" => -4061,
        "ENOBUFS" => -4060,
        "ENODEV" => -4059,
        "ENOENT" => -4058,
        "ENOMEM" => -4057,
        "ENONET" => -4056,
        "ENOSPC" => -4055,
        "ENOSYS" => -4054,
        "ENOTCONN" => -4053,
        "ENOTDIR" => -4052,
        "ENOTEMPTY" => -4051,
        "ENOTSOCK" => -4050,
        "ENOTSUP" => -4049,
        "EPERM" => -4048,
        "EPIPE" => -4047,
        "EPROTO" => -4046,
        "EPROTONOSUPPORT" => -4045,
        "EPROTOTYPE" => -4044,
        "EROFS" => -4043,
        "ESHUTDOWN" => -4042,
        "ESPIPE" => -4041,
        "ESRCH" => -4040,
        "ETIMEDOUT" => -4039,
        "ETXTBSY" => -4038,
        "EXDEV" => -4037,
        "EFBIG" => -4036,
        "ENOPROTOOPT" => -4035,
        "ERANGE" => -4034,
        "ENXIO" => -4033,
        "EMLINK" => -4032,
        "EHOSTDOWN" => -4031,
        "EREMOTEIO" => -4030,
        "ENOTTY" => -4029,
        "EFTYPE" => -4028,
        "EILSEQ" => -4027,
        "EOVERFLOW" => -4026,
        "ESOCKTNOSUPPORT" => -4025,
        "ENODATA" => -4024,
        "EUNATCH" => -4023,
        "ENOEXEC" => -4022,
        "EOF" => -4095,
        _ => -4094,
    }
}

/// `uv_translate_sys_error` for the Win32 error codes Rust surfaces.
#[cfg_attr(unix, allow(dead_code))]
fn win32_error_name(code: i32) -> &'static str {
    match code {
        998 | 10013 | 740 | 1920 => "EACCES",
        1227 | 10048 => "EADDRINUSE",
        10049 => "EADDRNOTAVAIL",
        10047 => "EAFNOSUPPORT",
        10035 => "EAGAIN",
        10037 => "EALREADY",
        1004 | 6 => "EBADF",
        33 | 231 | 32 => "EBUSY",
        995 | 10004 => "ECANCELED",
        1113 => "ECHARSET",
        1236 | 10053 => "ECONNABORTED",
        1225 | 10061 => "ECONNREFUSED",
        64 | 10054 => "ECONNRESET",
        183 | 80 => "EEXIST",
        111 | 10014 => "EFAULT",
        1232 | 10065 => "EHOSTUNREACH",
        122 | 13 | 87 | 1464 | 10022 | 10046 => "EINVAL",
        1102 | 1111 | 23 | 1166 | 1165 | 1393 | 1129 | 1101 | 31 | 1106 | 1117 | 1104 | 205 | 110 | 1103 | 156 => "EIO",
        10056 => "EISCONN",
        1921 => "ELOOP",
        4 | 10024 => "EMFILE",
        10040 => "EMSGSIZE",
        206 => "ENAMETOOLONG",
        1231 | 10051 => "ENETUNREACH",
        10055 => "ENOBUFS",
        161 | 267 | 203 | 2 | 15 | 4392 | 126 | 3 | 123 | 11001 | 11004 => "ENOENT",
        8 | 14 => "ENOMEM",
        82 | 112 | 277 | 1100 | 39 => "ENOSPC",
        2250 | 10057 => "ENOTCONN",
        145 => "ENOTEMPTY",
        10038 => "ENOTSOCK",
        50 => "ENOTSUP",
        109 => "EOF",
        5 | 1314 => "EPERM",
        230 | 232 | 233 | 10058 => "EPIPE",
        10043 => "EPROTONOSUPPORT",
        19 => "EROFS",
        121 | 10060 => "ETIMEDOUT",
        17 => "EXDEV",
        1 => "EISDIR",
        208 => "E2BIG",
        10044 => "ESOCKTNOSUPPORT",
        193 => "EFTYPE",
        _ => "UNKNOWN",
    }
}

/// Platform errno for a uv error name (negative, as Node reports `err.errno`).
fn uv_errno_for_name(name: &str) -> i32 {
    #[cfg(unix)]
    {
        use libc::*;
        let raw = match name {
            "EPERM" => EPERM,
            "ENOENT" => ENOENT,
            "EACCES" => EACCES,
            "EEXIST" => EEXIST,
            "ENOTDIR" => ENOTDIR,
            "EISDIR" => EISDIR,
            "EINVAL" => EINVAL,
            "ENOTEMPTY" => ENOTEMPTY,
            "EBUSY" => EBUSY,
            "EXDEV" => EXDEV,
            "EROFS" => EROFS,
            "ENOSPC" => ENOSPC,
            "EMLINK" => EMLINK,
            "EFBIG" => EFBIG,
            "ETXTBSY" => ETXTBSY,
            "ESPIPE" => ESPIPE,
            "EINTR" => EINTR,
            "EAGAIN" => EAGAIN,
            "EPIPE" => EPIPE,
            "ENOMEM" => ENOMEM,
            "ENOTSUP" => ENOTSUP,
            "ELOOP" => ELOOP,
            "ENAMETOOLONG" => ENAMETOOLONG,
            "EIO" => EIO,
            "EBADF" => EBADF,
            "EOF" => return -4095,
            _ => return -4094,
        };
        -raw
    }
    #[cfg(not(unix))]
    {
        win_uv_errno(name)
    }
}

/// `(code, errno, description)` exactly as Node reports a failed libuv call.
pub(crate) fn uv_error_info(e: &io::Error) -> (String, i32, String) {
    if let Some(raw) = e.raw_os_error() {
        #[cfg(unix)]
        {
            return match unix_errno_name(raw) {
                Some(name) => (name.to_string(), -raw, uv_desc(name).to_string()),
                None => {
                    let s = format!("Unknown system error {}", -raw);
                    (s.clone(), -raw, s)
                }
            };
        }
        #[cfg(not(unix))]
        {
            let name = win32_error_name(raw);
            return (name.to_string(), win_uv_errno(name), uv_desc(name).to_string());
        }
    }
    let name = io_kind_name(e.kind());
    (name.to_string(), uv_errno_for_name(name), uv_desc(name).to_string())
}

fn io_kind_name(kind: io::ErrorKind) -> &'static str {
    use io::ErrorKind::*;
    match kind {
        NotFound => "ENOENT",
        PermissionDenied => "EACCES",
        AlreadyExists => "EEXIST",
        InvalidInput | InvalidData => "EINVAL",
        Interrupted => "EINTR",
        WouldBlock => "EAGAIN",
        BrokenPipe => "EPIPE",
        UnexpectedEof => "EOF",
        OutOfMemory => "ENOMEM",
        Unsupported => "ENOTSUP",
        IsADirectory => "EISDIR",
        NotADirectory => "ENOTDIR",
        DirectoryNotEmpty => "ENOTEMPTY",
        ReadOnlyFilesystem => "EROFS",
        StorageFull => "ENOSPC",
        CrossesDevices => "EXDEV",
        TooManyLinks => "EMLINK",
        FileTooLarge => "EFBIG",
        ResourceBusy => "EBUSY",
        ExecutableFileBusy => "ETXTBSY",
        NotSeekable => "ESPIPE",
        _ => "UNKNOWN",
    }
}

fn uv_code(e: &io::Error) -> String {
    uv_error_info(e).0
}

/// Builds a Node uv exception: `CODE: desc, syscall 'path' -> 'dest'`.
fn uv_exception(code: &str, errno: i32, desc: &str, syscall: &str, path: Option<&str>, dest: Option<&str>) -> Error {
    let mut message = format!("{code}: {desc}, {syscall}");
    if let Some(p) = path {
        message.push_str(&format!(" '{p}'"));
    }
    if let Some(d) = dest {
        message.push_str(&format!(" -> '{d}'"));
    }
    Error::Node(NodeError {
        code: code.to_string(),
        errno,
        syscall: syscall.to_string(),
        path: path.map(str::to_string),
        dest: dest.map(str::to_string),
        message,
    })
}

fn io_error(e: io::Error, syscall: &str, path: Option<&str>, dest: Option<&str>) -> Error {
    let (code, errno, desc) = uv_error_info(&e);
    uv_exception(&code, errno, &desc, syscall, path, dest)
}

/// A synthesized uv error (`name` is a uv code such as `"EISDIR"`).
fn uv_error(name: &str, syscall: &str, path: Option<&str>, dest: Option<&str>) -> Error {
    uv_exception(name, uv_errno_for_name(name), uv_desc(name), syscall, path, dest)
}

fn error_code(e: &Error) -> Option<&str> {
    match e {
        Error::Node(n) => Some(n.code.as_str()),
        Error::Js(j) => j.code.as_deref(),
        _ => None,
    }
}

fn type_error(code: &str, message: String) -> Error {
    Error::Js(JsError {
        name: "TypeError".to_string(),
        message,
        code: Some(code.to_string()),
        cause: None,
    })
}

/// `util.inspect(string)` with default options (quote selection, escapes, line splitting).
fn inspect_string(s: &str) -> String {
    let quote = if !s.contains('\'') {
        '\''
    } else if !s.contains('"') {
        '"'
    } else if !s.contains('`') && !s.contains("${") {
        '`'
    } else {
        '\''
    };
    let escape = |line: &str| -> String {
        let mut out = String::new();
        out.push(quote);
        let units: Vec<u16> = line.encode_utf16().collect();
        let mut i = 0;
        while i < units.len() {
            let u = units[i];
            let c = u32::from(u);
            if (0xD800..0xDC00).contains(&u) && i + 1 < units.len() && (0xDC00..0xE000).contains(&units[i + 1]) {
                out.push_str(&String::from_utf16_lossy(&units[i..i + 2]));
                i += 2;
                continue;
            }
            if (0xD800..0xE000).contains(&u) {
                out.push_str(&format!("\\u{c:x}"));
            } else if c == u32::from(quote) && quote == '\'' {
                out.push_str("\\'");
            } else if c == u32::from('\\') {
                out.push_str("\\\\");
            } else if c < 0x20 || (0x7F..=0x9F).contains(&c) {
                match c {
                    0x08 => out.push_str("\\b"),
                    0x09 => out.push_str("\\t"),
                    0x0A => out.push_str("\\n"),
                    0x0C => out.push_str("\\f"),
                    0x0D => out.push_str("\\r"),
                    _ => out.push_str(&format!("\\x{c:02X}")),
                }
            } else {
                out.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
            }
            i += 1;
        }
        out.push(quote);
        out
    };
    let len16 = s.encode_utf16().count();
    // `value.length > kMinLineWidth (16) && value.length > breakLength (128) - 4`
    if len16 > 128 - 4 && s.contains('\n') {
        let mut lines: Vec<&str> = Vec::new();
        let mut start = 0;
        for (i, ch) in s.char_indices() {
            if ch == '\n' {
                lines.push(&s[start..=i]);
                start = i + 1;
            }
        }
        if start < s.len() {
            lines.push(&s[start..]);
        }
        return lines.iter().map(|l| escape(l)).collect::<Vec<_>>().join(" +\n  ");
    }
    escape(s)
}

/// `ERR_INVALID_ARG_VALUE` (`TypeError`).
fn invalid_arg_value(name: &str, value: &str, reason: &str) -> Error {
    let mut inspected = inspect_string(value);
    let units: Vec<u16> = inspected.encode_utf16().collect();
    if units.len() > 128 {
        inspected = format!("{}...", String::from_utf16_lossy(&units[..128]));
    }
    let kind = if name.contains('.') { "property" } else { "argument" };
    type_error(
        "ERR_INVALID_ARG_VALUE",
        format!("The {kind} '{name}' {reason}. Received {inspected}"),
    )
}

/// `getValidatedPath(p, name)`: rejects strings with NUL bytes.
fn validate_path(p: &str, name: &str) -> Result<()> {
    if p.contains('\0') {
        return Err(invalid_arg_value(
            name,
            p,
            "must be a string, Uint8Array, or URL without null bytes",
        ));
    }
    Ok(())
}

/// The OS path for a JS path string (relative paths honour `CwdGuard`).
fn os_path(p: &str) -> PathBuf {
    if !p.is_empty() && crate::env::testing::cwd_overridden() && !crate::path::is_absolute(p) {
        Path::new(&crate::env::cwd()).join(p)
    } else {
        PathBuf::from(p)
    }
}

fn strip_verbatim(p: String) -> String {
    if let Some(rest) = p.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{rest}")
    } else if let Some(rest) = p.strip_prefix("\\\\?\\") {
        rest.to_string()
    } else {
        p
    }
}

fn lossy(s: OsString) -> String {
    match s.into_string() {
        Ok(s) => s,
        Err(s) => s.to_string_lossy().into_owned(),
    }
}

// ---------------------------------------------------------------------------------------
// Stats and Dirent

/// `fs.Stats`. Times are `*Ms` floats like Node (`sec * 1e3 + nsec / 1e6`) plus the exact
/// nanosecond values of `{ bigint: true }` stats. `Stats::default()` is the all-zero value
/// `fs.watchFile` reports for a missing file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub dev: u64,
    pub mode: u32,
    pub nlink: u64,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u64,
    pub blksize: u64,
    pub ino: u64,
    pub size: u64,
    pub blocks: u64,
    pub atime_ms: f64,
    pub mtime_ms: f64,
    pub ctime_ms: f64,
    pub birthtime_ms: f64,
    pub atime_ns: i128,
    pub mtime_ns: i128,
    pub ctime_ns: i128,
    pub birthtime_ns: i128,
}

fn ms_from_timespec(sec: i64, nsec: i64) -> f64 {
    sec as f64 * 1000.0 + nsec as f64 / 1_000_000.0
}

fn ns_from_timespec(sec: i64, nsec: i64) -> i128 {
    i128::from(sec) * 1_000_000_000 + i128::from(nsec)
}

fn system_time_parts(t: io::Result<std::time::SystemTime>) -> (i64, i64) {
    match t {
        Ok(t) => match t.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => (d.as_secs() as i64, i64::from(d.subsec_nanos())),
            Err(e) => {
                let d = e.duration();
                let (s, n) = (d.as_secs() as i64, i64::from(d.subsec_nanos()));
                if n == 0 { (-s, 0) } else { (-s - 1, 1_000_000_000 - n) }
            }
        },
        Err(_) => (0, 0),
    }
}

impl Stats {
    fn from_metadata(m: &std::fs::Metadata) -> Stats {
        let (bs, bn) = system_time_parts(m.created());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Stats {
                dev: m.dev(),
                mode: m.mode(),
                nlink: m.nlink(),
                uid: m.uid(),
                gid: m.gid(),
                rdev: m.rdev(),
                blksize: m.blksize(),
                ino: m.ino(),
                size: m.size(),
                blocks: m.blocks(),
                atime_ms: ms_from_timespec(m.atime(), m.atime_nsec()),
                mtime_ms: ms_from_timespec(m.mtime(), m.mtime_nsec()),
                ctime_ms: ms_from_timespec(m.ctime(), m.ctime_nsec()),
                birthtime_ms: ms_from_timespec(bs, bn),
                atime_ns: ns_from_timespec(m.atime(), m.atime_nsec()),
                mtime_ns: ns_from_timespec(m.mtime(), m.mtime_nsec()),
                ctime_ns: ns_from_timespec(m.ctime(), m.ctime_nsec()),
                birthtime_ns: ns_from_timespec(bs, bn),
            }
        }
        #[cfg(not(unix))]
        {
            // libuv on Windows: rw (or r when read-only) for everyone, x for directories.
            let ft = m.file_type();
            let mut mode = if m.permissions().readonly() { 0o444 } else { 0o666 };
            if ft.is_symlink() {
                mode |= S_IFLNK;
            } else if ft.is_dir() {
                mode |= S_IFDIR | 0o111;
            } else {
                mode |= S_IFREG;
            }
            let (as_, an) = system_time_parts(m.accessed());
            let (ms, mn) = system_time_parts(m.modified());
            Stats {
                dev: 0,
                mode,
                nlink: 1,
                uid: 0,
                gid: 0,
                rdev: 0,
                blksize: 4096,
                ino: 0,
                size: m.len(),
                blocks: m.len().div_ceil(512),
                atime_ms: ms_from_timespec(as_, an),
                mtime_ms: ms_from_timespec(ms, mn),
                ctime_ms: ms_from_timespec(ms, mn),
                birthtime_ms: ms_from_timespec(bs, bn),
                atime_ns: ns_from_timespec(as_, an),
                mtime_ns: ns_from_timespec(ms, mn),
                ctime_ns: ns_from_timespec(ms, mn),
                birthtime_ns: ns_from_timespec(bs, bn),
            }
        }
    }

    fn kind(&self) -> u32 {
        self.mode & S_IFMT
    }
    pub fn is_file(&self) -> bool {
        self.kind() == S_IFREG
    }
    pub fn is_directory(&self) -> bool {
        self.kind() == S_IFDIR
    }
    pub fn is_symbolic_link(&self) -> bool {
        self.kind() == S_IFLNK
    }
    pub fn is_socket(&self) -> bool {
        self.kind() == S_IFSOCK
    }
    pub fn is_fifo(&self) -> bool {
        self.kind() == S_IFIFO
    }
    pub fn is_block_device(&self) -> bool {
        self.kind() == S_IFBLK
    }
    pub fn is_character_device(&self) -> bool {
        self.kind() == S_IFCHR
    }
    /// `stats.atime.getTime()` (`Math.round(atimeMs)`).
    pub fn atime(&self) -> i64 {
        (self.atime_ms + 0.5).floor() as i64
    }
    /// `stats.mtime.getTime()`.
    pub fn mtime(&self) -> i64 {
        (self.mtime_ms + 0.5).floor() as i64
    }
    /// `stats.ctime.getTime()`.
    pub fn ctime(&self) -> i64 {
        (self.ctime_ms + 0.5).floor() as i64
    }
    /// `stats.birthtime.getTime()`.
    pub fn birthtime(&self) -> i64 {
        (self.birthtime_ms + 0.5).floor() as i64
    }
}

/// `fs.Dirent` (from `readdir(p, { withFileTypes: true })`). `file_type` holds the
/// `S_IF*` bits (0 when unknown).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dirent {
    pub name: String,
    pub parent_path: String,
    pub file_type: u32,
}

impl Dirent {
    pub fn is_file(&self) -> bool {
        self.file_type == S_IFREG
    }
    pub fn is_directory(&self) -> bool {
        self.file_type == S_IFDIR
    }
    pub fn is_symbolic_link(&self) -> bool {
        self.file_type == S_IFLNK
    }
    pub fn is_socket(&self) -> bool {
        self.file_type == S_IFSOCK
    }
    pub fn is_fifo(&self) -> bool {
        self.file_type == S_IFIFO
    }
    pub fn is_block_device(&self) -> bool {
        self.file_type == S_IFBLK
    }
    pub fn is_character_device(&self) -> bool {
        self.file_type == S_IFCHR
    }
}

fn file_type_bits(ft: &std::fs::FileType) -> u32 {
    if ft.is_symlink() {
        return S_IFLNK;
    }
    if ft.is_dir() {
        return S_IFDIR;
    }
    if ft.is_file() {
        return S_IFREG;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if ft.is_socket() {
            return S_IFSOCK;
        }
        if ft.is_fifo() {
            return S_IFIFO;
        }
        if ft.is_block_device() {
            return S_IFBLK;
        }
        if ft.is_char_device() {
            return S_IFCHR;
        }
    }
    0
}

// ---------------------------------------------------------------------------------------
// open flags

#[derive(Clone, Copy, Debug, Default)]
struct OpenFlags {
    read: bool,
    write: bool,
    append: bool,
    create: bool,
    truncate: bool,
    excl: bool,
    sync: bool,
}

/// `stringToFlags`.
fn parse_flags(flags: &str) -> Result<OpenFlags> {
    let f = |read, write, append, create, truncate, excl, sync| OpenFlags {
        read,
        write,
        append,
        create,
        truncate,
        excl,
        sync,
    };
    Ok(match flags {
        "r" => f(true, false, false, false, false, false, false),
        "rs" | "sr" => f(true, false, false, false, false, false, true),
        "r+" => f(true, true, false, false, false, false, false),
        "rs+" | "sr+" => f(true, true, false, false, false, false, true),
        "w" => f(false, true, false, true, true, false, false),
        "wx" | "xw" => f(false, true, false, true, true, true, false),
        "w+" => f(true, true, false, true, true, false, false),
        "wx+" | "xw+" => f(true, true, false, true, true, true, false),
        "a" => f(false, true, true, true, false, false, false),
        "ax" | "xa" => f(false, true, true, true, false, true, false),
        "as" | "sa" => f(false, true, true, true, false, false, true),
        "a+" => f(true, true, true, true, false, false, false),
        "ax+" | "xa+" => f(true, true, true, true, false, true, false),
        "as+" | "sa+" => f(true, true, true, true, false, false, true),
        _ => return Err(invalid_arg_value("flags", flags, "is invalid")),
    })
}

fn open_std(p: &str, flags: &str, mode: Option<u32>) -> Result<std::fs::File> {
    let fl = parse_flags(flags)?;
    let mut o = std::fs::OpenOptions::new();
    o.read(fl.read)
        .write(fl.write && !fl.append)
        .append(fl.append)
        .truncate(fl.truncate);
    if fl.excl {
        o.create_new(true);
    } else if fl.create {
        o.create(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(mode.unwrap_or(0o666));
        if fl.sync {
            o.custom_flags(libc::O_SYNC);
        }
    }
    #[cfg(not(unix))]
    let _ = mode;
    let os = os_path(p);
    o.open(&os).map_err(|e| {
        #[cfg(windows)]
        {
            // libuv reports EISDIR when Windows refuses to open a directory as a file.
            if matches!(e.raw_os_error(), Some(5) | Some(80)) && std::fs::metadata(&os).is_ok_and(|m| m.is_dir()) {
                if !fl.write && !fl.append {
                    return uv_error("EISDIR", "read", None, None);
                }
                return uv_error("EISDIR", "open", Some(p), None);
            }
        }
        io_error(e, "open", Some(p), None)
    })
}

// ---------------------------------------------------------------------------------------
// Sync API

/// `fs.readFileSync(p)`.
pub fn read_file(p: &str) -> Result<Vec<u8>> {
    validate_path(p, "path")?;
    let mut f = open_std(p, "r", None)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(|e| io_error(e, "read", None, None))?;
    Ok(buf)
}

/// `fs.readFileSync(p, "utf8")` (invalid UTF-8 becomes U+FFFD; a BOM is kept).
pub fn read_to_string(p: &str) -> Result<String> {
    let bytes = read_file(p)?;
    Ok(match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    })
}

/// `fs.writeFileSync(p, data, { mode })`.
pub fn write_file(p: &str, data: impl AsRef<[u8]>, mode: Option<u32>) -> Result<()> {
    write_file_with_flag(p, data, "w", mode)
}

/// `fs.writeFileSync(p, data, { flag, mode })` (e.g. `flag: "wx"`).
pub fn write_file_with_flag(p: &str, data: impl AsRef<[u8]>, flag: &str, mode: Option<u32>) -> Result<()> {
    validate_path(p, "path")?;
    let mut f = open_std(p, flag, mode)?;
    f.write_all(data.as_ref()).map_err(|e| io_error(e, "write", None, None))
}

/// `fs.appendFileSync(p, data)`.
pub fn append_file(p: &str, data: impl AsRef<[u8]>) -> Result<()> {
    write_file_with_flag(p, data, "a", None)
}

fn mkdir_raw(p: &Path, mode: u32) -> io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    b.create(p)
}

fn io_from_name(name: &str) -> io::Error {
    #[cfg(unix)]
    {
        io::Error::from_raw_os_error(-uv_errno_for_name(name))
    }
    #[cfg(not(unix))]
    {
        let code = match name {
            "EEXIST" => 183,
            "ENOTDIR" => 267,
            "ENOENT" => 2,
            _ => 31,
        };
        io::Error::from_raw_os_error(code)
    }
}

/// Node's `MKDirpSync` (src/node_file.cc).
fn mkdirp(target: &Path, mode: u32) -> io::Result<()> {
    let mut stack: Vec<PathBuf> = vec![target.to_path_buf()];
    while let Some(next) = stack.pop() {
        let mut err = mkdir_raw(&next, mode).err();
        while let Some(e) = err.take() {
            let code = uv_code(&e);
            match code.as_str() {
                "EACCES" | "ENOSPC" | "ENOTDIR" | "EPERM" => return Err(e),
                "ENOENT" => {
                    match next.parent().filter(|p| !p.as_os_str().is_empty()) {
                        Some(parent) if parent != next.as_path() => {
                            let parent = parent.to_path_buf();
                            stack.push(next.clone());
                            stack.push(parent);
                        }
                        _ => {
                            if stack.is_empty() {
                                err = Some(io_from_name("EEXIST"));
                                continue;
                            }
                        }
                    }
                    break;
                }
                _ => {
                    match std::fs::metadata(&next) {
                        Ok(m) if !m.is_dir() => {
                            if code == "EEXIST" && !stack.is_empty() {
                                return Err(io_from_name("ENOTDIR"));
                            }
                            return Err(io_from_name("EEXIST"));
                        }
                        Ok(_) => {}
                        Err(stat_err) => return Err(stat_err),
                    }
                    break;
                }
            }
        }
    }
    Ok(())
}

/// `fs.mkdirSync(p, { recursive, mode })` (default mode `0o777`).
pub fn mkdir(p: &str, recursive: bool, mode: Option<u32>) -> Result<()> {
    validate_path(p, "path")?;
    let os = os_path(p);
    let mode = mode.unwrap_or(0o777);
    let res = if recursive {
        mkdirp(&os, mode)
    } else {
        mkdir_raw(&os, mode)
    };
    res.map_err(|e| io_error(e, "mkdir", Some(p), None))
}

fn read_dir_raw(p: &str, with_types: bool) -> Result<Vec<(OsString, u32)>> {
    let os = os_path(p);
    let rd = std::fs::read_dir(&os).map_err(|e| io_error(e, "scandir", Some(p), None))?;
    let mut out = Vec::new();
    for entry in rd {
        let entry = entry.map_err(|e| io_error(e, "scandir", Some(p), None))?;
        let kind = if with_types {
            match entry.file_type() {
                Ok(ft) => file_type_bits(&ft),
                Err(_) => std::fs::symlink_metadata(entry.path())
                    .map(|m| file_type_bits(&m.file_type()))
                    .unwrap_or(0),
            }
        } else {
            0
        };
        out.push((entry.file_name(), kind));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        out.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    }
    Ok(out)
}

/// `fs.readdirSync(p)`.
pub fn readdir(p: &str) -> Result<Vec<String>> {
    validate_path(p, "path")?;
    Ok(read_dir_raw(p, false)?.into_iter().map(|(n, _)| lossy(n)).collect())
}

/// `fs.readdirSync(p, { withFileTypes: true })`.
pub fn readdir_with_file_types(p: &str) -> Result<Vec<Dirent>> {
    validate_path(p, "path")?;
    Ok(read_dir_raw(p, true)?
        .into_iter()
        .map(|(n, kind)| Dirent {
            name: lossy(n),
            parent_path: p.to_string(),
            file_type: kind,
        })
        .collect())
}

/// `fs.statSync(p)`.
pub fn stat(p: &str) -> Result<Stats> {
    validate_path(p, "path")?;
    std::fs::metadata(os_path(p))
        .map(|m| Stats::from_metadata(&m))
        .map_err(|e| io_error(e, "stat", Some(p), None))
}

/// `fs.lstatSync(p)`.
pub fn lstat(p: &str) -> Result<Stats> {
    validate_path(p, "path")?;
    std::fs::symlink_metadata(os_path(p))
        .map(|m| Stats::from_metadata(&m))
        .map_err(|e| io_error(e, "lstat", Some(p), None))
}

/// `fs.statSync(p, { throwIfNoEntry: false })`: `None` for ENOENT and ENOTDIR.
pub fn stat_opt(p: &str) -> Result<Option<Stats>> {
    match stat(p) {
        Ok(s) => Ok(Some(s)),
        Err(e) if matches!(error_code(&e), Some("ENOENT") | Some("ENOTDIR")) => Ok(None),
        Err(e) => Err(e),
    }
}

/// `fs.lstatSync(p, { throwIfNoEntry: false })`: `None` for ENOENT only.
pub fn lstat_opt(p: &str) -> Result<Option<Stats>> {
    match lstat(p) {
        Ok(s) => Ok(Some(s)),
        Err(e) if error_code(&e) == Some("ENOENT") => Ok(None),
        Err(e) => Err(e),
    }
}

/// `fs.existsSync(p)` (follows symlinks; invalid paths are `false`).
pub fn exists(p: &str) -> bool {
    if p.is_empty() || p.contains('\0') {
        return false;
    }
    std::fs::metadata(os_path(p)).is_ok()
}

/// `fs.accessSync(p, mode)` (`constants::{F_OK, R_OK, W_OK, X_OK}`).
pub fn access(p: &str, mode: u32) -> Result<()> {
    validate_path(p, "path")?;
    let os = os_path(p);
    #[cfg(unix)]
    {
        let flags = nix::unistd::AccessFlags::from_bits_truncate(mode as libc::c_int);
        nix::unistd::access(&os, flags)
            .map_err(|errno| io_error(io::Error::from_raw_os_error(errno as i32), "access", Some(p), None))
    }
    #[cfg(not(unix))]
    {
        match std::fs::metadata(&os) {
            Ok(m) => {
                if mode & W_OK != 0 && m.permissions().readonly() && !m.is_dir() {
                    return Err(uv_error("EPERM", "access", Some(p), None));
                }
                Ok(())
            }
            Err(e) => Err(io_error(e, "access", Some(p), None)),
        }
    }
}

fn eisdir_rm(p: &str) -> Error {
    let errno = {
        #[cfg(unix)]
        {
            libc::EISDIR
        }
        #[cfg(not(unix))]
        {
            21
        }
    };
    Error::Node(NodeError {
        code: "ERR_FS_EISDIR".to_string(),
        errno,
        syscall: "rm".to_string(),
        path: Some(p.to_string()),
        dest: None,
        message: format!("Path is a directory: rm returned EISDIR (is a directory) {p}"),
    })
}

/// `std::filesystem::remove`: rmdir for directories, unlink otherwise.
fn fs_remove(p: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => std::fs::remove_dir(p),
        _ => std::fs::remove_file(p),
    }
}

/// `std::filesystem::remove_all` as shipped with Node's toolchain on this platform.
fn fs_remove_all(p: &Path) -> io::Result<()> {
    let meta = match std::fs::symlink_metadata(p) {
        Ok(m) => m,
        Err(e) if uv_code(&e) == "ENOENT" => return Ok(()),
        Err(e) if uv_code(&e) == "ENOTDIR" => return std::fs::remove_file(p),
        Err(e) => return Err(e),
    };
    if !meta.is_dir() {
        return std::fs::remove_file(p);
    }
    let mut child_err: Option<io::Error> = None;
    for entry in std::fs::read_dir(p)? {
        if let Err(e) = entry.and_then(|ent| fs_remove_all(&ent.path())) {
            if cfg!(target_vendor = "apple") {
                // libc++: stop iterating, still try to remove the directory itself;
                // its error (usually ENOTEMPTY) replaces the child's.
                child_err = Some(e);
                break;
            }
            // libstdc++ / MSVC: the first error is reported.
            return Err(e);
        }
    }
    std::fs::remove_dir(p)?;
    child_err.map_or(Ok(()), Err)
}

/// The `ErrnoException` thrown by the C++ part of `fs.rmSync`.
fn rm_cpp_error(e: &io::Error, p: &str) -> Error {
    // `ThrowErrnoException(errno, "rm", message, path)` with C errno values; the code is
    // Node's `errno_string(errno)` ("" for UV_UNKNOWN).
    #[cfg(unix)]
    let (eperm, enotempty, enotdir, eaccess) = (libc::EPERM, libc::ENOTEMPTY, libc::ENOTDIR, (libc::EPERM, "EPERM"));
    // MSVC <errno.h> values; permission errors use ERROR_ACCESS_DENIED (5), which
    // `errno_string` names "EIO".
    #[cfg(not(unix))]
    let (eperm, enotempty, enotdir, eaccess) = (1, 41, 20, (5, "EIO"));
    let (errno, name, msg) = match uv_code(e).as_str() {
        "EPERM" => (eperm, "EPERM", format!("Operation not permitted: {p}")),
        "ENOTEMPTY" => (enotempty, "ENOTEMPTY", format!("Directory not empty: {p}")),
        "ENOTDIR" => (enotdir, "ENOTDIR", format!("Not a directory: {p}")),
        "EACCES" => (eaccess.0, eaccess.1, format!("Permission denied: {p}")),
        _ => {
            let text = e.to_string();
            let text = match text.rfind(" (os error ") {
                Some(i) => text[..i].to_string(),
                None => text,
            };
            (-4094, "", format!("Unknown error: {text}"))
        }
    };
    Error::Node(NodeError {
        code: name.to_string(),
        errno,
        syscall: "rm".to_string(),
        path: Some(p.to_string()),
        dest: None,
        message: format!("{name}, {msg} '{p}'"),
    })
}

/// `fs.rmSync(p, { recursive, force })`.
pub fn rm(p: &str, recursive: bool, force: bool) -> Result<()> {
    validate_path(p, "path")?;
    if !force || !recursive {
        let st = if force { lstat_opt(p)? } else { Some(lstat(p)?) };
        if st.is_some_and(|s| s.is_directory()) && !recursive {
            return Err(eisdir_rm(p));
        }
    }
    let os = os_path(p);
    match std::fs::symlink_metadata(&os) {
        Err(e) if matches!(uv_code(&e).as_str(), "ENOENT" | "ENOTDIR") => return Ok(()),
        Ok(m) if m.is_dir() && !recursive => return Err(eisdir_rm(p)),
        _ => {}
    }
    let res = if recursive { fs_remove_all(&os) } else { fs_remove(&os) };
    match res {
        Ok(()) => Ok(()),
        Err(e) if uv_code(&e) == "ENOENT" => Ok(()),
        Err(e) => Err(rm_cpp_error(&e, p)),
    }
}

/// `fs.unlinkSync(p)`.
pub fn unlink(p: &str) -> Result<()> {
    validate_path(p, "path")?;
    std::fs::remove_file(os_path(p)).map_err(|e| io_error(e, "unlink", Some(p), None))
}

/// `fs.rmdirSync(p)`.
pub fn rmdir(p: &str) -> Result<()> {
    validate_path(p, "path")?;
    std::fs::remove_dir(os_path(p)).map_err(|e| io_error(e, "rmdir", Some(p), None))
}

/// `fs.renameSync(from, to)`.
pub fn rename(from: &str, to: &str) -> Result<()> {
    validate_path(from, "oldPath")?;
    validate_path(to, "newPath")?;
    std::fs::rename(os_path(from), os_path(to)).map_err(|e| io_error(e, "rename", Some(from), Some(to)))
}

/// `fs.copyFileSync(from, to)`.
pub fn copy_file(from: &str, to: &str) -> Result<()> {
    copy_file_mode(from, to, 0)
}

/// `fs.copyFileSync(from, to, mode)` (`constants::COPYFILE_EXCL` fails when `to` exists).
pub fn copy_file_mode(from: &str, to: &str, mode: u32) -> Result<()> {
    validate_path(from, "src")?;
    validate_path(to, "dest")?;
    let (src, dst) = (os_path(from), os_path(to));
    let err = |e: io::Error| io_error(e, "copyfile", Some(from), Some(to));
    let src_meta = std::fs::metadata(&src).map_err(err)?;
    if src_meta.is_dir() {
        let name = if cfg!(target_vendor = "apple") {
            "ENOTSUP"
        } else if cfg!(windows) {
            "EPERM"
        } else {
            "EISDIR"
        };
        return Err(uv_error(name, "copyfile", Some(from), Some(to)));
    }
    if mode & COPYFILE_EXCL != 0 {
        let mut input = std::fs::File::open(&src).map_err(err)?;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dst)
            .map_err(err)?;
        io::copy(&mut input, &mut output).map_err(err)?;
        std::fs::set_permissions(&dst, src_meta.permissions()).map_err(err)?;
        return Ok(());
    }
    std::fs::copy(&src, &dst).map(|_| ()).map_err(err)
}

/// Windows root regex of `realpathSync`: `^(?:[a-zA-Z]:|[\\/]{2}[^\\/]+[\\/][^\\/]+)?[\\/]*`.
fn split_root_win(s: &str) -> String {
    let b = s.as_bytes();
    let is_sep = |c: u8| c == b'/' || c == b'\\';
    let mut i = 0;
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        i = 2;
    } else if b.len() >= 2 && is_sep(b[0]) && is_sep(b[1]) {
        let mut j = 2;
        let start = j;
        while j < b.len() && !is_sep(b[j]) {
            j += 1;
        }
        if j > start && j < b.len() && is_sep(b[j]) {
            let k0 = j + 1;
            let mut k = k0;
            while k < b.len() && !is_sep(b[k]) {
                k += 1;
            }
            if k > k0 {
                i = k;
            }
        }
    }
    while i < b.len() && is_sep(b[i]) {
        i += 1;
    }
    s[..i].to_string()
}

fn split_root(s: &str) -> String {
    if cfg!(windows) {
        split_root_win(s)
    } else {
        let n = s.bytes().take_while(|c| *c == b'/').count();
        s[..n].to_string()
    }
}

fn next_part(p: &str, i: usize) -> Option<usize> {
    p.as_bytes()
        .iter()
        .enumerate()
        .skip(i)
        .find(|(_, c)| **c == b'/' || (cfg!(windows) && **c == b'\\'))
        .map(|(i, _)| i)
}

#[cfg(unix)]
fn dev_ino(m: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn dev_ino(_m: &std::fs::Metadata) -> Option<(u64, u64)> {
    None
}

/// `fs.realpathSync(p)`: the JS walk of Node's `lib/fs.js`.
pub fn realpath(p: &str) -> Result<String> {
    validate_path(p, "path")?;
    let mut p = crate::path::resolve(&[p]);
    let mut seen_links: indexmap::IndexMap<(u64, u64), String> = indexmap::IndexMap::new();
    let mut known_hard: indexmap::IndexSet<String> = indexmap::IndexSet::new();
    let mut reached_pipe_or_socket = false;

    let mut current = split_root(&p);
    let mut base = current.clone();
    let mut pos = current.len();

    if cfg!(windows) {
        std::fs::symlink_metadata(&base).map_err(|e| io_error(e, "lstat", Some(&base), None))?;
        known_hard.insert(base.clone());
    }

    while pos < p.len() {
        let previous = current.clone();
        match next_part(&p, pos) {
            None => {
                let last = &p[pos..];
                current.push_str(last);
                base = format!("{previous}{last}");
                pos = p.len();
            }
            Some(result) => {
                current.push_str(&p[pos..=result]);
                base = format!("{previous}{}", &p[pos..result]);
                pos = result + 1;
            }
        }

        if known_hard.contains(&base) {
            if reached_pipe_or_socket {
                break;
            }
            continue;
        }

        let stats = std::fs::symlink_metadata(&base).map_err(|e| io_error(e, "lstat", Some(&base), None))?;
        if !stats.file_type().is_symlink() {
            known_hard.insert(base.clone());
            continue;
        }

        let id = if cfg!(windows) { None } else { dev_ino(&stats) };
        let mut link_target = id.and_then(|id| seen_links.get(&id).cloned());
        if link_target.is_none() {
            let target_stats = std::fs::metadata(&base).map_err(|e| io_error(e, "stat", Some(&base), None))?;
            let bits = file_type_bits(&target_stats.file_type());
            reached_pipe_or_socket = bits == S_IFIFO || bits == S_IFSOCK;
            let t = std::fs::read_link(&base).map_err(|e| io_error(e, "readlink", Some(&base), None))?;
            link_target = Some(strip_verbatim(lossy(t.into_os_string())));
        }
        let link_target = link_target.unwrap_or_default();
        let resolved_link = crate::path::resolve(&[&previous, &link_target]);
        if let Some(id) = id {
            seen_links.insert(id, link_target);
        }

        // Resolve the link, then start over
        p = crate::path::resolve(&[&resolved_link, &p[pos..]]);

        // Skip over roots
        current = split_root(&p);
        base = current.clone();
        pos = current.len();

        if cfg!(windows) && !known_hard.contains(&base) {
            std::fs::symlink_metadata(&base).map_err(|e| io_error(e, "lstat", Some(&base), None))?;
            known_hard.insert(base.clone());
        }
    }
    Ok(p)
}

/// `fs.realpathSync.native(p)` (realpath(3); errors use syscall `realpath`).
pub fn realpath_native(p: &str) -> Result<String> {
    validate_path(p, "path")?;
    std::fs::canonicalize(os_path(p))
        .map(|r| strip_verbatim(lossy(r.into_os_string())))
        .map_err(|e| io_error(e, "realpath", Some(p), None))
}

/// `fs.chmodSync(p, mode)` (Windows only toggles the read-only attribute, like libuv).
pub fn chmod(p: &str, mode: u32) -> Result<()> {
    validate_path(p, "path")?;
    let os = os_path(p);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&os, std::fs::Permissions::from_mode(mode))
            .map_err(|e| io_error(e, "chmod", Some(p), None))
    }
    #[cfg(not(unix))]
    {
        let meta = std::fs::metadata(&os).map_err(|e| io_error(e, "chmod", Some(p), None))?;
        let mut perms = meta.permissions();
        perms.set_readonly(mode & 0o200 == 0);
        std::fs::set_permissions(&os, perms).map_err(|e| io_error(e, "chmod", Some(p), None))
    }
}

/// `fs.chownSync(p, uid, gid)` (no-op on Windows, like Node).
pub fn chown(p: &str, uid: u32, gid: u32) -> Result<()> {
    validate_path(p, "path")?;
    #[cfg(unix)]
    {
        std::os::unix::fs::chown(os_path(p), Some(uid), Some(gid)).map_err(|e| io_error(e, "chown", Some(p), None))
    }
    #[cfg(not(unix))]
    {
        let _ = (uid, gid);
        Ok(())
    }
}

const TEMP_CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

fn random_suffix() -> String {
    let mut bytes = [0u8; 6];
    if getrandom::fill(&mut bytes).is_err() {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (seed >> (i * 8)) as u8;
        }
    }
    bytes
        .iter()
        .map(|b| TEMP_CHARS[usize::from(*b) % TEMP_CHARS.len()] as char)
        .collect()
}

/// Creates `<prefix>XXXXXX` with mode 0o700; returns (path, attempted path on error).
fn mkdtemp_impl(prefix: &str) -> std::result::Result<String, (io::Error, String)> {
    let mut last = String::new();
    for _ in 0..100 {
        let candidate = format!("{prefix}{}", random_suffix());
        match mkdir_raw(&os_path(&candidate), 0o700) {
            Ok(()) => return Ok(candidate),
            Err(e) if uv_code(&e) == "EEXIST" => last = candidate,
            Err(e) => return Err((e, candidate)),
        }
    }
    Err((io_from_name("EEXIST"), last))
}

/// `fs.mkdtempSync(prefix)`.
pub fn mkdtemp(prefix: &str) -> Result<String> {
    validate_path(prefix, "prefix")?;
    mkdtemp_impl(prefix).map_err(|(e, _)| io_error(e, "mkdtemp", Some(&format!("{prefix}XXXXXX")), None))
}

/// `fs.symlinkSync(target, p)`. The target is stored verbatim (relative to the link).
pub fn symlink(target: &str, p: &str) -> Result<()> {
    symlink_type(target, p, None)
}

/// `fs.symlinkSync(target, p, type)` with `type` `"file" | "dir" | "junction"` (Windows).
pub fn symlink_type(target: &str, p: &str, kind: Option<&str>) -> Result<()> {
    validate_path(target, "target")?;
    validate_path(p, "path")?;
    let link = os_path(p);
    let err = |e: io::Error| io_error(e, "symlink", Some(target), Some(p));
    #[cfg(unix)]
    {
        let _ = kind;
        std::os::unix::fs::symlink(target, &link).map_err(err)
    }
    #[cfg(windows)]
    {
        let is_dir = match kind {
            Some("dir") | Some("junction") => true,
            Some(_) => false,
            None => {
                let absolute = crate::path::resolve(&[p, "..", target]);
                std::fs::metadata(&absolute).is_ok_and(|m| m.is_dir())
            }
        };
        let target_os = if crate::path::is_absolute(target) {
            target.to_string()
        } else {
            target.replace('/', "\\")
        };
        if is_dir {
            std::os::windows::fs::symlink_dir(&target_os, &link).map_err(err)
        } else {
            std::os::windows::fs::symlink_file(&target_os, &link).map_err(err)
        }
    }
}

/// `fs.linkSync(existing, new)`.
pub fn link(existing: &str, new_path: &str) -> Result<()> {
    validate_path(existing, "existingPath")?;
    validate_path(new_path, "newPath")?;
    std::fs::hard_link(os_path(existing), os_path(new_path))
        .map_err(|e| io_error(e, "link", Some(existing), Some(new_path)))
}

/// `fs.readlinkSync(p)`.
pub fn readlink(p: &str) -> Result<String> {
    validate_path(p, "path")?;
    std::fs::read_link(os_path(p))
        .map(|t| strip_verbatim(lossy(t.into_os_string())))
        .map_err(|e| io_error(e, "readlink", Some(p), None))
}

fn file_time(seconds: f64) -> filetime::FileTime {
    let secs = seconds.floor();
    let nanos = ((seconds - secs) * 1e9).round().clamp(0.0, 999_999_999.0);
    filetime::FileTime::from_unix_time(secs as i64, nanos as u32)
}

/// `fs.utimesSync(p, atime, mtime)` with times in seconds (a JS `Date` is `ms / 1000`).
pub fn utimes(p: &str, atime_secs: f64, mtime_secs: f64) -> Result<()> {
    validate_path(p, "path")?;
    filetime::set_file_times(os_path(p), file_time(atime_secs), file_time(mtime_secs))
        .map_err(|e| io_error(e, "utime", Some(p), None))
}

/// `fs.truncateSync(p, len)`.
pub fn truncate(p: &str, len: u64) -> Result<()> {
    validate_path(p, "path")?;
    let f = open_std(p, "r+", None)?;
    f.set_len(len).map_err(|e| io_error(e, "ftruncate", None, None))
}

/// A file opened with [`open`] (`fs.openSync` + the fd-based sync functions).
#[derive(Debug)]
pub struct File {
    file: std::fs::File,
    path: String,
}

fn read_at(file: &std::fs::File, buf: &mut [u8], position: Option<u64>) -> io::Result<usize> {
    match position {
        None => (&*file).read(buf),
        Some(pos) => {
            #[cfg(unix)]
            {
                std::os::unix::fs::FileExt::read_at(file, buf, pos)
            }
            #[cfg(windows)]
            {
                std::os::windows::fs::FileExt::seek_read(file, buf, pos)
            }
        }
    }
}

fn write_at(file: &std::fs::File, data: &[u8], position: Option<u64>) -> io::Result<usize> {
    match position {
        None => (&*file).write(data),
        Some(pos) => {
            #[cfg(unix)]
            {
                std::os::unix::fs::FileExt::write_at(file, data, pos)
            }
            #[cfg(windows)]
            {
                std::os::windows::fs::FileExt::seek_write(file, data, pos)
            }
        }
    }
}

impl File {
    /// The path passed to [`open`].
    pub fn path(&self) -> &str {
        &self.path
    }

    /// `fs.readSync(fd, buf, 0, buf.len(), position)`; returns the number of bytes read.
    pub fn read(&mut self, buf: &mut [u8], position: Option<u64>) -> Result<usize> {
        read_at(&self.file, buf, position).map_err(|e| io_error(e, "read", None, None))
    }

    /// `fs.writeSync(fd, data, 0, data.len(), position)`; returns the number of bytes written.
    pub fn write(&mut self, data: &[u8], position: Option<u64>) -> Result<usize> {
        write_at(&self.file, data, position).map_err(|e| io_error(e, "write", None, None))
    }

    /// Writes all of `data` at the current position.
    pub fn write_all(&mut self, data: &[u8]) -> Result<()> {
        self.file.write_all(data).map_err(|e| io_error(e, "write", None, None))
    }

    /// Reads from the current position to the end.
    pub fn read_to_end(&mut self) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        self.file
            .read_to_end(&mut buf)
            .map_err(|e| io_error(e, "read", None, None))?;
        Ok(buf)
    }

    /// Moves the current position (used by stream-like readers).
    pub fn seek(&mut self, pos: u64) -> Result<u64> {
        self.file
            .seek(io::SeekFrom::Start(pos))
            .map_err(|e| io_error(e, "read", None, None))
    }

    /// `fs.fstatSync(fd)`.
    pub fn stat(&self) -> Result<Stats> {
        self.file
            .metadata()
            .map(|m| Stats::from_metadata(&m))
            .map_err(|e| io_error(e, "fstat", None, None))
    }

    /// `fs.ftruncateSync(fd, len)`.
    pub fn truncate(&self, len: u64) -> Result<()> {
        self.file.set_len(len).map_err(|e| io_error(e, "ftruncate", None, None))
    }

    /// `fs.fsyncSync(fd)`.
    pub fn sync(&self) -> Result<()> {
        self.file.sync_all().map_err(|e| io_error(e, "fsync", None, None))
    }

    /// `fs.closeSync(fd)`.
    pub fn close(self) -> Result<()> {
        drop(self.file);
        Ok(())
    }
}

/// `fs.openSync(p, flags, mode)` (`flags` as in Node: `"r"`, `"w"`, `"wx"`, `"a+"`, ...).
pub fn open(p: &str, flags: &str, mode: Option<u32>) -> Result<File> {
    validate_path(p, "path")?;
    let file = open_std(p, flags, mode)?;
    Ok(File {
        file,
        path: p.to_string(),
    })
}

// ---------------------------------------------------------------------------------------
// fs.watchFile

/// Handle returned by [`watch_file`]. [`StatWatcher::stop`] is `fs.unwatchFile(p, listener)`.
/// Dropping the handle does not stop polling (as in Node).
#[derive(Clone)]
pub struct StatWatcher {
    first: crate::time::Timeout,
    interval: crate::time::Interval,
}

impl StatWatcher {
    /// `fs.unwatchFile(p, listener)`.
    pub fn stop(&self) {
        self.first.clear();
        self.interval.clear();
    }

    /// Whether the watcher is still polling.
    pub fn is_active(&self) -> bool {
        self.interval.is_pending()
    }
}

struct PollState {
    path: PathBuf,
    /// Last successful stat (all zero before the first one).
    last: Stats,
    /// libuv `busy_polling`: 0 before the first poll, 1 after a successful stat, the
    /// (negative) error number after a failed one.
    busy: i32,
    /// Node `StatWatcher[kOldStatus]`.
    old_status: i32,
    listener: StatListener,
}

type StatListener = Box<dyn FnMut(&Stats, &Stats) + Send>;

impl PollState {
    /// Node `onchange(newStatus, stats)`: the listener gets `(current, previous)`.
    fn emit(&mut self, status: i32, current: &Stats, previous: &Stats) {
        if self.old_status == -1 && status == -1 && current.nlink == previous.nlink {
            return;
        }
        self.old_status = status;
        (self.listener)(current, previous);
    }

    /// libuv `poll_cb` (src/fs-poll.c).
    fn poll(&mut self) {
        match std::fs::metadata(&self.path) {
            Err(e) => {
                let status = uv_error_info(&e).1;
                if self.busy != status {
                    let previous = self.last.clone();
                    self.emit(status, &Stats::default(), &previous);
                    self.busy = status;
                }
            }
            Ok(m) => {
                let current = Stats::from_metadata(&m);
                if self.busy != 0 && (self.busy < 0 || !same_stat(&self.last, &current)) {
                    let previous = self.last.clone();
                    self.emit(0, &current, &previous);
                }
                self.last = current;
                self.busy = 1;
            }
        }
    }
}

/// libuv `statbuf_eq`.
fn same_stat(a: &Stats, b: &Stats) -> bool {
    a.ctime_ns == b.ctime_ns
        && a.mtime_ns == b.mtime_ns
        && a.birthtime_ns == b.birthtime_ns
        && a.size == b.size
        && a.mode == b.mode
        && a.uid == b.uid
        && a.gid == b.gid
        && a.ino == b.ino
        && a.dev == b.dev
}

/// `fs.watchFile(p, { interval }, (current, previous) => ...)`: polls `stat(p)` every
/// `interval_ms` and calls `listener(current, previous)` when the result changes. A missing
/// file is reported once with all-zero stats (`Stats::default()`), like Node. Polling uses
/// [`crate::time`] timers, so fake timers drive it in tests.
///
/// PORT: Node shares one poller per resolved path between all listeners; here every call
/// polls on its own (identical results unless the same file is watched twice).
pub fn watch_file(
    p: &str,
    interval_ms: u64,
    listener: impl FnMut(&Stats, &Stats) + Send + 'static,
) -> Result<StatWatcher> {
    validate_path(p, "path")?;
    let state = Arc::new(Mutex::new(PollState {
        path: os_path(&crate::path::resolve(&[p])),
        last: Stats::default(),
        busy: 0,
        old_status: -1,
        listener: Box::new(listener),
    }));
    // libuv stats once right away, then every interval.
    let first_state = state.clone();
    let first = crate::time::set_timeout(0, move || first_state.lock().unwrap().poll());
    let interval = crate::time::set_interval(interval_ms, move || state.lock().unwrap().poll());
    Ok(StatWatcher { first, interval })
}

// ---------------------------------------------------------------------------------------
// fs/promises

/// `node:fs/promises`: the same operations on tokio's blocking pool (the caller's
/// `CwdGuard` is carried over). Behaviour differences from the sync API follow Node:
/// [`rm`](promises::rm) uses rimraf, [`realpath`](promises::realpath) is native, and
/// [`mkdtemp`](promises::mkdtemp) errors show the attempted name.
pub mod promises {
    use super::*;

    async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
        let cwd = crate::env::testing::cwd_overridden().then(crate::env::cwd);
        let task = move || {
            let _guard = cwd.map(crate::env::testing::CwdGuard::set);
            f()
        };
        match tokio::runtime::Handle::try_current() {
            Ok(_) => match tokio::task::spawn_blocking(task).await {
                Ok(r) => r,
                Err(e) => Err(Error::msg(format!("fs task failed: {e}"))),
            },
            Err(_) => task(),
        }
    }

    pub async fn read_file(p: &str) -> Result<Vec<u8>> {
        let p = p.to_string();
        blocking(move || super::read_file(&p)).await
    }

    pub async fn read_to_string(p: &str) -> Result<String> {
        let p = p.to_string();
        blocking(move || super::read_to_string(&p)).await
    }

    pub async fn write_file(p: &str, data: impl AsRef<[u8]>, mode: Option<u32>) -> Result<()> {
        let (p, data) = (p.to_string(), data.as_ref().to_vec());
        blocking(move || super::write_file(&p, data, mode)).await
    }

    pub async fn write_file_with_flag(p: &str, data: impl AsRef<[u8]>, flag: &str, mode: Option<u32>) -> Result<()> {
        let (p, data, flag) = (p.to_string(), data.as_ref().to_vec(), flag.to_string());
        blocking(move || super::write_file_with_flag(&p, data, &flag, mode)).await
    }

    pub async fn append_file(p: &str, data: impl AsRef<[u8]>) -> Result<()> {
        let (p, data) = (p.to_string(), data.as_ref().to_vec());
        blocking(move || super::append_file(&p, data)).await
    }

    pub async fn mkdir(p: &str, recursive: bool, mode: Option<u32>) -> Result<()> {
        let p = p.to_string();
        blocking(move || super::mkdir(&p, recursive, mode)).await
    }

    pub async fn readdir(p: &str) -> Result<Vec<String>> {
        let p = p.to_string();
        blocking(move || super::readdir(&p)).await
    }

    pub async fn readdir_with_file_types(p: &str) -> Result<Vec<Dirent>> {
        let p = p.to_string();
        blocking(move || super::readdir_with_file_types(&p)).await
    }

    pub async fn stat(p: &str) -> Result<Stats> {
        let p = p.to_string();
        blocking(move || super::stat(&p)).await
    }

    pub async fn lstat(p: &str) -> Result<Stats> {
        let p = p.to_string();
        blocking(move || super::lstat(&p)).await
    }

    /// Not in `fs/promises`; mirrors `existsSync` for async callers.
    pub async fn exists(p: &str) -> bool {
        let p = p.to_string();
        blocking(move || Ok(super::exists(&p))).await.unwrap_or(false)
    }

    pub async fn access(p: &str, mode: u32) -> Result<()> {
        let p = p.to_string();
        blocking(move || super::access(&p, mode)).await
    }

    fn rimraf(path: &str) -> Result<()> {
        match rimraf_inner(path) {
            Err(e) if error_code(&e) == Some("ENOENT") => Ok(()),
            r => r,
        }
    }

    fn rimraf_inner(path: &str) -> Result<()> {
        match super::lstat(path) {
            Err(e) => {
                if error_code(&e) == Some("ENOENT") {
                    return Ok(());
                }
                if cfg!(windows) && error_code(&e) == Some("EPERM") {
                    return fix_win_eperm(path, e);
                }
            }
            Ok(st) if st.is_directory() => return rmdir_(path, None),
            Ok(_) => {}
        }
        match super::unlink(path) {
            Ok(()) => Ok(()),
            Err(e) => match error_code(&e) {
                Some("ENOENT") => Ok(()),
                Some("EISDIR") => rmdir_(path, Some(e)),
                Some("EPERM") => {
                    if cfg!(windows) {
                        fix_win_eperm(path, e)
                    } else {
                        rmdir_(path, Some(e))
                    }
                }
                _ => Err(e),
            },
        }
    }

    fn fix_win_eperm(path: &str, original: Error) -> Result<()> {
        if let Err(e) = super::chmod(path, 0o666) {
            return if error_code(&e) == Some("ENOENT") {
                Ok(())
            } else {
                Err(original)
            };
        }
        match super::stat(path) {
            Err(e) => {
                if error_code(&e) == Some("ENOENT") {
                    Ok(())
                } else {
                    Err(original)
                }
            }
            Ok(st) if st.is_directory() => rmdir_(path, Some(original)),
            Ok(_) => super::unlink(path),
        }
    }

    fn rmdir_(path: &str, original: Option<Error>) -> Result<()> {
        match super::rmdir(path) {
            Ok(()) => Ok(()),
            Err(e) => {
                let code = error_code(&e).unwrap_or("");
                let not_empty = code == "ENOTEMPTY" || code == "EEXIST" || (!cfg!(windows) && code == "EPERM");
                if not_empty {
                    return rmchildren(path);
                }
                if code == "ENOTDIR" {
                    return match original {
                        Some(o) => Err(o),
                        None => Ok(()),
                    };
                }
                Err(e)
            }
        }
    }

    fn rmchildren(path: &str) -> Result<()> {
        let files = super::read_dir_raw(path, false)?;
        let sep = crate::path::sep();
        for (name, _) in files {
            let child = format!("{path}{sep}{}", lossy(name));
            rimraf(&child)?;
        }
        super::rmdir(path)
    }

    /// `fsp.rm(p, { recursive, force })`.
    pub async fn rm(p: &str, recursive: bool, force: bool) -> Result<()> {
        let p = p.to_string();
        blocking(move || {
            validate_path(&p, "path")?;
            match super::lstat(&p) {
                Err(e) if force && error_code(&e) == Some("ENOENT") => {}
                Err(e) => return Err(e),
                Ok(st) => {
                    if st.is_directory() && !recursive {
                        return Err(eisdir_rm(&p));
                    }
                }
            }
            rimraf(&p)
        })
        .await
    }

    pub async fn unlink(p: &str) -> Result<()> {
        let p = p.to_string();
        blocking(move || super::unlink(&p)).await
    }

    pub async fn rmdir(p: &str) -> Result<()> {
        let p = p.to_string();
        blocking(move || super::rmdir(&p)).await
    }

    pub async fn rename(from: &str, to: &str) -> Result<()> {
        let (from, to) = (from.to_string(), to.to_string());
        blocking(move || super::rename(&from, &to)).await
    }

    pub async fn copy_file(from: &str, to: &str) -> Result<()> {
        let (from, to) = (from.to_string(), to.to_string());
        blocking(move || super::copy_file(&from, &to)).await
    }

    pub async fn copy_file_mode(from: &str, to: &str, mode: u32) -> Result<()> {
        let (from, to) = (from.to_string(), to.to_string());
        blocking(move || super::copy_file_mode(&from, &to, mode)).await
    }

    /// `fsp.realpath(p)` (native realpath(3)).
    pub async fn realpath(p: &str) -> Result<String> {
        let p = p.to_string();
        blocking(move || super::realpath_native(&p)).await
    }

    pub async fn chmod(p: &str, mode: u32) -> Result<()> {
        let p = p.to_string();
        blocking(move || super::chmod(&p, mode)).await
    }

    pub async fn chown(p: &str, uid: u32, gid: u32) -> Result<()> {
        let p = p.to_string();
        blocking(move || super::chown(&p, uid, gid)).await
    }

    /// `fsp.mkdtemp(prefix)`.
    pub async fn mkdtemp(prefix: &str) -> Result<String> {
        let prefix = prefix.to_string();
        blocking(move || {
            validate_path(&prefix, "prefix")?;
            mkdtemp_impl(&prefix).map_err(|(e, attempted)| io_error(e, "mkdtemp", Some(&attempted), None))
        })
        .await
    }

    pub async fn symlink(target: &str, p: &str) -> Result<()> {
        let (target, p) = (target.to_string(), p.to_string());
        blocking(move || super::symlink(&target, &p)).await
    }

    pub async fn symlink_type(target: &str, p: &str, kind: Option<&str>) -> Result<()> {
        let (target, p, kind) = (target.to_string(), p.to_string(), kind.map(str::to_string));
        blocking(move || super::symlink_type(&target, &p, kind.as_deref())).await
    }

    pub async fn link(existing: &str, new_path: &str) -> Result<()> {
        let (existing, new_path) = (existing.to_string(), new_path.to_string());
        blocking(move || super::link(&existing, &new_path)).await
    }

    pub async fn readlink(p: &str) -> Result<String> {
        let p = p.to_string();
        blocking(move || super::readlink(&p)).await
    }

    pub async fn utimes(p: &str, atime_secs: f64, mtime_secs: f64) -> Result<()> {
        let p = p.to_string();
        blocking(move || super::utimes(&p, atime_secs, mtime_secs)).await
    }

    pub async fn truncate(p: &str, len: u64) -> Result<()> {
        let p = p.to_string();
        blocking(move || super::truncate(&p, len)).await
    }

    /// `fsp.open(p, flags, mode)`.
    pub async fn open(p: &str, flags: &str, mode: Option<u32>) -> Result<FileHandle> {
        let (p, flags) = (p.to_string(), flags.to_string());
        let path = p.clone();
        let file = blocking(move || {
            validate_path(&p, "path")?;
            open_std(&p, &flags, mode)
        })
        .await?;
        Ok(FileHandle {
            file: Arc::new(Mutex::new(Some(Arc::new(file)))),
            path,
        })
    }

    /// `FileHandle` from [`open`]. Operations after [`FileHandle::close`] fail with EBADF.
    #[derive(Clone, Debug)]
    pub struct FileHandle {
        file: Arc<Mutex<Option<Arc<std::fs::File>>>>,
        path: String,
    }

    impl FileHandle {
        /// Node `fsCall`: a closed handle fails with `Error: file closed` (code `EBADF`,
        /// `syscall` = the method's binding name; Node leaves `errno` unset).
        fn get(&self, syscall: &str) -> Result<Arc<std::fs::File>> {
            self.file.lock().unwrap().clone().ok_or_else(|| {
                Error::Node(NodeError {
                    code: "EBADF".to_string(),
                    errno: uv_errno_for_name("EBADF"),
                    syscall: syscall.to_string(),
                    path: None,
                    dest: None,
                    message: "file closed".to_string(),
                })
            })
        }

        /// The path passed to [`open`].
        pub fn path(&self) -> &str {
            &self.path
        }

        /// `filehandle.read(buf, 0, buf.len(), position)`; returns `bytesRead`.
        pub async fn read(&self, buf: &mut [u8], position: Option<u64>) -> Result<usize> {
            let file = self.get("read")?;
            let len = buf.len();
            let data = blocking(move || {
                let mut tmp = vec![0u8; len];
                let n = read_at(&file, &mut tmp, position).map_err(|e| io_error(e, "read", None, None))?;
                tmp.truncate(n);
                Ok(tmp)
            })
            .await?;
            buf[..data.len()].copy_from_slice(&data);
            Ok(data.len())
        }

        /// `filehandle.write(data, 0, data.len(), position)`; returns `bytesWritten`.
        pub async fn write(&self, data: impl AsRef<[u8]>, position: Option<u64>) -> Result<usize> {
            let file = self.get("write")?;
            let data = data.as_ref().to_vec();
            blocking(move || write_at(&file, &data, position).map_err(|e| io_error(e, "write", None, None))).await
        }

        /// `filehandle.readFile()` (from the current position to the end).
        pub async fn read_file(&self) -> Result<Vec<u8>> {
            let file = self.get("readFile")?;
            blocking(move || {
                let mut buf = Vec::new();
                (&*file)
                    .read_to_end(&mut buf)
                    .map_err(|e| io_error(e, "read", None, None))?;
                Ok(buf)
            })
            .await
        }

        /// `filehandle.writeFile(data)` (writes all of `data` at the current position).
        pub async fn write_file(&self, data: impl AsRef<[u8]>) -> Result<()> {
            let file = self.get("writeFile")?;
            let data = data.as_ref().to_vec();
            blocking(move || (&*file).write_all(&data).map_err(|e| io_error(e, "write", None, None))).await
        }

        /// `filehandle.stat()`.
        pub async fn stat(&self) -> Result<Stats> {
            let file = self.get("fstat")?;
            blocking(move || {
                file.metadata()
                    .map(|m| Stats::from_metadata(&m))
                    .map_err(|e| io_error(e, "fstat", None, None))
            })
            .await
        }

        /// `filehandle.truncate(len)`.
        pub async fn truncate(&self, len: u64) -> Result<()> {
            let file = self.get("ftruncate")?;
            blocking(move || file.set_len(len).map_err(|e| io_error(e, "ftruncate", None, None))).await
        }

        /// `filehandle.sync()`.
        pub async fn sync(&self) -> Result<()> {
            let file = self.get("fsync")?;
            blocking(move || file.sync_all().map_err(|e| io_error(e, "fsync", None, None))).await
        }

        /// `filehandle.close()`.
        pub async fn close(&self) -> Result<()> {
            let file = self.file.lock().unwrap().take();
            drop(file);
            Ok(())
        }
    }
}
