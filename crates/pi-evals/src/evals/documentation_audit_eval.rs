//! Port of packages/evals/evals/documentation-audit.eval.ts

use std::sync::Arc;

use pi_agent_core::AgentToolResult;
use pi_ai::{ConstrainedSamplingConfig, JsonSchemaStrict, UserContentBlock, faux_text};
use pi_coding_agent::core::extensions::types::{ConstrainedSamplingOption, ToolDefinition, define_tool};
use pi_js::vendor::typebox::t;
use pi_js::{Error, Result};
use serde_json::Value;

use crate::docker::repository_root;
use crate::harness::{
    EvalCaseDef, EvalSuite, EvalSuiteKind, PiCodingAgentHarnessOptions, PiCodingAgentInput,
    create_pi_coding_agent_harness, run_eval_case, tool_calls,
};

const TOOL_NAME: &str = "submit_documentation_audit";

fn submit_audit_tool() -> ToolDefinition {
    let parameters = t::object([
        (
            "verdict",
            t::union([t::literal("match"), t::literal("mismatch"), t::literal("inconclusive")]),
        ),
        ("explanation", t::string().with("minLength", 1).with("maxLength", 2000)),
    ])
    .with("additionalProperties", false);

    define_tool(ToolDefinition {
        name: TOOL_NAME.to_string(),
        label: "Submit documentation audit".to_string(),
        description: "Submit the final verdict after completing the documentation investigation.".to_string(),
        prompt_snippet: Some("Submit the final documentation audit as validated structured data".to_string()),
        prompt_guidelines: None,
        parameters,
        constrained_sampling: Some(ConstrainedSamplingOption::Config(
            ConstrainedSamplingConfig::JsonSchema {
                strict: JsonSchemaStrict::Prefer,
            },
        )),
        render_shell: None,
        prepare_arguments: None,
        output_schema: None,
        exposure: None,
        namespace: None,
        annotations: None,
        default_active: None,
        prepare_loadout: None,
        execution_mode: None,
        execute: Arc::new(|_tool_call_id, params, _signal, _on_update, _ctx| {
            Box::pin(async move {
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(faux_text("Documentation audit submitted."))],
                    details: Some(params),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: Some(true),
                    control: None,
                })
            })
        }),
        render_call: None,
        render_result: None,
    })
}

fn collect_markdown_pages(root: &str, current_rel: &str, out: &mut Vec<String>) -> Result<()> {
    let dir = if current_rel.is_empty() {
        root.to_string()
    } else {
        pi_js::path::join(&[root, current_rel])
    };
    if !pi_js::fs::exists(&dir) {
        return Ok(());
    }
    for entry in pi_js::fs::readdir_with_file_types(&dir)? {
        let rel = if current_rel.is_empty() {
            entry.name.clone()
        } else {
            format!("{current_rel}/{}", entry.name)
        };
        if entry.is_directory() {
            collect_markdown_pages(root, &rel, out)?;
        } else if entry.is_file() && rel.ends_with(".md") {
            out.push(rel.replace('\\', "/"));
        }
    }
    Ok(())
}

