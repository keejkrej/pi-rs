//! Port of packages/coding-agent/test/mcp-command.test.ts

use std::sync::{Arc, Mutex};

use pi_coding_agent::extensions::mcp::cli::{McpCommandOptions, run_mcp_command};
use serde_json::{Value, json};
use tempfile::TempDir;

const STDIO_FIXTURE_SH: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  case "$line" in
    *'"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"stdio-fixture","version":"1.0.0"}}}\n' "$id"
      ;;
    *'"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}}\n' "$id"
      ;;
    *'"tools/call"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"hello"}]}}\n' "$id"
      ;;
  esac
done
"#;

fn fixture_entry() -> Value {
    json!({
        "command": "/bin/sh",
        "args": ["-c", STDIO_FIXTURE_SH]
    })
}

async fn run_cmd(
    args: &[&str],
    servers: Option<Value>,
    existing_dir: Option<&TempDir>,
) -> (i32, String, Option<TempDir>) {
    let owned_dir = if existing_dir.is_none() {
        Some(tempfile::tempdir().expect("tempdir"))
    } else {
        None
    };
    let dir = existing_dir.unwrap_or_else(|| owned_dir.as_ref().expect("dir"));
    let dir_str = dir.path().to_string_lossy().into_owned();
    if let Some(srvs) = servers {
        let cfg_path = dir.path().join("mcp.json");
        std::fs::write(&cfg_path, pi_js::json::stringify(&json!({ "mcpServers": srvs }))).expect("write mcp.json");
    }
    let output: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let out_log = Arc::clone(&output);
    let out_err = Arc::clone(&output);
    let owned_args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    let code = run_mcp_command(
        &owned_args,
        McpCommandOptions {
            cwd: dir_str.clone(),
            agent_dir: dir_str,
            credentials: None,
            provider_token: None,
            open_url: None,
            log: Some(Arc::new(move |line| {
                out_log.lock().unwrap_or_else(|e| e.into_inner()).push(line.to_string());
            })),
            error: Some(Arc::new(move |line| {
                out_err.lock().unwrap_or_else(|e| e.into_inner()).push(line.to_string());
            })),
        },
    )
    .await
    .expect("run_mcp_command");
    let joined = output.lock().unwrap_or_else(|e| e.into_inner()).join("\n");
    (code, joined, owned_dir)
}

fn read_config(path: &std::path::Path) -> Value {
    let raw = std::fs::read_to_string(path).expect("read config");
    pi_js::json::parse(&raw).expect("parse config json")
}

fn all_servers() -> Value {
    json!({
        "fixture": fixture_entry(),
        "broken": { "command": "pi-test-missing-mcp-server" },
        "parked": { "command": "/bin/sh", "args": ["-c", STDIO_FIXTURE_SH], "enabled": false },
        "bad": { "args": ["no command"] }
    })
}

#[cfg(unix)]
#[tokio::test]
async fn lists_servers_with_state_tools_and_errors_and_fails_while_anything_is_wrong() {
    let (exit_code, output, _dir) = run_cmd(&["list"], Some(all_servers()), None).await;
    assert_eq!(exit_code, 1, "output: {output}");
    assert!(
        output.contains("fixture: connected, 1 tool (codemode, global)\n"),
        "output: {output}"
    );
    assert!(output.contains("  tools: echo"), "output: {output}");
    assert!(
        output.contains("broken: failed (codemode, global)\n  pi-test-missing-mcp-server\n"),
        "output: {output}"
    );
    assert!(
        output.contains("ENOENT") && output.contains("spawn 'pi-test-missing-mcp-server'"),
        "output: {output}"
    );
    assert!(
        output.contains("parked: disabled (codemode, global)"),
        "output: {output}"
    );
    assert!(output.contains("config error: "), "output: {output}");
    assert!(
        output.contains("server \"bad\" needs either \"command\""),
        "output: {output}"
    );

    let (ok_code, ok_out, _ok_dir) = run_cmd(&["list"], Some(json!({ "fixture": fixture_entry() })), None).await;
    assert_eq!(ok_code, 0, "output: {ok_out}");
}

