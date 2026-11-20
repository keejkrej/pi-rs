//! Port of packages/coding-agent/test/export-html-deep-tree.test.ts

use std::fmt::Write as _;

use pi_codemode::{QuickJS, QuickJSOptions, load_quick_js_wasm};
use pi_coding_agent::core::export_html::{ExportOptions, export_from_file};
use pi_coding_agent::core::session_manager::CURRENT_SESSION_VERSION;
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use regex::Regex;
use serde_json::{Value, json};

// #11225
#[tokio::test]
async fn builds_and_flattens_single_child_chain_deeper_than_call_stack() {
    init_theme(Some("dark"), Some(false));
    let depth = 20_000usize;
    let temp = tempfile::tempdir().expect("tempdir");
    let session_path = temp.path().join("session.jsonl");
    let mut jsonl = format!(
        "{}\n",
        pi_js::json::stringify(&json!({
            "type": "session",
            "version": CURRENT_SESSION_VERSION,
            "id": "deep-test",
            "timestamp": "1970-01-01T00:00:00.000Z",
            "cwd": temp.path().to_str().expect("utf8"),
        }))
    );
    for i in 0..depth {
        let parent = if i == 0 {
            Value::Null
        } else {
            Value::String(format!("e{}", i - 1))
        };
        let entry = json!({
            "type": "message",
            "id": format!("e{i}"),
            "parentId": parent,
            "timestamp": "1970-01-01T00:00:00.000Z",
            "message": { "role": "user", "content": "x", "timestamp": i * 1000 },
        });
        let _ = writeln!(jsonl, "{}", pi_js::json::stringify(&entry));
    }
    std::fs::write(&session_path, &jsonl).expect("write deep session");

    let out_html = temp.path().join("out.html");
    let exported = export_from_file(
        session_path.to_str().expect("utf8"),
        Some(ExportOptions {
            output_path: Some(out_html.to_str().expect("utf8").to_string()),
            theme_name: None,
            theme_override: None,
            tool_renderer: None,
        }),
    )
    .await
    .expect("export html");
    let html = std::fs::read_to_string(&exported).expect("read exported html");

    let session_re = Regex::new(r#"<script id="session-data" type="application/json">([^<]+)</script>"#).unwrap();
    let session_b64 = session_re
        .captures(&html)
        .and_then(|c| c.get(1))
        .expect("session-data")
        .as_str();
    let script_re = Regex::new(r"(?s)<script>(.*?)</script>").unwrap();
    let viewer = script_re
        .captures_iter(&html)
        .filter_map(|c| c.get(1).map(|m| m.as_str()))
        .last()
        .expect("viewer script");
    let session_json = String::from_utf8(pi_js::b64::decode(session_b64)).expect("utf8");

    let patched_viewer = viewer
        .replace(
            "const base64 = document.getElementById('session-data').textContent;\n      const binary = atob(base64);\n      const bytes = new Uint8Array(binary.length);\n      for (let i = 0; i < binary.length; i++) {\n        bytes[i] = binary.charCodeAt(i);\n      }\n      const data = JSON.parse(new TextDecoder('utf-8').decode(bytes));",
            &format!("const data = {session_json};"),
        )
        .replace(
            "let treeNodeMap = null;",
            "let treeNodeMap = null; globalThis.__treeApi = { buildTree, buildActivePathIds, flattenTree, findNewestLeaf, getPath }; return globalThis.__treeApi;",
        );

    let wasm = load_quick_js_wasm(None).await.expect("load quickjs wasm");
    let vm = QuickJS::create(QuickJSOptions {
        wasm,
        memory_limit: None,
        max_stack_size: None,
        interrupt_handler: None,
        wasi: None,
    })
    .expect("create quickjs");

    let leaf_id = format!("e{}", depth - 1);
    let summary_json = vm.with_scope(|_| {
        vm.eval_code(
            "globalThis.URLSearchParams = class { constructor() {} get() { return null; } };\n\
             globalThis.document = { getElementById: () => ({ textContent: '' }), querySelector: () => null };\n\
             globalThis.window = { location: { search: '' } };",
            Some("shim.js"),
            None,
        )
        .expect("shim");
        vm.eval_code(&patched_viewer, Some("viewer.js"), None).expect("viewer");
        let check = format!(
            "(() => {{\n\
                const api = globalThis.__treeApi;\n\
                const tree = api.buildTree();\n\
                const flat = api.flattenTree(tree, api.buildActivePathIds('{leaf_id}'));\n\
                const path = api.getPath('{leaf_id}');\n\
                return JSON.stringify({{\n\
                    treeLen: tree.length,\n\
                    flatLen: flat.length,\n\
                    flatLastId: flat[flat.length - 1].node.entry.id,\n\
                    newestLeaf: api.findNewestLeaf('e0'),\n\
                    pathLen: path.length,\n\
                    pathFirst: path[0].id,\n\
                    pathLast: path[path.length - 1].id\n\
                }});\n\
            }})()"
        );
        vm.eval_code(&check, Some("check.js"), None)
            .expect("check")
            .to_string()
            .expect("str")
    });
    let summary: Value = pi_js::json::parse(&summary_json).expect("summary");
    assert_eq!(summary["treeLen"], 1);
    assert_eq!(summary["flatLen"], depth);
    assert_eq!(summary["flatLastId"], leaf_id);
    assert_eq!(summary["newestLeaf"], leaf_id);
    assert_eq!(summary["pathLen"], depth);
    assert_eq!(summary["pathFirst"], "e0");
    assert_eq!(summary["pathLast"], leaf_id);
}
