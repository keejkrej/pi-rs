//! Port of packages/coding-agent/test/mcp-extension.test.ts (+ bdd9f0399 BOM regression)

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use indexmap::{IndexMap, IndexSet};
use pi_ai::UserContentBlock;
use pi_coding_agent::core::tools::truncate::truncate_middle;
use pi_coding_agent::extensions::mcp::config::{
    LoadMcpConfigOptions, McpExposure, McpHttpServerConfig, McpOAuthConfig, McpServerConfig, McpServerEntry,
    McpStdioServerConfig, add_mcp_server_config, get_mcp_tool_exposure, load_mcp_config,
};
use pi_coding_agent::extensions::mcp::log::McpServerLog;
use pi_coding_agent::extensions::mcp::oauth::McpOAuthCredentialStore;
use pi_coding_agent::extensions::mcp::resources::McpResourceServer;
use pi_coding_agent::extensions::mcp::runtime::{
    CreatedTransport, McpServerConnection, McpServerConnectionOptions, ServerState, create_default_transport,
};
use pi_coding_agent::extensions::mcp::tools::{
    ConvertMcpResultOptions, McpOutputSaver, McpToolCaller, convert_mcp_result, create_mcp_tool_name,
};
use pi_coding_agent::extensions::mcp::{
    MAX_SERVERS_SECTION_CHARS, McpServerListing, McpServerListingConnection, render_servers_section,
};
// WIRE(@mcp-w3): switch to crate::core::auth_storage::InMemoryAuthStorageBackend once landed
use pi_coding_agent::extensions::wire::InMemoryAuthStorageBackend;
use pi_js::fetch::{Headers, Response as JsResponse};
use pi_js::{Error, Unsubscribe};
use pi_mcp::protocol::content::{
    AudioContent, AudioContentType, BlobResourceContents, ContentBlock, EmbeddedResourceContent,
    EmbeddedResourceContentType, ResourceContents, ResourceLinkContent, ResourceLinkContentType, TextContent,
    TextContentType, TextResourceContents,
};
use pi_mcp::protocol::jsonrpc::{JsonRpcErrorObject, JsonRpcErrorResponse, JsonRpcSuccessResponse, JsonRpcVersion};
use pi_mcp::testing::{InMemoryTransport, create_in_memory_transport_pair};
use pi_mcp::transports::transport::{
    McpTransport, McpTransportCloseListener, McpTransportErrorListener, McpTransportMessageListener,
};
use pi_mcp::{
    CallToolResult, JsonRpcMessage, LATEST_PROTOCOL_VERSION, McpAuthRequiredError, McpHttpError, McpRequestOptions,
    McpSessionExpiredError,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::watch;

const TOKEN_HEADER: &str = "Bearer ${TOKEN}";

fn stdio_cfg(command: &str) -> McpStdioServerConfig {
    McpStdioServerConfig {
        exposure: None,
        description: None,
        tool_exposure: None,
        enabled: None,
        timeout: None,
        r#type: None,
        command: command.to_string(),
        args: None,
        env: None,
        cwd: None,
    }
}

fn http_cfg(url: &str) -> McpHttpServerConfig {
    McpHttpServerConfig {
        exposure: None,
        description: None,
        tool_exposure: None,
        enabled: None,
        timeout: None,
        r#type: None,
        url: url.to_string(),
        headers: None,
        oauth: None,
    }
}

#[cfg(unix)]
static HOME_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct EnvVarGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, val: &str) -> Self {
        let prev = pi_js::env::var(key);
        pi_js::env::set_var(key, val);
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => pi_js::env::set_var(self.key, v),
            None => pi_js::env::remove_var(self.key),
        }
    }
}

fn setup_dirs(global: Value, project: Value) -> (TempDir, String, String) {
    let root = tempfile::tempdir().expect("tempdir");
    let agent_dir = root.path().join("agent");
    let cwd = root.path().join("project");
    std::fs::create_dir_all(&agent_dir).expect("mkdir agent");
    std::fs::create_dir_all(cwd.join(".pi")).expect("mkdir .pi");
    std::fs::write(agent_dir.join("mcp.json"), pi_js::json::stringify(&global)).expect("write global");
    std::fs::write(cwd.join(".pi").join("mcp.json"), pi_js::json::stringify(&project)).expect("write project");
    (
        root,
        agent_dir.to_string_lossy().into_owned(),
        cwd.to_string_lossy().into_owned(),
    )
}

#[test]
fn merges_global_and_trusted_project_servers_and_validates_entries() {
    let (_dir, agent_dir, cwd) = setup_dirs(
        json!({
            "mcpServers": {
                "shared": { "command": "global-cmd" },
                "remote": { "url": "https://example.com/mcp", "headers": { "Authorization": TOKEN_HEADER } },
                "off": { "command": "x", "enabled": false },
                "bad": { "args": ["no command"] },
                "legacy": { "type": "sse", "url": "https://example.com/sse" },
                "badUrl": { "url": "example.com/mcp" },
                "bad name": { "command": "x" }
            }
        }),
        json!({
            "mcpServers": {
                "shared": { "command": "project-cmd", "exposure": "direct" }
            }
        }),
    );

    let trusted = load_mcp_config(&LoadMcpConfigOptions {
        agent_dir: agent_dir.clone(),
        cwd: cwd.clone(),
        project_trusted: true,
    });
    assert_eq!(trusted.servers.len(), 3);
    assert_eq!(trusted.servers[0].name, "shared");
    assert_eq!(
        trusted.servers[0].scope,
        Some(pi_coding_agent::extensions::mcp::config::McpScope::Project)
    );
    assert_eq!(trusted.servers[1].name, "remote");
    assert_eq!(trusted.servers[2].name, "off");
    assert_eq!(trusted.errors.len(), 4);
    assert!(trusted.errors[0].contains("server \"bad\" needs either \"command\""));
    assert!(trusted.errors[1].contains("legacy SSE transport is not supported"));
    assert!(trusted.errors[2].contains("server \"badUrl\": url must be an http or https URL"));
    assert!(trusted.errors[3].contains("invalid server name \"bad name\""));

    let untrusted = load_mcp_config(&LoadMcpConfigOptions {
        agent_dir,
        cwd,
        project_trusted: false,
    });
    match &untrusted
        .servers
        .iter()
        .find(|s| s.name == "shared")
        .expect("shared")
        .config
    {
        McpServerConfig::Stdio(s) => assert_eq!(s.command, "global-cmd"),
        _ => panic!("expected stdio"),
    }
}

