//! Port of packages/ai/src/api/typesafe-system-one.ts

use serde_json::{Map, Value, json};

use pi_js::{Error, Result};

use crate::api::system_one_shared::{SystemOneTransport, SystemOneWireRequest, classify_system_one};
use crate::types::{ClassifierContext, ClassifierModel, ClassifierOptions, ClassifierResult};

struct TypeSafeSystemOneTransport;

impl SystemOneTransport for TypeSafeSystemOneTransport {
    fn api(&self) -> &str {
        "typesafe-system-one"
    }

    fn label(&self) -> &str {
        "System One API"
    }

    fn url(&self, model: &ClassifierModel) -> Result<String> {
        let base = format!("{}/", model.base_url.trim_end_matches('/'));
        let parsed = url::Url::parse(&base).map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?;
        let joined = parsed
            .join("systemone")
            .map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?;
        Ok(joined.to_string())
    }

    fn payload(&self, model: &ClassifierModel, request: SystemOneWireRequest) -> Value {
        json!({
            "model": model.id,
            "state": request.state,
            "questions": request.questions,
        })
    }

    fn output<'a>(&self, body: &'a Value) -> Result<&'a Map<String, Value>> {
        body.as_object()
            .ok_or_else(|| Error::msg("System One API returned an unexpected response"))
    }
}

static TRANSPORT: TypeSafeSystemOneTransport = TypeSafeSystemOneTransport;

/// TypeSafe System One classification with public `bool` values mapped to wire-level `noul`.
pub async fn classify(
    model: &ClassifierModel,
    context: &ClassifierContext,
    options: Option<ClassifierOptions>,
) -> ClassifierResult {
    classify_system_one(&TRANSPORT, model, context, options).await
}
