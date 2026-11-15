//! Port of packages/chord/test/bundle.test.ts
//!
//! JS bundling and VM/CommonJS execution are explicitly unsupported in `pi-rs`;
//! pure-Rust manifest validation and unsupported-JS error contracts are tested here.

use indexmap::IndexMap;
use pi_chord::bundler::{BundleFacetPackageOptions, BundleFacetsOptions, bundle_facet_package, bundle_facets};
use pi_chord::node::{
    FACET_BUNDLE_FORMAT, FACET_BUNDLE_FORMAT_VERSION, FacetBundleArtifactLoaderOptions, FacetBundleEntry,
    FacetBundleLoaderOptions, FacetBundleManifest, FacetBundlePlugin, FacetBundleStringOrUrl,
    ReadFacetBundleArtifactOptions, create_facet_bundle_artifact_loader, create_facet_bundle_loader,
    read_facet_bundle_artifact, read_facet_bundle_manifest,
};

#[tokio::test]
async fn js_bundle_entrypoints_report_explicit_unsupported_error() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().to_string_lossy().into_owned();
    let mut entries = IndexMap::new();
    entries.insert("worker".to_string(), format!("{dir}/entry.ts"));

    let err = bundle_facets(BundleFacetsOptions {
        plugin: FacetBundlePlugin {
            id: "test-bundle".to_string(),
            version: Some("1".to_string()),
        },
        entries,
        outdir: format!("{dir}/bundle"),
        working_directory: None,
        external: None,
        source_map: Some(true),
        minify: None,
        define: None,
        platform: None,
        target: None,
    })
    .await
    .unwrap_err();
    assert!(err.message().contains("not supported by pi-rs"), "got: {err}");

    let pkg_err = bundle_facet_package(BundleFacetPackageOptions {
        package_path: dir.clone(),
        outdir: format!("{dir}/build"),
        default_facets: None,
    })
    .await
    .unwrap_err();
    assert!(pkg_err.message().contains("not supported by pi-rs"), "got: {pkg_err}");

    let manifest_path = format!("{dir}/chord-facets.json");
    let art_err = read_facet_bundle_artifact(ReadFacetBundleArtifactOptions {
        manifest_path: FacetBundleStringOrUrl::String(manifest_path.clone()),
        entry: "worker".to_string(),
    })
    .await
    .unwrap_err();
    assert!(art_err.message().contains("not supported by pi-rs"), "got: {art_err}");

    let loader_err = create_facet_bundle_loader(FacetBundleLoaderOptions {
        manifest_path: FacetBundleStringOrUrl::String(manifest_path),
        entry: "worker".to_string(),
        verify_integrity: None,
        resolve_external: None,
    })
    .err()
    .expect("expected unsupported error");
    assert!(
        loader_err.message().contains("not supported by pi-rs"),
        "got: {loader_err}"
    );

    let art_loader_err = create_facet_bundle_artifact_loader(FacetBundleArtifactLoaderOptions {
        artifact: serde_json::json!({}),
        resolve_external: None,
        temporary_directory: None,
    })
    .err()
    .expect("expected unsupported error");
    assert!(
        art_loader_err.message().contains("not supported by pi-rs"),
        "got: {art_loader_err}"
    );
}

#[tokio::test]
async fn reads_and_validates_facet_bundle_manifests() {
    let temp = tempfile::tempdir().unwrap();
    let manifest_path = temp.path().join("chord-facets.json");

    let mut entries = IndexMap::new();
    entries.insert(
        "worker".to_string(),
        FacetBundleEntry {
            file: "facet-0123456789ab-TEST.cjs".to_string(),
            integrity: "sha256-abc123".to_string(),
            external_imports: vec!["@earendil-works/chord".to_string()],
            source_map: Some("facet-0123456789ab-TEST.cjs.map".to_string()),
        },
    );
    let manifest = FacetBundleManifest {
        format: FACET_BUNDLE_FORMAT.to_string(),
        format_version: FACET_BUNDLE_FORMAT_VERSION,
        plugin: FacetBundlePlugin {
            id: "test-bundle".to_string(),
            version: Some("1".to_string()),
        },
        entries,
    };
    tokio::fs::write(
        &manifest_path,
        pi_js::json::stringify_pretty(&serde_json::to_value(&manifest).unwrap(), "  "),
    )
    .await
    .unwrap();

    let loaded = read_facet_bundle_manifest(FacetBundleStringOrUrl::String(
        manifest_path.to_string_lossy().into_owned(),
    ))
    .await
    .unwrap();
    assert_eq!(loaded, manifest);

    let file_url = url::Url::from_file_path(&manifest_path).unwrap();
    let via_url = read_facet_bundle_manifest(FacetBundleStringOrUrl::Url(file_url))
        .await
        .unwrap();
    assert_eq!(via_url, manifest);

    let bad_url = url::Url::parse("https://example.com/chord-facets.json").unwrap();
    let url_err = read_facet_bundle_manifest(FacetBundleStringOrUrl::Url(bad_url))
        .await
        .unwrap_err();
    assert!(url_err.message().contains("must be a file URL"), "got: {url_err}");

    let escaped = serde_json::json!({
        "format": FACET_BUNDLE_FORMAT,
        "formatVersion": FACET_BUNDLE_FORMAT_VERSION,
        "plugin": { "id": "test-bundle" },
        "entries": {
            "worker": {
                "file": "../outside.cjs",
                "integrity": "sha256-abc123",
                "externalImports": []
            }
        }
    });
    tokio::fs::write(&manifest_path, pi_js::json::stringify(&escaped))
        .await
        .unwrap();
    let escaped_err = read_facet_bundle_manifest(FacetBundleStringOrUrl::String(
        manifest_path.to_string_lossy().into_owned(),
    ))
    .await
    .unwrap_err();
    assert!(
        escaped_err
            .message()
            .contains("must be a filename relative to its manifest"),
        "got: {escaped_err}"
    );
}
