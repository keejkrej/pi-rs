//! Port of packages/durable/test/sqlite-migrations.test.ts

use pi_chord::background_context;
use pi_durable::ids::{EntryId, id_from_number};
use pi_durable::storage::sqlite::node::{
    NodeSqliteStorageOptions, open_node_sqlite_database, open_node_sqlite_storage,
};
use pi_durable::storage::sqlite::{
    CURRENT_SQLITE_SCHEMA_VERSION, SQLITE_MIGRATIONS, SqliteDatabase, SqliteMigration,
    SqliteRowExt, SqliteValue, apply_sqlite_migrations,
};
use pi_durable::types::{
    ConversationRecord, EntryRecord, ROOT_CONVERSATION_ID, Storage, StorageWrite,
};
use serde_json::json;

fn temp_database_path() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("storage.sqlite");
    let path_str = path.to_string_lossy().into_owned();
    (dir, path_str)
}

#[tokio::test]
async fn creates_current_schema_and_can_be_applied_repeatedly() {
    let (_dir, path) = temp_database_path();
    let database = open_node_sqlite_database(&path, NodeSqliteStorageOptions::default())
        .await
        .expect("open db");

    apply_sqlite_migrations(database.as_ref(), None)
        .await
        .expect("first migrate");
    apply_sqlite_migrations(database.as_ref(), None)
        .await
        .expect("second migrate");

    let schema_row = database
        .get("SELECT version FROM durable_schema WHERE singleton = 1", &[])
        .await
        .expect("select version")
        .expect("version row");
    assert_eq!(
        schema_row.get_i64("version").expect("version i64"),
        CURRENT_SQLITE_SCHEMA_VERSION
    );

    let metadata_row = database
        .get(
            "SELECT next_id, next_seq FROM durable_metadata WHERE singleton = 1",
            &[],
        )
        .await
        .expect("select metadata")
        .expect("metadata row");
    assert_eq!(metadata_row.get_str("next_id").expect("next_id"), "2");
    assert_eq!(metadata_row.get_i64("next_seq").expect("next_seq"), 1);

    database.close().await.expect("close db");
}

