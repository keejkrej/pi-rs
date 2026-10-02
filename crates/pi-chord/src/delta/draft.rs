//! Port of packages/chord/src/delta/draft.ts

/// A mutable transaction-scoped view of a JSON value, preserving tuple positions.
///
/// PORT: TS `Draft<T, Depth>` is a recursive mutable mapped type that stops at depth 8, which is
/// only a TypeScript recursion limit. The runtime value is still shaped as `T`, so this alias is `T`.
pub type Draft<T> = T;
