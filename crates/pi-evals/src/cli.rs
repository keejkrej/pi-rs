//! Port of packages/evals/src/cli.ts

use std::process::ExitCode;

use indexmap::IndexMap;
use pi_js::{Error, Result};
use serde::Serialize;
use serde_json::Value;

use crate::docker::{
    BuiltImages, build_images, create_docker_context, discover_cases, package_root, require_eval_auth_file, run_task,
};
use crate::evals::{documentation_audit_eval, smoke_eval};
use crate::plan::{DOCUMENTATION_VARIANTS, DiscoveredEvalCase, EvalTask, create_task_plan, parse_discovered_cases};
use crate::report::{
    EvalObservation, VitestJsonStatus, errored_observation, format_eval_comparison_report, read_task_observation,
    summarize_eval_observations,
};

/// Parsed CLI options for documentation comparison runs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EvalCliOptions {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub runs_per_variant: i64,
    pub requested_files: Vec<String>,
    pub discovery_args: Vec<String>,
}

fn is_runner_option(name: &str) -> bool {
    matches!(name, "--provider" | "--model" | "--runs-per-variant")
}

fn is_filter_option(name: &str) -> bool {
    matches!(name, "-t" | "--testNamePattern")
}

fn trim_non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|s| pi_js::str16::trim(&s).to_string())
        .filter(|s| !s.is_empty())
}

/// Parses documentation-eval CLI options using an explicit environment map (or host env if omitted).
pub fn parse_eval_cli(args: &[String], environment: Option<&IndexMap<String, String>>) -> Result<EvalCliOptions> {
    let mut provider: Option<String> = None;
    let mut model: Option<String> = None;
    let mut runs_text: Option<String> = None;
    let mut cli_selected_model = false;
    let mut requested_files = Vec::new();
    let mut discovery_args = Vec::new();

    let mut index = 0usize;
    while index < args.len() {
        let argument = &args[index];
        if argument.ends_with(".docs.eval.ts") || argument.ends_with("_docs_eval.rs") {
            requested_files.push(argument.clone());
            index += 1;
            continue;
        }
        if is_filter_option(argument) {
            let Some(value) = args.get(index + 1) else {
                return Err(Error::msg(format!("Missing value for {argument}.")));
            };
            discovery_args.push(argument.clone());
            discovery_args.push(value.clone());
            index += 2;
            continue;
        }
        if let Some(rest) = argument.strip_prefix("--testNamePattern=") {
            if rest.is_empty() {
                return Err(Error::msg("Missing value for --testNamePattern."));
            }
            discovery_args.push(argument.clone());
            index += 1;
            continue;
        }

        let (name, inline_value) = match argument.find('=') {
            Some(eq) => (&argument[..eq], Some(&argument[eq + 1..])),
            None => (argument.as_str(), None),
        };
        if is_runner_option(name) {
            let value = match inline_value {
                Some(v) => v.to_string(),
                None => match args.get(index + 1) {
                    Some(v) if !v.starts_with('-') => v.clone(),
                    _ => return Err(Error::msg(format!("Missing value for {name}."))),
                },
            };
            if value.is_empty() {
                return Err(Error::msg(format!("Missing value for {name}.")));
            }
            if name == "--provider" {
                provider = Some(value);
            } else if name == "--model" {
                model = Some(value);
            } else {
                runs_text = Some(value);
            }
            if name != "--runs-per-variant" {
                cli_selected_model = true;
            }
            if inline_value.is_none() {
                index += 1;
            }
            index += 1;
            continue;
        }
        return Err(Error::msg(format!("Unsupported eval argument: {argument}")));
    }

    let lookup_env = |key: &str| -> Option<String> {
        match environment {
            Some(env) => env.get(key).cloned(),
            None => pi_js::env::var(key),
        }
    };

    provider = trim_non_empty(provider);
    model = trim_non_empty(model);
    if cli_selected_model {
        if provider.is_none() || model.is_none() {
            return Err(Error::msg("CLI model selection requires both --provider and --model."));
        }
    } else {
        provider = trim_non_empty(lookup_env("PI_PROVIDER"));
        model = trim_non_empty(lookup_env("PI_MODEL"));
        if provider.is_some() != model.is_some() {
            return Err(Error::msg("Set both PI_PROVIDER and PI_MODEL, or neither."));
        }
    }

    let configured_runs = trim_non_empty(runs_text.or_else(|| lookup_env("PI_EVAL_RUNS_PER_VARIANT")));
    let runs_per_variant = match configured_runs {
        Some(text) => text
            .parse::<i64>()
            .map_err(|_| Error::msg("Runs per variant must be a positive integer."))?,
        None => 1,
    };
    if runs_per_variant < 1 {
        return Err(Error::msg("Runs per variant must be a positive integer."));
    }

    Ok(EvalCliOptions {
        provider,
        model,
        runs_per_variant,
        requested_files,
        discovery_args,
    })
}

