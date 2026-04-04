use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use pi_acp::run_acp_stdio;
use pi_core::agent::{AgentSession, AllowAllPermissions, EventSink, NullEventSink, SessionEvent};
use pi_core::config::SettingsManager;
use pi_core::models::ModelDescriptor;
use pi_core::session::SessionManager;
use pi_openai::OpenAiCodexProvider;
use std::io::{self, BufRead, Write};
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
    Print { prompt: String },
    Rpc,
    Repl,
}

struct StdoutSink;

#[async_trait::async_trait(?Send)]
impl EventSink for StdoutSink {
    async fn emit(&self, event: SessionEvent) -> anyhow::Result<()> {
        match event {
            SessionEvent::AssistantMessage(text) => {
                println!("{text}");
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
                            pi_core::messages::UserContentBlock::Text(text) =>
                                Some(text.text.as_str()),
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

    match cli.command.unwrap_or(Commands::Repl) {
        Commands::Login => provider.login().await,
        Commands::Logout => provider.logout(),
        Commands::Acp => run_acp_stdio().await,
        Commands::Print { prompt } => {
            let mut session = open_session(provider.clone()).await?;
            let sink = NullEventSink;
            let permissions = AllowAllPermissions;
            let output = session.prompt(prompt, &sink, &permissions).await?;
            if !output.is_empty() {
                println!("{output}");
            }
            Ok(())
        }
        Commands::Rpc => run_rpc(provider.clone()).await,
        Commands::Repl => run_repl(provider.clone()).await,
    }
}

async fn open_session(provider: Arc<OpenAiCodexProvider>) -> Result<AgentSession> {
    let cwd = std::env::current_dir()?;
    let settings = SettingsManager::new(&cwd)?.merged()?;
    let default_model = settings
        .default_model
        .unwrap_or_else(|| "gpt-5.4".to_string());
    let model = ModelDescriptor::defaults()
        .into_iter()
        .find(|model| model.id == default_model)
        .unwrap_or_else(|| ModelDescriptor::defaults().into_iter().next().unwrap());
    AgentSession::new(
        cwd.clone(),
        SessionManager::new(&cwd)?,
        model,
        settings
            .default_thinking_level
            .unwrap_or_else(|| "medium".to_string()),
        provider,
    )
}

async fn run_repl(provider: Arc<OpenAiCodexProvider>) -> Result<()> {
    let mut session = open_session(provider).await?;
    let sink = StdoutSink;
    let permissions = AllowAllPermissions;
    println!("pi-rs headless REPL. Ctrl+D to exit.");
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if line.trim() == "/exit" || line.trim() == "/quit" {
            break;
        }
        let output = session.prompt(line, &sink, &permissions).await?;
        if !output.is_empty() {
            writeln!(stdout, "{output}")?;
        }
        write!(stdout, "> ")?;
        stdout.flush()?;
    }
    Ok(())
}

async fn run_rpc(provider: Arc<OpenAiCodexProvider>) -> Result<()> {
    let mut session = open_session(provider).await?;
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
                let output = session.prompt(prompt, &sink, &permissions).await?;
                serde_json::json!({"id": id, "ok": true, "output": output})
            }
            "session" => serde_json::json!({
                "id": id,
                "ok": true,
                "sessionId": session.session_manager.header().id,
                "path": session.session_manager.session_file().display().to_string()
            }),
            _ => {
                serde_json::json!({"id": id, "ok": false, "error": format!("unknown command {command}")})
            }
        };
        println!("{}", serde_json::to_string(&response)?);
    }
    Ok(())
}
