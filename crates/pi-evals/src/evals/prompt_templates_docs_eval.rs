//! Port of packages/evals/evals/prompt-templates.docs.eval.ts

use std::sync::Arc;

use pi_agent_core::AgentMessage;
use pi_ai::content_text;
use pi_coding_agent::core::agent_session::AgentSession;
use pi_coding_agent::core::source_info::SourceScope;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::harness::{
    EvalCaseDef, EvalSuite, EvalSuiteKind, Judge, PiCodingAgentHarnessOptions, PiCodingAgentInput, PiCodingAgentStep,
    StructuredOutputJudgeOptions, StructuredOutputMatchMode, create_pi_documentation_eval_harness, run_eval_case,
    serialize_to_json_value, structured_output_judge,
};

const TEMPLATE_NAME: &str = "current-time";
const COMMAND: &str = "/current-time";
const TEMPLATE_PROMPT: &str = "Tell me the current time and timezone.";

struct PromptTemplateScenario {
    title: &'static str,
    request: &'static str,
    scope: SourceScope,
}

const SCENARIOS: [PromptTemplateScenario; 2] = [
    PromptTemplateScenario {
        title: "Create a project prompt template",
        request: "This project is already trusted. Add a /current-time prompt template for this project containing exactly: Tell me the current time and timezone.",
        scope: SourceScope::Project,
    },
    PromptTemplateScenario {
        title: "Create a user prompt template",
        request: "Make a /current-time prompt template available in all my Pi sessions containing exactly: Tell me the current time and timezone.",
        scope: SourceScope::User,
    },
];

/// Matched template summary captured by [`collect_prompt_template_eval_output`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptTemplateRecord {
    pub scope: SourceScope,
    pub at_expected_path: bool,
}

/// Structured output emitted by the prompt-templates documentation eval.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptTemplateEvalOutput {
    pub template: Option<PromptTemplateRecord>,
    pub prompt_diagnostics: Vec<String>,
    pub extensions: Vec<String>,
    pub extension_errors: Vec<String>,
    pub skills: Vec<String>,
    pub other_scope_template_exists: bool,
    pub expanded_to_time_request: bool,
}

/// Inspects loaded prompt templates, extensions, skills, and expanded prompt text for `scope`.
pub fn collect_prompt_template_eval_output(
    session: &AgentSession,
    agent_dir: &str,
    scope: SourceScope,
) -> PromptTemplateEvalOutput {
    let workspace = session.session_manager().get_cwd();
    let file_name = format!("{TEMPLATE_NAME}.md");
    let project_path = pi_js::path::join(&[&workspace, ".pi", "prompts", &file_name]);
    let user_path = pi_js::path::join(&[agent_dir, "prompts", &file_name]);
    let (expected_path, other_path) = match scope {
        SourceScope::Project => (project_path, user_path),
        SourceScope::User | SourceScope::Temporary => (user_path, project_path),
    };

    let (prompts, diagnostics) = session.resource_loader().get_prompts();
    let extensions = session.resource_loader().get_extensions();
    let matching_templates: Vec<_> = prompts.into_iter().filter(|p| p.name == TEMPLATE_NAME).collect();
    let template = if matching_templates.len() == 1 {
        let matched = &matching_templates[0];
        Some(PromptTemplateRecord {
            scope: matched.source_info.scope,
            at_expected_path: matched.file_path == expected_path,
        })
    } else {
        None
    };

    let messages = session.messages();
    let expanded_prompt = messages.iter().rev().find_map(AgentMessage::as_user);
    let expanded_text = expanded_prompt
        .map(|user| pi_js::str16::trim(&content_text(&user.content, None)).to_string())
        .unwrap_or_default();
    let (skills, _) = session.resource_loader().get_skills();

    PromptTemplateEvalOutput {
        template,
        prompt_diagnostics: diagnostics.into_iter().map(|d| d.message).collect(),
        extensions: extensions
            .extensions
            .into_iter()
            .filter(|ext| !ext.hidden)
            .map(|ext| ext.path)
            .collect(),
        extension_errors: extensions.errors.into_iter().map(|err| err.error).collect(),
        skills: skills.into_iter().map(|s| s.name).collect(),
        other_scope_template_exists: pi_js::fs::exists(&other_path),
        expanded_to_time_request: expanded_text == TEMPLATE_PROMPT,
    }
}

/// Builds the strict [`StructuredOutputJudge`](crate::harness::structured_output_judge) for `scope`.
pub fn prompt_template_judge(scope: SourceScope) -> Judge {
    let mut judge_opts = StructuredOutputJudgeOptions::new(json!({
        "template": {
            "scope": scope,
            "atExpectedPath": true,
        },
        "promptDiagnostics": [],
        "extensions": [],
        "extensionErrors": [],
        "skills": [],
        "otherScopeTemplateExists": false,
        "expandedToTimeRequest": true,
    }));
    judge_opts.match_mode = StructuredOutputMatchMode::Strict;
    judge_opts.allow_extras = false;
    structured_output_judge(judge_opts)
}

/// Returns the documentation-lift eval suite for creating and invoking prompt templates.
pub fn suite() -> EvalSuite {
    let file = "evals/prompt-templates.docs.eval.ts".to_string();
    let case_id = format!("creates, reloads, and invokes {COMMAND}");
    let cases = SCENARIOS
        .iter()
        .map(|scenario| {
            let eval_set = scenario.title.to_string();
            let eval_set_clone = eval_set.clone();
            let case_id_clone = case_id.clone();
            let request = scenario.request;
            let scope = scenario.scope;
            EvalCaseDef {
                eval_set,
                case_id: case_id.clone(),
                file: file.clone(),
                execute: Arc::new(move |signal| {
                    let eval_set = eval_set_clone.clone();
                    let case_id = case_id_clone.clone();
                    Box::pin(async move {
                        let harness = create_pi_documentation_eval_harness(PiCodingAgentHarnessOptions {
                            output: Some(Arc::new(move |session, _response, _system_prompt, agent_dir| {
                                let out = collect_prompt_template_eval_output(session, agent_dir, scope);
                                Box::pin(async move { Ok(serialize_to_json_value(&out)) })
                            })),
                            ..Default::default()
                        })?;

                        let judges = vec![prompt_template_judge(scope)];
                        let steps = vec![
                            PiCodingAgentStep::Prompt {
                                content: request.to_string(),
                            },
                            PiCodingAgentStep::Reload,
                            PiCodingAgentStep::Prompt {
                                content: COMMAND.to_string(),
                            },
                        ];
                        let input_json = serialize_to_json_value(&steps);
                        run_eval_case::<_, fn(&crate::report::HarnessRun) -> pi_js::Result<()>>(
                            &eval_set,
                            &case_id,
                            &harness,
                            PiCodingAgentInput::Steps(steps),
                            input_json,
                            &judges,
                            Some(None),
                            signal,
                            None,
                        )
                        .await
                    })
                }),
            }
        })
        .collect();

    EvalSuite {
        file,
        kind: EvalSuiteKind::Docs,
        cases,
    }
}