#[tokio::test]
async fn rejects_database_newer_than_portable_core() {
    let (_dir, path) = temp_database_path();
    let database = open_node_sqlite_database(&path, NodeSqliteStorageOptions::default())
        .await
        .expect("open db");
    apply_sqlite_migrations(database.as_ref(), None)
        .await
        .expect("migrate");
    database
        .run(
            "UPDATE durable_schema SET version = ? WHERE singleton = 1",
            &[SqliteValue::Integer(CURRENT_SQLITE_SCHEMA_VERSION + 1)],
        )
        .await
        .expect("bump version");
    database.close().await.expect("close db");

    let err = match open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default()).await {
        Ok(_) => panic!("expected newer database version error"),
        Err(err) => err,
    };
    assert!(
        err.to_string().contains("is newer than supported version"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn rolls_initial_bootstrap_and_every_pending_migration_back_together() {
    let (_dir, path) = temp_database_path();
    let database = open_node_sqlite_database(&path, NodeSqliteStorageOptions::default())
        .await
        .expect("open db");

    let first = SqliteMigration::borrowed(
        1,
        &[
            "CREATE TABLE migration_first (value TEXT) STRICT",
            "INSERT INTO migration_first (value) VALUES ('retained')",
        ],
    );
    let failed = vec![
        first.clone(),
        SqliteMigration::borrowed(
            2,
            &[
                "CREATE TABLE migration_second (value TEXT) STRICT",
                "THIS IS NOT SQL",
            ],
        ),
    ];

    assert!(apply_sqlite_migrations(database.as_ref(), Some(&failed)).await.is_err());

    let count_row = database
        .get(
            "SELECT count(*) AS count FROM sqlite_schema WHERE name IN ('durable_schema', 'migration_first', 'migration_second')",
            &[],
        )
        .await
        .expect("check schema count")
        .expect("count row");
    assert_eq!(count_row.get_i64("count").expect("count"), 0);

    let succeeding = vec![
        first,
        SqliteMigration::borrowed(2, &["CREATE TABLE migration_second (value TEXT) STRICT"]),
    ];
    apply_sqlite_migrations(database.as_ref(), Some(&succeeding))
        .await
        .expect("retry succeeding migrations");

    let schema_row = database
        .get("SELECT version FROM durable_schema WHERE singleton = 1", &[])
        .await
        .expect("select version")
        .expect("row");
    assert_eq!(schema_row.get_i64("version").expect("version"), 2);

    let retained = database
        .get("SELECT value FROM migration_first", &[])
        .await
        .expect("select migration_first")
        .expect("retained row");
    assert_eq!(retained.get_str("value").expect("value"), "retained");

    database.close().await.expect("close db");
}

#[tokio::test]
async fn rolls_failed_migration_back_and_preserves_stored_data_for_successful_retry() {
    let context = background_context();
    let (_dir, path) = temp_database_path();
    let storage = open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default())
        .await
        .expect("open storage");

    storage
        .commit(
            vec![
                StorageWrite::Conversation {
                    value: ConversationRecord {
                        id: ROOT_CONVERSATION_ID,
                        parent: None,
                        owner: None,
                    },
                },
                StorageWrite::Entry {
                    value: EntryRecord {
                        id: id_from_number::<EntryId>(2),
                        conversation_id: ROOT_CONVERSATION_ID,
                        kind: "retained".to_string(),
                        model: None,
                        data: Some(json!({ "retained": true })),
                        head: None,
                        edits: None,
                        by_task_id: None,
                    },
                },
            ],
            context.clone(),
        )
        .await
        .expect("commit retained entry");
    storage.close(context).await.expect("close storage");

    let database = open_node_sqlite_database(&path, NodeSqliteStorageOptions::default())
        .await
        .expect("reopen db");
    let next_version = CURRENT_SQLITE_SCHEMA_VERSION + 1;

    let mut failed_migrations = SQLITE_MIGRATIONS.clone();
    failed_migrations.push(SqliteMigration::borrowed(
        next_version,
        &[
            "CREATE TABLE migration_probe (value TEXT) STRICT",
            "THIS IS NOT SQL",
        ],
    ));
    assert!(
        apply_sqlite_migrations(database.as_ref(), Some(&failed_migrations))
            .await
            .is_err()
    );

    let version_row = database
        .get("SELECT version FROM durable_schema WHERE singleton = 1", &[])
        .await
        .expect("get version")
        .expect("row");
    assert_eq!(
        version_row.get_i64("version").expect("version"),
        CURRENT_SQLITE_SCHEMA_VERSION
    );

    let probe_row = database
        .get(
            "SELECT count(*) AS count FROM sqlite_schema WHERE type = 'table' AND name = 'migration_probe'",
            &[],
        )
        .await
        .expect("check probe table")
        .expect("row");
    assert_eq!(probe_row.get_i64("count").expect("count"), 0);

    let mut successful_migrations = SQLITE_MIGRATIONS.clone();
    successful_migrations.push(SqliteMigration::borrowed(
        next_version,
        &["CREATE TABLE migration_probe (value TEXT) STRICT"],
    ));
    apply_sqlite_migrations(database.as_ref(), Some(&successful_migrations))
        .await
        .expect("retry migration");

    let new_version_row = database
        .get("SELECT version FROM durable_schema WHERE singleton = 1", &[])
        .await
        .expect("get version")
        .expect("row");
    assert_eq!(new_version_row.get_i64("version").expect("version"), next_version);

    let entry_row = database
        .get("SELECT record, commit_seq FROM entries WHERE id = 2", &[])
        .await
        .expect("get entry")
        .expect("entry row");
    assert_eq!(entry_row.get_i64("commit_seq").expect("commit_seq"), 1);
    let parsed_entry: EntryRecord =
        serde_json::from_str(entry_row.get_str("record").expect("record str"))
            .expect("parse entry");
    assert_eq!(
        parsed_entry,
        EntryRecord {
            id: id_from_number::<EntryId>(2),
            conversation_id: ROOT_CONVERSATION_ID,
            kind: "retained".to_string(),
            model: None,
            data: Some(json!({ "retained": true })),
            head: None,
            edits: None,
            by_task_id: None,
        }
    );

    let meta_row = database
        .get(
            "SELECT next_id, next_seq FROM durable_metadata WHERE singleton = 1",
            &[],
        )
        .await
        .expect("get meta")
        .expect("meta row");
    assert_eq!(meta_row.get_str("next_id").expect("next_id"), "3");
    assert_eq!(meta_row.get_i64("next_seq").expect("next_seq"), 2);

    database.close().await.expect("close db");
}
