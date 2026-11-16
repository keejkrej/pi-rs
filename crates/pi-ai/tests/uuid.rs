//! Port of packages/ai/test/uuid.test.ts (unit ai-utils-errors)

use indexmap::IndexSet;

use pi_ai::utils::uuid::uuidv7;
use serial_test::serial;

const TIMESTAMP: i64 = 0x0123_4567_89ab;

fn is_uuidv7(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, &b) in bytes.iter().enumerate() {
        match i {
            8 | 13 | 18 | 23 => {
                if b != b'-' {
                    return false;
                }
            }
            14 => {
                if b != b'7' {
                    return false;
                }
            }
            19 => {
                if !matches!(b, b'8' | b'9' | b'a' | b'b') {
                    return false;
                }
            }
            _ => {
                if !matches!(b, b'0'..=b'9' | b'a'..=b'f') {
                    return false;
                }
            }
        }
    }
    true
}

fn parse_timestamp(uuid: &str) -> i64 {
    let hex: String = uuid.replace('-', "");
    i64::from_str_radix(&hex[..12], 16).unwrap()
}

#[test]
#[serial]
fn generates_ordered_uuidv7s_while_preserving_follower_timestamps() {
    let first = uuidv7(None).unwrap();
    let second = uuidv7(None).unwrap();
    let third = uuidv7(None).unwrap();
    let fourth = uuidv7(None).unwrap();

    let ordinary_ids = vec![first, second, third, fourth];
    let follower_ts = TIMESTAMP - 1_000;
    let followers = vec![uuidv7(Some(follower_ts)).unwrap(), uuidv7(Some(follower_ts)).unwrap()];

    for id in ordinary_ids.iter().chain(followers.iter()) {
        assert!(is_uuidv7(id), "invalid uuidv7: {id}");
    }

    let mut sorted = ordinary_ids.clone();
    sorted.sort();
    assert_eq!(ordinary_ids, sorted);
    assert_eq!(ordinary_ids.iter().collect::<IndexSet<_>>().len(), ordinary_ids.len());
    assert_eq!(
        followers.iter().map(|u| parse_timestamp(u)).collect::<Vec<_>>(),
        vec![follower_ts, follower_ts]
    );
    assert_eq!(followers.iter().collect::<IndexSet<_>>().len(), followers.len());
}

#[test]
fn accepts_timestamp_boundaries_and_rejects_out_of_range() {
    for ts in [0_i64, (1_i64 << 48) - 1] {
        let id = uuidv7(Some(ts)).unwrap();
        assert!(is_uuidv7(&id));
        assert_eq!(parse_timestamp(&id), ts);
    }

    for invalid in [-1_i64, 1_i64 << 48] {
        let err = uuidv7(Some(invalid)).unwrap_err();
        assert_eq!(err.name(), "RangeError");
    }
}
