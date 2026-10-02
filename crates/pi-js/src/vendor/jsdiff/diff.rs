//! Port of `diff/libesm/diff/` (diff@8.0.4).

// PORT: `diff/json.js` (`diffJson`, `canonicalize`) is not ported: it diffs arbitrary JS values
// through `JSON.stringify` replacers and `toJSON`, which pi never uses.

pub mod array;
pub mod base;
pub mod character;
pub mod css;
pub mod line;
pub mod sentence;
pub mod word;
