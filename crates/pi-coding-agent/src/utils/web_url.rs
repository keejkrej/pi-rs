//! Port of packages/coding-agent/src/utils/web-url.ts

use pi_tui::utils::replace_control_characters;

/// The `http:` or `https:` URL `value` names, normalized, or `None`.
///
/// The shared launcher also opens files and custom protocols, so login only opens web URLs.
/// Normalizing percent-encodes control characters, which would otherwise end a terminal hyperlink early.
pub fn web_url(value: &str) -> Option<String> {
    let parsed = url::Url::parse(value).ok()?;
    match parsed.scheme() {
        "http" | "https" => Some(parsed.to_string()),
        _ => None,
    }
}

/// `value` normalized as a web URL, else with control characters replaced: an untrusted string printed as is
/// could end a hyperlink early or send its own escape sequences to the terminal.
pub fn display_url(value: &str) -> String {
    web_url(value).unwrap_or_else(|| replace_control_characters(value))
}
