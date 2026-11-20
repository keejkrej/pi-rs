//! Port of packages/coding-agent/test/export-html-system-messages.test.ts

use std::path::PathBuf;

use pi_ai::types::SystemMessage;
use pi_ai::utils::text::{get_system_message_text, render_system_message_update};
use pi_codemode::{QuickJS, QuickJSOptions, load_quick_js_wasm};
use regex::Regex;
use serde_json::{Value, json};

async fn eval_system_helpers(template_js: &str, entries: &Value) -> Value {
    let start = template_js
        .find("function systemContentText(content)")
        .expect("systemContentText start");
    let end = template_js
        .find("const hasSystemEntries")
        .expect("hasSystemEntries end");
    assert!(end > start);

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
        "(() => {{\n\
            const entries = {entries_json};\n\
            const byId = new Map(entries.map(e => [e.id, e]));\n\
            {slice}\n\
            return JSON.stringify({{\n\
                textInitial: entries[0]?.message ? getSystemMessageText(entries[0].message) : null,\n\
                updateRendered: entries[2]?.message ? renderSystemMessageUpdate(entries[2].message) : null,\n\
                leadingFirst: entries[0] ? isLeadingSystemEntry(entries[0]) : null,\n\
                leadingUpdate: entries[entries.length - 1] ? isLeadingSystemEntry(entries[entries.length - 1]) : null,\n\
                updateDesc: entries[2]?.message ? describeSystemUpdate(entries[2].message) : null,\n\
            }});\n\
        }})()",
        entries_json = pi_js::json::stringify(entries),
        slice = &template_js[start..end],
    );
    let out = vm.with_scope(|_| {
        vm.eval_code(&script, Some("sys.js"), None)
            .expect("eval sys helpers")
            .to_string()
            .expect("string")
    });
    pi_js::json::parse(&out).expect("json")
}

// #11261: system messages were stored in the export but never rendered.
#[tokio::test]
async fn export_html_renders_system_messages_and_updates() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let template_js =
        std::fs::read_to_string(manifest_dir.join("src/core/export_html/template.js")).expect("read template.js");

    let initial_val = json!({
        "role": "system",
        "content": "You are a helpful assistant.",
        "sections": { "rules": "<rules>\nbe concise\n</rules>", "project_context": "<project_context>\nv1\n</project_context>" },
        "toolsAdded": [{ "name": "read", "description": "Read a file", "parameters": { "type": "object", "properties": {} } }],
        "timestamp": 1
    });
    let update_val = json!({
        "role": "system",
        "content": [{ "type": "text", "text": "Extra instructions" }],
        "sections": { "project_context": "<project_context>\nv2\n</project_context>", "rules": null },
        "toolsAdded": [{ "name": "bash", "description": "Run a command", "parameters": { "type": "object", "properties": {} } }],
        "toolsRemoved": [{ "name": "read" }],
        "timestamp": 2
    });
    let initial_msg: SystemMessage = serde_json::from_value(initial_val.clone()).expect("initial system msg");
    let update_msg: SystemMessage = serde_json::from_value(update_val.clone()).expect("update system msg");

    let entries = json!([
        { "type": "message", "id": "s1", "parentId": null, "message": initial_val },
        { "type": "message", "id": "u1", "parentId": "s1", "message": { "role": "user", "content": "hi" } },
        { "type": "message", "id": "s2", "parentId": "u1", "message": update_val }
    ]);

    let out = eval_system_helpers(&template_js, &entries).await;
    assert_eq!(out["textInitial"], get_system_message_text(&initial_msg));
    assert_eq!(out["updateRendered"], render_system_message_update(&update_msg));
    assert_eq!(out["leadingFirst"], true);
    assert_eq!(out["leadingUpdate"], false);
    assert_eq!(
        out["updateDesc"],
        "instructions, project_context, -rules, tools +bash -read"
    );

    let compact_entries = json!([
        { "type": "compaction", "id": "c1", "parentId": null, "systemMessage": initial_val },
        { "type": "message", "id": "s2", "parentId": "c1", "message": update_val }
    ]);
    let compact_out = eval_system_helpers(&template_js, &compact_entries).await;
    assert_eq!(compact_out["leadingUpdate"], false);

    assert!(
        Regex::new(r"if \(msg\.role === 'system'\) \{\s*let html = '';")
            .unwrap()
            .is_match(&template_js)
    );
    assert!(template_js.contains("renderSystemPromptBlock('System Prompt', text)"));
    assert!(template_js.contains("renderToolsList('Available Tools', msg.toolsAdded)"));
}
