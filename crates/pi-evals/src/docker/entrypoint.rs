//! Port of packages/evals/docker/entrypoint.ts

use std::process::{Command, Stdio};

use indexmap::IndexSet;
use pi_js::{Error, Result};

use crate::evals::{
    custom_provider_docs_eval, extensions_docs_eval, models_docs_eval, openai_provider_docs_eval,
    prompt_templates_docs_eval, tui_docs_eval,
};
use crate::harness::{DiscoveredTestEntry, EvalCaseDef, EvalSuite};
use crate::plan::DocumentationVariant;
use crate::report::{VitestJsonFile, VitestJsonFileStatus, VitestJsonReport, VitestJsonStatus};

fn parse_positive_id(name: &str) -> Result<u32> {
    let raw = pi_js::env::var(name).ok_or_else(|| Error::msg(format!("{name} must be a positive integer.")))?;
    match raw.parse::<i64>() {
        Ok(id) if id >= 1 && u32::try_from(id).is_ok() => Ok(id as u32),
        _ => Err(Error::msg(format!("{name} must be a positive integer."))),
    }
}

fn assert_directory_entries(path: &str, expected_entries: &[&str]) -> Result<()> {
    let mut actual = pi_js::fs::readdir(path)?;
    actual.sort();
    let mut expected: Vec<String> = expected_entries.iter().map(|s| (*s).to_string()).collect();
    expected.sort();
    if actual != expected {
        return Err(Error::msg(format!(
            "Unexpected eval image contents in {path}: {}",
            actual.join(", ")
        )));
    }
    Ok(())
}

