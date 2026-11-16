//! Port of packages/ai/test/oauth-device-code.test.ts

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_ai::auth::oauth::device_code::{OAuthDeviceCodePollResult, poll_oauth_device_code_flow};
use pi_js::Error;
use pi_js::abort::AbortController;
use pi_js::time;
use pi_js::time::testing::{clear_system_time, parse_date_utc, set_system_time};

struct FakeTimeGuard;

impl FakeTimeGuard {
    fn set(ms: i64) -> Self {
        set_system_time(ms);
        Self
    }
}

impl Drop for FakeTimeGuard {
    fn drop(&mut self) {
        clear_system_time();
    }
}

#[tokio::test(start_paused = true)]
async fn polls_immediately_and_returns_the_completed_value() {
    let start_ms = parse_date_utc("2026-03-09T00:00:00Z").unwrap();
    let _clock = FakeTimeGuard::set(start_ms);

    let poll_times = Arc::new(Mutex::new(Vec::new()));
    let poll_times_c = poll_times.clone();
    let sig = AbortController::new().signal();

    let task = tokio::spawn(async move {
        poll_oauth_device_code_flow(Some(2.0), Some(30.0), false, &sig, move || {
            let pt = poll_times_c.clone();
            async move {
                let mut g = pt.lock().unwrap();
                g.push(time::now_ms());
                if g.len() == 1 {
                    Ok(OAuthDeviceCodePollResult::Pending)
                } else {
                    Ok(OAuthDeviceCodePollResult::Complete {
                        value: "token".to_owned(),
                    })
                }
            }
        })
        .await
    });

    tokio::task::yield_now().await;
    assert_eq!(*poll_times.lock().unwrap(), vec![start_ms]);

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(1999)).await;
    tokio::task::yield_now().await;
    assert_eq!(*poll_times.lock().unwrap(), vec![start_ms]);

    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(task.await.unwrap().unwrap(), "token");
    assert_eq!(
        *poll_times.lock().unwrap(),
        vec![start_ms, parse_date_utc("2026-03-09T00:00:02Z").unwrap()]
    );
}

#[tokio::test(start_paused = true)]
async fn can_wait_before_the_first_poll() {
    let start_ms = parse_date_utc("2026-03-09T00:00:00Z").unwrap();
    let _clock = FakeTimeGuard::set(start_ms);

    let poll_times = Arc::new(Mutex::new(Vec::new()));
    let poll_times_c = poll_times.clone();
    let sig = AbortController::new().signal();

    let task = tokio::spawn(async move {
        poll_oauth_device_code_flow(Some(2.0), Some(30.0), true, &sig, move || {
            let pt = poll_times_c.clone();
            async move {
                pt.lock().unwrap().push(time::now_ms());
                Ok(OAuthDeviceCodePollResult::Complete {
                    value: "token".to_owned(),
                })
            }
        })
        .await
    });

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(1999)).await;
    tokio::task::yield_now().await;
    assert!(poll_times.lock().unwrap().is_empty());

    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(task.await.unwrap().unwrap(), "token");
    assert_eq!(
        *poll_times.lock().unwrap(),
        vec![parse_date_utc("2026-03-09T00:00:02Z").unwrap()]
    );
}

#[tokio::test(start_paused = true)]
async fn increases_the_interval_by_5_seconds_after_slow_down_without_a_server_interval() {
    let start_ms = parse_date_utc("2026-03-09T00:00:00Z").unwrap();
    let _clock = FakeTimeGuard::set(start_ms);

    let poll_times = Arc::new(Mutex::new(Vec::new()));
    let results = Arc::new(Mutex::new(VecDeque::from([
        OAuthDeviceCodePollResult::SlowDown { interval_seconds: None },
        OAuthDeviceCodePollResult::Complete {
            value: "token".to_owned(),
        },
    ])));
    let pt_c = poll_times.clone();
    let res_c = results.clone();
    let sig = AbortController::new().signal();

    let task = tokio::spawn(async move {
        poll_oauth_device_code_flow(Some(2.0), Some(900.0), false, &sig, move || {
            let pt = pt_c.clone();
            let res = res_c.clone();
            async move {
                pt.lock().unwrap().push(time::now_ms());
                res.lock()
                    .unwrap()
                    .pop_front()
                    .ok_or_else(|| Error::msg("Unexpected extra poll"))
            }
        })
        .await
    });

    tokio::task::yield_now().await;
    assert_eq!(*poll_times.lock().unwrap(), vec![start_ms]);

    tokio::time::advance(Duration::from_millis(6999)).await;
    tokio::task::yield_now().await;
    assert_eq!(*poll_times.lock().unwrap(), vec![start_ms]);

    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(task.await.unwrap().unwrap(), "token");
    assert_eq!(*poll_times.lock().unwrap(), vec![start_ms, start_ms + 7000]);
}

#[tokio::test(start_paused = true)]
async fn honors_a_server_provided_slow_down_interval() {
    let start_ms = parse_date_utc("2026-03-09T00:00:00Z").unwrap();
    let _clock = FakeTimeGuard::set(start_ms);

    let poll_times = Arc::new(Mutex::new(Vec::new()));
    let results = Arc::new(Mutex::new(VecDeque::from([
        OAuthDeviceCodePollResult::SlowDown {
            interval_seconds: Some(30.0),
        },
        OAuthDeviceCodePollResult::Complete {
            value: "token".to_owned(),
        },
    ])));
    let pt_c = poll_times.clone();
    let res_c = results.clone();
    let sig = AbortController::new().signal();

    let task = tokio::spawn(async move {
        poll_oauth_device_code_flow(Some(2.0), Some(900.0), false, &sig, move || {
            let pt = pt_c.clone();
            let res = res_c.clone();
            async move {
                pt.lock().unwrap().push(time::now_ms());
                res.lock()
                    .unwrap()
                    .pop_front()
                    .ok_or_else(|| Error::msg("Unexpected extra poll"))
            }
        })
        .await
    });

    tokio::task::yield_now().await;
    assert_eq!(*poll_times.lock().unwrap(), vec![start_ms]);

    tokio::time::advance(Duration::from_millis(29999)).await;
    tokio::task::yield_now().await;
    assert_eq!(*poll_times.lock().unwrap(), vec![start_ms]);

    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(task.await.unwrap().unwrap(), "token");
    assert_eq!(*poll_times.lock().unwrap(), vec![start_ms, start_ms + 30_000]);
}

#[tokio::test(start_paused = true)]
async fn cancels_an_in_flight_wait() {
    let controller = AbortController::new();
    let sig = controller.signal();

    let task = tokio::spawn(async move {
        poll_oauth_device_code_flow(Some(5.0), Some(30.0), false, &sig, || async {
            Ok::<_, Error>(OAuthDeviceCodePollResult::<String>::Pending)
        })
        .await
    });

    tokio::task::yield_now().await;
    controller.abort(None);
    let err = task.await.unwrap().unwrap_err();
    assert!(err.to_string().contains("Login cancelled"));
}
