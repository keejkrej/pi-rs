//! Port of packages/coding-agent/test/file-mutation-queue.test.ts#L1-100 (unit coding-agent-tools-primitives)

// @generated-mods begin (scaffold-owned, do not edit)
#[path = "file_mutation_queue/part_2.rs"]
mod part_2;
// @generated-mods end

use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_coding_agent::core::tools::with_file_mutation_queue;
use tempfile::TempDir;

#[tokio::test]
async fn serializes_operations_for_same_file() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let path = "/tmp/pi-rs-file-mutation-queue-same";

    let o1 = Arc::clone(&order);
    let first = with_file_mutation_queue(path, move || async move {
        o1.lock().unwrap().push("first:start");
        tokio::time::sleep(Duration::from_millis(30)).await;
        o1.lock().unwrap().push("first:end");
    });

    let o2 = Arc::clone(&order);
    let second = with_file_mutation_queue(path, move || async move {
        o2.lock().unwrap().push("second:start");
        o2.lock().unwrap().push("second:end");
    });

    tokio::join!(first, second);
    assert_eq!(
        *order.lock().unwrap(),
        vec!["first:start", "first:end", "second:start", "second:end"]
    );
}

#[tokio::test]
async fn allows_different_files_to_proceed_in_parallel() {
    let order = Arc::new(Mutex::new(Vec::new()));

    let o1 = Arc::clone(&order);
    let a = with_file_mutation_queue("/tmp/pi-rs-file-mutation-queue-a", move || async move {
        o1.lock().unwrap().push("a:start");
        tokio::time::sleep(Duration::from_millis(30)).await;
        o1.lock().unwrap().push("a:end");
    });

    let o2 = Arc::clone(&order);
    let b = with_file_mutation_queue("/tmp/pi-rs-file-mutation-queue-b", move || async move {
        o2.lock().unwrap().push("b:start");
        tokio::time::sleep(Duration::from_millis(30)).await;
        o2.lock().unwrap().push("b:end");
    });

    tokio::join!(a, b);
    let recorded = order.lock().unwrap();
    let idx = |label: &str| recorded.iter().position(|x| *x == label).unwrap();
    assert!(idx("a:start") < idx("a:end"));
    assert!(idx("b:start") < idx("b:end"));
    assert!(idx("b:start") < idx("a:end"));
}

#[cfg(unix)]
#[tokio::test]
async fn uses_same_queue_for_symlink_aliases() {
    let dir = TempDir::new().unwrap();
    let target_path = dir.path().join("target.txt");
    let alias_path = dir.path().join("alias.txt");
    std::fs::write(&target_path, "hello\n").unwrap();
    std::os::unix::fs::symlink(&target_path, &alias_path).unwrap();

    let order = Arc::new(Mutex::new(Vec::new()));
    let t_str = target_path.to_string_lossy().into_owned();
    let a_str = alias_path.to_string_lossy().into_owned();

    let o1 = Arc::clone(&order);
    let f1 = with_file_mutation_queue(&t_str, move || async move {
        o1.lock().unwrap().push("target:start");
        tokio::time::sleep(Duration::from_millis(30)).await;
        o1.lock().unwrap().push("target:end");
    });

    let o2 = Arc::clone(&order);
    let f2 = with_file_mutation_queue(&a_str, move || async move {
        o2.lock().unwrap().push("alias:start");
        o2.lock().unwrap().push("alias:end");
    });

    tokio::join!(f1, f2);
    assert_eq!(
        *order.lock().unwrap(),
        vec!["target:start", "target:end", "alias:start", "alias:end"]
    );
}
