//! Port of packages/ai/src/auth/oauth/trusted-url.ts

use url::Url;

/// The normalized `http:` or `https:` URL `value` names, or `None`.
///
/// Login flows open server-supplied URLs with the platform URL handler, which would also launch
/// a `file:` URL or custom protocol handler. Parsing and re-serializing via `url::Url` normalizes
/// percent-encoding and control characters for safe browser opening and terminal hyperlinks.
pub fn trusted_http_url(value: &str) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    let parsed = Url::parse(value).ok()?;
    match parsed.scheme() {
        "http" | "https" => Some(parsed.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_http_and_https_and_rejects_others() {
        assert_eq!(
            trusted_http_url("https://example.com/device?a=1 2"),
            Some("https://example.com/device?a=1%202".to_owned())
        );
        assert_eq!(
            trusted_http_url("http://127.0.0.1:8080/cb"),
            Some("http://127.0.0.1:8080/cb".to_owned())
        );
        assert_eq!(trusted_http_url("file:///etc/passwd"), None);
        assert_eq!(trusted_http_url("javascript:alert(1)"), None);
        assert_eq!(trusted_http_url("not a url"), None);
        assert_eq!(trusted_http_url(""), None);
    }
}
