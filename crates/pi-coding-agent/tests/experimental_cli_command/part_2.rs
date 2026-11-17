//! Port of packages/coding-agent/test/experimental-cli-command.test.ts

use pi_coding_agent::cli::experimental::cli::CLI;
use pi_coding_agent::cli::experimental::command::CommandParseResult;

const UNSUPPORTED_SERVER_OPTIONS: &str = "The experimental server command does not support existing CLI options yet";
const UNSUPPORTED_CLIENT_OPTIONS: &str = "The experimental client command does not support existing CLI options yet";

fn s(items: &[&str]) -> Vec<String> {
    items.iter().map(|v| (*v).to_string()).collect()
}

#[test]
fn parses_server_configuration() {
    let result = CLI.parse(&s(&[
        "server",
        "--server-id",
        "00000000-0000-4000-8000-000000000001",
        "--session-dir",
        "~/pi-sessions",
        "--provider",
        "anthropic",
        "--model",
        "claude-sonnet-4-5",
        "-e",
        "./first-plugin",
        "-e=./second-plugin",
    ]));
    let cmd = result.command().expect("server parse ok");
    let json = serde_json::to_value(cmd).expect("serialize");
    assert_eq!(
        json,
        serde_json::json!({
            "command": "server",
            "serverId": "00000000-0000-4000-8000-000000000001",
            "sessionDir": "~/pi-sessions",
            "provider": "anthropic",
            "model": "claude-sonnet-4-5",
            "pluginPackages": ["./first-plugin", "./second-plugin"]
        })
    );
}

#[test]
fn parses_provider_qualified_server_models() {
    let result = CLI.parse(&s(&["server", "--model", "anthropic/claude-sonnet-4-5:high"]));
    let cmd = result.command().expect("parse ok");
    assert_eq!(
        serde_json::to_value(cmd).expect("serialize"),
        serde_json::json!({
            "command": "server",
            "model": "anthropic/claude-sonnet-4-5:high"
        })
    );
}

#[test]
fn parses_client_transport_addresses() {
    let unix = CLI.parse(&s(&["client", "--connect", "unix:///tmp/pi.sock"]));
    assert_eq!(
        serde_json::to_value(unix.command().expect("unix ok")).expect("serialize"),
        serde_json::json!({
            "command": "client",
            "connect": { "transport": "unix", "path": "/tmp/pi.sock" }
        })
    );

    let radius = CLI.parse(&s(&[
        "client",
        "--connect",
        "radius://00000000-0000-4000-8000-000000000001",
        "--session-id",
        "demo-1",
    ]));
    assert_eq!(
        serde_json::to_value(radius.command().expect("radius ok")).expect("serialize"),
        serde_json::json!({
            "command": "client",
            "connect": {
                "transport": "radius",
                "serverId": "00000000-0000-4000-8000-000000000001"
            },
            "sessionId": "demo-1"
        })
    );
}

#[test]
fn parses_client_session_selection_flags() {
    let cases = [
        ("-c", "continue"),
        ("--continue", "continue"),
        ("-r", "resume"),
        ("--resume", "resume"),
    ];
    for (option, property) in cases {
        let res = CLI.parse(&s(&["client", option]));
        let val = serde_json::to_value(res.command().expect("parse ok")).expect("serialize");
        let mut expected = serde_json::Map::new();
        expected.insert("command".to_string(), serde_json::Value::String("client".to_string()));
        expected.insert(property.to_string(), serde_json::Value::Bool(true));
        assert_eq!(val, serde_json::Value::Object(expected));
    }
}

#[test]
fn parses_session_selection_followed_by_one_shot_prompt() {
    let res = CLI.parse(&s(&["client", "-r", "Explain this project"]));
    assert_eq!(
        serde_json::to_value(res.command().expect("parse ok")).expect("serialize"),
        serde_json::json!({
            "command": "client",
            "resume": true,
            "prompt": "Explain this project"
        })
    );
}

