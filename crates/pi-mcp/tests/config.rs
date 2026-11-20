use indexmap::IndexMap;
use pi_mcp::{McpServerConfigOptions, McpServerConfigResult, is_mcp_server_override, validate_mcp_server_config};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};
use std::sync::{Arc, Mutex};

fn error_of(result: &McpServerConfigResult) -> &str {
    match result {
        McpServerConfigResult::Invalid { error } => error.as_str(),
        _ => "",
    }
}

fn kind_of(result: &McpServerConfigResult) -> &'static str {
    match result {
        McpServerConfigResult::Stdio(_) => "stdio",
        McpServerConfigResult::Http(_) => "http",
        McpServerConfigResult::Invalid { .. } => "invalid",
    }
}

#[test]
fn accepts_stdio_and_http_entries_and_keeps_them_as_given() {
    let stdio =
        json!({ "command": "npx", "args": ["-y", "server"], "env": { "A": "1" }, "cwd": "sub", "enabled": false });
    let stdio_res = validate_mcp_server_config("files", &stdio, None);
    assert_eq!(kind_of(&stdio_res), "stdio");
    if let McpServerConfigResult::Stdio(cfg) = stdio_res {
        assert_eq!(serde_json::to_value(cfg).unwrap(), stdio);
    }

    assert_eq!(
        kind_of(&validate_mcp_server_config(
            "files",
            &json!({ "type": "stdio", "command": "x" }),
            None
        )),
        "stdio"
    );

    let http = json!({
        "url": "https://example.com/mcp",
        "headers": { "Authorization": "Bearer token" },
        "oauth": { "clientId": "id", "callbackPort": 8080, "scope": "read" },
        "timeout": 30.0,
        "description": "Docs"
    });
    let http_res = validate_mcp_server_config("docs", &http, None);
    assert_eq!(kind_of(&http_res), "http");
    if let McpServerConfigResult::Http(cfg) = http_res {
        assert_eq!(serde_json::to_value(cfg).unwrap(), http);
    }

    for kind in ["http", "streamable-http"] {
        assert_eq!(
            kind_of(&validate_mcp_server_config(
                "docs",
                &json!({ "type": kind, "url": "http://localhost:3000/mcp" }),
                None
            )),
            "http"
        );
    }
}

#[test]
fn picks_the_transport_from_type_url_and_command() {
    assert_eq!(
        kind_of(&validate_mcp_server_config(
            "s",
            &json!({ "url": "https://a.example", "command": "x" }),
            None
        )),
        "http"
    );
    assert_eq!(
        kind_of(&validate_mcp_server_config(
            "s",
            &json!({ "type": "stdio", "url": "https://a.example", "command": "x" }),
            None
        )),
        "stdio"
    );
    assert_eq!(
        error_of(&validate_mcp_server_config(
            "s",
            &json!({ "type": "http", "command": "x" }),
            None
        )),
        "server \"s\" needs either \"command\" (stdio) or \"url\" (streamable HTTP)"
    );
    assert_eq!(
        error_of(&validate_mcp_server_config(
            "s",
            &json!({ "type": "sse", "url": "https://a.example/sse" }),
            None
        )),
        "server \"s\": legacy SSE transport is not supported; use the streamable HTTP URL"
    );
}

