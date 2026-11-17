//! Port of packages/coding-agent/test/experimental-session-worker-lifecycle.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use pi_coding_agent::experimental::session_worker::{WorkerLifecycle, WorkerLifecycleOptions};

const GENERATION: &str = "generation-1";

fn create_lifecycle(initial_demand_grace_ms: u64, orphan_demand_grace_ms: u64) -> (WorkerLifecycle, Arc<AtomicUsize>) {
    let retire_calls = Arc::new(AtomicUsize::new(0));
    let retire_cb = Arc::clone(&retire_calls);
    let lifecycle = WorkerLifecycle::new(WorkerLifecycleOptions {
        initial_server_connection_id: Some(GENERATION.to_string()),
        initial_demand_grace_ms,
        orphan_demand_grace_ms,
        on_retire: Arc::new(move || {
            retire_cb.fetch_add(1, Ordering::AcqRel);
        }),
    });
    (lifecycle, retire_calls)
}

#[tokio::test(start_paused = true)]
async fn retires_only_after_client_demand_and_harness_activity_are_both_gone() {
    let (lifecycle, retire) = create_lifecycle(100, 200);
    lifecycle
        .set_demand(GENERATION, "attachment-1", true)
        .expect("set demand");
    lifecycle.set_harness_active(true);
    lifecycle
        .set_demand(GENERATION, "attachment-1", false)
        .expect("clear demand");
    assert_eq!(retire.load(Ordering::Acquire), 0);

    lifecycle.set_harness_active(false);
    tokio::task::yield_now().await;
    assert_eq!(retire.load(Ordering::Acquire), 1);
    lifecycle.close();
}

#[tokio::test(start_paused = true)]
async fn retains_the_worker_until_every_presentation_attachment_is_released() {
    let (lifecycle, retire) = create_lifecycle(100, 200);
    lifecycle.set_demand(GENERATION, "attachment-1", true).expect("set 1");
    lifecycle.set_demand(GENERATION, "attachment-2", true).expect("set 2");
    lifecycle
        .set_demand(GENERATION, "attachment-1", false)
        .expect("clear 1");
    assert_eq!(retire.load(Ordering::Acquire), 0);
    lifecycle
        .set_demand(GENERATION, "attachment-2", false)
        .expect("clear 2");
    assert_eq!(retire.load(Ordering::Acquire), 1);
    lifecycle.close();
}

#[tokio::test(start_paused = true)]
async fn does_not_retire_on_repeated_activity_updates_while_the_harness_stays_active() {
    let (lifecycle, retire) = create_lifecycle(100, 200);
    lifecycle
        .set_demand(GENERATION, "attachment-1", true)
        .expect("set demand");
    lifecycle.set_harness_active(true);
    lifecycle
        .set_demand(GENERATION, "attachment-1", false)
        .expect("clear demand");
    lifecycle.set_harness_active(true);
    assert_eq!(retire.load(Ordering::Acquire), 0);
    lifecycle.set_harness_active(false);
    assert_eq!(retire.load(Ordering::Acquire), 1);
    lifecycle.close();
}

#[tokio::test(start_paused = true)]
async fn an_idle_harness_update_does_not_retire_while_demand_remains() {
    let (lifecycle, retire) = create_lifecycle(100, 200);
    lifecycle
        .set_demand(GENERATION, "attachment-1", true)
        .expect("set demand");
    lifecycle.set_harness_active(false);
    assert_eq!(retire.load(Ordering::Acquire), 0);
    lifecycle.close();
}

#[tokio::test(start_paused = true)]
async fn does_not_retire_while_a_demand_acknowledgement_holds_reconciliation() {
    let (lifecycle, retire) = create_lifecycle(100, 200);
    lifecycle
        .set_demand(GENERATION, "attachment-1", true)
        .expect("set demand");
    let hold = lifecycle.hold_retirement();
    lifecycle
        .set_demand(GENERATION, "attachment-1", false)
        .expect("clear demand");
    assert_eq!(retire.load(Ordering::Acquire), 0);
    hold.release();
    assert_eq!(retire.load(Ordering::Acquire), 1);
    lifecycle.close();
}

