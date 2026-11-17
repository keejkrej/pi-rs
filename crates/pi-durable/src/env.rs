//! Port of packages/durable/src/env/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod node;
// @generated-mods end

use std::error::Error as StdError;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_chord::Context;
use serde::{Deserialize, Serialize};

/// Fallible operation result alias mirroring `Result<T, E>` in TS.
pub type EnvResult<T, E> = std::result::Result<T, E>;

/// Wrap a success value.
pub fn ok<T, E>(value: T) -> std::result::Result<T, E> {
    Ok(value)
}

/// Wrap an error value.
pub fn err<T, E>(error: E) -> std::result::Result<T, E> {
    Err(error)
}

/// Unwrap or convert the error into `pi_js::Error`.
pub fn get_or_throw<T, E: Into<pi_js::Error>>(result: std::result::Result<T, E>) -> pi_js::Result<T> {
    result.map_err(Into::into)
}

/// Convert a result into `Option<T>`, discarding any error.
pub fn get_or_undefined<T, E>(result: std::result::Result<T, E>) -> Option<T> {
    result.ok()
}

/// Convert any displayable error into a `pi_js::Error`.
pub fn to_error(error: impl fmt::Display) -> pi_js::Error {
    pi_js::Error::msg(error.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileErrorCode {
    Aborted,
    NotFound,
    PermissionDenied,
    NotDirectory,
    IsDirectory,
    Invalid,
    NotSupported,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileError {
    pub code: FileErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
}

impl FileError {
    pub fn new(code: FileErrorCode, message: impl Into<String>, path: Option<String>) -> Self {
        Self {
            code,
            message: message.into(),
            path,
            cause: None,
        }
    }

    pub fn with_cause(mut self, cause: impl Into<String>) -> Self {
        self.cause = Some(cause.into());
        self
    }

    pub fn name(&self) -> &'static str {
        "FileError"
    }
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl StdError for FileError {}

impl From<FileError> for pi_js::Error {
    fn from(value: FileError) -> Self {
        pi_js::Error::typed("FileError", value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionErrorCode {
    Aborted,
    Timeout,
    ShellUnavailable,
    SpawnError,
    CallbackError,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionError {
    pub code: ExecutionErrorCode,
    pub message: String,
    /// Spill file of a command that timed out or was aborted after its output crossed the spill thresholds.
    #[serde(rename = "spillPath", default, skip_serializing_if = "Option::is_none")]
    pub spill_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
}

impl ExecutionError {
    pub fn new(code: ExecutionErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            spill_path: None,
            cause: None,
        }
    }

    pub fn with_spill_path(mut self, spill_path: Option<String>) -> Self {
        self.spill_path = spill_path;
        self
    }

    pub fn with_cause(mut self, cause: impl Into<String>) -> Self {
        self.cause = Some(cause.into());
        self
    }

    pub fn name(&self) -> &'static str {
        "ExecutionError"
    }
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl StdError for ExecutionError {}

impl From<ExecutionError> for pi_js::Error {
    fn from(value: ExecutionError) -> Self {
        pi_js::Error::typed("ExecutionError", value)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    pub name: String,
    pub path: String,
    pub kind: FileKind,
    pub size: u64,
    pub mtime_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextLine {
    pub text: String,
    pub terminated: bool,
}

#[async_trait]
pub trait TextLineReader: Send + Sync {
    async fn read_line(&mut self, context: &Context) -> std::result::Result<Option<TextLine>, FileError>;
    async fn close(&mut self, context: &Context);
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadTextLinesOptions {
    pub max_lines: Option<usize>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDirOptions {
    pub recursive: Option<bool>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveOptions {
    pub recursive: Option<bool>,
    pub force: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTempFileOptions {
    pub prefix: Option<String>,
    pub suffix: Option<String>,
}

/// Content written or appended to a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileContent<'a> {
    Text(&'a str),
    Bytes(&'a [u8]),
}

impl<'a> FileContent<'a> {
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            FileContent::Text(text) => text.as_bytes(),
            FileContent::Bytes(bytes) => bytes,
        }
    }
}

impl<'a> From<&'a str> for FileContent<'a> {
    fn from(value: &'a str) -> Self {
        FileContent::Text(value)
    }
}

impl<'a> From<&'a String> for FileContent<'a> {
    fn from(value: &'a String) -> Self {
        FileContent::Text(value.as_str())
    }
}

impl<'a> From<&'a [u8]> for FileContent<'a> {
    fn from(value: &'a [u8]) -> Self {
        FileContent::Bytes(value)
    }
}

impl<'a> From<&'a Vec<u8>> for FileContent<'a> {
    fn from(value: &'a Vec<u8>) -> Self {
        FileContent::Bytes(value.as_slice())
    }
}

/// Spill the complete output to a temporary file once it exceeds either threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellSpillOptions {
    pub after_bytes: usize,
    /// Complete or partial lines.
    pub after_lines: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellExecResult {
    pub exit_code: i32,
    /// Temporary file holding the complete raw output, when the spill thresholds were exceeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spill_path: Option<String>,
}

pub type OnShellOutput = Arc<dyn Fn(&str, &Context) -> pi_js::Result<()> + Send + Sync>;

#[derive(Clone, Default)]
pub struct ShellExecOptions {
    pub cwd: Option<String>,
    pub env: Option<IndexMap<String, String>>,
    pub inherit_env: Option<bool>,
    pub timeout: Option<f64>,
    /// Every decoded chunk of combined stdout and stderr as it arrives: raw, unbounded, and unthrottled.
    pub on_output: Option<OnShellOutput>,
    pub spill: Option<ShellSpillOptions>,
}

impl fmt::Debug for ShellExecOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShellExecOptions")
            .field("cwd", &self.cwd)
            .field("env", &self.env)
            .field("inherit_env", &self.inherit_env)
            .field("timeout", &self.timeout)
            .field("has_on_output", &self.on_output.is_some())
            .field("spill", &self.spill)
            .finish()
    }
}

/// Shared cleanup lifecycle for environment capabilities.
#[async_trait]
pub trait EnvCleanup: Send + Sync {
    async fn cleanup(&self, context: &Context);
}

/// Portable filesystem capability. Operations return failures rather than throwing.
#[async_trait]
pub trait FileSystem: EnvCleanup {
    /// The file namespace: equal ids see the same files at the same paths, whatever their `cwd`.
    fn id(&self) -> &str;
    fn cwd(&self) -> &str;

    async fn absolute_path(&self, path: &str, context: &Context) -> std::result::Result<String, FileError>;
    async fn join_path(&self, parts: &[&str], context: &Context) -> std::result::Result<String, FileError>;
    async fn read_text_file(&self, path: &str, context: &Context) -> std::result::Result<String, FileError>;
    async fn open_text_line_reader(
        &self,
        path: &str,
        context: &Context,
    ) -> std::result::Result<Box<dyn TextLineReader>, FileError>;
    async fn read_text_lines(
        &self,
        path: &str,
        options: Option<ReadTextLinesOptions>,
        context: &Context,
    ) -> std::result::Result<Vec<String>, FileError>;
    async fn read_binary_file(&self, path: &str, context: &Context) -> std::result::Result<Vec<u8>, FileError>;
    async fn write_file(
        &self,
        path: &str,
        content: FileContent<'_>,
        context: &Context,
    ) -> std::result::Result<(), FileError>;
    async fn append_file(
        &self,
        path: &str,
        content: FileContent<'_>,
        context: &Context,
    ) -> std::result::Result<(), FileError>;
    /// Truncate or extend a file to exactly `size` bytes.
    async fn truncate_file(&self, path: &str, size: u64, context: &Context) -> std::result::Result<(), FileError>;
    /// Validate a floating-point byte size from JS-shaped callers before truncating.
    async fn truncate_file_number(
        &self,
        path: &str,
        size: f64,
        context: &Context,
    ) -> std::result::Result<(), FileError> {
        let resolved = self.absolute_path(path, context).await?;
        if context.abort_signal().is_some_and(|signal| signal.aborted()) {
            return Err(FileError::new(FileErrorCode::Aborted, "aborted", Some(resolved)));
        }
        if !size.is_finite() || size < 0.0 || size.fract() != 0.0 || size > pi_js::num::MAX_SAFE_INTEGER as f64 {
            return Err(FileError::new(
                FileErrorCode::Invalid,
                "File size must be a non-negative safe integer",
                Some(resolved),
            ));
        }
        self.truncate_file(path, size as u64, context).await
    }
    /// Flush file contents and metadata needed to retrieve them from an open file handle.
    async fn flush_file(&self, path: &str, context: &Context) -> std::result::Result<(), FileError>;
    async fn rename_file(
        &self,
        source_path: &str,
        destination_path: &str,
        context: &Context,
    ) -> std::result::Result<(), FileError>;
    async fn file_info(&self, path: &str, context: &Context) -> std::result::Result<FileInfo, FileError>;
    async fn list_dir(&self, path: &str, context: &Context) -> std::result::Result<Vec<FileInfo>, FileError>;
    async fn canonical_path(&self, path: &str, context: &Context) -> std::result::Result<String, FileError>;
    async fn exists(&self, path: &str, context: &Context) -> std::result::Result<bool, FileError>;
    async fn create_dir(
        &self,
        path: &str,
        options: Option<CreateDirOptions>,
        context: &Context,
    ) -> std::result::Result<(), FileError>;
    async fn remove(
        &self,
        path: &str,
        options: Option<RemoveOptions>,
        context: &Context,
    ) -> std::result::Result<(), FileError>;
    async fn create_temp_dir(&self, prefix: Option<&str>, context: &Context) -> std::result::Result<String, FileError>;
    async fn create_temp_file(
        &self,
        options: Option<CreateTempFileOptions>,
        context: &Context,
    ) -> std::result::Result<String, FileError>;
}

#[async_trait]
pub trait Shell: EnvCleanup {
    async fn exec(
        &self,
        command: &str,
        options: Option<ShellExecOptions>,
        context: &Context,
    ) -> std::result::Result<ShellExecResult, ExecutionError>;
}

#[async_trait]
pub trait ExecutionEnv: FileSystem + Shell {}

impl<T: FileSystem + Shell + ?Sized> ExecutionEnv for T {}
