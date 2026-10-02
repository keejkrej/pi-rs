//! Port of packages/protocol/src/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod cbor;
pub mod codec;
pub mod framing;
pub mod protocol;
// @generated-mods end
pub use pi_js::{Error, Result};

pub use crate::cbor::{
    CborError, CborOptions, CborValue, DEFAULT_MAX_CBOR_BYTE_LENGTH, DEFAULT_MAX_CBOR_CONTAINER_LENGTH,
    DEFAULT_MAX_CBOR_DEPTH, decode_cbor, encode_cbor,
};
pub use crate::codec::*;
pub use crate::framing::*;
pub use crate::protocol::{
    AttachmentEnvelope, CancelEnvelope, ClientHello, ClientMessage, PROTOCOL_VERSION, ProtocolError, ProtocolErrorCode,
    RequestEnvelope, ResponseEnvelope, RpcTarget, ServerHello, ServerHelloError, ServerId, ServerMessage,
    ServiceEventEnvelope, SessionTarget, is_server_id,
};