/// Returns the documentation-audit host suite checking every markdown doc page against implementation source.
pub fn suite() -> Result<EvalSuite> {
    let repo_root = repository_root();
    let docs_root = pi_js::path::resolve(&[&repo_root, "crates/pi-coding-agent/package/docs"]);
    let source_root = pi_js::path::resolve(&[&repo_root, "crates/pi-coding-agent/src"]);
    let mut pages = Vec::new();
    collect_markdown_pages(&docs_root, "", &mut pages)?;
    pages.sort_by(|left, right| pi_js::intl::locale_compare(left, right));

    let eval_set = "Audit documentation against implementation".to_string();
    let file = "evals/documentation-audit.eval.ts".to_string();
    let cases = pages
        .into_iter()
        .map(|page| {
            let case_id = format!("{page} matches the implementation");
            let documentation_path = pi_js::path::resolve(&[&docs_root, &page]);
            let prompt = format!(
                "Audit this Pi documentation page against the repository implementation.\n\nDocumentation page: {documentation_path}\nRepository root: {repo_root}\n\nRead the complete page and the relevant implementation. Report a mismatch only for a clear, user-visible contradiction between an explicit documentation claim and actual runtime behavior. Follow the runtime path; names, comments, types, isolated helpers, and tests are not sufficient evidence by themselves.\n\nMissing internal detail, ambiguous wording, hypothetical misuse, and undocumented edge cases are not mismatches. If the evidence is not decisive, report inconclusive. Otherwise report match.\n\nFor a mismatch, quote the claim, cite the implementation path and symbol, and state the concrete behavior a user would observe.\n\nCall {TOOL_NAME} exactly once as your final action. Do not return prose."
            );
            let eval_set_ref = eval_set.clone();
            let case_id_ref = case_id.clone();
            let source_root_ref = source_root.clone();
            let file_ref = file.clone();

            EvalCaseDef {
                eval_set: eval_set.clone(),
                case_id,
                file: file_ref,
                execute: Arc::new(move |signal| {
                    let eval_set = eval_set_ref.clone();
                    let case_id = case_id_ref.clone();
                    let documentation_path = documentation_path.clone();
                    let source_root = source_root_ref.clone();
                    let prompt = prompt.clone();
                    Box::pin(async move {
                        let harness = create_pi_coding_agent_harness(PiCodingAgentHarnessOptions {
                            name: Some("documentation-page-audit".to_string()),
                            tools: Some(vec![
                                "read".to_string(),
                                "grep".to_string(),
                                "find".to_string(),
                                "ls".to_string(),
                                TOOL_NAME.to_string(),
                            ]),
                            custom_tools: Some(vec![submit_audit_tool()]),
                            ..Default::default()
                        });

                        run_eval_case(
                            &eval_set,
                            &case_id,
                            &harness,
                            PiCodingAgentInput::Text(prompt.clone()),
                            Value::String(prompt),
                            &[],
                            None,
                            signal,
                            Some(move |run: &crate::report::HarnessRun| {
                                let calls = tool_calls(run);
                                let read_paths: Vec<String> = calls
                                    .iter()
                                    .filter(|c| c.name() == "read" && c.is_ok())
                                    .filter_map(|c| c.arguments()?.get("path")?.as_str())
                                    .map(|p| pi_js::path::resolve(&[p]))
                                    .collect();
                                if !read_paths.contains(&documentation_path) {
                                    return Err(Error::msg(format!(
                                        "Audit did not read documentation page: {documentation_path}"
                                    )));
                                }
                                let sep = pi_js::path::sep();
                                let escaped_prefix = format!("..{sep}");
                                let read_source = read_paths.iter().any(|read_path| {
                                    let rel = pi_js::path::relative(&source_root, read_path);
                                    !rel.is_empty()
                                        && !rel.starts_with(&escaped_prefix)
                                        && rel != ".."
                                        && !pi_js::path::is_absolute(&rel)
                                });
                                if !read_source {
                                    return Err(Error::msg(format!(
                                        "Audit did not read any implementation source under {source_root}"
                                    )));
                                }

                                let audit_calls: Vec<_> =
                                    calls.into_iter().filter(|c| c.name() == TOOL_NAME).collect();
                                if audit_calls.len() != 1 {
                                    return Err(Error::msg(format!(
                                        "Expected 1 {TOOL_NAME} call, got {}",
                                        audit_calls.len()
                                    )));
                                }
                                let audit_call = &audit_calls[0];
                                if !audit_call.is_ok() {
                                    return Err(Error::msg(format!("{TOOL_NAME} call did not succeed")));
                                }
                                let args = audit_call.arguments();
                                let verdict = args.and_then(|m| m.get("verdict")).and_then(Value::as_str);
                                let explanation = args.and_then(|m| m.get("explanation")).and_then(Value::as_str);
                                if verdict != Some("match") {
                                    return Err(Error::msg(
                                        explanation
                                            .unwrap_or("Documentation audit verdict was not \"match\"")
                                            .to_string(),
                                    ));
                                }
                                Ok(())
                            }),
                        )
                        .await
                    })
                }),
            }
        })
        .collect();

    Ok(EvalSuite {
        file,
        kind: EvalSuiteKind::Audit,
        cases,
    })
}
