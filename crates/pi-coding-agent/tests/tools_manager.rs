//! Port of packages/coding-agent/test/tools-manager.test.ts

use std::sync::{Arc, Mutex, PoisonError};

use pi_coding_agent::config::ENV_AGENT_DIR;
use pi_coding_agent::utils::tools_manager::{ManagedTool, ToolStatus, ToolStatusKind, ensure_tool, get_latest_version};
use pi_js::Error;
use pi_js::env::testing::EnvGuard;
use pi_js::fetch::testing::{mock_fetch, response};
use tempfile::TempDir;

fn redirect_response(location: &str) -> pi_js::fetch::Response {
    response(302, &[("location", location)], "")
}

mod get_latest_version_tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn resolves_the_version_from_the_release_page_redirect() {
        let seen_urls = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen_ref = seen_urls.clone();
        let _guard = mock_fetch(move |req| {
            let seen = seen_ref.clone();
            Box::pin(async move {
                seen.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(req.url.clone());
                Ok(redirect_response("https://github.com/sharkdp/fd/releases/tag/v10.4.2"))
            })
        });

        let version = get_latest_version("sharkdp/fd").await.expect("get_latest_version");
        assert_eq!(version, "10.4.2");
        assert_eq!(
            seen_urls.lock().unwrap_or_else(PoisonError::into_inner).as_slice(),
            ["https://github.com/sharkdp/fd/releases/latest"]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn keeps_tags_without_a_v_prefix_intact() {
        let _guard = mock_fetch(|_req| {
            Box::pin(async {
                Ok(redirect_response(
                    "https://github.com/BurntSushi/ripgrep/releases/tag/15.2.0",
                ))
            })
        });

        assert_eq!(get_latest_version("BurntSushi/ripgrep").await.expect("ok"), "15.2.0");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolves_relative_redirect_targets() {
        let _guard = mock_fetch(|_req| Box::pin(async { Ok(redirect_response("/sharkdp/fd/releases/tag/v10.4.2")) }));

        assert_eq!(get_latest_version("sharkdp/fd").await.expect("ok"), "10.4.2");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolves_redirect_even_with_non_empty_response_body() {
        // PORT: TS asserted `response.bodyUsed === true` after cancelling the 302 body; in Rust
        // `get_latest_version` drops the owned `Response` directly on return, closing any stream.
        let _guard = mock_fetch(|_req| {
            Box::pin(async {
                Ok(response(
                    302,
                    &[("location", "https://github.com/sharkdp/fd/releases/tag/v10.4.2")],
                    "<html></html>",
                ))
            })
        });

        assert_eq!(get_latest_version("sharkdp/fd").await.expect("ok"), "10.4.2");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fails_clearly_when_the_endpoint_does_not_redirect() {
        let _guard = mock_fetch(|_req| Box::pin(async { Ok(response(404, &[], "not found")) }));

        let err = get_latest_version("sharkdp/fd").await.expect_err("expected error");
        assert!(
            err.to_string()
                .contains("Failed to resolve latest sharkdp/fd release: HTTP 404 without redirect"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fails_clearly_when_the_redirect_does_not_point_at_a_release_tag() {
        let _guard = mock_fetch(|_req| Box::pin(async { Ok(redirect_response("https://github.com/login")) }));

        let err = get_latest_version("sharkdp/fd").await.expect_err("expected error");
        assert!(
            err.to_string().contains(
                "Failed to resolve latest sharkdp/fd release: unexpected redirect to https://github.com/login"
            ),
            "unexpected error: {err}"
        );
    }
}

mod ensure_tool_tests {
    use super::*;

    fn isolate_missing_tools_env() -> (TempDir, EnvGuard, EnvGuard) {
        let dir = TempDir::new().expect("temp dir");
        let agent_dir = dir.path().join("agent");
        let empty_path = dir.path().join("empty-bin");
        std::fs::create_dir_all(&agent_dir).expect("mkdir agent");
        std::fs::create_dir_all(&empty_path).expect("mkdir empty-bin");
        let agent_guard = EnvGuard::set(&ENV_AGENT_DIR, Some(agent_dir.to_str().expect("utf8")));
        let path_guard = EnvGuard::set("PATH", Some(empty_path.to_str().expect("utf8")));
        (dir, agent_guard, path_guard)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reports_status_through_a_callback_without_writing_to_console() {
        let (_dir, _agent_g, _path_g) = isolate_missing_tools_env();
        let _offline = EnvGuard::set("PI_OFFLINE", Some("1"));
        let statuses: Arc<Mutex<Vec<ToolStatus>>> = Arc::new(Mutex::new(Vec::new()));
        let statuses_ref = statuses.clone();

        let result = ensure_tool(
            ManagedTool::Fd,
            Some(move |status| {
                statuses_ref.lock().unwrap_or_else(PoisonError::into_inner).push(status);
            }),
        )
        .await;

        assert_eq!(result, None);
        pretty_assertions::assert_eq!(
            *statuses.lock().unwrap_or_else(PoisonError::into_inner),
            vec![ToolStatus {
                r#type: ToolStatusKind::Warning,
                message: "fd not found. Offline mode enabled, skipping download.".to_owned(),
            }]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn surfaces_the_error_cause_chain_when_a_download_fails() {
        let (_dir, _agent_g, _path_g) = isolate_missing_tools_env();
        let _offline = EnvGuard::set("PI_OFFLINE", None);
        let _guard = mock_fetch(|_req| {
            Box::pin(async {
                Err(Error::js_with_cause(
                    "TypeError",
                    "fetch failed",
                    Error::msg("connect ETIMEDOUT 140.82.113.3:443"),
                ))
            })
        });

        let statuses: Arc<Mutex<Vec<ToolStatus>>> = Arc::new(Mutex::new(Vec::new()));
        let statuses_ref = statuses.clone();

        let result = ensure_tool(
            ManagedTool::Fd,
            Some(move |status| {
                statuses_ref.lock().unwrap_or_else(PoisonError::into_inner).push(status);
            }),
        )
        .await;

        assert_eq!(result, None);
        pretty_assertions::assert_eq!(
            *statuses.lock().unwrap_or_else(PoisonError::into_inner),
            vec![
                ToolStatus {
                    r#type: ToolStatusKind::Info,
                    message: "fd not found. Downloading...".to_owned(),
                },
                ToolStatus {
                    r#type: ToolStatusKind::Warning,
                    message: "Failed to download fd: fetch failed: connect ETIMEDOUT 140.82.113.3:443".to_owned(),
                },
            ]
        );
    }
}
