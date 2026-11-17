//! Port of packages/ai/src/utils/node-http-proxy.ts

use url::Url;

use crate::types::ProviderEnv;

pub const UNSUPPORTED_PROXY_PROTOCOL_MESSAGE: &str =
    "Unsupported proxy protocol. SOCKS and PAC proxy URLs are not supported; use an HTTP or HTTPS proxy URL.";

fn default_proxy_port(protocol: &str) -> u16 {
    match protocol {
        "ftp" => 21,
        "gopher" => 70,
        "http" => 80,
        "https" => 443,
        "ws" => 80,
        "wss" => 443,
        _ => 0,
    }
}

fn get_proxy_env(key: &str, env: Option<&ProviderEnv>) -> String {
    let lowercase_key = key.to_ascii_lowercase();
    let uppercase_key = key.to_ascii_uppercase();

    if let Some(e) = env {
        if let Some(val) = e.get(&lowercase_key).filter(|v| !v.is_empty()) {
            return val.clone();
        }
        if let Some(val) = e.get(&uppercase_key).filter(|v| !v.is_empty()) {
            return val.clone();
        }
    }

    if let Some(val) = pi_js::env::var(&lowercase_key).filter(|v| !v.is_empty()) {
        return val;
    }
    if let Some(val) = pi_js::env::var(&uppercase_key).filter(|v| !v.is_empty()) {
        return val;
    }
    String::new()
}

fn strip_brackets(host: &str) -> &str {
    host.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or(host)
}

struct NoProxyEntry {
    host: String,
    port: u16,
}

fn parse_no_proxy_entry(entry: &str) -> Option<NoProxyEntry> {
    let trimmed = entry.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(after_open) = trimmed.strip_prefix('[') {
        if let Some(close_idx) = after_open.find(']') {
            let host = after_open[..close_idx].to_string();
            let rest = &after_open[close_idx + 1..];
            if let Some(port_str) = rest.strip_prefix(':') {
                let port = port_str.parse::<u16>().unwrap_or(0);
                return Some(NoProxyEntry { host, port });
            }
            return Some(NoProxyEntry { host, port: 0 });
        }
    }

    if trimmed.matches(':').count() > 1 {
        return Some(NoProxyEntry { host: trimmed, port: 0 });
    }

    if let Some(colon_idx) = trimmed.rfind(':') {
        if trimmed.find(':') == Some(colon_idx) {
            let host = trimmed[..colon_idx].to_string();
            if let Ok(port) = trimmed[colon_idx + 1..].parse::<u16>() {
                return Some(NoProxyEntry { host, port });
            }
        }
    }

    Some(NoProxyEntry { host: trimmed, port: 0 })
}

fn should_proxy_hostname(hostname: &str, port: u16, env: Option<&ProviderEnv>) -> bool {
    let no_proxy = get_proxy_env("no_proxy", env).to_ascii_lowercase();
    if no_proxy.is_empty() {
        return true;
    }
    if no_proxy == "*" {
        return false;
    }

    let lower_target = hostname.to_ascii_lowercase();
    let normalized_target_host = strip_brackets(&lower_target);

    for entry in no_proxy.split(|c: char| c == ',' || c.is_ascii_whitespace()) {
        let Some(parsed) = parse_no_proxy_entry(entry) else {
            continue;
        };

        if parsed.port != 0 && parsed.port != port {
            continue;
        }

        let mut domain = strip_brackets(&parsed.host);
        if let Some(stripped) = domain.strip_prefix("*.") {
            domain = stripped;
        } else if let Some(stripped) = domain.strip_prefix('.').or_else(|| domain.strip_prefix('*')) {
            domain = stripped;
        }

        if domain.is_empty() {
            continue;
        }

        if normalized_target_host == domain {
            return false;
        }

        if normalized_target_host.ends_with(&format!(".{domain}")) {
            return false;
        }
    }

    true
}

fn get_proxy_for_url(target_url: &str, env: Option<&ProviderEnv>) -> String {
    let Ok(parsed_url) = Url::parse(target_url) else {
        return String::new();
    };
    let scheme = parsed_url.scheme();
    let Some(host_str) = parsed_url.host_str() else {
        return String::new();
    };
    if scheme.is_empty() || host_str.is_empty() {
        return String::new();
    }

    let hostname = strip_brackets(host_str);
    let port = parsed_url.port().unwrap_or_else(|| default_proxy_port(scheme));
    if !should_proxy_hostname(hostname, port, env) {
        return String::new();
    }

    let mut proxy = get_proxy_env(&format!("{scheme}_proxy"), env);
    if proxy.is_empty() {
        proxy = get_proxy_env("all_proxy", env);
    }
    if !proxy.is_empty() && !proxy.contains("://") {
        proxy = format!("{scheme}://{proxy}");
    }
    proxy
}

pub fn resolve_http_proxy_url_for_target(target_url: &str, env: Option<&ProviderEnv>) -> pi_js::Result<Option<Url>> {
    let proxy = get_proxy_for_url(target_url, env);
    if proxy.is_empty() {
        return Ok(None);
    }

    let proxy_url = Url::parse(&proxy)
        .map_err(|err| pi_js::Error::msg(format!("Invalid proxy URL {}: {err}", pi_js::json::stringify(&proxy))))?;

    let protocol = proxy_url.scheme();
    if protocol != "http" && protocol != "https" {
        return Err(pi_js::Error::msg(format!(
            "{UNSUPPORTED_PROXY_PROTOCOL_MESSAGE} Got {protocol}:"
        )));
    }

    Ok(Some(proxy_url))
}
