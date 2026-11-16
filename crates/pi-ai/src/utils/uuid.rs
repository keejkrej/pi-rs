//! Port of packages/ai/src/utils/uuid.ts

use std::sync::Mutex;

use pi_js::{Error, Result};

const MAX_UUID_V7_TIMESTAMP: i64 = 0xffff_ffff_ffff;
const MAX_SEQUENCE: u64 = (1u64 << 41) - 1;

struct UuidState {
    last_ordinary_timestamp: i64,
    sequence: Option<u64>,
}

static STATE: Mutex<UuidState> = Mutex::new(UuidState {
    last_ordinary_timestamp: -1,
    sequence: None,
});

/// Generate a time-ordered UUIDv7. A supplied timestamp is preserved for follower ids.
pub fn uuidv7(timestamp_ms: Option<i64>) -> Result<String> {
    let requested_timestamp = timestamp_ms.unwrap_or_else(pi_js::time::now_ms);
    if requested_timestamp < 0 || requested_timestamp > MAX_UUID_V7_TIMESTAMP {
        return Err(Error::js(
            "RangeError",
            format!("UUIDv7 timestamp must be an integer between 0 and {MAX_UUID_V7_TIMESTAMP}"),
        ));
    }

    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let effective_timestamp = match timestamp_ms {
        None => {
            let eff = requested_timestamp.max(state.last_ordinary_timestamp);
            state.last_ordinary_timestamp = eff;
            eff
        }
        Some(_) => requested_timestamp,
    };

    let random = pi_js::crypto::random_bytes(16);
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&random[..16]);

    let seq = match state.sequence {
        None => {
            let s = (u64::from(bytes[1]) << 32)
                | (u64::from(bytes[2]) << 24)
                | (u64::from(bytes[3]) << 16)
                | (u64::from(bytes[4]) << 8)
                | u64::from(bytes[5]);
            state.sequence = Some(s);
            s
        }
        Some(prev) => {
            if prev == MAX_SEQUENCE {
                return Err(Error::js("RangeError", "UUIDv7 generator sequence exhausted"));
            }
            let next = prev + 1;
            state.sequence = Some(next);
            next
        }
    };
    drop(state);

    let timestamp = effective_timestamp as u64;
    for index in 0..6 {
        bytes[5 - index] = ((timestamp >> (index * 8)) & 0xff) as u8;
    }
    bytes[6] = 0x70 | (((seq >> 37) & 0x0f) as u8);
    bytes[7] = ((seq >> 29) & 0xff) as u8;
    bytes[8] = 0x80 | (((seq >> 23) & 0x3f) as u8);
    bytes[9] = ((seq >> 15) & 0xff) as u8;
    bytes[10] = ((seq >> 7) & 0xff) as u8;
    bytes[11] = (((seq & 0x7f) << 1) as u8) | (bytes[11] & 0x01);

    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    ))
}
