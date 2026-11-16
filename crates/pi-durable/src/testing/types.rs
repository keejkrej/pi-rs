//! Port of packages/durable/src/testing/types.ts

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value as JsonValue;

use crate::types::Storage;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Test-runner assertion surface used by portable storage conformance cases.
pub trait StorageConformanceAssertions: Send + Sync {
    fn ok(&self, value: bool, message: &str);
    fn strict_equal(&self, actual: &JsonValue, expected: &JsonValue);
    fn deep_equal(&self, actual: &JsonValue, expected: &JsonValue);
    fn partial_deep_equal(&self, actual: &JsonValue, expected: &JsonValue);
    fn greater_than(&self, actual: i64, expected: i64);
    fn rejects_containing(&self, result: Result<(), pi_js::Error>, message_includes: &str);
    fn rejects_error_name(&self, result: Result<(), pi_js::Error>, expected_name: &str);
}

pub type StorageConformanceCallback = Box<dyn FnOnce(Arc<dyn Storage>) -> BoxFuture<'static, ()> + Send>;

pub type StorageConformanceProvider = Arc<dyn Fn(StorageConformanceCallback) -> BoxFuture<'static, ()> + Send + Sync>;

pub struct StorageConformanceOptions {
    pub assertions: Arc<dyn StorageConformanceAssertions>,
    pub with_storage: StorageConformanceProvider,
}

type ConformanceTestFn =
    Arc<dyn Fn(Arc<dyn Storage>, Arc<dyn StorageConformanceAssertions>) -> BoxFuture<'static, ()> + Send + Sync>;

pub struct StorageConformanceCase {
    pub name: &'static str,
    with_storage: StorageConformanceProvider,
    assertions: Arc<dyn StorageConformanceAssertions>,
    test: ConformanceTestFn,
}

impl StorageConformanceCase {
    pub fn new(
        name: &'static str,
        with_storage: StorageConformanceProvider,
        assertions: Arc<dyn StorageConformanceAssertions>,
        test: ConformanceTestFn,
    ) -> Self {
        Self {
            name,
            with_storage,
            assertions,
            test,
        }
    }

    pub async fn run(&self) {
        let test = Arc::clone(&self.test);
        let assertions = Arc::clone(&self.assertions);
        let callback: StorageConformanceCallback = Box::new(move |storage| test(storage, assertions));
        (self.with_storage)(callback).await;
    }
}
