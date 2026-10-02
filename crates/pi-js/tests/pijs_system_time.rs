//! pi_js::time parity: `Date.parse` / `toISOString` vectors from node v24.21.0 (`TZ=UTC`)
//! plus fake-timer behaviour of `set_timeout`, `set_interval`, `sleep` and the clock.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_js::abort::{AbortController, AbortReason};
use pi_js::error::Error;
use pi_js::time::{self, testing};

#[test]
fn date_parse_matches_node_in_utc() {
    let mut failures = Vec::new();
    for (input, want) in DATE_PARSE_UTC {
        let got = testing::parse_date_utc(input);
        if got != *want {
            failures.push(format!("Date.parse({input:?}): got {got:?}, want {want:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn date_parse_with_explicit_offsets_ignores_local_zone() {
    for (input, want) in DATE_PARSE_UTC {
        // Forms that carry an offset (or ISO date-only forms, which are UTC) parse the same
        // in every time zone.
        let has_zone = input.ends_with('Z') || input.contains("GMT") || input.contains("UTC");
        if has_zone && want.is_some() && testing::parse_date_utc(input) == *want {
            assert_eq!(time::parse_date(input), *want, "Date.parse({input:?})");
        }
    }
    assert_eq!(time::parse_date("2020-01-01"), Some(1_577_836_800_000));
    assert_eq!(time::parse_date("2020-01-01T00:00:00+01:00"), Some(1_577_833_200_000));
    assert_eq!(time::parse_date("not a date"), None);
}

/// Local-time forms depend on the host zone. Vectors exist for `America/New_York`,
/// `Europe/Berlin` and `Asia/Tokyo` (DST gaps and overlaps, LMT, range edges); run with
/// e.g. `TZ=Europe/Berlin cargo test -p pi-js --test pijs_system_time` to check one.
#[test]
fn date_parse_local_forms_match_node_for_the_tz_zone() {
    let Some(tz) = pi_js::env::var("TZ") else { return };
    let Some((_, vectors)) = DATE_PARSE_LOCAL.iter().find(|(zone, _)| *zone == tz) else {
        return;
    };
    let mut failures = Vec::new();
    for (input, want) in *vectors {
        let got = time::parse_date(input);
        if got != *want {
            failures.push(format!("Date.parse({input:?}) in {tz}: got {got:?}, want {want:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn iso_string_matches_node() {
    for (ms, want) in ISO_STRING {
        match want {
            Some(s) => {
                assert_eq!(time::try_iso_string(*ms).unwrap(), *s, "toISOString({ms})");
                assert_eq!(time::iso_string(*ms), *s, "toISOString({ms})");
            }
            None => {
                let err = time::try_iso_string(*ms).unwrap_err();
                assert_eq!(err.to_string(), "Invalid time value");
                match err {
                    Error::Js(e) => assert_eq!(e.name, "RangeError"),
                    other => panic!("expected RangeError, got {other:?}"),
                }
            }
        }
    }
}

async fn advance(ms: u64) {
    tokio::time::advance(Duration::from_millis(ms)).await;
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn set_system_time_follows_the_paused_clock() {
    testing::set_system_time(1_700_000_000_000);
    assert_eq!(time::now_ms(), 1_700_000_000_000);
    assert_eq!(time::iso_now(), "2023-11-14T22:13:20.000Z");
    let perf = time::performance_now();
    advance(1500).await;
    assert_eq!(time::now_ms(), 1_700_000_001_500);
    assert_eq!(time::performance_now() - perf, 1500.0);
    testing::set_system_time(0);
    assert_eq!(time::iso_now(), "1970-01-01T00:00:00.000Z");
    testing::clear_system_time();
    assert!(time::now_ms() > 1_700_000_000_000);
}

#[tokio::test(start_paused = true)]
async fn set_timeout_fires_at_its_deadline() {
    let log = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let (a, b, c) = (log.clone(), log.clone(), log.clone());
    let t1 = time::set_timeout(100, move || a.lock().unwrap().push("100"));
    let _t2 = time::set_timeout(50, move || b.lock().unwrap().push("50"));
    let _t3 = time::set_timeout(100, move || c.lock().unwrap().push("100b"));
    assert_eq!(testing::timer_count(), 3);
    advance(49).await;
    assert!(log.lock().unwrap().is_empty());
    advance(1).await;
    assert_eq!(*log.lock().unwrap(), ["50"]);
    assert!(t1.is_pending());
    advance(50).await;
    // Same deadline: creation order, like Node's timer lists.
    assert_eq!(*log.lock().unwrap(), ["50", "100", "100b"]);
    assert!(!t1.is_pending());
    assert_eq!(testing::timer_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn deadline_is_fixed_when_the_timer_is_created() {
    let fired = Arc::new(AtomicUsize::new(0));
    let f = fired.clone();
    let _t = time::set_timeout(100, move || {
        f.fetch_add(1, Ordering::SeqCst);
    });
    // Advancing before the timer task was ever polled must still count.
    tokio::time::advance(Duration::from_millis(100)).await;
    advance(0).await;
    assert_eq!(fired.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn clear_timeout_prevents_the_callback() {
    let fired = Arc::new(AtomicUsize::new(0));
    let f = fired.clone();
    let t = time::set_timeout(10, move || {
        f.fetch_add(1, Ordering::SeqCst);
    });
    t.unref();
    t.clear();
    assert!(!t.is_pending());
    advance(100).await;
    assert_eq!(fired.load(Ordering::SeqCst), 0);
    assert_eq!(testing::timer_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn clear_all_timers_clears_timeouts_and_intervals() {
    let fired = Arc::new(AtomicUsize::new(0));
    let (f1, f2) = (fired.clone(), fired.clone());
    let _t = time::set_timeout(10, move || {
        f1.fetch_add(1, Ordering::SeqCst);
    });
    let _i = time::set_interval(10, move || {
        f2.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(testing::timer_count(), 2);
    testing::clear_all_timers();
    assert_eq!(testing::timer_count(), 0);
    advance(100).await;
    assert_eq!(fired.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn set_interval_ticks_until_cleared() {
    let ticks = Arc::new(AtomicUsize::new(0));
    let t = ticks.clone();
    let iv = time::set_interval(1000, move || {
        t.fetch_add(1, Ordering::SeqCst);
    });
    advance(999).await;
    assert_eq!(ticks.load(Ordering::SeqCst), 0);
    advance(1).await;
    assert_eq!(ticks.load(Ordering::SeqCst), 1);
    advance(2500).await;
    assert_eq!(ticks.load(Ordering::SeqCst), 3);
    assert!(iv.is_pending());
    iv.clear();
    assert!(!iv.is_pending());
    advance(5000).await;
    assert_eq!(ticks.load(Ordering::SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn oversized_delays_become_one_millisecond() {
    let fired = Arc::new(AtomicUsize::new(0));
    let f = fired.clone();
    let _t = time::set_timeout(u64::from(u32::MAX), move || {
        f.fetch_add(1, Ordering::SeqCst);
    });
    advance(1).await;
    assert_eq!(fired.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn sleep_resolves_after_the_delay() {
    let start = tokio::time::Instant::now();
    time::sleep(250, None).await.unwrap();
    assert_eq!(start.elapsed(), Duration::from_millis(250));
}

#[tokio::test(start_paused = true)]
async fn sleep_rejects_with_the_signal_reason() {
    let ctrl = AbortController::new();
    let signal = ctrl.signal();
    let task = tokio::spawn(async move { time::sleep(10_000, Some(&signal)).await });
    advance(100).await;
    ctrl.abort(Some(AbortReason::error("CustomError", "stop")));
    let err = task.await.unwrap().unwrap_err();
    match err {
        Error::Abort(r) => {
            assert_eq!(r.name, "CustomError");
            assert_eq!(r.message, "stop");
        }
        other => panic!("expected abort, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn sleep_with_an_aborted_signal_fails_immediately() {
    let ctrl = AbortController::new();
    ctrl.abort(None);
    let start = tokio::time::Instant::now();
    let err = time::sleep(1000, Some(&ctrl.signal())).await.unwrap_err();
    assert!(matches!(&err, Error::Abort(r) if r.name == "AbortError"), "{err:?}");
    assert_eq!(start.elapsed(), Duration::ZERO);
    // A signal that never aborts does not affect the sleep.
    let live = AbortController::new();
    time::sleep(5, Some(&live.signal())).await.unwrap();
}

#[test]
fn set_timeout_outside_a_runtime_uses_a_thread() {
    let (tx, rx) = std::sync::mpsc::channel();
    let _t = time::set_timeout(5, move || tx.send(()).unwrap());
    rx.recv_timeout(Duration::from_secs(10)).expect("timeout fired");
}

// ---------------------------------------------------------------------------------------
// Generated by node v24.21.0 (`DATE_PARSE_UTC` / `ISO_STRING` with TZ=UTC,
// `DATE_PARSE_LOCAL` with the named TZ).

const DATE_PARSE_UTC: &[(&str, Option<i64>)] = &[
    ("", None),
    (" ", None),
    ("x", None),
    ("Jan", None),
    ("Jan 5", Some(978652800000)),
    ("5 Jan", Some(978652800000)),
    ("1/5", Some(978652800000)),
    ("Jan 5 49", Some(2493417600000)),
    ("Jan 5 50", Some(-630806400000)),
    ("Jan 5 99", Some(915494400000)),
    ("Jan 5 100", Some(-59011113600000)),
    ("2020 Jan", Some(1577836800000)),
    ("Jan 2020", Some(1577836800000)),
    ("13/1/2020", None),
    ("1/13/2020", Some(1578873600000)),
    ("2020/1/13", Some(1578873600000)),
    ("2020-02-31", Some(1583107200000)),
    ("2020-1-5", Some(1578182400000)),
    ("2020-01-01 10:00", Some(1577872800000)),
    ("10:00 2020-01-01", Some(1577872800000)),
    ("2020-01-01T24:00", Some(1577923200000)),
    ("2020-01-01T24:00:01", None),
    ("1 2 3 4", None),
    ("2020", Some(1577836800000)),
    ("2020-01", Some(1577836800000)),
    ("2020-01-01", Some(1577836800000)),
    ("2020-01-01T00:00", Some(1577836800000)),
    ("2020-01-01T00:00:00", Some(1577836800000)),
    ("2020-01-01T00:00:00Z", Some(1577836800000)),
    ("2020-01-01T00:00:00.000Z", Some(1577836800000)),
    ("2020-01-01T00:00:00.1Z", Some(1577836800100)),
    ("2020-01-01T00:00:00.12Z", Some(1577836800120)),
    ("2020-01-01T10:00:00.123456789Z", Some(1577872800123)),
    ("2020-01-01T10:00:00.0001234567891Z", Some(1577872800123)),
    ("2020-01-01T00:00:00+01:00", Some(1577833200000)),
    ("2020-01-01T00:00:00-0530", Some(1577856600000)),
    ("2020-01-01T00:00:00+0100", Some(1577833200000)),
    ("2020-01-01T00:00:00+01", None),
    ("+002020-01-01T00:00:00Z", Some(1577836800000)),
    ("-000001-01-01T00:00:00Z", Some(-62198755200000)),
    ("-000000-01-01T00:00:00Z", None),
    ("+275760-09-13T00:00:00.000Z", Some(8640000000000000)),
    ("+275760-09-13T00:00:00.001Z", None),
    ("-271821-04-20T00:00:00.000Z", Some(-8640000000000000)),
    ("-271821-04-19T23:59:59.999Z", None),
    ("0000-01-01T00:00:00Z", Some(-62167219200000)),
    ("1970-01-01T00:00:00Z", Some(0)),
    ("1969-12-31T23:59:59.999Z", Some(-1)),
    ("2020-13-01", None),
    ("2020-00-01", None),
    ("2020-01-00", None),
    ("2020-01-32", None),
    ("2020-02-30T00:00:00Z", Some(1583020800000)),
    ("2020-01-01T25:00", None),
    ("2020-01-01T23:60", None),
    ("2020-01-01T23:59:60", None),
    ("2020-01-01T", None),
    ("2020-01-01Tx", None),
    ("2020-01-01 ", Some(1577836800000)),
    (" 2020-01-01", Some(1577836800000)),
    ("2020-01-01T00:00:00z", Some(1577836800000)),
    ("2020-01-01t00:00:00Z", Some(1577836800000)),
    ("Thu, 01 Jan 1970 00:00:00 GMT", Some(0)),
    ("Thu, 01 Jan 1970 00:00:00 GMT+0100", Some(-3600000)),
    ("Thu, 01 Jan 1970 00:00:00 GMT-0230", Some(9000000)),
    ("Mon, 06 Oct 2025 12:00:00 GMT", Some(1759752000000)),
    ("Mon, 06 Oct 2025 12:00:00 UTC", Some(1759752000000)),
    ("Mon, 06 Oct 2025 12:00:00 UT", Some(1759752000000)),
    ("Mon, 06 Oct 2025 12:00:00 Z", Some(1759752000000)),
    ("Mon, 06 Oct 2025 12:00:00 EST", Some(1759770000000)),
    ("Mon, 06 Oct 2025 12:00:00 EDT", Some(1759766400000)),
    ("Mon, 06 Oct 2025 12:00:00 PST", Some(1759780800000)),
    ("Mon, 06 Oct 2025 12:00:00 PDT", Some(1759777200000)),
    ("Mon, 06 Oct 2025 12:00:00 CST", Some(1759773600000)),
    ("Mon, 06 Oct 2025 12:00:00 MDT", Some(1759773600000)),
    ("Mon, 06 Oct 2025 12:00:00 +0000", Some(1759752000000)),
    ("Mon, 06 Oct 2025 12:00:00 -0700", Some(1759777200000)),
    ("Monday, 06-Oct-25 12:00:00 GMT", Some(1759752000000)),
    (
        "Mon Oct 06 2025 12:00:00 GMT+0200 (Central European Summer Time)",
        Some(1759744800000),
    ),
    ("Mon Oct 6 2025", Some(1759708800000)),
    ("October 6, 2025", Some(1759708800000)),
    ("Oct 6, 2025 3:04 PM", Some(1759763040000)),
    ("Oct 6, 2025 3:04:05 pm", Some(1759763045000)),
    ("6 October 2025 15:04", Some(1759763040000)),
    ("12/31/1999 11:59 pm", Some(946684740000)),
    ("12/31/1999 12:00 am", Some(946598400000)),
    ("12/31/1999 12:00 pm", Some(946641600000)),
    ("12/31/1999 13:00 pm", None),
    ("12/31/1999 0:00 am", Some(946598400000)),
    ("2025/10/06 15:04:05", Some(1759763045000)),
    ("2025.10.06", Some(1759708800000)),
    ("10.06.2025", Some(1759708800000)),
    ("06.10.2025", Some(1749513600000)),
    ("Oct-06-2025", Some(1759708800000)),
    ("2025-Oct-06", Some(1759708800000)),
    ("06-Oct-2025", Some(1759708800000)),
    ("Tue Oct 06 2025", Some(1759708800000)),
    ("Sat, 1 Jan 2000 00:00:00 +0000 (UTC)", Some(946684800000)),
    ("(comment) Jan 1 2000", Some(946684800000)),
    ("Jan (c) 1 2000", Some(946684800000)),
    ("Jan 1 2000 (unclosed", Some(946684800000)),
    ("Jan 1 2000 10:00:00.5", Some(946720800500)),
    ("Jan 1 2000 10:00:00.123", Some(946720800123)),
    ("Jan 1 2000 10:00:00.1239", Some(946720800123)),
    ("Jan 1 2000 10:00:00:00", Some(946720800000)),
    ("Jan 1 2000 10", Some(946684800000)),
    ("Jan 1 2000 10:", Some(946720800000)),
    ("Jan 1 2000 10:00 GMT+5", Some(946702800000)),
    ("Jan 1 2000 10:00 GMT+05:30", Some(946701000000)),
    ("Jan 1 2000 10:00 +5", Some(946702800000)),
    ("Jan 1 2000 10:00 +530", Some(946701000000)),
    ("Jan 1 2000 10:00 +05:30", Some(946701000000)),
    ("Jan 1 2000 10:00 UTC+1", Some(946717200000)),
    ("Jan 1 2000 10:00 Z+1", Some(946717200000)),
    ("Jan 1 2000 T10:00", None),
    ("2000 1 1", Some(946684800000)),
    ("1 1 2000", Some(946684800000)),
    ("32 Jan 2000", None),
    ("Jan 32 2000", None),
    ("Feb 29 2001", Some(983404800000)),
    ("Jan 1 0", Some(946684800000)),
    ("Jan 1 0000", Some(946684800000)),
    ("Jan 1 -1", None),
    ("Jan 1 10000", Some(253402300800000)),
    ("Jan 1 275760", Some(8639977881600000)),
    ("Jan 1 275761", None),
    ("Sep 13 275760", Some(8640000000000000)),
    ("Septembre 1 2000", Some(967766400000)),
    ("Sept 1 2000", Some(967766400000)),
    ("Se 1 2000", None),
    ("January 1st 2000", None),
    ("1 Jan 2000 AD", None),
    ("Jan 1 2000 BC", None),
    ("2000-01-01T00:00:00.000+00:00", Some(946684800000)),
    ("2000-01-01T00:00:00.000-00:00", Some(946684800000)),
    ("2000-01-01T00:00:00.000+24:00", None),
    ("2000-01-01T00:00:00.000+23:59", Some(946598460000)),
    ("2000-01-01T00:00:00.000+2400", None),
    ("2000-01-01 00:00:00Z", Some(946684800000)),
    ("2000-01-01 00:00:00 Z", Some(946684800000)),
    ("2000-01-01T00:00:00 GMT", None),
    ("2000-01-01 00:00:00 +0100", Some(946681200000)),
    ("2000-01-01 00:00:00 PST", Some(946713600000)),
    ("2000-01-01Z", Some(946684800000)),
    ("2000-01Z", Some(946684800000)),
    ("2000Z", Some(946684800000)),
    ("2000-01-01T00Z", None),
    ("20000101", None),
    ("2000-0101", None),
    ("2000/01/01 00:00:00 UTC", Some(946684800000)),
    ("1e3", None),
    ("1000", Some(-30610224000000)),
    ("1000 1", Some(-30610224000000)),
    ("99", Some(915148800000)),
    ("99 Jan", Some(915148800000)),
    ("Jan 99", Some(915148800000)),
    ("1 Jan 1", Some(978307200000)),
    (
        "Thu Jan 01 1970 00:00:00 GMT+0000 (Coordinated Universal Time)",
        Some(0),
    ),
    ("\u{9}2020-01-01T00:00:00Z", None),
    ("Jan 1 2000", Some(946684800000)),
    ("Jan　1 2000", Some(946684800000)),
    ("Jan﻿1 2000", Some(946684800000)),
    ("Jän 1 2000", None),
    ("Jan 1 2000 ä", None),
    ("Jan 1, 2000, 10:00:00 AM", Some(946720800000)),
    ("Jan 1 2000 noon", None),
    ("2000-02-29", Some(951782400000)),
    ("1900-02-29", Some(-2203891200000)),
    ("1900-02-29T00:00:00Z", Some(-2203891200000)),
    ("Fri, 31 Dec 1999 23:59:59 GMT", Some(946684799000)),
    ("Fri 31 Dec 1999 23:59:59 -1200", Some(946727999000)),
    ("Fri 31 Dec 1999 23:59:59 +1400", Some(946634399000)),
    ("1999-12-31T23:59:59.999999Z", Some(946684799999)),
    ("2000-01-01T00:00:00,5Z", None),
    ("2000-W01-1", None),
    ("2000-001", Some(946684800000)),
    ("T10:00", None),
    ("10:00", None),
    ("10:00 Jan 1 2000", Some(946720800000)),
    ("Jan 1 10:00 2000", Some(946720800000)),
    ("2000 Jan 1 10:00", Some(946720800000)),
    ("Jan 2000 1", Some(946684800000)),
    ("1 2000 Jan", Some(946684800000)),
    ("Mon", None),
    ("2000-01-01T00:00:00.000Z ", None),
    ("2000-01-01T00:00:00.000Zx", None),
    ("2000-01-01T00:00:00.Z", None),
    ("2000-01-01T00:00:.5Z", None),
    ("2000-1-1T00:00:00Z", None),
    ("2000-01-1", Some(946684800000)),
    ("02000-01-01", Some(946684800000)),
    ("+2000-01-01", Some(946684800000)),
    ("-2000-01-01", Some(946684800000)),
    ("2000-01-01T1:00", None),
];

const ISO_STRING: &[(i64, Option<&str>)] = &[
    (0, Some("1970-01-01T00:00:00.000Z")),
    (1, Some("1970-01-01T00:00:00.001Z")),
    (-1, Some("1969-12-31T23:59:59.999Z")),
    (999, Some("1970-01-01T00:00:00.999Z")),
    (-999, Some("1969-12-31T23:59:59.001Z")),
    (1000, Some("1970-01-01T00:00:01.000Z")),
    (-1000, Some("1969-12-31T23:59:59.000Z")),
    (86399999, Some("1970-01-01T23:59:59.999Z")),
    (-86400000, Some("1969-12-31T00:00:00.000Z")),
    (951782400000, Some("2000-02-29T00:00:00.000Z")),
    (1759752000000, Some("2025-10-06T12:00:00.000Z")),
    (253402300799999, Some("9999-12-31T23:59:59.999Z")),
    (253402300800000, Some("+010000-01-01T00:00:00.000Z")),
    (-62167219200000, Some("0000-01-01T00:00:00.000Z")),
    (-62167219200001, Some("-000001-12-31T23:59:59.999Z")),
    (-62198755200000, Some("-000001-01-01T00:00:00.000Z")),
    (8640000000000000, Some("+275760-09-13T00:00:00.000Z")),
    (-8640000000000000, Some("-271821-04-20T00:00:00.000Z")),
    (8640000000000001, None),
    (-8640000000000001, None),
    (1000000000000000, Some("+033658-09-27T01:46:40.000Z")),
    (-1000000000000000, Some("-029719-04-05T22:13:20.000Z")),
    (-12345678901234, Some("1578-10-13T04:44:58.766Z")),
    (4102444800000, Some("2100-01-01T00:00:00.000Z")),
    (946684799999, Some("1999-12-31T23:59:59.999Z")),
];
type DateVectors = &'static [(&'static str, Option<i64>)];

const DATE_PARSE_LOCAL: &[(&str, DateVectors)] = &[
    (
        "America/New_York",
        &[
            ("2020-06-01T12:00", Some(1591027200000)),
            ("2020-06-01T12:00:00.500", Some(1591027200500)),
            ("2020-01-01T00:00:00", Some(1577854800000)),
            ("Jan 1 2000", Some(946702800000)),
            ("Jan 1 2000 10:00", Some(946738800000)),
            ("1/5/2020 10:00 pm", Some(1578279600000)),
            ("2020-03-08T01:59:59", Some(1583650799000)),
            ("2020-03-08T02:00", Some(1583650800000)),
            ("2020-03-08T02:30", Some(1583652600000)),
            ("2020-03-08T03:00", Some(1583650800000)),
            ("2020-11-01T00:59", Some(1604206740000)),
            ("2020-11-01T01:00", Some(1604206800000)),
            ("2020-11-01T01:30", Some(1604208600000)),
            ("2020-11-01T01:59:59", Some(1604210399000)),
            ("2020-11-01T02:00", Some(1604214000000)),
            ("Mar 8 2020 2:30", Some(1583652600000)),
            ("Nov 1 2020 1:30", Some(1604208600000)),
            ("1800-01-01T00:00", Some(-5364644638000)),
            ("1883-11-18T12:03:57", Some(-2717650801000)),
            ("1883-11-18T12:04", Some(-2717650560000)),
            ("1960-06-01T00:00", Some(-302472000000)),
            ("2100-07-04T00:00", Some(4118356800000)),
            ("-001000-01-01T00:00", Some(-93724110238000)),
            ("275760-09-12T00:00", None),
            ("Mon, 06 Oct 2025 12:00:00", Some(1759766400000)),
            ("2020-06-01", Some(1590969600000)),
            ("Jun 1 2020", Some(1590984000000)),
            ("1970-01-01T00:00", Some(18000000)),
            ("+275760-09-12T00:00", Some(8639999928000000)),
            ("+275760-09-13T00:00", None),
            ("-271821-04-20T00:00", Some(-8639999982238000)),
            ("-271821-04-21T00:00", Some(-8639999895838000)),
            ("-271821-04-19T23:00", Some(-8639999985838000)),
            ("2020-03-29T02:30", Some(1585463400000)),
            ("2020-10-25T02:30", Some(1603607400000)),
            ("2020-03-29T01:59", Some(1585461540000)),
            ("2020-10-25T03:00", Some(1603609200000)),
            ("Oct 25 2020 2:30 am", Some(1603607400000)),
            ("1900-01-01T00:00", Some(-2208970800000)),
        ],
    ),
    (
        "Europe/Berlin",
        &[
            ("2020-06-01T12:00", Some(1591005600000)),
            ("2020-06-01T12:00:00.500", Some(1591005600500)),
            ("2020-01-01T00:00:00", Some(1577833200000)),
            ("Jan 1 2000", Some(946681200000)),
            ("Jan 1 2000 10:00", Some(946717200000)),
            ("1/5/2020 10:00 pm", Some(1578258000000)),
            ("2020-03-08T01:59:59", Some(1583629199000)),
            ("2020-03-08T02:00", Some(1583629200000)),
            ("2020-03-08T02:30", Some(1583631000000)),
            ("2020-03-08T03:00", Some(1583632800000)),
            ("2020-11-01T00:59", Some(1604188740000)),
            ("2020-11-01T01:00", Some(1604188800000)),
            ("2020-11-01T01:30", Some(1604190600000)),
            ("2020-11-01T01:59:59", Some(1604192399000)),
            ("2020-11-01T02:00", Some(1604192400000)),
            ("Mar 8 2020 2:30", Some(1583631000000)),
            ("Nov 1 2020 1:30", Some(1604190600000)),
            ("1800-01-01T00:00", Some(-5364665608000)),
            ("1883-11-18T12:03:57", Some(-2717671771000)),
            ("1883-11-18T12:04", Some(-2717671768000)),
            ("1960-06-01T00:00", Some(-302490000000)),
            ("2100-07-04T00:00", Some(4118335200000)),
            ("-001000-01-01T00:00", Some(-93724131208000)),
            ("275760-09-12T00:00", None),
            ("Mon, 06 Oct 2025 12:00:00", Some(1759744800000)),
            ("2020-06-01", Some(1590969600000)),
            ("Jun 1 2020", Some(1590962400000)),
            ("1970-01-01T00:00", Some(-3600000)),
            ("+275760-09-12T00:00", Some(8639999906400000)),
            ("+275760-09-13T00:00", Some(8639999992800000)),
            ("-271821-04-20T00:00", None),
            ("-271821-04-21T00:00", Some(-8639999916808000)),
            ("-271821-04-19T23:00", None),
            ("2020-03-29T02:30", Some(1585445400000)),
            ("2020-10-25T02:30", Some(1603585800000)),
            ("2020-03-29T01:59", Some(1585443540000)),
            ("2020-10-25T03:00", Some(1603591200000)),
            ("Oct 25 2020 2:30 am", Some(1603585800000)),
            ("1900-01-01T00:00", Some(-2208992400000)),
        ],
    ),
    (
        "Asia/Tokyo",
        &[
            ("2020-06-01T12:00", Some(1590980400000)),
            ("2020-06-01T12:00:00.500", Some(1590980400500)),
            ("2020-01-01T00:00:00", Some(1577804400000)),
            ("Jan 1 2000", Some(946652400000)),
            ("Jan 1 2000 10:00", Some(946688400000)),
            ("1/5/2020 10:00 pm", Some(1578229200000)),
            ("2020-03-08T01:59:59", Some(1583600399000)),
            ("2020-03-08T02:00", Some(1583600400000)),
            ("2020-03-08T02:30", Some(1583602200000)),
            ("2020-03-08T03:00", Some(1583604000000)),
            ("2020-11-01T00:59", Some(1604159940000)),
            ("2020-11-01T01:00", Some(1604160000000)),
            ("2020-11-01T01:30", Some(1604161800000)),
            ("2020-11-01T01:59:59", Some(1604163599000)),
            ("2020-11-01T02:00", Some(1604163600000)),
            ("Mar 8 2020 2:30", Some(1583602200000)),
            ("Nov 1 2020 1:30", Some(1604161800000)),
            ("1800-01-01T00:00", Some(-5364695939000)),
            ("1883-11-18T12:03:57", Some(-2717702102000)),
            ("1883-11-18T12:04", Some(-2717702099000)),
            ("1960-06-01T00:00", Some(-302518800000)),
            ("2100-07-04T00:00", Some(4118310000000)),
            ("-001000-01-01T00:00", Some(-93724161539000)),
            ("275760-09-12T00:00", None),
            ("Mon, 06 Oct 2025 12:00:00", Some(1759719600000)),
            ("2020-06-01", Some(1590969600000)),
            ("Jun 1 2020", Some(1590937200000)),
            ("1970-01-01T00:00", Some(-32400000)),
            ("+275760-09-12T00:00", Some(8639999881200000)),
            ("+275760-09-13T00:00", Some(8639999967600000)),
            ("-271821-04-20T00:00", None),
            ("-271821-04-21T00:00", Some(-8639999947139000)),
            ("-271821-04-19T23:00", None),
            ("2020-03-29T02:30", Some(1585416600000)),
            ("2020-10-25T02:30", Some(1603560600000)),
            ("2020-03-29T01:59", Some(1585414740000)),
            ("2020-10-25T03:00", Some(1603562400000)),
            ("Oct 25 2020 2:30 am", Some(1603560600000)),
            ("1900-01-01T00:00", Some(-2209021200000)),
        ],
    ),
];
