use crate::config::sessions_root;
use crate::messages::{AgentMessage, SessionHeader, SessionMessageEntry};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
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
    pub first_message: String,
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

    pub fn list_for_cwd(cwd: &Path) -> Result<Vec<SessionInfo>> {
        let dir = session_dir_for(cwd)?;
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut sessions = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.path().extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Ok(manager) = Self::load(&entry.path()) {
                let metadata = entry.metadata()?;
                let first_message = manager
                    .entries
                    .iter()
                    .find_map(|entry| match entry {
                        SessionEntry::Message { message, .. } => Some(message.as_text()),
                        _ => None,
                    })
                    .unwrap_or_default();
                sessions.push(SessionInfo {
                    id: manager.header.id.clone(),
                    path: entry.path(),
                    cwd: PathBuf::from(&manager.header.cwd),
                    created: DateTime::<Utc>::from(
                        metadata.created().unwrap_or(std::time::SystemTime::now()),
                    ),
                    modified: DateTime::<Utc>::from(
                        metadata.modified().unwrap_or(std::time::SystemTime::now()),
                    ),
                    first_message,
                });
            }
        }
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

fn session_dir_for(cwd: &Path) -> Result<PathBuf> {
    let slug = cwd.display().to_string().replace('/', "-");
    Ok(sessions_root()?.join(format!("--{}--", slug)))
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
}
