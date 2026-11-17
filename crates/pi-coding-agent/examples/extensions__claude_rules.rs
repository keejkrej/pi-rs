//! Port of packages/coding-agent/examples/extensions/claude-rules.ts

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    BeforeAgentStartEvent, BeforeAgentStartEventResult, Extension, ExtensionApi, ExtensionFactory, InlineExtension,
    NotifyType, SessionStartEvent,
};

fn find_markdown_files(dir: &str, base_path: &str) -> Vec<String> {
    let mut results = Vec::new();
    if !pi_js::fs::exists(dir) {
        return results;
    }
    let Ok(entries) = pi_js::fs::readdir_with_file_types(dir) else {
        return results;
    };
    for entry in entries {
        let relative_path = if base_path.is_empty() {
            entry.name.clone()
        } else {
            format!("{base_path}/{}", entry.name)
        };
        if entry.is_directory() {
            results.extend(find_markdown_files(
                &pi_js::path::join(&[dir, &entry.name]),
                &relative_path,
            ));
        } else if entry.is_file() && entry.name.ends_with(".md") {
            results.push(relative_path);
        }
    }
    results
}

pub struct ClaudeRulesExtension;

#[async_trait]
impl Extension for ClaudeRulesExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let rule_files: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        {
            let rule_files_ss = Arc::clone(&rule_files);
            pi.on::<SessionStartEvent>(Arc::new(move |_event, ctx| {
                let rule_files = Arc::clone(&rule_files_ss);
                Box::pin(async move {
                    let rules_dir = pi_js::path::join(&[&ctx.cwd()?, ".claude", "rules"]);
                    let found = find_markdown_files(&rules_dir, "");
                    let count = found.len();
                    {
                        let mut guard = rule_files.lock().unwrap_or_else(|e| e.into_inner());
                        *guard = found;
                    }
                    if count > 0 {
                        ctx.ui()?.notify(
                            &format!("Found {count} rule(s) in .claude/rules/"),
                            Some(NotifyType::Info),
                        );
                    }
                    Ok(None)
                })
            }));
        }

        {
            let rule_files_bas = Arc::clone(&rule_files);
            pi.on::<BeforeAgentStartEvent>(Arc::new(move |event, _ctx| {
                let rule_files = Arc::clone(&rule_files_bas);
                Box::pin(async move {
                    let files = rule_files.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    if files.is_empty() {
                        return Ok(None);
                    }
                    let rules_list = files
                        .iter()
                        .map(|f| format!("- .claude/rules/{f}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let appendix = format!(
                        "\n\n## Project Rules\n\nThe following project rules are available in .claude/rules/:\n\n{rules_list}\n\nWhen working on tasks related to these rules, use the read tool to load the relevant rule files for guidance.\n"
                    );
                    Ok(Some(BeforeAgentStartEventResult {
                        message: None,
                        system_prompt: Some(format!("{}{}", event.system_prompt, appendix)),
                    }))
                })
            }));
        }

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(ClaudeRulesExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
