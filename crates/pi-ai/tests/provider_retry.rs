//! Port of packages/ai/test/provider-retry.test.ts (unit ai-utils-errors)

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use pi_ai::utils::provider_retry::{
    ProviderHttpError, ProviderRetryOptions, retry_provider_request, retry_provider_request_with_no_retry_statuses,
};
use pi_js::abort::AbortController;
use pi_js::fetch::Headers;

fn provider_error(status: Option<u16>, pairs: &[(&str, &str)]) -> pi_js::Error {
    let headers = (!pairs.is_empty()).then(|| Headers::from_pairs(pairs.iter().copied()));
    ProviderHttpError::new(
        status,
        headers,
        format!(
            "Provider error: {}",
            status.map_or("unknown".to_string(), |s| s.to_string())
        ),
    )
    .into_js_error()
}

#[tokio::test(start_paused = true)]
async fn retries_retryable_provider_errors() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);

    let handle = tokio::spawn(async move {
        retry_provider_request(
            move || {
                let c = Arc::clone(&c);
                async move {
                    let n = c.fetch_add(1, Ordering::SeqCst);
                    if n == 0 {
                        Err(provider_error(Some(429), &[("retry-after-ms", "1000")]))
                    } else {
                        Ok("ok".to_string())
                    }
                }
            },
            ProviderRetryOptions {
                max_retries: Some(1),
                ..Default::default()
            },
        )
        .await
    });

    tokio::task::yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    tokio::time::advance(Duration::from_millis(999)).await;
    tokio::task::yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;

    assert_eq!(handle.await.unwrap().unwrap(), "ok");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn does_not_retry_when_provider_marks_non_retryable() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);

    let err = retry_provider_request::<String, _, _>(
        move || {
            c.fetch_add(1, Ordering::SeqCst);
            async { Err(provider_error(Some(429), &[("x-should-retry", "false")])) }
        },
        ProviderRetryOptions {
            max_retries: Some(2),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();

    assert!(err.message().contains("429"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn rejects_provider_requested_retry_delay_above_limit() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);

    let err = retry_provider_request::<String, _, _>(
        move || {
            c.fetch_add(1, Ordering::SeqCst);
            async { Err(provider_error(Some(429), &[("retry-after", "277403")])) }
        },
        ProviderRetryOptions {
            max_retries: Some(1),
            max_retry_delay_ms: Some(1000),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();

    assert!(err.message().contains("Server requested 277403s retry delay (max: 1s)"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn allows_disabling_provider_requested_retry_delay_cap() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);

    let handle = tokio::spawn(async move {
        retry_provider_request(
            move || {
                let c = Arc::clone(&c);
                async move {
                    let n = c.fetch_add(1, Ordering::SeqCst);
                    if n == 0 {
                        Err(provider_error(Some(429), &[("retry-after", "2")]))
                    } else {
                        Ok("ok".to_string())
                    }
                }
            },
            ProviderRetryOptions {
                max_retries: Some(1),
                max_retry_delay_ms: Some(0),
                ..Default::default()
            },
        )
        .await
    });

    tokio::task::yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    tokio::time::advance(Duration::from_millis(1999)).await;
    tokio::task::yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;

    assert_eq!(handle.await.unwrap().unwrap(), "ok");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn aborts_provider_requested_retry_delay() {
    let ctrl = AbortController::new();
    let sig = ctrl.signal();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);

    let handle = tokio::spawn(async move {
        retry_provider_request::<String, _, _>(
            move || {
                c.fetch_add(1, Ordering::SeqCst);
                async { Err(provider_error(Some(429), &[("retry-after", "277403")])) }
            },
            ProviderRetryOptions {
                max_retries: Some(2),
                max_retry_delay_ms: Some(0),
                signal: Some(sig),
            },
        )
        .await
    });

    tokio::task::yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    ctrl.abort(None);
    let err = handle.await.unwrap().unwrap_err();
    assert!(err.is_abort());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn does_not_retry_statuses_listed_in_no_retry_statuses() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);

    let err = retry_provider_request_with_no_retry_statuses::<String, _, _>(
        move || {
            c.fetch_add(1, Ordering::SeqCst);
            async { Err(provider_error(Some(504), &[("retry-after-ms", "0")])) }
        },
        ProviderRetryOptions {
            max_retries: Some(2),
            ..Default::default()
        },
        Some(&[504]),
    )
    .await
    .unwrap_err();

    assert!(err.message().contains("504"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
