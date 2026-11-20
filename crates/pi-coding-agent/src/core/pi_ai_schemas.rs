//! Port of packages/coding-agent/src/core/pi-ai-schemas.ts

pub use pi_ai::providers::compat_schema::provider_compat_schema;
pub use pi_ai::providers::model_schema::{
    model_cost_schema, model_input_limits_schema, model_input_modality_schema, model_prompt_cache_schema,
    model_thinking_level_schema, thinking_level_map_schema,
};
use pi_schema::Schema;
use serde_json::Value;

/// `model_cost_schema` with every field optional, for model overrides that change some of the rates.
pub fn partial_model_cost_schema() -> Schema {
    let mut val = model_cost_schema().into_value();
    if let Value::Object(ref mut map) = val {
        map.remove("required");
    }
    Schema::from_value(val)
}
