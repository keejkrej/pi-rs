use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use pi_acp::run_acp_stdio;
use pi_core::agent::{AgentSession, AllowAllPermissions, EventSink, NullEventSink, SessionEvent};
use pi_core::config::SettingsManager;
use pi_core::messages::{AssistantContentBlock, UserContentBlock};
use pi_core::models::ModelDescriptor;
use pi_core::session::SessionManager;
use pi_openai::OpenAiCodexProvider;
use std::io::{self, BufRead};
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "pi")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    Login,
    Logout,
    Acp,
    Print {
        prompt: String,
        #[arg(long)]
        session: Option<String>,
    },
    Rpc {
        #[arg(long)]
        session: Option<String>,
    },
    Repl {
        #[arg(long)]
        session: Option<String>,
    },
    Sessions,
}

struct StdoutSink;

#[async_trait::async_trait(?Send)]
impl EventSink for StdoutSink {
    async fn emit(&self, event: SessionEvent) -> anyhow::Result<()> {
        match event {
            SessionEvent::AssistantMessage { content } => {
                let text = content
                    .into_iter()
                    .filter_map(|block| match block {
                        AssistantContentBlock::Text(text) => Some(text.text),
                        AssistantContentBlock::Thinking(_) | AssistantContentBlock::ToolCall(_) => {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("");
                if !text.is_empty() {
                    println!("{text}");
                }
            }
            SessionEvent::ToolCallStart {
                tool_name, title, ..
            } => {
                eprintln!("[tool:{tool_name}] {title}");
            }
            SessionEvent::ToolCallResult {
                tool_name, output, ..
            } => {
                eprintln!(
                    "[tool:{tool_name}] {}",
                    output
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            UserContentBlock::Text(text) => Some(text.text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                );
            }
            _ => {}
        }
        Ok(())
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let provider = Arc::new(OpenAiCodexProvider::new()?);

    match cli.command.unwrap_or(Commands::Repl { session: None }) {
        Commands::Login => provider.login().await,
        Commands::Logout => provider.logout(),
        Commands::Acp => run_acp_stdio().await,
        Commands::Print { prompt, session } => {
            let mut session = open_session(provider.clone(), session.as_deref()).await?;
            let sink = NullEventSink;
            let permissions = AllowAllPermissions;
            let output = session.prompt_text(prompt, &sink, &permissions).await?;
            if !output.output.is_empty() {
                println!("{}", output.output);
            }
            Ok(())
        }
        Commands::Rpc { session } => run_rpc(provider.clone(), session.as_deref()).await,
        Commands::Repl { session } => run_repl(provider.clone(), session.as_deref()).await,
        Commands::Sessions => list_sessions(),
    }
}

async fn open_session(
    provider: Arc<OpenAiCodexProvider>,
    session_id: Option<&str>,
) -> Result<AgentSession> {
    let cwd = std::env::current_dir()?;
    let settings = SettingsManager::new(&cwd)?.merged()?;
    let manager = match session_id {
        Some(session_id) => SessionManager::load_by_id(&cwd, session_id)?,
        None => SessionManager::new(&cwd)?,
    };
    let model_id = manager
        .current_model_id()
        .or(settings.default_model)
        .unwrap_or_else(|| "gpt-5.4".to_string());
    let model = ModelDescriptor::by_id(&model_id)
        .or_else(|| ModelDescriptor::defaults().into_iter().next())
        .ok_or_else(|| anyhow!("no default models available"))?;
    let thinking_level = manager
        .current_thinking_level()
        .or(settings.default_thinking_level)
        .unwrap_or_else(|| "medium".to_string());

    AgentSession::new(cwd, manager, model, thinking_level, provider)
}

async fn run_repl(provider: Arc<OpenAiCodexProvider>, session_id: Option<&str>) -> Result<()> {
    let mut session = open_session(provider.clone(), session_id).await?;
    let sink = StdoutSink;
    let permissions = AllowAllPermissions;
    println!(
        "pi-rs headless REPL. Session {}. Ctrl+D to exit.",
        session.session_manager.header().id
    );
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if line.trim() == "/exit" || line.trim() == "/quit" {
            break;
        }
        if line.trim() == "/new" {
            session = open_session(provider.clone(), None).await?;
            println!(
                "Switched to new session {}",
                session.session_manager.header().id
            );
            continue;
        }
        if let Some(next_session) = line.trim().strip_prefix("/resume ") {
            session = open_session(provider.clone(), Some(next_session.trim())).await?;
            println!("Resumed session {}", session.session_manager.header().id);
            continue;
        }
        let _ = session.prompt_text(line, &sink, &permissions).await?;
    }
    Ok(())
}

async fn run_rpc(provider: Arc<OpenAiCodexProvider>, session_id: Option<&str>) -> Result<()> {
    let mut session = open_session(provider.clone(), session_id).await?;
    let sink = NullEventSink;
    let permissions = AllowAllPermissions;
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: serde_json::Value = serde_json::from_str(&line).context("invalid rpc json")?;
        let id = request
            .get("id")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let command = request
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let response = match command {
            "prompt" => {
                let prompt = request
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let output = session.prompt_text(prompt, &sink, &permissions).await?;
                serde_json::json!({"id": id, "ok": true, "output": output.output, "stopReason": format!("{:?}", output.stop_reason)})
            }
            "session" => serde_json::json!({
                "id": id,
                "ok": true,
                "sessionId": session.session_manager.header().id,
                "path": session.session_manager.session_file().display().to_string()
            }),
            "new_session" => {
                session = open_session(provider.clone(), None).await?;
                serde_json::json!({
                    "id": id,
                    "ok": true,
                    "sessionId": session.session_manager.header().id,
                    "path": session.session_manager.session_file().display().to_string()
                })
            }
            "resume_session" => {
                let target = request
                    .get("sessionId")
                    .and_then(|v| v.as_str())
                    .context("missing sessionId")?;
                session = open_session(provider.clone(), Some(target)).await?;
                serde_json::json!({
                    "id": id,
                    "ok": true,
                    "sessionId": session.session_manager.header().id,
                    "path": session.session_manager.session_file().display().to_string()
                })
            }
            "list_sessions" => {
                let cwd = std::env::current_dir()?;
                let sessions = SessionManager::list_for_cwd(&cwd)?
                    .into_iter()
                    .map(|session| {
                        serde_json::json!({
                            "sessionId": session.id,
                            "title": session.title,
                            "updatedAt": session.updated_at,
                            "path": session.path.display().to_string(),
                        })
                    })
                    .collect::<Vec<_>>();
                serde_json::json!({"id": id, "ok": true, "sessions": sessions})
            }
            _ => {
                serde_json::json!({"id": id, "ok": false, "error": format!("unknown command {command}")})
            }
        };
        println!("{}", serde_json::to_string(&response)?);
    }
    Ok(())
}

fn list_sessions() -> Result<()> {
    let cwd = std::env::current_dir()?;
    for session in SessionManager::list_for_cwd(&cwd)? {
        println!("{}\t{}\t{}", session.id, session.updated_at, session.title);
    }
    Ok(())
}
