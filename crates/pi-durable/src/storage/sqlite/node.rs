//! Port of packages/durable/src/storage/sqlite/node.ts

use std::any::Any;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rusqlite::types::{Value as RusqliteValue, ValueRef};
use tokio::sync::Mutex as AsyncMutex;

use crate::storage::sqlite::database::{
    BoxedSqliteTxCallback, SqliteDatabase, SqliteExecutor, SqliteRow, SqliteValue,
};
use crate::storage::sqlite::storage::SqliteStorage;

const DEFAULT_WAL_AUTO_CHECKPOINT_PAGES: i64 = 1_000;
const DEFAULT_BUSY_TIMEOUT_MS: u64 = 5_000;

/// Connection settings for a durable SQLite storage file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NodeSqliteStorageOptions {
    pub wal_auto_checkpoint_pages: Option<i64>,
    pub busy_timeout_ms: Option<u64>,
}

struct ConnectionState {
    conn: Option<rusqlite::Connection>,
    closed: bool,
}

fn to_rusqlite_value(value: &SqliteValue) -> RusqliteValue {
    match value {
        SqliteValue::Null => RusqliteValue::Null,
        SqliteValue::Integer(v) => RusqliteValue::Integer(*v),
        SqliteValue::Real(v) => RusqliteValue::Real(*v),
        SqliteValue::Text(s) => RusqliteValue::Text(s.clone()),
        SqliteValue::Blob(b) => RusqliteValue::Blob(b.clone()),
    }
}

fn from_rusqlite_value(value: ValueRef<'_>) -> pi_js::Result<SqliteValue> {
    match value {
        ValueRef::Null => Ok(SqliteValue::Null),
        ValueRef::Integer(v) => Ok(SqliteValue::Integer(v)),
        ValueRef::Real(v) => Ok(SqliteValue::Real(v)),
        ValueRef::Text(bytes) => {
            let s = std::str::from_utf8(bytes)
                .map_err(|err| pi_js::Error::msg(format!("Invalid UTF-8 from SQLite: {err}")))?;
            Ok(SqliteValue::Text(s.to_string()))
        }
        ValueRef::Blob(bytes) => Ok(SqliteValue::Blob(bytes.to_vec())),
    }
}

fn row_from_rusqlite(row: &rusqlite::Row<'_>, column_names: &[String]) -> pi_js::Result<SqliteRow> {
    let mut map = SqliteRow::with_capacity(column_names.len());
    for (idx, name) in column_names.iter().enumerate() {
        let val_ref = row
            .get_ref(idx)
            .map_err(|err| pi_js::Error::msg(err.to_string()))?;
        map.insert(name.clone(), from_rusqlite_value(val_ref)?);
    }
    Ok(map)
}

fn exec_on_conn(conn: &rusqlite::Connection, sql: &str) -> pi_js::Result<()> {
    conn.execute_batch(sql)
        .map_err(|err| pi_js::Error::msg(err.to_string()))
}

fn run_on_conn(conn: &rusqlite::Connection, sql: &str, params: &[SqliteValue]) -> pi_js::Result<()> {
    let mut stmt = conn
        .prepare_cached(sql)
        .map_err(|err| pi_js::Error::msg(err.to_string()))?;
    let bound: Vec<RusqliteValue> = params.iter().map(to_rusqlite_value).collect();
    stmt.execute(rusqlite::params_from_iter(bound))
        .map_err(|err| pi_js::Error::msg(err.to_string()))?;
    Ok(())
}

fn get_on_conn(
    conn: &rusqlite::Connection,
    sql: &str,
    params: &[SqliteValue],
) -> pi_js::Result<Option<SqliteRow>> {
    let mut stmt = conn
        .prepare_cached(sql)
        .map_err(|err| pi_js::Error::msg(err.to_string()))?;
    let column_names: Vec<String> = stmt
        .column_names()
        .into_iter()
        .map(ToString::to_string)
        .collect();
    let bound: Vec<RusqliteValue> = params.iter().map(to_rusqlite_value).collect();
    let mut rows = stmt
        .query(rusqlite::params_from_iter(bound))
        .map_err(|err| pi_js::Error::msg(err.to_string()))?;
    let Some(row) = rows
        .next()
        .map_err(|err| pi_js::Error::msg(err.to_string()))?
    else {
        return Ok(None);
    };
    Ok(Some(row_from_rusqlite(row, &column_names)?))
}

