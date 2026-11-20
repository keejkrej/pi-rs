//! Port of packages/coding-agent/test/suite/regressions/8092-extension-realpath-jiti.test.ts

#![cfg(unix)]

use std::os::unix::fs::symlink;

use pi_coding_agent::core::extensions::{clear_extension_cache, load_extensions};
use tempfile::TempDir;

// Regression #8092: symlinked extensions in pnpm virtual store resolve dependencies relative to their canonical path.
#[tokio::test]
async fn loads_an_extension_whose_declared_dep_is_only_reachable_via_the_pnpm_store() {
    clear_extension_cache();
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path();

    let store_pkg_dir = root
        .join("node_modules")
        .join(".pnpm")
        .join("@test+ext@1.0.0")
        .join("node_modules")
        .join("@test");
    let ext_dir = store_pkg_dir.join("ext");
    let dep_dir = store_pkg_dir.join("dep");
    std::fs::create_dir_all(&ext_dir).expect("mkdir ext");
    std::fs::create_dir_all(&dep_dir).expect("mkdir dep");

    std::fs::write(dep_dir.join("index.js"), "export const value = 42;\n").expect("write dep");
    std::fs::write(
        ext_dir.join("index.ts"),
        r#"
import { value } from "@test/dep";

export default function (pi: any) {
    pi.registerCommand("realpath-check", {
        description: "v=" + String(value),
        handler: async () => {},
    });
}
"#,
    )
    .expect("write ext");

    let top_level = root.join("node_modules").join("@test");
    std::fs::create_dir_all(&top_level).expect("mkdir top_level");
    symlink(&ext_dir, top_level.join("ext")).expect("symlink");

    let entry = top_level.join("ext").join("index.ts");
    assert!(entry.exists());

    let res = load_extensions(
        &[entry.to_str().expect("utf8").to_string()],
        root.to_str().expect("utf8"),
        None,
        None,
    )
    .await;
    assert!(res.errors.is_empty(), "errors: {:?}", res.errors);
    assert_eq!(res.extensions.len(), 1);
    let commands = res.extensions[0].commands.lock().unwrap();
    let cmd = commands.get("realpath-check").expect("registered command");
    assert_eq!(cmd.description.as_deref(), Some("v=42"));

    clear_extension_cache();
}
