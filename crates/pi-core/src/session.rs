use crate::config::sessions_root;
use crate::messages::{AgentMessage, SessionHeader, SessionMessageEntry};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SessionEntry {
    #[serde(rename = "message")]
    Message {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        message: AgentMessage,
    },
    #[serde(rename = "model_change")]
    ModelChange {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        provider: String,
        #[serde(rename = "modelId")]
        model_id: String,
    },
    #[serde(rename = "thinking_level_change")]
    ThinkingLevelChange {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(rename = "thinkingLevel")]
        thinking_level: String,
    },
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub path: PathBuf,
    pub cwd: PathBuf,
    pub created: DateTime<Utc>,
    pub modified: DateTime<Utc>,
    pub updated_at: String,
    pub title: String,
    pub first_message: String,
    pub current_model_id: Option<String>,
    pub current_thinking_level: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SessionManager {
    session_file: PathBuf,
    header: SessionHeader,
    entries: Vec<SessionEntry>,
}

impl SessionManager {
    pub fn new(cwd: &Path) -> Result<Self> {
        let dir = session_dir_for(cwd)?;
        fs::create_dir_all(&dir)?;
        let filename = format!(
            "{}_{}.jsonl",
            Utc::now().format("%Y%m%d%H%M%S"),
            Uuid::new_v4()
        );
        let session_file = dir.join(filename);
        let header = SessionHeader::new(cwd.display().to_string());
        let manager = Self {
            session_file,
            header,
            entries: Vec::new(),
        };
        manager.persist()?;
        register_session_path(cwd, &manager.header.id, &manager.session_file)?;
        Ok(manager)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read session {}", path.display()))?;
        let mut lines = content.lines();
        let header: SessionHeader =
            serde_json::from_str(lines.next().context("missing session header")?)?;
        let mut entries = Vec::new();
        for line in lines {
            if line.trim().is_empty() {
                continue;
            }
            entries.push(serde_json::from_str(line)?);
        }
        Ok(Self {
            session_file: path.to_path_buf(),
            header,
            entries,
        })
    }

    pub fn load_by_id(cwd: &Path, session_id: &str) -> Result<Self> {
        let dir = session_dir_for(cwd)?;
        if !dir.exists() {
            anyhow::bail!("no sessions found for {}", cwd.display());
        }

        if let Some(indexed) = lookup_indexed_session_path(cwd, session_id)? {
            if indexed.exists() {
                return Self::load(&indexed);
            }
        }

        let mut found = None;
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let manager = match Self::load(&path) {
                Ok(manager) => manager,
                Err(_) => continue,
            };
            register_session_path(cwd, &manager.header.id, &path)?;
            if manager.header.id == session_id {
                found = Some(manager);
            }
        }

