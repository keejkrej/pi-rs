//! Port of packages/protocol/src/cbor/encoder.ts

use std::sync::{Arc, Mutex, PoisonError};

use pi_js::{Error, Result, num};

use super::CborValue;
use super::options::{
    CborError, CborOptions, MAX_UINT32, ResolvedCborOptions, resolve_options, text_decoder, text_encoder,
};

fn cbor_error(message: impl Into<String>) -> Error {
    Error::typed("CborError", CborError::new(message))
}

struct CborWriterBuffer {
    bytes: Vec<u8>,
    offset: usize,
}

impl CborWriterBuffer {
    fn ensure_capacity(&mut self, max_byte_length: i64, additional_bytes: usize) -> Result<()> {
        let Some(required) = self.offset.checked_add(additional_bytes) else {
            return Err(cbor_error(format!(
                "CBOR byte length exceeds configured limit of {max_byte_length}"
            )));
        };
        let max_len = max_byte_length as usize;
        if required > max_len {
            return Err(cbor_error(format!(
                "CBOR byte length exceeds configured limit of {max_byte_length}"
            )));
        }
        if required <= self.bytes.len() {
            return Ok(());
        }

        let mut capacity = self.bytes.len().max(1);
        while capacity < required {
            capacity = max_len.min(required.max(capacity.saturating_mul(2)));
        }
        self.bytes.resize(capacity, 0);
        Ok(())
    }
}

struct CborWriterInner {
    max_byte_length: i64,
    buffer: Mutex<CborWriterBuffer>,
}

/// PORT: TS class with identity. Handle; methods take `&self`.
struct CborWriter {
    inner: Arc<CborWriterInner>,
}

impl CborWriter {
    fn new(max_byte_length: i64) -> Self {
        let initial = (max_byte_length.clamp(0, 256)) as usize;
        Self {
            inner: Arc::new(CborWriterInner {
                max_byte_length,
                buffer: Mutex::new(CborWriterBuffer {
                    bytes: vec![0u8; initial],
                    offset: 0,
                }),
            }),
        }
    }

    fn write_byte(&self, value: u8) -> Result<()> {
        let mut buffer = self.inner.buffer.lock().unwrap_or_else(PoisonError::into_inner);
        buffer.ensure_capacity(self.inner.max_byte_length, 1)?;
        let offset = buffer.offset;
        buffer.bytes[offset] = value;
        buffer.offset = offset + 1;
        Ok(())
    }

    fn write_bytes(&self, bytes: &[u8]) -> Result<()> {
        let mut buffer = self.inner.buffer.lock().unwrap_or_else(PoisonError::into_inner);
        buffer.ensure_capacity(self.inner.max_byte_length, bytes.len())?;
        let offset = buffer.offset;
        buffer.bytes[offset..offset + bytes.len()].copy_from_slice(bytes);
        buffer.offset = offset + bytes.len();
        Ok(())
    }

