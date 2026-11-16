//! Port of packages/ai/test/model-data-validation.test.ts
// PORT: packages/ai/scripts/*.ts is not ported per PORTING.md §2.6 (`rust: null`);
// this suite asserts all static catalog, schema, modality, and manifest invariants
// directly against `src/providers/data/*.json`, `.manifest.json`, and `model_catalog`.

use indexmap::IndexSet;

use pi_ai::model_catalog::{
    flatten_chat_model_catalog, flatten_classifier_model_catalog, flatten_image_model_catalog, parse_model_groups_json,
};
use pi_ai::models_generated::{
    BUILTIN_MODEL_PROVIDER_IDS, classifier_models_catalog, image_models_catalog, models_catalog,
};
use pi_ai::types::ModelModality;

const MANIFEST_JSON: &str = include_str!("../src/providers/data/.manifest.json");

#[test]
fn validates_manifest_schema_version_timestamp_and_provider_files() {
    let manifest = pi_js::json::parse(MANIFEST_JSON).expect("parse .manifest.json");
    assert_eq!(
        manifest.get("schemaVersion").and_then(|v| v.as_i64()),
        Some(6),
        "expected manifest schemaVersion 6"
    );
    let generated_at = manifest
        .get("generatedAt")
        .and_then(|v| v.as_str())
        .expect("manifest.generatedAt");
    assert!(
        pi_js::time::parse_date(generated_at).is_some(),
        "expected parseable ISO generatedAt: {generated_at}"
    );

    let files = manifest
        .get("files")
        .and_then(|v| v.as_object())
        .expect("manifest.files object");
    assert_eq!(files.len(), BUILTIN_MODEL_PROVIDER_IDS.len());

    let data_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/providers/data");
    for &provider in BUILTIN_MODEL_PROVIDER_IDS {
        let filename = format!("{provider}.json");
        let expected_hash = files
            .get(&filename)
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("missing manifest hash for {filename}"));
        let raw_bytes =
            std::fs::read(data_dir.join(&filename)).unwrap_or_else(|e| panic!("failed to read {filename}: {e}"));
        let actual_hash = pi_js::crypto::sha256_hex(&raw_bytes);
        assert_eq!(actual_hash, expected_hash, "sha256 mismatch for {filename}");
    }
}

#[test]
fn validates_all_provider_data_groups_types_ids_and_modalities() {
    let data_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/providers/data");
    let chat_catalog = models_catalog();
    let image_catalog = image_models_catalog();
    let classifier_catalog = classifier_models_catalog();

    for &provider in BUILTIN_MODEL_PROVIDER_IDS {
        let raw = std::fs::read_to_string(data_dir.join(format!("{provider}.json")))
            .unwrap_or_else(|e| panic!("read {provider}.json: {e}"));
        let groups = parse_model_groups_json(&raw).unwrap_or_else(|e| panic!("parse {provider}.json: {e}"));

        let mut seen_ids_per_type: IndexSet<(String, String)> = IndexSet::new();
        for (api_group, entries) in &groups {
            for (key, value) in entries {
                let model_type = value
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or_else(|| panic!("{provider}/{api_group}/{key}: missing type"));
                assert!(
                    matches!(model_type, "chat" | "image" | "classifier"),
                    "{provider}/{key}: expected \"chat\", \"image\", or \"classifier\", got {model_type}"
                );
                let id = value
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_else(|| panic!("{provider}/{key}: missing id"));
                assert_eq!(
                    key,
                    &format!("{model_type}:{id}"),
                    "{provider}: key {key} must match type:id"
                );
                assert_eq!(
                    value.get("provider").and_then(|v| v.as_str()),
                    Some(provider),
                    "{provider}/{key}: mismatched provider field"
                );
                assert_eq!(
                    value.get("api").and_then(|v| v.as_str()),
                    Some(api_group.as_str()),
                    "{provider}/{key}: grouped under wrong API {api_group}"
                );
                assert!(
                    seen_ids_per_type.insert((model_type.to_owned(), id.to_owned())),
                    "{provider}: duplicate {model_type} id {id} across API groups"
                );
                if model_type == "chat" {
                    assert!(
                        value.get("output").is_none(),
                        "{provider}/{key}: chat model must not declare output modalities"
                    );
                }
            }
        }

        let chats = flatten_chat_model_catalog(provider, &groups).expect("flatten chat");
        let images = flatten_image_model_catalog(provider, &groups).expect("flatten image");
        let classifiers = flatten_classifier_model_catalog(provider, &groups).expect("flatten classifier");

        assert_eq!(chat_catalog.get(provider), Some(&chats));
        assert_eq!(image_catalog.get(provider), Some(&images));
        assert_eq!(classifier_catalog.get(provider), Some(&classifiers));

        for img in images.values() {
            assert!(
                img.output.contains(&ModelModality::Image),
                "{provider}/{} image model must support image output",
                img.id
            );
        }
    }
}
