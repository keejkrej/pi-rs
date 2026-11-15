//! Port of packages/protocol/src/codec.ts

use std::sync::{Arc, Mutex, PoisonError};

use indexmap::IndexMap;
use pi_js::{Error, Result, num, str16};
use serde::Serialize;
use serde_json::{Number, Value};

use crate::cbor::{CborOptions, CborValue, decode_cbor, encode_cbor};
use crate::framing::{DEFAULT_MAX_FRAME_LENGTH, FrameDecoder, FrameDecoderOptions, encode_frame};
use crate::protocol::{
    CLIENT_MESSAGE_VALIDATOR, ClientMessage, PROTOCOL_VERSION, SERVER_MESSAGE_VALIDATOR, ServerMessage,
};

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

fn validation_error(message: impl Into<String>) -> Error {
    Error::typed("ProtocolValidationError", ProtocolValidationError::new(message))
}

fn cbor_to_json(value: &CborValue) -> std::result::Result<Value, ()> {
    match value {
        CborValue::Null => Ok(Value::Null),
        CborValue::Bool(b) => Ok(Value::Bool(*b)),
        CborValue::Number(n) => {
            if !n.is_finite() {
                return Err(());
            }
            if *n == 0.0 && n.is_sign_negative() {
                Number::from_f64(*n).map(Value::Number).ok_or(())
            } else if num::is_integer(*n) && n.abs() <= num::MAX_SAFE_INTEGER as f64 {
                if *n >= 0.0 {
                    Ok(Value::from(*n as u64))
                } else {
                    Ok(Value::from(*n as i64))
                }
            } else {
                Number::from_f64(*n).map(Value::Number).ok_or(())
            }
        }
        CborValue::String(s) => Ok(Value::String(s.clone())),
        CborValue::Bytes(_) => Err(()),
        CborValue::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(cbor_to_json(item)?);
            }
            Ok(Value::Array(out))
        }
        CborValue::Map(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), cbor_to_json(v)?);
            }
            Ok(Value::Object(out))
        }
    }
}

fn json_to_cbor(value: &Value) -> Result<CborValue> {
    match value {
        Value::Null => Ok(CborValue::Null),
        Value::Bool(b) => Ok(CborValue::Bool(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(CborValue::Number(i as f64))
            } else if let Some(u) = n.as_u64() {
                Ok(CborValue::Number(u as f64))
            } else if let Some(f) = n.as_f64() {
                Ok(CborValue::Number(f))
            } else {
                Err(Error::msg("Invalid JSON number"))
            }
        }
        Value::String(s) => Ok(CborValue::String(s.clone())),
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(json_to_cbor(item)?);
            }
            Ok(CborValue::Array(out))
        }
        Value::Object(map) => {
            let mut out = IndexMap::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), json_to_cbor(v)?);
            }
            Ok(CborValue::Map(out))
        }
    }
}

pub fn parse_client_message(value: &CborValue) -> Result<ClientMessage> {
    let Ok(mut json) = cbor_to_json(value) else {
        return Err(validation_error("Invalid client protocol message"));
    };
    if !CLIENT_MESSAGE_VALIDATOR.check(&json) {
        return Err(validation_error("Invalid client protocol message"));
    }
    // PORT: TypeBox accepts `-0` for `version: Type.Integer({ minimum: 0 })`; normalize to `0` for `i64`.
    if let Value::Object(ref mut map) = json
        && map.get("type").and_then(Value::as_str) == Some("hello")
        && let Some(Value::Number(n)) = map.get("version")
        && n.as_f64() == Some(0.0)
        && n.as_i64().is_none()
    {
        map.insert("version".to_string(), Value::from(0u64));
    }
    serde_json::from_value(json).map_err(|_| validation_error("Invalid client protocol message"))
}

