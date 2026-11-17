//! Port of packages/coding-agent/test/experimental-presentation-facets.test.ts

use std::sync::Arc;

use pi_chord::node::{
    FACET_BUNDLE_ARTIFACT_FORMAT, FACET_BUNDLE_ARTIFACT_FORMAT_VERSION, FACET_BUNDLE_FORMAT,
    FACET_BUNDLE_FORMAT_VERSION, FACET_BUNDLE_MANIFEST_FILE, FacetBundleArtifact, FacetBundleEntry,
    FacetBundleManifest, FacetBundlePlugin,
};
use pi_coding_agent::cli::experimental::command_options::TransportAddress;
use pi_coding_agent::cli::experimental::commands::client::ClientCommand;
use pi_coding_agent::experimental::client_runtime::{OpenClientRuntimeOptions, open_client_runtime};
use pi_coding_agent::experimental::plugins::bundled::{
    create_presentation_facet_data, create_presentation_facet_loaders, create_session_plugin_facet_loader,
};
use pi_coding_agent::experimental::plugins::package::{
    create_server_plugin_package, normalize_plugin_package_paths, read_session_plugin_package_profile,
    remove_session_plugin_package_profile, restore_server_plugin_package_profile, write_session_plugin_package_profile,
};
use pi_js::crypto::random_uuid;
use pi_js::fs::promises::{mkdir, write_file};
use pi_js::path::join;
use serde_json::json;
use tempfile::tempdir;

#[tokio::test]
async fn rejects_local_plugin_paths_for_radius_servers() {
    let err = open_client_runtime(
        &ClientCommand {
            auth: None,
            connect: Some(TransportAddress::Radius {
                server_id: random_uuid(),
            }),
            session_id: None,
            continue_: false,
            resume: false,
            provider: None,
            model: None,
            plugin_packages: vec!["./local-plugin".to_string()],
            prompt: None,
        },
        OpenClientRuntimeOptions::default(),
    )
    .await
    .err()
    .expect("must reject plugin packages on radius");

    assert!(
        err.message().contains("only be configured on a local Unix server"),
        "unexpected error: {}",
        err.message()
    );
}

