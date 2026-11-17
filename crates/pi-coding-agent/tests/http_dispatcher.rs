//! Port of packages/coding-agent/test/http-dispatcher.test.ts

use std::sync::{Arc, Mutex};

use pi_coding_agent::core::http_dispatcher::{
    DEFAULT_HTTP_IDLE_TIMEOUT_MS, HTTP_IDLE_TIMEOUT_CHOICES, apply_http_proxy_settings, configure_http_dispatcher,
    configure_http_dispatcher_value, format_http_idle_timeout_ms, parse_http_idle_timeout_ms,
};
use pi_js::env::testing::EnvGuard;
use pi_js::env::var as env_var;
use pi_js::fetch::{Request, fetch};
use serde_json::json;
use serial_test::serial;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct DispatcherResetGuard;

impl Drop for DispatcherResetGuard {
    fn drop(&mut self) {
        let _ = configure_http_dispatcher(None);
    }
}

#[test]
#[serial]
fn applies_http_proxy_without_overriding_existing_env_vars_or_accepting_empty_values() {
    let _g_http = EnvGuard::set("HTTP_PROXY", None);
    let _g_https = EnvGuard::set("HTTPS_PROXY", None);

    apply_http_proxy_settings(Some("http://127.0.0.1:7890"));
    assert_eq!(env_var("HTTP_PROXY").as_deref(), Some("http://127.0.0.1:7890"));
    assert_eq!(env_var("HTTPS_PROXY").as_deref(), Some("http://127.0.0.1:7890"));

    let _g_http_pre = EnvGuard::set("HTTP_PROXY", Some("http://env-http:8080"));
    let _g_https_pre = EnvGuard::set("HTTPS_PROXY", Some("http://env-https:8080"));
    apply_http_proxy_settings(Some("http://settings:7890"));
    assert_eq!(env_var("HTTP_PROXY").as_deref(), Some("http://env-http:8080"));
    assert_eq!(env_var("HTTPS_PROXY").as_deref(), Some("http://env-https:8080"));

    let _g_http_clr = EnvGuard::set("HTTP_PROXY", None);
    let _g_https_clr = EnvGuard::set("HTTPS_PROXY", None);
    apply_http_proxy_settings(Some("   "));
    assert_eq!(env_var("HTTP_PROXY"), None);
    assert_eq!(env_var("HTTPS_PROXY"), None);
}

#[test]
fn parses_and_formats_http_idle_timeout_choices() {
    assert_eq!(DEFAULT_HTTP_IDLE_TIMEOUT_MS, 300_000);
    assert_eq!(HTTP_IDLE_TIMEOUT_CHOICES.len(), 5);
    assert_eq!(parse_http_idle_timeout_ms(&json!("disabled")), Some(0));
    assert_eq!(parse_http_idle_timeout_ms(&json!(60_000)), Some(60_000));
    assert_eq!(parse_http_idle_timeout_ms(&json!("   ")), None);
    assert_eq!(parse_http_idle_timeout_ms(&json!(-5)), None);
    assert_eq!(format_http_idle_timeout_ms(0), "disabled");
    assert_eq!(format_http_idle_timeout_ms(120_000), "2 min");
    assert_eq!(format_http_idle_timeout_ms(45_000), "45 sec");
}

#[tokio::test]
#[serial]
async fn tunnels_and_routes_proxied_http_origins_through_configured_dispatcher() {
    let _dispatcher_guard = DispatcherResetGuard;
    let _g_no_proxy = EnvGuard::set_many(&[
        ("HTTP_PROXY", None),
        ("HTTPS_PROXY", None),
        ("http_proxy", None),
        ("https_proxy", None),
        ("NO_PROXY", None),
        ("no_proxy", None),
    ]);

    let origin_listener = TcpListener::bind("127.0.0.1:0").await.expect("bind origin");
    let origin_port = origin_listener.local_addr().unwrap().port();
    let origin_task = tokio::spawn(async move {
        while let Ok((mut stream, _)) = origin_listener.accept().await {
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf).await;
            let resp = b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\norigin";
            let _ = stream.write_all(resp).await;
        }
    });

    let proxy_request_lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let lines_ref = proxy_request_lines.clone();
    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.expect("bind proxy");
    let proxy_port = proxy_listener.local_addr().unwrap().port();
    let proxy_task = tokio::spawn(async move {
        while let Ok((mut client, _)) = proxy_listener.accept().await {
            let lines = lines_ref.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let Ok(n) = client.read(&mut buf).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                let raw = String::from_utf8_lossy(&buf[..n]).into_owned();
                let first_line = raw.lines().next().unwrap_or("").trim_end().to_owned();
                lines.lock().unwrap().push(first_line.clone());

                if first_line.starts_with("CONNECT ") {
                    if let Ok(mut upstream) = tokio::net::TcpStream::connect(("127.0.0.1", origin_port)).await {
                        let _ = client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await;
                        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                    }
                } else if let Ok(mut upstream) = tokio::net::TcpStream::connect(("127.0.0.1", origin_port)).await {
                    let _ = upstream.write_all(&buf[..n]).await;
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                }
            });
        }
    });

    let proxy_url = format!("http://127.0.0.1:{proxy_port}");
    let _g_proxy = EnvGuard::set("HTTP_PROXY", Some(&proxy_url));
    configure_http_dispatcher(None).expect("configure dispatcher");

    let origin_url = format!("http://127.0.0.1:{origin_port}/v1/chat/completions");
    for _ in 0..2 {
        let resp = fetch(Request::get(&origin_url)).await.expect("proxied fetch");
        assert_eq!(resp.text().await.unwrap(), "origin");
    }

    let seen = proxy_request_lines.lock().unwrap().clone();
    assert!(!seen.is_empty(), "expected requests to go through HTTP_PROXY");
    assert!(
        seen.iter()
            .any(|line| line.contains(&format!("127.0.0.1:{origin_port}"))),
        "unexpected proxy request lines: {seen:?}"
    );

    assert!(configure_http_dispatcher_value(&json!("disabled")).is_ok());
    assert!(configure_http_dispatcher_value(&json!("invalid")).is_err());

    proxy_task.abort();
    origin_task.abort();
}
