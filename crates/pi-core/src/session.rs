use crate::config::sessions_root;
use crate::messages::{AgentMessage, SessionHeader};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedModel {
    pub provider: String,
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionContext {
    pub messages: Vec<AgentMessage>,
    pub thinking_level: String,
    pub model: Option<SavedModel>,
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
    pub name: Option<String>,
    pub parent_session_path: Option<String>,
    pub first_message: String,
    pub current_model_id: Option<String>,
    pub current_thinking_level: Option<String>,
    pub leaf_id: Option<String>,
}

#[derive(Debug, Clone)]
pub enum SessionEntry {
    Message {
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        message: AgentMessage,
    },
    ModelChange {
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        provider: String,
        model_id: String,
    },
    ThinkingLevelChange {
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        thinking_level: String,
    },
    Compaction {
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        summary: String,
        first_kept_entry_id: String,
        tokens_before: u64,
        details: Option<Value>,
        from_hook: Option<bool>,
    },
    BranchSummary {
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        from_id: String,
        summary: String,
        details: Option<Value>,
        from_hook: Option<bool>,
    },
    Label {
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        target_id: String,
        label: Option<String>,
    },
    SessionInfo {
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        name: Option<String>,
    },
    Unknown {
        raw: Value,
    },
}

impl SessionEntry {
    pub fn id(&self) -> Option<&str> {
        match self {
            SessionEntry::Message { id, .. }
            | SessionEntry::ModelChange { id, .. }
            | SessionEntry::ThinkingLevelChange { id, .. }
            | SessionEntry::Compaction { id, .. }
            | SessionEntry::BranchSummary { id, .. }
            | SessionEntry::Label { id, .. }
            | SessionEntry::SessionInfo { id, .. } => Some(id),
            SessionEntry::Unknown { raw } => raw.get("id").and_then(Value::as_str),
        }
    }

    pub fn parent_id(&self) -> Option<&str> {
        match self {
            SessionEntry::Message { parent_id, .. }
            | SessionEntry::ModelChange { parent_id, .. }
            | SessionEntry::ThinkingLevelChange { parent_id, .. }
            | SessionEntry::Compaction { parent_id, .. }
            | SessionEntry::BranchSummary { parent_id, .. }
            | SessionEntry::Label { parent_id, .. }
            | SessionEntry::SessionInfo { parent_id, .. } => parent_id.as_deref(),
            SessionEntry::Unknown { raw } => raw.get("parentId").and_then(Value::as_str),
        }
    }

    pub fn timestamp(&self) -> Option<&str> {
        match self {
            SessionEntry::Message { timestamp, .. }
            | SessionEntry::ModelChange { timestamp, .. }
            | SessionEntry::ThinkingLevelChange { timestamp, .. }
            | SessionEntry::Compaction { timestamp, .. }
            | SessionEntry::BranchSummary { timestamp, .. }
            | SessionEntry::Label { timestamp, .. }
            | SessionEntry::SessionInfo { timestamp, .. } => Some(timestamp),
            SessionEntry::Unknown { raw } => raw.get("timestamp").and_then(Value::as_str),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionManager {
    session_file: PathBuf,
    header: SessionHeader,
    entries: Vec<SessionEntry>,
    by_id: HashMap<String, usize>,
    leaf_id: Option<String>,
    labels_by_id: BTreeMap<String, String>,
}

impl SessionManager {
    pub fn new(cwd: &Path) -> Result<Self> {
        Self::new_with_parent(cwd, None)
    }

    pub fn new_with_parent(cwd: &Path, parent_session: Option<String>) -> Result<Self> {
        let dir = session_dir_for(cwd)?;
        fs::create_dir_all(&dir)?;
        let filename = format!(
            "{}_{}.jsonl",
            Utc::now().format("%Y%m%d%H%M%S"),
            Uuid::new_v4()
        );
        let session_file = dir.join(filename);
        let mut header = SessionHeader::new(cwd.display().to_string());
        header.parent_session = parent_session;
        let manager = Self {
            session_file,
            header,
            entries: Vec::new(),
            by_id: HashMap::new(),
            leaf_id: None,
            labels_by_id: BTreeMap::new(),
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
            entries.push(parse_session_entry(line)?);
        }
        let mut manager = Self {
            session_file: path.to_path_buf(),
            header,
            entries,
            by_id: HashMap::new(),
            leaf_id: None,
            labels_by_id: BTreeMap::new(),
        };
        manager.rebuild_index();
        Ok(manager)
    }

    pub fn load_target(cwd: &Path, target: &str) -> Result<Self> {
        let target_path = Path::new(target);
        if target_path.exists() || target.ends_with(".jsonl") || target.contains('/') {
            return Self::load(target_path);
        }
        Self::load_by_id(cwd, target)
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

    pub fn continue_recent(cwd: &Path) -> Result<Self> {
        if let Some(path) = find_most_recent_session(&session_dir_for(cwd)?)? {
            Self::load(&path)
        } else {
            Self::new(cwd)
        }
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

    pub fn get_entries(&self) -> Vec<SessionEntry> {
        self.entries.clone()
    }

    pub fn get_entry(&self, id: &str) -> Option<&SessionEntry> {
        self.by_id.get(id).and_then(|idx| self.entries.get(*idx))
    }

    pub fn get_leaf_id(&self) -> Option<&str> {
        self.leaf_id.as_deref()
    }

    pub fn branch(&mut self, id: &str) -> Result<()> {
        if !self.by_id.contains_key(id) {
            anyhow::bail!("entry {id} not found");
        }
        self.leaf_id = Some(id.to_string());
        Ok(())
    }

    pub fn reset_leaf(&mut self) {
        self.leaf_id = None;
    }

    pub fn get_branch(&self, from_id: Option<&str>) -> Vec<&SessionEntry> {
        let mut path = Vec::new();
        let start_id = from_id.or(self.leaf_id.as_deref());
        let mut current_id = start_id.map(str::to_string);
        while let Some(id) = current_id {
            let Some(entry) = self.get_entry(&id) else {
                break;
            };
            path.push(entry);
            current_id = entry.parent_id().map(str::to_string);
        }
        path.reverse();
        path
    }

    pub fn build_session_context(&self) -> SessionContext {
        build_session_context(&self.entries, self.leaf_id.as_deref(), Some(&self.by_id))
    }

    pub fn session_info(&self) -> Result<SessionInfo> {
        let metadata = fs::metadata(&self.session_file)?;
        Ok(self.build_session_info(metadata))
    }

    pub fn session_name(&self) -> Option<String> {
        self.entries.iter().rev().find_map(|entry| match entry {
            SessionEntry::SessionInfo { name, .. } => {
                let trimmed = name.as_deref().map(str::trim).unwrap_or_default();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            }
            _ => None,
        })
    }

    pub fn title(&self) -> String {
        if let Some(name) = self.session_name() {
            return name;
        }
        let first_message = self.first_user_message_text();
        let trimmed = first_message.trim();
        if trimmed.is_empty() {
            format!("Session {}", &self.header.id[..8])
        } else {
            trimmed.chars().take(80).collect()
        }
    }

    pub fn current_model_id(&self) -> Option<String> {
        self.build_session_context()
            .model
            .map(|model| model.model_id)
    }

    pub fn current_thinking_level(&self) -> Option<String> {
        let thinking_level = self.build_session_context().thinking_level;
        if thinking_level == "off" {
            None
        } else {
            Some(thinking_level)
        }
    }

    pub fn messages(&self) -> Vec<AgentMessage> {
        self.build_session_context().messages
    }

    pub fn push_message(&mut self, message: AgentMessage) -> Result<String> {
        let entry = SessionEntry::Message {
            id: short_id(),
            parent_id: self.leaf_id.clone(),
            timestamp: Utc::now().to_rfc3339(),
            message,
        };
        self.append_entry(entry)
    }

    pub fn push_model_change(
        &mut self,
        provider: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Result<String> {
        let entry = SessionEntry::ModelChange {
            id: short_id(),
            parent_id: self.leaf_id.clone(),
            timestamp: Utc::now().to_rfc3339(),
            provider: provider.into(),
            model_id: model_id.into(),
        };
        self.append_entry(entry)
    }

    pub fn push_thinking_level_change(
        &mut self,
        thinking_level: impl Into<String>,
    ) -> Result<String> {
        let entry = SessionEntry::ThinkingLevelChange {
            id: short_id(),
            parent_id: self.leaf_id.clone(),
            timestamp: Utc::now().to_rfc3339(),
            thinking_level: thinking_level.into(),
        };
        self.append_entry(entry)
    }

    pub fn append_session_info(&mut self, name: impl Into<String>) -> Result<String> {
        let name = name.into();
        let entry = SessionEntry::SessionInfo {
            id: short_id(),
            parent_id: self.leaf_id.clone(),
            timestamp: Utc::now().to_rfc3339(),
            name: if name.trim().is_empty() {
                None
            } else {
                Some(name.trim().to_string())
            },
        };
        self.append_entry(entry)
    }

    pub fn append_label_change(
        &mut self,
        target_id: impl Into<String>,
        label: Option<String>,
    ) -> Result<String> {
        let target_id = target_id.into();
        if !self.by_id.contains_key(&target_id) {
            anyhow::bail!("entry {target_id} not found");
        }
        let entry = SessionEntry::Label {
            id: short_id(),
            parent_id: self.leaf_id.clone(),
            timestamp: Utc::now().to_rfc3339(),
            target_id,
            label,
        };
        self.append_entry(entry)
    }

    pub fn append_compaction(
        &mut self,
        summary: impl Into<String>,
        first_kept_entry_id: impl Into<String>,
        tokens_before: u64,
        details: Option<Value>,
        from_hook: Option<bool>,
    ) -> Result<String> {
        let entry = SessionEntry::Compaction {
            id: short_id(),
            parent_id: self.leaf_id.clone(),
            timestamp: Utc::now().to_rfc3339(),
            summary: summary.into(),
            first_kept_entry_id: first_kept_entry_id.into(),
            tokens_before,
            details,
            from_hook,
        };
        self.append_entry(entry)
    }

    pub fn append_branch_summary(
        &mut self,
        from_id: impl Into<String>,
        summary: impl Into<String>,
        details: Option<Value>,
        from_hook: Option<bool>,
    ) -> Result<String> {
        let entry = SessionEntry::BranchSummary {
            id: short_id(),
            parent_id: self.leaf_id.clone(),
            timestamp: Utc::now().to_rfc3339(),
            from_id: from_id.into(),
            summary: summary.into(),
            details,
            from_hook,
        };
        self.append_entry(entry)
    }

    pub fn create_fork(&self, leaf_id: Option<&str>) -> Result<Self> {
        let cwd = PathBuf::from(&self.header.cwd);
        let parent_session = self
            .session_file
            .to_str()
            .map(str::to_string)
            .or_else(|| Some(self.session_file.display().to_string()));
        let mut manager = Self::new_with_parent(&cwd, parent_session)?;
        manager.entries = match leaf_id {
            Some(leaf_id) => self
                .get_branch(Some(leaf_id))
                .into_iter()
                .cloned()
                .collect::<Vec<_>>(),
            None => Vec::new(),
        };
        manager.rebuild_index();
        manager.persist()?;
        Ok(manager)
    }

    pub fn persist(&self) -> Result<()> {
        if let Some(parent) = self.session_file.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut lines = vec![serde_json::to_string(&self.header)?];
        for entry in &self.entries {
            lines.push(serialize_session_entry(entry)?);
        }
        fs::write(&self.session_file, lines.join("\n") + "\n")?;
        Ok(())
    }

    fn append_entry(&mut self, entry: SessionEntry) -> Result<String> {
        let id = entry
            .id()
            .map(str::to_string)
            .context("session entry missing id")?;
        self.entries.push(entry);
        self.rebuild_index();
        self.persist()?;
        Ok(id)
    }

    fn rebuild_index(&mut self) {
        self.by_id.clear();
        self.labels_by_id.clear();
        self.leaf_id = None;
        for (idx, entry) in self.entries.iter().enumerate() {
            if let Some(id) = entry.id() {
                self.by_id.insert(id.to_string(), idx);
                self.leaf_id = Some(id.to_string());
            }
            if let SessionEntry::Label {
                target_id, label, ..
            } = entry
            {
                if let Some(label) = label
                    .as_deref()
                    .map(str::trim)
                    .filter(|label| !label.is_empty())
                {
                    self.labels_by_id
                        .insert(target_id.clone(), label.to_string());
                } else {
                    self.labels_by_id.remove(target_id);
                }
            }
        }
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
            name: self.session_name(),
            parent_session_path: self.header.parent_session.clone(),
            first_message: self.first_user_message_text(),
            current_model_id: self.current_model_id(),
            current_thinking_level: self.current_thinking_level(),
            leaf_id: self.leaf_id.clone(),
        }
    }

    fn first_user_message_text(&self) -> String {
        self.entries
            .iter()
            .find_map(|entry| match entry {
                SessionEntry::Message {
                    message: user_message @ AgentMessage::User { .. },
                    ..
                } => Some(user_message.as_text()),
                _ => None,
            })
            .unwrap_or_default()
    }
}

pub fn build_session_context(
    entries: &[SessionEntry],
    leaf_id: Option<&str>,
    by_id: Option<&HashMap<String, usize>>,
) -> SessionContext {
    let local_index;
    let by_id = if let Some(by_id) = by_id {
        by_id
    } else {
        local_index = entries
            .iter()
            .enumerate()
            .filter_map(|(idx, entry)| entry.id().map(|id| (id.to_string(), idx)))
            .collect::<HashMap<_, _>>();
        &local_index
    };

    if leaf_id == Some("") {
        return SessionContext {
            messages: Vec::new(),
            thinking_level: "off".to_string(),
            model: None,
        };
    }

    let mut path = Vec::new();
    let mut current = leaf_id
        .and_then(|id| by_id.get(id).copied())
        .or_else(|| entries.iter().rposition(|entry| entry.id().is_some()));
    while let Some(idx) = current {
        let entry = &entries[idx];
        path.push(entry);
        current = entry
            .parent_id()
            .and_then(|parent_id| by_id.get(parent_id).copied());
    }
    path.reverse();

    let mut thinking_level = "off".to_string();
    let mut model = None;
    let mut latest_compaction_idx = None;

    for (idx, entry) in path.iter().enumerate() {
        match entry {
            SessionEntry::ThinkingLevelChange {
                thinking_level: level,
                ..
            } => {
                thinking_level = level.clone();
            }
            SessionEntry::ModelChange {
                provider, model_id, ..
            } => {
                model = Some(SavedModel {
                    provider: provider.clone(),
                    model_id: model_id.clone(),
                });
            }
            SessionEntry::Message {
                message:
                    AgentMessage::Assistant {
                        provider,
                        model: model_id,
                        ..
                    },
                ..
            } => {
                model = Some(SavedModel {
                    provider: provider.clone(),
                    model_id: model_id.clone(),
                });
            }
            SessionEntry::Compaction { .. } => {
                latest_compaction_idx = Some(idx);
            }
            _ => {}
        }
    }

    let mut messages = Vec::new();

    if let Some(compaction_idx) = latest_compaction_idx {
        if let SessionEntry::Compaction {
            summary,
            tokens_before,
            timestamp,
            first_kept_entry_id,
            ..
        } = path[compaction_idx]
        {
            messages.push(AgentMessage::CompactionSummary {
                summary: summary.clone(),
                tokens_before: *tokens_before,
                timestamp: parse_entry_timestamp(timestamp),
            });
            let mut found_first_kept = false;
            for entry in &path[..compaction_idx] {
                if entry.id() == Some(first_kept_entry_id.as_str()) {
                    found_first_kept = true;
                }
                if found_first_kept {
                    append_context_message(&mut messages, entry);
                }
            }
        }
        for entry in &path[compaction_idx + 1..] {
            append_context_message(&mut messages, entry);
        }
    } else {
        for entry in path {
            append_context_message(&mut messages, entry);
        }
    }

    SessionContext {
        messages,
        thinking_level,
        model,
    }
}

fn append_context_message(messages: &mut Vec<AgentMessage>, entry: &SessionEntry) {
    match entry {
        SessionEntry::Message { message, .. } => messages.push(message.clone()),
        SessionEntry::BranchSummary {
            summary,
            from_id,
            timestamp,
            ..
        } => messages.push(AgentMessage::BranchSummary {
            summary: summary.clone(),
            from_id: from_id.clone(),
            timestamp: parse_entry_timestamp(timestamp),
        }),
        _ => {}
    }
}

fn parse_session_entry(line: &str) -> Result<SessionEntry> {
    let raw: Value = serde_json::from_str(line)?;
    let entry_type = raw.get("type").and_then(Value::as_str).unwrap_or_default();
    Ok(match entry_type {
        "message" => SessionEntry::Message {
            id: get_string(&raw, "id")?,
            parent_id: get_optional_string(&raw, "parentId"),
            timestamp: get_string(&raw, "timestamp")?,
            message: serde_json::from_value(
                raw.get("message").cloned().context("missing message")?,
            )?,
        },
        "model_change" => SessionEntry::ModelChange {
            id: get_string(&raw, "id")?,
            parent_id: get_optional_string(&raw, "parentId"),
            timestamp: get_string(&raw, "timestamp")?,
            provider: get_string(&raw, "provider")?,
            model_id: get_string(&raw, "modelId")?,
        },
        "thinking_level_change" => SessionEntry::ThinkingLevelChange {
            id: get_string(&raw, "id")?,
            parent_id: get_optional_string(&raw, "parentId"),
            timestamp: get_string(&raw, "timestamp")?,
            thinking_level: get_string(&raw, "thinkingLevel")?,
        },
        "compaction" => SessionEntry::Compaction {
            id: get_string(&raw, "id")?,
            parent_id: get_optional_string(&raw, "parentId"),
            timestamp: get_string(&raw, "timestamp")?,
            summary: get_string(&raw, "summary")?,
            first_kept_entry_id: get_string(&raw, "firstKeptEntryId")?,
            tokens_before: raw
                .get("tokensBefore")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            details: raw.get("details").cloned(),
            from_hook: raw.get("fromHook").and_then(Value::as_bool),
        },
        "branch_summary" => SessionEntry::BranchSummary {
            id: get_string(&raw, "id")?,
            parent_id: get_optional_string(&raw, "parentId"),
            timestamp: get_string(&raw, "timestamp")?,
            from_id: get_string(&raw, "fromId")?,
            summary: get_string(&raw, "summary")?,
            details: raw.get("details").cloned(),
            from_hook: raw.get("fromHook").and_then(Value::as_bool),
        },
        "label" => SessionEntry::Label {
            id: get_string(&raw, "id")?,
            parent_id: get_optional_string(&raw, "parentId"),
            timestamp: get_string(&raw, "timestamp")?,
            target_id: get_string(&raw, "targetId")?,
            label: get_optional_string(&raw, "label"),
        },
        "session_info" => SessionEntry::SessionInfo {
            id: get_string(&raw, "id")?,
            parent_id: get_optional_string(&raw, "parentId"),
            timestamp: get_string(&raw, "timestamp")?,
            name: get_optional_string(&raw, "name"),
        },
        _ => SessionEntry::Unknown { raw },
    })
}

fn serialize_session_entry(entry: &SessionEntry) -> Result<String> {
    let value = match entry {
        SessionEntry::Message {
            id,
            parent_id,
            timestamp,
            message,
        } => serde_json::json!({
            "type": "message",
            "id": id,
            "parentId": parent_id,
            "timestamp": timestamp,
            "message": message,
        }),
        SessionEntry::ModelChange {
            id,
            parent_id,
            timestamp,
            provider,
            model_id,
        } => serde_json::json!({
            "type": "model_change",
            "id": id,
            "parentId": parent_id,
            "timestamp": timestamp,
            "provider": provider,
            "modelId": model_id,
        }),
        SessionEntry::ThinkingLevelChange {
            id,
            parent_id,
            timestamp,
            thinking_level,
        } => serde_json::json!({
            "type": "thinking_level_change",
            "id": id,
            "parentId": parent_id,
            "timestamp": timestamp,
            "thinkingLevel": thinking_level,
        }),
        SessionEntry::Compaction {
            id,
            parent_id,
            timestamp,
            summary,
            first_kept_entry_id,
            tokens_before,
            details,
            from_hook,
        } => serde_json::json!({
            "type": "compaction",
            "id": id,
            "parentId": parent_id,
            "timestamp": timestamp,
            "summary": summary,
            "firstKeptEntryId": first_kept_entry_id,
            "tokensBefore": tokens_before,
            "details": details,
            "fromHook": from_hook,
        }),
        SessionEntry::BranchSummary {
            id,
            parent_id,
            timestamp,
            from_id,
            summary,
            details,
            from_hook,
        } => serde_json::json!({
            "type": "branch_summary",
            "id": id,
            "parentId": parent_id,
            "timestamp": timestamp,
            "fromId": from_id,
            "summary": summary,
            "details": details,
            "fromHook": from_hook,
        }),
        SessionEntry::Label {
            id,
            parent_id,
            timestamp,
            target_id,
            label,
        } => serde_json::json!({
            "type": "label",
            "id": id,
            "parentId": parent_id,
            "timestamp": timestamp,
            "targetId": target_id,
            "label": label,
        }),
        SessionEntry::SessionInfo {
            id,
            parent_id,
            timestamp,
            name,
        } => serde_json::json!({
            "type": "session_info",
            "id": id,
            "parentId": parent_id,
            "timestamp": timestamp,
            "name": name,
        }),
        SessionEntry::Unknown { raw } => raw.clone(),
    };
    Ok(serde_json::to_string(&value)?)
}

fn parse_entry_timestamp(timestamp: &str) -> i64 {
    DateTime::parse_from_rfc3339(timestamp)
        .map(|dt| dt.timestamp_millis())
        .unwrap_or_else(|_| Utc::now().timestamp_millis())
}

fn get_string(raw: &Value, key: &str) -> Result<String> {
    raw.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .with_context(|| format!("missing {key}"))
}

fn get_optional_string(raw: &Value, key: &str) -> Option<String> {
    raw.get(key).and_then(Value::as_str).map(str::to_string)
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

fn find_most_recent_session(dir: &Path) -> Result<Option<PathBuf>> {
    if !dir.exists() {
        return Ok(None);
    }
    let mut sessions = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .filter_map(|path| {
            let metadata = fs::metadata(&path).ok()?;
            Some((path, metadata.modified().ok()?))
        })
        .collect::<Vec<_>>();
    sessions.sort_by(|a, b| b.1.cmp(&a.1));
    Ok(sessions.into_iter().next().map(|(path, _)| path))
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

    #[test]
    fn branch_context_uses_active_leaf() {
        let temp = std::env::temp_dir().join(format!("pi-rs-session-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&temp).unwrap();
        let mut manager = SessionManager::new(&temp).unwrap();
        let user_a = manager
            .push_message(AgentMessage::User {
                content: vec![UserContentBlock::Text(TextContent::new("A"))],
                timestamp: 1,
            })
            .unwrap();
        manager
            .push_message(AgentMessage::Assistant {
                content: vec![],
                api: "api".into(),
                provider: "openai-codex".into(),
                model: "gpt-5.4".into(),
                usage: Default::default(),
                stop_reason: "stop".into(),
                error_message: None,
                timestamp: 2,
            })
            .unwrap();
        manager.branch(&user_a).unwrap();
        manager
            .push_message(AgentMessage::User {
                content: vec![UserContentBlock::Text(TextContent::new("B"))],
                timestamp: 3,
            })
            .unwrap();

        let context = manager.build_session_context();
        assert_eq!(context.messages.len(), 2);
        assert_eq!(context.messages[0].as_text(), "A");
        assert_eq!(context.messages[1].as_text(), "B");
    }

    #[test]
    fn fork_copies_selected_branch_only() {
        let temp = std::env::temp_dir().join(format!("pi-rs-session-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&temp).unwrap();
        let mut manager = SessionManager::new(&temp).unwrap();
        let root = manager
            .push_message(AgentMessage::User {
                content: vec![UserContentBlock::Text(TextContent::new("root"))],
                timestamp: 1,
            })
            .unwrap();
        manager
            .push_message(AgentMessage::Assistant {
                content: vec![],
                api: "api".into(),
                provider: "openai-codex".into(),
                model: "gpt-5.4".into(),
                usage: Default::default(),
                stop_reason: "stop".into(),
                error_message: None,
                timestamp: 2,
            })
            .unwrap();
        manager.branch(&root).unwrap();
        let branch = manager
            .push_message(AgentMessage::User {
                content: vec![UserContentBlock::Text(TextContent::new("branch"))],
                timestamp: 3,
            })
            .unwrap();

        let fork = manager.create_fork(Some(&branch)).unwrap();
        assert_eq!(
            fork.header().parent_session.as_deref(),
            Some(manager.session_file().to_str().unwrap())
        );
        assert_eq!(fork.build_session_context().messages.len(), 2);
        assert_eq!(fork.build_session_context().messages[1].as_text(), "branch");
    }

    #[test]
    fn unknown_entries_are_preserved() {
        let temp = std::env::temp_dir().join(format!("pi-rs-session-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&temp).unwrap();
        let file = temp.join("manual.jsonl");
        let header = SessionHeader::new(temp.display().to_string());
        let unknown = serde_json::json!({
            "type": "custom",
            "id": "abc12345",
            "parentId": null,
            "timestamp": "2026-01-01T00:00:00Z",
            "payload": {"ok": true}
        });
        std::fs::write(
            &file,
            format!(
                "{}\n{}\n",
                serde_json::to_string(&header).unwrap(),
                serde_json::to_string(&unknown).unwrap()
            ),
        )
        .unwrap();

        let mut manager = SessionManager::load(&file).unwrap();
        manager
            .push_message(AgentMessage::User {
                content: vec![UserContentBlock::Text(TextContent::new("next"))],
                timestamp: 1,
            })
            .unwrap();
        let content = std::fs::read_to_string(manager.session_file()).unwrap();
        assert!(content.contains("\"type\":\"custom\""));
        assert!(content.contains("\"payload\":{\"ok\":true}"));
    }
}
