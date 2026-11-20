//! Port of packages/ai/test/compat-schema.test.ts

use indexmap::IndexMap;
use pi_ai::providers::compat_schema::{
    anthropic_messages_compat_schema, bedrock_compat_schema, open_ai_completions_compat_schema,
    open_ai_responses_compat_schema, provider_compat_schema,
};
use pi_schema::Schema;
use serde_json::{Value, json};

fn schema_defaults(schema: &Schema) -> IndexMap<String, Value> {
    let val = schema.to_value();
    let Some(props) = val.get("properties").and_then(Value::as_object) else {
        return IndexMap::new();
    };
    let mut out = IndexMap::new();
    for (name, prop) in props {
        if let Some(default) = prop.get("default") {
            out.insert(name.clone(), default.clone());
        }
    }
    out
}

#[test]
fn preserves_api_specific_defaults_and_omits_them_from_provider_superset() {
    let completions_defaults = schema_defaults(&open_ai_completions_compat_schema());
    assert!(!completions_defaults.contains_key("thinkingFormat"));
    assert!(!completions_defaults.contains_key("supportsLongCacheRetention"));

    let responses_defaults = schema_defaults(&open_ai_responses_compat_schema());
    assert_eq!(responses_defaults.get("supportsDeveloperRole"), Some(&json!(true)));

    let anthropic_defaults = schema_defaults(&anthropic_messages_compat_schema());
    assert!(!anthropic_defaults.contains_key("sendSessionAffinityHeaders"));
    assert_eq!(anthropic_defaults.get("supportsLongCacheRetention"), Some(&json!(true)));

    let bedrock_defaults = schema_defaults(&bedrock_compat_schema());
    assert_eq!(bedrock_defaults.get("supportsStrictMode"), Some(&json!(false)));

    let provider_defaults = schema_defaults(&provider_compat_schema());
    assert!(!provider_defaults.contains_key("supportsDeveloperRole"));
    assert!(!provider_defaults.contains_key("sendSessionAffinityHeaders"));
    assert!(!provider_defaults.contains_key("supportsLongCacheRetention"));
    assert!(!provider_defaults.contains_key("supportsStrictMode"));
}

#[test]
fn validates_provider_superset_session_affinity_and_compat_properties() {
    let compiled = pi_schema::compile(provider_compat_schema(), Default::default()).unwrap();
    assert!(
        compiled
            .validate(&json!({ "supportsStore": true, "sessionAffinityFormat": "openai-nosession" }))
            .is_ok()
    );
    assert!(
        compiled
            .validate(&json!({ "sessionAffinityFormat": "invalid" }))
            .is_err()
    );
}
