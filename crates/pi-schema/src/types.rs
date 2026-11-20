//! Public types, options, and error types for JSON Schema compilation and validation.

use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::builders::Schema;
use crate::formats;

/// JSON Schema draft dialect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Draft {
    /// JSON Schema Draft 2020-12 (default unless `$schema` specifies Draft 7).
    #[default]
    Draft2020_12,
    /// JSON Schema Draft 7.
    Draft7,
}

/// How [`compile`](crate::compile) handles a `$ref` whose target cannot be resolved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum UnresolvedRefs {
    /// Return [`SchemaError::UnresolvedRef`] during compilation (default).
    #[default]
    Throw,
    /// Compile the unresolved `$ref` as `false` (rejects values only at that location with `"unresolved reference"`).
    Reject,
}

/// A format validator function for string `"format"` keywords.
#[derive(Clone)]
pub struct FormatCheck(Arc<dyn Fn(&str) -> bool + Send + Sync>);

impl FormatCheck {
    /// Wraps a predicate closure or function pointer as a [`FormatCheck`].
    pub fn new<F>(f: F) -> Self
    where
        F: Fn(&str) -> bool + Send + Sync + 'static,
    {
        Self(Arc::new(f))
    }

    /// Wraps a plain function pointer as a [`FormatCheck`].
    pub fn from_fn(f: fn(&str) -> bool) -> Self {
        Self(Arc::new(f))
    }

    /// Evaluates the format check on `value`.
    pub fn test(&self, value: &str) -> bool {
        (self.0)(value)
    }
}

impl fmt::Debug for FormatCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FormatCheck(..)")
    }
}

impl From<fn(&str) -> bool> for FormatCheck {
    fn from(f: fn(&str) -> bool) -> Self {
        Self::from_fn(f)
    }
}

/// Options controlling schema compilation, dialect, formats, external documents, and coercion.
#[derive(Clone, Debug, Default)]
pub struct CompileOptions {
    /// Explicit draft override. When `None`, root `$schema` is inspected for Draft 7 and defaults to Draft 2020-12.
    pub draft: Option<Draft>,
    /// Format validators by format name. When `None`, `format` is treated as an annotation (Draft 2020-12 default).
    pub formats: Option<IndexMap<String, FormatCheck>>,
    /// External schema documents keyed by URI for `$ref` resolution.
    pub schemas: IndexMap<String, Value>,
    /// Enable coercion mode (`coerce` / `parse`).
    pub coerce: bool,
    /// How unresolved `$ref`s are handled.
    pub unresolved_refs: UnresolvedRefs,
}

impl CompileOptions {
    /// Sets the schema draft dialect explicitly.
    pub fn draft(mut self, draft: Draft) -> Self {
        self.draft = Some(draft);
        self
    }

    /// Enables validation of the standard JSON Schema 2020-12 formats.
    pub fn with_default_formats(mut self) -> Self {
        self.formats = Some(formats::formats());
        self
    }

    /// Sets custom format validators.
    pub fn with_formats(mut self, formats: IndexMap<String, FormatCheck>) -> Self {
        self.formats = Some(formats);
        self
    }

    /// Registers an external schema document by URI for `$ref` resolution.
    pub fn with_schema(mut self, uri: impl Into<String>, schema: Value) -> Self {
        self.schemas.insert(uri.into(), schema);
        self
    }

    /// Registers multiple external schema documents by URI.
    pub fn with_schemas(mut self, schemas: IndexMap<String, Value>) -> Self {
        self.schemas = schemas;
        self
    }

    /// Enables or disables coercion mode.
    pub fn coerce(mut self, coerce: bool) -> Self {
        self.coerce = coerce;
        self
    }

    /// Sets unresolved reference handling.
    pub fn unresolved_refs(mut self, mode: UnresolvedRefs) -> Self {
        self.unresolved_refs = mode;
        self
    }
}

/// The result of `validate()` or `parse()`: the validated/coerced value on success, or all collected errors.
pub type ParseResult<T = Value> = Result<T, Vec<ValidationError>>;

/// One validation failure at an instance JSON Pointer path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationError {
    /// JSON Pointer path within the validated value (`""` for the root value).
    pub path: String,
    /// JSON Pointer path within the schema (`"#"` for the root schema).
    pub schema_path: String,
    /// Failing JSON Schema keyword (for example `"type"`, `"required"`, `"minimum"`, `"$ref"`).
    pub keyword: String,
    /// Human-readable explanation of the failure.
    pub message: String,
}

impl ValidationError {
    /// Creates a [`ValidationError`] with an explicit schema path.
    pub fn new(
        path: impl Into<String>,
        schema_path: impl Into<String>,
        keyword: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            schema_path: schema_path.into(),
            keyword: keyword.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            write!(f, "{}: {}", self.keyword, self.message)
        } else {
            write!(f, "{} ({}): {}", self.path, self.keyword, self.message)
        }
    }
}

/// Error returned when compiling a malformed schema or encountering an unresolvable `$ref`.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum SchemaError {
    #[error("A schema must be an object or a boolean, got {0}")]
    InvalidSchema(String),
    #[error("Unresolved $ref {reference} ({detail})")]
    UnresolvedRef { reference: String, detail: String },
    #[error("Invalid regular expression /{pattern}/: {message}")]
    InvalidRegex { pattern: String, message: String },
}

/// Types that can be passed to [`compile`](crate::compile) and [`compile_coercing`](crate::compile_coercing).
pub trait AsSchema {
    fn as_schema_value(&self) -> Cow<'_, Value>;
}

impl AsSchema for Value {
    fn as_schema_value(&self) -> Cow<'_, Value> {
        Cow::Borrowed(self)
    }
}

impl AsSchema for &Value {
    fn as_schema_value(&self) -> Cow<'_, Value> {
        Cow::Borrowed(self)
    }
}

impl AsSchema for Schema {
    fn as_schema_value(&self) -> Cow<'_, Value> {
        Cow::Borrowed(self.as_value())
    }
}

impl AsSchema for &Schema {
    fn as_schema_value(&self) -> Cow<'_, Value> {
        Cow::Borrowed(self.as_value())
    }
}

impl AsSchema for bool {
    fn as_schema_value(&self) -> Cow<'_, Value> {
        Cow::Owned(Value::Bool(*self))
    }
}
