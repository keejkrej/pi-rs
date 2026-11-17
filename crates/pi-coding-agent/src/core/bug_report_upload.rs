//! HTTP multipart uploader for bug report bundles.
//!
//! Port of `packages/coding-agent/src/core/bug-report-upload.ts`.

use pi_ai::uuidv7;
use pi_js::abort::AbortSignal;
use pi_js::error::{Error, Result};
use pi_js::fetch::{Request, fetch};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::core::bug_report::{BugReportBundle, bug_report_files};
use crate::core::radius::get_radius_gateway_url;

/// Options controlling [`upload_bug_report`].
#[derive(Clone, Debug, Default)]
pub struct UploadBugReportOptions {
    pub token: Option<String>,
    pub signal: Option<AbortSignal>,
    pub gateway_url: Option<String>,
}

/// Successful bug report upload response carrying the allocated report ID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadBugReportResponse {
    pub id: String,
}

fn build_multipart_body(bundle: &BugReportBundle, boundary: &str) -> Vec<u8> {
    let mut body = Vec::new();
    for file in bug_report_files(bundle) {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\n",
                file.name, file.name
            )
            .as_bytes(),
        );
        body.extend_from_slice(format!("Content-Type: {}\r\n\r\n", file.content_type).as_bytes());
        body.extend_from_slice(file.data.as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

/// Upload a report as multipart form data, anonymously or attributed to a Radius account.
pub async fn upload_bug_report(
    bundle: &BugReportBundle,
    options: Option<UploadBugReportOptions>,
) -> Result<UploadBugReportResponse> {
    let opts = options.unwrap_or_default();
    let gateway = opts.gateway_url.unwrap_or_else(get_radius_gateway_url);
    let base = Url::parse(&gateway).map_err(|e| Error::js("TypeError", format!("Invalid gateway URL: {e}")))?;
    let endpoint = base
        .join("/v1/bug-reports")
        .map_err(|e| Error::js("TypeError", format!("Invalid bug report endpoint URL: {e}")))?;

    let suffix = uuidv7(None).unwrap_or_else(|_| pi_js::time::now_ms().to_string());
    let boundary = format!("----pi-bug-report-{suffix}");
    let body = build_multipart_body(bundle, &boundary);

    let mut request = Request::post(endpoint.to_string())
        .header("content-type", &format!("multipart/form-data; boundary={boundary}"))
        .body(body);
    if let Some(token) = &opts.token
        && !token.is_empty()
    {
        request = request.header("authorization", &format!("Bearer {token}"));
    }
    if let Some(signal) = opts.signal {
        request = request.signal(signal);
    }

    let response = fetch(request).await?;
    let status = response.status();
    let status_text = response.status_text().to_owned();
    let ok = response.ok();

    let body_text = response.text().await.unwrap_or_default();
    let json = pi_js::json::parse(&body_text).ok();

    if ok
        && let Some(ref val) = json
        && val.get("ok").and_then(Value::as_bool) == Some(true)
        && let Some(id) = val
            .get("bug_report")
            .and_then(|br| br.get("id"))
            .and_then(Value::as_str)
    {
        return Ok(UploadBugReportResponse { id: id.to_owned() });
    }

    let detail = json.as_ref().and_then(|val| {
        if val.get("ok").and_then(Value::as_bool) == Some(true) {
            return None;
        }
        val.get("description")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| val.get("error").and_then(Value::as_str).filter(|s| !s.is_empty()))
            .map(ToOwned::to_owned)
    });

    let fallback = if !status_text.is_empty() {
        status_text
    } else {
        status.to_string()
    };
    Err(Error::msg(format!(
        "Bug report upload failed: {}",
        detail.unwrap_or(fallback)
    )))
}
