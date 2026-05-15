use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{execute, terminal};
use pi_agent::agent::{AgentSession, AllowAllPermissions, EventSink, NullEventSink, SessionEvent};
use pi_agent::config::SettingsManager;
use pi_agent::messages::{AssistantContentBlock, UserContentBlock};
use pi_agent::models::ModelDescriptor;
use pi_agent::session::SessionManager;
use pi_ai::OpenAiCodexProvider;
use pi_coding_agent::acp::run_acp_stdio;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use std::io::{self, BufRead};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
    Tui {
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        resume: bool,
    },
    Print {
        prompt: String,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        resume: bool,
    },
    Rpc {
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        resume: bool,
    },
    Repl {
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        resume: bool,
    },
    Models,
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

    match cli.command.unwrap_or(Commands::Tui {
        session: None,
        resume: false,
    }) {
        Commands::Login => provider.login().await,
        Commands::Logout => provider.logout(),
        Commands::Acp => run_acp_stdio().await,
        Commands::Tui { session, resume } => {
            run_tui(provider.clone(), session.as_deref(), resume).await
        }
        Commands::Print {
            prompt,
            session,
            resume,
        } => {
            let mut session = open_session(provider.clone(), session.as_deref(), resume).await?;
            let sink = NullEventSink;
            let permissions = AllowAllPermissions;
            let output = session.prompt_text(prompt, &sink, &permissions).await?;
            if !output.output.is_empty() {
                println!("{}", output.output);
            }
            Ok(())
        }
        Commands::Rpc { session, resume } => {
            run_rpc(provider.clone(), session.as_deref(), resume).await
        }
        Commands::Repl { session, resume } => {
            run_repl(provider.clone(), session.as_deref(), resume).await
        }
        Commands::Models => list_models(),
        Commands::Sessions => list_sessions(),
    }
}

async fn open_session(
    provider: Arc<OpenAiCodexProvider>,
    session_target: Option<&str>,
    resume_recent: bool,
) -> Result<AgentSession> {
    let cwd = std::env::current_dir()?;
    let settings = SettingsManager::new(&cwd)?.merged()?;
    let manager = match session_target {
        Some(session_target) => SessionManager::load_target(&cwd, session_target)?,
        None if resume_recent => SessionManager::continue_recent(&cwd)?,
        None => SessionManager::new(&cwd)?,
    };
    let model_id = manager
        .current_model_id()
        .or(settings.default_model)
        .unwrap_or_else(|| "gpt-5.5".to_string());
    let configured_provider = manager
        .current_model_provider()
        .or(settings.default_provider);
    let model = ModelDescriptor::resolve(configured_provider.as_deref(), Some(&model_id))
        .or_else(|| ModelDescriptor::defaults().into_iter().next())
        .ok_or_else(|| anyhow!("no default models available"))?;
    let thinking_level = manager
        .current_thinking_level()
        .or(settings.default_thinking_level)
        .unwrap_or_else(|| "medium".to_string());

    AgentSession::new(cwd, manager, model, thinking_level, provider)
}

struct TuiSink {
    lines: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait(?Send)]
