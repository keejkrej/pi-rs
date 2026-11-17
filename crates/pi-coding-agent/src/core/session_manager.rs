//! Port of packages/coding-agent/src/core/session-manager.ts

use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use fancy_regex::Regex;
use indexmap::{IndexMap, IndexSet};
use pi_agent_core::AgentMessage;
use pi_ai::{
    AssistantContentBlock, Message, SystemMessage, Usage, UserContentBlock, UserMessageContent,
    get_current_system_message, uuidv7,
};
use pi_js::abort::AbortSignal;
use pi_js::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::config::{APP_NAME, get_agent_dir as get_default_agent_dir, get_sessions_dir};
use crate::core::messages::{
    CustomMessageContent, create_branch_summary_message, create_compaction_summary_message, create_custom_message,
};
use crate::utils::paths::{PathInputOptions, normalize_path, paths_equal, resolve_path};

pub const CURRENT_SESSION_VERSION: i64 = 3;

static SESSION_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?$").expect("valid regex"));

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHeader {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
    pub id: String,
    pub timestamp: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewSessionOptions {
    pub id: Option<String>,
    pub parent_session: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMessageEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub message: AgentMessage,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingLevelChangeEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub thinking_level: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelChangeEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub provider: String,
    pub model_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub kind: String,
    pub provider: String,
    pub model: String,
    pub usage: Usage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub summary: String,
    #[serde(default)]
    pub first_kept_entry_id: String,
    pub tokens_before: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_hook: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_message: Option<SystemMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_kept_entry_index: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub from_id: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_hook: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub custom_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub target_id: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfoEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

fn default_empty_custom_content() -> CustomMessageContent {
    CustomMessageContent::Blocks(Vec::new())
}

fn deserialize_null_custom_content<'de, D>(deserializer: D) -> std::result::Result<CustomMessageContent, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<CustomMessageContent>::deserialize(deserializer)?.unwrap_or_else(default_empty_custom_content))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessageEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub custom_type: String,
    #[serde(
        default = "default_empty_custom_content",
        deserialize_with = "deserialize_null_custom_content"
    )]
    pub content: CustomMessageContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    pub display: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ContextEditableContent {
    Text(String),
    Blocks(Vec<Value>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEditReplacement {
    pub content: ContextEditableContent,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEditEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub target_id: String,
    pub replacement: Option<ContextEditReplacement>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SessionEntry {
    #[serde(rename = "message")]
    Message(SessionMessageEntry),
    #[serde(rename = "thinking_level_change")]
    ThinkingLevelChange(ThinkingLevelChangeEntry),
    #[serde(rename = "model_change")]
    ModelChange(ModelChangeEntry),
    #[serde(rename = "usage")]
    Usage(UsageEntry),
    #[serde(rename = "compaction")]
    Compaction(CompactionEntry),
    #[serde(rename = "branch_summary")]
    BranchSummary(BranchSummaryEntry),
    #[serde(rename = "custom")]
    Custom(CustomEntry),
    #[serde(rename = "custom_message")]
    CustomMessage(CustomMessageEntry),
    #[serde(rename = "context_edit")]
    ContextEdit(ContextEditEntry),
    #[serde(rename = "label")]
    Label(LabelEntry),
    #[serde(rename = "session_info")]
    SessionInfo(SessionInfoEntry),
}

impl SessionEntry {
    pub fn id(&self) -> &str {
        match self {
            Self::Message(e) => &e.id,
            Self::ThinkingLevelChange(e) => &e.id,
            Self::ModelChange(e) => &e.id,
            Self::Usage(e) => &e.id,
            Self::Compaction(e) => &e.id,
            Self::BranchSummary(e) => &e.id,
            Self::Custom(e) => &e.id,
            Self::CustomMessage(e) => &e.id,
            Self::ContextEdit(e) => &e.id,
            Self::Label(e) => &e.id,
            Self::SessionInfo(e) => &e.id,
        }
    }

    pub fn parent_id(&self) -> Option<&str> {
        match self {
            Self::Message(e) => e.parent_id.as_deref(),
            Self::ThinkingLevelChange(e) => e.parent_id.as_deref(),
            Self::ModelChange(e) => e.parent_id.as_deref(),
            Self::Usage(e) => e.parent_id.as_deref(),
            Self::Compaction(e) => e.parent_id.as_deref(),
            Self::BranchSummary(e) => e.parent_id.as_deref(),
            Self::Custom(e) => e.parent_id.as_deref(),
            Self::CustomMessage(e) => e.parent_id.as_deref(),
            Self::ContextEdit(e) => e.parent_id.as_deref(),
            Self::Label(e) => e.parent_id.as_deref(),
            Self::SessionInfo(e) => e.parent_id.as_deref(),
        }
    }

    pub fn set_id(&mut self, id: String) {
        match self {
            Self::Message(e) => e.id = id,
            Self::ThinkingLevelChange(e) => e.id = id,
            Self::ModelChange(e) => e.id = id,
            Self::Usage(e) => e.id = id,
            Self::Compaction(e) => e.id = id,
            Self::BranchSummary(e) => e.id = id,
            Self::Custom(e) => e.id = id,
            Self::CustomMessage(e) => e.id = id,
            Self::ContextEdit(e) => e.id = id,
            Self::Label(e) => e.id = id,
            Self::SessionInfo(e) => e.id = id,
        }
    }

    pub fn set_parent_id(&mut self, parent_id: Option<String>) {
        match self {
            Self::Message(e) => e.parent_id = parent_id,
            Self::ThinkingLevelChange(e) => e.parent_id = parent_id,
            Self::ModelChange(e) => e.parent_id = parent_id,
            Self::Usage(e) => e.parent_id = parent_id,
            Self::Compaction(e) => e.parent_id = parent_id,
            Self::BranchSummary(e) => e.parent_id = parent_id,
            Self::Custom(e) => e.parent_id = parent_id,
            Self::CustomMessage(e) => e.parent_id = parent_id,
            Self::ContextEdit(e) => e.parent_id = parent_id,
            Self::Label(e) => e.parent_id = parent_id,
            Self::SessionInfo(e) => e.parent_id = parent_id,
        }
    }

    pub fn timestamp(&self) -> &str {
        match self {
            Self::Message(e) => &e.timestamp,
            Self::ThinkingLevelChange(e) => &e.timestamp,
            Self::ModelChange(e) => &e.timestamp,
            Self::Usage(e) => &e.timestamp,
            Self::Compaction(e) => &e.timestamp,
            Self::BranchSummary(e) => &e.timestamp,
            Self::Custom(e) => &e.timestamp,
            Self::CustomMessage(e) => &e.timestamp,
            Self::ContextEdit(e) => &e.timestamp,
            Self::Label(e) => &e.timestamp,
            Self::SessionInfo(e) => &e.timestamp,
        }
    }

    pub fn entry_type(&self) -> &'static str {
        match self {
            Self::Message(_) => "message",
            Self::ThinkingLevelChange(_) => "thinking_level_change",
            Self::ModelChange(_) => "model_change",
            Self::Usage(_) => "usage",
            Self::Compaction(_) => "compaction",
            Self::BranchSummary(_) => "branch_summary",
            Self::Custom(_) => "custom",
            Self::CustomMessage(_) => "custom_message",
            Self::ContextEdit(_) => "context_edit",
            Self::Label(_) => "label",
            Self::SessionInfo(_) => "session_info",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum FileEntry {
    #[serde(rename = "session")]
    Session(SessionHeader),
    #[serde(rename = "message")]
    Message(SessionMessageEntry),
    #[serde(rename = "thinking_level_change")]
    ThinkingLevelChange(ThinkingLevelChangeEntry),
    #[serde(rename = "model_change")]
    ModelChange(ModelChangeEntry),
    #[serde(rename = "usage")]
    Usage(UsageEntry),
    #[serde(rename = "compaction")]
    Compaction(CompactionEntry),
    #[serde(rename = "branch_summary")]
    BranchSummary(BranchSummaryEntry),
    #[serde(rename = "custom")]
    Custom(CustomEntry),
    #[serde(rename = "custom_message")]
    CustomMessage(CustomMessageEntry),
    #[serde(rename = "context_edit")]
    ContextEdit(ContextEditEntry),
    #[serde(rename = "label")]
    Label(LabelEntry),
    #[serde(rename = "session_info")]
    SessionInfo(SessionInfoEntry),
}

impl FileEntry {
    pub fn into_session_entry(self) -> Option<SessionEntry> {
        match self {
            Self::Session(_) => None,
            Self::Message(e) => Some(SessionEntry::Message(e)),
            Self::ThinkingLevelChange(e) => Some(SessionEntry::ThinkingLevelChange(e)),
            Self::ModelChange(e) => Some(SessionEntry::ModelChange(e)),
            Self::Usage(e) => Some(SessionEntry::Usage(e)),
            Self::Compaction(e) => Some(SessionEntry::Compaction(e)),
            Self::BranchSummary(e) => Some(SessionEntry::BranchSummary(e)),
            Self::Custom(e) => Some(SessionEntry::Custom(e)),
            Self::CustomMessage(e) => Some(SessionEntry::CustomMessage(e)),
            Self::ContextEdit(e) => Some(SessionEntry::ContextEdit(e)),
            Self::Label(e) => Some(SessionEntry::Label(e)),
            Self::SessionInfo(e) => Some(SessionEntry::SessionInfo(e)),
        }
    }

    pub fn from_session_entry(entry: SessionEntry) -> Self {
        match entry {
            SessionEntry::Message(e) => Self::Message(e),
            SessionEntry::ThinkingLevelChange(e) => Self::ThinkingLevelChange(e),
            SessionEntry::ModelChange(e) => Self::ModelChange(e),
            SessionEntry::Usage(e) => Self::Usage(e),
            SessionEntry::Compaction(e) => Self::Compaction(e),
            SessionEntry::BranchSummary(e) => Self::BranchSummary(e),
            SessionEntry::Custom(e) => Self::Custom(e),
            SessionEntry::CustomMessage(e) => Self::CustomMessage(e),
            SessionEntry::ContextEdit(e) => Self::ContextEdit(e),
            SessionEntry::Label(e) => Self::Label(e),
            SessionEntry::SessionInfo(e) => Self::SessionInfo(e),
        }
    }

    pub fn as_session_entry(&self) -> Option<SessionEntry> {
        self.clone().into_session_entry()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTreeNode {
    pub entry: SessionEntry,
    pub children: Vec<SessionTreeNode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_timestamp: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectedSessionEntry {
    pub source_entry: SessionEntry,
    pub messages: Vec<AgentMessage>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionModelRef {
    pub provider: String,
    pub model_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionProjection {
    pub entries: Vec<ProjectedSessionEntry>,
    pub messages: Vec<AgentMessage>,
    pub thinking_level: String,
    pub model: Option<SessionModelRef>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionContext {
    pub messages: Vec<AgentMessage>,
    pub thinking_level: String,
    pub model: Option<SessionModelRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub path: String,
    pub id: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_path: Option<String>,
    pub created: i64,
    pub modified: i64,
    pub message_count: usize,
    pub first_message: String,
    pub all_messages_text: String,
}

pub type ReadonlySessionManager = SessionManager;

pub type SessionListProgress<'a> = &'a (dyn Fn(usize, usize, Option<&[SessionInfo]>) + Send + Sync);

fn create_session_id() -> String {
    uuidv7(None).unwrap_or_else(|_| pi_js::crypto::random_uuid())
}

pub fn assert_valid_session_id(id: &str) -> Result<()> {
    let ok = SESSION_ID_RE.is_match(id).unwrap_or(false);
    if !ok {
        return Err(Error::msg(
            "Session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character",
        ));
    }
    Ok(())
}

fn generate_id(by_id: &IndexMap<String, SessionEntry>) -> String {
    for _ in 0..100 {
        let uuid = pi_js::crypto::random_uuid();
        let short = uuid[..8].to_owned();
        if !by_id.contains_key(&short) {
            return short;
        }
    }
    pi_js::crypto::random_uuid()
}

fn generate_id_from_set(ids: &IndexSet<String>) -> String {
    for _ in 0..100 {
        let uuid = pi_js::crypto::random_uuid();
        let short = uuid[..8].to_owned();
        if !ids.contains(&short) {
            return short;
        }
    }
    pi_js::crypto::random_uuid()
}

fn migrate_v1_to_v2(entries: &mut Vec<FileEntry>) {
    let mut ids: IndexSet<String> = IndexSet::new();
    let mut prev_id: Option<String> = None;

    for idx in 0..entries.len() {
        if let FileEntry::Session(ref mut hdr) = entries[idx] {
            hdr.version = Some(2);
            continue;
        }

        let id = generate_id_from_set(&ids);
        ids.insert(id.clone());
        let parent_id = prev_id.clone();
        prev_id = Some(id.clone());

        let comp_target_idx = match &entries[idx] {
            FileEntry::Compaction(c) => c.first_kept_entry_index,
            _ => None,
        };

        let target_id_opt = comp_target_idx.and_then(|t_idx| {
            entries.get(t_idx).and_then(|e| match e {
                FileEntry::Session(_) => None,
                other => other.as_session_entry().map(|se| se.id().to_owned()),
            })
        });

        match &mut entries[idx] {
            FileEntry::Message(e) => {
                e.id = id;
                e.parent_id = parent_id;
            }
            FileEntry::ThinkingLevelChange(e) => {
                e.id = id;
                e.parent_id = parent_id;
            }
            FileEntry::ModelChange(e) => {
                e.id = id;
                e.parent_id = parent_id;
            }
            FileEntry::Usage(e) => {
                e.id = id;
                e.parent_id = parent_id;
            }
            FileEntry::Compaction(e) => {
                e.id = id;
                e.parent_id = parent_id;
                if let Some(tid) = target_id_opt {
                    e.first_kept_entry_id = tid;
                }
                e.first_kept_entry_index = None;
            }
            FileEntry::BranchSummary(e) => {
                e.id = id;
                e.parent_id = parent_id;
            }
            FileEntry::Custom(e) => {
                e.id = id;
                e.parent_id = parent_id;
            }
            FileEntry::CustomMessage(e) => {
                e.id = id;
                e.parent_id = parent_id;
            }
            FileEntry::ContextEdit(e) => {
                e.id = id;
                e.parent_id = parent_id;
            }
            FileEntry::Label(e) => {
                e.id = id;
                e.parent_id = parent_id;
            }
            FileEntry::SessionInfo(e) => {
                e.id = id;
                e.parent_id = parent_id;
            }
            FileEntry::Session(_) => {}
        }
    }
}

fn migrate_v2_to_v3(entries: &mut Vec<FileEntry>) {
    for entry in entries.iter_mut() {
        match entry {
            FileEntry::Session(hdr) => {
                hdr.version = Some(3);
            }
            FileEntry::Message(msg_entry) => {
                if let AgentMessage::Custom(Value::Object(ref mut map)) = msg_entry.message
                    && map.get("role").and_then(Value::as_str) == Some("hookMessage")
                {
                    map.insert("role".to_owned(), Value::String("custom".to_owned()));
                }
            }
            _ => {}
        }
    }
}

fn migrate_to_current_version(entries: &mut Vec<FileEntry>) -> bool {
    let header_version = entries.iter().find_map(|e| match e {
        FileEntry::Session(hdr) => Some(hdr.version.unwrap_or(1)),
        _ => None,
    });
    let version = header_version.unwrap_or(1);
    if version >= CURRENT_SESSION_VERSION {
        return false;
    }
    if version < 2 {
        migrate_v1_to_v2(entries);
    }
    if version < 3 {
        migrate_v2_to_v3(entries);
    }
    true
}

pub fn migrate_session_entries(entries: &mut Vec<FileEntry>) {
    migrate_to_current_version(entries);
}

pub fn parse_session_entries(content: &str) -> Vec<FileEntry> {
    let mut entries = Vec::new();
    for line in pi_js::str16::trim(content).split('\n') {
        if pi_js::str16::trim(line).is_empty() {
            continue;
        }
        if let Some(entry) = parse_session_entry_line(line) {
            entries.push(entry);
        }
    }
    entries
}

pub fn get_latest_compaction_entry(entries: &[SessionEntry]) -> Option<CompactionEntry> {
    for entry in entries.iter().rev() {
        if let SessionEntry::Compaction(comp) = entry {
            return Some(comp.clone());
        }
    }
    None
}

fn build_entry_index(
    entries: &[SessionEntry],
    by_id: Option<&IndexMap<String, SessionEntry>>,
) -> IndexMap<String, SessionEntry> {
    if let Some(existing) = by_id {
        return existing.clone();
    }
    let mut index = IndexMap::new();
    for entry in entries {
        index.insert(entry.id().to_owned(), entry.clone());
    }
    index
}

fn build_session_path(
    entries: &[SessionEntry],
    leaf_id: Option<Option<&str>>,
    by_id: Option<&IndexMap<String, SessionEntry>>,
) -> Vec<SessionEntry> {
    let index = build_entry_index(entries, by_id);
    if let Some(None) = leaf_id {
        return Vec::new();
    }
    let mut leaf = leaf_id.flatten().and_then(|id| index.get(id).cloned());
    if leaf.is_none() {
        leaf = entries.last().cloned();
    }
    let Some(start) = leaf else {
        return Vec::new();
    };

    let mut path = Vec::new();
    let mut current: Option<SessionEntry> = Some(start);
    let mut visited = IndexSet::new();
    while let Some(cur) = current {
        if !visited.insert(cur.id().to_owned()) {
            break;
        }
        let next_parent = cur.parent_id().map(str::to_owned);
        path.push(cur);
        current = next_parent.and_then(|pid| index.get(&pid).cloned());
    }
    path.reverse();
    path
}

fn get_session_context_settings(path: &[SessionEntry]) -> (String, Option<SessionModelRef>) {
    let mut thinking_level = "off".to_owned();
    let mut model: Option<SessionModelRef> = None;

    for entry in path {
        match entry {
            SessionEntry::ThinkingLevelChange(e) => {
                thinking_level = e.thinking_level.clone();
            }
            SessionEntry::ModelChange(e) => {
                model = Some(SessionModelRef {
                    provider: e.provider.clone(),
                    model_id: e.model_id.clone(),
                });
            }
            SessionEntry::Message(msg_entry) => {
                if let AgentMessage::Message(Message::Assistant(asst)) = &msg_entry.message {
                    model = Some(SessionModelRef {
                        provider: asst.provider.clone(),
                        model_id: asst.model.clone(),
                    });
                }
            }
            _ => {}
        }
    }

    (thinking_level, model)
}

pub(crate) fn repair_null_content_agent_message(message: &AgentMessage) -> AgentMessage {
    let AgentMessage::Custom(Value::Object(map)) = message else {
        return message.clone();
    };
    let Some(role) = map.get("role").and_then(Value::as_str) else {
        return message.clone();
    };
    let content_is_null_or_missing = map.get("content").is_none_or(Value::is_null);
    if !content_is_null_or_missing {
        return message.clone();
    }
    let mut repaired = map.clone();
    match role {
        "system" => {
            repaired.insert("content".to_owned(), Value::String(String::new()));
            let repaired_val = Value::Object(repaired);
            serde_json::from_value::<Message>(repaired_val.clone())
                .map(AgentMessage::Message)
                .unwrap_or(AgentMessage::Custom(repaired_val))
        }
        "user" | "assistant" | "toolResult" => {
            repaired.insert("content".to_owned(), Value::Array(Vec::new()));
            let repaired_val = Value::Object(repaired);
            serde_json::from_value::<Message>(repaired_val.clone())
                .map(AgentMessage::Message)
                .unwrap_or(AgentMessage::Custom(repaired_val))
        }
        "custom" => {
            repaired.insert("content".to_owned(), Value::Array(Vec::new()));
            AgentMessage::Custom(Value::Object(repaired))
        }
        _ => message.clone(),
    }
}

pub fn session_entry_to_context_messages(entry: &SessionEntry) -> Vec<AgentMessage> {
    match entry {
        SessionEntry::Message(msg_entry) => {
            vec![repair_null_content_agent_message(&msg_entry.message)]
        }
        SessionEntry::CustomMessage(cm) => {
            let msg = create_custom_message(
                cm.custom_type.clone(),
                cm.content.clone(),
                cm.display,
                cm.details.clone(),
                &cm.timestamp,
            );
            vec![msg.into()]
        }
        SessionEntry::BranchSummary(bs) if !bs.summary.is_empty() => {
            vec![create_branch_summary_message(bs.summary.clone(), Some(bs.from_id.clone()), &bs.timestamp).into()]
        }
        SessionEntry::Compaction(comp) => {
            let summary: AgentMessage =
                create_compaction_summary_message(comp.summary.clone(), comp.tokens_before, &comp.timestamp).into();
            if let Some(ref sys) = comp.system_message {
                vec![AgentMessage::Message(Message::System(sys.clone())), summary]
            } else {
                vec![summary]
            }
        }
        _ => Vec::new(),
    }
}

pub fn build_context_entries(
    entries: &[SessionEntry],
    leaf_id: Option<Option<&str>>,
    by_id: Option<&IndexMap<String, SessionEntry>>,
) -> Vec<SessionEntry> {
    let path = build_session_path(entries, leaf_id, by_id);
    let mut latest_compaction: Option<CompactionEntry> = None;
    for entry in &path {
        if let SessionEntry::Compaction(comp) = entry {
            latest_compaction = Some(comp.clone());
        }
    }

    let Some(compaction) = latest_compaction else {
        return path;
    };

    let Some(compaction_idx) = path.iter().position(|e| e.id() == compaction.id) else {
        return path;
    };

    let mut context_entries = vec![SessionEntry::Compaction(compaction.clone())];
    let mut found_first_kept = false;
    for entry in &path[..compaction_idx] {
        if entry.id() == compaction.first_kept_entry_id {
            found_first_kept = true;
        }
        let is_system_message = matches!(
            entry,
            SessionEntry::Message(m) if m.message.role() == Some("system")
        );
        if found_first_kept && !is_system_message {
            context_entries.push(entry.clone());
        }
    }
    context_entries.extend(path[compaction_idx + 1..].iter().cloned());
    context_entries
}

pub fn project_context_entry(entry: &SessionEntry, edit: Option<&ContextEditEntry>) -> Vec<AgentMessage> {
    let messages = session_entry_to_context_messages(entry);
    let Some(edit_entry) = edit else {
        return messages;
    };
    let Some(ref replacement) = edit_entry.replacement else {
        return Vec::new();
    };

    messages
        .into_iter()
        .map(|message| {
            let role = message.role().unwrap_or("");
            if role != "user" && role != "assistant" && role != "toolResult" && role != "custom" {
                return message;
            }
            let content_val = match &replacement.content {
                ContextEditableContent::Text(t) if role == "assistant" || role == "toolResult" => {
                    serde_json::json!([{ "type": "text", "text": t }])
                }
                ContextEditableContent::Text(t) => Value::String(t.clone()),
                ContextEditableContent::Blocks(b) => Value::Array(b.clone()),
            };
            let mut val = serde_json::to_value(&message).unwrap_or(Value::Null);
            if let Value::Object(ref mut map) = val {
                map.insert("content".to_owned(), content_val);
            }
            serde_json::from_value::<AgentMessage>(val).unwrap_or(message)
        })
        .collect()
}

pub fn build_session_projection(
    entries: &[SessionEntry],
    leaf_id: Option<Option<&str>>,
    by_id: Option<&IndexMap<String, SessionEntry>>,
) -> SessionProjection {
    let path = build_session_path(entries, leaf_id, by_id);
    let (thinking_level, model) = get_session_context_settings(&path);
    let context_entries = build_context_entries(entries, leaf_id, by_id);

    let mut edits: IndexMap<String, ContextEditEntry> = IndexMap::new();
    for entry in &context_entries {
        if let SessionEntry::ContextEdit(ce) = entry {
            edits.insert(ce.target_id.clone(), ce.clone());
        }
    }

    let mut projected_entries = Vec::with_capacity(context_entries.len());
    let mut flat_messages = Vec::new();

    for (index, source_entry) in context_entries.into_iter().enumerate() {
        let messages = if matches!(source_entry, SessionEntry::Compaction(_)) && index > 0 {
            Vec::new()
        } else {
            let edit = edits.get(source_entry.id());
            project_context_entry(&source_entry, edit)
        };
        flat_messages.extend(messages.iter().cloned());
        projected_entries.push(ProjectedSessionEntry { source_entry, messages });
    }

    SessionProjection {
        entries: projected_entries,
        messages: flat_messages,
        thinking_level,
        model,
    }
}

pub fn build_session_context(
    entries: &[SessionEntry],
    leaf_id: Option<Option<&str>>,
    by_id: Option<&IndexMap<String, SessionEntry>>,
) -> SessionContext {
    let proj = build_session_projection(entries, leaf_id, by_id);
    SessionContext {
        messages: proj.messages,
        thinking_level: proj.thinking_level,
        model: proj.model,
    }
}

fn get_default_session_dir_path(cwd: &str, agent_dir: Option<&str>) -> String {
    let resolved_cwd = resolve_path(cwd, None, PathInputOptions::default());
    let agent_owned;
    let agent = match agent_dir {
        Some(a) => a,
        None => {
            agent_owned = get_default_agent_dir();
            &agent_owned
        }
    };
    let resolved_agent = resolve_path(agent, None, PathInputOptions::default());

    let stripped = resolved_cwd.trim_start_matches(['/', '\\']);
    let replaced: String = stripped
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' => '-',
            _ => c,
        })
        .collect();
    let safe_path = format!("--{replaced}--");
    pi_js::path::join(&[&resolved_agent, "sessions", &safe_path])
}

pub fn get_default_session_dir(cwd: &str, agent_dir: Option<&str>) -> Result<String> {
    let session_dir = get_default_session_dir_path(cwd, agent_dir);
    if !pi_js::fs::exists(&session_dir) {
        pi_js::fs::mkdir(&session_dir, true, None)?;
    }
    Ok(session_dir)
}

const SESSION_READ_BUFFER_SIZE: usize = 1024 * 1024;
const SESSION_HEADER_READ_BUFFER_SIZE: usize = 4096;
const MAX_SESSION_HEADER_SCAN_BYTES: usize = 1024 * 1024;

#[derive(Default)]
struct Utf8StreamDecoder {
    incomplete: [u8; 4],
    incomplete_len: usize,
}

impl Utf8StreamDecoder {
    fn write(&mut self, chunk: &[u8]) -> String {
        let mut out = String::new();
        let mut combined;
        let data = if self.incomplete_len > 0 {
            combined = Vec::with_capacity(self.incomplete_len + chunk.len());
            combined.extend_from_slice(&self.incomplete[..self.incomplete_len]);
            combined.extend_from_slice(chunk);
            self.incomplete_len = 0;
            combined.as_slice()
        } else {
            chunk
        };

        let mut offset = 0usize;
        while offset < data.len() {
            match std::str::from_utf8(&data[offset..]) {
                Ok(valid) => {
                    out.push_str(valid);
                    break;
                }
                Err(err) => {
                    let valid_up_to = err.valid_up_to();
                    if valid_up_to > 0
                        && let Ok(valid) = std::str::from_utf8(&data[offset..offset + valid_up_to])
                    {
                        out.push_str(valid);
                    }
                    offset += valid_up_to;
                    match err.error_len() {
                        Some(invalid_len) => {
                            out.push('\u{FFFD}');
                            offset += invalid_len;
                        }
                        None => {
                            let rem = &data[offset..];
                            let n = rem.len().min(4);
                            self.incomplete[..n].copy_from_slice(&rem[..n]);
                            self.incomplete_len = n;
                            break;
                        }
                    }
                }
            }
        }
        out
    }

    fn end(&mut self) -> String {
        if self.incomplete_len > 0 {
            self.incomplete_len = 0;
            "\u{FFFD}".to_owned()
        } else {
            String::new()
        }
    }
}

fn parse_session_entry_line(line: &str) -> Option<FileEntry> {
    let trimmed = pi_js::str16::trim(line);
    if trimmed.is_empty() {
        return None;
    }
    let val = pi_js::json::parse(trimmed).ok()?;
    serde_json::from_value::<FileEntry>(val).ok()
}

pub fn load_entries_from_file(file_path: &str) -> Result<Vec<FileEntry>> {
    let resolved = normalize_path(file_path, Default::default());
    if !pi_js::fs::exists(&resolved) {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    let mut pending = String::new();
    let mut file = pi_js::fs::open(&resolved, "r", None)?;
    let mut decoder = Utf8StreamDecoder::default();
    let mut buf = vec![0u8; SESSION_READ_BUFFER_SIZE];

    loop {
        let n = file.read(&mut buf, None)?;
        if n == 0 {
            break;
        }
        pending.push_str(&decoder.write(&buf[..n]));
        let mut line_start = 0usize;
        while let Some(rel_nl) = pending[line_start..].find('\n') {
            let nl_idx = line_start + rel_nl;
            if let Some(entry) = parse_session_entry_line(&pending[line_start..nl_idx]) {
                entries.push(entry);
            }
            line_start = nl_idx + 1;
        }
        if line_start > 0 {
            pending.drain(..line_start);
        }
    }

    pending.push_str(&decoder.end());
    if let Some(final_entry) = parse_session_entry_line(&pending) {
        entries.push(final_entry);
    }

    if entries.is_empty() {
        return Ok(entries);
    }
    if !matches!(entries[0], FileEntry::Session(ref h) if !h.id.is_empty()) {
        // Also allow empty string check via raw JSON? TS checked `typeof header.id === "string"`,
        // which Serde already enforced when deserializing `SessionHeader.id: String`.
        if !matches!(entries[0], FileEntry::Session(_)) {
            return Ok(Vec::new());
        }
    }

    if !pending.is_empty() {
        let _ = pi_js::fs::append_file(&resolved, "\n");
    }
    Ok(entries)
}

enum HeaderCandidate {
    KeepScanning,
    NonHeader,
    Header(SessionHeader),
}

fn parse_session_header_candidate(line: &str) -> HeaderCandidate {
    if pi_js::str16::trim(line).is_empty() {
        return HeaderCandidate::KeepScanning;
    }
    let Some(entry) = parse_session_entry_line(line) else {
        return HeaderCandidate::KeepScanning;
    };
    match entry {
        FileEntry::Session(hdr) => HeaderCandidate::Header(hdr),
        _ => HeaderCandidate::NonHeader,
    }
}

const SESSION_HEADER_SCAN_LIMIT_CODE: &str = "SESSION_HEADER_SCAN_LIMIT";

fn read_session_header(file_path: &str) -> Result<Option<SessionHeader>> {
    let mut file = pi_js::fs::open(file_path, "r", None)?;
    let mut decoder = Utf8StreamDecoder::default();
    let mut buf = vec![0u8; SESSION_HEADER_READ_BUFFER_SIZE];
    let mut line_chunks: Vec<String> = Vec::new();
    let mut scanned_bytes = 0usize;

    while scanned_bytes < MAX_SESSION_HEADER_SCAN_BYTES {
        let read_len = buf.len().min(MAX_SESSION_HEADER_SCAN_BYTES - scanned_bytes);
        let n = file.read(&mut buf[..read_len], None)?;
        if n == 0 {
            line_chunks.push(decoder.end());
            let joined = line_chunks.concat();
            return Ok(match parse_session_header_candidate(&joined) {
                HeaderCandidate::Header(h) => Some(h),
                HeaderCandidate::NonHeader | HeaderCandidate::KeepScanning => None,
            });
        }
        scanned_bytes += n;

        let chunk = decoder.write(&buf[..n]);
        let mut line_start = 0usize;
        while let Some(rel_nl) = chunk[line_start..].find('\n') {
            let nl_idx = line_start + rel_nl;
            line_chunks.push(chunk[line_start..nl_idx].to_owned());
            let joined = line_chunks.concat();
            match parse_session_header_candidate(&joined) {
                HeaderCandidate::Header(h) => return Ok(Some(h)),
                HeaderCandidate::NonHeader => return Ok(None),
                HeaderCandidate::KeepScanning => {}
            }
            line_chunks.clear();
            line_start = nl_idx + 1;
        }
        line_chunks.push(chunk[line_start..].to_owned());
    }

    let mut probe = [0u8; 1];
    if file.read(&mut probe, None)? == 0 {
        line_chunks.push(decoder.end());
        let joined = line_chunks.concat();
        return Ok(match parse_session_header_candidate(&joined) {
            HeaderCandidate::Header(h) => Some(h),
            HeaderCandidate::NonHeader | HeaderCandidate::KeepScanning => None,
        });
    }

    Err(Error::js_with_code(
        "SessionHeaderScanLimitError",
        format!("Session header exceeds {MAX_SESSION_HEADER_SCAN_BYTES}-byte scan limit: {file_path}"),
        SESSION_HEADER_SCAN_LIMIT_CODE,
    ))
}

fn read_session_header_for_discovery(file_path: &str) -> Option<SessionHeader> {
    read_session_header(file_path).ok().flatten()
}

fn get_session_header_cwd(header: &SessionHeader) -> Option<&str> {
    if header.cwd.is_empty() { None } else { Some(&header.cwd) }
}

fn session_cwd_matches(cwd: Option<&str>, resolved_cwd: &str) -> bool {
    let Some(c) = cwd.filter(|s| !s.is_empty()) else {
        return false;
    };
    paths_equal(&resolve_path(c, None, PathInputOptions::default()), resolved_cwd)
}

pub fn find_most_recent_session(session_dir: &str, cwd: Option<&str>) -> Option<String> {
    let resolved_session_dir = normalize_path(session_dir, Default::default());
    let resolved_cwd = cwd.map(|c| resolve_path(c, None, PathInputOptions::default()));

    let entries = pi_js::fs::readdir(&resolved_session_dir).ok()?;
    let mut files: Vec<(String, i64)> = entries
        .into_iter()
        .filter(|name| name.ends_with(".jsonl"))
        .filter_map(|name| {
            let p = pi_js::path::join(&[&resolved_session_dir, &name]);
            let st = pi_js::fs::stat(&p).ok()?;
            Some((p, st.mtime_ns as i64))
        })
        .collect();
    files.sort_by(|a, b| b.1.cmp(&a.1));

    for (path, _) in files {
        if let Some(header) = read_session_header_for_discovery(&path) {
            let matches_cwd = match resolved_cwd {
                Some(ref rcwd) => session_cwd_matches(get_session_header_cwd(&header), rcwd),
                None => true,
            };
            if matches_cwd {
                return Some(path);
            }
        }
    }
    None
}

fn extract_text_content_from_message(message: &Message) -> String {
    match message {
        Message::User(u) => match &u.content {
            UserMessageContent::Text(t) => t.clone(),
            UserMessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    UserContentBlock::Text(t) => Some(t.text.as_str()),
                    UserContentBlock::Image(_) => None,
                })
                .collect::<Vec<_>>()
                .join(" "),
        },
        Message::Assistant(a) => a
            .content
            .iter()
            .filter_map(|b| match b {
                AssistantContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" "),
        Message::System(_) | Message::ToolResult(_) => String::new(),
    }
}

fn get_message_activity_time(entry: &SessionMessageEntry) -> Option<i64> {
    let role = entry.message.role()?;
    if role != "user" && role != "assistant" {
        return None;
    }
    match &entry.message {
        AgentMessage::Message(Message::User(u)) => return Some(u.timestamp),
        AgentMessage::Message(Message::Assistant(a)) => return Some(a.timestamp),
        AgentMessage::Custom(val) => {
            if let Some(ts) = val.get("timestamp").and_then(Value::as_f64)
                && ts.is_finite()
            {
                return Some(ts as i64);
            }
        }
        _ => {}
    }
    pi_js::time::parse_date(&entry.timestamp)
}

async fn build_session_info(
    file_path: &str,
    signal: Option<&AbortSignal>,
    file_mtime_ms: Option<i64>,
) -> Result<Option<SessionInfo>> {
    if let Some(sig) = signal {
        sig.throw_if_aborted()?;
    }
    let mtime_ms = match file_mtime_ms {
        Some(m) => m,
        None => match pi_js::fs::stat(file_path) {
            Ok(s) => s.mtime(),
            Err(_) => {
                if let Some(sig) = signal {
                    sig.throw_if_aborted()?;
                }
                return Ok(None);
            }
        },
    };

    let Ok(file) = tokio::fs::File::open(file_path).await else {
        if let Some(sig) = signal {
            sig.throw_if_aborted()?;
        }
        return Ok(None);
    };

    let mut header: Option<SessionHeader> = None;
    let mut message_count = 0usize;
    let mut first_message = String::new();
    let mut all_messages: Vec<String> = Vec::new();
    let mut name: Option<String> = None;
    let mut last_activity_time: Option<i64> = None;

    let mut lines = BufReader::new(file).lines();
    loop {
        if let Some(sig) = signal {
            sig.throw_if_aborted()?;
        }
        let next_line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(_) => {
                if let Some(sig) = signal {
                    sig.throw_if_aborted()?;
                }
                return Ok(None);
            }
        };
        let Some(entry) = parse_session_entry_line(&next_line) else {
            continue;
        };

        if header.is_none() {
            match entry {
                FileEntry::Session(h) => {
                    header = Some(h);
                    continue;
                }
                _ => return Ok(None),
            }
        }

        match entry {
            FileEntry::SessionInfo(si) => {
                let trimmed = si.name.as_deref().map(pi_js::str16::trim).unwrap_or("");
                name = if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_owned())
                };
            }
            FileEntry::Message(msg_entry) => {
                message_count += 1;
                if let Some(activity_ts) = get_message_activity_time(&msg_entry) {
                    last_activity_time = Some(match last_activity_time {
                        Some(prev) => prev.max(activity_ts),
                        None => activity_ts,
                    });
                }
                if let AgentMessage::Message(ref msg) = msg_entry.message
                    && matches!(msg, Message::User(_) | Message::Assistant(_))
                {
                    let text_content = extract_text_content_from_message(msg);
                    if !text_content.is_empty() {
                        if first_message.is_empty() && matches!(msg, Message::User(_)) {
                            first_message = text_content.clone();
                        }
                        all_messages.push(text_content);
                    }
                }
            }
            _ => {}
        }
    }

    if let Some(sig) = signal {
        sig.throw_if_aborted()?;
    }

    let Some(hdr) = header else {
        return Ok(None);
    };

    let header_time = pi_js::time::parse_date(&hdr.timestamp);
    let created = header_time.unwrap_or(mtime_ms);
    let modified = match last_activity_time {
        Some(t) if t > 0 => t,
        _ => header_time.unwrap_or(mtime_ms),
    };

    Ok(Some(SessionInfo {
        path: file_path.to_owned(),
        id: hdr.id,
        cwd: hdr.cwd,
        name,
        parent_session_path: hdr.parent_session,
        created,
        modified,
        message_count,
        first_message: if first_message.is_empty() {
            "(no messages)".to_owned()
        } else {
            first_message
        },
        all_messages_text: all_messages.join(" "),
    }))
}

const MAX_CONCURRENT_SESSION_INFO_LOADS: usize = 10;
const CURRENT_SESSION_LIST_PUBLISH_INTERVAL: usize = 10;
const ALL_SESSION_LIST_PUBLISH_INTERVAL: usize = 100;

struct SessionFileCandidate {
    path: String,
    mtime_ms: Option<i64>,
}

fn sort_session_infos(mut sessions: Vec<SessionInfo>) -> Vec<SessionInfo> {
    sessions.sort_by(|a, b| b.modified.cmp(&a.modified));
    sessions
}

async fn list_sessions_from_dir(
    dir: &str,
    on_progress: Option<SessionListProgress<'_>>,
    signal: Option<&AbortSignal>,
) -> Result<Vec<SessionInfo>> {
    if let Some(sig) = signal {
        sig.throw_if_aborted()?;
    }
    if !pi_js::fs::exists(dir) {
        return Ok(Vec::new());
    }

    let dir_entries = match pi_js::fs::readdir(dir) {
        Ok(entries) => entries,
        Err(_) => {
            if let Some(sig) = signal {
                sig.throw_if_aborted()?;
            }
            return Ok(Vec::new());
        }
    };

    let mut files: Vec<String> = dir_entries
        .into_iter()
        .filter(|name| name.ends_with(".jsonl"))
        .collect();
    files.sort_by(|a, b| b.cmp(a));
    let candidates: Vec<SessionFileCandidate> = files
        .into_iter()
        .map(|name| SessionFileCandidate {
            path: pi_js::path::join(&[dir, &name]),
            mtime_ms: None,
        })
        .collect();

    let total = candidates.len();
    let mut partial_sessions: Vec<SessionInfo> = Vec::new();
    let mut loaded = 0usize;
    let mut results = Vec::new();

    for chunk in candidates.chunks(MAX_CONCURRENT_SESSION_INFO_LOADS) {
        if let Some(sig) = signal {
            sig.throw_if_aborted()?;
        }
        let futs = chunk.iter().map(|c| build_session_info(&c.path, signal, c.mtime_ms));
        let chunk_res = futures::future::join_all(futs).await;
        for item in chunk_res {
            let info_opt = item?;
            loaded += 1;
            if let Some(info) = info_opt {
                partial_sessions.push(info.clone());
                results.push(info);
            }
            if let Some(cb) = on_progress {
                let publish_partial =
                    loaded == 1 || loaded % CURRENT_SESSION_LIST_PUBLISH_INTERVAL == 0 || loaded == total;
                if publish_partial {
                    let sorted = sort_session_infos(partial_sessions.clone());
                    cb(loaded, total, Some(&sorted));
                } else {
                    cb(loaded, total, None);
                }
            }
        }
    }

    Ok(results)
}

struct SessionManagerInner {
    session_id: String,
    session_file: Option<String>,
    session_dir: String,
    cwd: String,
    persist: bool,
    flushed: bool,
    file_entries: Vec<FileEntry>,
    by_id: IndexMap<String, SessionEntry>,
    labels_by_id: IndexMap<String, String>,
    label_timestamps_by_id: IndexMap<String, String>,
    leaf_id: Option<String>,
}

impl SessionManagerInner {
    fn rewrite_file(&self) -> Result<()> {
        if !self.persist {
            return Ok(());
        }
        let Some(ref path) = self.session_file else {
            return Ok(());
        };
        let mut out = String::new();
        for entry in &self.file_entries {
            out.push_str(&pi_js::json::try_stringify(entry)?);
            out.push('\n');
        }
        pi_js::fs::write_file(path, out, None)
    }

    fn build_index(&mut self) {
        self.by_id.clear();
        self.labels_by_id.clear();
        self.label_timestamps_by_id.clear();
        self.leaf_id = None;

        for fe in &self.file_entries {
            let Some(entry) = fe.as_session_entry() else {
                continue;
            };
            let id = entry.id().to_owned();
            if let SessionEntry::Label(ref lbl) = entry {
                match lbl.label {
                    Some(ref text) if !text.is_empty() => {
                        self.labels_by_id.insert(lbl.target_id.clone(), text.clone());
                        self.label_timestamps_by_id
                            .insert(lbl.target_id.clone(), lbl.timestamp.clone());
                    }
                    _ => {
                        self.labels_by_id.shift_remove(&lbl.target_id);
                        self.label_timestamps_by_id.shift_remove(&lbl.target_id);
                    }
                }
            }
            self.by_id.insert(id.clone(), entry);
            self.leaf_id = Some(id);
        }
    }

    fn new_session(&mut self, options: Option<NewSessionOptions>) -> Result<Option<String>> {
        let id = match options.as_ref().and_then(|o| o.id.as_deref()) {
            Some(explicit) => {
                assert_valid_session_id(explicit)?;
                explicit.to_owned()
            }
            None => create_session_id(),
        };
        self.session_id = id;
        let timestamp = pi_js::time::iso_now();
        let header = SessionHeader {
            version: Some(CURRENT_SESSION_VERSION),
            id: self.session_id.clone(),
            timestamp: timestamp.clone(),
            cwd: self.cwd.clone(),
            parent_session: options.and_then(|o| o.parent_session),
        };
        self.file_entries = vec![FileEntry::Session(header)];
        self.by_id.clear();
        self.labels_by_id.clear();
        self.label_timestamps_by_id.clear();
        self.leaf_id = None;
        self.flushed = false;

        if self.persist {
            let file_ts = timestamp.replace([':', '.'], "-");
            self.session_file = Some(pi_js::path::join(&[
                &self.session_dir,
                &format!("{file_ts}_{}.jsonl", self.session_id),
            ]));
        }
        Ok(self.session_file.clone())
    }

    fn load_entries(&mut self, entries: Vec<FileEntry>, options: Option<NewSessionOptions>) -> Result<()> {
        let header = entries.iter().find_map(|e| match e {
            FileEntry::Session(h) => Some(h.clone()),
            _ => None,
        });

        if let Some(hdr) = header {
            self.file_entries = entries;
            self.session_id = hdr.id;
            if migrate_to_current_version(&mut self.file_entries) {
                self.rewrite_file()?;
            }
        } else {
            self.new_session(options)?;
            self.file_entries.extend(entries);
        }

        self.build_index();
        Ok(())
    }

    fn set_session_file(&mut self, session_file: &str, preloaded: Option<Vec<FileEntry>>) -> Result<()> {
        let resolved = resolve_path(session_file, None, PathInputOptions::default());
        self.session_file = Some(resolved.clone());

        if pi_js::fs::exists(&resolved) {
            let entries = match preloaded {
                Some(e) => e,
                None => load_entries_from_file(&resolved)?,
            };

            if entries.is_empty() {
                if pi_js::fs::stat(&resolved)?.size > 0 {
                    return Err(Error::msg(format!(
                        "Session file is not a valid {} session: {resolved}",
                        *APP_NAME
                    )));
                }
                self.new_session(None)?;
                self.session_file = Some(resolved);
                self.rewrite_file()?;
                self.flushed = true;
                return Ok(());
            }

            self.load_entries(entries, None)?;
            self.flushed = true;
        } else {
            self.new_session(None)?;
            self.session_file = Some(resolved);
        }
        Ok(())
    }

    fn has_conversation(&self) -> bool {
        self.file_entries.iter().any(|fe| {
            matches!(
                fe,
                FileEntry::Message(m)
                    if matches!(m.message.role(), Some("user") | Some("assistant"))
            )
        })
    }

    fn persist_entry(&mut self, entry: &SessionEntry) -> Result<()> {
        if !self.persist {
            return Ok(());
        }
        let Some(ref path) = self.session_file else {
            return Ok(());
        };
        if !self.flushed {
            if !self.has_conversation() {
                return Ok(());
            }
            let mut out = String::new();
            for fe in &self.file_entries {
                out.push_str(&pi_js::json::try_stringify(fe)?);
                out.push('\n');
            }
            pi_js::fs::write_file_with_flag(path, out, "wx", None)?;
            self.flushed = true;
        } else {
            let line = format!(
                "{}\n",
                pi_js::json::try_stringify(&FileEntry::from_session_entry(entry.clone()))?
            );
            pi_js::fs::append_file(path, line)?;
        }
        Ok(())
    }

    fn append_entry(&mut self, entry: SessionEntry) -> Result<()> {
        self.file_entries.push(FileEntry::from_session_entry(entry.clone()));
        let id = entry.id().to_owned();
        self.by_id.insert(id.clone(), entry.clone());
        self.leaf_id = Some(id);
        self.persist_entry(&entry)
    }

    fn get_entries(&self) -> Vec<SessionEntry> {
        self.file_entries
            .iter()
            .filter_map(FileEntry::as_session_entry)
            .collect()
    }

    fn build_session_projection(&self) -> SessionProjection {
        let entries = self.get_entries();
        build_session_projection(&entries, Some(self.leaf_id.as_deref()), Some(&self.by_id))
    }

    fn get_branch(&self, from_id: Option<&str>) -> Vec<SessionEntry> {
        let mut path = Vec::new();
        let start_id = from_id.or(self.leaf_id.as_deref());
        let mut current = start_id.and_then(|id| self.by_id.get(id).cloned());
        let mut visited = IndexSet::new();
        while let Some(cur) = current {
            if !visited.insert(cur.id().to_owned()) {
                break;
            }
            let pid = cur.parent_id().map(str::to_owned);
            path.push(cur);
            current = pid.and_then(|p| self.by_id.get(&p).cloned());
        }
        path.reverse();
        path
    }
}

#[derive(Clone)]
pub struct SessionManager {
    inner: Arc<Mutex<SessionManagerInner>>,
}

impl Default for SessionManager {
    fn default() -> Self {
        let mut inner = SessionManagerInner {
            session_id: String::new(),
            session_file: None,
            session_dir: String::new(),
            cwd: String::new(),
            persist: false,
            flushed: false,
            file_entries: Vec::new(),
            by_id: IndexMap::new(),
            labels_by_id: IndexMap::new(),
            label_timestamps_by_id: IndexMap::new(),
            leaf_id: None,
        };
        let _ = inner.new_session(None);
        Self {
            inner: Arc::new(Mutex::new(inner)),
        }
    }
}

impl SessionManager {
    fn init(
        cwd: &str,
        session_dir: &str,
        session_file: Option<&str>,
        persist: bool,
        new_session_options: Option<NewSessionOptions>,
        preloaded_entries: Option<Vec<FileEntry>>,
    ) -> Result<Self> {
        let resolved_cwd = resolve_path(cwd, None, PathInputOptions::default());
        let norm_dir = if session_dir.is_empty() {
            String::new()
        } else {
            normalize_path(session_dir, Default::default())
        };
        if persist && !norm_dir.is_empty() && !pi_js::fs::exists(&norm_dir) {
            pi_js::fs::mkdir(&norm_dir, true, None)?;
        }

        let mut inner = SessionManagerInner {
            session_id: String::new(),
            session_file: None,
            session_dir: norm_dir,
            cwd: resolved_cwd,
            persist,
            flushed: false,
            file_entries: Vec::new(),
            by_id: IndexMap::new(),
            labels_by_id: IndexMap::new(),
            label_timestamps_by_id: IndexMap::new(),
            leaf_id: None,
        };

        if let Some(file) = session_file {
            inner.set_session_file(file, preloaded_entries)?;
        } else if let Some(preloaded) = preloaded_entries
            && !preloaded.is_empty()
        {
            inner.load_entries(preloaded, new_session_options)?;
        } else {
            inner.new_session(new_session_options)?;
        }

        Ok(Self {
            inner: Arc::new(Mutex::new(inner)),
        })
    }

    pub fn set_session_file(&self, session_file: &str) -> Result<()> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set_session_file(session_file, None)
    }

    pub fn new_session(&self, options: Option<NewSessionOptions>) -> Result<Option<String>> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .new_session(options)
    }

    pub fn is_persisted(&self) -> bool {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner).persist
    }

    pub fn get_cwd(&self) -> String {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner).cwd.clone()
    }

    pub fn get_session_dir(&self) -> String {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .session_dir
            .clone()
    }

    pub fn uses_default_session_dir(&self) -> bool {
        let g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        g.session_dir == get_default_session_dir_path(&g.cwd, None)
    }

    pub fn get_session_id(&self) -> String {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .session_id
            .clone()
    }

    pub fn get_session_file(&self) -> Option<String> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .session_file
            .clone()
    }

    pub fn append_message(&self, message: AgentMessage) -> Result<String> {
        let mut g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let id = generate_id(&g.by_id);
        let entry = SessionEntry::Message(SessionMessageEntry {
            id: id.clone(),
            parent_id: g.leaf_id.clone(),
            timestamp: pi_js::time::iso_now(),
            message,
        });
        g.append_entry(entry)?;
        Ok(id)
    }

    pub fn append_thinking_level_change(&self, thinking_level: &str) -> Result<String> {
        let mut g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let id = generate_id(&g.by_id);
        let entry = SessionEntry::ThinkingLevelChange(ThinkingLevelChangeEntry {
            id: id.clone(),
            parent_id: g.leaf_id.clone(),
            timestamp: pi_js::time::iso_now(),
            thinking_level: thinking_level.to_owned(),
        });
        g.append_entry(entry)?;
        Ok(id)
    }

    pub fn append_model_change(&self, provider: &str, model_id: &str) -> Result<String> {
        let mut g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let id = generate_id(&g.by_id);
        let entry = SessionEntry::ModelChange(ModelChangeEntry {
            id: id.clone(),
            parent_id: g.leaf_id.clone(),
            timestamp: pi_js::time::iso_now(),
            provider: provider.to_owned(),
            model_id: model_id.to_owned(),
        });
        g.append_entry(entry)?;
        Ok(id)
    }

    pub fn append_usage(
        &self,
        kind: &str,
        provider: &str,
        model: &str,
        usage: Usage,
        note: Option<&str>,
    ) -> Result<UsageEntry> {
        let mut g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = UsageEntry {
            id: generate_id(&g.by_id),
            parent_id: g.leaf_id.clone(),
            timestamp: pi_js::time::iso_now(),
            kind: kind.to_owned(),
            provider: provider.to_owned(),
            model: model.to_owned(),
            usage,
            note: note.filter(|s| !s.is_empty()).map(str::to_owned),
        };
        g.append_entry(SessionEntry::Usage(entry.clone()))?;
        Ok(entry)
    }

    pub fn append_compaction(
        &self,
        summary: &str,
        first_kept_entry_id: Option<&str>,
        tokens_before: i64,
        details: Option<Value>,
        from_hook: Option<bool>,
        usage: Option<Usage>,
    ) -> Result<String> {
        let mut g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let timestamp = pi_js::time::iso_now();
        let proj = g.build_session_projection();
        let llm_msgs: Vec<&Message> = proj.messages.iter().filter_map(AgentMessage::as_message).collect();
        let system_message = get_current_system_message(&llm_msgs).map(|mut sys| {
            sys.timestamp = pi_js::time::parse_date(&timestamp).unwrap_or_else(pi_js::time::now_ms);
            sys
        });
        let id = generate_id(&g.by_id);
        let kept_id = first_kept_entry_id.unwrap_or(&id).to_owned();
        let entry = SessionEntry::Compaction(CompactionEntry {
            id: id.clone(),
            parent_id: g.leaf_id.clone(),
            timestamp,
            summary: summary.to_owned(),
            first_kept_entry_id: kept_id,
            tokens_before,
            details,
            usage,
            from_hook,
            system_message,
            first_kept_entry_index: None,
        });
        g.append_entry(entry)?;
        Ok(id)
    }

    pub fn append_custom_entry(&self, custom_type: &str, data: Option<Value>) -> Result<String> {
        let mut g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let id = generate_id(&g.by_id);
        let entry = SessionEntry::Custom(CustomEntry {
            id: id.clone(),
            parent_id: g.leaf_id.clone(),
            timestamp: pi_js::time::iso_now(),
            custom_type: custom_type.to_owned(),
            data,
        });
        g.append_entry(entry)?;
        Ok(id)
    }

    pub fn append_session_info(&self, name: &str) -> Result<String> {
        let mut g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let sanitized_name = name.replace("\r\n", " ").replace(['\r', '\n'], " ");
        let trimmed = pi_js::str16::trim(&sanitized_name).to_owned();
        let id = generate_id(&g.by_id);
        let entry = SessionEntry::SessionInfo(SessionInfoEntry {
            id: id.clone(),
            parent_id: g.leaf_id.clone(),
            timestamp: pi_js::time::iso_now(),
            name: Some(trimmed),
        });
        g.append_entry(entry)?;
        Ok(id)
    }

    pub fn get_session_name(&self) -> Option<String> {
        let g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        for fe in g.file_entries.iter().rev() {
            if let FileEntry::SessionInfo(si) = fe {
                let trimmed = si.name.as_deref().map(pi_js::str16::trim).unwrap_or("");
                return if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_owned())
                };
            }
        }
        None
    }

    pub fn append_custom_message_entry(
        &self,
        custom_type: &str,
        content: CustomMessageContent,
        display: bool,
        details: Option<Value>,
    ) -> Result<String> {
        let mut g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let id = generate_id(&g.by_id);
        let entry = SessionEntry::CustomMessage(CustomMessageEntry {
            id: id.clone(),
            parent_id: g.leaf_id.clone(),
            timestamp: pi_js::time::iso_now(),
            custom_type: custom_type.to_owned(),
            content,
            details,
            display,
        });
        g.append_entry(entry)?;
        Ok(id)
    }

    pub fn append_context_edit(&self, target_id: &str, replacement: Option<ContextEditReplacement>) -> Result<String> {
        let mut g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(target) = g.by_id.get(target_id).cloned() else {
            return Err(Error::msg(format!("Entry {target_id} not found")));
        };
        if !g.get_branch(None).iter().any(|e| e.id() == target_id) {
            return Err(Error::msg(format!("Entry {target_id} is not on the active branch")));
        }

        let target_role = match &target {
            SessionEntry::CustomMessage(_) => Some("custom"),
            SessionEntry::Message(m) => match m.message.role() {
                Some("user") => Some("user"),
                Some("assistant") => Some("assistant"),
                Some("toolResult") => Some("toolResult"),
                _ => None,
            },
            _ => None,
        };
        let Some(role) = target_role else {
            return Err(Error::msg(format!(
                "Entry {target_id} does not contribute editable model content"
            )));
        };

        let normalized_replacement = replacement.map(|rep| match rep.content {
            ContextEditableContent::Text(t) if role == "assistant" || role == "toolResult" => ContextEditReplacement {
                content: ContextEditableContent::Blocks(vec![serde_json::json!({
                    "type": "text",
                    "text": t
                })]),
            },
            other => ContextEditReplacement { content: other },
        });

        let id = generate_id(&g.by_id);
        let entry = SessionEntry::ContextEdit(ContextEditEntry {
            id: id.clone(),
            parent_id: g.leaf_id.clone(),
            timestamp: pi_js::time::iso_now(),
            target_id: target_id.to_owned(),
            replacement: normalized_replacement,
        });
        g.append_entry(entry)?;
        Ok(id)
    }

    pub fn get_leaf_id(&self) -> Option<String> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .leaf_id
            .clone()
    }

    pub fn get_leaf_entry(&self) -> Option<SessionEntry> {
        let g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        g.leaf_id.as_deref().and_then(|id| g.by_id.get(id).cloned())
    }

    pub fn get_entry(&self, id: &str) -> Option<SessionEntry> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .by_id
            .get(id)
            .cloned()
    }

    pub fn get_children(&self, parent_id: &str) -> Vec<SessionEntry> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .by_id
            .values()
            .filter(|e| e.parent_id() == Some(parent_id))
            .cloned()
            .collect()
    }

    pub fn get_label(&self, id: &str) -> Option<String> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .labels_by_id
            .get(id)
            .cloned()
    }

    pub fn append_label_change(&self, target_id: &str, label: Option<&str>) -> Result<String> {
        let mut g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if !g.by_id.contains_key(target_id) {
            return Err(Error::msg(format!("Entry {target_id} not found")));
        }
        let timestamp = pi_js::time::iso_now();
        let id = generate_id(&g.by_id);
        let entry = SessionEntry::Label(LabelEntry {
            id: id.clone(),
            parent_id: g.leaf_id.clone(),
            timestamp: timestamp.clone(),
            target_id: target_id.to_owned(),
            label: label.map(str::to_owned),
        });
        g.append_entry(entry)?;
        match label {
            Some(text) if !text.is_empty() => {
                g.labels_by_id.insert(target_id.to_owned(), text.to_owned());
                g.label_timestamps_by_id.insert(target_id.to_owned(), timestamp);
            }
            _ => {
                g.labels_by_id.shift_remove(target_id);
                g.label_timestamps_by_id.shift_remove(target_id);
            }
        }
        Ok(id)
    }

    pub fn get_branch(&self, from_id: Option<&str>) -> Vec<SessionEntry> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_branch(from_id)
    }

    pub fn build_context_entries(&self) -> Vec<SessionEntry> {
        let g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let entries = g.get_entries();
        build_context_entries(&entries, Some(g.leaf_id.as_deref()), Some(&g.by_id))
    }

    pub fn build_session_projection(&self) -> SessionProjection {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .build_session_projection()
    }

    pub fn build_session_context(&self) -> SessionContext {
        let proj = self.build_session_projection();
        SessionContext {
            messages: proj.messages,
            thinking_level: proj.thinking_level,
            model: proj.model,
        }
    }

    pub fn get_header(&self) -> Option<SessionHeader> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .file_entries
            .iter()
            .find_map(|fe| match fe {
                FileEntry::Session(h) => Some(h.clone()),
                _ => None,
            })
    }

    pub fn get_entry_count(&self) -> usize {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner).by_id.len()
    }

    pub fn get_entries(&self) -> Vec<SessionEntry> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner).get_entries()
    }

    pub fn get_tree(&self) -> Vec<SessionTreeNode> {
        let g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let entries = g.get_entries();
        let mut nodes: Vec<SessionTreeNode> = entries
            .iter()
            .map(|entry| SessionTreeNode {
                entry: entry.clone(),
                children: Vec::new(),
                label: g.labels_by_id.get(entry.id()).cloned(),
                label_timestamp: g.label_timestamps_by_id.get(entry.id()).cloned(),
            })
            .collect();

        let mut index_by_id: IndexMap<String, usize> = IndexMap::new();
        for (idx, n) in nodes.iter().enumerate() {
            index_by_id.insert(n.entry.id().to_owned(), idx);
        }

        let mut children_indices: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
        let mut root_indices: Vec<usize> = Vec::new();
        for (idx, n) in nodes.iter().enumerate() {
            match n.entry.parent_id() {
                None => root_indices.push(idx),
                Some(pid) if pid == n.entry.id() => root_indices.push(idx),
                Some(pid) => {
                    if let Some(&parent_idx) = index_by_id.get(pid) {
                        children_indices[parent_idx].push(idx);
                    } else {
                        root_indices.push(idx);
                    }
                }
            }
        }

        fn build_subtree(
            idx: usize,
            nodes: &mut Vec<Option<SessionTreeNode>>,
            children_indices: &[Vec<usize>],
        ) -> SessionTreeNode {
            let mut node = nodes[idx].take().unwrap_or_else(|| unreachable!());
            let mut kids: Vec<SessionTreeNode> = children_indices[idx]
                .iter()
                .map(|&child_idx| build_subtree(child_idx, nodes, children_indices))
                .collect();
            kids.sort_by_key(|c| pi_js::time::parse_date(c.entry.timestamp()).unwrap_or(0));
            node.children = kids;
            node
        }

        let mut opt_nodes: Vec<Option<SessionTreeNode>> = nodes.drain(..).map(Some).collect();
        let mut roots: Vec<SessionTreeNode> = root_indices
            .into_iter()
            .map(|idx| build_subtree(idx, &mut opt_nodes, &children_indices))
            .collect();
        roots.sort_by_key(|r| pi_js::time::parse_date(r.entry.timestamp()).unwrap_or(0));
        roots
    }

    pub fn branch(&self, branch_from_id: &str) -> Result<()> {
        let mut g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if !g.by_id.contains_key(branch_from_id) {
            return Err(Error::msg(format!("Entry {branch_from_id} not found")));
        }
        g.leaf_id = Some(branch_from_id.to_owned());
        Ok(())
    }

    pub fn reset_leaf(&self) {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner).leaf_id = None;
    }

    pub fn branch_with_summary(
        &self,
        branch_from_id: Option<&str>,
        summary: &str,
        details: Option<Value>,
        from_hook: Option<bool>,
        usage: Option<Usage>,
    ) -> Result<String> {
        let mut g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(bid) = branch_from_id
            && !g.by_id.contains_key(bid)
        {
            return Err(Error::msg(format!("Entry {bid} not found")));
        }
        let from_id = g.leaf_id.clone().unwrap_or_else(|| "root".to_owned());
        g.leaf_id = branch_from_id.map(str::to_owned);
        let id = generate_id(&g.by_id);
        let entry = SessionEntry::BranchSummary(BranchSummaryEntry {
            id: id.clone(),
            parent_id: branch_from_id.map(str::to_owned),
            timestamp: pi_js::time::iso_now(),
            from_id,
            summary: summary.to_owned(),
            details,
            usage,
            from_hook,
        });
        g.append_entry(entry)?;
        Ok(id)
    }

    pub fn create_branched_session(&self, leaf_id: &str) -> Result<Option<String>> {
        let mut g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let previous_session_file = g.session_file.clone();
        let path = g.get_branch(Some(leaf_id));
        if path.is_empty() {
            return Err(Error::msg(format!("Entry {leaf_id} not found")));
        }

        let mut path_without_labels: Vec<SessionEntry> = Vec::new();
        let mut replacement_by_label_id: IndexMap<String, String> = IndexMap::new();
        let mut pending_label_ids: Vec<String> = Vec::new();
        let mut path_parent_id: Option<String> = None;

        for entry in path {
            if let SessionEntry::Label(lbl) = entry {
                pending_label_ids.push(lbl.id);
                continue;
            }
            let entry_id = entry.id().to_owned();
            for lbl_id in pending_label_ids.drain(..) {
                replacement_by_label_id.insert(lbl_id, entry_id.clone());
            }
            let mut cloned = entry;
            cloned.set_parent_id(path_parent_id);
            if let SessionEntry::Compaction(ref mut comp) = cloned
                && comp.first_kept_entry_id != comp.id
                && let Some(rep) = replacement_by_label_id.get(&comp.first_kept_entry_id)
            {
                comp.first_kept_entry_id = rep.clone();
            }
            path_parent_id = Some(entry_id);
            path_without_labels.push(cloned);
        }

        let new_session_id = create_session_id();
        let timestamp = pi_js::time::iso_now();
        let file_ts = timestamp.replace([':', '.'], "-");
        let new_session_file = pi_js::path::join(&[&g.session_dir, &format!("{file_ts}_{new_session_id}.jsonl")]);

        let header = SessionHeader {
            version: Some(CURRENT_SESSION_VERSION),
            id: new_session_id.clone(),
            timestamp,
            cwd: g.cwd.clone(),
            parent_session: if g.persist { previous_session_file } else { None },
        };

        let mut path_entry_ids: IndexSet<String> = path_without_labels.iter().map(|e| e.id().to_owned()).collect();
        let mut labels_to_write: Vec<(String, String, String)> = Vec::new();
        for (target_id, label) in &g.labels_by_id {
            if path_entry_ids.contains(target_id)
                && let Some(ts) = g.label_timestamps_by_id.get(target_id)
            {
                labels_to_write.push((target_id.clone(), label.clone(), ts.clone()));
            }
        }

        let mut parent_id = path_without_labels.last().map(|e| e.id().to_owned());
        let mut label_entries: Vec<SessionEntry> = Vec::with_capacity(labels_to_write.len());
        for (target_id, label, label_ts) in labels_to_write {
            let lbl_id = generate_id_from_set(&path_entry_ids);
            path_entry_ids.insert(lbl_id.clone());
            let entry = SessionEntry::Label(LabelEntry {
                id: lbl_id.clone(),
                parent_id,
                timestamp: label_ts,
                target_id,
                label: Some(label),
            });
            label_entries.push(entry);
            parent_id = Some(lbl_id);
        }

        let mut new_file_entries = Vec::with_capacity(1 + path_without_labels.len() + label_entries.len());
        new_file_entries.push(FileEntry::Session(header));
        new_file_entries.extend(path_without_labels.into_iter().map(FileEntry::from_session_entry));
        new_file_entries.extend(label_entries.into_iter().map(FileEntry::from_session_entry));

        if g.persist {
            g.file_entries = new_file_entries;
            g.session_id = new_session_id;
            g.session_file = Some(new_session_file.clone());
            g.build_index();
            if g.has_conversation() {
                g.rewrite_file()?;
                g.flushed = true;
            } else {
                g.flushed = false;
            }
            Ok(Some(new_session_file))
        } else {
            g.file_entries = new_file_entries;
            g.session_id = new_session_id;
            g.build_index();
            Ok(None)
        }
    }

    pub fn create(cwd: &str, session_dir: Option<&str>, options: Option<NewSessionOptions>) -> Result<Self> {
        let dir = match session_dir {
            Some(d) => normalize_path(d, Default::default()),
            None => get_default_session_dir(cwd, None)?,
        };
        Self::init(cwd, &dir, None, true, options, None)
    }

    pub fn open(path: &str, session_dir: Option<&str>, cwd_override: Option<&str>) -> Result<Self> {
        let resolved_path = resolve_path(path, None, PathInputOptions::default());
        let mut header: Option<SessionHeader> = None;
        let mut preloaded: Option<Vec<FileEntry>> = None;

        if cwd_override.is_none() && pi_js::fs::exists(&resolved_path) {
            match read_session_header(&resolved_path) {
                Ok(h) => header = h,
                Err(err) if err.code() == Some(SESSION_HEADER_SCAN_LIMIT_CODE) => {
                    let loaded = load_entries_from_file(&resolved_path)?;
                    header = loaded.first().and_then(|fe| match fe {
                        FileEntry::Session(h) => Some(h.clone()),
                        _ => None,
                    });
                    preloaded = Some(loaded);
                }
                Err(err) => return Err(err),
            }
        }

        let env_cwd = pi_js::env::cwd();
        let cwd = match cwd_override {
            Some(c) => c,
            None => header.as_ref().and_then(get_session_header_cwd).unwrap_or(&env_cwd),
        };
        let dir = match session_dir {
            Some(d) => normalize_path(d, Default::default()),
            None => resolve_path("..", Some(&resolved_path), PathInputOptions::default()),
        };
        Self::init(cwd, &dir, Some(&resolved_path), true, None, preloaded)
    }

    pub fn continue_recent(cwd: &str, session_dir: Option<&str>) -> Result<Self> {
        let dir = match session_dir {
            Some(d) => normalize_path(d, Default::default()),
            None => get_default_session_dir(cwd, None)?,
        };
        let filter_cwd = session_dir.is_some() && dir != get_default_session_dir_path(cwd, None);
        let most_recent = find_most_recent_session(&dir, if filter_cwd { Some(cwd) } else { None });
        Self::init(cwd, &dir, most_recent.as_deref(), true, None, None)
    }

    pub fn in_memory(
        cwd: Option<&str>,
        options: Option<NewSessionOptions>,
        entries: Option<Vec<FileEntry>>,
    ) -> Result<Self> {
        let env_cwd;
        let effective_cwd = match cwd {
            Some(c) => c,
            None => {
                env_cwd = pi_js::env::cwd();
                &env_cwd
            }
        };
        Self::init(effective_cwd, "", None, false, options, entries)
    }

    pub fn fork_from(
        source_path: &str,
        target_cwd: &str,
        session_dir: Option<&str>,
        options: Option<NewSessionOptions>,
    ) -> Result<Self> {
        let resolved_source = resolve_path(source_path, None, PathInputOptions::default());
        let resolved_target_cwd = resolve_path(target_cwd, None, PathInputOptions::default());
        let source_entries = load_entries_from_file(&resolved_source)?;
        if source_entries.is_empty() {
            return Err(Error::msg(format!(
                "Cannot fork: source session file is empty or invalid: {resolved_source}"
            )));
        }
        let has_header = source_entries.iter().any(|e| matches!(e, FileEntry::Session(_)));
        if !has_header {
            return Err(Error::msg(format!(
                "Cannot fork: source session has no header: {resolved_source}"
            )));
        }

        let dir = match session_dir {
            Some(d) => normalize_path(d, Default::default()),
            None => get_default_session_dir(&resolved_target_cwd, None)?,
        };
        if !pi_js::fs::exists(&dir) {
            pi_js::fs::mkdir(&dir, true, None)?;
        }

        let new_session_id = match options.as_ref().and_then(|o| o.id.as_deref()) {
            Some(explicit) => {
                assert_valid_session_id(explicit)?;
                explicit.to_owned()
            }
            None => create_session_id(),
        };
        let timestamp = pi_js::time::iso_now();
        let file_ts = timestamp.replace([':', '.'], "-");
        let new_session_file = pi_js::path::join(&[&dir, &format!("{file_ts}_{new_session_id}.jsonl")]);

        let new_header = FileEntry::Session(SessionHeader {
            version: Some(CURRENT_SESSION_VERSION),
            id: new_session_id,
            timestamp,
            cwd: resolved_target_cwd.clone(),
            parent_session: Some(resolved_source),
        });
        let mut out = format!("{}\n", pi_js::json::try_stringify(&new_header)?);
        for entry in &source_entries {
            if !matches!(entry, FileEntry::Session(_)) {
                out.push_str(&pi_js::json::try_stringify(entry)?);
                out.push('\n');
            }
        }
        pi_js::fs::write_file_with_flag(&new_session_file, out, "wx", None)?;

        Self::init(&resolved_target_cwd, &dir, Some(&new_session_file), true, None, None)
    }

    pub fn find_by_id(cwd: &str, id: &str, session_dir: Option<&str>) -> Option<String> {
        let dir = match session_dir {
            Some(d) => normalize_path(d, Default::default()),
            None => get_default_session_dir(cwd, None).ok()?,
        };
        let filter_cwd = session_dir.is_some() && dir != get_default_session_dir_path(cwd, None);
        let resolved_cwd = resolve_path(cwd, None, PathInputOptions::default());

        let files = pi_js::fs::readdir(&dir).ok()?;
        for file in files {
            if !file.ends_with(".jsonl") {
                continue;
            }
            let path = pi_js::path::join(&[&dir, &file]);
            let Some(header) = read_session_header_for_discovery(&path) else {
                continue;
            };
            if header.id != id {
                continue;
            }
            if filter_cwd && !session_cwd_matches(get_session_header_cwd(&header), &resolved_cwd) {
                continue;
            }
            return Some(path);
        }
        None
    }

    pub async fn list(
        cwd: &str,
        session_dir: Option<&str>,
        on_progress: Option<SessionListProgress<'_>>,
        signal: Option<&AbortSignal>,
    ) -> Result<Vec<SessionInfo>> {
        let dir = match session_dir {
            Some(d) => normalize_path(d, Default::default()),
            None => get_default_session_dir(cwd, None)?,
        };
        let filter_cwd = session_dir.is_some() && dir != get_default_session_dir_path(cwd, None);
        let resolved_cwd = resolve_path(cwd, None, PathInputOptions::default());
        let include_session =
            |session: &SessionInfo| !filter_cwd || session_cwd_matches(Some(&session.cwd), &resolved_cwd);

        let wrapped_progress = |loaded: usize, total: usize, partial: Option<&[SessionInfo]>| {
            if let Some(cb) = on_progress {
                if let Some(list) = partial {
                    let filtered: Vec<SessionInfo> = list.iter().filter(|s| include_session(s)).cloned().collect();
                    cb(loaded, total, Some(&filtered));
                } else {
                    cb(loaded, total, None);
                }
            }
        };

        let sessions = list_sessions_from_dir(
            &dir,
            if on_progress.is_some() {
                Some(&wrapped_progress)
            } else {
                None
            },
            signal,
        )
        .await?;
        let filtered: Vec<SessionInfo> = sessions.into_iter().filter(|s| include_session(s)).collect();
        Ok(sort_session_infos(filtered))
    }

    pub async fn list_all(
        custom_session_dir: Option<&str>,
        on_progress: Option<SessionListProgress<'_>>,
        signal: Option<&AbortSignal>,
    ) -> Result<Vec<SessionInfo>> {
        if let Some(sig) = signal {
            sig.throw_if_aborted()?;
        }
        if let Some(custom_dir) = custom_session_dir {
            let norm = normalize_path(custom_dir, Default::default());
            let sessions = list_sessions_from_dir(&norm, on_progress, signal).await?;
            return Ok(sort_session_infos(sessions));
        }

        let sessions_dir = get_sessions_dir();
        if !pi_js::fs::exists(&sessions_dir) {
            return Ok(Vec::new());
        }

        let Ok(dir_entries) = pi_js::fs::readdir_with_file_types(&sessions_dir) else {
            if let Some(sig) = signal {
                sig.throw_if_aborted()?;
            }
            return Ok(Vec::new());
        };

        let dirs: Vec<String> = dir_entries
            .into_iter()
            .filter(|e| e.is_directory() || e.is_symbolic_link())
            .map(|e| pi_js::path::join(&[&sessions_dir, &e.name]))
            .collect();

        let mut all_files = Vec::new();
        for d in dirs {
            if let Some(sig) = signal {
                sig.throw_if_aborted()?;
            }
            if let Ok(files) = pi_js::fs::readdir(&d) {
                for f in files {
                    if f.ends_with(".jsonl") {
                        all_files.push(pi_js::path::join(&[&d, &f]));
                    }
                }
            }
        }

        let mut candidates: Vec<SessionFileCandidate> = all_files
            .into_iter()
            .map(|path| SessionFileCandidate {
                mtime_ms: pi_js::fs::stat(&path).ok().map(|s| s.mtime()),
                path,
            })
            .collect();

        candidates.sort_by(|a, b| {
            let am = a.mtime_ms.unwrap_or(i64::MIN);
            let bm = b.mtime_ms.unwrap_or(i64::MIN);
            bm.cmp(&am)
                .then_with(|| pi_js::path::basename(&b.path, None).cmp(&pi_js::path::basename(&a.path, None)))
        });

        let total_files = candidates.len();
        let mut loaded = 0usize;
        let mut partial_sessions: Vec<SessionInfo> = Vec::new();
        let mut results: Vec<SessionInfo> = Vec::new();

        for chunk in candidates.chunks(MAX_CONCURRENT_SESSION_INFO_LOADS) {
            if let Some(sig) = signal {
                sig.throw_if_aborted()?;
            }
            let futs = chunk.iter().map(|c| build_session_info(&c.path, signal, c.mtime_ms));
            let chunk_res = futures::future::join_all(futs).await;
            for item in chunk_res {
                let info_opt = item?;
                loaded += 1;
                if let Some(info) = info_opt {
                    partial_sessions.push(info.clone());
                    results.push(info);
                }
                if let Some(cb) = on_progress {
                    let publish =
                        loaded == 1 || loaded % ALL_SESSION_LIST_PUBLISH_INTERVAL == 0 || loaded == total_files;
                    if publish {
                        let sorted = sort_session_infos(partial_sessions.clone());
                        cb(loaded, total_files, Some(&sorted));
                    } else {
                        cb(loaded, total_files, None);
                    }
                }
            }
        }

        Ok(sort_session_infos(results))
    }
}
