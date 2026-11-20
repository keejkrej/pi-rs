//! Offline tests for eval suites, CLI parsing, Docker helpers, and harness/judge integration.

use indexmap::IndexMap;
use pi_ai::providers::faux::{FauxAssistantMessageContent, FauxResponseStep, faux_assistant_message, faux_provider};
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_coding_agent::core::resource_loader::DefaultResourceLoaderOptions;
use pi_coding_agent::core::sdk::{
    CreateAgentSessionFromServicesOptions, CreateAgentSessionServicesOptions, NoToolsMode,
    create_agent_session_from_services, create_agent_session_services,
};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::settings_manager::SettingsManager;
use pi_coding_agent::core::source_info::SourceScope;
use pi_evals::cli::{default_doc_eval_files, parse_eval_cli};
use pi_evals::docker::entrypoint::docs_suites;
use pi_evals::docker::{require_eval_auth_file, task_directory_name};
use pi_evals::evals::prompt_templates_docs_eval::{
    collect_prompt_template_eval_output, prompt_template_judge, suite as prompt_templates_suite,
};
use pi_evals::evals::{documentation_audit_eval, extensions_docs_eval, smoke_eval, tui_docs_eval};
use pi_evals::harness::{
    EvalSuiteKind, ExpectedToolCall, JudgeContext, SimpleHarnessResult, StructuredOutputJudgeOptions,
    StructuredOutputMatchMode, ToolCallJudgeOptions, apply_isolated_environment, create_harness, run_eval_case,
    serialize_to_json_value, structured_output_judge, tool_call_judge,
};
use pi_evals::plan::{DocumentationVariant, EvalTask, create_task_plan, parse_discovered_cases};
use pi_evals::report::{
    EvalOutcome, PI_SESSION_SNAPSHOT_ARTIFACT, TranscriptEvent, UsageSummary, VitestJsonFile, VitestJsonFileStatus,
    VitestJsonReport, VitestJsonStatus, read_task_observation,
};
use pi_js::env::testing::EnvGuard;
use serde_json::{Value, json};
use tempfile::TempDir;

#[test]
fn cli_default_doc_eval_files_includes_prompt_templates_and_all_suites() {
    let files = default_doc_eval_files();
    assert_eq!(
        files,
        vec![
            "evals/custom-provider.docs.eval.ts",
            "evals/extensions.docs.eval.ts",
            "evals/models.docs.eval.ts",
            "evals/openai-provider.docs.eval.ts",
            "evals/prompt-templates.docs.eval.ts",
            "evals/tui.docs.eval.ts",
        ]
    );
}

#[test]
fn cli_parses_options_filters_and_env_fallbacks() {
    let mut env = IndexMap::new();
    env.insert("PI_PROVIDER".to_string(), "openai-codex".to_string());
    env.insert("PI_MODEL".to_string(), "gpt-5.6-sol".to_string());
    env.insert("PI_EVAL_RUNS_PER_VARIANT".to_string(), "3".to_string());

    let parsed = parse_eval_cli(
        &[
            "evals/prompt-templates.docs.eval.ts".to_string(),
            "-t".to_string(),
            "project".to_string(),
        ],
        Some(&env),
    )
    .unwrap();
    assert_eq!(parsed.provider.as_deref(), Some("openai-codex"));
    assert_eq!(parsed.model.as_deref(), Some("gpt-5.6-sol"));
    assert_eq!(parsed.runs_per_variant, 3);
    assert_eq!(parsed.requested_files, vec!["evals/prompt-templates.docs.eval.ts"]);
    assert_eq!(parsed.discovery_args, vec!["-t", "project"]);

    let explicit = parse_eval_cli(
        &[
            "--provider=anthropic".to_string(),
            "--model".to_string(),
            "claude-sonnet-4-5".to_string(),
            "--runs-per-variant=2".to_string(),
            "--testNamePattern=user".to_string(),
        ],
        Some(&env),
    )
    .unwrap();
    assert_eq!(explicit.provider.as_deref(), Some("anthropic"));
    assert_eq!(explicit.model.as_deref(), Some("claude-sonnet-4-5"));
    assert_eq!(explicit.runs_per_variant, 2);
    assert_eq!(explicit.discovery_args, vec!["--testNamePattern=user"]);

    let bad_arg = parse_eval_cli(&["--unknown".to_string()], Some(&env)).unwrap_err();
    assert!(bad_arg.to_string().contains("Unsupported eval argument"), "{bad_arg}");

    let bad_runs = parse_eval_cli(&["--runs-per-variant".to_string(), "0".to_string()], Some(&env)).unwrap_err();
    assert!(bad_runs.to_string().contains("positive integer"), "{bad_runs}");
}