#[test]
fn parses_client_model_and_plugin_configuration() {
    let res = CLI.parse(&s(&[
        "client",
        "--model",
        "anthropic/claude-sonnet-4-5:high",
        "-e",
        "./first-plugin",
        "-e",
        "./second-plugin",
    ]));
    assert_eq!(
        serde_json::to_value(res.command().expect("parse ok")).expect("serialize"),
        serde_json::json!({
            "command": "client",
            "model": "anthropic/claude-sonnet-4-5:high",
            "pluginPackages": ["./first-plugin", "./second-plugin"]
        })
    );
}

#[test]
fn parses_authentication_sources() {
    let cases = [
        (
            ["--auth-token", "secret"],
            serde_json::json!({ "type": "token", "token": "secret" }),
        ),
        (
            ["--auth-token-file", "/tmp/token"],
            serde_json::json!({ "type": "file", "path": "/tmp/token" }),
        ),
    ];
    for (argv, expected_auth) in cases {
        for command in ["server", "client"] {
            let full = [command, argv[0], argv[1]];
            let res = CLI.parse(&s(&full));
            let val = serde_json::to_value(res.command().expect("parse ok")).expect("serialize");
            assert_eq!(val["command"], command);
            assert_eq!(val["auth"], expected_auth);
        }
    }
}

#[test]
fn permits_omitted_authentication_for_server_and_client() {
    for command in ["server", "client"] {
        let res = CLI.parse(&s(&[command]));
        assert_eq!(
            serde_json::to_value(res.command().expect("parse ok")).expect("serialize"),
            serde_json::json!({ "command": command })
        );
    }
}

#[test]
fn rejects_invalid_experimental_inputs() {
    let cases: [(&[&str], &str); 17] = [
        (
            &["client", "--listen", "unix:///tmp/pi.sock"],
            UNSUPPORTED_CLIENT_OPTIONS,
        ),
        (
            &["server", "--listen", "unix:///tmp/pi.sock"],
            UNSUPPORTED_SERVER_OPTIONS,
        ),
        (
            &["server", "--connect", "unix:///tmp/pi.sock"],
            UNSUPPORTED_SERVER_OPTIONS,
        ),
        (
            &["client", "--connect", "ws://localhost:8080"],
            "Unsupported --connect transport \"ws:\"",
        ),
        (
            &["client", "--connect", "radius://not-a-server"],
            "Radius transport address requires",
        ),
        (
            &["client", "--connect", "unix://relative.sock"],
            "Unix transport address must not include an authority",
        ),
        (
            &["client", "--connect", "unix:///tmp/pi.sock?wrong=value"],
            "Invalid --connect address",
        ),
        (&["client", "--provider", "anthropic"], "--provider requires --model"),
        (
            &["client", "-c", "-r"],
            "--session-id, --continue, and --resume are mutually exclusive",
        ),
        (&["client", "--continue=true"], "--continue does not take a value"),
        (&["server", "--provider", "anthropic"], "--provider requires --model"),
        (&["server", "--server-id", "not-a-uuid"], "Invalid --server-id"),
        (&["server", "--server-id"], "--server-id requires a value"),
        (&["server", "--session-dir"], "--session-dir requires a value"),
        (&["client", "-e"], "-e requires a value"),
        (
            &["server", "--session-dir", "/tmp/first", "--session-dir=/tmp/second"],
            "--session-dir may only be specified once",
        ),
        (&["client", "--connect="], "--connect requires a value"),
    ];

    for (argv, expected_substr) in cases {
        let res = CLI.parse(&s(argv));
        assert!(!res.is_ok(), "expected parse error for {argv:?}, got ok");
        assert!(
            res.errors().iter().any(|err| err.contains(expected_substr)),
            "errors {:?} for {argv:?} did not contain {expected_substr:?}",
            res.errors()
        );
    }
}

#[test]
fn rejects_unsupported_options_without_parsing_them_through_the_stable_cli() {
    let res = CLI.parse(&s(&["client", "--tui-mode", "wrong", "--model", "claude-sonnet"]));
    assert_eq!(
        res,
        CommandParseResult::Err {
            errors: vec![UNSUPPORTED_CLIENT_OPTIONS.to_string()],
        }
    );
}
