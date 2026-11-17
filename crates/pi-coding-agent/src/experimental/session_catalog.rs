//! Port of packages/coding-agent/src/experimental/session-catalog.ts

use std::sync::LazyLock;

use pi_js::crypto::random_uuid;
use pi_js::fs::promises::{mkdir, read_to_string, readdir, rm, write_file};
use pi_js::json::{parse, stringify_pretty};
use pi_js::path::join;
use pi_js::time::now_ms;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One server-hosted Session: a directory holding `meta.json` and the worker-owned `session.sqlite`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCatalogMetadata {
    pub id: String,
    pub created_at: i64,
    /// The working directory the Session's agent runs in.
    pub cwd: String,
    /// The Session directory. Workers lock it and own the storage inside it.
    pub path: String,
}

#[derive(Clone, Debug, Default)]
pub struct CreateSessionOptions {
    pub id: Option<String>,
    pub cwd: String,
}

const METADATA_FILE: &str = "meta.json";
const STORAGE_FILE: &str = "session.sqlite";

static SESSION_ID_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$").unwrap_or_else(|e| panic!("invalid session id regex: {e}"))
});

pub fn is_session_id(id: &str) -> bool {
    SESSION_ID_RE.is_match(id)
}

/// The durable storage file of a Session. Only the Session's worker opens it.
pub fn session_storage_path(metadata: &SessionCatalogMetadata) -> String {
    join(&[&metadata.path, STORAGE_FILE])
}

/// Every Session in the directory. Entries without valid metadata are skipped.
pub async fn list_sessions(session_dir: &str) -> pi_js::Result<Vec<SessionCatalogMetadata>> {
    let names = match readdir(session_dir).await {
        Ok(names) => names,
        Err(error) if error.code() == Some("ENOENT") => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut sessions = Vec::new();
    for name in names.into_iter().filter(|n| is_session_id(n)) {
        if let Some(meta) = read_session(session_dir, &name).await? {
            sessions.push(meta);
        }
    }
    Ok(sessions)
}

/// One Session by ID, or None when it does not exist.
pub async fn read_session(session_dir: &str, id: &str) -> pi_js::Result<Option<SessionCatalogMetadata>> {
    if !is_session_id(id) {
        return Ok(None);
    }
    let path = join(&[session_dir, id]);
    let raw = match read_to_string(&join(&[&path, METADATA_FILE])).await {
        Ok(raw) => raw,
        Err(_) => return Ok(None),
    };
    let value = match parse(&raw) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let Some(obj) = value.as_object() else {
        return Ok(None);
    };
    let Some(created_at_num) = obj.get("createdAt").and_then(Value::as_f64) else {
        return Ok(None);
    };
    if !created_at_num.is_finite() {
        return Ok(None);
    }
    let Some(cwd) = obj.get("cwd").and_then(Value::as_str) else {
        return Ok(None);
    };
    Ok(Some(SessionCatalogMetadata {
        id: id.to_string(),
        created_at: created_at_num as i64,
        cwd: cwd.to_string(),
        path,
    }))
}

/// Create an empty Session. Its worker creates the storage on first open.
pub async fn create_session(session_dir: &str, options: CreateSessionOptions) -> pi_js::Result<SessionCatalogMetadata> {
    let id = options.id.unwrap_or_else(random_uuid);
    if !is_session_id(&id) {
        return Err(pi_js::Error::msg(format!("Invalid session ID: {id}")));
    }
    let path = join(&[session_dir, &id]);
    mkdir(session_dir, true, None).await?;
    if let Err(error) = mkdir(&path, false, None).await {
        if error.code() == Some("EEXIST") {
            return Err(pi_js::Error::msg(format!("Session {id} already exists")));
        }
        return Err(error);
    }
    let metadata = SessionCatalogMetadata {
        id,
        created_at: now_ms(),
        cwd: options.cwd,
        path: path.clone(),
    };
    let mut obj = serde_json::Map::new();
    obj.insert("createdAt".to_string(), Value::from(metadata.created_at));
    obj.insert("cwd".to_string(), Value::String(metadata.cwd.clone()));
    let formatted = format!("{}\n", stringify_pretty(&Value::Object(obj), "\t"));
    write_file(&join(&[&path, METADATA_FILE]), formatted.as_bytes(), None).await?;
    Ok(metadata)
}

/// Delete a Session directory. Its worker must be closed first.
pub async fn delete_session(metadata: &SessionCatalogMetadata) -> pi_js::Result<()> {
    rm(&metadata.path, true, true).await
}