// Regression: #11191 / bdd9f0399
#[test]
fn reads_and_edits_mcp_json_files_with_utf8_bom() {
    let (_dir, agent_dir, cwd) = setup_dirs(json!({}), json!({}));
    let global_path = std::path::Path::new(&agent_dir).join("mcp.json");
    let project_path = std::path::Path::new(&cwd).join(".pi").join("mcp.json");
    std::fs::write(
        &global_path,
        "\u{FEFF}{\"mcpServers\":{\"demo\":{\"command\":\"echo\",\"enabled\":false}}}\n",
    )
    .expect("write bom global");
    std::fs::write(
        &project_path,
        "\u{FEFF}{\"mcpServers\":{\"local\":{\"command\":\"local-cmd\"}}}\n",
    )
    .expect("write bom project");

    let loaded = load_mcp_config(&LoadMcpConfigOptions {
        agent_dir,
        cwd,
        project_trusted: true,
    });
    assert!(loaded.errors.is_empty(), "errors: {:?}", loaded.errors);
    let pairs: Vec<_> = loaded.servers.iter().map(|s| (s.name.as_str(), s.scope)).collect();
    assert_eq!(
        pairs,
        vec![
            ("demo", Some(pi_coding_agent::extensions::mcp::config::McpScope::Global)),
            (
                "local",
                Some(pi_coding_agent::extensions::mcp::config::McpScope::Project)
            ),
        ]
    );

    let replaced = add_mcp_server_config(
        &global_path.to_string_lossy(),
        "other",
        &McpServerConfig::Stdio(stdio_cfg("other-cmd")),
        None,
    )
    .expect("add_mcp_server_config");
    assert!(!replaced);
    let parsed = pi_js::json::parse(&std::fs::read_to_string(&global_path).expect("read")).expect("json");
    assert_eq!(
        parsed.get("mcpServers"),
        Some(&json!({
            "demo": { "command": "echo", "enabled": false },
            "other": { "command": "other-cmd" }
        }))
    );
}

// Regression: #10239
#[test]
fn rejects_server_names_that_differ_only_in_dash_and_underscore() {
    let (_dir, agent_dir, cwd) = setup_dirs(
        json!({ "mcpServers": { "work-files": { "command": "a" }, "work_files": { "command": "b" } } }),
        json!({}),
    );
    let loaded = load_mcp_config(&LoadMcpConfigOptions {
        agent_dir,
        cwd,
        project_trusted: false,
    });
    let names: Vec<_> = loaded.servers.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["work-files"]);
    assert_eq!(loaded.errors.len(), 1);
    assert!(loaded.errors[0].contains("server \"work_files\" conflicts with \"work-files\""));
}

