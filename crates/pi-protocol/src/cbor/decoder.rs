//! Port of packages/protocol/src/cbor/decoder.ts

use std::sync::{Arc, Mutex, PoisonError};

use indexmap::{IndexMap, IndexSet};
use pi_js::{Error, Result, num};

use super::CborValue;
use super::options::{CborError, CborOptions, ResolvedCborOptions, UINT32_BASE, resolve_options, text_decoder};

fn cbor_error(message: impl Into<String>) -> Error {
    Error::typed("CborError", CborError::new(message))
}

struct CborReaderState {
    bytes: Vec<u8>,
    offset: usize,
}

struct CborReaderInner {
    options: ResolvedCborOptions,
    state: Mutex<CborReaderState>,
}

/// PORT: TS class with identity. Handle; methods take `&self`.
/// `read_bytes` returns an owned copy: the mutex cannot lend a subslice.
struct CborReader {
    inner: Arc<CborReaderInner>,
}

impl CborReader {
    fn new(bytes: Vec<u8>, options: ResolvedCborOptions) -> Self {
        Self {
            inner: Arc::new(CborReaderInner {
                options,
                state: Mutex::new(CborReaderState { bytes, offset: 0 }),
            }),
        }
    }

    fn decode(&self) -> Result<CborValue> {
        let value = self.read_item(0)?;
        let state = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.offset != state.bytes.len() {
            return Err(cbor_error("CBOR payload contains trailing data"));
        }
        Ok(value)
    }

    fn read_item(&self, depth: i64) -> Result<CborValue> {
        if depth > self.inner.options.max_depth {
            return Err(cbor_error(format!(
                "CBOR nesting depth exceeds configured limit of {}",
                self.inner.options.max_depth
            )));
        }
        let initial = self.read_byte()?;
        let major_type = initial >> 5;
        let additional_information = initial & 0x1f;

        match major_type {
            0 => {
                let value = self.read_argument(additional_information)?;
                Ok(CborValue::Number(value as f64))
            }
            1 => {
                let value = -1 - self.read_argument(additional_information)?;
                if !num::is_safe_integer(value as f64) {
                    return Err(cbor_error("Decoded CBOR integer is outside the safe range"));
                }
                Ok(CborValue::Number(value as f64))
            }
            2 => {
                let length = self.read_length(
                    additional_information,
                    "byte string",
                    self.inner.options.max_byte_length,
                )?;
                Ok(CborValue::Bytes(self.read_bytes(length)?))
            }
            3 => {
                let length = self.read_length(
                    additional_information,
                    "text string",
                    self.inner.options.max_byte_length,
                )?;
                let bytes = self.read_bytes(length)?;
                let text = text_decoder
                    .decode(&bytes)
                    .map_err(|_| cbor_error("CBOR text string contains invalid UTF-8"))?;
                Ok(CborValue::String(text))
            }
            4 => {
                let length =
                    self.read_length(additional_information, "array", self.inner.options.max_container_length)?;
                let mut result = Vec::with_capacity((length as usize).min(1024));
                for _ in 0..length {
                    result.push(self.read_item(depth + 1)?);
                }
                Ok(CborValue::Array(result))
            }
            5 => {
                let length =
                    self.read_length(additional_information, "map", self.inner.options.max_container_length)?;
                let mut result = IndexMap::with_capacity((length as usize).min(1024));
                let mut keys = IndexSet::with_capacity((length as usize).min(1024));
                for _ in 0..length {
                    let CborValue::String(key) = self.read_item(depth + 1)? else {
                        return Err(cbor_error("CBOR map keys must be strings"));
                    };
                    if !keys.insert(key.clone()) {
                        return Err(cbor_error("CBOR map contains a duplicate key"));
                    }
                    let val = self.read_item(depth + 1)?;
                    result.insert(key, val);
                }
                Ok(CborValue::Map(result))
            }
            6 => Err(cbor_error("CBOR tags are not supported")),
            7 => self.read_simple(additional_information),
            _ => Err(cbor_error("Malformed CBOR major type")),
        }
    }

