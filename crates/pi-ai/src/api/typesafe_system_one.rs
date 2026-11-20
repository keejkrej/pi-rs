//! Port of packages/ai/src/api/typesafe-system-one.ts

use serde_json::{Map, Value};
use url::Url;

use pi_js::{Error, Result};

use crate::api::classifier_shared::is_record;
use crate::api::system_one_shared::{
    ResolvedSystemOneCompat, SystemOneTransport, SystemOneWireRequest, classify_system_one, serialize_to_value,
};
use crate::types::{ClassifierContext, ClassifierModel, ClassifierOptions, ClassifierResult, SystemOneImageInput};

struct TypeSafeSystemOneTransport;

impl SystemOneTransport for TypeSafeSystemOneTransport {
    fn api(&self) -> &str {
        "typesafe-system-one"
    }

    fn label(&self) -> &str {
        "System One API"
    }

    fn detect_compat(&self, model: &ClassifierModel) -> ResolvedSystemOneCompat {
        // OpenRouter by provider ID, which survives baseUrl overrides, or by host, which covers regional endpoints.
        if model.provider == "openrouter" || model.base_url.contains("openrouter.ai") {
            ResolvedSystemOneCompat {
                image_input: SystemOneImageInput::StateImageUrls,
                send_image_opt_in_header: false,
            }
        } else {
            ResolvedSystemOneCompat {
                image_input: SystemOneImageInput::Assets,
                send_image_opt_in_header: true,
            }
        }
    }

    fn url(&self, model: &ClassifierModel) -> Result<Url> {
        let base = format!("{}/", model.base_url.trim_end_matches('/'));
        let parsed = Url::parse(&base).map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?;
        parsed
            .join("systemone")
            .map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))
    }

    fn payload(&self, model: &ClassifierModel, request: SystemOneWireRequest) -> Value {
        let mut map = Map::new();
        map.insert("model".to_owned(), Value::String(model.id.clone()));
        if let Value::Object(req_map) = serialize_to_value(&request) {
            for (k, v) in req_map {
                map.insert(k, v);
            }
        }
        Value::Object(map)
    }

    fn output<'a>(&self, body: &'a Value) -> Result<&'a Map<String, Value>> {
        if !is_record(body) {
            return Err(Error::msg("System One API returned an unexpected response"));
        }
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