impl EventSink for TuiSink {
    async fn emit(&self, event: SessionEvent) -> anyhow::Result<()> {
        let mut lines = self.lines.lock().expect("lock poisoned");
        match event {
            SessionEvent::AssistantMessage { content } => {
                let text = content
                    .into_iter()
                    .filter_map(|block| match block {
                        AssistantContentBlock::Text(text) => Some(text.text),
                        AssistantContentBlock::Thinking(thinking) => {
                            Some(format!("[thinking]\n{}", thinking.thinking))
                        }
                        AssistantContentBlock::ToolCall(_) => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                if !text.is_empty() {
                    lines.push(format!("Assistant: {text}"));
                }
            }
            SessionEvent::ToolCallStart {
                tool_name, title, ..
            } => {
                lines.push(format!("Tool {tool_name}: {title}"));
            }
            SessionEvent::ToolCallResult {
                tool_name, output, ..
            } => {
                let text = output
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        UserContentBlock::Text(text) => Some(text.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let status = if output.is_error { "failed" } else { "done" };
                lines.push(format!("Tool {tool_name} {status}: {text}"));
            }
            SessionEvent::SessionInfo { title, .. } => {
                lines.push(format!("Session: {title}"));
            }
            SessionEvent::UserMessage { .. } => {}
        }
        Ok(())
    }
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

async fn run_tui(
    provider: Arc<OpenAiCodexProvider>,
    session_target: Option<&str>,
    resume_recent: bool,
) -> Result<()> {
    let mut session = open_session(provider.clone(), session_target, resume_recent).await?;
    let _guard = TerminalGuard::enter()?;
    let backend = ratatui::backend::CrosstermBackend::new(io::stdout());
    let mut terminal = ratatui::Terminal::new(backend)?;
    let messages = Arc::new(Mutex::new(vec![format!(
        "pi · session {} · model {} · Ctrl-C/Esc to quit",
        session.session_manager.header().id,
        session.current_model().canonical_id()
    )]));
    let sink = TuiSink {
        lines: messages.clone(),
    };
    let permissions = AllowAllPermissions;
    let mut input = String::new();

    loop {
        let snapshot = messages.lock().expect("lock poisoned").join("\n\n");
        terminal.draw(|frame| {
            let [body_area, editor_area] =
                Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).areas(frame.area());
            let body = Paragraph::new(snapshot.clone())
                .block(Block::new().title("Conversation").borders(Borders::ALL))
                .wrap(Wrap { trim: false });
            let editor = Paragraph::new(input.as_str())
                .block(Block::new().title("Prompt").borders(Borders::ALL))
                .wrap(Wrap { trim: false });
            frame.render_widget(body, body_area);
            frame.render_widget(editor, editor_area);
        })?;

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }

        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match key.code {
            KeyCode::Esc => break,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Enter => {
                let prompt = input.trim().to_string();
                if prompt.is_empty() {
                    continue;
                }
                input.clear();
                messages
                    .lock()
                    .expect("lock poisoned")
                    .push(format!("You: {prompt}"));
                terminal.draw(|frame| {
                    let [body, editor] =
                        Layout::vertical([Constraint::Min(1), Constraint::Length(3)])
                            .areas(frame.area());
                    let snapshot = messages.lock().expect("lock poisoned").join("\n\n");
                    frame.render_widget(
                        Paragraph::new(snapshot)
                            .block(Block::new().title("Conversation").borders(Borders::ALL))
                            .wrap(Wrap { trim: false }),
                        body,
                    );
                    frame.render_widget(
                        Paragraph::new("waiting for model...").block(
                            Block::new()
                                .title("Prompt (running...)")
                                .borders(Borders::ALL),
                        ),
                        editor,
                    );
                })?;
                let result = session.prompt_text(prompt, &sink, &permissions).await;
                if let Err(error) = result {
                    messages
                        .lock()
                        .expect("lock poisoned")
                        .push(format!("Error: {error:#}"));
                }
            }
            KeyCode::Char(ch) => input.push(ch),
            _ => {}
        }
    }

    terminal::disable_raw_mode()?;
    Ok(())
}

async fn run_repl(
    provider: Arc<OpenAiCodexProvider>,
    session_target: Option<&str>,
    resume_recent: bool,
) -> Result<()> {
    let mut session = open_session(provider.clone(), session_target, resume_recent).await?;
    let sink = StdoutSink;
    let permissions = AllowAllPermissions;
    println!(
        "pi headless REPL. Session {}. Ctrl+D to exit.",
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
        let _ = session.prompt_text(line, &sink, &permissions).await?;
    }
    Ok(())
}

async fn run_rpc(
    provider: Arc<OpenAiCodexProvider>,
    session_target: Option<&str>,
    resume_recent: bool,
) -> Result<()> {
    let mut session = open_session(provider.clone(), session_target, resume_recent).await?;
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
                "path": session.session_manager.session_file().display().to_string(),
                "leafId": session.session_manager.get_leaf_id(),
                "model": session.current_model().id,
                "thinkingLevel": session.current_thinking_level(),
            }),
            "new_session" => {
                let parent_session = request
                    .get("parentSession")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .or_else(|| Some(session.session_manager.session_file().display().to_string()));
                session.new_session(parent_session)?;
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
                session.switch_session_target(target)?;
                serde_json::json!({
                    "id": id,
                    "ok": true,
                    "sessionId": session.session_manager.header().id,
                    "path": session.session_manager.session_file().display().to_string()
                })
            }
            "switch_session" => {
                let target = request
                    .get("session")
                    .or_else(|| request.get("sessionId"))
                    .and_then(|v| v.as_str())
                    .context("missing session")?;
                session.switch_session_target(target)?;
                serde_json::json!({
                    "id": id,
                    "ok": true,
                    "sessionId": session.session_manager.header().id,
                    "path": session.session_manager.session_file().display().to_string(),
                    "leafId": session.session_manager.get_leaf_id(),
                })
            }
            "navigate_tree" => {
                let target = request
                    .get("targetId")
                    .and_then(|v| v.as_str())
                    .context("missing targetId")?;
                let outcome = session.navigate_tree(target)?;
                serde_json::json!({
                    "id": id,
                    "ok": true,
                    "leafId": session.session_manager.get_leaf_id(),
                    "editorText": outcome.editor_text,
                    "model": session.current_model().id,
                    "thinkingLevel": session.current_thinking_level(),
                })
            }
            "fork" => {
                let entry_id = request
                    .get("entryId")
                    .and_then(|v| v.as_str())
                    .context("missing entryId")?;
                let outcome = session.fork(entry_id)?;
                serde_json::json!({
                    "id": id,
                    "ok": true,
                    "sessionId": outcome.session_id,
                    "path": outcome.session_path.display().to_string(),
                    "editorText": outcome.editor_text,
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

fn list_models() -> Result<()> {
    for model in ModelDescriptor::defaults() {
        println!(
            "{}\t{}\t{}\t{}",
            model.provider,
            model.id,
            model.api,
            model.base_url.as_deref().unwrap_or("")
        );
    }
    Ok(())
}

fn list_sessions() -> Result<()> {
    let cwd = std::env::current_dir()?;
    for session in SessionManager::list_for_cwd(&cwd)? {
        println!(
            "{}\t{}\t{}\t{}",
            session.id,
            session.updated_at,
            session.leaf_id.as_deref().unwrap_or("root"),
            session.title
        );
    }
    Ok(())
}
