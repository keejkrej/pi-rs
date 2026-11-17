//! Port of packages/coding-agent/examples/extensions/ssh.ts

use std::process::{ExitCode, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    BeforeAgentStartEvent, BeforeAgentStartEventResult, Extension, ExtensionApi, ExtensionFactory, FlagOptions,
    FlagType, FlagValue, InlineExtension, NotifyType, SessionStartEvent, UserBashEvent, UserBashEventResult,
};
use pi_coding_agent::core::tools::tool_definition_wrapper::create_tool_definition_from_agent_tool;
use pi_coding_agent::core::tools::{
    BashExecOptions, BashExecResult, BashOperations, BashToolOptions, EditOperations, EditToolOptions, ReadOperations,
    ReadToolOptions, WriteOperations, WriteToolOptions, create_bash_tool, create_edit_tool, create_read_tool,
    create_write_tool,
};
use pi_js::BoxFuture;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

#[derive(Clone)]
struct SshTarget {
    remote: String,
    remote_cwd: String,
}

fn to_remote_path(path: &str, local_cwd: &str, remote_cwd: &str) -> String {
    if local_cwd.is_empty() {
        path.to_string()
    } else {
        path.replacen(local_cwd, remote_cwd, 1)
    }
}

fn shell_quote(value: &str) -> pi_js::Result<String> {
    pi_js::json::try_stringify(&serde_json::Value::String(value.to_string()))
}

async fn ssh_exec(remote: &str, command: &str) -> pi_js::Result<Vec<u8>> {
    let output = Command::new("ssh")
        .args([remote, command])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| pi_js::Error::msg(e.to_string()))?;

    if !output.status.success() {
        let code = output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "null".to_string());
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(pi_js::Error::msg(format!("SSH failed ({code}): {stderr}")));
    }

    Ok(output.stdout)
}

#[derive(Clone)]
struct RemoteReadOps {
    remote: String,
    remote_cwd: String,
    local_cwd: String,
}

