//! Port of packages/coding-agent/test/export-html-cost.test.ts

use std::path::PathBuf;

use pi_codemode::{QuickJS, QuickJSOptions, load_quick_js_wasm};
use serde_json::Value;

// #9980: provider-reported totals may differ from catalog-estimated components.
#[tokio::test]
async fn uses_recorded_totals_while_falling_back_to_component_sums_for_old_sessions() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let template_js =
        std::fs::read_to_string(manifest_dir.join("src/core/export_html/template.js")).expect("read template.js");
    let start = template_js
        .find("function computeStats(entryList)")
        .expect("computeStats start");
    let end = template_js
        .find("const globalStats = computeStats(entries);")
        .expect("computeStats end");
    assert!(end > start);

    let entries = serde_json::json!([
        {
            "type": "message",
            "message": {
                "role": "assistant",
                "content": [],
                "usage": { "cost": { "input": 0.1, "output": 0.2, "cacheRead": 0, "cacheWrite": 0, "total": 0.9 } }
            }
        },
        {
            "type": "message",
            "message": {
                "role": "assistant",
                "content": [],
                "usage": { "cost": { "input": 0.4, "output": 0.1, "cacheRead": 0, "cacheWrite": 0 } }
            }
        }
    ]);

    let wasm = load_quick_js_wasm(None).await.expect("load quickjs wasm");
    let vm = QuickJS::create(QuickJSOptions {
        wasm,
        memory_limit: None,
        max_stack_size: None,
        interrupt_handler: None,
        wasi: None,
    })
    .expect("create quickjs");

    let script = format!(
        "{}\nJSON.stringify(computeStats({}));",
        &template_js[start..end],
        pi_js::json::stringify(&entries),
    );
    let result_json = vm.with_scope(|_| {
        vm.eval_code(&script, Some("cost.js"), None)
            .expect("eval cost")
            .to_string()
            .expect("string")
    });
    let stats: Value = pi_js::json::parse(&result_json).expect("stats json");
    let input = stats["cost"]["input"].as_f64().expect("input cost");
    let output = stats["cost"]["output"].as_f64().expect("output cost");
    let total = stats["cost"]["total"].as_f64().expect("total cost");
    assert!((input - 0.5).abs() < 1e-6, "input={input}");
    assert!((output - 0.3).abs() < 1e-6, "output={output}");
    assert!((total - 1.4).abs() < 1e-6, "total={total}");
}
