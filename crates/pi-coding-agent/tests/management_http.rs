//! Port of packages/coding-agent/test/management-http.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pi_coding_agent::utils::management_http::{FetchRetryOptions, fetch_with_retry};
use pi_js::Error;
use pi_js::abort::AbortController;
use pi_js::fetch::Request;
use pi_js::fetch::testing::{mock_fetch, response};

#[tokio::test(flavor = "current_thread")]
async fn retries_a_transient_transport_failure_twice_and_succeeds() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = calls.clone();
    let _guard = mock_fetch(move |_req| {
        let calls = calls_ref.clone();
        Box::pin(async move {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                Err(Error::js("TypeError", "fetch failed"))
            } else {
                Ok(response(200, &[("content-type", "application/json")], r#"{"ok":true}"#))
            }
        })
    });

    let res = fetch_with_retry(Request::get("https://example.test"), FetchRetryOptions::default())
        .await
        .expect("fetch succeeds on 3rd attempt");
    assert!(res.ok());
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test(flavor = "current_thread")]
async fn shares_timeout_budget_across_attempts() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = calls.clone();
    let _guard = mock_fetch(move |req| {
        let calls = calls_ref.clone();
        Box::pin(async move {
            assert!(req.signal.is_some());
            let n = calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Err(Error::js("TypeError", "fetch failed"))
            } else {
                Ok(response(200, &[("content-type", "application/json")], r#"{"ok":true}"#))
            }
        })
    });

    let opts = FetchRetryOptions {
        timeout_ms: Some(1_000),
        ..Default::default()
    };
    let res = fetch_with_retry(Request::get("https://example.test"), opts)
        .await
        .expect("fetch succeeds on retry within timeout budget");
    assert!(res.ok());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn retries_an_attempt_timeout() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = calls.clone();
    let _guard = mock_fetch(move |req| {
        let calls = calls_ref.clone();
        Box::pin(async move {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                if let Some(sig) = req.signal {
                    sig.cancelled().await;
                    sig.throw_if_aborted()?;
                }
                Err(Error::js("TimeoutError", "attempt timed out"))
            } else {
                Ok(response(200, &[("content-type", "application/json")], r#"{"ok":true}"#))
            }
        })
    });

    let opts = FetchRetryOptions {
        attempt_timeout_ms: Some(25),
        ..Default::default()
    };
    let res = fetch_with_retry(Request::get("https://example.test"), opts)
        .await
        .expect("retries after attempt timeout");
    assert!(res.ok());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn retries_transient_http_responses_and_returns_successful_response() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = calls.clone();
    let _guard = mock_fetch(move |_req| {
        let calls = calls_ref.clone();
        Box::pin(async move {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(response(503, &[], "busy"))
            } else {
                Ok(response(200, &[("content-type", "application/json")], r#"{"ok":true}"#))
            }
        })
    });

    let res = fetch_with_retry(Request::get("https://example.test"), FetchRetryOptions::default())
        .await
        .expect("503 retried into 200");
    assert!(res.ok());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn does_not_retry_caller_cancellation() {
    let controller = AbortController::new();
    controller.abort(None);

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = calls.clone();
    let _guard = mock_fetch(move |_req| {
        let calls = calls_ref.clone();
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(response(200, &[], "unexpected"))
        })
    });

    let req = Request::get("https://example.test").signal(controller.signal());
    let res = fetch_with_retry(req, FetchRetryOptions::default()).await;
    assert!(res.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