fn artifact_run_id() -> String {
    let timestamp = pi_js::time::iso_now().replace(':', "-");
    format!("{timestamp}_{}", pi_js::crypto::random_uuid())
}

fn map_rust_docs_eval_name(path: &str) -> Option<&'static str> {
    let normalized = path.replace('\\', "/");
    match normalized.rsplit('/').next()? {
        "custom_provider_docs_eval.rs" => Some("evals/custom-provider.docs.eval.ts"),
        "extensions_docs_eval.rs" => Some("evals/extensions.docs.eval.ts"),
        "models_docs_eval.rs" => Some("evals/models.docs.eval.ts"),
        "openai_provider_docs_eval.rs" => Some("evals/openai-provider.docs.eval.ts"),
        "prompt_templates_docs_eval.rs" => Some("evals/prompt-templates.docs.eval.ts"),
        "tui_docs_eval.rs" => Some("evals/tui.docs.eval.ts"),
        _ => None,
    }
}

fn container_path(pkg_root: &str, raw_path: &str) -> Result<String> {
    if raw_path.starts_with("evals/") && raw_path.ends_with(".docs.eval.ts") {
        return Ok(raw_path.replace('\\', "/"));
    }
    let absolute = pi_js::path::resolve(&[pkg_root, raw_path]);
    let pkg_relative = pi_js::path::relative(pkg_root, &absolute);
    if pkg_relative == ".." || pkg_relative.starts_with("..") || pi_js::path::is_absolute(&pkg_relative) {
        return Err(Error::msg(format!("Eval file must be inside {pkg_root}: {raw_path}")));
    }
    if let Some(mapped) = map_rust_docs_eval_name(&pkg_relative) {
        return Ok(mapped.to_string());
    }
    Ok(pkg_relative.replace('\\', "/"))
}

fn normalize_discovered_file(path: &str) -> Result<String> {
    let prefix = "/repo/packages/evals/";
    if let Some(stripped) = path.strip_prefix(prefix) {
        return Ok(stripped.to_string());
    }
    Err(Error::msg(format!(
        "Discovered eval path is outside the container package: {path}"
    )))
}