#[test]
fn validates_exposure_and_auto_enable_codemode_with_project_precedence() {
    let (_dir, agent_dir, cwd) = setup_dirs(
        json!({
            "autoEnableCodemode": false,
            "mcpServers": {
                "later": { "command": "x", "exposure": "deferred" },
                "scripts": { "command": "x", "exposure": "codemode-deferred", "toolExposure": { "a": "codemode-deferred" } },
                "off": { "command": "x", "exposure": "hidden" },
                "wrong": { "command": "x", "exposure": "model-only" },
                "described": { "command": "x", "description": "Docs search" },
                "badDescription": { "command": "x", "description": 1 }
            }
        }),
        json!({ "autoEnableCodemode": "yes", "mcpServers": {} }),
    );

    let untrusted = load_mcp_config(&LoadMcpConfigOptions {
        agent_dir: agent_dir.clone(),
        cwd: cwd.clone(),
        project_trusted: false,
    });
    assert_eq!(untrusted.auto_enable_codemode, Some(false));
    let exposures: Vec<_> = untrusted
        .servers
        .iter()
        .map(|s| match &s.config {
            McpServerConfig::Stdio(st) => (s.name.as_str(), st.exposure),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(
        exposures,
        vec![
            ("later", Some(McpExposure::Deferred)),
            ("scripts", Some(McpExposure::Codemode)),
            ("off", Some(McpExposure::Hidden)),
            ("described", None),
        ]
    );
    assert_eq!(untrusted.errors.len(), 2);
    assert!(untrusted.errors[0].contains("server \"wrong\": exposure must be one of"));
    assert!(untrusted.errors[1].contains("server \"badDescription\": description must be a string"));

    let trusted = load_mcp_config(&LoadMcpConfigOptions {
        agent_dir,
        cwd,
        project_trusted: true,
    });
    assert_eq!(trusted.auto_enable_codemode, Some(false));
    assert!(
        trusted
            .errors
            .iter()
            .any(|e| e.contains("autoEnableCodemode must be a boolean"))
    );
}

#[test]
fn validates_oauth_callback_url_scope_and_client_name() {
    let (_dir, agent_dir, cwd) = setup_dirs(
        json!({
            "mcpServers": {
                "ok": { "url": "https://a.example/mcp", "oauth": { "callbackUrl": "http://localhost:8080/callback", "scope": "a b" } },
                "ipv6": { "url": "https://a.example/mcp", "oauth": { "callbackUrl": "http://[::1]/cb", "callbackPort": 9000 } },
                "same": { "url": "https://a.example/mcp", "oauth": { "callbackUrl": "http://127.0.0.1:2/cb", "callbackPort": 2 } },
                "remote": { "url": "https://a.example/mcp", "oauth": { "callbackUrl": "https://example.com/callback" } },
                "both": { "url": "https://a.example/mcp", "oauth": { "callbackUrl": "http://127.0.0.1:1/cb", "callbackPort": 2 } },
                "scope": { "url": "https://a.example/mcp", "oauth": { "scope": ["a"] } },
                "named": { "url": "https://a.example/mcp", "oauth": { "clientName": "Claude Code" } },
                "unnamed": { "url": "https://a.example/mcp", "oauth": { "clientName": " " } },
                "metadata": { "url": "https://a.example/mcp", "oauth": { "authServerMetadataUrl": "https://idp.example/m" } },
                "plainMetadata": { "url": "https://a.example/mcp", "oauth": { "authServerMetadataUrl": "http://idp.example/m" } }
            }
        }),
        json!({}),
    );
    let loaded = load_mcp_config(&LoadMcpConfigOptions {
        agent_dir,
        cwd,
        project_trusted: false,
    });
    let names: Vec<_> = loaded.servers.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["ok", "ipv6", "same", "named", "metadata"]);
    assert_eq!(loaded.errors.len(), 5);
    assert!(loaded.errors[0].contains("server \"remote\": oauth.callbackUrl must be an http URI on localhost"));
    assert!(
        loaded.errors[1].contains("server \"both\": oauth.callbackUrl and oauth.callbackPort name different ports")
    );
    assert!(loaded.errors[2].contains("server \"scope\": oauth.scope must be a string"));
    assert!(loaded.errors[3].contains("server \"unnamed\": oauth.clientName must be a non-empty string"));
    assert!(loaded.errors[4].contains("server \"plainMetadata\": oauth.authServerMetadataUrl must be an https URL"));
}

#[test]
fn resolves_per_tool_exposure_from_exact_names_then_patterns_in_order() {
    let (_dir, agent_dir, cwd) = setup_dirs(
        json!({
            "mcpServers": {
                "gh": {
                    "command": "x",
                    "exposure": "deferred",
                    "toolExposure": { "get_*": "codemode", "get_me": "direct", "*delete*": "hidden", "get_file.*": "direct" }
                },
                "bad": { "command": "x", "toolExposure": { "a": "visible" } }
            }
        }),
        json!({}),
    );
    let loaded = load_mcp_config(&LoadMcpConfigOptions {
        agent_dir,
        cwd,
        project_trusted: false,
    });
    assert_eq!(loaded.errors.len(), 1);
    assert!(loaded.errors[0].contains("server \"bad\": toolExposure \"a\" must be one of"));
    let cfg = &loaded.servers[0].config;
    assert_eq!(get_mcp_tool_exposure(cfg, "get_me"), McpExposure::Direct);
    assert_eq!(get_mcp_tool_exposure(cfg, "get_issue"), McpExposure::Codemode);
    assert_eq!(get_mcp_tool_exposure(cfg, "get_delete_hint"), McpExposure::Codemode);
    assert_eq!(get_mcp_tool_exposure(cfg, "delete_repo"), McpExposure::Hidden);
    assert_eq!(get_mcp_tool_exposure(cfg, "list_issues"), McpExposure::Deferred);

    let mut dot_map = IndexMap::new();
    dot_map.insert("get_file.*".to_string(), McpExposure::Direct);
    let mut dot_stdio = stdio_cfg("x");
    dot_stdio.tool_exposure = Some(dot_map);
    let dot_cfg = McpServerConfig::Stdio(dot_stdio);
    assert_eq!(get_mcp_tool_exposure(&dot_cfg, "get_file_x"), McpExposure::Codemode);
}

#[test]
fn validates_provider_auth_and_accepts_it_only_in_global_mcp_json() {
    let (_dir, agent_dir, cwd) = setup_dirs(
        json!({
            "mcpServers": {
                "radius": { "url": "https://radius.example/mcp", "auth": { "provider": "radius" } },
                "local": { "url": "http://localhost:8788/mcp", "auth": { "provider": "radius-dev" } },
                "plain": { "url": "http://radius.example/mcp", "auth": { "provider": "radius" } },
                "empty": { "url": "https://radius.example/mcp", "auth": { "provider": "" } }
            }
        }),
        json!({
            "mcpServers": {
                "radius": { "url": "https://evil.example/mcp", "auth": { "provider": "radius" } }
            }
        }),
    );
    let loaded = load_mcp_config(&LoadMcpConfigOptions {
        agent_dir,
        cwd,
        project_trusted: true,
    });
    let urls: Vec<_> = loaded
        .servers
        .iter()
        .map(|s| match &s.config {
            McpServerConfig::Http(h) => (s.name.as_str(), s.scope, h.url.as_str()),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(
        urls,
        vec![
            (
                "radius",
                Some(pi_coding_agent::extensions::mcp::config::McpScope::Global),
                "https://radius.example/mcp"
            ),
            (
                "local",
                Some(pi_coding_agent::extensions::mcp::config::McpScope::Global),
                "http://localhost:8788/mcp"
            ),
        ]
    );
    assert_eq!(loaded.errors.len(), 3);
    assert!(loaded.errors[0].contains("server \"plain\": auth requires an https URL"));
    assert!(loaded.errors[1].contains("server \"empty\": auth.provider must be a provider name"));
    assert!(loaded.errors[2].contains("server \"radius\": auth is only allowed in the global mcp.json"));
}

#[test]
fn creates_provider_safe_tool_names() {
    assert_eq!(create_mcp_tool_name("docs", "search", |_| false), "mcp__docs__search");
    assert_eq!(
        create_mcp_tool_name("my-server", "get.item/v2", |_| false),
        "mcp__my_server__get_item_v2"
    );
    let long = create_mcp_tool_name("server", &"x".repeat(100), |_| false);
    assert_eq!(long.len(), 64);
    assert!(long.starts_with("mcp__server__x"));
    assert_ne!(
        create_mcp_tool_name("server", &format!("{}y", "x".repeat(100)), |_| false),
        long
    );
    let taken = create_mcp_tool_name("s", "a_b", |_| false);
    let second = create_mcp_tool_name("s", "a-b", |name| name == taken);
    assert!(second.starts_with("mcp__s__a_b_") && second.len() == "mcp__s__a_b_".len() + 8);
}

#[tokio::test]
async fn converts_results_and_flags_errors() {
    let blocks = vec![
        ContentBlock::ResourceLink(ResourceLinkContent {
            r#type: ResourceLinkContentType::ResourceLink,
            uri: "file:///a".to_string(),
            name: "a".to_string(),
            title: None,
            description: None,
            mime_type: None,
            size: None,
            annotations: None,
            meta: None,
        }),
        ContentBlock::Resource(EmbeddedResourceContent {
            r#type: EmbeddedResourceContentType::Resource,
            resource: ResourceContents::Text(TextResourceContents {
                uri: "file:///b".to_string(),
                text: "b text".to_string(),
                mime_type: None,
                meta: None,
            }),
            annotations: None,
            meta: None,
        }),
        ContentBlock::Audio(AudioContent {
            r#type: AudioContentType::Audio,
            data: String::new(),
            mime_type: "audio/wav".to_string(),
            annotations: None,
            meta: None,
        }),
    ];
    let mut structured = IndexMap::new();
    structured.insert("ok".to_string(), Value::Bool(true));
    let mut meta = IndexMap::new();
    meta.insert("trace".to_string(), Value::String("x".to_string()));
    let res1 = convert_mcp_result(
        "docs",
        "t",
        CallToolResult {
            content: blocks,
            structured_content: Some(structured),
            is_error: None,
            meta: Some(meta),
        },
        &ConvertMcpResultOptions::default(),
    )
    .await;
    let texts: Vec<_> = res1
        .content
        .iter()
        .filter_map(|b| match b {
            UserContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        texts,
        vec!["[Resource file:///a \"a\"]", "b text", "[audio audio/wav omitted]"]
    );
    assert_eq!(res1.details, Some(json!({ "server": "docs", "tool": "t" })));
    assert!(
        res1.structured_content
            .as_ref()
            .and_then(|v| v.as_object())
            .is_some_and(|o| !o.contains_key("_meta") && o.contains_key("structuredContent"))
    );

    let mut sc2 = IndexMap::new();
    sc2.insert("n".to_string(), json!(1));
    let res2 = convert_mcp_result(
        "docs",
        "t",
        CallToolResult {
            content: Vec::new(),
            structured_content: Some(sc2),
            is_error: None,
            meta: None,
        },
        &ConvertMcpResultOptions::default(),
    )
    .await;
    assert_eq!(
        res2.content,
        vec![UserContentBlock::Text(pi_ai::TextContent::new("{\n  \"n\": 1\n}"))]
    );

    let err_res = convert_mcp_result(
        "docs",
        "t",
        CallToolResult {
            content: vec![ContentBlock::Text(pi_mcp::protocol::content::TextContent {
                r#type: TextContentType::Text,
                text: "nope".to_string(),
                annotations: None,
                meta: None,
            })],
            structured_content: None,
            is_error: Some(true),
            meta: None,
        },
        &ConvertMcpResultOptions::default(),
    )
    .await;
    assert_eq!(err_res.is_error, Some(true));
    assert_eq!(
        err_res.content,
        vec![UserContentBlock::Text(pi_ai::TextContent::new("nope"))]
    );

    let empty_err = convert_mcp_result(
        "docs",
        "t",
        CallToolResult {
            content: Vec::new(),
            structured_content: None,
            is_error: Some(true),
            meta: None,
        },
        &ConvertMcpResultOptions::default(),
    )
    .await;
    assert_eq!(
        empty_err.content,
        vec![UserContentBlock::Text(pi_ai::TextContent::new(
            "MCP tool docs/t returned an error"
        ))]
    );
}

#[tokio::test]
async fn points_resource_links_to_read_mcp_resource_and_saves_binary_resources() {
    let saved: Arc<Mutex<Vec<(Vec<u8>, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let saved_clone = Arc::clone(&saved);
    let saver: McpOutputSaver = Arc::new(move |data, ext| {
        let saved_clone = Arc::clone(&saved_clone);
        Box::pin(async move {
            saved_clone
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((data, ext.clone()));
            Ok(format!("/tmp/saved{ext}"))
        })
    });
    let converted = convert_mcp_result(
        "docs",
        "t",
        CallToolResult {
            content: vec![
                ContentBlock::ResourceLink(ResourceLinkContent {
                    r#type: ResourceLinkContentType::ResourceLink,
                    uri: "docs://guide".to_string(),
                    name: "guide".to_string(),
                    title: Some("The Guide".to_string()),
                    mime_type: Some("text/markdown".to_string()),
                    size: Some(2048),
                    description: Some("How to use it".to_string()),
                    annotations: None,
                    meta: None,
                }),
                ContentBlock::Resource(EmbeddedResourceContent {
                    r#type: EmbeddedResourceContentType::Resource,
                    resource: ResourceContents::Blob(BlobResourceContents {
                        uri: "file:///r/report.pdf".to_string(),
                        mime_type: Some("application/pdf".to_string()),
                        blob: "JVBERg==".to_string(),
                        meta: None,
                    }),
                    annotations: None,
                    meta: None,
                }),
                ContentBlock::Resource(EmbeddedResourceContent {
                    r#type: EmbeddedResourceContentType::Resource,
                    resource: ResourceContents::Blob(BlobResourceContents {
                        uri: "docs://logo".to_string(),
                        mime_type: Some("image/png".to_string()),
                        blob: "AAAA".to_string(),
                        meta: None,
                    }),
                    annotations: None,
                    meta: None,
                }),
            ],
            structured_content: None,
            is_error: None,
            meta: None,
        },
        &ConvertMcpResultOptions {
            save_output: Some(saver),
            readable_resources: true,
        },
    )
    .await;

    assert_eq!(converted.content.len(), 3);
    assert_eq!(
        converted.content[0],
        UserContentBlock::Text(pi_ai::TextContent::new(
            "[Resource docs://guide \"The Guide\" (text/markdown, 2.0KB): How to use it. Read it with read_mcp_resource (server \"docs\")]"
        ))
    );
    assert_eq!(
        converted.content[1],
        UserContentBlock::Text(pi_ai::TextContent::new(
            "[Binary resource file:///r/report.pdf (application/pdf, 4B) saved to /tmp/saved.pdf]"
        ))
    );
    assert_eq!(
        converted.content[2],
        UserContentBlock::Image(pi_ai::ImageContent::new("AAAA", "image/png"))
    );
    assert_eq!(
        *saved.lock().unwrap_or_else(|e| e.into_inner()),
        vec![(b"%PDF".to_vec(), ".pdf".to_string())]
    );
}

#[tokio::test]
async fn cuts_middle_of_model_facing_text_over_20kb_and_keeps_full_result_for_scripts() {
    let saved: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let saved_clone = Arc::clone(&saved);
    let saver: McpOutputSaver = Arc::new(move |data, _| {
        let saved_clone = Arc::clone(&saved_clone);
        Box::pin(async move {
            saved_clone.lock().unwrap_or_else(|e| e.into_inner()).push(data);
            Ok("/tmp/full.txt".to_string())
        })
    });
    let full: String = (1..=3000).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
    let result = CallToolResult {
        content: vec![
            ContentBlock::Text(TextContent {
                r#type: TextContentType::Text,
                text: full.clone(),
                annotations: None,
                meta: None,
            }),
            ContentBlock::Image(pi_mcp::protocol::content::ImageContent {
                r#type: pi_mcp::protocol::content::ImageContentType::Image,
                data: "AAAA".to_string(),
                mime_type: "image/png".to_string(),
                annotations: None,
                meta: None,
            }),
        ],
        structured_content: None,
        is_error: None,
        meta: None,
    };
    let converted = convert_mcp_result(
        "docs",
        "snapshot",
        result,
        &ConvertMcpResultOptions {
            save_output: Some(Arc::clone(&saver)),
            readable_resources: false,
        },
    )
    .await;
    assert_eq!(converted.content.len(), 2);
    let text = match &converted.content[0] {
        UserContentBlock::Text(t) => &t.text,
        _ => panic!("expected text"),
    };
    let expected_prefix = format!(
        "Warning: truncated output (original token count: {})\nTotal output lines: 3000\n\nline 1\nline 2\n",
        full.len().div_ceil(4)
    );
    assert!(text.starts_with(&expected_prefix), "text: {text}");
    assert!(text.contains("chars truncated…"));
    assert!(text.ends_with("line 3000\n\n[Full output: /tmp/full.txt (read it with offset/limit)]"));
    assert!(text.len() < 21 * 1024);
    assert_eq!(
        converted.details,
        Some(json!({ "server": "docs", "tool": "snapshot", "fullOutputPath": "/tmp/full.txt" }))
    );
    assert_eq!(saved.lock().unwrap_or_else(|e| e.into_inner()).len(), 1);
}

#[test]
fn cuts_multi_byte_text_only_at_character_boundaries() {
    let text = format!("{}end", "é".repeat(20_000));
    let result = truncate_middle(&text, 1001);
    assert!(result.truncated);
    assert!(!result.content.contains('\u{FFFD}'));
    assert!(result.content.ends_with("end"));
}

type InterceptSendFn =
    Arc<dyn Fn(&InMemoryTransport, JsonRpcMessage) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>;

struct InterceptTransport {
    inner: Arc<InMemoryTransport>,
    send_fn: Option<InterceptSendFn>,
}

#[async_trait]
impl McpTransport for InterceptTransport {
    async fn start(&self) -> pi_js::Result<()> {
        self.inner.start().await
    }
    async fn send(&self, message: JsonRpcMessage) -> pi_js::Result<()> {
        if let Some(ref f) = self.send_fn {
            return f(&self.inner, message).await;
        }
        self.inner.send(message).await
    }
    async fn close(&self) -> pi_js::Result<()> {
        self.inner.close().await
    }
    fn on_message(&self, listener: McpTransportMessageListener) -> Unsubscribe {
        self.inner.on_message(listener)
    }
    fn on_error(&self, listener: McpTransportErrorListener) -> Unsubscribe {
        self.inner.on_error(listener)
    }
    fn on_close(&self, listener: McpTransportCloseListener) -> Unsubscribe {
        self.inner.on_close(listener)
    }
    fn set_protocol_version(&self, version: &str) {
        self.inner.set_protocol_version(version);
    }
}

fn spawn_mock_transport(
    servers: &Arc<Mutex<Vec<Arc<InMemoryTransport>>>>,
    methods: Option<Arc<Mutex<Vec<String>>>>,
    no_tools: bool,
    send_fn: Option<InterceptSendFn>,
) -> Arc<dyn McpTransport> {
    let pair = create_in_memory_transport_pair();
    let client = Arc::new(pair.client);
    let server = Arc::new(pair.server);
    servers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Arc::clone(&server));

    let srv_cb = Arc::clone(&server);
    let _ = server.on_message(Arc::new(move |msg| {
        let JsonRpcMessage::Request(req) = msg else {
            return;
        };
        let req = req.clone();
        if let Some(ref m) = methods {
            m.lock().unwrap_or_else(|e| e.into_inner()).push(req.method.clone());
        }
        let srv = Arc::clone(&srv_cb);
        tokio::spawn(async move {
            let reply = match req.method.as_str() {
                "initialize" => JsonRpcMessage::Response(pi_mcp::JsonRpcResponse::Success(JsonRpcSuccessResponse {
                    jsonrpc: JsonRpcVersion::V2,
                    id: req.id,
                    result: json!({
                        "protocolVersion": LATEST_PROTOCOL_VERSION,
                        "capabilities": if no_tools { json!({ "prompts": {} }) } else { json!({ "tools": {} }) },
                        "serverInfo": { "name": "fake", "version": "1.0.0" }
                    }),
                })),
                "tools/list" => {
                    if no_tools {
                        JsonRpcMessage::Response(pi_mcp::JsonRpcResponse::Error(JsonRpcErrorResponse {
                            jsonrpc: JsonRpcVersion::V2,
                            id: req.id,
                            error: JsonRpcErrorObject {
                                code: -32601,
                                message: "Method not found".to_string(),
                                data: None,
                            },
                        }))
                    } else {
                        JsonRpcMessage::Response(pi_mcp::JsonRpcResponse::Success(JsonRpcSuccessResponse {
                            jsonrpc: JsonRpcVersion::V2,
                            id: req.id,
                            result: json!({ "tools": [] }),
                        }))
                    }
                }
                "resources/read" => {
                    JsonRpcMessage::Response(pi_mcp::JsonRpcResponse::Success(JsonRpcSuccessResponse {
                        jsonrpc: JsonRpcVersion::V2,
                        id: req.id,
                        result: json!({ "contents": [{ "uri": "docs://a", "text": "ok" }] }),
                    }))
                }
                _ => JsonRpcMessage::Response(pi_mcp::JsonRpcResponse::Success(JsonRpcSuccessResponse {
                    jsonrpc: JsonRpcVersion::V2,
                    id: req.id,
                    result: json!({ "content": [{ "type": "text", "text": "ok" }] }),
                })),
            };
            let _ = srv.send(reply).await;
        });
    }));

    let srv_start = Arc::clone(&server);
    tokio::spawn(async move {
        let _ = srv_start.start().await;
    });

    Arc::new(InterceptTransport { inner: client, send_fn })
}

fn connect_with(
    entry: McpServerEntry,
    transports: Vec<Arc<dyn McpTransport>>,
    log: Option<Arc<McpServerLog>>,
) -> (Arc<McpServerConnection>, Arc<Mutex<usize>>) {
    let opened = Arc::new(Mutex::new(0usize));
    let opened_cb = Arc::clone(&opened);
    let list = Mutex::new(transports.into_iter());
    let conn = McpServerConnection::new(McpServerConnectionOptions {
        entry,
        cwd: pi_js::env::cwd(),
        create_transport: Arc::new(move |_, _, _| {
            *opened_cb.lock().unwrap_or_else(|e| e.into_inner()) += 1;
            let t = list
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .next()
                .ok_or_else(|| Error::msg("no more test transports"))?;
            Ok(CreatedTransport::Custom(t))
        }),
        credentials: McpOAuthCredentialStore::new(Some(Arc::new(InMemoryAuthStorageBackend::new())), None),
        provider_token: None,
        on_tools: Arc::new(|_| {}),
        on_change: None,
        log,
    });
    (conn, opened)
}

#[tokio::test]
async fn starts_new_session_and_retries_once_when_session_expired() {
    let servers = Arc::new(Mutex::new(Vec::new()));
    let t1 = spawn_mock_transport(
        &servers,
        None,
        false,
        Some(Arc::new(|inner, msg| {
            let inner = inner.clone();
            Box::pin(async move {
                if let JsonRpcMessage::Request(ref r) = msg
                    && r.method == "tools/call"
                {
                    return Err(Error::typed(
                        "McpSessionExpiredError",
                        McpSessionExpiredError::new(Some("gone".to_string())),
                    ));
                }
                inner.send(msg).await
            })
        })),
    );
    let t2 = spawn_mock_transport(&servers, None, false, None);
    let (conn, opened) = connect_with(
        McpServerEntry {
            name: "fake".to_string(),
            config: McpServerConfig::Stdio(stdio_cfg("unused")),
            auth_provider: None,
            source: "test".to_string(),
            scope: None,
        },
        vec![t1, t2],
        None,
    );
    let (r1, r2) = tokio::join!(conn.call_tool("echo", None, None), conn.call_tool("echo", None, None));
    assert_eq!(r1.expect("r1").content.len(), 1);
    assert_eq!(r2.expect("r2").content.len(), 1);
    assert_eq!(*opened.lock().unwrap_or_else(|e| e.into_inner()), 2);
    conn.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn expands_tilde_in_command_arguments_and_cwd_of_stdio_servers() {
    let _lock = HOME_LOCK.lock().await;
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(home.path().join("work")).expect("mkdir work");
    let script_path = home.path().join("server.sh");
    std::fs::write(
        &script_path,
        r#"
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  case "$line" in
    *'"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"cwd","version":"1"}}}\n' "$id"
      ;;
    *'"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"cwd","inputSchema":{"type":"object"}}]}}\n' "$id"
      ;;
    *)
      wd=$(pwd)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"%s"}]}}\n' "$id" "$wd"
      ;;
  esac
done
"#,
    )
    .expect("write server.sh");
    let home_str = home.path().to_string_lossy().into_owned();
    let _guard = EnvVarGuard::set("HOME", &home_str);

    let conn = McpServerConnection::new(McpServerConnectionOptions {
        entry: McpServerEntry {
            name: "home".to_string(),
            config: McpServerConfig::Stdio({
                let mut s = stdio_cfg("/bin/sh");
                s.args = Some(vec!["~/server.sh".to_string()]);
                s.cwd = Some("~/work".to_string());
                s
            }),
            auth_provider: None,
            source: "test".to_string(),
            scope: None,
        },
        cwd: std::env::temp_dir().to_string_lossy().into_owned(),
        create_transport: Arc::new(create_default_transport),
        credentials: McpOAuthCredentialStore::new(Some(Arc::new(InMemoryAuthStorageBackend::new())), None),
        provider_token: None,
        on_tools: Arc::new(|_| {}),
        on_change: None,
        log: None,
    });
    let res = conn.call_tool("cwd", None, None).await.expect("call_tool");
    let reported = match &res.content[0] {
        ContentBlock::Text(t) => &t.text,
        _ => panic!("expected text"),
    };
    assert_eq!(
        std::fs::canonicalize(reported).expect("canon reported"),
        std::fs::canonicalize(home.path().join("work")).expect("canon work")
    );
    conn.close().await;
}