#[test]
fn reports_invalid_names_and_fields() {
    let cases: [(&str, Value, &str); 13] = [
        (
            "bad name",
            json!({ "command": "x" }),
            "invalid server name \"bad name\" (use letters, digits, \"_\" and \"-\")",
        ),
        ("s", json!("npx"), "server \"s\" must be an object"),
        ("s", json!([]), "server \"s\" must be an object"),
        (
            "s",
            json!({ "command": "x", "enabled": "yes" }),
            "server \"s\": enabled must be a boolean",
        ),
        (
            "s",
            json!({ "command": "x", "description": 1 }),
            "server \"s\": description must be a string",
        ),
        (
            "s",
            json!({ "command": "x", "timeout": 0 }),
            "server \"s\": timeout must be a positive number of seconds",
        ),
        (
            "s",
            json!({ "command": "x", "args": "a b" }),
            "server \"s\": args must be an array of strings",
        ),
        (
            "s",
            json!({ "command": "x", "env": { "A": 1 } }),
            "server \"s\": env must map names to strings",
        ),
        (
            "s",
            json!({ "command": "x", "cwd": 1 }),
            "server \"s\": cwd must be a string",
        ),
        (
            "s",
            json!({ "url": "file:///x" }),
            "server \"s\": url must be an http or https URL",
        ),
        (
            "s",
            json!({ "url": "not a url" }),
            "server \"s\": url must be an http or https URL",
        ),
        (
            "s",
            json!({ "url": "https://a.example", "headers": { "A": 1 } }),
            "server \"s\": headers must map names to strings",
        ),
        (
            "s",
            json!({}),
            "server \"s\" needs either \"command\" (stdio) or \"url\" (streamable HTTP)",
        ),
    ];

    for (name, raw, expected_err) in cases {
        assert_eq!(error_of(&validate_mcp_server_config(name, &raw, None)), expected_err);
    }
}

#[test]
fn checks_oauth_settings() {
    let oauth_err = |settings: Value| -> String {
        error_of(&validate_mcp_server_config(
            "s",
            &json!({ "url": "https://a.example", "oauth": settings }),
            None,
        ))
        .to_string()
    };

    assert_eq!(oauth_err(json!("x")), "server \"s\": oauth must be an object");
    assert_eq!(
        oauth_err(json!({ "clientId": 1 })),
        "server \"s\": oauth.clientId must be a string"
    );
    assert_eq!(
        oauth_err(json!({ "clientSecret": 1 })),
        "server \"s\": oauth.clientSecret must be a string"
    );
    assert_eq!(
        oauth_err(json!({ "callbackPort": 70000 })),
        "server \"s\": oauth.callbackPort must be a port number"
    );
    assert!(
        oauth_err(json!({ "callbackUrl": "https://localhost/callback" }))
            .contains("oauth.callbackUrl must be an http URI")
    );
    assert!(
        oauth_err(json!({ "callbackUrl": "http://example.com/callback" }))
            .contains("oauth.callbackUrl must be an http URI")
    );
    assert!(
        oauth_err(json!({ "callbackUrl": "http://localhost/callback?x=1" }))
            .contains("oauth.callbackUrl must be an http URI")
    );
    assert_eq!(
        oauth_err(json!({ "callbackUrl": "http://[::1]:8080/cb", "callbackPort": 8080 })),
        ""
    );
    assert_eq!(
        oauth_err(json!({ "callbackUrl": "http://localhost:8080/cb", "callbackPort": 9090 })),
        "server \"s\": oauth.callbackUrl and oauth.callbackPort name different ports"
    );
    assert_eq!(
        oauth_err(json!({ "scope": 1 })),
        "server \"s\": oauth.scope must be a string"
    );
    assert_eq!(
        oauth_err(json!({ "clientName": " " })),
        "server \"s\": oauth.clientName must be a non-empty string"
    );
    assert_eq!(
        oauth_err(json!({ "clientRegistration": "x" })),
        "server \"s\": oauth.clientRegistration must be \"auto\", \"dcr\", or \"cimd\""
    );
    assert_eq!(
        oauth_err(json!({ "clientRegistration": "dcr", "clientName": "app" })),
        ""
    );
    assert!(
        oauth_err(json!({ "authServerMetadataUrl": "http://auth.example/.well-known/x" }))
            .contains("oauth.authServerMetadataUrl must be an https URL")
    );
    assert_eq!(
        oauth_err(json!({ "authServerMetadataUrl": "http://127.0.0.1/.well-known/x" })),
        ""
    );
}

