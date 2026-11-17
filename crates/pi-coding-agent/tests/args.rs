//! Port of packages/coding-agent/test/args.test.ts (plus GUI flag/subcommand dispatch tests)

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pi_ai::ModelThinkingLevel;
use pi_coding_agent::cli::args::{
    ArgDiagnostic, ArgDiagnosticType, ListModelsOption, Mode, UnknownFlagValue, build_gui_forward_args,
    dispatch_gui, is_gui_subcommand, normalize_session_name, parse_args, resolve_gui_executable_from, testing,
};
use pi_tui::TuiMode;

#[test]
fn parses_version_flag_and_precedence() {
    assert_eq!(parse_args(&["--version"]).version, Some(true));
    assert_eq!(parse_args(&["-v"]).version, Some(true));

    let res = parse_args(&["--version", "--help", "some message"]);
    assert_eq!(res.version, Some(true));
    assert_eq!(res.help, Some(true));
    assert!(res.messages.contains(&"some message".to_string()));
}

#[test]
fn parses_help_flag() {
    assert_eq!(parse_args(&["--help"]).help, Some(true));
    assert_eq!(parse_args(&["-h"]).help, Some(true));
}

#[test]
fn parses_print_flag() {
    assert_eq!(parse_args(&["--print"]).print, Some(true));
    assert_eq!(parse_args(&["-p"]).print, Some(true));

    let prompt = "---\ntitle: hello\n---\nSay hi.";
    let res = parse_args(&["-p", prompt]);
    assert_eq!(res.print, Some(true));
    assert_eq!(res.messages, vec![prompt]);
    assert!(res.unknown_flags.is_empty());

    let res2 = parse_args(&["-p", "--provider", "openai", "Say hi."]);
    assert_eq!(res2.print, Some(true));
    assert_eq!(res2.provider.as_deref(), Some("openai"));
    assert_eq!(res2.messages, vec!["Say hi."]);
}

