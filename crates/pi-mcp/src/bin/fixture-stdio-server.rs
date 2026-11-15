//! Port of packages/mcp/test/fixtures/stdio-server.mjs (fixture process entry)

use std::io::{BufRead, Write};

use serde_json::{Map, Value, json};

fn main() {
    eprintln!("stdio fixture ready");
    let _ = std::io::stderr().flush();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        let Ok(Value::Object(message)) = pi_js::json::parse(&line) else {
            continue;
        };
        let Some(id) = message.get("id").cloned() else {
            continue;
        };
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let mut resp = Map::new();
        resp.insert("jsonrpc".to_string(), Value::String("2.0".to_string()));
        resp.insert("id".to_string(), id);

        match method {
            "initialize" => {
                resp.insert(
                    "result".to_string(),
                    json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "stdio-fixture", "version": "1.0.0" }
                    }),
                );
            }
            "tools/list" => {
                resp.insert(
                    "result".to_string(),
                    json!({
                        "tools": [{ "name": "echo", "inputSchema": { "type": "object" } }]
                    }),
                );
            }
            "tools/call" => {
                let text = message
                    .get("params")
                    .and_then(|p| p.get("arguments"))
                    .and_then(|a| a.get("text"))
                    .map(|v| match v {
                        Value::String(s) => s.clone(),
                        Value::Null => "null".to_string(),
                        Value::Bool(b) => b.to_string(),
                        Value::Number(n) => match n.as_f64() {
                            Some(f) => pi_js::json::number_to_string(f),
                            None => n.to_string(),
                        },
                        other => pi_js::json::stringify(other),
                    })
                    .unwrap_or_else(|| "undefined".to_string());
                resp.insert(
                    "result".to_string(),
                    json!({
                        "content": [{ "type": "text", "text": text }]
                    }),
                );
            }
            "ping" => {
                resp.insert("result".to_string(), json!({}));
            }
            _ => {
                resp.insert("error".to_string(), json!({ "code": -32601, "message": "not found" }));
            }
        }
        let line_out = pi_js::json::stringify(&Value::Object(resp));
        let _ = writeln!(stdout, "{line_out}");
        let _ = stdout.flush();
    }
}
