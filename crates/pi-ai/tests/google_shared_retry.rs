//! Port of packages/ai/test/google-shared-retry.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pi_ai::api::google_shared::retry_google_request;
use pi_ai::utils::error_body::SdkErrorInfo;
use pi_ai::utils::provider_retry::ProviderRetryOptions;
use pi_js::Error;
use pretty_assertions::assert_eq;

/// Shaped like `@google/genai`'s `ApiError`: carries `status`, without response `headers`.
// PORT: Rust represents status-only SDK errors with `SdkErrorInfo`.
fn google_api_error(status: u16) -> Error {
    SdkErrorInfo {
        status: Some(status),
        message: format!("got status: {status}"),
        ..Default::default()
    }
    .into_js_error()
}

#[tokio::test(start_paused = true)]
async fn retries_a_headers_less_sdk_error_with_a_retryable_status() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&calls);

    let result = retry_google_request(
        move || {
            let count = calls_ref.fetch_add(1, Ordering::SeqCst);
            async move {
                if count == 0 {
                    Err(google_api_error(429))
                } else {
                    Ok("ok".to_owned())
                }
            }
        },
        ProviderRetryOptions {
            max_retries: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("retryable 429 should succeed on retry");

    assert_eq!(result, "ok");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn does_not_retry_when_max_retries_is_unset() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&calls);

    let err = retry_google_request(
        move || {
            calls_ref.fetch_add(1, Ordering::SeqCst);
            async move { Err::<String, _>(google_api_error(429)) }
        },
        ProviderRetryOptions::default(),
    )
    .await
    .expect_err("should fail without retries");

    assert_eq!(err.to_string(), "got status: 429");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn does_not_retry_a_non_retryable_status() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&calls);

    let err = retry_google_request(
        move || {
            calls_ref.fetch_add(1, Ordering::SeqCst);
            async move { Err::<String, _>(google_api_error(400)) }
        },
        ProviderRetryOptions {
            max_retries: Some(2),
            ..Default::default()
        },
    )
    .await
    .expect_err("400 should not retry");

    assert_eq!(err.to_string(), "got status: 400");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
