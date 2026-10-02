//! Port of packages/protocol/src/cbor/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod decoder;
pub mod encoder;
pub mod options;
// @generated-mods end

use indexmap::IndexMap;

pub use decoder::decode_cbor;
pub use encoder::encode_cbor;
pub use options::{
    CborError, CborOptions, DEFAULT_MAX_CBOR_BYTE_LENGTH, DEFAULT_MAX_CBOR_CONTAINER_LENGTH, DEFAULT_MAX_CBOR_DEPTH,
};

/// CBOR data model for [`encode_cbor`] / [`decode_cbor`].
///
/// PORT: TS `unknown`. `serde_json::Value` cannot represent a byte string or `-0`, both of which
/// this codec round-trips. Maps keep insertion order. Owned trees cannot express cycles, array
/// holes, `undefined`, or lone surrogates; those TS inputs are rejected before a value exists.
#[derive(Clone, Debug, PartialEq)]
pub enum CborValue {
    Null,
    Bool(bool),
    /// Finite number. `-0.0` is distinct from `0.0` on the wire (`PartialEq` still equates them).
    Number(f64),
    String(String),
    /// Major type 2, a definite byte string (`Uint8Array`).
    Bytes(Vec<u8>),
    Array(Vec<CborValue>),
    Map(IndexMap<String, CborValue>),
}
