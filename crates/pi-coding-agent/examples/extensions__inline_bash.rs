//! Port of packages/coding-agent/examples/extensions/inline-bash.ts

use std::process::ExitCode;
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    ExecOptions, Extension, ExtensionApi, ExtensionFactory, InlineExtension, InputEvent, InputEventResult, NotifyType,
};

const TIMEOUT_MS: u64 = 30_000;

static INLINE_BASH_RE: LazyLock<Option<regex::Regex>> = LazyLock::new(|| regex::Regex::new(r"!\{([^}]+)\}").ok());

struct Expansion {
    command: String,
    output: String,
    error: Option<String>,
}

pub struct InlineBashExtension;

#[async_trait]
impl Extension for InlineBashExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let pi_in = pi.clone();
        pi.on::<InputEvent>(Arc::new(move |event, ctx| {
            let pi = pi_in.clone();
            Box::pin(async move {
                let text = &event.text;
                let trimmed_start = text.trim_start();
                if trimmed_start.starts_with('!') && !trimmed_start.starts_with("!{") {
                    return Ok(Some(InputEventResult::Continue));
                }

                let Some(re) = INLINE_BASH_RE.as_ref() else {
                    return Ok(Some(InputEventResult::Continue));
                };

                let matches: Vec<(String, String)> = re
                    .captures_iter(text)
                    .filter_map(|caps| {
                        let full = caps.get(0)?.as_str().to_string();
                        let command = caps.get(1)?.as_str().to_string();
                        Some((full, command))
                    })
                    .collect();

                if matches.is_empty() {
                    return Ok(Some(InputEventResult::Continue));
                }

                let mut result = text.clone();
                let mut expansions = Vec::new();

                for (full, command) in matches {
                    match pi
                        .exec(
                            "bash",
                            &["-c".to_string(), command.clone()],
                            Some(ExecOptions {
                                signal: None,
                                timeout: Some(TIMEOUT_MS),
                                cwd: None,
                            }),
                        )
                        .await
                    {
                        Ok(bash_result) => {
                            let output = if !bash_result.stdout.is_empty() {
                                bash_result.stdout
                            } else {
                                bash_result.stderr.clone()
                            };
                            let trimmed = output.trim().to_string();
                            let error = if bash_result.code != 0 && !bash_result.stderr.is_empty() {
                                Some(format!("exit code {}", bash_result.code))
                            } else {
                                None
                            };
                            result = result.replacen(&full, &trimmed, 1);
                            expansions.push(Expansion {
                                command,
                                output: trimmed,
                                error,
                            });
                        }
                        Err(err) => {
                            let error_msg = err.to_string();
                            result = result.replacen(&full, &format!("[error: {error_msg}]"), 1);
                            expansions.push(Expansion {
                                command,
                                output: String::new(),
                                error: Some(error_msg),
                            });
                        }
                    }
                }

                if ctx.has_ui()? && !expansions.is_empty() {
                    let summary: Vec<String> = expansions
                        .iter()
                        .map(|e| {
                            let status = e.error.as_deref().map(|err| format!(" ({err})")).unwrap_or_default();
                            let preview = if e.output.chars().count() > 50 {
                                format!("{}...", e.output.chars().take(50).collect::<String>())
                            } else {
                                e.output.clone()
                            };
                            format!("!{{{}}}{status} -> \"{preview}\"", e.command)
                        })
                        .collect();
                    ctx.ui()?.notify(
                        &format!(
                            "Expanded {} inline command(s):\n{}",
                            expansions.len(),
                            summary.join("\n")
                        ),
                        Some(NotifyType::Info),
                    );
                }

                Ok(Some(InputEventResult::Transform {
                    text: result,
                    images: event.images,
                }))
            })
        }));

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(InlineBashExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