#[tokio::test]
async fn connects_to_servers_without_tools_capability_without_listing_tools() {
    let servers = Arc::new(Mutex::new(Vec::new()));
    let methods = Arc::new(Mutex::new(Vec::new()));
    let t = spawn_mock_transport(&servers, Some(Arc::clone(&methods)), true, None);
    let (conn, _) = connect_with(
        McpServerEntry {
            name: "fake".to_string(),
            config: McpServerConfig::Stdio(stdio_cfg("unused")),
            auth_provider: None,
            source: "test".to_string(),
            scope: None,
        },
        vec![t],
        None,
    );
    conn.get_client().await.expect("get_client");
    assert_eq!(conn.state(), ServerState::Connected);
    assert!(conn.tools().is_empty());
    assert_eq!(
        *methods.lock().unwrap_or_else(|e| e.into_inner()),
        vec!["initialize".to_string()]
    );
    conn.close().await;
}

#[tokio::test]
async fn marks_dropped_connection_and_reconnects_on_next_call() {
    let servers = Arc::new(Mutex::new(Vec::new()));
    let t1 = spawn_mock_transport(&servers, None, false, None);
    let t2 = spawn_mock_transport(&servers, None, false, None);
    let (conn, opened) = connect_with(
        McpServerEntry {
            name: "fake".to_string(),
            config: McpServerConfig::Stdio(stdio_cfg("unused")),
            auth_provider: None,
            source: "test".to_string(),
            scope: None,
        },
        vec![t1, t2],
        None,
    );
    conn.get_client().await.expect("connect");
    let srv = servers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .first()
        .cloned()
        .expect("srv");
    srv.close().await.expect("close srv");
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(conn.state(), ServerState::Disconnected);
    assert_eq!(conn.error().as_deref(), Some("Connection closed"));
    let res = conn.call_tool("echo", None, None).await.expect("reconnect call");
    assert_eq!(res.content.len(), 1);
    assert_eq!(conn.state(), ServerState::Connected);
    assert_eq!(*opened.lock().unwrap_or_else(|e| e.into_inner()), 2);
    conn.close().await;
}

