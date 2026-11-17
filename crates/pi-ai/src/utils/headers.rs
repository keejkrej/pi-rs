//! Port of packages/ai/src/utils/headers.ts

use indexmap::IndexMap;
use pi_js::fetch::Headers;

use crate::types::ProviderHeaders;

pub fn headers_to_record(headers: &Headers) -> IndexMap<String, String> {
    let mut result = IndexMap::new();
    for (key, value) in headers.entries() {
        result.insert(key, value);
    }
    result
}

pub fn provider_headers_to_record<'a>(
    header_sources: impl IntoIterator<Item = Option<&'a ProviderHeaders>>,
) -> Option<IndexMap<String, String>> {
    let mut merged: IndexMap<String, (String, String)> = IndexMap::new();
    for source in header_sources.into_iter().flatten() {
        for (name, value) in source {
            let normalized_name = name.to_ascii_lowercase();
            merged.shift_remove(&normalized_name);
            if let Some(val) = value {
                merged.insert(normalized_name, (name.clone(), val.clone()));
            }
        }
    }
    if merged.is_empty() {
        None
    } else {
        Some(merged.into_values().collect())
    }
}
