//! Port of packages/durable/src/testing/runner.ts

use std::sync::Arc;

use crate::testing::assertions::create_expect_assertions;
use crate::testing::storage_conformance::create_storage_conformance;
use crate::testing::types::{StorageConformanceCase, StorageConformanceOptions, StorageConformanceProvider};

pub trait StorageConformanceRunner {
    fn run_suite(&self, name: &str, cases: Vec<StorageConformanceCase>);
}

/// Build the full conformance case list using standard assertions.
pub fn register_storage_conformance(with_storage: StorageConformanceProvider) -> Vec<StorageConformanceCase> {
    create_storage_conformance(StorageConformanceOptions {
        assertions: create_expect_assertions(),
        with_storage,
    })
}

/// Execute every storage conformance case sequentially for one storage backend.
pub async fn run_storage_conformance(_name: &str, with_storage: StorageConformanceProvider) {
    for case in register_storage_conformance(with_storage) {
        case.run().await;
    }
}

/// Execute a specific conformance case matching `case_name`.
pub async fn run_storage_conformance_case(case_name: &str, with_storage: StorageConformanceProvider) {
    let cases = register_storage_conformance(with_storage);
    let found = cases
        .into_iter()
        .find(|c| c.name == case_name)
        .unwrap_or_else(|| panic!("Unknown storage conformance case: {case_name}"));
    found.run().await;
}

pub fn make_storage_provider<F, Fut>(factory: F) -> StorageConformanceProvider
where
    F: Fn(crate::testing::types::StorageConformanceCallback) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    Arc::new(move |cb| Box::pin(factory(cb)))
}
