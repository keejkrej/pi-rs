// PORT: QuickJS plugin runtime enabled per user requirement (overrides §10.6)
//! Port of packages/coding-agent/test/extensions-discovery.test.ts

use std::fs;
use std::path::PathBuf;

use pi_coding_agent::core::extensions::discover_and_load_extensions;
use pi_coding_agent::core::extensions::mcp_wire::CONFIG_DIR_NAME_WIRE;

fn setup_dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let root = dir.path().to_path_buf();
    let ext_dir = root.join("extensions");
    fs::create_dir_all(&ext_dir).expect("mkdir extensions");
    (dir, root, ext_dir)
}

fn extension_code() -> &'static str {
    r#"
        export default function(pi) {
            pi.registerCommand("test", { handler: async () => {} });
        }
    "#
}

fn extension_code_with_tool(tool_name: &str) -> String {
    format!(
        r#"
        import {{ Type }} from "typebox";
        export default function(pi) {{
            pi.registerTool({{
                name: "{tool_name}",
                label: "{tool_name}",
                description: "Test tool",
                parameters: Type.Object({{}}),
                execute: async () => ({{ content: [{{ type: "text", text: "ok" }}] }}),
            }});
        }}
    "#
    )
}

#[tokio::test]
async fn discovers_direct_ts_files_in_extensions_dir() {
    let (_guard, root, ext_dir) = setup_dirs();
    fs::write(ext_dir.join("foo.ts"), extension_code()).unwrap();
    fs::write(ext_dir.join("bar.ts"), extension_code()).unwrap();

    let result = discover_and_load_extensions(&[], root.to_str().unwrap(), Some(root.to_str().unwrap()), None).await;

    assert_eq!(result.errors.len(), 0, "errors: {:?}", result.errors);
    assert_eq!(result.extensions.len(), 2);
    let mut names: Vec<_> = result
        .extensions
        .iter()
        .map(|e| {
            std::path::Path::new(&e.path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    assert_eq!(names, vec!["bar.ts", "foo.ts"]);
}

#[tokio::test]
async fn loads_coding_agent_entrypoint_without_rewriting_pi_ai_subpaths() {
    let (_guard, root, ext_dir) = setup_dirs();
    fs::write(
        ext_dir.join("coding-agent-import.ts"),
        r#"
            import { getAgentDir } from "@earendil-works/pi-coding-agent";
            void getAgentDir;
            export default function(pi) {
                pi.registerCommand("test", { handler: async () => {} });
            }
        "#,
    )
    .unwrap();

    let result = discover_and_load_extensions(&[], root.to_str().unwrap(), Some(root.to_str().unwrap()), None).await;

    assert_eq!(result.errors, vec![]);
    assert_eq!(result.extensions.len(), 1);
}

#[tokio::test]
async fn does_not_infer_package_ownership_from_ancestor_manifests() {
    // Regression for #9863.
    let (_guard, root, ext_dir) = setup_dirs();
    let dep_dir = root.join("node_modules/@earendil-works/pi-coding-agent");
    fs::create_dir_all(&dep_dir).unwrap();
    fs::write(
        root.join("package.json"),
        serde_json::json!({
            "name": "application",
            "type": "module",
            "dependencies": { "@earendil-works/pi-coding-agent": "1.0.0" }
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        dep_dir.join("package.json"),
        serde_json::json!({
            "name": "@earendil-works/pi-coding-agent",
            "type": "module",
            "exports": "./index.js"
        })
        .to_string(),
    )
    .unwrap();
    fs::write(dep_dir.join("index.js"), "export const physicalDependency = true;").unwrap();
    fs::write(
        ext_dir.join("compiled-esm-extension.js"),
        r#"
            import { physicalDependency } from "@earendil-works/pi-coding-agent";
            export default function(pi) {
                if (physicalDependency) pi.registerCommand("physical-dependency", { handler: async () => {} });
            }
        "#,
    )
    .unwrap();

    let result = discover_and_load_extensions(&[], root.to_str().unwrap(), Some(root.to_str().unwrap()), None).await;

    assert_eq!(result.errors, vec![]);
    assert_eq!(result.extensions.len(), 1);
    assert!(
        result.extensions[0]
            .commands
            .lock()
            .unwrap()
            .contains_key("physical-dependency")
    );
    assert_eq!(result.warnings, vec![]);
}

#[tokio::test]
async fn keeps_pi_ai_oauth_compatibility_barrel_resolvable() {
    let (_guard, root, ext_dir) = setup_dirs();
    fs::write(
        ext_dir.join("oauth-import.ts"),
        r#"
            import * as oauth from "@earendil-works/pi-ai/oauth";
            void oauth;
            export default function(pi) {
                pi.registerCommand("test", { handler: async () => {} });
            }
        "#,
    )
    .unwrap();

    let result = discover_and_load_extensions(&[], root.to_str().unwrap(), Some(root.to_str().unwrap()), None).await;

    assert_eq!(result.errors, vec![]);
    assert_eq!(result.extensions.len(), 1);
}

#[tokio::test]
async fn discovers_direct_js_files_in_extensions_dir() {
    let (_guard, root, ext_dir) = setup_dirs();
    fs::write(ext_dir.join("foo.js"), extension_code()).unwrap();

    let result = discover_and_load_extensions(&[], root.to_str().unwrap(), Some(root.to_str().unwrap()), None).await;

    assert_eq!(result.errors.len(), 0);
    assert_eq!(result.extensions.len(), 1);
    assert!(result.extensions[0].path.ends_with("foo.js"));
}

#[tokio::test]
async fn discovers_subdirectory_with_index_ts_and_prefers_ts_over_js() {
    let (_guard, root, ext_dir) = setup_dirs();
    let subdir = ext_dir.join("my-extension");
    fs::create_dir_all(&subdir).unwrap();
    fs::write(subdir.join("index.ts"), extension_code()).unwrap();
    fs::write(subdir.join("index.js"), extension_code()).unwrap();

    let result = discover_and_load_extensions(&[], root.to_str().unwrap(), Some(root.to_str().unwrap()), None).await;

    assert_eq!(result.errors.len(), 0);
    assert_eq!(result.extensions.len(), 1);
    assert!(result.extensions[0].path.contains("index.ts"));
}

#[tokio::test]
async fn discovers_subdirectory_with_index_js() {
    let (_guard, root, ext_dir) = setup_dirs();
    let subdir = ext_dir.join("my-extension");
    fs::create_dir_all(&subdir).unwrap();
    fs::write(subdir.join("index.js"), extension_code()).unwrap();

    let result = discover_and_load_extensions(&[], root.to_str().unwrap(), Some(root.to_str().unwrap()), None).await;

    assert_eq!(result.errors.len(), 0);
    assert_eq!(result.extensions.len(), 1);
    assert!(result.extensions[0].path.contains("index.js"));
}

#[tokio::test]
async fn discovers_subdirectory_with_package_json_pi_field_and_tilde_entries() {
    let (_guard, root, ext_dir) = setup_dirs();
    let subdir = ext_dir.join("tilde-package");
    fs::create_dir_all(subdir.join("~")).unwrap();
    let direct_path = subdir.join("~entry.ts");
    let slash_path = subdir.join("~/entry.ts");
    fs::write(&direct_path, extension_code()).unwrap();
    fs::write(&slash_path, extension_code()).unwrap();
    fs::write(
        subdir.join("package.json"),
        serde_json::json!({
            "name": "tilde-package",
            "pi": { "extensions": ["~entry.ts", "~/entry.ts"] }
        })
        .to_string(),
    )
    .unwrap();

    let result = discover_and_load_extensions(&[], root.to_str().unwrap(), Some(root.to_str().unwrap()), None).await;

    assert_eq!(result.errors.len(), 0);
    assert_eq!(result.extensions.len(), 2);
}

#[tokio::test]
async fn package_json_takes_precedence_over_index_and_can_declare_multiple() {
    let (_guard, root, ext_dir) = setup_dirs();
    let pkg = ext_dir.join("my-package");
    fs::create_dir_all(&pkg).unwrap();
    fs::write(pkg.join("index.ts"), extension_code_with_tool("from-index")).unwrap();
    fs::write(pkg.join("custom.ts"), extension_code_with_tool("from-custom")).unwrap();
    fs::write(pkg.join("second.ts"), extension_code_with_tool("from-second")).unwrap();
    fs::write(
        pkg.join("package.json"),
        serde_json::json!({
            "name": "my-package",
            "pi": { "extensions": ["./custom.ts", "./second.ts", "./missing.ts"] }
        })
        .to_string(),
    )
    .unwrap();

    let result = discover_and_load_extensions(&[], root.to_str().unwrap(), Some(root.to_str().unwrap()), None).await;

    assert_eq!(result.errors.len(), 0);
    assert_eq!(result.extensions.len(), 2);
    assert!(result.extensions[0].tools.lock().unwrap().contains_key("from-custom"));
    assert!(result.extensions[1].tools.lock().unwrap().contains_key("from-second"));
    assert!(!result.extensions[0].tools.lock().unwrap().contains_key("from-index"));
}

#[tokio::test]
async fn ignores_package_json_without_pi_field_and_non_extension_subdirs() {
    let (_guard, root, ext_dir) = setup_dirs();
    let pkg = ext_dir.join("my-package");
    fs::create_dir_all(&pkg).unwrap();
    fs::write(pkg.join("index.ts"), extension_code()).unwrap();
    fs::write(pkg.join("package.json"), r#"{"name":"my-package","version":"1.0.0"}"#).unwrap();

    let not_ext = ext_dir.join("not-an-extension");
    fs::create_dir_all(&not_ext).unwrap();
    fs::write(not_ext.join("helper.ts"), extension_code()).unwrap();

    let nested = ext_dir.join("container/nested");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("index.ts"), extension_code()).unwrap();

    let result = discover_and_load_extensions(&[], root.to_str().unwrap(), Some(root.to_str().unwrap()), None).await;

    assert_eq!(result.errors.len(), 0);
    assert_eq!(result.extensions.len(), 1);
    assert!(result.extensions[0].path.contains("index.ts"));
}

#[tokio::test]
async fn reports_errors_for_invalid_extension_code_and_handles_explicit_paths() {
    let (_guard, root, ext_dir) = setup_dirs();
    fs::write(ext_dir.join("invalid.ts"), "this is not valid typescript export").unwrap();
    let custom_path = root.join("custom-location/my-ext.ts");
    fs::create_dir_all(custom_path.parent().unwrap()).unwrap();
    fs::write(&custom_path, extension_code()).unwrap();

    let result = discover_and_load_extensions(
        &[custom_path.to_string_lossy().into_owned()],
        root.to_str().unwrap(),
        Some(root.to_str().unwrap()),
        None,
    )
    .await;

    assert_eq!(result.errors.len(), 1);
    assert!(result.errors[0].path.contains("invalid.ts"));
    assert_eq!(result.extensions.len(), 1);
    assert!(result.extensions[0].path.contains("my-ext.ts"));
}

#[tokio::test]
async fn resolves_dependencies_from_extension_own_node_modules_and_local_config_dir() {
    let (_guard, root, ext_dir) = setup_dirs();
    // Test node_modules dependency resolution (equivalent to `examples/extensions/with-deps` importing `ms`)
    let with_deps = root.join("with-deps");
    let ms_dir = with_deps.join("node_modules/ms");
    fs::create_dir_all(&ms_dir).unwrap();
    fs::write(
        ms_dir.join("index.js"),
        "export default function ms(s) { return 42000; }",
    )
    .unwrap();
    fs::write(
        with_deps.join("index.ts"),
        r#"
            import ms from "ms";
            import { Type } from "typebox";
            export default function(pi) {
                if (ms("42s") === 42000) {
                    pi.registerTool({
                        name: "parse_duration",
                        label: "Parse Duration",
                        description: "Parse duration",
                        parameters: Type.Object({}),
                        execute: async () => ({ content: [{ type: "text", text: String(ms("42s")) }] }),
                    });
                }
            }
        "#,
    )
    .unwrap();

    // Also test .pi/extensions local discovery + renderer registration
    let local_pi_ext = root.join(CONFIG_DIR_NAME_WIRE).join("extensions");
    fs::create_dir_all(&local_pi_ext).unwrap();
    let _ = ext_dir;
    fs::write(
        local_pi_ext.join("with-renderer.ts"),
        r#"
            export default function(pi) {
                pi.registerMarkdownTransformer((markdown) => markdown);
                pi.registerMessageRenderer("my-custom-type", () => null);
                pi.registerEntryRenderer("my-entry-type", () => null);
            }
        "#,
    )
    .unwrap();

    let result = discover_and_load_extensions(
        &[with_deps.to_string_lossy().into_owned()],
        root.to_str().unwrap(),
        Some(root.to_str().unwrap()),
        None,
    )
    .await;

    assert_eq!(result.errors, vec![]);
    assert_eq!(result.extensions.len(), 2);
    assert!(
        result
            .extensions
            .iter()
            .any(|e| e.tools.lock().unwrap().contains_key("parse_duration"))
    );
    let renderer_ext = result
        .extensions
        .iter()
        .find(|e| e.path.contains("with-renderer.ts"))
        .unwrap();
    assert!(renderer_ext.markdown_transformer.lock().unwrap().is_some());
    assert!(
        renderer_ext
            .message_renderers
            .lock()
            .unwrap()
            .contains_key("my-custom-type")
    );
    assert!(
        renderer_ext
            .entry_renderers
            .lock()
            .unwrap()
            .contains_key("my-entry-type")
    );
}