#[tokio::test]
async fn retries_http_connections_that_fail_with_transient_error() {
    let servers = Arc::new(Mutex::new(Vec::new()));
    let t1 = spawn_mock_transport(
        &servers,
        None,
        false,
        Some(Arc::new(|_, _| {
            Box::pin(async {
                Err(Error::typed(
                    "McpHttpError",
                    McpHttpError::new(503, "MCP HTTP request failed with status 503", None),
                ))
            })
        })),
    );
    let t2 = spawn_mock_transport(&servers, None, false, None);
    let (conn, opened) = connect_with(
        McpServerEntry {
            name: "fake".to_string(),
            config: McpServerConfig::Http({
                let mut h = http_cfg("http://unused.invalid");
                h.headers = Some(IndexMap::from([("Authorization".to_string(), "x".to_string())]));
                h
            }),
            auth_provider: None,
            source: "test".to_string(),
            scope: None,
        },
        vec![t1, t2],
        None,
    );
    conn.get_client().await.expect("retry succeed");
    assert_eq!(conn.state(), ServerState::Connected);
    assert_eq!(*opened.lock().unwrap_or_else(|e| e.into_inner()), 2);
    conn.close().await;

    let t_bad = spawn_mock_transport(
        &servers,
        None,
        false,
        Some(Arc::new(|_, _| {
            Box::pin(async {
                Err(Error::typed(
                    "McpHttpError",
                    McpHttpError::new(400, "MCP HTTP request failed with status 400: bad", None),
                ))
            })
        })),
    );
    let (failing, f_opened) = connect_with(
        McpServerEntry {
            name: "fake".to_string(),
            config: McpServerConfig::Http({
                let mut h = http_cfg("http://unused.invalid");
                h.headers = Some(IndexMap::from([("Authorization".to_string(), "x".to_string())]));
                h
            }),
            auth_provider: None,
            source: "test".to_string(),
            scope: None,
        },
        vec![t_bad],
        None,
    );
    let err = failing.get_client().await.err().expect("should fail 400");
    assert!(err.message().contains("status 400: bad"));
    assert_eq!(failing.state(), ServerState::Failed);
    assert_eq!(*f_opened.lock().unwrap_or_else(|e| e.into_inner()), 1);
}

