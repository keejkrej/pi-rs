//! Port of packages/coding-agent/examples/extensions/interactive-shell.ts

use std::process::ExitCode;
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use indexmap::IndexSet;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, ExtensionMode, InlineExtension, UserBashEvent, UserBashEventResult,
    mcp_wire::BashResult,
};
use pi_tui::Component;
use serde_json::Value;

const DEFAULT_INTERACTIVE_COMMANDS: &[&str] = &[
    "vim",
    "nvim",
    "vi",
    "nano",
    "emacs",
    "pico",
    "micro",
    "helix",
    "hx",
    "kak",
    "less",
    "more",
    "most",
    "git commit",
    "git rebase",
    "git merge",
    "git cherry-pick",
    "git revert",
    "git add -p",
    "git add --patch",
    "git add -i",
    "git add --interactive",
    "git stash -p",
    "git stash --patch",
    "git reset -p",
    "git reset --patch",
    "git checkout -p",
    "git checkout --patch",
    "git difftool",
    "git mergetool",
    "htop",
    "top",
    "btop",
    "glances",
    "ranger",
    "nnn",
    "lf",
    "mc",
    "vifm",
    "tig",
    "lazygit",
    "gitui",
    "fzf",
    "sk",
    "ssh",
    "telnet",
    "mosh",
    "psql",
    "mysql",
    "sqlite3",
    "mongosh",
    "redis-cli",
    "kubectl edit",
    "kubectl exec -it",
    "docker exec -it",
    "docker run -it",
    "tmux",
    "screen",
    "ncdu",
];

static NOOP_COMPONENT: LazyLock<Arc<dyn Component>> = LazyLock::new(|| Arc::new(EmptyComponent));

struct EmptyComponent;

impl Component for EmptyComponent {
    fn render(&self, _width: usize) -> Vec<String> {
        Vec::new()
    }

    fn invalidate(&self) {}
}

fn get_interactive_commands() -> Vec<String> {
    let additional: Vec<String> = pi_js::env::var("INTERACTIVE_COMMANDS")
        .map(|val| {
            val.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let excluded: IndexSet<String> = pi_js::env::var("INTERACTIVE_EXCLUDE")
        .map(|val| val.split(',').map(|s| s.trim().to_lowercase()).collect())
        .unwrap_or_default();

    DEFAULT_INTERACTIVE_COMMANDS
        .iter()
        .map(|s| (*s).to_string())
        .chain(additional)
        .filter(|cmd| !excluded.contains(&cmd.to_lowercase()))
        .collect()
}

fn is_interactive_command(command: &str) -> bool {
    let trimmed = command.trim().to_lowercase();
    let commands = get_interactive_commands();

    for cmd in commands {
        let cmd_lower = cmd.to_lowercase();
        if trimmed == cmd_lower
            || trimmed.starts_with(&format!("{cmd_lower} "))
            || trimmed.starts_with(&format!("{cmd_lower}\t"))
        {
            return true;
        }
        if let Some(pipe_idx) = trimmed.rfind('|') {
            let after_pipe = trimmed[pipe_idx + 1..].trim();
            if after_pipe == cmd_lower || after_pipe.starts_with(&format!("{cmd_lower} ")) {
                return true;
            }
        }
    }
    false
}

pub struct InteractiveShellExtension;

#[async_trait]
impl Extension for InteractiveShellExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.on::<UserBashEvent>(Arc::new(|event, ctx| {
            Box::pin(async move {
                let mut command = event.command;
                let mut force_interactive = false;

                if let Some(rest) = command.strip_prefix("i ").or_else(|| command.strip_prefix("i\t")) {
                    force_interactive = true;
                    command = rest.trim().to_string();
                }

                if !force_interactive && !is_interactive_command(&command) {
                    return Ok(None);
                }

                if ctx.mode()? != ExtensionMode::Tui {
                    return Ok(Some(UserBashEventResult::Result {
                        result: BashResult {
                            output: "(interactive commands require TUI)".to_string(),
                            exit_code: Some(1),
                            cancelled: false,
                            truncated: false,
                            full_output_path: None,
                        },
                    }));
                }

                let command_for_spawn = command.clone();
                let value = ctx
                    .ui()?
                    .custom(
                        Box::new(move |tui, _theme, _kb, done| {
                            Box::pin(async move {
                                tui.stop(None);
                                pi_js::console::stdout_write("\x1b[2J\x1b[H");

                                let shell = pi_js::env::var("SHELL")
                                    .filter(|s| !s.is_empty())
                                    .unwrap_or_else(|| "/bin/sh".to_string());
                                let status_res = tokio::task::spawn_blocking(move || {
                                    std::process::Command::new(shell)
                                        .args(["-c", &command_for_spawn])
                                        .status()
                                })
                                .await;

                                let code = match status_res {
                                    Ok(Ok(status)) => status.code().map(i64::from),
                                    _ => Some(1),
                                };

                                tui.start();
                                tui.request_render(Some(true));

                                let json_val = match code {
                                    Some(c) => serde_json::json!(c),
                                    None => Value::Null,
                                };
                                done(json_val);

                                Ok(Arc::clone(&*NOOP_COMPONENT))
                            })
                        }),
                        None,
                    )
                    .await?;

                let exit_code = value.as_i64().unwrap_or(1);
                let output = if exit_code == 0 {
                    "(interactive command completed successfully)".to_string()
                } else {
                    format!("(interactive command exited with code {exit_code})")
                };

                Ok(Some(UserBashEventResult::Result {
                    result: BashResult {
                        output,
                        exit_code: Some(exit_code),
                        cancelled: false,
                        truncated: false,
                        full_output_path: None,
                    },
                }))
            })
        }));

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(InteractiveShellExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