#[test]
fn parses_continue_and_resume_flags() {
    assert_eq!(parse_args(&["--continue"]).r#continue, Some(true));
    assert_eq!(parse_args(&["-c"]).r#continue, Some(true));
    assert_eq!(parse_args(&["--resume"]).resume, Some(true));
    assert_eq!(parse_args(&["-r"]).resume, Some(true));
}

#[test]
fn parses_flags_with_values() {
    assert_eq!(parse_args(&["--provider", "openai"]).provider.as_deref(), Some("openai"));
    assert_eq!(parse_args(&["--model", "gpt-4o"]).model.as_deref(), Some("gpt-4o"));
    assert_eq!(
        parse_args(&["--api-key", "sk-test-key"]).api_key.as_deref(),
        Some("sk-test-key")
    );
    assert_eq!(
        parse_args(&["--system-prompt", "You are a helpful assistant"])
            .system_prompt
            .as_deref(),
        Some("You are a helpful assistant")
    );
    assert_eq!(
        parse_args(&["--append-system-prompt", "Additional context"]).append_system_prompt,
        Some(vec!["Additional context".to_string()])
    );
    assert_eq!(
        parse_args(&[
            "--append-system-prompt",
            "Context A",
            "--append-system-prompt",
            "Context B",
        ])
        .append_system_prompt,
        Some(vec!["Context A".to_string(), "Context B".to_string()])
    );
    assert_eq!(
        parse_args(&["--session", "/path/to/session.jsonl"]).session.as_deref(),
        Some("/path/to/session.jsonl")
    );
    assert_eq!(
        parse_args(&["--session-id", "orchestrated-session"])
            .session_id
            .as_deref(),
        Some("orchestrated-session")
    );
    let fork_res = parse_args(&["--fork", "1234abcd"]);
    assert_eq!(fork_res.fork.as_deref(), Some("1234abcd"));
    assert!(fork_res.messages.is_empty());

    assert_eq!(
        parse_args(&["--export", "session.jsonl"]).export.as_deref(),
        Some("session.jsonl")
    );
    assert_eq!(
        parse_args(&["--thinking", "high"]).thinking,
        Some(ModelThinkingLevel::High)
    );
    assert_eq!(
        parse_args(&["--models", "gpt-4o,claude-sonnet,gemini-pro"]).models,
        Some(vec![
            "gpt-4o".to_string(),
            "claude-sonnet".to_string(),
            "gemini-pro".to_string(),
        ])
    );
}

#[test]
fn parses_mode_flag_and_diagnostics() {
    for (raw, expected) in [("text", Mode::Text), ("json", Mode::Json), ("rpc", Mode::Rpc)] {
        let res = parse_args(&["--mode", raw]);
        assert_eq!(res.mode, Some(expected));
        assert!(res.diagnostics.is_empty());
    }

    for invalid in ["yaml", ""] {
        let res = parse_args(&["--mode", invalid, "--version"]);
        assert_eq!(res.mode, None);
        assert_eq!(res.version, Some(true));
        assert!(res.messages.is_empty());
        assert!(res.unknown_flags.is_empty());
        assert_eq!(
            res.diagnostics,
            vec![ArgDiagnostic {
                r#type: ArgDiagnosticType::Error,
                message: format!("Invalid mode \"{invalid}\". Valid values: text, json, rpc"),
            }]
        );
    }

    let missing = parse_args(&["--mode"]);
    assert_eq!(missing.mode, None);
    assert!(missing.unknown_flags.is_empty());
    assert_eq!(
        missing.diagnostics,
        vec![ArgDiagnostic {
            r#type: ArgDiagnosticType::Error,
            message: "--mode requires text, json, or rpc".to_string(),
        }]
    );

    let next_opt = parse_args(&["--mode", "--version"]);
    assert_eq!(next_opt.mode, None);
    assert_eq!(next_opt.version, Some(true));
    assert!(next_opt.unknown_flags.is_empty());
    assert_eq!(
        next_opt.diagnostics,
        vec![ArgDiagnostic {
            r#type: ArgDiagnosticType::Error,
            message: "--mode requires text, json, or rpc".to_string(),
        }]
    );

    let after_valid = parse_args(&["--mode", "json", "--mode", "yaml"]);
    assert_eq!(
        after_valid.diagnostics,
        vec![ArgDiagnostic {
            r#type: ArgDiagnosticType::Error,
            message: "Invalid mode \"yaml\". Valid values: text, json, rpc".to_string(),
        }]
    );
}

#[test]
fn parses_name_flag_and_normalization() {
    assert_eq!(
        parse_args(&["--name", "my-session"]).name.as_deref(),
        Some("my-session")
    );
    assert_eq!(
        parse_args(&["-n", "quick-session"]).name.as_deref(),
        Some("quick-session")
    );
    assert_eq!(parse_args(&["--name", ""]).name.as_deref(), Some(""));
    assert_eq!(
        normalize_session_name("  named session  ").as_deref(),
        Some("named session")
    );
    assert_eq!(normalize_session_name("   "), None);

    assert_eq!(
        parse_args(&["--name"]).diagnostics,
        vec![ArgDiagnostic {
            r#type: ArgDiagnosticType::Error,
            message: "--name requires a value".to_string(),
        }]
    );

    let combined = parse_args(&["--name", "named-run", "--print", "--model", "gpt-4o", "hello"]);
    assert_eq!(combined.name.as_deref(), Some("named-run"));
    assert_eq!(combined.print, Some(true));
    assert_eq!(combined.model.as_deref(), Some("gpt-4o"));
    assert_eq!(combined.messages, vec!["hello"]);
}

#[test]
fn parses_no_session_and_preserves_custom_session_id() {
    assert_eq!(parse_args(&["--no-session"]).no_session, Some(true));

    let help = parse_args(&["--session-id", "ephemeral-id", "--help"]);
    assert_eq!(help.session_id.as_deref(), Some("ephemeral-id"));
    assert_eq!(help.help, Some(true));

    let list = parse_args(&["--session-id", "ephemeral-id", "--list-models"]);
    assert_eq!(list.session_id.as_deref(), Some("ephemeral-id"));
    assert_eq!(list.list_models, Some(ListModelsOption::All));

    let no_sess = parse_args(&["--session-id", "ephemeral-id", "--no-session"]);
    assert_eq!(no_sess.session_id.as_deref(), Some("ephemeral-id"));
    assert_eq!(no_sess.no_session, Some(true));
}

#[test]
fn parses_resource_and_approval_flags() {
    assert_eq!(
        parse_args(&["--extension", "./my-extension.ts"]).extensions,
        Some(vec!["./my-extension.ts".to_string()])
    );
    assert_eq!(
        parse_args(&["-e", "./my-extension.ts"]).extensions,
        Some(vec!["./my-extension.ts".to_string()])
    );
    assert_eq!(
        parse_args(&["--extension", "./ext1.ts", "-e", "./ext2.ts"]).extensions,
        Some(vec!["./ext1.ts".to_string(), "./ext2.ts".to_string()])
    );

    let no_ext = parse_args(&["--no-extensions", "-e", "foo.ts", "-e", "bar.ts"]);
    assert_eq!(no_ext.no_extensions, Some(true));
    assert_eq!(no_ext.extensions, Some(vec!["foo.ts".to_string(), "bar.ts".to_string()]));

    assert_eq!(
        parse_args(&["--skill", "./skill-a", "--skill", "./skill-b"]).skills,
        Some(vec!["./skill-a".to_string(), "./skill-b".to_string()])
    );
    assert_eq!(
        parse_args(&["--prompt-template", "./one", "--prompt-template", "./two"]).prompt_templates,
        Some(vec!["./one".to_string(), "./two".to_string()])
    );
    assert_eq!(
        parse_args(&["--theme", "./dark.json", "--theme", "./light.json"]).themes,
        Some(vec!["./dark.json".to_string(), "./light.json".to_string()])
    );
    assert_eq!(parse_args(&["--use-theme", "light"]).use_theme.as_deref(), Some("light"));

    let missing_theme = parse_args(&["--use-theme", "--print"]);
    assert_eq!(missing_theme.use_theme, None);
    assert_eq!(missing_theme.print, Some(true));
    assert_eq!(
        missing_theme.diagnostics,
        vec![ArgDiagnostic {
            r#type: ArgDiagnosticType::Error,
            message: "--use-theme requires a theme name".to_string(),
        }]
    );

    assert_eq!(parse_args(&["--no-skills"]).no_skills, Some(true));
    assert_eq!(parse_args(&["--no-prompt-templates"]).no_prompt_templates, Some(true));
    assert_eq!(parse_args(&["--no-themes"]).no_themes, Some(true));
    assert_eq!(parse_args(&["--no-context-files"]).no_context_files, Some(true));
    assert_eq!(parse_args(&["-nc"]).no_context_files, Some(true));

    assert_eq!(parse_args(&["--approve"]).project_trust_override, Some(true));
    assert_eq!(parse_args(&["-a"]).project_trust_override, Some(true));
    assert_eq!(parse_args(&["--no-approve"]).project_trust_override, Some(false));
    assert_eq!(parse_args(&["-na"]).project_trust_override, Some(false));
    assert_eq!(parse_args(&["--verbose"]).verbose, Some(true));
    assert_eq!(parse_args(&["--offline"]).offline, Some(true));
}

#[test]
fn parses_tui_mode_flag() {
    assert_eq!(parse_args(&["--tui-mode", "regular"]).tui_mode, Some(TuiMode::Regular));
    assert_eq!(
        parse_args(&["--tui-mode", "fullscreen"]).tui_mode,
        Some(TuiMode::Fullscreen)
    );
    assert_eq!(
        parse_args(&["--tui-mode", "other"]).diagnostics,
        vec![ArgDiagnostic {
            r#type: ArgDiagnosticType::Error,
            message: "Invalid TUI mode \"other\". Valid values: regular, fullscreen".to_string(),
        }]
    );
    assert_eq!(
        parse_args(&["--tui-mode"]).diagnostics,
        vec![ArgDiagnostic {
            r#type: ArgDiagnosticType::Error,
            message: "--tui-mode requires regular or fullscreen".to_string(),
        }]
    );

    let legacy = parse_args(&["--ui-mode", "fullscreen"]);
    assert_eq!(legacy.tui_mode, None);
    assert_eq!(
        legacy.unknown_flags.get("ui-mode"),
        Some(&UnknownFlagValue::String("fullscreen".to_string()))
    );
}

#[test]
fn parses_tool_flags() {
    assert_eq!(parse_args(&["--no-tools"]).no_tools, Some(true));
    assert_eq!(parse_args(&["-nt"]).no_tools, Some(true));
    assert_eq!(parse_args(&["--no-builtin-tools"]).no_builtin_tools, Some(true));
    assert_eq!(parse_args(&["-nbt"]).no_builtin_tools, Some(true));
    assert_eq!(
        parse_args(&["--tools", "read,bash"]).tools,
        Some(vec!["read".to_string(), "bash".to_string()])
    );
    assert_eq!(
        parse_args(&["-t", "read,bash"]).tools,
        Some(vec!["read".to_string(), "bash".to_string()])
    );
    assert_eq!(
        parse_args(&["--exclude-tools", "read,bash"]).exclude_tools,
        Some(vec!["read".to_string(), "bash".to_string()])
    );
    assert_eq!(
        parse_args(&["-xt", "read,bash"]).exclude_tools,
        Some(vec!["read".to_string(), "bash".to_string()])
    );

    let both1 = parse_args(&["--no-tools", "--tools", "read,bash"]);
    assert_eq!(both1.no_tools, Some(true));
    assert_eq!(both1.tools, Some(vec!["read".to_string(), "bash".to_string()]));

    let both2 = parse_args(&["--no-builtin-tools", "--tools", "read,bash"]);
    assert_eq!(both2.no_builtin_tools, Some(true));
    assert_eq!(both2.tools, Some(vec!["read".to_string(), "bash".to_string()]));
}

#[test]
fn parses_messages_file_args_unknown_flags_and_combinations() {
    assert_eq!(parse_args(&["hello", "world"]).messages, vec!["hello", "world"]);
    assert_eq!(
        parse_args(&["@README.md", "@src/main.ts"]).file_args,
        vec!["README.md", "src/main.ts"]
    );
    let mixed = parse_args(&["@file.txt", "explain this", "@image.png"]);
    assert_eq!(mixed.file_args, vec!["file.txt", "image.png"]);
    assert_eq!(mixed.messages, vec!["explain this"]);

    let unk_str = parse_args(&["--unknown-flag", "message"]);
    assert!(unk_str.messages.is_empty());
    assert_eq!(
        unk_str.unknown_flags.get("unknown-flag"),
        Some(&UnknownFlagValue::String("message".to_string()))
    );

    let unk_bool = parse_args(&["--unknown-flag"]);
    assert_eq!(
        unk_bool.unknown_flags.get("unknown-flag"),
        Some(&UnknownFlagValue::Bool(true))
    );

    let unk_eq = parse_args(&["--unknown-flag=value"]);
    assert_eq!(
        unk_eq.unknown_flags.get("unknown-flag"),
        Some(&UnknownFlagValue::String("value".to_string()))
    );

    let complex = parse_args(&[
        "--provider",
        "anthropic",
        "--model",
        "claude-sonnet",
        "--print",
        "--thinking",
        "high",
        "@prompt.md",
        "Do the task",
    ]);
    assert_eq!(complex.provider.as_deref(), Some("anthropic"));
    assert_eq!(complex.model.as_deref(), Some("claude-sonnet"));
    assert_eq!(complex.print, Some(true));
    assert_eq!(complex.thinking, Some(ModelThinkingLevel::High));
    assert_eq!(complex.file_args, vec!["prompt.md"]);
    assert_eq!(complex.messages, vec!["Do the task"]);
}

#[test]
fn parses_gui_flag_and_subcommand() {
    let flag = parse_args(&["--gui", "--session", "abc", "--cwd", "/tmp/work"]);
    assert_eq!(flag.gui, Some(true));
    assert_eq!(flag.session.as_deref(), Some("abc"));
    assert!(flag.unknown_flags.is_empty());

    assert!(is_gui_subcommand(&["gui", "--session", "s1"]));
    assert!(!is_gui_subcommand(&["--gui", "--session", "s1"]));
    assert!(!is_gui_subcommand::<&str>(&[]));
}

#[test]
fn builds_gui_forward_args_correctly() {
    assert_eq!(
        build_gui_forward_args(&["--gui", "--session", "s1", "--model", "opus"], "/repo/work"),
        vec!["--cwd", "/repo/work", "--session", "s1", "--model", "opus"]
    );
    assert_eq!(
        build_gui_forward_args(&["gui", "--cwd", "/explicit", "--session", "s2"], "/repo/work"),
        vec!["--cwd", "/explicit", "--session", "s2"]
    );
    assert_eq!(
        build_gui_forward_args(&["--gui", "--", "--gui", "literal"], "/repo"),
        vec!["--cwd", "/repo", "--", "--gui", "literal"]
    );
}

#[test]
fn resolves_sibling_gui_executable_with_path_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let bin_name = if pi_js::env::platform() == "win32" {
        "pi-gui.exe"
    } else {
        "pi-gui"
    };
    let current_exe = dir.path().join("pi");
    std::fs::write(&current_exe, "").unwrap();
    assert_eq!(resolve_gui_executable_from(Some(&current_exe)), PathBuf::from(bin_name));

    let sibling = dir.path().join(bin_name);
    std::fs::write(&sibling, "").unwrap();
    assert_eq!(resolve_gui_executable_from(Some(&current_exe)), sibling);
    assert_eq!(resolve_gui_executable_from(None), PathBuf::from(bin_name));
}

#[tokio::test]
async fn dispatch_gui_invokes_override_with_resolved_args_and_exit_code() {
    type Call = (PathBuf, Vec<String>, String);
    let recorded: Arc<Mutex<Vec<Call>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded_cb = Arc::clone(&recorded);
    let _guard = testing::override_dispatch_gui(move |exe: &Path, args: &[String], cwd: &str| {
        recorded_cb
            .lock()
            .unwrap()
            .push((exe.to_path_buf(), args.to_vec(), cwd.to_string()));
        Box::pin(async move { Ok(42) })
    });

    let code = dispatch_gui(&["gui", "--session", "demo-1", "--model", "gpt-5"], "/work/proj")
        .await
        .unwrap();
    assert_eq!(code, 42);
    let calls = recorded.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, vec!["--cwd", "/work/proj", "--session", "demo-1", "--model", "gpt-5"]);
    assert_eq!(calls[0].2, "/work/proj");
}
