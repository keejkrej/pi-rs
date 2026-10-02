//! Port of packages/protocol/src/cbor/decoder.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use pi_js::Result;

use super::CborValue;
use super::options::{CborOptions, ResolvedCborOptions};

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
        todo!("port: CborReader::new")
    }

    fn decode(&self) -> Result<CborValue> {
        todo!("port: CborReader::decode")
    }

    fn read_item(&self, depth: i64) -> Result<CborValue> {
        todo!("port: CborReader::read_item")
    }

    fn read_simple(&self, additional_information: u8) -> Result<CborValue> {
        todo!("port: CborReader::read_simple")
    }

    fn read_length(&self, additional_information: u8, kind: &str, limit: i64) -> Result<i64> {
        todo!("port: CborReader::read_length")
    }

    fn read_argument(&self, additional_information: u8) -> Result<i64> {
        todo!("port: CborReader::read_argument")
    }

    fn read_byte(&self) -> Result<u8> {
        todo!("port: CborReader::read_byte")
    }

    fn read_bytes(&self, length: i64) -> Result<Vec<u8>> {
        todo!("port: CborReader::read_bytes")
    }
}

/// Decodes exactly one item from the protocol's strict RFC 8949 subset.
pub fn decode_cbor(bytes: &[u8], options: Option<CborOptions>) -> Result<CborValue> {
    todo!("port: decode_cbor")
}