#[tokio::test]
async fn retries_resource_reads_but_not_tool_calls_after_transient_http_error() {
    let servers = Arc::new(Mutex::new(Vec::new()));
    let failed: Arc<Mutex<IndexSet<String>>> = Arc::new(Mutex::new(IndexSet::new()));
    let failed_cb = Arc::clone(&failed);
    let t = spawn_mock_transport(
        &servers,
        None,
        false,
        Some(Arc::new(move |inner, msg| {
            let inner = inner.clone();
            let failed_cb = Arc::clone(&failed_cb);
            Box::pin(async move {
                if let JsonRpcMessage::Request(ref r) = msg
                    && (r.method == "resources/read" || r.method == "tools/call")
                {
                    let mut set = failed_cb.lock().unwrap_or_else(|e| e.into_inner());
                    if !set.contains(&r.method) {
                        set.insert(r.method.clone());
                        return Err(Error::typed(
                            "McpHttpError",
                            McpHttpError::new(502, "MCP HTTP request failed with status 502", None),
                        ));
                    }
                }
                inner.send(msg).await
            })
        })),
    );
    let (conn, _) = connect_with(
        McpServerEntry {
            name: "fake".to_string(),
            config: McpServerConfig::Stdio(stdio_cfg("unused")),
            auth_provider: None,
            source: "test".to_string(),
            scope: None,
        },
        vec![t],
        None,
    );
    let res = conn
        .read_resource("docs://a", McpRequestOptions::default())
        .await
        .expect("read_resource retry ok");
    assert_eq!(res.contents.len(), 1);
    let call_err = conn
        .call_tool("echo", None, None)
        .await
        .expect_err("call_tool should not retry");
    assert!(call_err.message().contains("status 502"));
    conn.close().await;
}

