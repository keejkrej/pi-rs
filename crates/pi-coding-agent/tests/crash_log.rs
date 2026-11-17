//! Port of packages/coding-agent/test/crash-log.test.ts

use pi_coding_agent::core::crash_log::{ExtensionStackMetadata, find_extension_stack_matches};
use pi_coding_agent::core::source_info::{SourceInfo, SourceOrigin, SourceScope};

fn package_extension(source: &str, base_dir: &str, entry: &str) -> ExtensionStackMetadata {
    let normalized_base_dir = base_dir.replace('\\', "/");
    let resolved_path = format!("{normalized_base_dir}/{entry}");
    ExtensionStackMetadata {
        path: resolved_path.clone(),
        resolved_path: resolved_path.clone(),
        source_info: SourceInfo {
            path: resolved_path,
            source: source.to_owned(),
            scope: SourceScope::User,
            origin: SourceOrigin::Package,
            base_dir: Some(base_dir.to_owned()),
            changelog_path: None,
        },
    }
}

#[test]
fn matches_stack_frames_beneath_loaded_package_roots() {
    let memory = package_extension(
        "npm:pi-observational-memory",
        "/home/fedora/.pi/agent/npm/node_modules/pi-observational-memory",
        "extensions/index.ts",
    );
    let unrelated = package_extension(
        "npm:unrelated",
        "/home/fedora/.pi/agent/npm/node_modules/unrelated",
        "extensions/index.ts",
    );
    let stack = concat!(
        "TypeError: Cannot read properties of undefined (reading 'runtime')\n",
        "    at streamSimple (file:///home/fedora/.local/lib/node_modules/@earendil-works/pi-coding-agent/dist/bundle/chunks/chunk-CMRUVXTE.js:1093:16944)\n",
        "    at /home/fedora/.pi/agent/npm/node_modules/pi-observational-memory/src/agents/worker-stream.ts:43:45",
    );

    pretty_assertions::assert_eq!(
        find_extension_stack_matches(Some(stack), &[memory, unrelated]),
        vec!["npm:pi-observational-memory".to_owned()]
    );
}

#[test]
fn normalizes_windows_paths_and_deduplicates_package_extensions() {
    let root = r"C:\Users\reporter\.pi\agent\npm\node_modules\@scope\memory";
    let first = package_extension("npm:@scope/memory", root, "extensions/first.ts");
    let second = package_extension("npm:@scope/memory", root, "extensions/second.ts");
    let stack = concat!(
        "Error: broken\n",
        r"    at run (c:\users\reporter\.pi\agent\npm\node_modules\@scope\memory\src\worker.ts:4:2)",
    );

    pretty_assertions::assert_eq!(
        find_extension_stack_matches(Some(stack), &[first, second]),
        vec!["npm:@scope/memory".to_owned()]
    );
}

#[test]
fn does_not_attribute_sibling_single_file_packages() {
    let extension = |name: &str| ExtensionStackMetadata {
        path: format!("/plugins/{name}.ts"),
        resolved_path: format!("/plugins/{name}.ts"),
        source_info: SourceInfo {
            path: format!("/plugins/{name}.ts"),
            source: format!("/plugins/{name}.ts"),
            scope: SourceScope::User,
            origin: SourceOrigin::Package,
            base_dir: Some("/plugins".to_owned()),
            changelog_path: None,
        },
    };
    let stack = "Error: broken\n    at run (file:///plugins/b.ts:4:2)";

    pretty_assertions::assert_eq!(
        find_extension_stack_matches(Some(stack), &[extension("a"), extension("b")]),
        vec!["/plugins/b.ts".to_owned()]
    );
}

#[test]
fn ignores_extension_paths_in_the_error_message() {
    let extension = package_extension("npm:memory", "/tmp/node_modules/memory", "extensions/index.ts");
    let stack = format!(
        "Error: Failed to read {}\n    at run (file:///opt/pi/dist/core.js:4:2)",
        extension.resolved_path
    );

    pretty_assertions::assert_eq!(
        find_extension_stack_matches(Some(&stack), &[extension]),
        Vec::<String>::new()
    );
}

#[test]
fn decodes_frame_paths_independently_from_malformed_error_text() {
    let extension = ExtensionStackMetadata {
        path: "/Users/reporter/.pi/agent/extensions/local memory/index.ts".to_owned(),
        resolved_path: "/Users/reporter/.pi/agent/extensions/local memory/index.ts".to_owned(),
        source_info: SourceInfo {
            path: "/Users/reporter/.pi/agent/extensions/local memory/index.ts".to_owned(),
            source: "local".to_owned(),
            scope: SourceScope::User,
            origin: SourceOrigin::TopLevel,
            base_dir: Some("/Users/reporter/.pi/agent/extensions".to_owned()),
            changelog_path: None,
        },
    };
    let stack = concat!(
        "Error: progress 100%\n",
        "    at run (file:///Users/reporter/.pi/agent/extensions/local%20memory/worker.ts:4:2)",
    );

    pretty_assertions::assert_eq!(
        find_extension_stack_matches(Some(stack), std::slice::from_ref(&extension)),
        vec![extension.path]
    );
}