pub fn parse_server_message(value: &CborValue) -> Result<ServerMessage> {
    let Ok(json) = cbor_to_json(value) else {
        return Err(validation_error("Invalid server protocol message"));
    };
    if !SERVER_MESSAGE_VALIDATOR.check(&json) {
        return Err(validation_error("Invalid server protocol message"));
    }
    serde_json::from_value(json).map_err(|_| validation_error("Invalid server protocol message"))
}

fn bounded_error_message(error: &pi_js::Error) -> String {
    let message = error.message();
    if str16::len(&message) <= 500 {
        message
    } else {
        format!("{}...", str16::slice(&message, 0, Some(497)))
    }
}

fn encode_protocol_message<T: Serialize>(
    value: &T,
    parse: fn(&CborValue) -> Result<T>,
    kind: &str,
    options: Option<FrameDecoderOptions>,
) -> Result<Vec<u8>> {
    let json = serde_json::to_value(value).map_err(|_| validation_error(format!("Invalid {kind} protocol message")))?;
    let cbor = json_to_cbor(&json).map_err(|error| {
        validation_error(format!(
            "Unable to encode {kind} protocol message: {}",
            bounded_error_message(&error)
        ))
    })?;
    let validated = parse(&cbor)?;
    let validated_json = serde_json::to_value(&validated)
        .map_err(|e| validation_error(format!("Unable to encode {kind} protocol message: {e}")))?;
    let validated_cbor = json_to_cbor(&validated_json).map_err(|error| {
        validation_error(format!(
            "Unable to encode {kind} protocol message: {}",
            bounded_error_message(&error)
        ))
    })?;
    let max_frame_length = options
        .and_then(|o| o.max_frame_length)
        .unwrap_or(DEFAULT_MAX_FRAME_LENGTH);
    let cbor_bytes = encode_cbor(
        &validated_cbor,
        Some(CborOptions {
            max_byte_length: Some(max_frame_length),
            ..Default::default()
        }),
    )
    .map_err(|error| {
        if error.downcast_ref::<ProtocolValidationError>().is_some() {
            error
        } else {
            validation_error(format!(
                "Unable to encode {kind} protocol message: {}",
                bounded_error_message(&error)
            ))
        }
    })?;
    encode_frame(&cbor_bytes).map_err(|error| {
        if error.downcast_ref::<ProtocolValidationError>().is_some() {
            error
        } else {
            validation_error(format!(
                "Unable to encode {kind} protocol message: {}",
                bounded_error_message(&error)
            ))
        }
    })
}

/// Validates and encodes one complete length-prefixed client message.
pub fn encode_client_message(message: &ClientMessage, options: Option<FrameDecoderOptions>) -> Result<Vec<u8>> {
    encode_protocol_message(message, parse_client_message, "client", options)
}