    fn write_uint16(&self, value: u16) -> Result<()> {
        let mut buffer = self.inner.buffer.lock().unwrap_or_else(PoisonError::into_inner);
        buffer.ensure_capacity(self.inner.max_byte_length, 2)?;
        let offset = buffer.offset;
        buffer.bytes[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
        buffer.offset = offset + 2;
        Ok(())
    }

    fn write_uint32(&self, value: u32) -> Result<()> {
        let mut buffer = self.inner.buffer.lock().unwrap_or_else(PoisonError::into_inner);
        buffer.ensure_capacity(self.inner.max_byte_length, 4)?;
        let offset = buffer.offset;
        buffer.bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
        buffer.offset = offset + 4;
        Ok(())
    }

    fn write_uint64(&self, value: u64) -> Result<()> {
        let high = (value >> 32) as u32;
        let low = value as u32;
        self.write_uint32(high)?;
        self.write_uint32(low)
    }

    fn write_float64(&self, value: f64) -> Result<()> {
        let mut buffer = self.inner.buffer.lock().unwrap_or_else(PoisonError::into_inner);
        buffer.ensure_capacity(self.inner.max_byte_length, 9)?;
        let offset = buffer.offset;
        buffer.bytes[offset] = 0xfb;
        buffer.bytes[offset + 1..offset + 9].copy_from_slice(&value.to_be_bytes());
        buffer.offset = offset + 9;
        Ok(())
    }

    fn finish(&self) -> Vec<u8> {
        let buffer = self.inner.buffer.lock().unwrap_or_else(PoisonError::into_inner);
        buffer.bytes[..buffer.offset].to_vec()
    }
}

fn write_argument(writer: &CborWriter, major_type: u8, value: u64) -> Result<()> {
    let prefix = major_type << 5;
    if value < 24 {
        writer.write_byte(prefix | (value as u8))
    } else if value <= 0xff {
        writer.write_byte(prefix | 24)?;
        writer.write_byte(value as u8)
    } else if value <= 0xffff {
        writer.write_byte(prefix | 25)?;
        writer.write_uint16(value as u16)
    } else if value <= MAX_UINT32 as u64 {
        writer.write_byte(prefix | 26)?;
        writer.write_uint32(value as u32)
    } else {
        writer.write_byte(prefix | 27)?;
        writer.write_uint64(value)
    }
}

fn encode_text(writer: &CborWriter, value: &str, options: &ResolvedCborOptions) -> Result<()> {
    let bytes = text_encoder.encode(value);
    if bytes.len() as i64 > options.max_byte_length {
        return Err(cbor_error(format!(
            "CBOR text string length exceeds configured limit of {}",
            options.max_byte_length
        )));
    }
    if text_decoder.decode(&bytes)? != value {
        return Err(cbor_error("CBOR text strings must contain valid Unicode scalar values"));
    }
    write_argument(writer, 3, bytes.len() as u64)?;
    writer.write_bytes(&bytes)
}

// PORT: the TS `ancestors` set is omitted. An owned [`CborValue`] tree cannot contain a cycle.
fn encode_value(writer: &CborWriter, value: &CborValue, options: &ResolvedCborOptions, depth: i64) -> Result<()> {
    if depth > options.max_depth {
        return Err(cbor_error(format!(
            "CBOR nesting depth exceeds configured limit of {}",
            options.max_depth
        )));
    }

    match value {
        CborValue::Null => writer.write_byte(0xf6),
        CborValue::Bool(b) => writer.write_byte(if *b { 0xf5 } else { 0xf4 }),
        CborValue::Number(n) => {
            if !n.is_finite() {
                return Err(cbor_error("CBOR numbers must be finite"));
            }
            let is_neg_zero = *n == 0.0 && n.is_sign_negative();
            if num::is_integer(*n) && !is_neg_zero {
                if !num::is_safe_integer(*n) {
                    return Err(cbor_error("CBOR integers must be safe JavaScript integers"));
                }
                let int = *n as i64;
                if int >= 0 {
                    write_argument(writer, 0, int as u64)
                } else {
                    write_argument(writer, 1, (-1 - int) as u64)
                }
            } else {
                writer.write_float64(*n)
            }
        }
        CborValue::String(s) => encode_text(writer, s, options),
        CborValue::Bytes(bytes) => {
            if bytes.len() as i64 > options.max_byte_length {
                return Err(cbor_error(format!(
                    "CBOR byte string length exceeds configured limit of {}",
                    options.max_byte_length
                )));
            }
            write_argument(writer, 2, bytes.len() as u64)?;
            writer.write_bytes(bytes)
        }
        CborValue::Array(items) => {
            if items.len() as i64 > options.max_container_length {
                return Err(cbor_error(format!(
                    "CBOR array length exceeds configured limit of {}",
                    options.max_container_length
                )));
            }
            write_argument(writer, 4, items.len() as u64)?;
            for item in items {
                encode_value(writer, item, options, depth + 1)?;
            }
            Ok(())
        }
        CborValue::Map(map) => {
            if map.len() as i64 > options.max_container_length {
                return Err(cbor_error(format!(
                    "CBOR map length exceeds configured limit of {}",
                    options.max_container_length
                )));
            }
            write_argument(writer, 5, map.len() as u64)?;
            for (key, entry_value) in map {
                encode_text(writer, key, options)?;
                encode_value(writer, entry_value, options, depth + 1)?;
            }
            Ok(())
        }
    }
}

/// Encodes the protocol's strict, definite-length RFC 8949 subset.
pub fn encode_cbor(value: &CborValue, options: Option<CborOptions>) -> Result<Vec<u8>> {
    let resolved = resolve_options(options)?;
    let writer = CborWriter::new(resolved.max_byte_length);
    encode_value(&writer, value, &resolved, 0)?;
    Ok(writer.finish())
}
