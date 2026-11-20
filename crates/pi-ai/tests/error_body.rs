//! Port of packages/ai/test/error-body.test.ts (unit ai-utils-errors)

use pi_ai::utils::error_body::{
    MAX_PROVIDER_ERROR_BODY_CHARS, SdkErrorInfo, format_provider_error, normalize_provider_error, normalize_sdk_error,
    truncate_error_text, with_cause_detail,
};
use serde_json::json;

fn info(message: impl Into<String>) -> SdkErrorInfo {
    SdkErrorInfo {
        status_code: None,
        status: None,
        metadata_http_status_code: None,
        response_status_code: None,
        body: None,
        error: None,
        response_body: None,
        message: message.into(),
    }
}

#[test]
fn extracts_status_and_body_from_mistral_shaped_error() {
    let err = SdkErrorInfo {
        status_code: Some(403),
        body: Some(r#"{"error":"blocked by gateway WAF"}"#.to_string()),
        ..info("Mistral request failed")
    };
    let norm = normalize_sdk_error(&err);
    assert_eq!(norm.status, Some(403));
    assert_eq!(norm.body.as_deref(), Some(r#"{"error":"blocked by gateway WAF"}"#));
    assert!(!norm.message_carries_body);
}

#[test]
fn reads_parsed_body_off_openai_api_error_when_message_is_opaque() {
    let err = SdkErrorInfo {
        status: Some(403),
        error: Some(json!({"error": "blocked by gateway WAF"})),
        ..info("403 status code (no body)")
    };
    let norm = normalize_sdk_error(&err);
    assert_eq!(norm.status, Some(403));
    assert_eq!(norm.body.as_deref(), Some(r#"{"error":"blocked by gateway WAF"}"#));
    assert!(!norm.message_carries_body);
}

#[test]
fn preserves_message_when_google_genai_folds_body_into_it() {
    let body = r#"{"error":{"code":403,"message":"Permission denied"}}"#;
    let err = SdkErrorInfo {
        status: Some(403),
        ..info(body)
    };
    let norm = normalize_sdk_error(&err);
    assert_eq!(norm.status, Some(403));
    assert!(norm.message_carries_body);
    assert_eq!(norm.message, body);
}

#[test]
fn extracts_status_and_body_from_bedrock_service_exception() {
    let err = SdkErrorInfo {
        metadata_http_status_code: Some(403),
        response_status_code: Some(403),
        response_body: Some(json!(r#"{"message":"blocked by gateway WAF"}"#)),
        ..info("UnknownError")
    };
    let norm = normalize_sdk_error(&err);
    assert_eq!(norm.status, Some(403));
    assert_eq!(norm.body.as_deref(), Some(r#"{"message":"blocked by gateway WAF"}"#));
    assert!(!norm.message_carries_body);
}

#[test]
fn ignores_empty_parsed_body_object() {
    let err = SdkErrorInfo {
        status: Some(403),
        error: Some(json!({})),
        ..info("403 status code (no body)")
    };
    let norm = normalize_sdk_error(&err);
    assert_eq!(norm.body, None);
    assert!(norm.message_carries_body);
}

#[test]
fn surfaces_plain_parsed_json_body_object() {
    let err = SdkErrorInfo {
        status: Some(400),
        error: Some(json!({"message": "schema validation failed", "field": "tools[0]"})),
        ..info("400 status code (no body)")
    };
    let norm = normalize_sdk_error(&err);
    assert_eq!(
        norm.body.as_deref(),
        Some(r#"{"message":"schema validation failed","field":"tools[0]"}"#)
    );
    assert!(!norm.message_carries_body);
}

#[test]
fn normalizes_boxed_js_error_with_sdk_info() {
    let js_err = SdkErrorInfo {
        status: Some(403),
        error: Some(json!({"error": "blocked by gateway WAF"})),
        ..info("403 status code (no body)")
    }
    .into_js_error();

    let norm = normalize_provider_error(&js_err);
    assert_eq!(norm.status, Some(403));
    assert_eq!(norm.body.as_deref(), Some(r#"{"error":"blocked by gateway WAF"}"#));
}

#[test]
fn truncates_body_at_cap() {
    let long_body = "x".repeat(MAX_PROVIDER_ERROR_BODY_CHARS + 50);
    let err = SdkErrorInfo {
        status_code: Some(500),
        body: Some(long_body.clone()),
        ..info("failed")
    };
    let norm = normalize_sdk_error(&err);
    let body = norm.body.as_deref().unwrap();
    assert!(body.contains("... [truncated 50 chars]"));
    assert!(body.len() < long_body.len());
}

#[test]
fn sets_message_carries_body_when_message_contains_extracted_body() {
    let err = SdkErrorInfo {
        status_code: Some(500),
        body: Some("upstream exploded".to_string()),
        ..info("500: upstream exploded")
    };
    let norm = normalize_sdk_error(&err);
    assert!(norm.message_carries_body);
}

#[test]
fn format_provider_error_surfaces_status_and_body() {
    let norm = normalize_sdk_error(&SdkErrorInfo {
        status: Some(403),
        error: Some(json!({"error": "blocked by gateway WAF"})),
        ..info("403 status code (no body)")
    });

    let formatted = format_provider_error(&norm, None);
    assert!(formatted.contains("403"));
    assert!(formatted.contains("blocked by gateway WAF"));
    assert_ne!(formatted, "403 status code (no body)");

    assert_eq!(
        format_provider_error(&norm, Some("OpenAI API error")),
        r#"OpenAI API error (403): {"error":"blocked by gateway WAF"}"#
    );

    let body = r#"{"error":{"message":"Permission denied"}}"#;
    let google_norm = normalize_sdk_error(&SdkErrorInfo {
        status: Some(403),
        ..info(body)
    });
    assert_eq!(
        format_provider_error(&google_norm, Some("OpenAI API error")),
        format!("OpenAI API error (403): {body}")
    );

    let bare_norm = normalize_provider_error(&pi_js::Error::msg(r#"{"reason":"boom"}"#));
    assert_eq!(format_provider_error(&bare_norm, None), r#"{"reason":"boom"}"#);
}

// #9265
#[test]
fn truncate_error_text_keeps_independent_excerpt_without_retaining_whole_body() {
    let body = "<html>".repeat(1_000_000);
    let kept = truncate_error_text(&body, MAX_PROVIDER_ERROR_BODY_CHARS);
    drop(body);
    assert!(kept.starts_with("<html><html>"));
    assert!(kept.ends_with(&format!(
        "[truncated {} chars]",
        6_000_000 - MAX_PROVIDER_ERROR_BODY_CHARS
    )));
}

#[test]
fn connection_error_cause_detail_appends_deepest_cause_and_error_code() {
    let socket_err = pi_js::Error::js_with_code("Error", "connect ECONNREFUSED 127.0.0.1:8080", "ECONNREFUSED");
    let fetch_err = pi_js::Error::js_with_cause("TypeError", "fetch failed", socket_err);
    let error = pi_js::Error::js_with_cause("Error", "Connection error.", fetch_err);

    assert_eq!(
        format_provider_error(&normalize_provider_error(&error), None),
        "Connection error.: connect ECONNREFUSED 127.0.0.1:8080"
    );

    let cause_no_code = pi_js::Error::js_with_code("Error", "other side closed", "UND_ERR_SOCKET");
    let err_with_code = pi_js::Error::js_with_cause("Error", "Connection error.", cause_no_code);
    assert_eq!(
        with_cause_detail(&err_with_code),
        "Connection error.: other side closed (UND_ERR_SOCKET)"
    );

    assert_eq!(
        with_cause_detail(&pi_js::Error::js("Error", "Connection error.")),
        "Connection error."
    );
}
