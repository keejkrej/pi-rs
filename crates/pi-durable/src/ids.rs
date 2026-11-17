//! Port of packages/durable/src/ids.ts

use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::types::JsonValue;

/// Marker trait identifying a durable record ID kind.
pub trait IdKind: 'static + Send + Sync {
    const KIND: &'static str;
}

/// Marker kind for [`ConversationId`].
#[derive(Debug)]
pub enum ConversationKind {}
impl IdKind for ConversationKind {
    const KIND: &'static str = "conversation";
}

/// Marker kind for [`EntryId`].
#[derive(Debug)]
pub enum EntryKind {}
impl IdKind for EntryKind {
    const KIND: &'static str = "entry";
}

/// Marker kind for [`TaskId`].
#[derive(Debug)]
pub enum TaskKind {}
impl IdKind for TaskKind {
    const KIND: &'static str = "task";
}

/// Marker kind for [`SubmissionId`].
#[derive(Debug)]
pub enum SubmissionKind {}
impl IdKind for SubmissionKind {
    const KIND: &'static str = "submission";
}

/// Marker kind for [`DocumentId`].
#[derive(Debug)]
pub enum DocumentKind {}
impl IdKind for DocumentKind {
    const KIND: &'static str = "document";
}

/// Nominal numeric identifier branded by durable record kind and (for tasks) result type.
#[repr(transparent)]
pub struct Id<Kind: IdKind, Type = JsonValue>(i64, PhantomData<fn() -> (Kind, Type)>);

impl<Kind: IdKind, Type> Clone for Id<Kind, Type> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Kind: IdKind, Type> Copy for Id<Kind, Type> {}

impl<Kind: IdKind, Type> PartialEq for Id<Kind, Type> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl<Kind: IdKind, Type> Eq for Id<Kind, Type> {}

impl<Kind: IdKind, Type> PartialOrd for Id<Kind, Type> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<Kind: IdKind, Type> Ord for Id<Kind, Type> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

impl<Kind: IdKind, Type> Hash for Id<Kind, Type> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl<Kind: IdKind, Type> fmt::Debug for Id<Kind, Type> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Id<{}>({})", Kind::KIND, self.0)
    }
}

impl<Kind: IdKind, Type> fmt::Display for Id<Kind, Type> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl<Kind: IdKind, Type> Serialize for Id<Kind, Type> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de, Kind: IdKind, Type> Deserialize<'de> for Id<Kind, Type> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = i64::deserialize(deserializer)?;
        Ok(Self::new_unchecked(raw))
    }
}

impl<Kind: IdKind, Type> From<Id<Kind, Type>> for i64 {
    fn from(id: Id<Kind, Type>) -> Self {
        id.0
    }
}

impl<Kind: IdKind, Type> Id<Kind, Type> {
    /// Construct an ID from a raw numeric value at a trusted boundary.
    pub const fn new_unchecked(value: i64) -> Self {
        Self(value, PhantomData)
    }

    /// Return the underlying integer value.
    pub const fn as_i64(self) -> i64 {
        self.0
    }

    /// Re-brand the raw ID into another type parameter of the same kind (e.g. erasing a task result).
    pub const fn cast<NewType>(self) -> Id<Kind, NewType> {
        Id(self.0, PhantomData)
    }
}

impl<Type> Id<TaskKind, Type> {
    /// Erase the strongly typed task result parameter to [`JsonValue`].
    pub const fn erased(self) -> Id<TaskKind, JsonValue> {
        self.cast::<JsonValue>()
    }
}

/// Identifier of a durable conversation.
pub type ConversationId = Id<ConversationKind>;

/// Identifier of a durable transcript entry.
pub type EntryId = Id<EntryKind>;

/// Identifier of a durable task, carrying its phantom result type `Result`.
pub type TaskId<Result = JsonValue> = Id<TaskKind, Result>;

/// Identifier of a durable submission.
pub type SubmissionId = Id<SubmissionKind>;

/// Identifier of a durable document incarnation.
pub type DocumentId = Id<DocumentKind>;

/// Strictly increasing sequence assigned to one atomic storage commit; gaps are permitted.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Seq(i64);

impl Seq {
    /// Construct a commit sequence at a trusted storage boundary.
    pub const fn new_unchecked(value: i64) -> Self {
        Self(value)
    }

    /// Return the underlying integer sequence number.
    pub const fn as_i64(self) -> i64 {
        self.0
    }
}

impl fmt::Debug for Seq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Seq({})", self.0)
    }
}

impl fmt::Display for Seq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<Seq> for i64 {
    fn from(seq: Seq) -> Self {
        seq.0
    }
}

/// Apply an erased ID brand at a trusted numeric allocation or decoding boundary.
pub const fn id_from_number<Kind: IdKind, Type>(value: i64) -> Id<Kind, Type> {
    Id::new_unchecked(value)
}

/// Apply the erased commit-sequence brand at a trusted storage boundary.
pub const fn seq_from_number(value: i64) -> Seq {
    Seq::new_unchecked(value)
}