    fn read_simple(&self, additional_information: u8) -> Result<CborValue> {
        match additional_information {
            20 => Ok(CborValue::Bool(false)),
            21 => Ok(CborValue::Bool(true)),
            22 => Ok(CborValue::Null),
            27 => {
                let bytes = self.read_bytes(8)?;
                let raw: [u8; 8] = bytes.try_into().map_err(|_| cbor_error("Truncated CBOR payload"))?;
                let value = f64::from_be_bytes(raw);
                if !value.is_finite() {
                    return Err(cbor_error("Decoded CBOR number must be finite"));
                }
                if num::is_integer(value) && !num::is_safe_integer(value) {
                    return Err(cbor_error("Decoded CBOR integer is outside the safe range"));
                }
                Ok(CborValue::Number(value))
            }
            31 => Err(cbor_error("CBOR break marker is not supported")),
            _ => Err(cbor_error("Unsupported CBOR simple value or floating-point width")),
        }
    }

    fn read_length(&self, additional_information: u8, kind: &str, limit: i64) -> Result<i64> {
        if additional_information == 31 {
            return Err(cbor_error(format!("Indefinite-length CBOR {kind}s are not supported")));
        }
        let length = self.read_argument(additional_information)?;
        if length > limit {
            return Err(cbor_error(format!(
                "CBOR {kind} length exceeds configured limit of {limit}"
            )));
        }
        Ok(length)
    }

    fn read_argument(&self, additional_information: u8) -> Result<i64> {
        if additional_information < 24 {
            return Ok(i64::from(additional_information));
        }
        match additional_information {
            24 => Ok(i64::from(self.read_byte()?)),
            25 => {
                let bytes = self.read_bytes(2)?;
                Ok(i64::from(bytes[0]) * 0x100 + i64::from(bytes[1]))
            }
            26 => {
                let bytes = self.read_bytes(4)?;
                Ok(i64::from(bytes[0]) * 0x1_000_000
                    + i64::from(bytes[1]) * 0x1_0000
                    + i64::from(bytes[2]) * 0x100
                    + i64::from(bytes[3]))
            }
            27 => {
                let high = self.read_argument(26)?;
                let low = self.read_argument(26)?;
                if high > 0x1f_ffff {
                    return Err(cbor_error("Decoded CBOR integer or length is outside the safe range"));
                }
                Ok(high * UINT32_BASE + low)
            }
            31 => Err(cbor_error("Indefinite-length CBOR items are not supported")),
            _ => Err(cbor_error("Malformed CBOR additional information")),
        }
    }

    fn read_byte(&self) -> Result<u8> {
        let mut state = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.offset >= state.bytes.len() {
            return Err(cbor_error("Truncated CBOR payload"));
        }
        let value = state.bytes[state.offset];
        state.offset += 1;
        Ok(value)
    }

    fn read_bytes(&self, length: i64) -> Result<Vec<u8>> {
        let len = usize::try_from(length).map_err(|_| cbor_error("Truncated CBOR payload"))?;
        let mut state = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        if len > state.bytes.len() - state.offset {
            return Err(cbor_error("Truncated CBOR payload"));
        }
        let value = state.bytes[state.offset..state.offset + len].to_vec();
        state.offset += len;
        Ok(value)
    }
}

/// Decodes exactly one item from the protocol's strict RFC 8949 subset.
pub fn decode_cbor(bytes: &[u8], options: Option<CborOptions>) -> Result<CborValue> {
    let resolved = resolve_options(options)?;
    if bytes.len() as i64 > resolved.max_byte_length {
        return Err(cbor_error(format!(
            "CBOR byte length exceeds configured limit of {}",
            resolved.max_byte_length
        )));
    }
    CborReader::new(bytes.to_vec(), resolved).decode()
}
