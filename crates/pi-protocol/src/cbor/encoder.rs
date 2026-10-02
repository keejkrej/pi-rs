//! Port of packages/protocol/src/cbor/encoder.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use pi_js::Result;

use super::CborValue;
use super::options::{CborOptions, ResolvedCborOptions};

struct CborWriterBuffer {
    bytes: Vec<u8>,
    offset: usize,
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
        todo!("port: CborWriter::new")
    }

    fn write_byte(&self, value: u8) -> Result<()> {
        todo!("port: CborWriter::write_byte")
    }

    fn write_bytes(&self, bytes: &[u8]) -> Result<()> {
        todo!("port: CborWriter::write_bytes")
    }

    fn write_uint16(&self, value: u16) -> Result<()> {
        todo!("port: CborWriter::write_uint16")
    }

    fn write_uint32(&self, value: u32) -> Result<()> {
        todo!("port: CborWriter::write_uint32")
    }

    fn write_uint64(&self, value: u64) -> Result<()> {
        todo!("port: CborWriter::write_uint64")
    }

    fn write_float64(&self, value: f64) -> Result<()> {
        todo!("port: CborWriter::write_float64")
    }

    fn finish(&self) -> Vec<u8> {
        todo!("port: CborWriter::finish")
    }

    fn ensure_capacity(&self, additional_bytes: usize) -> Result<()> {
        todo!("port: CborWriter::ensure_capacity")
    }
}

fn write_argument(writer: &CborWriter, major_type: u8, value: u64) -> Result<()> {
    todo!("port: write_argument")
}

fn encode_text(writer: &CborWriter, value: &str, options: &ResolvedCborOptions) -> Result<()> {
    todo!("port: encode_text")
}

// PORT: the TS `ancestors` set is omitted. An owned [`CborValue`] tree cannot contain a cycle.
fn encode_value(writer: &CborWriter, value: &CborValue, options: &ResolvedCborOptions, depth: i64) -> Result<()> {
    todo!("port: encode_value")
}

/// Encodes the protocol's strict, definite-length RFC 8949 subset.
pub fn encode_cbor(value: &CborValue, options: Option<CborOptions>) -> Result<Vec<u8>> {
    todo!("port: encode_cbor")
}
