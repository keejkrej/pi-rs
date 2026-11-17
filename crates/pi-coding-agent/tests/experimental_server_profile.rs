//! Port of packages/coding-agent/test/experimental-server-profile.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pi_coding_agent::experimental::server::acquire_server_profile;
use pi_js::fs::promises::{read_to_string, readdir, write_file};
use pi_js::path::join;
use tempfile::tempdir;

const FIRST_SERVER_ID: &str = "00000000-0000-4000-8000-000000000001";
const SECOND_SERVER_ID: &str = "00000000-0000-4000-8000-000000000002";

#[tokio::test]
async fn serializes_launchers_and_preserves_the_server_identity() {
    let dir = tempdir().expect("create temp dir");
    let directory = dir.path().to_string_lossy().into_owned();
    let mut first = acquire_server_profile(&directory, None).await.expect("acquire first");

    let second_acquired = Arc::new(AtomicBool::new(false));
    let second_flag = Arc::clone(&second_acquired);
    let dir_clone = directory.clone();
    let pending_second = tokio::spawn(async move {
        let second = acquire_server_profile(&dir_clone, None).await.expect("acquire second");
        second_flag.store(true, Ordering::Release);
        second
    });

    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(!second_acquired.load(Ordering::Acquire));

    first.release().await.expect("release first");
    let mut second = pending_second.await.expect("join second");
    assert!(second_acquired.load(Ordering::Acquire));
    assert_eq!(second.server_id, first.server_id);
    let persisted = read_to_string(&join(&[&directory, "default-server-id"]))
        .await
        .expect("read default id");
    assert_eq!(persisted.trim(), first.server_id);
    second.release().await.expect("release second");
}

#[tokio::test]
async fn does_not_serialize_different_server_ids_in_one_directory() {
    let dir = tempdir().expect("create temp dir");
    let directory = dir.path().to_string_lossy().into_owned();
    let (mut first, mut second) = tokio::try_join!(
        acquire_server_profile(&directory, Some(FIRST_SERVER_ID)),
        acquire_server_profile(&directory, Some(SECOND_SERVER_ID)),
    )
    .expect("acquire both");

    assert_eq!(first.server_id, FIRST_SERVER_ID);
    assert_eq!(second.server_id, SECOND_SERVER_ID);
    let mut entries = readdir(&directory).await.expect("readdir");
    entries.sort();
    assert_eq!(
        entries,
        vec![
            format!("launcher-{FIRST_SERVER_ID}.lock"),
            format!("launcher-{SECOND_SERVER_ID}.lock"),
        ]
    );
    first.release().await.expect("release first");
    second.release().await.expect("release second");
}

#[tokio::test]
async fn does_not_share_the_default_identity_across_server_directories() {
    let first_dir = tempdir().expect("first dir");
    let second_dir = tempdir().expect("second dir");
    let d1 = first_dir.path().to_string_lossy().into_owned();
    let d2 = second_dir.path().to_string_lossy().into_owned();

    let (mut first, mut second) =
        tokio::try_join!(acquire_server_profile(&d1, None), acquire_server_profile(&d2, None),).expect("acquire");
    assert_ne!(first.server_id, second.server_id);
    first.release().await.expect("release first");
    second.release().await.expect("release second");
}

#[tokio::test]
async fn rejects_a_corrupt_default_identity() {
    let dir = tempdir().expect("temp dir");
    let directory = dir.path().to_string_lossy().into_owned();
    write_file(&join(&[&directory, "default-server-id"]), b"invalid\n", None)
        .await
        .expect("write invalid");

    let err = acquire_server_profile(&directory, None).await.err().expect("must fail");
    assert!(
        err.message().contains("Invalid default experimental server identity"),
        "unexpected error: {}",
        err.message()
    );
}

#[tokio::test]
async fn rejects_an_invalid_explicit_server_id() {
    let dir = tempdir().expect("temp dir");
    let directory = dir.path().to_string_lossy().into_owned();
    let err = acquire_server_profile(&directory, Some("invalid"))
        .await
        .err()
        .expect("must fail");
    assert!(
        err.message().contains("Invalid experimental server ID"),
        "unexpected error: {}",
        err.message()
    );
}
