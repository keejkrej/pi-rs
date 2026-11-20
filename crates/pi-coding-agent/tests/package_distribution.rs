//! Port of packages/coding-agent/test/package-distribution.test.ts

use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

fn read_coding_agent_package_json() -> Value {
    let pkg_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("package")
        .join("package.json");
    let raw = fs::read_to_string(&pkg_path).expect("read crates/pi-coding-agent/package/package.json");
    pi_js::json::parse(&raw).expect("parse package.json")
}

#[test]
fn uses_the_bundle_for_executables_and_modular_output_for_libraries() {
    let package_json = read_coding_agent_package_json();
    assert_eq!(package_json["bin"]["pi"], json!("dist/bundle/cli.js"));
    assert_eq!(package_json["main"], json!("./dist/index.js"));
    assert_eq!(package_json["exports"]["."]["import"], json!("./dist/index.js"));
    assert_eq!(
        package_json["exports"]["./rpc-entry"]["import"],
        json!("./dist/bundle/rpc-entry.js")
    );
}
