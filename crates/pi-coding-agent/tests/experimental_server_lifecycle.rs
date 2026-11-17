//! Port of packages/coding-agent/test/experimental-server-lifecycle.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use pi_coding_agent::experimental::server::ServerLifetime;

#[tokio::test(start_paused = true)]
async fn holds_a_foreground_server_until_explicit_shutdown() {
    let retire_calls = Arc::new(AtomicUsize::new(0));
    let retire_cb = Arc::clone(&retire_calls);
    let lifetime = ServerLifetime::new(true);
    lifetime.start(Arc::new(move || {
        retire_cb.fetch_add(1, Ordering::AcqRel);
    }));

    tokio::time::advance(Duration::from_millis(60_000)).await;
    tokio::task::yield_now().await;
    assert_eq!(retire_calls.load(Ordering::Acquire), 0);
    lifetime.stop();
}

#[tokio::test(start_paused = true)]
async fn retires_an_automatic_server_if_no_first_client_arrives() {
    let retire_calls = Arc::new(AtomicUsize::new(0));
    let retire_cb = Arc::clone(&retire_calls);
    let lifetime = ServerLifetime::new(false);
    lifetime.start(Arc::new(move || {
        retire_cb.fetch_add(1, Ordering::AcqRel);
    }));

    tokio::time::advance(Duration::from_millis(10_999)).await;
    tokio::task::yield_now().await;
    assert_eq!(retire_calls.load(Ordering::Acquire), 0);

    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(retire_calls.load(Ordering::Acquire), 1);
    lifetime.stop();
}

#[tokio::test(start_paused = true)]
async fn requires_both_client_and_worker_demand_to_disappear() {
    let retire_calls = Arc::new(AtomicUsize::new(0));
    let retire_cb = Arc::clone(&retire_calls);
    let lifetime = ServerLifetime::new(false);
    lifetime.start(Arc::new(move || {
        retire_cb.fetch_add(1, Ordering::AcqRel);
    }));

    lifetime.set_connection_count(1);
    lifetime.set_worker_count(1);
    lifetime.set_connection_count(0);
    tokio::time::advance(Duration::from_millis(10_000)).await;
    tokio::task::yield_now().await;
    assert_eq!(retire_calls.load(Ordering::Acquire), 0);

    lifetime.set_worker_count(0);
    tokio::time::advance(Duration::from_millis(999)).await;
    tokio::task::yield_now().await;
    assert_eq!(retire_calls.load(Ordering::Acquire), 0);

    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(retire_calls.load(Ordering::Acquire), 1);
    lifetime.stop();
}