#[test]
fn docs_suites_discover_all_seven_cases_and_plan_balanced_tasks() {
    let suites = docs_suites();
    assert_eq!(suites.len(), 6);
    assert!(suites.iter().all(|s| s.kind == EvalSuiteKind::Docs));

    let discovered: Vec<Value> = suites
        .iter()
        .flat_map(|suite| {
            suite.cases.iter().map(|case_def| {
                json!({
                    "name": case_def.discovered_name(),
                    "file": format!("/repo/packages/evals/{}", case_def.file),
                })
            })
        })
        .collect();
    let cases = parse_discovered_cases(&Value::Array(discovered)).unwrap();
    assert_eq!(cases.len(), 7);
    assert!(cases.iter().any(|c| {
        c.eval_set == "Create a project prompt template"
            && c.case_id == "creates, reloads, and invokes /current-time"
            && c.file == "/repo/packages/evals/evals/prompt-templates.docs.eval.ts"
    }));
    assert!(cases.iter().any(|c| {
        c.eval_set == "Create a user prompt template"
            && c.case_id == "creates, reloads, and invokes /current-time"
            && c.file == "/repo/packages/evals/evals/prompt-templates.docs.eval.ts"
    }));

    let tasks = create_task_plan(&cases, "fixture/model", 1).unwrap();
    assert_eq!(tasks.len(), 14);
}

