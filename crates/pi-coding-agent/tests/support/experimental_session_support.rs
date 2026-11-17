//! Port of packages/coding-agent/test/experimental-session-support.ts

use std::sync::Arc;

use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::harness::types::ModelRef;
use pi_durable::storage::sqlite::node::{NodeSqliteStorageOptions, open_node_sqlite_storage};
use pi_durable::{AGENT_DOC, ROOT_CONVERSATION_ID, create_session};
use pi_js::fs::promises::{mkdir, write_file_with_flag};
use pi_js::path::join;

use pi_coding_agent::experimental::session_catalog::{
    CreateSessionOptions, SessionCatalogMetadata, create_session as create_catalog_session, read_session,
    session_storage_path,
};

pub async fn create_experimental_sessions(
    sessions_root: &str,
    ids: &[&str],
    cwd: Option<&str>,
) -> pi_js::Result<Vec<SessionCatalogMetadata>> {
    let resolved_cwd = cwd.map(ToString::to_string).unwrap_or_else(pi_js::env::cwd);
    let mut metadata = Vec::with_capacity(ids.len());
    for id in ids {
        metadata.push(
            create_catalog_session(
                sessions_root,
                CreateSessionOptions {
                    id: Some((*id).to_string()),
                    cwd: resolved_cwd.clone(),
                },
            )
            .await?,
        );
    }
    Ok(metadata)
}

pub async fn configure_experimental_worker_model(agent_dir: &str) -> pi_js::Result<()> {
    mkdir(agent_dir, true, None).await?;
    let auth = serde_json::json!({
        "anthropic": {
            "type": "api_key",
            "key": "test-key",
        }
    });
    let encoded = pi_js::json::stringify(&auth);
    write_file_with_flag(&join(&[agent_dir, "auth.json"]), encoded.as_bytes(), "w", Some(0o600)).await
}

pub struct ReadSessionState {
    pub model: Option<ModelRef>,
}

/// The root conversation's model, read from the Session's durable storage while no worker owns it.
pub async fn read_experimental_session_state(sessions_root: &str, session_id: &str) -> pi_js::Result<ReadSessionState> {
    let metadata = read_session(sessions_root, session_id)
        .await?
        .ok_or_else(|| pi_js::Error::msg(format!("Expected Session {session_id}")))?;
    let storage = Arc::new(
        open_node_sqlite_storage(&session_storage_path(&metadata), NodeSqliteStorageOptions::default()).await?,
    );
    let session = create_session(storage, None);
    let res = session
        .snapshot_conversation(&*AGENT_DOC, ROOT_CONVERSATION_ID, BACKGROUND_CONTEXT.clone())
        .await;
    let close_res = session.close(BACKGROUND_CONTEXT.clone()).await;
    let agent = res?;
    close_res?;
    Ok(ReadSessionState {
        model: agent.and_then(|a| a.model),
    })
}
