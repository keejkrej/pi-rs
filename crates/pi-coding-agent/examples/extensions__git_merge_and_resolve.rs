//! Port of packages/coding-agent/examples/extensions/git-merge-and-resolve.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    AgentEndEvent, CustomMessageContent, Extension, ExtensionApi, ExtensionFactory, InlineExtension,
    InputDeliveryBehavior, NotifyType, SendUserMessageOptions,
};

struct ConflictBlock {
    file: String,
    start_line: usize,
    separator_line: usize,
    end_line: usize,
}

async fn find_conflicts(pi: &ExtensionApi, cwd: &str) -> pi_js::Result<Vec<ConflictBlock>> {
    let res = pi
        .exec(
            "git",
            &[
                "diff".to_string(),
                "--name-only".to_string(),
                "--diff-filter=U".to_string(),
            ],
            None,
        )
        .await?;
    if res.code != 0 || res.stdout.trim().is_empty() {
        return Ok(Vec::new());
    }

    let mut blocks = Vec::new();
    for file in res.stdout.trim().lines() {
        let full_path = pi_js::path::join(&[cwd, file]);
        let Ok(content) = pi_js::fs::promises::read_to_string(&full_path).await else {
            continue;
        };
        let mut block_start: Option<usize> = None;
        let mut separator_line: Option<usize> = None;
        for (idx, line) in content.lines().enumerate() {
            let line_no = idx + 1;
            if line.starts_with("<<<<<<<") {
                block_start = Some(line_no);
                separator_line = None;
            } else if line.starts_with("=======") && block_start.is_some() {
                separator_line = Some(line_no);
            } else if line.starts_with(">>>>>>>")
                && let (Some(start), Some(sep)) = (block_start, separator_line)
            {
                blocks.push(ConflictBlock {
                    file: file.to_string(),
                    start_line: start,
                    separator_line: sep,
                    end_line: line_no,
                });
                block_start = None;
                separator_line = None;
            }
        }
    }
    Ok(blocks)
}

fn format_range(start: usize, end: usize) -> String {
    if start > end {
        "empty".to_string()
    } else if start == end {
        start.to_string()
    } else {
        format!("{start}-{end}")
    }
}

fn format_conflicts(r#ref: &str, blocks: &[ConflictBlock]) -> String {
    let mut lines = vec![format!("Merged {ref} with conflicts:"), String::new()];
    for b in blocks {
        let ours = format_range(b.start_line + 1, b.separator_line.saturating_sub(1));
        let theirs = format_range(b.separator_line + 1, b.end_line.saturating_sub(1));
        lines.push(format!(
            "  {}:{}-{} (ours {ours}, theirs {theirs})",
            b.file, b.start_line, b.end_line
        ));
    }
    lines.push(String::new());
    lines.push("Resolve these conflicts.".to_string());
    lines.join("\n")
}

pub struct GitMergeAndResolveExtension;

#[async_trait]
impl Extension for GitMergeAndResolveExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        let pi_ae = pi.clone();
        pi.on::<AgentEndEvent>(Arc::new(move |_event, ctx| {
            let pi = pi_ae.clone();
            Box::pin(async move {
                let rev_parse = pi
                    .exec("git", &["rev-parse".to_string(), "--git-dir".to_string()], None)
                    .await?;
                if rev_parse.code != 0 {
                    return Ok(None);
                }

                let mut merge_ref = "MERGE_HEAD".to_string();
                let merge_head = pi
                    .exec("git", &["rev-parse".to_string(), "MERGE_HEAD".to_string()], None)
                    .await?;

                if merge_head.code != 0 {
                    let status = pi
                        .exec("git", &["status".to_string(), "--porcelain".to_string()], None)
                        .await?;
                    if !status.stdout.trim().is_empty() {
                        return Ok(None);
                    }

                    let upstream = pi
                        .exec(
                            "git",
                            &[
                                "rev-parse".to_string(),
                                "--abbrev-ref".to_string(),
                                "--symbolic-full-name".to_string(),
                                "@{u}".to_string(),
                            ],
                            None,
                        )
                        .await?;
                    if upstream.code != 0 {
                        return Ok(None);
                    }

                    merge_ref = upstream.stdout.trim().to_string();
                    let remote = merge_ref.split('/').next().unwrap_or("").to_string();
                    let ui = ctx.ui()?;
                    ui.notify(
                        &format!("git-merge-and-resolve: fetching {remote}, merging {merge_ref}"),
                        Some(NotifyType::Info),
                    );

                    let fetch_res = pi.exec("git", &["fetch".to_string(), remote], None).await?;
                    if fetch_res.code != 0 {
                        ui.notify(
                            &format!("git-merge-and-resolve: fetch failed: {}", fetch_res.stderr.trim()),
                            Some(NotifyType::Warning),
                        );
                        return Ok(None);
                    }

                    let merge_res = pi
                        .exec(
                            "git",
                            &["merge".to_string(), "--no-ff".to_string(), merge_ref.clone()],
                            None,
                        )
                        .await?;
                    if merge_res.code == 0 {
                        return Ok(None);
                    }
                }

                let conflicts = find_conflicts(&pi, &ctx.cwd()?).await?;
                if conflicts.is_empty() {
                    return Ok(None);
                }

                pi.send_user_message(
                    CustomMessageContent::Text(format_conflicts(&merge_ref, &conflicts)),
                    Some(SendUserMessageOptions {
                        deliver_as: Some(InputDeliveryBehavior::FollowUp),
                        expand_prompt_templates: None,
                    }),
                )?;

                Ok(None)
            })
        }));

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(GitMergeAndResolveExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
