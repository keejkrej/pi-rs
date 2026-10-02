//! Port of `diff/libesm/util/` (diff@8.0.4).

// PORT: `util/params.js` (`generateOptions`, a JS options-object merge) has no Rust counterpart;
// callers apply defaults on the option structs directly.

pub mod array;
pub mod distance_iterator;
pub mod string;