#[tokio::test]
async fn restores_plugin_package_selections_for_later_server_generations() {
    let dir = tempdir().expect("temp dir");
    let directory = dir.path().to_string_lossy().into_owned();
    let server_id = random_uuid();
    let package_paths = vec![
        join(&[&directory, "first-plugin"]),
        join(&[&directory, "second-plugin"]),
    ];

    assert_eq!(
        restore_server_plugin_package_profile(&directory, &server_id, Some(&package_paths))
            .await
            .expect("save"),
        package_paths
    );
    assert_eq!(
        restore_server_plugin_package_profile(&directory, &server_id, None)
            .await
            .expect("restore"),
        package_paths
    );
    assert_eq!(
        restore_server_plugin_package_profile(&directory, &server_id, Some(&[]))
            .await
            .expect("clear"),
        Vec::<String>::new()
    );
    assert_eq!(
        restore_server_plugin_package_profile(&directory, &server_id, None)
            .await
            .expect("restore cleared"),
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn persists_and_normalizes_session_plugin_package_profiles() {
    let dir = tempdir().expect("temp dir");
    let directory = dir.path().to_string_lossy().into_owned();
    let server_id = random_uuid();
    let session_path = join(&[&directory, "sessions", "s1"]);
    let dup_paths = vec![join(&[&directory, "plugin-a"]), join(&[&directory, ".", "plugin-a"])];
    let dup_err =
        normalize_plugin_package_paths(&dup_paths).expect_err("duplicate plugin package paths must be rejected");
    assert!(dup_err.message().contains("must be unique"));

    let raw_paths = vec![join(&[&directory, ".", "plugin-a"]), join(&[&directory, "plugin-b"])];
    let normalized = normalize_plugin_package_paths(&raw_paths).expect("normalize");
    assert_eq!(normalized.len(), 2);

    write_session_plugin_package_profile(&directory, &server_id, &session_path, &raw_paths)
        .await
        .expect("write session profile");
    assert_eq!(
        read_session_plugin_package_profile(&directory, &server_id, &session_path)
            .await
            .expect("read"),
        Some(normalized)
    );
    remove_session_plugin_package_profile(&directory, &server_id, &session_path)
        .await
        .expect("remove");
    assert_eq!(
        read_session_plugin_package_profile(&directory, &server_id, &session_path)
            .await
            .expect("read removed"),
        None
    );
}

#[tokio::test]
async fn reports_unsupported_js_bundling_and_loads_manifest_and_artifact_contracts() {
    // PORT: §2.6 JS facet bundling/evaluation is unsupported in pi-rs; verify canonical cache paths,
    // manifest inspection when `session` entry is absent, presentation artifact envelope validation,
    // and canonical unsupported-bundle error when TS/JS bundling/loading is requested.
    let dir = tempdir().expect("temp dir");
    let directory = dir.path().to_string_lossy().into_owned();
    let server_id = random_uuid();
    let package_path = join(&[&directory, "pi-example-plugin"]);
    mkdir(&join(&[&package_path, "src"]), true, None).await.expect("mkdir");
    write_file(&join(&[&package_path, "src", "tui.ts"]), b"export default {};\n", None)
        .await
        .expect("write ts");

    let plugin = create_server_plugin_package(&directory, &server_id, &package_path);
    assert!(
        plugin.manifest_path.contains(&format!("/plugin-builds/{server_id}/"))
            && plugin
                .manifest_path
                .ends_with(&format!("/{FACET_BUNDLE_MANIFEST_FILE}")),
        "unexpected manifest path: {}",
        plugin.manifest_path
    );
    let build_err = plugin.build().await.err().expect("JS bundling must report unsupported");
    assert!(
        build_err
            .message()
            .contains("JavaScript facet bundles are not supported by pi-rs"),
        "unexpected build error: {}",
        build_err.message()
    );

    // Manifest without `session` entry (e.g., `tui`-only) loads an empty LoadedFacets cleanly.
    let manifest_dir = join(&[&directory, "manifest-no-session"]);
    mkdir(&manifest_dir, true, None).await.expect("mkdir manifest");
    let manifest_path = join(&[&manifest_dir, FACET_BUNDLE_MANIFEST_FILE]);
    let mut entries = indexmap::IndexMap::new();
    entries.insert(
        "tui".to_string(),
        FacetBundleEntry {
            file: "tui.cjs".to_string(),
            integrity: "sha256-test".to_string(),
            external_imports: Vec::new(),
            source_map: None,
        },
    );
    let manifest = FacetBundleManifest {
        format: FACET_BUNDLE_FORMAT.to_string(),
        format_version: FACET_BUNDLE_FORMAT_VERSION,
        plugin: FacetBundlePlugin {
            id: "@test/no-session".to_string(),
            version: Some("1.0.0".to_string()),
        },
        entries,
    };
    let manifest_val = serde_json::to_value(&manifest).expect("manifest to value");
    let manifest_bytes = pi_js::json::stringify_pretty(&manifest_val, "  ").into_bytes();
    write_file(&manifest_path, &manifest_bytes, None)
        .await
        .expect("write manifest");
    let session_loader = create_session_plugin_facet_loader(&[manifest_path]).expect("some loader");
    let loaded = session_loader.load().await.expect("empty manifest loads cleanly");
    assert!(loaded.facets().is_empty());
    loaded.dispose().await.expect("dispose");

    // Presentation data construction & artifact loader validation.
    let artifact = FacetBundleArtifact {
        format: FACET_BUNDLE_ARTIFACT_FORMAT.to_string(),
        format_version: FACET_BUNDLE_ARTIFACT_FORMAT_VERSION,
        plugin: FacetBundlePlugin {
            id: "@test/plugin".to_string(),
            version: Some("1.0.0".to_string()),
        },
        entry_name: "tui".to_string(),
        entry: FacetBundleEntry {
            file: "tui.cjs".to_string(),
            integrity: "sha256-test".to_string(),
            external_imports: vec!["@earendil-works/chord".to_string()],
            source_map: None,
        },
        source: "// js".to_string(),
        source_map_contents: None,
    };
    let data = create_presentation_facet_data(&[artifact]);
    let loaders = create_presentation_facet_loaders(&data).expect("loaders");
    assert_eq!(loaders.len(), 1);
    let artifact_err = loaders[0].load().await.err().expect("JS artifact eval unsupported");
    assert!(
        artifact_err
            .message()
            .contains("JavaScript facet bundles are not supported by pi-rs"),
        "unexpected artifact error: {}",
        artifact_err.message()
    );

    assert!(
        create_presentation_facet_loaders(&json!({}))
            .expect("empty ok")
            .is_empty()
    );
    assert!(create_presentation_facet_loaders(&json!("bad")).is_err());
    let _: Option<Arc<()>> = None;
}