#[tokio::test(start_paused = true)]
async fn holds_retirement_only_for_requests_from_the_active_attachment() {
    let (lifecycle, retire) = create_lifecycle(100, 200);
    lifecycle
        .set_demand(GENERATION, "attachment-1", true)
        .expect("set demand");
    let hold = lifecycle
        .begin_request(GENERATION, "attachment-1")
        .expect("begin request");
    lifecycle
        .set_demand(GENERATION, "attachment-1", false)
        .expect("clear demand");
    assert_eq!(retire.load(Ordering::Acquire), 0);
    hold.release();
    assert_eq!(retire.load(Ordering::Acquire), 1);
    let err = lifecycle
        .begin_request(GENERATION, "attachment-1")
        .err()
        .expect("must fail after retiring");
    assert!(err.message().contains("retiring"));
    lifecycle.close();
}

#[tokio::test(start_paused = true)]
async fn rejects_requests_from_stale_generations_and_attachments() {
    let (lifecycle, _retire) = create_lifecycle(100, 200);
    lifecycle
        .set_demand(GENERATION, "attachment-1", true)
        .expect("set demand");
    let stale_err = lifecycle
        .begin_request("stale", "attachment-1")
        .err()
        .expect("stale err");
    assert!(stale_err.message().contains("stale server generation"));
    let att_err = lifecycle
        .begin_request(GENERATION, "wrong-attachment")
        .err()
        .expect("att err");
    assert!(att_err.message().contains("active attachment"));
    lifecycle.close();
}

#[tokio::test(start_paused = true)]
async fn retains_disconnected_generation_demand_for_the_orphan_grace() {
    let (lifecycle, retire) = create_lifecycle(100, 200);
    lifecycle
        .set_demand(GENERATION, "attachment-1", true)
        .expect("set demand");
    lifecycle.server_disconnected(GENERATION);

    tokio::time::advance(Duration::from_millis(199)).await;
    tokio::task::yield_now().await;
    assert_eq!(retire.load(Ordering::Acquire), 0);

    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(retire.load(Ordering::Acquire), 1);
    lifecycle.close();
}

#[tokio::test(start_paused = true)]
async fn allows_a_replacement_generation_to_retain_the_worker() {
    let (lifecycle, retire) = create_lifecycle(100, 200);
    lifecycle
        .set_demand(GENERATION, "attachment-1", true)
        .expect("set demand");
    lifecycle.server_disconnected(GENERATION);
    lifecycle.server_connected("generation-2");
    lifecycle
        .set_demand("generation-2", "attachment-2", true)
        .expect("set gen2 demand");

    tokio::time::advance(Duration::from_millis(200)).await;
    tokio::task::yield_now().await;
    assert_eq!(retire.load(Ordering::Acquire), 0);

    lifecycle
        .set_demand("generation-2", "attachment-2", false)
        .expect("clear gen2 demand");
    tokio::task::yield_now().await;
    assert_eq!(retire.load(Ordering::Acquire), 1);
    lifecycle.close();
}

#[tokio::test(start_paused = true)]
async fn retires_a_launched_worker_that_never_receives_initial_demand() {
    let (lifecycle, retire) = create_lifecycle(100, 200);
    tokio::time::advance(Duration::from_millis(100)).await;
    tokio::task::yield_now().await;
    assert_eq!(retire.load(Ordering::Acquire), 1);
    lifecycle.close();
}

#[tokio::test(start_paused = true)]
async fn rejects_demand_after_retirement_has_won_the_race() {
    let (lifecycle, _retire) = create_lifecycle(100, 200);
    tokio::time::advance(Duration::from_millis(100)).await;
    tokio::task::yield_now().await;
    let err = lifecycle
        .set_demand(GENERATION, "attachment-1", true)
        .err()
        .expect("should fail after retire");
    assert!(err.message().contains("retiring"));
    lifecycle.close();
}

#[tokio::test(start_paused = true)]
async fn rejects_demand_from_a_stale_server_generation() {
    let (lifecycle, _retire) = create_lifecycle(100, 200);
    let err = lifecycle
        .set_demand("stale", "attachment-1", true)
        .err()
        .expect("stale gen error");
    assert!(err.message().contains("stale server generation"));
    lifecycle.close();
}