fn all_on_conn(
    conn: &rusqlite::Connection,
    sql: &str,
    params: &[SqliteValue],
) -> pi_js::Result<Vec<SqliteRow>> {
    let mut stmt = conn
        .prepare_cached(sql)
        .map_err(|err| pi_js::Error::msg(err.to_string()))?;
    let column_names: Vec<String> = stmt
        .column_names()
        .into_iter()
        .map(ToString::to_string)
        .collect();
    let bound: Vec<RusqliteValue> = params.iter().map(to_rusqlite_value).collect();
    let mut rows = stmt
        .query(rusqlite::params_from_iter(bound))
        .map_err(|err| pi_js::Error::msg(err.to_string()))?;
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|err| pi_js::Error::msg(err.to_string()))?
    {
        out.push(row_from_rusqlite(row, &column_names)?);
    }
    Ok(out)
}

struct NodeSqliteTransaction {
    conn_state: Arc<Mutex<ConnectionState>>,
    active: Arc<AtomicBool>,
}

impl NodeSqliteTransaction {
    fn with_conn<T>(&self, f: impl FnOnce(&rusqlite::Connection) -> pi_js::Result<T>) -> pi_js::Result<T> {
        if !self.active.load(Ordering::SeqCst) {
            return Err(pi_js::Error::msg(
                "SQLite transaction handle is no longer active",
            ));
        }
        let guard = self.conn_state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(ref conn) = guard.conn else {
            return Err(pi_js::Error::msg("SQLite database is closed"));
        };
        f(conn)
    }
}

#[async_trait]
impl SqliteExecutor for NodeSqliteTransaction {
    async fn exec(&self, sql: &str) -> pi_js::Result<()> {
        self.with_conn(|conn| exec_on_conn(conn, sql))
    }

    async fn run(&self, sql: &str, params: &[SqliteValue]) -> pi_js::Result<()> {
        self.with_conn(|conn| run_on_conn(conn, sql, params))
    }

    async fn get(&self, sql: &str, params: &[SqliteValue]) -> pi_js::Result<Option<SqliteRow>> {
        self.with_conn(|conn| get_on_conn(conn, sql, params))
    }

    async fn all(&self, sql: &str, params: &[SqliteValue]) -> pi_js::Result<Vec<SqliteRow>> {
        self.with_conn(|conn| all_on_conn(conn, sql, params))
    }
}

/// [`SqliteDatabase`] adapter backed by `rusqlite::Connection` with serialized FIFO access.
pub struct NodeSqliteDatabase {
    access: AsyncMutex<()>,
    conn_state: Arc<Mutex<ConnectionState>>,
}

impl NodeSqliteDatabase {
    pub fn new(mut conn: rusqlite::Connection) -> Self {
        conn.set_prepared_statement_cache_capacity(128);
        Self {
            access: AsyncMutex::new(()),
            conn_state: Arc::new(Mutex::new(ConnectionState {
                conn: Some(conn),
                closed: false,
            })),
        }
    }

    fn with_conn_sync<T>(
        &self,
        f: impl FnOnce(&rusqlite::Connection) -> pi_js::Result<T>,
    ) -> pi_js::Result<T> {
        let guard = self.conn_state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(ref conn) = guard.conn else {
            return Err(pi_js::Error::msg("SQLite database is closed"));
        };
        f(conn)
    }
}

#[async_trait]
impl SqliteExecutor for NodeSqliteDatabase {
    async fn exec(&self, sql: &str) -> pi_js::Result<()> {
        let _guard = self.access.lock().await;
        self.with_conn_sync(|conn| exec_on_conn(conn, sql))
    }

    async fn run(&self, sql: &str, params: &[SqliteValue]) -> pi_js::Result<()> {
        let _guard = self.access.lock().await;
        self.with_conn_sync(|conn| run_on_conn(conn, sql, params))
    }

    async fn get(&self, sql: &str, params: &[SqliteValue]) -> pi_js::Result<Option<SqliteRow>> {
        let _guard = self.access.lock().await;
        self.with_conn_sync(|conn| get_on_conn(conn, sql, params))
    }