#[cfg(unix)]
#[tokio::test]
async fn prints_json_for_scripts() {
    let (exit_code, output, _dir) = run_cmd(
        &["list", "--json"],
        Some(json!({
            "fixture": fixture_entry(),
            "parked": { "command": "/bin/sh", "args": ["-c", STDIO_FIXTURE_SH], "enabled": false }
        })),
        None,
    )
    .await;
    assert_eq!(exit_code, 0, "output: {output}");
    let parsed = pi_js::json::parse(&output).expect("json output");
    let servers = parsed.get("servers").and_then(Value::as_array).expect("servers array");
    let summary: Vec<Value> = servers
        .iter()
        .map(|s| {
            json!({
                "name": s.get("name").cloned().unwrap_or(Value::Null),
                "state": s.get("state").cloned().unwrap_or(Value::Null),
                "tools": s.get("tools").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            json!({ "name": "fixture", "state": "connected", "tools": ["echo"] }),
            json!({ "name": "parked", "state": "disabled", "tools": [] }),
        ]
    );
}

#[tokio::test]
async fn rejects_unknown_servers_and_servers_without_oauth_for_login_and_logout() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (code, output, _) = run_cmd(&["login", "nope"], Some(all_servers()), Some(&dir)).await;
    assert_eq!(code, 1);
    assert_eq!(
        output,
        "No MCP server named \"nope\". Configured: fixture, broken, parked."
    );

    let (code, output, _) = run_cmd(&["logout", "fixture"], None, Some(&dir)).await;
    assert_eq!(code, 1);
    assert_eq!(
        output,
        "MCP server \"fixture\" does not use OAuth. Only HTTP servers without an Authorization header do."
    );

    let (code, _, _) = run_cmd(&["frobnicate"], None, Some(&dir)).await;
    assert_eq!(code, 1);
}

#[tokio::test]
async fn adds_stdio_servers_and_passes_options_after_command_through() {
    let (code, output, dir_opt) = run_cmd(
        &[
            "add", "--env", "A=1", "--env", "B=x=y", "files", "--", "npx", "-y", "server", "--root", ".",
        ],
        None,
        None,
    )
    .await;
    let dir = dir_opt.expect("dir");
    assert_eq!(code, 0, "output: {output}");
    assert!(output.contains("Added global MCP server \"files\""));
    assert_eq!(
        read_config(&dir.path().join("mcp.json")),
        json!({
            "mcpServers": {
                "files": { "command": "npx", "args": ["-y", "server", "--root", "."], "env": { "A": "1", "B": "x=y" } }
            }
        })
    );

    // Without `--`, options after the command belong to the command too.
    let (rep_code, rep_out, _) = run_cmd(&["add", "files", "node", "server.js", "--port", "1"], None, Some(&dir)).await;
    assert_eq!(rep_code, 0, "output: {rep_out}");
    assert!(rep_out.contains("Replaced global MCP server \"files\""));
    assert_eq!(
        read_config(&dir.path().join("mcp.json")),
        json!({
            "mcpServers": { "files": { "command": "node", "args": ["server.js", "--port", "1"] } }
        })
    );
}

#[tokio::test]
async fn adds_http_servers_and_keeps_other_content_of_file() {
    let (code, output, dir_opt) = run_cmd(
        &[
            "add",
            "docs",
            "--url",
            "https://example.com/mcp",
            "--bearer-token-env-var",
            "DOCS_TOKEN",
            "--header",
            "X-Team=core",
            "--exposure",
            "direct",
            "--description",
            "Product docs",
        ],
        Some(json!({ "fixture": fixture_entry() })),
        None,
    )
    .await;
    let dir = dir_opt.expect("dir");
    assert_eq!(code, 0, "output: {output}");
    assert!(!output.contains("mcp login"));
    assert_eq!(
        read_config(&dir.path().join("mcp.json")),
        json!({
            "mcpServers": {
                "fixture": fixture_entry(),
                "docs": {
                    "url": "https://example.com/mcp",
                    "headers": { "X-Team": "core", "Authorization": "Bearer ${DOCS_TOKEN}" },
                    "exposure": "direct",
                    "description": "Product docs"
                }
            }
        })
    );

    let (oauth_code, oauth_out, _) = run_cmd(
        &[
            "add",
            "sentry",
            "--url",
            "https://mcp.sentry.dev/mcp",
            "--oauth-client-id",
            "pi",
            "--oauth-client-name",
            "Claude Code",
        ],
        None,
        Some(&dir),
    )
    .await;
    assert_eq!(oauth_code, 0, "output: {oauth_out}");
    assert!(oauth_out.contains("mcp login sentry"), "output: {oauth_out}");
    let cfg = read_config(&dir.path().join("mcp.json"));
    assert_eq!(
        cfg.get("mcpServers").and_then(|s| s.get("sentry")),
        Some(&json!({
            "url": "https://mcp.sentry.dev/mcp",
            "oauth": { "clientId": "pi", "clientName": "Claude Code" }
        }))
    );
}

#[tokio::test]
async fn rejects_invalid_add_invocations_without_writing() {
    let cases: &[&[&str]] = &[
        &["add", "x"],
        &["add", "x", "--url", "https://example.com", "--", "cmd"],
        &["add", "bad name", "--", "cmd"],
        &["add", "x", "--url", "ftp://example.com"],
        &["add", "x", "--env", "A=1", "--url", "https://example.com"],
        &["add", "x", "--header", "A=1", "--", "cmd"],
        &["add", "x", "--env", "NOVALUE", "--", "cmd"],
        &["add", "x", "--exposure", "loud", "--", "cmd"],
    ];
    for args in cases {
        let (code, output, dir_opt) = run_cmd(args, None, None).await;
        let dir = dir_opt.expect("dir");
        assert_eq!(code, 1, "args {:?} output: {output}", args);
        assert!(
            !dir.path().join("mcp.json").exists(),
            "args {:?} unexpectedly wrote mcp.json",
            args
        );
    }
}

#[tokio::test]
async fn adds_and_removes_project_servers() {
    let (code, output, dir_opt) = run_cmd(&["add", "-l", "local", "--", "node", "server.js"], None, None).await;
    let dir = dir_opt.expect("dir");
    assert_eq!(code, 0, "output: {output}");
    assert!(output.contains("The project is not trusted"), "output: {output}");

    let project_cfg = dir.path().join(".pi").join("mcp.json");
    assert_eq!(
        read_config(&project_cfg),
        json!({ "mcpServers": { "local": { "command": "node", "args": ["server.js"] } } })
    );

    let (wrong_code, wrong_out, _) = run_cmd(&["remove", "local"], None, Some(&dir)).await;
    assert_eq!(wrong_code, 1);
    assert!(
        wrong_out.contains(&format!("It is defined in {}; use --local.", project_cfg.display())),
        "output: {wrong_out}"
    );

    let (rem_code, rem_out, _) = run_cmd(&["remove", "local", "--local"], None, Some(&dir)).await;
    assert_eq!(rem_code, 0, "output: {rem_out}");
    assert!(rem_out.contains("Removed project MCP server \"local\""));
    assert_eq!(read_config(&project_cfg), json!({ "mcpServers": {} }));
}

#[tokio::test]
async fn removes_global_servers() {
    let (code, _, dir_opt) = run_cmd(&["remove", "broken"], Some(all_servers()), None).await;
    let dir = dir_opt.expect("dir");
    assert_eq!(code, 0);
    let keys: Vec<String> = read_config(&dir.path().join("mcp.json"))
        .get("mcpServers")
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    assert_eq!(keys, vec!["fixture", "parked", "bad"]);

    let (miss_code, miss_out, _) = run_cmd(&["remove", "broken"], None, Some(&dir)).await;
    assert_eq!(miss_code, 1);
    assert!(miss_out.contains("No global MCP server named \"broken\""));
}
