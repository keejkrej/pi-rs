//! Port of packages/server/test/listener.test.ts

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_js::{Error, Result};
use pi_server::ServerListener;
use pi_server::connection::ByteConnectionAcceptor;
use pi_server::testing::{TestServerOptions, create_test_server};

struct TestListener {
    accept: Mutex<Option<ByteConnectionAcceptor>>,
    start_count: AtomicUsize,
    close_count: AtomicUsize,
    start_error: Mutex<Option<Error>>,
}

impl TestListener {
    fn new(start_error: Option<Error>) -> Arc<Self> {
        Arc::new(Self {
            accept: Mutex::new(None),
            start_count: AtomicUsize::new(0),
            close_count: AtomicUsize::new(0),
            start_error: Mutex::new(start_error),
        })
    }
}

#[async_trait]
impl ServerListener for TestListener {
    async fn start(&self, accept: ByteConnectionAcceptor) -> Result<()> {
        self.start_count.fetch_add(1, Ordering::SeqCst);
        *self.accept.lock().expect("accept mutex poisoned") = Some(accept);
        if let Some(err) = self.start_error.lock().expect("start_error mutex poisoned").take() {
            return Err(err);
        }
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        self.close_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn starts_and_closes_every_configured_listener() {
    let first = TestListener::new(None);
    let second = TestListener::new(None);
    let test_server = create_test_server(TestServerOptions {
        listeners: vec![first.clone(), second.clone()],
        ..Default::default()
    })
    .expect("create server");

    test_server.server.start().await.expect("start server");
    assert!(first.accept.lock().expect("accept mutex").is_some());
    assert!(second.accept.lock().expect("accept mutex").is_some());

    test_server.server.close().await.expect("close server");
    assert_eq!(first.close_count.load(Ordering::SeqCst), 1);
    assert_eq!(second.close_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn closes_previously_started_listeners_when_startup_fails() {
    let first = TestListener::new(None);
    let second = TestListener::new(Some(Error::msg("listener failed")));
    let test_server = create_test_server(TestServerOptions {
        listeners: vec![first.clone(), second.clone()],
        ..Default::default()
    })
    .expect("create server");

    let err = test_server.server.start().await.expect_err("expected startup failure");
    assert_eq!(err.message(), "listener failed");
    assert_eq!(first.close_count.load(Ordering::SeqCst), 1);
    assert_eq!(second.close_count.load(Ordering::SeqCst), 0);
}
