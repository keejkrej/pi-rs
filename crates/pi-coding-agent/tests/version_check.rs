//! Port of packages/coding-agent/test/version-check.test.ts

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use pi_coding_agent::utils::version_check::{
    LatestPiRelease, VersionCheckOptions, check_for_new_pi_version, compare_package_versions,
    format_version_check_error, get_latest_pi_release, get_latest_pi_version, is_newer_package_version,
};
use pi_js::Error;
use pi_js::env::testing::EnvGuard;
use pi_js::fetch::Request;
use pi_js::fetch::testing::{mock_fetch, response};

mod support;

#[test]
fn compares_package_versions() {
    assert!(compare_package_versions("0.70.6", "0.70.5").is_some_and(|c| c > 0));
    assert_eq!(compare_package_versions("0.70.5", "0.70.5"), Some(0));
    assert!(compare_package_versions("0.70.4", "0.70.5").is_some_and(|c| c < 0));
    assert!(compare_package_versions("5.0.0-beta.20", "5.0.0-beta.9").is_some_and(|c| c > 0));
    assert!(!is_newer_package_version("0.70.5", "0.70.5"));
    assert!(is_newer_package_version("0.70.6", "0.70.5"));
}

#[tokio::test(flavor = "current_thread")]
async fn returns_only_newer_versions() {
    let _net = support::test_network_env::allow_network();
    let _skip = EnvGuard::set("PI_SKIP_VERSION_CHECK", None);
    let _guard = mock_fetch(|_req| {
        Box::pin(async {
            Ok(response(
                200,
                &[("content-type", "application/json")],
                r#"{"version":"1.2.3"}"#,
            ))
        })
    });

    assert_eq!(check_for_new_pi_version("1.2.3").await, None);
    assert_eq!(
        check_for_new_pi_version("1.2.2").await,
        Some(LatestPiRelease {
            version: "1.2.3".to_owned(),
            package_name: None,
            note: None,
        })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn uses_pi_dev_version_check_api_with_pi_user_agent() {
    let _net = support::test_network_env::allow_network();
    let seen_req: Arc<Mutex<Option<(String, Option<String>, Option<String>)>>> = Arc::new(Mutex::new(None));
    let seen_ref = seen_req.clone();
    let _guard = mock_fetch(move |req: Request| {
        let seen = seen_ref.clone();
        Box::pin(async move {
            let ua = req.headers.get("User-Agent");
            let accept = req.headers.get("accept");
            *seen.lock().unwrap_or_else(PoisonError::into_inner) = Some((req.url.clone(), ua, accept));
            Ok(response(
                200,
                &[("content-type", "application/json")],
                r#"{"version":"1.2.4"}"#,
            ))
        })
    });

    let ver = get_latest_pi_version("1.2.3", VersionCheckOptions::default())
        .await
        .expect("get_latest_pi_version ok");
    assert_eq!(ver.as_deref(), Some("1.2.4"));

    let (url, ua, accept) = seen_req
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .expect("request recorded");
    assert_eq!(url, "https://pi.dev/api/latest-version");
    let ua = ua.expect("User-Agent header set");
    assert!(ua.starts_with("pi/1.2.3 "), "unexpected User-Agent: {ua}");
    assert_eq!(accept.as_deref(), Some("application/json"));
}

#[tokio::test(flavor = "current_thread")]
async fn retries_transient_version_request_when_explicitly_requested() {
    let _net = support::test_network_env::allow_network();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = calls.clone();
    let _guard = mock_fetch(move |_req| {
        let calls = calls_ref.clone();
        Box::pin(async move {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                Err(Error::msg("fetch failed"))
            } else {
                Ok(response(
                    200,
                    &[("content-type", "application/json")],
                    r#"{"version":"1.2.4"}"#,
                ))
            }
        })
    });

    let release = get_latest_pi_release(
        "1.2.3",
        VersionCheckOptions {
            retry: true,
            ..Default::default()
        },
    )
    .await
    .expect("retries into release");
    assert_eq!(
        release,
        Some(LatestPiRelease {
            version: "1.2.4".to_owned(),
            package_name: None,
            note: None,
        })
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test(flavor = "current_thread")]
async fn keeps_automatic_version_checks_to_one_request() {
    let _net = support::test_network_env::allow_network();
    let _skip = EnvGuard::set("PI_SKIP_VERSION_CHECK", None);
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = calls.clone();
    let _guard = mock_fetch(move |_req| {
        let calls = calls_ref.clone();
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(Error::msg("fetch failed"))
        })
    });

    assert_eq!(check_for_new_pi_version("1.2.3").await, None);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn formats_nested_network_error_details() {
    // PORT: `Error` in pi_js models single `cause` chains rather than Node's `AggregateError` codes,
    // and `format_version_check_error` formats `"<root> (cause: <next>)"`.
    let inner = Error::js_with_code("Error", "connect timeout", "ETIMEDOUT");
    let err = Error::js_with_cause("Error", "fetch failed", inner);

    assert_eq!(
        format_version_check_error(&err),
        "fetch failed (cause: connect timeout)"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn returns_active_package_metadata_from_version_check_api() {
    let _net = support::test_network_env::allow_network();
    let _guard = mock_fetch(|_req| {
        Box::pin(async {
            Ok(response(
                200,
                &[("content-type", "application/json")],
                r#"{"packageName":"@new-scope/pi","version":"1.2.4"}"#,
            ))
        })
    });

    let release = get_latest_pi_release("1.2.3", VersionCheckOptions::default())
        .await
        .expect("release");
    assert_eq!(
        release,
        Some(LatestPiRelease {
            version: "1.2.4".to_owned(),
            package_name: Some("@new-scope/pi".to_owned()),
            note: None,
        })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn returns_update_notes_from_version_check_api() {
    let _net = support::test_network_env::allow_network();
    let _guard = mock_fetch(|_req| {
        Box::pin(async {
            Ok(response(
                200,
                &[("content-type", "application/json")],
                r#"{"note":" **Read this** ","version":"1.2.4"}"#,
            ))
        })
    });

    let release = get_latest_pi_release("1.2.3", VersionCheckOptions::default())
        .await
        .expect("release");
    assert_eq!(
        release,
        Some(LatestPiRelease {
            version: "1.2.4".to_owned(),
            package_name: None,
            note: Some("**Read this**".to_owned()),
        })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn skips_automatic_api_calls_when_version_checks_are_disabled() {
    let _net = support::test_network_env::allow_network();
    let _skip = EnvGuard::set("PI_SKIP_VERSION_CHECK", Some("1"));
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = calls.clone();
    let _guard = mock_fetch(move |_req| {
        let calls = calls_ref.clone();
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(response(200, &[], "{}"))
        })
    });

    assert_eq!(check_for_new_pi_version("1.2.3").await, None);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn allows_direct_api_calls_when_automatic_version_checks_are_disabled() {
    let _net = support::test_network_env::allow_network();
    let _skip = EnvGuard::set("PI_SKIP_VERSION_CHECK", Some("1"));
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = calls.clone();
    let _guard = mock_fetch(move |_req| {
        let calls = calls_ref.clone();
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(response(
                200,
                &[("content-type", "application/json")],
                r#"{"version":"1.2.4"}"#,
            ))
        })
    });

    assert_eq!(
        get_latest_pi_version("1.2.3", VersionCheckOptions::default())
            .await
            .expect("ok")
            .as_deref(),
        Some("1.2.4")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
