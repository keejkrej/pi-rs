//! Port of packages/evals/src/docker.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod entrypoint;
// @generated-mods end

use std::process::{Command, Stdio};

use indexmap::IndexMap;
use pi_js::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::plan::{DOCUMENTATION_VARIANTS, DocumentationVariant, EvalTask};

/// Image metadata built for a single documentation variant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltImage {
    pub name: String,
    pub id: String,
}

/// Built container images keyed by documentation variant.
pub type BuiltImages = IndexMap<DocumentationVariant, BuiltImage>;

/// Docker runtime settings shared across discovery and task execution invocations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DockerContext {
    pub images: BuiltImages,
    pub artifact_directory: String,
    pub auth_path: String,
    pub provider: String,
    pub model: String,
    pub runs_per_variant: i64,
}

/// Returns the crate root (`crates/pi-evals`).
pub fn package_root() -> String {
    pi_js::env::var("PI_EVALS_ROOT")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| env!("CARGO_MANIFEST_DIR").to_string())
}

/// Returns the workspace repository root.
pub fn repository_root() -> String {
    pi_js::path::resolve(&[&package_root(), "../.."])
}

fn execute(command: &str, args: &[String], capture: bool) -> Result<(i32, String)> {
    let mut cmd = Command::new(command);
    cmd.args(args)
        .current_dir(package_root())
        .env_clear()
        .envs(pi_js::env::vars());
    if capture {
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::inherit());
        let out = cmd
            .output()
            .map_err(|err| Error::from_io(err, "spawn", Some(command), None))?;
        let status = out.status.code().unwrap_or(1);
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        Ok((status, stdout))
    } else {
        let status = cmd
            .status()
            .map_err(|err| Error::from_io(err, "spawn", Some(command), None))?;
        Ok((status.code().unwrap_or(1), String::new()))
    }
}

fn require_success(command: &str, args: &[String]) -> Result<()> {
    let (status, _) = execute(command, args, false)?;
    if status != 0 {
        return Err(Error::msg(format!("{command} exited with status {status}.")));
    }
    Ok(())
}

/// Builds both `without_docs` and `with_docs` Docker images and resolves their image IDs.
pub fn build_images() -> Result<BuiltImages> {
    let repo_root = repository_root();
    let pkg_root = package_root();
    let digest = pi_js::crypto::sha256_hex(repo_root.as_bytes());
    let prefix = format!("pi-evals-{}", &digest[..12]);
    let mut images = BuiltImages::new();
    images.insert(
        DocumentationVariant::WithoutDocs,
        BuiltImage {
            name: format!("{prefix}-without-docs:local"),
            id: String::new(),
        },
    );
    images.insert(
        DocumentationVariant::WithDocs,
        BuiltImage {
            name: format!("{prefix}-with-docs:local"),
            id: String::new(),
        },
    );

    let dockerfile = pi_js::path::join(&[&pkg_root, "docker", "Dockerfile"]);
    for variant in DOCUMENTATION_VARIANTS {
        let image_name = images
            .get(&variant)
            .map(|entry| entry.name.clone())
            .ok_or_else(|| Error::msg(format!("Missing image slot for variant {variant}")))?;
        let build_args = vec![
            "build".to_string(),
            "--target".to_string(),
            variant.as_str().to_string(),
            "--tag".to_string(),
            image_name.clone(),
            "--file".to_string(),
            dockerfile.clone(),
            repo_root.clone(),
        ];
        require_success("docker", &build_args)?;

        let inspect_args = vec![
            "image".to_string(),
            "inspect".to_string(),
            "--format".to_string(),
            "{{.Id}}".to_string(),
            image_name.clone(),
        ];
        let (status, stdout) = execute("docker", &inspect_args, true)?;
        let image_id = pi_js::str16::trim(&stdout).to_string();
        if status != 0 || image_id.is_empty() {
            return Err(Error::msg(format!("Cannot inspect {image_name}.")));
        }
        if let Some(entry) = images.get_mut(&variant) {
            entry.id = image_id;
        }
    }
    Ok(images)
}

fn push_env(args: &mut Vec<String>, name: &str, value: &str) {
    args.push("--env".to_string());
    args.push(format!("{name}={value}"));
}

/// Validates and returns the host `auth.json` path containing credentials for `provider`.
pub fn require_eval_auth_file(provider: &str) -> Result<String> {
    let base_dir = pi_js::env::var("PI_CODING_AGENT_DIR")
        .filter(|v| !v.is_empty())
        .map(|d| pi_js::path::resolve(&[&d]))
        .unwrap_or_else(|| pi_js::path::join(&[&pi_js::env::home_dir(), ".pi", "agent"]));
    let path = pi_js::path::join(&[&base_dir, "auth.json"]);
    if !pi_js::fs::exists(&path) {
        return Err(Error::msg(format!("Eval authentication file does not exist: {path}")));
    }
    let stats = pi_js::fs::stat(&path)?;
    if !stats.is_file() {
        return Err(Error::msg(format!("Eval authentication file does not exist: {path}")));
    }
    let raw = pi_js::fs::read_to_string(&path)?;
    let credentials = pi_js::json::parse(&raw)
        .map_err(|err| Error::js_with_cause("Error", format!("Eval authentication file is invalid: {path}"), err))?;
    let Some(obj) = credentials.as_object() else {
        return Err(Error::msg(format!(
            "Eval authentication file has no credential for provider {provider}."
        )));
    };
    if !obj.contains_key(provider) {
        return Err(Error::msg(format!(
            "Eval authentication file has no credential for provider {provider}."
        )));
    }
    Ok(path)
}