fn compare_discovery(left: &[DiscoveredEvalCase], right: &[DiscoveredEvalCase]) -> Result<()> {
    let identities = |cases: &[DiscoveredEvalCase]| -> Result<Vec<String>> {
        let mut out = Vec::with_capacity(cases.len());
        for c in cases {
            let norm = normalize_discovered_file(&c.file)?;
            out.push(pi_js::json::try_stringify(&(&c.full_name, norm))?);
        }
        out.sort();
        Ok(out)
    };
    let left_ids = identities(left)?;
    let right_ids = identities(right)?;
    if left_ids != right_ids {
        return Err(Error::msg("Documentation variants discovered different eval cases."));
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProtocolCaseSummary<'a> {
    eval_set: &'a str,
    case_id: &'a str,
    file: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EvalProtocol<'a> {
    schema_version: i64,
    model: &'a str,
    runs_per_variant: i64,
    images: &'a BuiltImages,
    files: &'a [String],
    cases: Vec<ProtocolCaseSummary<'a>>,
    tasks: &'a [EvalTask],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedEvalProtocol<'a> {
    #[serde(flatten)]
    protocol: EvalProtocol<'a>,
    protocol_digest: &'a str,
}

/// Returns the sorted list of default documentation-lift eval suite paths.
pub fn default_doc_eval_files() -> Vec<String> {
    let mut files = vec![
        "evals/custom-provider.docs.eval.ts".to_string(),
        "evals/extensions.docs.eval.ts".to_string(),
        "evals/models.docs.eval.ts".to_string(),
        "evals/openai-provider.docs.eval.ts".to_string(),
        "evals/prompt-templates.docs.eval.ts".to_string(),
        "evals/tui.docs.eval.ts".to_string(),
    ];
    files.sort();
    files
}

async fn run_local_suite(suite: crate::harness::EvalSuite) -> Result<i32> {
    let mut failed = 0usize;
    let total = suite.cases.len();
    for (idx, case_def) in suite.cases.into_iter().enumerate() {
        pi_js::console::log(&format!(
            "[{}/{}] {} > {}",
            idx + 1,
            total,
            case_def.eval_set,
            case_def.case_id
        ));
        let assertion = (case_def.execute)(None).await?;
        if assertion.status == VitestJsonStatus::Failed {
            failed += 1;
            if let Some(messages) = assertion.failure_messages {
                for msg in messages {
                    pi_js::console::error(&msg);
                }
            }
        }
    }
    Ok(if failed == 0 { 0 } else { 1 })
}

async fn run_docs_cli(args: &[String]) -> Result<i32> {
    let cli = parse_eval_cli(args, None)?;
    let (Some(provider), Some(model)) = (cli.provider, cli.model) else {
        return Err(Error::msg(
            "Set PI_PROVIDER and PI_MODEL, or pass --provider and --model.",
        ));
    };
    let model_identity = format!("{provider}/{model}");
    let pkg_root = package_root();
    let raw_files = if cli.requested_files.is_empty() {
        default_doc_eval_files()
    } else {
        cli.requested_files
    };
    let mut files = Vec::with_capacity(raw_files.len());
    for file in raw_files {
        files.push(container_path(&pkg_root, &file)?);
    }
    files.sort();
    if files.is_empty() {
        return Err(Error::msg("No documentation eval files were selected."));
    }
    for file in &files {
        if !file.ends_with(".docs.eval.ts") {
            return Err(Error::msg(format!(
                "Documentation runner cannot execute non-doc eval: {file}"
            )));
        }
    }

    let artifact_directory = pi_js::path::resolve(&[&pkg_root, ".eval", &artifact_run_id()]);
    let auth_path = require_eval_auth_file(&provider)?;
    pi_js::fs::promises::mkdir(&artifact_directory, true, Some(0o700)).await?;
    let images = build_images()?;
    let without_id = images.get(&DOCUMENTATION_VARIANTS[0]).map(|i| i.id.as_str());
    let with_id = images.get(&DOCUMENTATION_VARIANTS[1]).map(|i| i.id.as_str());
    if without_id == with_id {
        return Err(Error::msg("Documentation variants resolved to the same image."));
    }
    let docker = create_docker_context(
        images,
        artifact_directory.clone(),
        auth_path,
        provider,
        model,
        cli.runs_per_variant,
    );

    let mut discoveries = Vec::with_capacity(DOCUMENTATION_VARIANTS.len());
    for variant in DOCUMENTATION_VARIANTS {
        let path = discover_cases(&docker, variant, &files, &cli.discovery_args)?;
        let raw = pi_js::fs::promises::read_to_string(&path).await?;
        let parsed_val: Value = pi_js::json::parse(&raw)?;
        discoveries.push(parse_discovered_cases(&parsed_val)?);
    }
    compare_discovery(&discoveries[0], &discoveries[1])?;
    let mut cases = Vec::with_capacity(discoveries[0].len());
    for eval_case in discoveries.into_iter().next().unwrap_or_default() {
        let normalized_file = normalize_discovered_file(&eval_case.file)?;
        cases.push(DiscoveredEvalCase {
            file: normalized_file,
            ..eval_case
        });
    }
    if cases.is_empty() {
        return Err(Error::msg("No documentation eval cases matched the selection."));
    }

    let tasks = create_task_plan(&cases, &model_identity, cli.runs_per_variant)?;
    let protocol_cases: Vec<ProtocolCaseSummary<'_>> = cases
        .iter()
        .map(|c| ProtocolCaseSummary {
            eval_set: &c.eval_set,
            case_id: &c.case_id,
            file: &c.file,
        })
        .collect();
    let protocol = EvalProtocol {
        schema_version: 1,
        model: &model_identity,
        runs_per_variant: cli.runs_per_variant,
        images: &docker.images,
        files: &files,
        cases: protocol_cases,
        tasks: &tasks,
    };
    let protocol_text = pi_js::json::try_stringify(&protocol)?;
    let protocol_digest = pi_js::crypto::sha256_hex(protocol_text.as_bytes());
    let persisted_protocol = PersistedEvalProtocol {
        protocol,
        protocol_digest: &protocol_digest,
    };
    let protocol_path = pi_js::path::resolve(&[&artifact_directory, "protocol.json"]);
    pi_js::fs::promises::write_file(
        &protocol_path,
        format!("{}\n", pi_js::json::try_stringify_pretty(&persisted_protocol, "  ")?).as_bytes(),
        None,
    )
    .await?;

    let expected_runs_path = pi_js::path::resolve(&[&artifact_directory, "expected-runs.json"]);
    pi_js::fs::promises::write_file(
        &expected_runs_path,
        format!("{}\n", pi_js::json::try_stringify_pretty(&tasks, "  ")?).as_bytes(),
        None,
    )
    .await?;

    let mut observations: Vec<EvalObservation> = Vec::with_capacity(tasks.len());
    let observations_path = pi_js::path::resolve(&[&artifact_directory, "observations.jsonl"]);
    let mut observed_lines: Vec<String> = Vec::with_capacity(tasks.len());
    for (index, task) in tasks.iter().enumerate() {
        pi_js::console::log(&format!(
            "\n[{}/{}] {} > {} | {} | {} | run {}",
            index + 1,
            tasks.len(),
            task.eval_set,
            task.case_id,
            task.variant,
            task.model,
            task.run_number
        ));
        let report_path = run_task(&docker, task)?;
        let observation = match report_path {
            Some(path) => read_task_observation(task, &path, &artifact_directory).await?,
            None => errored_observation(task),
        };
        observed_lines.push(pi_js::json::try_stringify(&observation)?);
        observations.push(observation);
        pi_js::fs::promises::write_file(
            &observations_path,
            format!("{}\n", observed_lines.join("\n")).as_bytes(),
            None,
        )
        .await?;
    }

    let expected_runs: Vec<crate::report::EvalRunIdentity> = tasks
        .iter()
        .map(|t| crate::report::EvalRunIdentity {
            eval_set: t.eval_set.clone(),
            case_id: t.case_id.clone(),
            variant: t.variant,
            model: t.model.clone(),
            run_number: t.run_number,
        })
        .collect();
    let report = summarize_eval_observations(&protocol_digest, &expected_runs, &observations);
    let report_text = format_eval_comparison_report(&report);
    let report_json_path = pi_js::path::resolve(&[&artifact_directory, "report.json"]);
    let report_txt_path = pi_js::path::resolve(&[&artifact_directory, "report.txt"]);
    pi_js::fs::promises::write_file(
        &report_json_path,
        format!("{}\n", pi_js::json::try_stringify_pretty(&report, "  ")?).as_bytes(),
        None,
    )
    .await?;
    pi_js::fs::promises::write_file(&report_txt_path, format!("{report_text}\n").as_bytes(), None).await?;
    pi_js::console::log(&format!("\n{report_text}"));
    pi_js::console::log(&format!("\nArtifacts: {artifact_directory}"));

    Ok(if report.blocked_pairs.is_empty() { 0 } else { 1 })
}

/// Entrypoint called by the `pi-evals` binary wrapper.
pub fn run() -> ExitCode {
    pi_js::ensure_crypto_provider();
    let args: Vec<String> = std::env::args().skip(1).collect();

    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(err) => {
            pi_js::console::error(&format!("Failed to initialize async runtime: {err}"));
            return ExitCode::from(1);
        }
    };

    let outcome: Result<i32> = runtime.block_on(async move {
        if let Some(first) = args.first() {
            if first == "--entrypoint" {
                return crate::docker::entrypoint::run(&args[1..]).await;
            }
            if first == "--host" {
                return run_local_suite(smoke_eval::suite()).await;
            }
            if first == "--audit" {
                return run_local_suite(documentation_audit_eval::suite()?).await;
            }
        }
        run_docs_cli(&args).await
    });

    match outcome {
        Ok(code) => ExitCode::from(u8::try_from(code.clamp(0, 255)).unwrap_or(1)),
        Err(Error::Exit(code)) => ExitCode::from(u8::try_from(code.clamp(0, 255)).unwrap_or(1)),
        Err(err) => {
            pi_js::console::error(&err.to_js_string());
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_path_resolves_doc_eval_ts_and_maps_rust_eval_paths() {
        let pkg_root = package_root();
        assert_eq!(
            container_path(&pkg_root, "evals/prompt-templates.docs.eval.ts").unwrap(),
            "evals/prompt-templates.docs.eval.ts"
        );
        assert_eq!(
            container_path(&pkg_root, "src/evals/prompt_templates_docs_eval.rs").unwrap(),
            "evals/prompt-templates.docs.eval.ts"
        );
        let err = container_path(&pkg_root, "../outside.docs.eval.ts").unwrap_err();
        assert!(err.to_string().contains("Eval file must be inside"), "{err}");
    }
}
