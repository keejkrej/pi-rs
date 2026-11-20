//! Port of packages/coding-agent/test/export-html-cjk.test.ts

use pi_codemode::{QuickJS, QuickJSOptions, load_quick_js_wasm};
use pi_coding_agent::core::export_html::{ExportOptions, export_from_file};
use pi_coding_agent::core::session_manager::CURRENT_SESSION_VERSION;
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use regex::Regex;
use serde_json::json;

fn extract_scripts(html: &str) -> (String, Vec<String>) {
    let session_re = Regex::new(r#"<script id="session-data" type="application/json">([^<]+)</script>"#).unwrap();
    let session_b64 = session_re
        .captures(html)
        .and_then(|c| c.get(1))
        .expect("missing session-data")
        .as_str()
        .to_string();
    let script_re = Regex::new(r"(?s)<script>(.*?)</script>").unwrap();
    let scripts: Vec<String> = script_re
        .captures_iter(html)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .collect();
    (session_b64, scripts)
}

// #10154
#[tokio::test]
async fn renders_cjk_emphasis_and_keeps_non_cjk_flanking_rules() {
    init_theme(Some("dark"), Some(false));
    let temp = tempfile::tempdir().expect("tempdir");
    let session_path = temp.path().join("session.jsonl");
    let header = json!({
        "type": "session",
        "version": CURRENT_SESSION_VERSION,
        "id": "cjk-test",
        "timestamp": "1970-01-01T00:00:00.000Z",
        "cwd": temp.path().to_str().expect("utf8"),
    });
    std::fs::write(&session_path, format!("{}\n", pi_js::json::stringify(&header))).expect("write session");

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
    let (session_b64, mut scripts) = extract_scripts(&html);
    let viewer = scripts.pop().expect("viewer script");
    let data_bytes = pi_js::b64::decode(&session_b64);
    let session_json = String::from_utf8(data_bytes).expect("utf8");

    let wasm = load_quick_js_wasm(None).await.expect("load quickjs wasm");
    let vm = QuickJS::create(QuickJSOptions {
        wasm,
        memory_limit: None,
        max_stack_size: None,
        interrupt_handler: None,
        wasi: None,
    })
    .expect("create quickjs");

    let patched_viewer = viewer
        .replace(
            "const base64 = document.getElementById('session-data').textContent;\n      const binary = atob(base64);\n      const bytes = new Uint8Array(binary.length);\n      for (let i = 0; i < binary.length; i++) {\n        bytes[i] = binary.charCodeAt(i);\n      }\n      const data = JSON.parse(new TextDecoder('utf-8').decode(bytes));",
            &format!("const data = {session_json};"),
        )
        .replace(
            "function safeMarkedParse(",
            "globalThis.__safeMarkedParse = safeMarkedParse; return safeMarkedParse; function safeMarkedParse(",
        );

    let result_json = vm.with_scope(|_| {
        vm.eval_code(
            "globalThis.URLSearchParams = class { constructor() {} get() { return null; } };\n\
             globalThis.document = { getElementById: () => ({ textContent: '' }), querySelector: () => null };\n\
             globalThis.window = { location: { search: '' } };",
            Some("shim.js"),
            None,
        )
        .expect("eval shim");
        for (idx, script) in scripts.iter().enumerate() {
            vm.eval_code(script, Some(&format!("vendor_{idx}.js")), None)
                .expect("eval vendor script");
        }
        vm.eval_code(&patched_viewer, Some("viewer.js"), None)
            .expect("eval viewer");
        let check = vm
            .eval_code(
                "JSON.stringify([\n\
                    globalThis.__safeMarkedParse('**这是测试。**后面'),\n\
                    globalThis.__safeMarkedParse('**一句话说清：**一台机器'),\n\
                    globalThis.__safeMarkedParse('**要点到此为止。**20 小时'),\n\
                    globalThis.__safeMarkedParse('字**「引用」**字'),\n\
                    globalThis.__safeMarkedParse('*斜体。*后面'),\n\
                    globalThis.__safeMarkedParse('**end.**Then next'),\n\
                ])",
                Some("check.js"),
                None,
            )
            .expect("eval check");
        check.to_string().expect("string result")
    });

    let actual: Vec<String> = serde_json::from_str(&result_json).expect("json array");
    assert_eq!(
        actual,
        vec![
            "<p><strong>这是测试。</strong>后面</p>\n",
            "<p><strong>一句话说清：</strong>一台机器</p>\n",
            "<p><strong>要点到此为止。</strong>20 小时</p>\n",
            "<p>字<strong>「引用」</strong>字</p>\n",
            "<p><em>斜体。</em>后面</p>\n",
            "<p>**end.**Then next</p>\n",
        ]
    );
}
