//! Port of packages/protocol/src/codec.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use pi_js::Result;

use crate::cbor::CborValue;
use crate::framing::{FrameDecoder, FrameDecoderOptions};
use crate::protocol::{ClientMessage, ServerMessage};

/// JS name `ProtocolValidationError`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ProtocolValidationError {
    pub message: String,
}

impl ProtocolValidationError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

pub fn parse_client_message(value: &CborValue) -> Result<ClientMessage> {
    todo!("port: parse_client_message")
}

pub fn parse_server_message(value: &CborValue) -> Result<ServerMessage> {
    todo!("port: parse_server_message")
}

fn bounded_error_message(error: &pi_js::Error) -> String {
    todo!("port: bounded_error_message")
}

fn encode_protocol_message<T>(
    value: &T,
    parse: fn(&CborValue) -> Result<T>,
    kind: &str,
    options: Option<FrameDecoderOptions>,
) -> Result<Vec<u8>> {
    todo!("port: encode_protocol_message")
}

/// Validates and encodes one complete length-prefixed client message.
pub fn encode_client_message(message: &ClientMessage, options: Option<FrameDecoderOptions>) -> Result<Vec<u8>> {
    todo!("port: encode_client_message")
}

/// Validates and encodes one complete length-prefixed server message.
pub fn encode_server_message(message: &ServerMessage, options: Option<FrameDecoderOptions>) -> Result<Vec<u8>> {
    todo!("port: encode_server_message")
}

struct ValidatedMessageDecoderInner<T> {
    failed: Mutex<bool>,
    frames: FrameDecoder,
    kind: String,
    max_frame_length: i64,
    parse: fn(&CborValue) -> Result<T>,
}

/// PORT: TS class with identity. Handle; methods take `&self`.
struct ValidatedMessageDecoder<T> {
    inner: Arc<ValidatedMessageDecoderInner<T>>,
}

impl<T> ValidatedMessageDecoder<T> {
    fn new(kind: &str, parse: fn(&CborValue) -> Result<T>, options: Option<FrameDecoderOptions>) -> Self {
        todo!("port: ValidatedMessageDecoder::new")
    }

    fn push(&self, chunk: &[u8]) -> Result<Vec<T>> {
        todo!("port: ValidatedMessageDecoder::push")
    }

    fn end(&self) -> Result<()> {
        todo!("port: ValidatedMessageDecoder::end")
    }
}

struct ClientMessageDecoderInner {
    decoder: ValidatedMessageDecoder<ClientMessage>,
}

/// Incrementally decodes and validates framed client messages.
///
/// PORT: TS class with identity. Handle; methods take `&self`.
#[derive(Clone)]
pub struct ClientMessageDecoder {
    inner: Arc<ClientMessageDecoderInner>,
}

impl ClientMessageDecoder {
    pub fn new(options: Option<FrameDecoderOptions>) -> Self {
        todo!("port: ClientMessageDecoder::new")
    }

    pub fn push(&self, chunk: &[u8]) -> Result<Vec<ClientMessage>> {
        todo!("port: ClientMessageDecoder::push")
    }

    pub fn end(&self) -> Result<()> {
        todo!("port: ClientMessageDecoder::end")
    }
}

struct ServerMessageDecoderInner {
    decoder: ValidatedMessageDecoder<ServerMessage>,
}

/// Incrementally decodes and validates framed server messages.
///
/// PORT: TS class with identity. Handle; methods take `&self`.
#[derive(Clone)]
pub struct ServerMessageDecoder {
    inner: Arc<ServerMessageDecoderInner>,
}

impl ServerMessageDecoder {
    pub fn new(options: Option<FrameDecoderOptions>) -> Self {
        todo!("port: ServerMessageDecoder::new")
    }

    pub fn push(&self, chunk: &[u8]) -> Result<Vec<ServerMessage>> {
        todo!("port: ServerMessageDecoder::push")
    }

    pub fn end(&self) -> Result<()> {
        todo!("port: ServerMessageDecoder::end")
    }
}

pub fn is_supported_protocol_version(version: f64) -> bool {
    todo!("port: is_supported_protocol_version")
}
