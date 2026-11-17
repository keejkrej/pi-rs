//! Port of `packages/coding-agent/test/bug-report.test.ts` (unit coding-agent-trust-mcp-bugreport-crashlog)

// @generated-mods begin (scaffold-owned, do not edit)
#[path = "bug_report/part_2.rs"]
mod part_2;
// @generated-mods end

use std::sync::{Arc, Mutex};

use pi_agent_core::ThinkingLevel;
use pi_coding_agent::core::bug_report::{
    BugReportBundle, BugReportProviderInfo, CollectBugReportMetadataOptions, bug_report_archive_file_name,
    bug_report_files, collect_bug_report_diagnostics, collect_bug_report_metadata, redact_json_value, redact_url,
    write_bug_report_archive,
};
use pi_coding_agent::core::bug_report_upload::{UploadBugReportOptions, upload_bug_report};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::settings_manager::Settings;
use pi_js::fetch::Headers;
use pi_js::fetch::testing::mock_fetch;
use serde_json::json;
use tempfile::TempDir;

#[test]
fn removes_url_credentials_and_secret_query_parameters() {
    assert_eq!(
        redact_url("https://user:pass@proxy.example.com:8080/"),
        "https://proxy.example.com:8080/"
    );
    assert_eq!(
        redact_url("git:https://pat@github.com/org/repo"),
        "git:https://github.com/org/repo"
    );
    assert_eq!(
        redact_url("https://api.example/v1?api-key=abc&model=x"),
        "https://api.example/v1?api-key=%3Credacted%3E&model=x"
    );
}

#[test]
fn redacts_nested_secret_values_without_hiding_token_counts() {
    let redacted = redact_json_value(&json!({
        "apiKey": "sk-123",
        "headers": { "Authorization": "Bearer x", "X-Trace": "1" },
        "compaction": { "reserveTokens": 16_384, "keepRecentTokens": 20_000 },
        "baseUrl": "https://me:secret@example.com/",
    }));

    assert_eq!(
        redacted,
        json!({
            "apiKey": "<redacted>",
            "headers": { "Authorization": "<redacted>", "X-Trace": "1" },
            "compaction": { "reserveTokens": 16_384, "keepRecentTokens": 20_000 },
            "baseUrl": "https://example.com/",
        })
    );
}

#[tokio::test]
async fn bug_report_bundle_archive_and_upload_work_offline() {
    let tmp = TempDir::new().unwrap();
    let sm = SessionManager::in_memory(Some(tmp.path().to_str().unwrap()), None, None).unwrap();
    let metadata = collect_bug_report_metadata(CollectBugReportMetadataOptions {
        id: Some("report-1".to_owned()),
        hint: Some("  something broke  ".to_owned()),
        session_id: sm.get_session_id(),
        cwd: sm.get_cwd(),
        include_session: false,
        include_summary: true,
        message_count: 2,
        model: None,
        provider_info: Some(BugReportProviderInfo {
            id: "openai".to_owned(),
            name: "OpenAI".to_owned(),
            base_url: Some("https://secret@api.openai.com/v1".to_owned()),
            header_names: vec!["x-custom".to_owned()],
            auth_types: vec!["api_key".to_owned()],
            auth_status: "api_key".to_owned(),
            using_oauth: false,
            registered_by_extension: false,
        }),
        provider_source: None,
        thinking_level: ThinkingLevel::Off,
        extensions: Vec::new(),
        extension_errors: Vec::new(),
        global_settings: Settings::default(),
        project_settings: Settings::default(),
    });
    assert_eq!(metadata.hint.as_deref(), Some("something broke"));
    assert_eq!(
        metadata.provider.as_ref().unwrap().base_url.as_deref(),
        Some("https://api.openai.com/v1")
    );
    assert!(metadata.session.cwd.is_none());

    let diagnostics = collect_bug_report_diagnostics(&sm, &[]);
    let bundle = BugReportBundle {
        metadata,
        diagnostics,
        session_jsonl: None,
        summary: Some("## What went wrong\nCrash".to_owned()),
    };

    let files = bug_report_files(&bundle);
    assert_eq!(files.len(), 3);
    let zip_path = tmp.path().join(bug_report_archive_file_name("report-1"));
    write_bug_report_archive(&bundle, zip_path.to_str().unwrap())
        .await
        .unwrap();
    assert!(zip_path.exists());

    let seen_auth: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let seen_body: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let auth_ref = Arc::clone(&seen_auth);
    let body_ref = Arc::clone(&seen_body);
    let _guard = mock_fetch(move |req| {
        let a = Arc::clone(&auth_ref);
        let b = Arc::clone(&body_ref);
        Box::pin(async move {
            *a.lock().unwrap() = req.headers.get("authorization");
            *b.lock().unwrap() = req.body_text().unwrap_or_default();
            let mut headers = Headers::new();
            headers.set("content-type", "application/json");
            Ok::<_, pi_js::error::Error>(pi_js::fetch::Response::new(
                200,
                headers,
                pi_js::json::stringify(&json!({ "ok": true, "bug_report": { "id": "uploaded-99" } })),
            ))
        })
    });

    let uploaded = upload_bug_report(
        &bundle,
        Some(UploadBugReportOptions {
            token: Some("tok-123".to_owned()),
            signal: None,
            gateway_url: Some("https://radius.example.test".to_owned()),
        }),
    )
    .await
    .unwrap();
    assert_eq!(uploaded.id, "uploaded-99");
    assert_eq!(seen_auth.lock().unwrap().as_deref(), Some("Bearer tok-123"));
    let multipart = seen_body.lock().unwrap();
    assert!(multipart.contains("filename=\"report.json\""));
    assert!(multipart.contains("filename=\"summary.md\""));
}