fn docker_args(
    context: &DockerContext,
    variant: DocumentationVariant,
    output_directory: &str,
    entrypoint_args: &[String],
) -> Result<Vec<String>> {
    pi_js::fs::mkdir(output_directory, true, Some(0o700))?;
    let mut args = vec![
        "run".to_string(),
        "--rm".to_string(),
        "--read-only".to_string(),
        "--tmpfs".to_string(),
        "/tmp:rw,exec,mode=1777".to_string(),
        "--mount".to_string(),
        format!("type=bind,source={output_directory},target=/artifacts"),
    ];
    push_env(&mut args, "PI_EVAL_ARTIFACT_DIR", "/artifacts");
    push_env(
        &mut args,
        "PI_EVAL_RUNS_PER_VARIANT",
        &context.runs_per_variant.to_string(),
    );
    push_env(&mut args, "PI_EVAL_SANDBOX_UID", "65532");
    push_env(&mut args, "PI_EVAL_SANDBOX_GID", "65532");
    push_env(&mut args, "PI_PROVIDER", &context.provider);
    push_env(&mut args, "PI_MODEL", &context.model);

    #[cfg(unix)]
    {
        let uid = nix::unistd::getuid().as_raw();
        let gid = nix::unistd::getgid().as_raw();
        push_env(&mut args, "PI_EVAL_ARTIFACT_UID", &uid.to_string());
        push_env(&mut args, "PI_EVAL_ARTIFACT_GID", &gid.to_string());
    }

    args.push("--mount".to_string());
    args.push(format!(
        "type=bind,source={},target=/run/pi-eval-secrets/auth.json,readonly",
        context.auth_path
    ));
    let image_name = context
        .images
        .get(&variant)
        .map(|img| img.name.clone())
        .ok_or_else(|| Error::msg(format!("Missing Docker image for variant {variant}")))?;
    args.push(image_name);
    args.extend(entrypoint_args.iter().cloned());
    Ok(args)
}

/// Constructs a shared [`DockerContext`] for paired container runs.
pub fn create_docker_context(
    images: BuiltImages,
    artifact_directory: String,
    auth_path: String,
    provider: String,
    model: String,
    runs_per_variant: i64,
) -> DockerContext {
    DockerContext {
        images,
        artifact_directory,
        auth_path,
        provider,
        model,
        runs_per_variant,
    }
}

/// Runs container discovery for a variant and returns the written `discovered-tests.json` path.
pub fn discover_cases(
    context: &DockerContext,
    variant: DocumentationVariant,
    files: &[String],
    discovery_args: &[String],
) -> Result<String> {
    let output_directory = pi_js::path::join(&[&context.artifact_directory, "discovery", variant.as_str()]);
    let mut entrypoint_args = vec!["--discover".to_string(), "--project".to_string(), "docs".to_string()];
    entrypoint_args.extend(files.iter().cloned());
    entrypoint_args.extend(discovery_args.iter().cloned());
    let args = docker_args(context, variant, &output_directory, &entrypoint_args)?;
    let (status, _) = execute("docker", &args, false)?;
    if status != 0 {
        return Err(Error::msg(format!("{variant} eval discovery failed.")));
    }
    Ok(pi_js::path::join(&[&output_directory, "discovered-tests.json"]))
}

/// Computes the deterministic SHA-256 directory name for an [`EvalTask`].
pub fn task_directory_name(task: &EvalTask) -> Result<String> {
    let identity = pi_js::json::try_stringify(&(
        &task.eval_set,
        &task.case_id,
        task.variant,
        &task.model,
        task.run_number,
    ))?;
    Ok(pi_js::crypto::sha256_hex(identity.as_bytes()))
}

/// Runs a single isolated [`EvalTask`] container and returns its `vitest.json` report path if produced.
pub fn run_task(context: &DockerContext, task: &EvalTask) -> Result<Option<String>> {
    let task_dir = task_directory_name(task)?;
    let output_directory = pi_js::path::join(&[&context.artifact_directory, "tasks", &task_dir]);
    let test_name = format!("{} {}", task.eval_set, task.case_id);
    let exact_name = format!("^{}$", pi_js::regex::escape(&test_name));
    let entrypoint_args = vec![
        "--project".to_string(),
        "docs".to_string(),
        task.file.clone(),
        "--testNamePattern".to_string(),
        exact_name,
    ];
    let args = docker_args(context, task.variant, &output_directory, &entrypoint_args)?;
    let _ = execute("docker", &args, false)?;
    let report_path = pi_js::path::join(&[&output_directory, "vitest.json"]);
    Ok(pi_js::fs::exists(&report_path).then_some(report_path))
}