    async fn all(&self, sql: &str, params: &[SqliteValue]) -> pi_js::Result<Vec<SqliteRow>> {
        let _guard = self.access.lock().await;
        self.with_conn_sync(|conn| all_on_conn(conn, sql, params))
    }
}

#[async_trait]
impl SqliteDatabase for NodeSqliteDatabase {
    async fn transaction_boxed<'a>(
        &'a self,
        callback: BoxedSqliteTxCallback<'a>,
    ) -> pi_js::Result<Box<dyn Any + Send>> {
        let _guard = self.access.lock().await;
        self.with_conn_sync(|conn| exec_on_conn(conn, "BEGIN IMMEDIATE"))?;

        let active = Arc::new(AtomicBool::new(true));
        let tx: Arc<dyn SqliteExecutor> = Arc::new(NodeSqliteTransaction {
            conn_state: Arc::clone(&self.conn_state),
            active: Arc::clone(&active),
        });

        let callback_result = callback(tx).await;
        active.store(false, Ordering::SeqCst);

        match callback_result {
            Ok(value) => {
                self.with_conn_sync(|conn| exec_on_conn(conn, "COMMIT"))?;
                Ok(value)
            }
            Err(err) => {
                if let Err(rollback_err) = self.with_conn_sync(|conn| exec_on_conn(conn, "ROLLBACK"))
                {
                    let msg = format!(
                        "SQLite transaction failed and rollback failed: {}; rollback: {}",
                        err.message(),
                        rollback_err.message()
                    );
                    return Err(pi_js::Error::js_with_cause("AggregateError", msg, err));
                }
                Err(err)
            }
        }
    }

    async fn close(&self) -> pi_js::Result<()> {
        let _guard = self.access.lock().await;
        let conn = {
            let mut guard = self.conn_state.lock().unwrap_or_else(|e| e.into_inner());
            if guard.closed {
                return Ok(());
            }
            guard.closed = true;
            guard.conn.take()
        };
        if let Some(conn) = conn {
            let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
            conn.close()
                .map_err(|(_, err)| pi_js::Error::msg(err.to_string()))?;
        }
        Ok(())
    }
}

/// Open and configure a local SQLite database facade.
pub async fn open_node_sqlite_database(
    path: &str,
    options: NodeSqliteStorageOptions,
) -> pi_js::Result<Arc<NodeSqliteDatabase>> {
    let checkpoint_pages = options
        .wal_auto_checkpoint_pages
        .unwrap_or(DEFAULT_WAL_AUTO_CHECKPOINT_PAGES);
    let timeout_ms = options.busy_timeout_ms.unwrap_or(DEFAULT_BUSY_TIMEOUT_MS);

    if path != ":memory:"
        && let Some(parent) = Path::new(path).parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|err| pi_js::Error::msg(err.to_string()))?;
    }

    let conn = if path == ":memory:" {
        rusqlite::Connection::open_in_memory()
    } else {
        rusqlite::Connection::open(path)
    }
    .map_err(|err| pi_js::Error::msg(err.to_string()))?;

    conn.busy_timeout(Duration::from_millis(timeout_ms))
        .map_err(|err| pi_js::Error::msg(err.to_string()))?;

    let adapter = Arc::new(NodeSqliteDatabase::new(conn));
    let pragma_result: pi_js::Result<()> = async {
        adapter.exec("PRAGMA journal_mode = WAL").await?;
        adapter.exec("PRAGMA synchronous = NORMAL").await?;
        adapter
            .exec(&format!("PRAGMA wal_autocheckpoint = {checkpoint_pages}"))
            .await?;
        Ok(())
    }
    .await;

    match pragma_result {
        Ok(()) => Ok(adapter),
        Err(err) => {
            let _ = adapter.close().await;
            Err(err)
        }
    }
}

/// Open or create file-backed durable storage using SQLite.
pub async fn open_node_sqlite_storage(
    path: &str,
    options: NodeSqliteStorageOptions,
) -> pi_js::Result<SqliteStorage> {
    let db = open_node_sqlite_database(path, options).await?;
    SqliteStorage::open(db).await
}