/// Validates and encodes one complete length-prefixed server message.
pub fn encode_server_message(message: &ServerMessage, options: Option<FrameDecoderOptions>) -> Result<Vec<u8>> {
    encode_protocol_message(message, parse_server_message, "server", options)
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
    fn try_new(kind: &str, parse: fn(&CborValue) -> Result<T>, options: Option<FrameDecoderOptions>) -> Result<Self> {
        let frames = FrameDecoder::try_new(options)?;
        let max_frame_length = options
            .and_then(|o| o.max_frame_length)
            .unwrap_or(DEFAULT_MAX_FRAME_LENGTH);
        Ok(Self {
            inner: Arc::new(ValidatedMessageDecoderInner {
                failed: Mutex::new(false),
                frames,
                kind: kind.to_string(),
                max_frame_length,
                parse,
            }),
        })
    }

    fn new(kind: &str, parse: fn(&CborValue) -> Result<T>, options: Option<FrameDecoderOptions>) -> Self {
        Self::try_new(kind, parse, options).unwrap_or_else(|e| panic!("{e}"))
    }

    fn push(&self, chunk: &[u8]) -> Result<Vec<T>> {
        if *self.inner.failed.lock().unwrap_or_else(PoisonError::into_inner) {
            return Err(validation_error(format!(
                "{} message decoder has failed",
                self.inner.kind
            )));
        }
        let decode_result: Result<Vec<T>> = (|| {
            let mut messages = Vec::new();
            for frame in self.inner.frames.push(chunk)? {
                let cbor = decode_cbor(
                    &frame,
                    Some(CborOptions {
                        max_byte_length: Some(self.inner.max_frame_length),
                        ..Default::default()
                    }),
                )?;
                messages.push((self.inner.parse)(&cbor)?);
            }
            Ok(messages)
        })();
        match decode_result {
            Ok(messages) => Ok(messages),
            Err(error) => {
                *self.inner.failed.lock().unwrap_or_else(PoisonError::into_inner) = true;
                if error.downcast_ref::<ProtocolValidationError>().is_some() {
                    Err(error)
                } else {
                    Err(validation_error(format!(
                        "Invalid {} protocol frame: {}",
                        self.inner.kind,
                        bounded_error_message(&error)
                    )))
                }
            }
        }
    }

    fn end(&self) -> Result<()> {
        if *self.inner.failed.lock().unwrap_or_else(PoisonError::into_inner) {
            return Err(validation_error(format!(
                "{} message decoder has failed",
                self.inner.kind
            )));
        }
        if let Err(error) = self.inner.frames.end() {
            *self.inner.failed.lock().unwrap_or_else(PoisonError::into_inner) = true;
            return Err(validation_error(format!(
                "Invalid {} protocol framing: {}",
                self.inner.kind,
                bounded_error_message(&error)
            )));
        }
        Ok(())
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
    /// PORT: Fallible constructor returning `RangeError` for out-of-range `max_frame_length`.
    pub fn try_new(options: Option<FrameDecoderOptions>) -> Result<Self> {
        Ok(Self {
            inner: Arc::new(ClientMessageDecoderInner {
                decoder: ValidatedMessageDecoder::try_new("client", parse_client_message, options)?,
            }),
        })
    }

    /// PORT: TS constructor throws `RangeError` for out-of-range `maxFrameLength`; skeleton `new`
    /// returns `Self`, so this panics on invalid options. Use [`ClientMessageDecoder::try_new`]
    /// to handle untrusted options without panicking.
    pub fn new(options: Option<FrameDecoderOptions>) -> Self {
        Self {
            inner: Arc::new(ClientMessageDecoderInner {
                decoder: ValidatedMessageDecoder::new("client", parse_client_message, options),
            }),
        }
    }

    pub fn push(&self, chunk: &[u8]) -> Result<Vec<ClientMessage>> {
        self.inner.decoder.push(chunk)
    }

    pub fn end(&self) -> Result<()> {
        self.inner.decoder.end()
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
    /// PORT: Fallible constructor returning `RangeError` for out-of-range `max_frame_length`.
    pub fn try_new(options: Option<FrameDecoderOptions>) -> Result<Self> {
        Ok(Self {
            inner: Arc::new(ServerMessageDecoderInner {
                decoder: ValidatedMessageDecoder::try_new("server", parse_server_message, options)?,
            }),
        })
    }

    /// PORT: TS constructor throws `RangeError` for out-of-range `maxFrameLength`; skeleton `new`
    /// returns `Self`, so this panics on invalid options. Use [`ServerMessageDecoder::try_new`]
    /// to handle untrusted options without panicking.
    pub fn new(options: Option<FrameDecoderOptions>) -> Self {
        Self {
            inner: Arc::new(ServerMessageDecoderInner {
                decoder: ValidatedMessageDecoder::new("server", parse_server_message, options),
            }),
        }
    }

    pub fn push(&self, chunk: &[u8]) -> Result<Vec<ServerMessage>> {
        self.inner.decoder.push(chunk)
    }

    pub fn end(&self) -> Result<()> {
        self.inner.decoder.end()
    }
}

pub fn is_supported_protocol_version(version: f64) -> bool {
    num::is_integer(version) && version == PROTOCOL_VERSION as f64
}