#[tokio::test]
async fn asks_oauth_servers_that_keep_rejecting_requests_for_new_sign_in() {
    let servers = Arc::new(Mutex::new(Vec::new()));
    let t = spawn_mock_transport(
        &servers,
        None,
        false,
        Some(Arc::new(|_, _| {
            Box::pin(async {
                let resp = JsResponse::null_body(401, Headers::new());
                Err(Error::typed(
                    "McpAuthRequiredError",
                    McpAuthRequiredError::new(&resp, None),
                ))
            })
        })),
    );
    let (conn, _) = connect_with(
        McpServerEntry {
            name: "fake".to_string(),
            config: McpServerConfig::Http(http_cfg("http://unused.invalid")),
            auth_provider: None,
            source: "test".to_string(),
            scope: None,
        },
        vec![t],
        None,
    );
    let err = conn.get_client().await.err().expect("should fail needs-auth");
    assert!(
        err.message()
            .contains("MCP server \"fake\" requires sign-in. Run /mcp to sign in.")
    );
    assert_eq!(conn.state(), ServerState::NeedsAuth);
    conn.close().await;
}

#[tokio::test]
async fn sends_provider_token_and_asks_for_provider_login_when_rejected() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("addr");
    let authorizations: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let auths_cb = Arc::clone(&authorizations);
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

    let accept = tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                biased;
                _ = shutdown_rx.changed() => break,
                res = listener.accept() => res,
            };
            let Ok((stream, _)) = accepted else { break };
            let auths = Arc::clone(&auths_cb);
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let svc = service_fn(move |req: Request<Incoming>| {
                    let auths = Arc::clone(&auths);
                    async move {
                        let hdr = req
                            .headers()
                            .get(hyper::header::AUTHORIZATION)
                            .and_then(|v| v.to_str().ok())
                            .map(String::from);
                        auths.lock().unwrap_or_else(|e| e.into_inner()).push(hdr);
                        let mut resp = Response::new(Full::new(Bytes::new()));
                        *resp.status_mut() = StatusCode::UNAUTHORIZED;
                        Ok::<_, Infallible>(resp)
                    }
                });
                let _ = http1::Builder::new().serve_connection(io, svc).await;
            });
        }
    });

    let conn = McpServerConnection::new(McpServerConnectionOptions {
        entry: McpServerEntry {
            name: "radius".to_string(),
            config: McpServerConfig::Http(http_cfg(&format!("http://127.0.0.1:{}/mcp", addr.port()))),
            auth_provider: Some("radius".to_string()),
            source: "test".to_string(),
            scope: None,
        },
        cwd: pi_js::env::cwd(),
        create_transport: Arc::new(create_default_transport),
        credentials: McpOAuthCredentialStore::new(Some(Arc::new(InMemoryAuthStorageBackend::new())), None),
        provider_token: Some(Arc::new(|provider| {
            let matched = provider == "radius";
            Box::pin(async move { if matched { Some("tok".to_string()) } else { None } })
        })),
        on_tools: Arc::new(|_| {}),
        on_change: None,
        log: None,
    });

    assert!(conn.oauth_url().is_none());
    let err = conn.get_client().await.err().expect("should fail 401");
    assert!(
        err.message()
            .contains("MCP server \"radius\" requires sign-in. Run /login radius to sign in."),
        "err: {}",
        err.message()
    );
    assert_eq!(conn.state(), ServerState::NeedsAuth);
    assert_eq!(
        *authorizations.lock().unwrap_or_else(|e| e.into_inner()),
        vec![Some("Bearer tok".to_string())]
    );

    conn.close().await;
    let _ = shutdown_tx.send(true);
    let _ = accept.await;
}