#[test]
fn allows_cimd_only_with_the_applications_check_of_the_callback() {
    let entry = |oauth: Value| json!({ "url": "https://a.example", "oauth": oauth });
    assert_eq!(
        error_of(&validate_mcp_server_config(
            "s",
            &entry(json!({ "clientRegistration": "cimd" })),
            None
        )),
        "server \"s\": oauth.clientRegistration \"cimd\" is not supported: there are no Client ID Metadata Documents"
    );

    let checked = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
    let checked_cb = checked.clone();
    let options = McpServerConfigOptions {
        check_metadata_document_callback: Some(Arc::new(move |callback_url| {
            checked_cb.lock().unwrap().push(callback_url.map(str::to_string));
            if callback_url.is_some_and(|u| u.ends_with("/other")) {
                Some("the documents do not list it".to_string())
            } else {
                None
            }
        })),
    };

    assert_eq!(
        kind_of(&validate_mcp_server_config(
            "s",
            &entry(json!({ "clientRegistration": "cimd" })),
            Some(&options)
        )),
        "http"
    );
    assert_eq!(
        error_of(&validate_mcp_server_config(
            "s",
            &entry(json!({ "clientRegistration": "cimd", "callbackUrl": "http://localhost/other" })),
            Some(&options)
        )),
        "server \"s\": the documents do not list it"
    );
    assert_eq!(
        *checked.lock().unwrap(),
        vec![None, Some("http://localhost/other".to_string())]
    );
    assert_eq!(
        error_of(&validate_mcp_server_config(
            "s",
            &entry(json!({ "clientRegistration": "cimd", "clientId": "id" })),
            Some(&options)
        )),
        "server \"s\": oauth.clientRegistration \"cimd\" cannot be combined with oauth.clientId or oauth.clientName"
    );
}

#[test]
fn keeps_unknown_fields_for_the_caller_after_shared_fields_are_valid() {
    fn validate_app_config(name: &str, raw: &Value) -> String {
        let result = validate_mcp_server_config(name, raw, None);
        match result {
            McpServerConfigResult::Invalid { error } => error,
            McpServerConfigResult::Stdio(cfg) => check_extra(name, "stdio", &cfg.extra),
            McpServerConfigResult::Http(cfg) => check_extra(name, "http", &cfg.extra),
        }
    }

    fn check_extra(name: &str, kind: &str, extra: &IndexMap<String, Value>) -> String {
        let shared = extra.get("shared");
        let run_on = extra.get("runOn");
        if let Some(s) = shared
            && !s.is_boolean()
        {
            return format!("server \"{name}\": shared must be a boolean");
        }
        if kind == "http" && run_on.is_some() {
            return format!("server \"{name}\": runOn is for stdio");
        }
        let shared_str = match shared {
            None => "undefined".to_string(),
            Some(Value::Bool(b)) => b.to_string(),
            Some(other) => other.to_string(),
        };
        format!("{kind} shared={shared_str}")
    }

    assert_eq!(
        validate_app_config("s", &json!({ "command": "x", "shared": true, "runOn": "host" })),
        "stdio shared=true"
    );
    assert_eq!(
        validate_app_config("s", &json!({ "url": "https://a.example", "runOn": "host" })),
        "server \"s\": runOn is for stdio"
    );
    assert_eq!(
        validate_app_config("s", &json!({ "command": "x", "shared": 1 })),
        "server \"s\": shared must be a boolean"
    );
    assert_eq!(
        validate_app_config("s", &json!({ "command": "x", "shared": 1, "enabled": "no" })),
        "server \"s\": enabled must be a boolean"
    );
}

#[test]
fn detects_server_overrides_without_command_url_or_type() {
    let as_map = |val: Value| -> Map<String, Value> { val.as_object().unwrap().clone() };
    assert!(is_mcp_server_override(&as_map(json!({ "enabled": false }))));
    assert!(is_mcp_server_override(&as_map(json!({}))));
    assert!(!is_mcp_server_override(&as_map(json!({ "command": "x" }))));
    assert!(!is_mcp_server_override(&as_map(json!({ "url": "https://a.example" }))));
    assert!(!is_mcp_server_override(&as_map(
        json!({ "type": "stdio", "enabled": false })
    )));
}
