//! Port of packages/ai/src/api/cloudflare-workers-ai-system-one.ts

use serde_json::{Map, Value, json};
use url::Url;

use pi_js::{Error, Result};

use crate::api::classifier_shared::is_record;
use crate::api::system_one_shared::{
    ResolvedSystemOneCompat, SystemOneTransport, SystemOneWireRequest, classify_system_one, serialize_to_value,
};
use crate::types::{ClassifierContext, ClassifierModel, ClassifierOptions, ClassifierResult, SystemOneImageInput};

const LABEL: &str = "Cloudflare Workers AI";

fn cloudflare_error_message(errors: Option<&Value>) -> String {
    if let Some(Value::Array(items)) = errors {
        let messages: Vec<&str> = items
            .iter()
            .filter_map(|item| item.as_object()?.get("message")?.as_str())
            .collect();
        if !messages.is_empty() {
            return format!("{LABEL} error: {}", messages.join("; "));
        }
    }
    format!("{LABEL} request failed")
}

fn format_state_value(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_owned(),
        Some(Value::Null) => "null".to_owned(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => pi_js::json::stringify(other),
    }
}

struct CloudflareWorkersAISystemOneTransport;

impl SystemOneTransport for CloudflareWorkersAISystemOneTransport {
    fn api(&self) -> &str {
        "cloudflare-workers-ai-system-one"
    }

    fn label(&self) -> &str {
        LABEL
    }

    fn detect_compat(&self, model: &ClassifierModel) -> ResolvedSystemOneCompat {
        ResolvedSystemOneCompat {
            image_input: if model.id.starts_with("@cf/cloudflare/clef") {
                SystemOneImageInput::Images
            } else {
                SystemOneImageInput::Assets
            },
            send_image_opt_in_header: false,
        }
    }

    fn url(&self, model: &ClassifierModel) -> Result<Url> {
        let base = format!("{}/", model.base_url.trim_end_matches('/'));
        let parsed = Url::parse(&base).map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?;
        parsed
            .join("run")
            .map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))
    }

    fn payload(&self, model: &ClassifierModel, request: SystemOneWireRequest) -> Value {
        json!({
            "model": model.id,
            "input": serialize_to_value(&request),
        })
    }

    fn output<'a>(&self, body: &'a Value) -> Result<&'a Map<String, Value>> {
        if !is_record(body) {
            return Err(Error::msg(format!("{LABEL} returned an unexpected response")));
        }
        let Some(root) = body.as_object() else {
            return Err(Error::msg(format!("{LABEL} returned an unexpected response")));
        };
        if root.get("success") == Some(&Value::Bool(false)) {
            return Err(Error::msg(cloudflare_error_message(root.get("errors"))));
        }
        let Some(result) = root.get("result").and_then(Value::as_object) else {
            return Err(Error::msg(format!("{LABEL} returned an unexpected response")));
        };
        if result.contains_key("answers") {
            return Ok(result);
        }
        if result.get("state").and_then(Value::as_str) != Some("Completed") {
            return Err(Error::msg(format!(
                "{LABEL} run did not complete (state: {})",
                format_state_value(result.get("state"))
            )));
        }
        result
            .get("result")
            .and_then(Value::as_object)
            .ok_or_else(|| Error::msg(format!("{LABEL} returned an unexpected response")))
    }
}

static TRANSPORT: CloudflareWorkersAISystemOneTransport = CloudflareWorkersAISystemOneTransport;

/// Cloudflare Workers AI System One classification with public `bool` values mapped to wire-level `noul`.
pub async fn classify(
    model: &ClassifierModel,
    context: &ClassifierContext,
    options: Option<ClassifierOptions>,
) -> ClassifierResult {
    classify_system_one(&TRANSPORT, model, context, options).await
}