#[tokio::test]
async fn appends_server_log_messages_to_log_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log_path = dir.path().join("mcp.log");
    let log = Arc::new(McpServerLog::new(log_path.to_string_lossy().into_owned()));
    let servers = Arc::new(Mutex::new(Vec::new()));
    let t = spawn_mock_transport(&servers, None, false, None);
    let (conn, _) = connect_with(
        McpServerEntry {
            name: "fake".to_string(),
            config: McpServerConfig::Stdio(stdio_cfg("unused")),
            auth_provider: None,
            source: "test".to_string(),
            scope: None,
        },
        vec![t],
        Some(log),
    );
    conn.get_client().await.expect("connect");
    let srv = servers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .first()
        .cloned()
        .expect("srv");

    let n1 = pi_mcp::protocol::jsonrpc::JsonRpcNotification {
        jsonrpc: JsonRpcVersion::V2,
        method: "notifications/message".to_string(),
        params: Some(json!({ "level": "warning", "logger": "db", "data": "slow\nquery" })),
    };
    let n2 = pi_mcp::protocol::jsonrpc::JsonRpcNotification {
        jsonrpc: JsonRpcVersion::V2,
        method: "notifications/message".to_string(),
        params: Some(json!({ "level": "error", "data": { "code": 7 } })),
    };
    srv.send(JsonRpcMessage::Notification(n1)).await.expect("send n1");
    srv.send(JsonRpcMessage::Notification(n2)).await.expect("send n2");
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    let raw = std::fs::read_to_string(&log_path).expect("read mcp.log");
    let stripped: String = raw
        .lines()
        .map(|l| {
            if !l.starts_with(' ')
                && let Some((_, rest)) = l.split_once(' ')
            {
                rest
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    assert_eq!(
        stripped,
        "[fake] warning db: slow\n    query\n[fake] error {\"code\":7}\n"
    );
    conn.close().await;
}

#[tokio::test]
async fn resolves_oauth_client_secret_lazily() {
    let servers = Arc::new(Mutex::new(Vec::new()));
    let t = spawn_mock_transport(&servers, None, false, None);
    let (conn, _) = connect_with(
        McpServerEntry {
            name: "fake".to_string(),
            config: McpServerConfig::Http({
                let mut h = http_cfg("http://unused.invalid");
                h.oauth = Some(McpOAuthConfig {
                    client_secret: Some("!exit 1".to_string()),
                    ..Default::default()
                });
                h
            }),
            auth_provider: None,
            source: "test".to_string(),
            scope: None,
        },
        vec![t],
        None,
    );
    let res = conn.call_tool("echo", None, None).await.expect("call_tool ok");
    assert_eq!(res.content.len(), 1);
    let err = conn.oauth_settings().expect_err("should fail lazy clientSecret");
    assert!(err.message().contains("oauth.clientSecret"), "err: {}", err.message());
    conn.close().await;
}

fn listing(name: &str, desc: Option<&str>, exposure: Option<McpExposure>) -> McpServerListing {
    let mut cfg = stdio_cfg("x");
    cfg.description = desc.map(String::from);
    cfg.exposure = exposure;
    McpServerListing {
        entry: McpServerEntry {
            name: name.to_string(),
            config: McpServerConfig::Stdio(cfg),
            auth_provider: None,
            source: "test".to_string(),
            scope: None,
        },
        connection: None,
    }
}

#[test]
fn renders_mcp_servers_section_and_enforces_limits() {
    let mut plain = listing("plain", None, None);
    plain.connection = Some(McpServerListingConnection {
        instructions: Some("From instructions.".to_string()),
    });
    let section = render_servers_section(&[
        listing("docs", Some("Docs search.\nMore."), None),
        listing("later", None, Some(McpExposure::Deferred)),
        listing("direct", Some("Declared."), Some(McpExposure::Direct)),
        plain,
    ])
    .expect("section");
    let lines: Vec<_> = section.split('\n').skip(1).collect();
    assert_eq!(
        lines,
        vec![
            "- mcp__docs (codemode): Docs search.",
            "- mcp__later (tool_search)",
            "- mcp__plain (codemode): From instructions.",
        ]
    );
    assert!(render_servers_section(&[listing("direct", Some("Declared."), Some(McpExposure::Direct))]).is_none());

    let forty: Vec<_> = (0..40)
        .map(|i| listing(&format!("server{i}"), Some(&"x".repeat(400)), None))
        .collect();
    let s_forty = render_servers_section(&forty).expect("40 section");
    assert!(s_forty.encode_utf16().count() <= MAX_SERVERS_SECTION_CHARS);
    assert_eq!(s_forty.split('\n').count(), 41);
    assert!(s_forty.contains("- mcp__server39 (codemode): x"));

    let two_hundred: Vec<_> = (0..200)
        .map(|i| listing(&format!("server-with-a-long-name-{i}"), Some("desc"), None))
        .collect();
    let s_200 = render_servers_section(&two_hundred).expect("200 section");
    assert!(s_200.encode_utf16().count() <= MAX_SERVERS_SECTION_CHARS);
    let l200: Vec<_> = s_200.split('\n').collect();
    let last = l200.last().copied().unwrap_or("");
    assert!(last.starts_with("- … ") && last.ends_with(" more servers; find their tools with searchTools()"));
    let omitted: usize = last
        .trim_start_matches("- … ")
        .split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .expect("omitted count");
    assert_eq!(l200.len() - 2 + omitted, 200);
}
