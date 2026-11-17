//! Port of packages/coding-agent/src/core/http-dispatcher.ts

use std::time::Duration;

use pi_ai::utils::node_http_proxy::resolve_http_proxy_url_for_target;
use pi_js::{Error, Result};
use serde_json::Value;

pub const DEFAULT_HTTP_IDLE_TIMEOUT_MS: u64 = 300_000;
const DEFAULT_AUTO_SELECT_FAMILY_ATTEMPT_TIMEOUT_MS: u64 = 2_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HttpIdleTimeoutChoice {
    pub label: &'static str,
    pub timeout_ms: u64,
}

pub const HTTP_IDLE_TIMEOUT_CHOICES: &[HttpIdleTimeoutChoice] = &[
    HttpIdleTimeoutChoice {
        label: "30 sec",
        timeout_ms: 30_000,
    },
    HttpIdleTimeoutChoice {
        label: "1 min",
        timeout_ms: 60_000,
    },
    HttpIdleTimeoutChoice {
        label: "2 min",
        timeout_ms: 120_000,
    },
    HttpIdleTimeoutChoice {
        label: "5 min",
        timeout_ms: 300_000,
    },
    HttpIdleTimeoutChoice {
        label: "disabled",
        timeout_ms: 0,
    },
];

pub fn parse_http_idle_timeout_ms(value: &Value) -> Option<u64> {
    match value {
        Value::String(s) => {
            let trimmed = pi_js::str16::trim(s);
            if trimmed.eq_ignore_ascii_case("disabled") {
                return Some(0);
            }
            if trimmed.is_empty() {
                return None;
            }
            let num = pi_js::num::number(trimmed);
            if !num.is_finite() || num < 0.0 {
                None
            } else {
                Some(num.floor() as u64)
            }
        }
        Value::Number(n) => {
            let v = n.as_f64()?;
            if !v.is_finite() || v < 0.0 {
                None
            } else {
                Some(v.floor() as u64)
            }
        }
        _ => None,
    }
}

pub fn format_http_idle_timeout_ms(timeout_ms: u64) -> String {
    if let Some(choice) = HTTP_IDLE_TIMEOUT_CHOICES.iter().find(|c| c.timeout_ms == timeout_ms) {
        return choice.label.to_owned();
    }
    let secs = (timeout_ms as f64) / 1000.0;
    format!("{} sec", pi_js::json::number_to_string(secs))
}

pub fn apply_http_proxy_settings(http_proxy: Option<&str>) {
    let Some(proxy) = http_proxy.map(pi_js::str16::trim).filter(|s| !s.is_empty()) else {
        return;
    };
    if pi_js::env::var("HTTP_PROXY").is_none() {
        pi_js::env::set_var("HTTP_PROXY", proxy);
    }
    if pi_js::env::var("HTTPS_PROXY").is_none() {
        pi_js::env::set_var("HTTPS_PROXY", proxy);
    }
}

pub fn configure_http_dispatcher(timeout_ms: Option<u64>) -> Result<()> {
    let raw_ms = timeout_ms.unwrap_or(DEFAULT_HTTP_IDLE_TIMEOUT_MS);
    let _connect_hint = DEFAULT_AUTO_SELECT_FAMILY_ATTEMPT_TIMEOUT_MS;
    let mut builder = pi_js::fetch::client_builder().user_agent("undici");
    if raw_ms > 0 {
        builder = builder.read_timeout(Duration::from_millis(raw_ms));
    } else {
        builder = builder.read_timeout(Duration::from_secs(365 * 24 * 3600));
    }
    let proxy = reqwest::Proxy::custom(|target_url| {
        resolve_http_proxy_url_for_target(target_url.as_str(), None)
            .ok()
            .flatten()
    });
    builder = builder.proxy(proxy);
    let client = builder
        .build()
        .map_err(|err| Error::msg(format!("Failed to build HTTP client: {err}")))?;
    pi_js::fetch::set_default_client(client);
    Ok(())
}

pub fn configure_http_dispatcher_value(timeout: &Value) -> Result<()> {
    let Some(ms) = parse_http_idle_timeout_ms(timeout) else {
        let display = match timeout {
            Value::String(s) => s.clone(),
            other => pi_js::json::stringify(other),
        };
        return Err(Error::msg(format!("Invalid HTTP idle timeout: {display}")));
    };
    configure_http_dispatcher(Some(ms))
}
