//! Port of packages/durable/src/storage/sqlite/database.ts

use std::any::Any;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;

use crate::ids::{Id, IdKind, Seq};

/// Values supported by the portable SQLite storage core.
#[derive(Clone, Debug, PartialEq)]
pub enum SqliteValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl From<i64> for SqliteValue {
    fn from(v: i64) -> Self {
        Self::Integer(v)
    }
}

impl From<i32> for SqliteValue {
    fn from(v: i32) -> Self {
        Self::Integer(i64::from(v))
    }
}

impl From<f64> for SqliteValue {
    fn from(v: f64) -> Self {
        Self::Real(v)
    }
}

impl From<bool> for SqliteValue {
    fn from(v: bool) -> Self {
        Self::Integer(if v { 1 } else { 0 })
    }
}

impl From<String> for SqliteValue {
    fn from(v: String) -> Self {
        Self::Text(v)
    }
}

impl From<&str> for SqliteValue {
    fn from(v: &str) -> Self {
        Self::Text(v.to_string())
    }
}

impl From<Vec<u8>> for SqliteValue {
    fn from(v: Vec<u8>) -> Self {
        Self::Blob(v)
    }
}

impl From<&[u8]> for SqliteValue {
    fn from(v: &[u8]) -> Self {
        Self::Blob(v.to_vec())
    }
}

impl<K: IdKind, T> From<Id<K, T>> for SqliteValue {
    fn from(id: Id<K, T>) -> Self {
        Self::Integer(id.as_i64())
    }
}

impl From<Seq> for SqliteValue {
    fn from(seq: Seq) -> Self {
        Self::Integer(seq.as_i64())
    }
}

impl<T: Into<SqliteValue>> From<Option<T>> for SqliteValue {
    fn from(opt: Option<T>) -> Self {
        match opt {
            Some(v) => v.into(),
            None => Self::Null,
        }
    }
}

/// Ordered column-name to value map representing one SQLite result row.
pub type SqliteRow = IndexMap<String, SqliteValue>;

/// Typed extraction helpers on [`SqliteRow`].
pub trait SqliteRowExt {
    fn get_i64(&self, column: &str) -> pi_js::Result<i64>;
    fn get_str(&self, column: &str) -> pi_js::Result<&str>;
    fn get_optional_str(&self, column: &str) -> pi_js::Result<Option<&str>>;
}

impl SqliteRowExt for SqliteRow {
    fn get_i64(&self, column: &str) -> pi_js::Result<i64> {
        match self.get(column) {
            Some(SqliteValue::Integer(v)) => Ok(*v),
            Some(other) => Err(pi_js::Error::msg(format!(
                "SQLite column '{column}' expected integer, got {other:?}"
            ))),
            None => Err(pi_js::Error::msg(format!(
                "SQLite row missing column '{column}'"
            ))),
        }
    }

    fn get_str(&self, column: &str) -> pi_js::Result<&str> {
        match self.get(column) {
            Some(SqliteValue::Text(s)) => Ok(s.as_str()),
            Some(other) => Err(pi_js::Error::msg(format!(
                "SQLite column '{column}' expected text, got {other:?}"
            ))),
            None => Err(pi_js::Error::msg(format!(
                "SQLite row missing column '{column}'"
            ))),
        }
    }

    fn get_optional_str(&self, column: &str) -> pi_js::Result<Option<&str>> {
        match self.get(column) {
            Some(SqliteValue::Null) | None => Ok(None),
            Some(SqliteValue::Text(s)) => Ok(Some(s.as_str())),
            Some(other) => Err(pi_js::Error::msg(format!(
                "SQLite column '{column}' expected nullable text, got {other:?}"
            ))),
        }
    }
}

pub type BoxedSqliteFuture<'a, T> = Pin<Box<dyn Future<Output = pi_js::Result<T>> + Send + 'a>>;

pub type BoxedSqliteTxCallback<'a> = Box<
    dyn FnOnce(Arc<dyn SqliteExecutor>) -> BoxedSqliteFuture<'a, Box<dyn Any + Send>> + Send + 'a,
>;

/// Asynchronous SQL operations shared by a database and its transaction handles.
#[async_trait]
pub trait SqliteExecutor: Send + Sync {
    async fn exec(&self, sql: &str) -> pi_js::Result<()>;
    async fn run(&self, sql: &str, params: &[SqliteValue]) -> pi_js::Result<()>;
    async fn get(&self, sql: &str, params: &[SqliteValue]) -> pi_js::Result<Option<SqliteRow>>;
    async fn all(&self, sql: &str, params: &[SqliteValue]) -> pi_js::Result<Vec<SqliteRow>>;
}

/// Minimal database facade required by `SqliteStorage`.
#[async_trait]
pub trait SqliteDatabase: SqliteExecutor {
    async fn transaction_boxed<'a>(
        &'a self,
        callback: BoxedSqliteTxCallback<'a>,
    ) -> pi_js::Result<Box<dyn Any + Send>>;

    async fn close(&self) -> pi_js::Result<()>;
}

/// Ergonomic typed `transaction` helper for any [`SqliteDatabase`].
#[async_trait]
pub trait SqliteDatabaseExt: SqliteDatabase {
    async fn transaction<'a, T, F, Fut>(&'a self, callback: F) -> pi_js::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<dyn SqliteExecutor>) -> Fut + Send + 'a,
        Fut: Future<Output = pi_js::Result<T>> + Send + 'a,
    {
        let boxed_cb: BoxedSqliteTxCallback<'a> = Box::new(move |tx| {
            Box::pin(async move {
                let value = callback(tx).await?;
                Ok(Box::new(value) as Box<dyn Any + Send>)
            })
        });
        let boxed = self.transaction_boxed(boxed_cb).await?;
        boxed.downcast::<T>().map(|b| *b).map_err(|_| {
            pi_js::Error::msg("SQLite transaction result downcast failed")
        })
    }
}

impl<D: SqliteDatabase + ?Sized> SqliteDatabaseExt for D {}