#[test]
fn smoke_audit_and_doc_suite_metadata_match_v1_5_2() {
    let smoke = smoke_eval::suite();
    assert_eq!(smoke.kind, EvalSuiteKind::Host);
    assert_eq!(smoke.file, "evals/smoke.eval.ts");
    assert_eq!(smoke.cases.len(), 1);
    assert_eq!(
        smoke.cases[0].discovered_name(),
        "Answer a basic prompt > returns the expected answer"
    );

    let audit = documentation_audit_eval::suite().unwrap();
    assert_eq!(audit.kind, EvalSuiteKind::Audit);
    assert_eq!(audit.file, "evals/documentation-audit.eval.ts");
    assert!(!audit.cases.is_empty());
    assert!(
        audit
            .cases
            .iter()
            .any(|c| c.case_id == "prompt-templates.md matches the implementation")
    );

    let ext_suite = extensions_docs_eval::suite();
    assert_eq!(ext_suite.kind, EvalSuiteKind::Docs);
    assert_eq!(ext_suite.file, "evals/extensions.docs.eval.ts");
    assert_eq!(ext_suite.cases.len(), 1);

    let tui_suite = tui_docs_eval::suite();
    assert_eq!(tui_suite.kind, EvalSuiteKind::Docs);
    assert_eq!(tui_suite.file, "evals/tui.docs.eval.ts");
    assert_eq!(tui_suite.cases.len(), 1);

    let pt_suite = prompt_templates_suite();
    assert_eq!(pt_suite.kind, EvalSuiteKind::Docs);
    assert_eq!(pt_suite.file, "evals/prompt-templates.docs.eval.ts");
    assert_eq!(pt_suite.cases.len(), 2);
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial]
async fn require_eval_auth_file_validates_provider_and_task_directory_name_is_deterministic() {
    let dir = TempDir::new().unwrap();
    let agent_dir = dir.path().to_string_lossy().into_owned();
    let _guard = EnvGuard::set("PI_CODING_AGENT_DIR", Some(&agent_dir));

    let missing_err = require_eval_auth_file("acme").unwrap_err();
    assert!(missing_err.to_string().contains("does not exist"), "{missing_err}");

    let auth_path = pi_js::path::join(&[&agent_dir, "auth.json"]);
    pi_js::fs::promises::write_file(&auth_path, b"{", None).await.unwrap();
    let invalid_err = require_eval_auth_file("acme").unwrap_err();
    assert!(invalid_err.to_string().contains("invalid"), "{invalid_err}");

    pi_js::fs::promises::write_file(&auth_path, br#"{"openai":{"type":"api_key","key":"k"}}"#, None)
        .await
        .unwrap();
    let no_provider_err = require_eval_auth_file("acme").unwrap_err();
    assert!(no_provider_err.to_string().contains("acme"), "{no_provider_err}");
    assert_eq!(require_eval_auth_file("openai").unwrap(), auth_path);

    let task = EvalTask {
        file: "evals/prompt-templates.docs.eval.ts".to_string(),
        full_name: "Create a project prompt template creates, reloads, and invokes /current-time".to_string(),
        eval_set: "Create a project prompt template".to_string(),
        case_id: "creates, reloads, and invokes /current-time".to_string(),
        variant: DocumentationVariant::WithDocs,
        model: "fixture/model".to_string(),
        run_number: 1,
    };
    let hash_a = task_directory_name(&task).unwrap();
    let hash_b = task_directory_name(&task).unwrap();
    assert_eq!(hash_a, hash_b);
    assert_eq!(hash_a.len(), 64);
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial]
async fn prompt_templates_output_and_judge_verify_project_and_user_scopes_offline() {
    for scope in [SourceScope::Project, SourceScope::User] {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let workspace = pi_js::path::join(&[&root, "workspace"]);
        let isolated_home = pi_js::path::join(&[&root, "home"]);
        let agent_dir = pi_js::path::join(&[&isolated_home, ".pi", "agent"]);
        pi_js::fs::promises::mkdir(&workspace, true, None).await.unwrap();
        pi_js::fs::promises::mkdir(&agent_dir, true, None).await.unwrap();
        let _env_guard = apply_isolated_environment(&isolated_home, &agent_dir);

        let faux = faux_provider(None);
        faux.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("12:00 UTC".to_string()),
            None,
        ))]);
        let model = faux.get_model();

        let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions {
            models_path: Some(None),
            auth_path: Some(pi_js::path::join(&[&agent_dir, "auth.json"])),
            models_store_path: Some(pi_js::path::join(&[&agent_dir, "models-store.json"])),
            allow_model_network: Some(false),
            refresh_on_create: Some(false),
            ..Default::default()
        })
        .await
        .unwrap();
        model_runtime.register_native_provider(faux.provider()).unwrap();

        let settings_manager = SettingsManager::in_memory(None, None);
        let services = create_agent_session_services(CreateAgentSessionServicesOptions {
            cwd: workspace.clone(),
            agent_dir: Some(agent_dir.clone()),
            settings_manager: Some(settings_manager),
            model_runtime: Some(model_runtime),
            resource_loader_options: Some(DefaultResourceLoaderOptions::default()),
            ..Default::default()
        })
        .await
        .unwrap();

        let created = create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
            services,
            session_manager: SessionManager::in_memory(Some(&workspace), None, None).unwrap(),
            session_start_event: None,
            model: Some(model),
            thinking_level: None,
            scoped_models: None,
            tools: None,
            exclude_tools: None,
            no_tools: Some(NoToolsMode::All),
            custom_tools: None,
        })
        .await
        .unwrap();

        let prompt_dir = match scope {
            SourceScope::Project => pi_js::path::join(&[&workspace, ".pi", "prompts"]),
            SourceScope::User | SourceScope::Temporary => pi_js::path::join(&[&agent_dir, "prompts"]),
        };
        pi_js::fs::promises::mkdir(&prompt_dir, true, None).await.unwrap();
        let prompt_file = pi_js::path::join(&[&prompt_dir, "current-time.md"]);
        pi_js::fs::promises::write_file(&prompt_file, b"Tell me the current time and timezone.", None)
            .await
            .unwrap();

        created.session.reload(None).await.unwrap();
        created.session.prompt("/current-time", None).await.unwrap();

        let output = collect_prompt_template_eval_output(&created.session, &agent_dir, scope);
        assert_eq!(output.template.as_ref().map(|t| t.scope), Some(scope));
        assert_eq!(output.template.as_ref().map(|t| t.at_expected_path), Some(true));
        assert!(output.prompt_diagnostics.is_empty());
        assert!(output.extensions.is_empty());
        assert!(output.extension_errors.is_empty());
        assert!(output.skills.is_empty());
        assert!(!output.other_scope_template_exists);
        assert!(output.expanded_to_time_request);

        let judge = prompt_template_judge(scope);
        let run = pi_evals::report::HarnessRun::default();
        let pass = judge
            .assess(JudgeContext {
                input: Value::Null,
                output: Some(serialize_to_json_value(&output)),
                tool_calls: Vec::new(),
                run: run.clone(),
                session: run.session.clone(),
                signal: None,
            })
            .await
            .unwrap();
        assert_eq!(pass.score, Some(1.0));

        // Checking against the opposite scope must fail strictly.
        let opposite_scope = match scope {
            SourceScope::Project => SourceScope::User,
            SourceScope::User | SourceScope::Temporary => SourceScope::Project,
        };
        let wrong_output = collect_prompt_template_eval_output(&created.session, &agent_dir, opposite_scope);
        let fail = judge
            .assess(JudgeContext {
                input: Value::Null,
                output: Some(serialize_to_json_value(&wrong_output)),
                tool_calls: Vec::new(),
                run: run.clone(),
                session: run.session,
                signal: None,
            })
            .await
            .unwrap();
        assert_eq!(fail.score, Some(0.0));

        created.session.dispose();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn run_eval_case_and_report_observation_round_trip_offline() {
    let mut args = IndexMap::new();
    args.insert("name".to_string(), Value::String("Bob".to_string()));
    let harness = create_harness("without_docs", move |_input: String, ctx| {
        let args = args.clone();
        Box::pin(async move {
            ctx.set_artifact(
                PI_SESSION_SNAPSHOT_ARTIFACT,
                Value::String("{\"type\":\"session\"}\n".to_string()),
            );
            Ok(SimpleHarnessResult {
                output: Some(json!({
                    "response": "Hello, Bob!",
                    "extensionErrors": [],
                    "toolResult": "Hello, Bob!",
                })),
                events: vec![
                    TranscriptEvent::ToolCall {
                        id: "tc-1".to_string(),
                        name: "hello".to_string(),
                        arguments: Some(args),
                        started_at: None,
                        finished_at: None,
                        duration_ms: None,
                        metadata: None,
                    },
                    TranscriptEvent::ToolResult {
                        tool_call_id: "tc-1".to_string(),
                        name: Some("hello".to_string()),
                        content: Some(Value::String("Hello, Bob!".to_string())),
                        error: None,
                        started_at: None,
                        finished_at: None,
                        duration_ms: None,
                        metadata: None,
                    },
                ],
                session_provider: Some("fixture".to_string()),
                session_model: Some("model".to_string()),
                session_metadata: None,
                usage: UsageSummary {
                    provider: Some("fixture".to_string()),
                    model: Some("model".to_string()),
                    input_tokens: Some(10.0),
                    output_tokens: Some(5.0),
                    reasoning_tokens: None,
                    total_tokens: Some(15.0),
                    tool_calls: Some(1.0),
                    retries: None,
                    metadata: None,
                },
                timings: None,
                artifacts: None,
                traces: None,
                errors: Vec::new(),
            })
        })
    });

    let mut strict_opts = StructuredOutputJudgeOptions::new(json!({
        "response": "Hello, Bob!",
        "extensionErrors": [],
        "toolResult": "Hello, Bob!",
    }));
    strict_opts.match_mode = StructuredOutputMatchMode::Strict;
    strict_opts.allow_extras = false;

    let mut expected_args = IndexMap::new();
    expected_args.insert("name".to_string(), Value::String("Bob".to_string()));
    let judges = vec![
        structured_output_judge(strict_opts),
        tool_call_judge(ToolCallJudgeOptions {
            expected_tools: vec![ExpectedToolCall {
                name: "hello".to_string(),
                arguments: Some(expected_args),
            }],
            ..Default::default()
        }),
    ];

    let eval_set = "Create and use a tool extension";
    let case_id = "creates, reloads, and invokes the extension";
    let assertion = run_eval_case::<_, fn(&pi_evals::report::HarnessRun) -> pi_js::Result<()>>(
        eval_set,
        case_id,
        &harness,
        "run".to_string(),
        Value::String("run".to_string()),
        &judges,
        Some(None),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(assertion.status, VitestJsonStatus::Passed);

    let dir = TempDir::new().unwrap();
    let dir_path = dir.path().to_string_lossy().into_owned();
    let report_path = pi_js::path::join(&[&dir_path, "vitest.json"]);
    let report = VitestJsonReport {
        num_failed_tests: 0.0,
        num_passed_tests: 1.0,
        num_pending_tests: 0.0,
        num_todo_tests: 0.0,
        num_total_tests: 1.0,
        start_time: 0.0,
        success: true,
        test_results: vec![VitestJsonFile {
            message: String::new(),
            name: "/repo/packages/evals/evals/extensions.docs.eval.ts".to_string(),
            status: VitestJsonFileStatus::Passed,
            start_time: Some(0.0),
            end_time: Some(10.0),
            assertion_results: vec![assertion],
        }],
    };
    pi_js::fs::promises::write_file(
        &report_path,
        pi_js::json::try_stringify(&report).unwrap().as_bytes(),
        None,
    )
    .await
    .unwrap();

    let task = EvalTask {
        file: "evals/extensions.docs.eval.ts".to_string(),
        full_name: format!("{eval_set} {case_id}"),
        eval_set: eval_set.to_string(),
        case_id: case_id.to_string(),
        variant: DocumentationVariant::WithoutDocs,
        model: "fixture/model".to_string(),
        run_number: 1,
    };
    let observation = read_task_observation(&task, &report_path, &dir_path).await.unwrap();
    assert_eq!(observation.outcome, EvalOutcome::Scored { score: 1.0 });
    assert_eq!(observation.total_tokens, Some(15.0));
}