fn assert_workspace(variant: DocumentationVariant) -> Result<()> {
    assert_directory_entries("/repo", &["bin", "package"])?;
    assert_directory_entries("/repo/bin", &["pi-evals"])?;

    let package_dir = pi_js::env::var("PI_PACKAGE_DIR")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/repo/package".to_string());
    let package_json = pi_js::path::join(&[&package_dir, "package.json"]);
    if !pi_js::fs::exists(&package_json) {
        return Err(Error::msg(format!(
            "Installed coding-agent is missing package.json at {package_json}."
        )));
    }

    match variant {
        DocumentationVariant::WithoutDocs => {
            for name in ["README.md", "CHANGELOG.md", "docs", "examples"] {
                let forbidden = pi_js::path::join(&[&package_dir, name]);
                if pi_js::fs::exists(&forbidden) {
                    return Err(Error::msg(format!("without_docs contains coding-agent {name}.")));
                }
            }
        }
        DocumentationVariant::WithDocs => {
            for rel in ["README.md", "CHANGELOG.md", "docs/models.md", "examples/README.md"] {
                let target = pi_js::path::join(&[&package_dir, rel]);
                let bytes = pi_js::fs::read_file(&target)
                    .map_err(|_| Error::msg(format!("Missing documentation: {target}")))?;
                if bytes.is_empty() {
                    return Err(Error::msg(format!("Missing documentation: {target}")));
                }
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn chown_tree(path: &str, uid: u32, gid: u32) -> Result<()> {
    let stats = pi_js::fs::lstat(path)?;
    if stats.is_directory() && !stats.is_symbolic_link() {
        for entry in pi_js::fs::readdir(path)? {
            let child = pi_js::path::join(&[path, &entry]);
            chown_tree(&child, uid, gid)?;
        }
    }
    pi_js::fs::chown(path, uid, gid)
}

#[cfg(not(unix))]
fn chown_tree(_path: &str, _uid: u32, _gid: u32) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn assert_root_only(path: &str) -> Result<()> {
    let stats = pi_js::fs::stat(path)?;
    if stats.uid != 0 || (stats.mode & 0o077) != 0 {
        return Err(Error::msg(format!(
            "Evaluator source must be readable only by root: {path}"
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn assert_root_only(_path: &str) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn assert_sandbox_cannot_read(path: &str, sandbox_uid: u32, sandbox_gid: u32) -> Result<()> {
    use std::os::unix::process::CommandExt;

    let is_dir = pi_js::fs::stat(path)?.is_directory();
    let probe_cmd = if is_dir {
        "ls \"$1\" >/dev/null"
    } else {
        "cat \"$1\" >/dev/null"
    };
    let status = Command::new("/bin/sh")
        .arg("-c")
        .arg(probe_cmd)
        .arg("sh")
        .arg(path)
        .uid(sandbox_uid)
        .gid(sandbox_gid)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|err| Error::from_io(err, "spawn", Some("/bin/sh"), None))?;
    if status.success() {
        return Err(Error::msg(format!(
            "The model-facing user can read evaluator source: {path}"
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn assert_sandbox_cannot_read(_path: &str, _sandbox_uid: u32, _sandbox_gid: u32) -> Result<()> {
    Ok(())
}

/// Returns all documentation-lift eval suites registered in the container runner.
pub fn docs_suites() -> Vec<EvalSuite> {
    vec![
        models_docs_eval::suite(),
        custom_provider_docs_eval::suite(),
        openai_provider_docs_eval::suite(),
        extensions_docs_eval::suite(),
        prompt_templates_docs_eval::suite(),
        tui_docs_eval::suite(),
    ]
}

fn parse_entrypoint_args(args: &[String]) -> Result<(bool, Vec<String>, Option<String>)> {
    let mut discover = false;
    let mut files = Vec::new();
    let mut pattern: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--discover" {
            discover = true;
            i += 1;
            continue;
        }
        if arg == "--project" {
            if i + 1 >= args.len() {
                return Err(Error::msg("Missing value for --project."));
            }
            i += 2;
            continue;
        }
        if arg == "-t" || arg == "--testNamePattern" {
            let Some(val) = args.get(i + 1) else {
                return Err(Error::msg(format!("Missing value for {arg}.")));
            };
            pattern = Some(val.clone());
            i += 2;
            continue;
        }
        if let Some(val) = arg.strip_prefix("--testNamePattern=") {
            pattern = Some(val.to_string());
            i += 1;
            continue;
        }
        if arg.ends_with(".eval.ts") || arg.ends_with("_eval.rs") {
            files.push(arg.clone());
        }
        i += 1;
    }

    if files.is_empty() {
        return Err(Error::msg("The container runner requires an explicit eval file."));
    }
    Ok((discover, files, pattern))
}

fn normalize_eval_file(file: &str) -> String {
    let normalized = file.replace('\\', "/");
    let trimmed = normalized
        .strip_prefix("/repo/packages/evals/")
        .or_else(|| normalized.strip_prefix("/repo/crates/pi-evals/"))
        .unwrap_or(&normalized);
    match trimmed {
        "src/evals/custom_provider_docs_eval.rs" | "evals/custom_provider_docs_eval.rs" => {
            "evals/custom-provider.docs.eval.ts".to_string()
        }
        "src/evals/extensions_docs_eval.rs" | "evals/extensions_docs_eval.rs" => {
            "evals/extensions.docs.eval.ts".to_string()
        }
        "src/evals/models_docs_eval.rs" | "evals/models_docs_eval.rs" => "evals/models.docs.eval.ts".to_string(),
        "src/evals/openai_provider_docs_eval.rs" | "evals/openai_provider_docs_eval.rs" => {
            "evals/openai-provider.docs.eval.ts".to_string()
        }
        "src/evals/prompt_templates_docs_eval.rs" | "evals/prompt_templates_docs_eval.rs" => {
            "evals/prompt-templates.docs.eval.ts".to_string()
        }
        "src/evals/tui_docs_eval.rs" | "evals/tui_docs_eval.rs" => "evals/tui.docs.eval.ts".to_string(),
        other => other.to_string(),
    }
}

/// Runs inside the Docker container entrypoint as root, preparing the sandbox and executing eval cases.
pub async fn run(args: &[String]) -> Result<i32> {
    let variant = match pi_js::env::var("PI_EVAL_VARIANT").as_deref() {
        Some("without_docs") => DocumentationVariant::WithoutDocs,
        Some("with_docs") => DocumentationVariant::WithDocs,
        _ => return Err(Error::msg("Eval image has no valid PI_EVAL_VARIANT.")),
    };
    let sandbox_uid = parse_positive_id("PI_EVAL_SANDBOX_UID")?;
    let sandbox_gid = parse_positive_id("PI_EVAL_SANDBOX_GID")?;

    assert_workspace(variant)?;
    assert_root_only("/repo/bin/pi-evals")?;
    assert_sandbox_cannot_read("/repo/bin/pi-evals", sandbox_uid, sandbox_gid)?;

    let agent_dir = "/tmp/pi-eval-host-agent";
    pi_js::fs::mkdir(agent_dir, true, None)?;
    let auth_source = "/run/pi-eval-secrets/auth.json";
    if pi_js::fs::exists(auth_source) {
        let dest = pi_js::path::join(&[agent_dir, "auth.json"]);
        pi_js::fs::copy_file(auth_source, &dest)?;
    }
    chown_tree(agent_dir, sandbox_uid, sandbox_gid)?;
    chown_tree("/artifacts", sandbox_uid, sandbox_gid)?;

    pi_js::env::set_var("HOME", "/tmp/pi-eval-bootstrap");
    pi_js::env::set_var("USERPROFILE", "/tmp/pi-eval-bootstrap");
    pi_js::env::set_var("PI_CODING_AGENT_DIR", agent_dir);
    pi_js::env::set_var("PI_EVAL_CONTAINER", "1");

    #[cfg(unix)]
    {
        nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o022));
    }

    let (discover, files, pattern) = parse_entrypoint_args(args)?;
    let requested: IndexSet<String> = files.iter().map(|f| normalize_eval_file(f)).collect();
    let regex = match pattern.as_deref() {
        Some(pat) => Some(pi_js::regex::ecma(pat, "")?),
        None => None,
    };

    let suites = docs_suites();
    let mut matched_cases: Vec<(String, EvalCaseDef)> = Vec::new();
    for suite in suites {
        let suite_file = normalize_eval_file(&suite.file);
        if !requested.contains(&suite_file) {
            continue;
        }
        for case_def in suite.cases {
            let full_name = case_def.full_name();
            if let Some(ref re) = regex
                && !pi_js::regex::test(re, &full_name)
            {
                continue;
            }
            matched_cases.push((suite.file.clone(), case_def));
        }
    }

    let status_code = if discover {
        let entries: Vec<DiscoveredTestEntry> = matched_cases
            .iter()
            .map(|(file, case_def)| DiscoveredTestEntry {
                name: case_def.discovered_name(),
                file: format!("/repo/packages/evals/{file}"),
            })
            .collect();
        let json = pi_js::json::try_stringify(&entries)?;
        pi_js::fs::write_file("/artifacts/discovered-tests.json", json.as_bytes(), None)?;
        0
    } else {
        let started_ms = pi_js::time::now_ms() as f64;
        let mut test_results = Vec::new();
        let mut num_failed = 0usize;
        let mut num_passed = 0usize;
        for (file, case_def) in matched_cases {
            let assertion = (case_def.execute)(None).await?;
            let failed = assertion.status == VitestJsonStatus::Failed;
            if failed {
                num_failed += 1;
            } else if assertion.status == VitestJsonStatus::Passed {
                num_passed += 1;
            }
            let end_ms = pi_js::time::now_ms() as f64;
            test_results.push(VitestJsonFile {
                message: String::new(),
                name: format!("/repo/packages/evals/{file}"),
                status: if failed {
                    VitestJsonFileStatus::Failed
                } else {
                    VitestJsonFileStatus::Passed
                },
                start_time: Some(started_ms),
                end_time: Some(end_ms),
                assertion_results: vec![assertion],
            });
        }
        let report = VitestJsonReport {
            num_failed_tests: num_failed as f64,
            num_passed_tests: num_passed as f64,
            num_pending_tests: 0.0,
            num_todo_tests: 0.0,
            num_total_tests: (num_failed + num_passed) as f64,
            start_time: started_ms,
            success: num_failed == 0,
            test_results,
        };
        let json = pi_js::json::try_stringify(&report)?;
        pi_js::fs::write_file("/artifacts/vitest.json", json.as_bytes(), None)?;
        if num_failed == 0 { 0 } else { 1 }
    };

    if pi_js::env::var("PI_EVAL_ARTIFACT_UID").is_some() && pi_js::env::var("PI_EVAL_ARTIFACT_GID").is_some() {
        let artifact_uid = parse_positive_id("PI_EVAL_ARTIFACT_UID")?;
        let artifact_gid = parse_positive_id("PI_EVAL_ARTIFACT_GID")?;
        chown_tree("/artifacts", artifact_uid, artifact_gid)?;
    }

    Ok(status_code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_eval_file_handles_container_prefixes_and_rust_modules() {
        assert_eq!(
            normalize_eval_file("/repo/packages/evals/evals/prompt-templates.docs.eval.ts"),
            "evals/prompt-templates.docs.eval.ts"
        );
        assert_eq!(
            normalize_eval_file("/repo/crates/pi-evals/src/evals/prompt_templates_docs_eval.rs"),
            "evals/prompt-templates.docs.eval.ts"
        );
    }

    #[test]
    fn parse_entrypoint_args_supports_discover_project_and_pattern() {
        let (discover, files, pattern) = parse_entrypoint_args(&[
            "--discover".to_string(),
            "--project".to_string(),
            "docs".to_string(),
            "evals/prompt-templates.docs.eval.ts".to_string(),
            "--testNamePattern".to_string(),
            "^Create a project prompt template$".to_string(),
        ])
        .unwrap();
        assert!(discover);
        assert_eq!(files, vec!["evals/prompt-templates.docs.eval.ts"]);
        assert_eq!(pattern.as_deref(), Some("^Create a project prompt template$"));
    }
}
