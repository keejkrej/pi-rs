//! Port of packages/durable/src/entries.ts

use std::borrow::Cow;
use std::marker::PhantomData;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::harness::types::{CompactionReason, ToolDiagnostic};
use crate::types::{EntryRecord, TypedEntry};

/// Payload carried by [`TOOL_RESULT_ENTRY`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultEntryData {
    pub diagnostics: Vec<ToolDiagnostic>,
}

/// Payload carried by [`COMPACTION_ENTRY`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionEntryData {
    pub reason: CompactionReason,
}

/// Typed entry kind with a narrowing guard matching [`EntryRecord::kind`].
pub struct Entry<D = ()> {
    pub kind: Cow<'static, str>,
    _marker: PhantomData<fn() -> D>,
}

impl<D> Clone for Entry<D> {
    fn clone(&self) -> Self {
        Self {
            kind: self.kind.clone(),
            _marker: PhantomData,
        }
    }
}

impl<D> std::fmt::Debug for Entry<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entry").field("kind", &self.kind).finish()
    }
}

impl<D> Entry<D> {
    /// Construct a built-in constant entry token.
    pub const fn builtin(kind: &'static str) -> Self {
        Self {
            kind: Cow::Borrowed(kind),
            _marker: PhantomData,
        }
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Returns `true` when `entry` is present and matches this token's `kind`.
    pub fn matches(&self, entry: Option<&EntryRecord>) -> bool {
        entry.is_some_and(|e| e.kind == self.kind.as_ref())
    }

    /// Extract a typed entry when `entry` matches this token's `kind`.
    pub fn extract(&self, entry: Option<&EntryRecord>) -> pi_js::Result<Option<TypedEntry<D>>>
    where
        D: DeserializeOwned,
    {
        let Some(record) = entry else {
            return Ok(None);
        };
        if record.kind != self.kind.as_ref() {
            return Ok(None);
        }
        let raw = record.data.clone().unwrap_or(crate::types::JsonValue::Null);
        let data: D =
            serde_json::from_value(raw).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
        Ok(Some(TypedEntry {
            record: record.clone(),
            data,
        }))
    }
}

/// Define a typed entry kind whose `matches()`/`extract()` narrow by `EntryRecord.kind`.
pub fn define_entry<D>(kind: impl Into<Cow<'static, str>>) -> pi_js::Result<Entry<D>> {
    let kind = kind.into();
    if kind.is_empty() {
        return Err(pi_js::Error::js(
            "TypeError",
            "Entry kind must be a non-empty string",
        ));
    }
    Ok(Entry {
        kind,
        _marker: PhantomData,
    })
}

/// User input: `model` is `[UserMessage]`. Written by submissions.
pub const USER_ENTRY: Entry<()> = Entry::builtin("pi.user");

/// Provider result with any stop reason: `model` is `[AssistantMessage]`. Written by generation.
pub const ASSISTANT_ENTRY: Entry<()> = Entry::builtin("pi.assistant");

/// Positional prompt and tool change: `model` is `[SystemMessage]` with empty `content`.
pub const SYSTEM_ENTRY: Entry<()> = Entry::builtin("pi.system");

/// Tool result: `model` is `[ToolResultMessage]`; `data` holds structured diagnostics.
pub const TOOL_RESULT_ENTRY: Entry<ToolResultEntryData> = Entry::builtin("pi.tool-result");

/// Start of a new context: always `head = SelfId`, optional `[UserMessage]` handoff.
pub const RESET_ENTRY: Entry<()> = Entry::builtin("pi.reset");

/// Compaction summary: `model` is `[UserMessage]` with wrapped summary, `head` the first kept entry.
pub const COMPACTION_ENTRY: Entry<CompactionEntryData> = Entry::builtin("pi.compaction");