        found.with_context(|| format!("session {session_id} not found for {}", cwd.display()))
    }

    pub fn list_for_cwd(cwd: &Path) -> Result<Vec<SessionInfo>> {
        let dir = session_dir_for(cwd)?;
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut sessions = Vec::new();
        let mut index = load_session_index(cwd).unwrap_or_default();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Ok(manager) = Self::load(&path) {
                let metadata = entry.metadata()?;
                index.sessions.insert(
                    manager.header.id.clone(),
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default()
                        .to_string(),
                );
                sessions.push(manager.build_session_info(metadata));
            }
        }
        store_session_index(cwd, &index)?;
        sessions.sort_by(|a, b| b.modified.cmp(&a.modified));
        Ok(sessions)
    }

    pub fn header(&self) -> &SessionHeader {
        &self.header
    }

    pub fn session_file(&self) -> &Path {
        &self.session_file
    }

    pub fn entries(&self) -> &[SessionEntry] {
        &self.entries
    }

    pub fn session_info(&self) -> Result<SessionInfo> {
        let metadata = fs::metadata(&self.session_file)?;
        Ok(self.build_session_info(metadata))
    }

    pub fn title(&self) -> String {
        let first_message = self.first_user_message_text();
        let trimmed = first_message.trim();
        if trimmed.is_empty() {
            format!("Session {}", &self.header.id[..8])
        } else {
            trimmed.chars().take(80).collect()
        }
    }

    pub fn current_model_id(&self) -> Option<String> {
        self.entries.iter().rev().find_map(|entry| match entry {
            SessionEntry::ModelChange { model_id, .. } => Some(model_id.clone()),
            SessionEntry::Message {
                message:
                    AgentMessage::Assistant {
                        model, provider, ..
                    },
                ..
            } if provider == "openai-codex" => Some(model.clone()),
            _ => None,
        })
    }

    pub fn current_thinking_level(&self) -> Option<String> {
        self.entries.iter().rev().find_map(|entry| match entry {
            SessionEntry::ThinkingLevelChange { thinking_level, .. } => {
                Some(thinking_level.clone())
            }
            _ => None,
        })
    }

    pub fn messages(&self) -> Vec<AgentMessage> {
        self.entries
            .iter()
            .filter_map(|entry| match entry {
                SessionEntry::Message { message, .. } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn push_message(&mut self, message: AgentMessage) -> Result<()> {
        let parent_id = self.last_entry_id();
        let entry = SessionMessageEntry::new(parent_id, message);
        self.entries.push(SessionEntry::Message {
            id: entry.id,
            parent_id: entry.parent_id,
            timestamp: entry.timestamp,
            message: entry.message,
        });
        self.persist()
    }

    pub fn push_model_change(
        &mut self,
        provider: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Result<()> {
        self.entries.push(SessionEntry::ModelChange {
            id: short_id(),
            parent_id: self.last_entry_id(),
            timestamp: Utc::now().to_rfc3339(),
            provider: provider.into(),
            model_id: model_id.into(),
        });
        self.persist()
    }

    pub fn push_thinking_level_change(&mut self, thinking_level: impl Into<String>) -> Result<()> {
        self.entries.push(SessionEntry::ThinkingLevelChange {
            id: short_id(),
            parent_id: self.last_entry_id(),
            timestamp: Utc::now().to_rfc3339(),
            thinking_level: thinking_level.into(),
        });
        self.persist()
    }

    pub fn persist(&self) -> Result<()> {
        if let Some(parent) = self.session_file.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut lines = vec![serde_json::to_string(&self.header)?];
        for entry in &self.entries {
            lines.push(serde_json::to_string(entry)?);
        }
        fs::write(&self.session_file, lines.join("\n") + "\n")?;
        Ok(())
    }

    fn build_session_info(&self, metadata: fs::Metadata) -> SessionInfo {
        let created =
            DateTime::<Utc>::from(metadata.created().unwrap_or(std::time::SystemTime::now()));
        let modified =
            DateTime::<Utc>::from(metadata.modified().unwrap_or(std::time::SystemTime::now()));
        SessionInfo {
            id: self.header.id.clone(),
            path: self.session_file.clone(),
            cwd: PathBuf::from(&self.header.cwd),
            created,
            modified,
            updated_at: modified.to_rfc3339(),
            title: self.title(),
            first_message: self.first_user_message_text(),
            current_model_id: self.current_model_id(),
            current_thinking_level: self.current_thinking_level(),
        }
    }

    fn first_user_message_text(&self) -> String {
        self.entries
            .iter()
            .find_map(|entry| match entry {
                SessionEntry::Message {
                    message: AgentMessage::User { .. },
                    ..
                } => Some(entry_text(entry)),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn last_entry_id(&self) -> Option<String> {
        self.entries.last().map(|entry| match entry {
            SessionEntry::Message { id, .. }
            | SessionEntry::ModelChange { id, .. }
            | SessionEntry::ThinkingLevelChange { id, .. } => id.clone(),
        })
    }
}

fn short_id() -> String {
    Uuid::new_v4().simple().to_string()[..8].to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SessionIndex {
    sessions: BTreeMap<String, String>,
}

fn session_dir_for(cwd: &Path) -> Result<PathBuf> {
    let slug = cwd.display().to_string().replace('/', "-");
    Ok(sessions_root()?.join(format!("--{}--", slug)))
}

fn session_index_path(cwd: &Path) -> Result<PathBuf> {
    Ok(session_dir_for(cwd)?.join("index.json"))
}

fn load_session_index(cwd: &Path) -> Result<SessionIndex> {
    let path = session_index_path(cwd)?;
    if !path.exists() {
        return Ok(SessionIndex::default());
    }
    let content = fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&content)?)
}

fn store_session_index(cwd: &Path, index: &SessionIndex) -> Result<()> {
    let path = session_index_path(cwd)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(index)?)?;
    Ok(())
}

fn register_session_path(cwd: &Path, session_id: &str, session_file: &Path) -> Result<()> {
    let mut index = load_session_index(cwd).unwrap_or_default();
    let filename = session_file
        .file_name()
        .and_then(|name| name.to_str())
        .context("session file missing filename")?;
    index
        .sessions
        .insert(session_id.to_string(), filename.to_string());
    store_session_index(cwd, &index)
}

fn lookup_indexed_session_path(cwd: &Path, session_id: &str) -> Result<Option<PathBuf>> {
    let index = load_session_index(cwd)?;
    let dir = session_dir_for(cwd)?;
    Ok(index
        .sessions
        .get(session_id)
        .map(|filename| dir.join(filename)))
}

fn entry_text(entry: &SessionEntry) -> String {
    match entry {
        SessionEntry::Message { message, .. } => message.as_text(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{TextContent, UserContentBlock};

    #[test]
    fn session_roundtrip_preserves_messages() {
        let temp = std::env::temp_dir().join(format!("pi-rs-session-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&temp).unwrap();
        let mut manager = SessionManager::new(&temp).unwrap();
        manager
            .push_message(AgentMessage::User {
                content: vec![UserContentBlock::Text(TextContent::new("hello"))],
                timestamp: 1,
            })
            .unwrap();

        let loaded = SessionManager::load(manager.session_file()).unwrap();
        assert_eq!(loaded.messages().len(), 1);
        assert_eq!(loaded.messages()[0].as_text(), "hello");
    }

    #[test]
    fn load_by_id_uses_index_and_repairs_missing_index_entries() {
        let temp = std::env::temp_dir().join(format!("pi-rs-session-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&temp).unwrap();
        let manager = SessionManager::new(&temp).unwrap();
        let session_id = manager.header().id.clone();

        let loaded = SessionManager::load_by_id(&temp, &session_id).unwrap();
        assert_eq!(loaded.header().id, session_id);

        std::fs::remove_file(session_index_path(&temp).unwrap()).unwrap();
        let repaired = SessionManager::load_by_id(&temp, &session_id).unwrap();
        assert_eq!(repaired.header().id, session_id);
        assert!(session_index_path(&temp).unwrap().exists());
    }

    #[test]
    fn session_info_tracks_title_and_last_selected_model() {
        let temp = std::env::temp_dir().join(format!("pi-rs-session-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&temp).unwrap();
        let mut manager = SessionManager::new(&temp).unwrap();
        manager
            .push_message(AgentMessage::User {
                content: vec![UserContentBlock::Text(TextContent::new(
                    "Implement ACP integration for Zed",
                ))],
                timestamp: 1,
            })
            .unwrap();
        manager
            .push_model_change("openai-codex", "gpt-5.4-mini")
            .unwrap();
        manager.push_thinking_level_change("high").unwrap();

        let info = manager.session_info().unwrap();
        assert_eq!(info.title, "Implement ACP integration for Zed");
        assert_eq!(info.current_model_id.as_deref(), Some("gpt-5.4-mini"));
        assert_eq!(info.current_thinking_level.as_deref(), Some("high"));
    }
}