impl ReadOperations for RemoteReadOps {
    fn read_file(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<Vec<u8>>> {
        let remote = self.remote.clone();
        let target = to_remote_path(absolute_path, &self.local_cwd, &self.remote_cwd);
        Box::pin(async move {
            let quoted = shell_quote(&target)?;
            ssh_exec(&remote, &format!("cat {quoted}")).await
        })
    }

    fn access(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<()>> {
        let remote = self.remote.clone();
        let target = to_remote_path(absolute_path, &self.local_cwd, &self.remote_cwd);
        Box::pin(async move {
            let quoted = shell_quote(&target)?;
            ssh_exec(&remote, &format!("test -r {quoted}")).await?;
            Ok(())
        })
    }

    fn detect_image_mime_type(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<Option<String>>> {
        let remote = self.remote.clone();
        let target = to_remote_path(absolute_path, &self.local_cwd, &self.remote_cwd);
        Box::pin(async move {
            let Ok(quoted) = shell_quote(&target) else {
                return Ok(None);
            };
            match ssh_exec(&remote, &format!("file --mime-type -b {quoted}")).await {
                Ok(bytes) => {
                    let mime = String::from_utf8_lossy(&bytes).trim().to_string();
                    if matches!(mime.as_str(), "image/jpeg" | "image/png" | "image/gif" | "image/webp") {
                        Ok(Some(mime))
                    } else {
                        Ok(None)
                    }
                }
                Err(_) => Ok(None),
            }
        })
    }
}

#[derive(Clone)]
struct RemoteWriteOps {
    remote: String,
    remote_cwd: String,
    local_cwd: String,
}

impl WriteOperations for RemoteWriteOps {
    fn write_file(&self, absolute_path: &str, content: &str) -> BoxFuture<pi_js::Result<()>> {
        let remote = self.remote.clone();
        let target = to_remote_path(absolute_path, &self.local_cwd, &self.remote_cwd);
        let encoded = pi_js::b64::encode(content.as_bytes());
        Box::pin(async move {
            let b64_q = shell_quote(&encoded)?;
            let path_q = shell_quote(&target)?;
            ssh_exec(&remote, &format!("echo {b64_q} | base64 -d > {path_q}")).await?;
            Ok(())
        })
    }

    fn mkdir(&self, dir: &str) -> BoxFuture<pi_js::Result<()>> {
        let remote = self.remote.clone();
        let target = to_remote_path(dir, &self.local_cwd, &self.remote_cwd);
        Box::pin(async move {
            let quoted = shell_quote(&target)?;
            ssh_exec(&remote, &format!("mkdir -p {quoted}")).await?;
            Ok(())
        })
    }
}

#[derive(Clone)]
struct RemoteEditOps {
    read: RemoteReadOps,
    write: RemoteWriteOps,
}

impl EditOperations for RemoteEditOps {
    fn read_file(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<Vec<u8>>> {
        self.read.read_file(absolute_path)
    }

    fn write_file(&self, absolute_path: &str, content: &str) -> BoxFuture<pi_js::Result<()>> {
        self.write.write_file(absolute_path, content)
    }

    fn access(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<()>> {
        self.read.access(absolute_path)
    }
}

#[derive(Clone)]
struct RemoteBashOps {
    remote: String,
    remote_cwd: String,
    local_cwd: String,
}

impl BashOperations for RemoteBashOps {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a str,
        options: BashExecOptions,
    ) -> BoxFuture<pi_js::Result<BashExecResult>> {
        let remote = self.remote.clone();
        let target_cwd = to_remote_path(cwd, &self.local_cwd, &self.remote_cwd);
        let command = command.to_string();
        Box::pin(async move {
            if options.signal.as_ref().is_some_and(|s| s.aborted()) {
                return Err(pi_js::Error::msg("aborted"));
            }

            let cwd_q = shell_quote(&target_cwd)?;
            let full_cmd = format!("cd {cwd_q} && {command}");

            let mut child = Command::new("ssh")
                .args([&remote, &full_cmd])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .map_err(|e| pi_js::Error::msg(e.to_string()))?;

            let stdout = child.stdout.take();
            let stderr = child.stderr.take();
            let on_data_out = options.on_data.clone();
            let on_data_err = options.on_data.clone();

            let out_task = tokio::spawn(async move {
                if let Some(mut reader) = stdout {
                    let mut buf = [0u8; 4096];
                    while let Ok(n) = reader.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        if let Some(cb) = &on_data_out {
                            cb(&buf[..n]);
                        }
                    }
                }
            });

            let err_task = tokio::spawn(async move {
                if let Some(mut reader) = stderr {
                    let mut buf = [0u8; 4096];
                    while let Ok(n) = reader.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        if let Some(cb) = &on_data_err {
                            cb(&buf[..n]);
                        }
                    }
                }
            });

            let mut timed_out = false;
            let mut aborted = false;

            let sleep_fut = match options.timeout {
                Some(t) if t > 0.0 => Some(tokio::time::sleep(Duration::from_millis((t * 1000.0) as u64))),
                _ => None,
            };
            tokio::pin!(sleep_fut);

            let wait_res = tokio::select! {
                res = child.wait() => Some(res),
                _ = async {
                    if let Some(sig) = &options.signal {
                        sig.cancelled().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    aborted = true;
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    None
                }
                _ = async {
                    if let Some(fut) = sleep_fut.as_mut().as_pin_mut() {
                        fut.await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    timed_out = true;
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    None
                }
            };

            let _ = out_task.await;
            let _ = err_task.await;

            if aborted || options.signal.as_ref().is_some_and(|s| s.aborted()) {
                return Err(pi_js::Error::msg("aborted"));
            }
            if timed_out {
                let t = options.timeout.unwrap_or(0.0);
                return Err(pi_js::Error::msg(format!("timeout:{t}")));
            }

            let status = wait_res
                .transpose()
                .map_err(|e| pi_js::Error::msg(e.to_string()))?
                .ok_or_else(|| pi_js::Error::msg("SSH child exited unexpectedly"))?;

            Ok(BashExecResult {
                exit_code: status.code().map(|c| c as i64),
            })
        })
    }
}

pub struct SshExtension;

#[async_trait]
impl Extension for SshExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.register_flag(
            "ssh",
            FlagOptions {
                description: Some("SSH remote: user@host or user@host:/path".to_string()),
                r#type: Some(FlagType::String),
                default: None,
            },
        )?;

        let local_cwd = pi_js::env::cwd();
        let local_read = create_read_tool(&local_cwd, None);
        let local_write = create_write_tool(&local_cwd, None);
        let local_edit = create_edit_tool(&local_cwd, None);
        let local_bash = create_bash_tool(&local_cwd, None);

        let resolved_ssh: Arc<Mutex<Option<SshTarget>>> = Arc::new(Mutex::new(None));

        // Override read tool
        let mut read_def = create_tool_definition_from_agent_tool(local_read.clone());
        let ssh_read = Arc::clone(&resolved_ssh);
        let cwd_read = local_cwd.clone();
        read_def.execute = Arc::new(move |id, params, signal, on_update, _ctx| {
            let ssh = ssh_read.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let cwd = cwd_read.clone();
            let fallback = Arc::clone(&local_read.execute);
            Box::pin(async move {
                if let Some(ssh) = ssh {
                    let tool = create_read_tool(
                        &cwd,
                        Some(ReadToolOptions {
                            operations: Some(Arc::new(RemoteReadOps {
                                remote: ssh.remote,
                                remote_cwd: ssh.remote_cwd,
                                local_cwd: cwd.clone(),
                            })),
                            ..Default::default()
                        }),
                    );
                    return (tool.execute)(id, params, signal, on_update)
                        .await
                        .map_err(|e| pi_js::Error::msg(e.to_string()));
                }
                fallback(id, params, signal, on_update)
                    .await
                    .map_err(|e| pi_js::Error::msg(e.to_string()))
            })
        });
        pi.register_tool(read_def)?;

        // Override write tool
        let mut write_def = create_tool_definition_from_agent_tool(local_write.clone());
        let ssh_write = Arc::clone(&resolved_ssh);
        let cwd_write = local_cwd.clone();
        write_def.execute = Arc::new(move |id, params, signal, on_update, _ctx| {
            let ssh = ssh_write.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let cwd = cwd_write.clone();
            let fallback = Arc::clone(&local_write.execute);
            Box::pin(async move {
                if let Some(ssh) = ssh {
                    let tool = create_write_tool(
                        &cwd,
                        Some(WriteToolOptions {
                            operations: Some(Arc::new(RemoteWriteOps {
                                remote: ssh.remote,
                                remote_cwd: ssh.remote_cwd,
                                local_cwd: cwd.clone(),
                            })),
                        }),
                    );
                    return (tool.execute)(id, params, signal, on_update)
                        .await
                        .map_err(|e| pi_js::Error::msg(e.to_string()));
                }
                fallback(id, params, signal, on_update)
                    .await
                    .map_err(|e| pi_js::Error::msg(e.to_string()))
            })
        });
        pi.register_tool(write_def)?;

        // Override edit tool
        let mut edit_def = create_tool_definition_from_agent_tool(local_edit.clone());
        let ssh_edit = Arc::clone(&resolved_ssh);
        let cwd_edit = local_cwd.clone();
        edit_def.execute = Arc::new(move |id, params, signal, on_update, _ctx| {
            let ssh = ssh_edit.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let cwd = cwd_edit.clone();
            let fallback = Arc::clone(&local_edit.execute);
            Box::pin(async move {
                if let Some(ssh) = ssh {
                    let read = RemoteReadOps {
                        remote: ssh.remote.clone(),
                        remote_cwd: ssh.remote_cwd.clone(),
                        local_cwd: cwd.clone(),
                    };
                    let write = RemoteWriteOps {
                        remote: ssh.remote,
                        remote_cwd: ssh.remote_cwd,
                        local_cwd: cwd.clone(),
                    };
                    let tool = create_edit_tool(
                        &cwd,
                        Some(EditToolOptions {
                            operations: Some(Arc::new(RemoteEditOps { read, write })),
                        }),
                    );
                    return (tool.execute)(id, params, signal, on_update)
                        .await
                        .map_err(|e| pi_js::Error::msg(e.to_string()));
                }
                fallback(id, params, signal, on_update)
                    .await
                    .map_err(|e| pi_js::Error::msg(e.to_string()))
            })
        });
        pi.register_tool(edit_def)?;

        // Override bash tool
        let mut bash_def = create_tool_definition_from_agent_tool(local_bash.clone());
        let ssh_bash = Arc::clone(&resolved_ssh);
        let cwd_bash = local_cwd.clone();
        bash_def.execute = Arc::new(move |id, params, signal, on_update, _ctx| {
            let ssh = ssh_bash.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let cwd = cwd_bash.clone();
            let fallback = Arc::clone(&local_bash.execute);
            Box::pin(async move {
                if let Some(ssh) = ssh {
                    let tool = create_bash_tool(
                        &cwd,
                        Some(BashToolOptions {
                            operations: Some(Arc::new(RemoteBashOps {
                                remote: ssh.remote,
                                remote_cwd: ssh.remote_cwd,
                                local_cwd: cwd.clone(),
                            })),
                            ..Default::default()
                        }),
                    );
                    return (tool.execute)(id, params, signal, on_update)
                        .await
                        .map_err(|e| pi_js::Error::msg(e.to_string()));
                }
                fallback(id, params, signal, on_update)
                    .await
                    .map_err(|e| pi_js::Error::msg(e.to_string()))
            })
        });
        pi.register_tool(bash_def)?;

        // Resolve SSH target on session_start
        let pi_start = pi.clone();
        let ssh_start = Arc::clone(&resolved_ssh);
        pi.on::<SessionStartEvent>(move |_event, ctx| {
            let pi_handle = pi_start.clone();
            let ssh_state = Arc::clone(&ssh_start);
            Box::pin(async move {
                if let Ok(Some(FlagValue::String(arg))) = pi_handle.get_flag("ssh")
                    && !arg.is_empty()
                {
                    let target = if let Some((remote, path)) = arg.split_once(':') {
                        SshTarget {
                            remote: remote.to_string(),
                            remote_cwd: path.to_string(),
                        }
                    } else {
                        let pwd_bytes = ssh_exec(&arg, "pwd").await?;
                        let pwd = String::from_utf8_lossy(&pwd_bytes).trim().to_string();
                        SshTarget {
                            remote: arg,
                            remote_cwd: pwd,
                        }
                    };

                    if let Ok(ui) = ctx.ui() {
                        let theme = ui.theme();
                        ui.set_status(
                            "ssh",
                            Some(&theme.fg("accent", &format!("SSH: {}:{}", target.remote, target.remote_cwd))),
                        );
                        ui.notify(
                            &format!("SSH mode: {}:{}", target.remote, target.remote_cwd),
                            Some(NotifyType::Info),
                        );
                    }
                    *ssh_state.lock().unwrap_or_else(|e| e.into_inner()) = Some(target);
                }
                Ok(None)
            })
        });

        // Handle user ! commands via SSH
        let ssh_user_bash = Arc::clone(&resolved_ssh);
        let cwd_user_bash = local_cwd.clone();
        pi.on::<UserBashEvent>(move |_event, _ctx| {
            let ssh = ssh_user_bash.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let cwd = cwd_user_bash.clone();
            Box::pin(async move {
                let Some(ssh) = ssh else {
                    return Ok(None);
                };
                Ok(Some(UserBashEventResult::Operations {
                    operations: Arc::new(RemoteBashOps {
                        remote: ssh.remote,
                        remote_cwd: ssh.remote_cwd,
                        local_cwd: cwd,
                    }),
                }))
            })
        });

        // Replace local cwd with remote cwd in system prompt
        let ssh_prompt = Arc::clone(&resolved_ssh);
        let cwd_prompt = local_cwd;
        pi.on::<BeforeAgentStartEvent>(move |event, _ctx| {
            let ssh = ssh_prompt.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let cwd = cwd_prompt.clone();
            Box::pin(async move {
                let Some(ssh) = ssh else {
                    return Ok(None);
                };
                let modified = event.system_prompt.replace(
                    &format!("Current working directory: {cwd}"),
                    &format!(
                        "Current working directory: {} (via SSH: {})",
                        ssh.remote_cwd, ssh.remote
                    ),
                );
                Ok(Some(BeforeAgentStartEventResult {
                    message: None,
                    system_prompt: Some(modified),
                }))
            })
        });

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(SshExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
