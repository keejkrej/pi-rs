//! Port of packages/coding-agent/test/experimental-session-worker-manager.test.ts

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use indexmap::IndexSet;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::{JsonValue, ServiceCall};
use pi_coding_agent::experimental::coordinator::CoordinatorConnectionEvent;
use pi_coding_agent::experimental::session_catalog::SessionCatalogMetadata;
use pi_coding_agent::experimental::session_worker::{
    SessionWorkerEvent, WorkerOperationResponse, WorkerOperationScope,
};
use pi_coding_agent::experimental::session_worker_manager::{
    SessionWorkerManager, WorkerCoordinator, WorkerKillHook, set_worker_kill_hook,
};
use pi_js::Unsubscribe;
use serde_json::{Value, json};
use serial_test::serial;

fn test_metadata() -> SessionCatalogMetadata {
    SessionCatalogMetadata {
        id: "session-1".to_string(),
        created_at: 1,
        cwd: "/tmp".to_string(),
        path: "/tmp/session-1".to_string(),
    }
}

fn worker_ready_payload() -> Value {
    let metadata = test_metadata();
    serde_json::to_value(SessionWorkerEvent::WorkerReady {
        token: "worker-token".to_string(),
        session_key: metadata.path.clone(),
        session_id: metadata.id.clone(),
        pid: 123,
        metadata,
        plugin_manifest_paths: Vec::new(),
    })
    .expect("serialize worker_ready")
}

type OnSendCallback = Arc<dyn Fn(&FakeCoordinator, &str, &Value) + Send + Sync>;

#[derive(Clone)]
struct FakeCoordinator {
    inner: Arc<FakeCoordinatorInner>,
}

struct FakeCoordinatorInner {
    sent: Mutex<Vec<(String, Value)>>,
    listeners: Mutex<Vec<(u64, Arc<dyn Fn(CoordinatorConnectionEvent) + Send + Sync>)>>,
    next_id: AtomicU64,
    on_send: Mutex<Option<OnSendCallback>>,
}

impl FakeCoordinator {
    fn new() -> Self {
        Self {
            inner: Arc::new(FakeCoordinatorInner {
                sent: Mutex::new(Vec::new()),
                listeners: Mutex::new(Vec::new()),
                next_id: AtomicU64::new(1),
                on_send: Mutex::new(None),
            }),
        }
    }

    fn set_on_send<F>(&self, cb: F)
    where
        F: Fn(&FakeCoordinator, &str, &Value) + Send + Sync + 'static,
    {
        *self.inner.on_send.lock().expect("lock") = Some(Arc::new(cb));
    }

    fn clear_on_send(&self) {
        *self.inner.on_send.lock().expect("lock") = None;
    }

    fn sent(&self) -> Vec<(String, Value)> {
        self.inner.sent.lock().expect("lock").clone()
    }

    fn emit(&self, event: CoordinatorConnectionEvent) {
        let listeners: Vec<_> = self
            .inner
            .listeners
            .lock()
            .expect("lock")
            .iter()
            .map(|(_, cb)| Arc::clone(cb))
            .collect();
        for listener in listeners {
            listener(event.clone());
        }
    }
}

impl WorkerCoordinator for FakeCoordinator {
    fn control_path(&self) -> &str {
        "/tmp/control.sock"
    }

    fn server_connection_id(&self) -> &str {
        "server-generation-1"
    }

    fn was_replaced(&self) -> bool {
        false
    }

    fn on_event(&self, listener: Arc<dyn Fn(CoordinatorConnectionEvent) + Send + Sync>) -> Unsubscribe {
        let id = self.inner.next_id.fetch_add(1, Ordering::AcqRel);
        self.inner.listeners.lock().expect("lock").push((id, listener));
        let inner = Arc::clone(&self.inner);
        Unsubscribe::new(move || {
            inner
                .listeners
                .lock()
                .expect("lock")
                .retain(|(entry_id, _)| *entry_id != id);
        })
    }

    fn send<'a>(&'a self, peer_id: &'a str, payload: &'a Value) -> futures::future::BoxFuture<'a, pi_js::Result<()>> {
        self.inner
            .sent
            .lock()
            .expect("lock")
            .push((peer_id.to_string(), payload.clone()));
        let cb = self.inner.on_send.lock().expect("lock").clone();
        if let Some(cb) = cb {
            cb(self, peer_id, payload);
        }
        Box::pin(futures::future::ready(Ok(())))
    }

    fn broadcast<'a>(&'a self, payload: &'a Value) -> futures::future::BoxFuture<'a, pi_js::Result<()>> {
        if payload.get("type").and_then(Value::as_str) == Some("discover_workers") {
            self.emit(CoordinatorConnectionEvent::Message {
                from: "worker-1".to_string(),
                payload: worker_ready_payload(),
            });
        }
        Box::pin(futures::future::ready(Ok(())))
    }
}

struct KillHookGuard(Option<WorkerKillHook>);

impl KillHookGuard {
    fn capture() -> (Self, Arc<Mutex<Vec<u32>>>) {
        let killed = Arc::new(Mutex::new(Vec::new()));
        let killed_ref = Arc::clone(&killed);
        let prev = set_worker_kill_hook(Some(Arc::new(move |pid| {
            killed_ref.lock().expect("lock").push(pid);
        })));
        (Self(prev), killed)
    }
}

impl Drop for KillHookGuard {
    fn drop(&mut self) {
        set_worker_kill_hook(self.0.take());
    }
}

async fn yield_tasks() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

async fn advance_and_yield(ms: u64) {
    tokio::time::advance(Duration::from_millis(ms)).await;
    yield_tasks().await;
}

async fn create_attached_worker() -> (
    FakeCoordinator,
    SessionWorkerManager,
    Arc<dyn pi_server::RoutedSessionHandle>,
    Arc<dyn pi_server::RoutedSessionAttachment>,
) {
    let coordinator = FakeCoordinator::new();
    let workers = SessionWorkerManager::new(Arc::new(coordinator.clone()), "/tmp", None, None);
    let peers = IndexSet::from(["worker-1".to_string()]);
    workers.discover(&peers).await.expect("discover");
    let metadata = test_metadata();
    let handle = workers
        .open_session(&metadata, BACKGROUND_CONTEXT.clone(), &[])
        .await
        .expect("open session");

    coordinator.set_on_send(move |coord, peer_id, payload| {
        if payload.get("type").and_then(Value::as_str) != Some("session_demand") {
            return;
        }
        let coord = coord.clone();
        let peer_id = peer_id.to_string();
        let request_id = payload
            .get("requestId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let attachment_id = payload
            .get("attachmentId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let attached = payload.get("attached").and_then(Value::as_bool).unwrap_or(false);
        tokio::spawn(async move {
            let msg = serde_json::to_value(SessionWorkerEvent::DemandApplied {
                token: "worker-token".to_string(),
                session_key: test_metadata().path,
                request_id,
                attachment_id,
                attached,
            })
            .expect("serialize demand_applied");
            coord.emit(CoordinatorConnectionEvent::Message {
                from: peer_id,
                payload: msg,
            });
        });
    });

    let attachment = handle
        .attach_client(BACKGROUND_CONTEXT.clone())
        .await
        .expect("attach client");
    (coordinator, workers, handle, attachment)
}

#[tokio::test]
async fn adopts_a_discovered_worker_with_its_existing_session_plugin_selection() {
    let coordinator = FakeCoordinator::new();
    let workers = SessionWorkerManager::new(Arc::new(coordinator.clone()), "/tmp", None, None);
    let peers = IndexSet::from(["worker-1".to_string()]);
    workers.discover(&peers).await.expect("discover");

    assert!(
        !coordinator
            .sent()
            .iter()
            .any(|(peer, payload)| peer == "worker-1" && payload == &json!({ "type": "shutdown" }))
    );
    assert_eq!(workers.worker_pids().len(), 1);
    let metadata = test_metadata();
    workers
        .assert_session_plugin_manifest_paths(&metadata, &[])
        .expect("plugin manifests match");
    workers.detach();
}

#[tokio::test]
async fn rejects_a_different_plugin_selection_for_an_active_session_without_stopping_it() {
    let (_coordinator, workers, _handle, attachment) = create_attached_worker().await;
    let metadata = test_metadata();
    let err = workers
        .assert_session_plugin_manifest_paths(&metadata, &["/tmp/plugin/chord-facets.json".to_string()])
        .expect_err("must reject conflicting selection");
    assert!(
        err.message().contains("active with a different plugin selection"),
        "unexpected error: {}",
        err.message()
    );
    assert_eq!(workers.worker_pids().len(), 1);
    workers.detach();
    let _ = attachment.release(BACKGROUND_CONTEXT.clone()).await;
}

#[tokio::test(start_paused = true)]
async fn compensates_a_timed_out_attachment_before_rejecting_it() {
    let coordinator = FakeCoordinator::new();
    let workers = SessionWorkerManager::new(Arc::new(coordinator.clone()), "/tmp", None, None);
    let peers = IndexSet::from(["worker-1".to_string()]);
    workers.discover(&peers).await.expect("discover");
    let handle = workers
        .open_session(&test_metadata(), BACKGROUND_CONTEXT.clone(), &[])
        .await
        .expect("open session");

    let demands = Arc::new(Mutex::new(Vec::<(String, bool)>::new()));
    let demands_ref = Arc::clone(&demands);
    coordinator.set_on_send(move |coord, peer_id, payload| {
        if payload.get("type").and_then(Value::as_str) != Some("session_demand") {
            return;
        }
        let Some(attachment_id) = payload
            .get("attachmentId")
            .and_then(Value::as_str)
            .map(ToString::to_string)
        else {
            return;
        };
        let Some(attached) = payload.get("attached").and_then(Value::as_bool) else {
            return;
        };
        demands_ref
            .lock()
            .expect("lock")
            .push((attachment_id.clone(), attached));
        if attached {
            return;
        }
        let coord = coord.clone();
        let peer_id = peer_id.to_string();
        let request_id = payload
            .get("requestId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        tokio::spawn(async move {
            let msg = serde_json::to_value(SessionWorkerEvent::DemandApplied {
                token: "worker-token".to_string(),
                session_key: test_metadata().path,
                request_id,
                attachment_id,
                attached: false,
            })
            .expect("serialize demand_applied");
            coord.emit(CoordinatorConnectionEvent::Message {
                from: peer_id,
                payload: msg,
            });
        });
    });

    let attaching = tokio::spawn({
        let handle = Arc::clone(&handle);
        async move { handle.attach_client(BACKGROUND_CONTEXT.clone()).await }
    });
    yield_tasks().await;
    advance_and_yield(5_000).await;

    let err = attaching.await.expect("join").expect_err("attach must time out");
    assert!(
        err.message().contains("timed out"),
        "unexpected error: {}",
        err.message()
    );
    let recorded = demands.lock().expect("lock").clone();
    assert_eq!(recorded.len(), 2);
    assert!(recorded[0].1);
    assert_eq!(recorded[1], (recorded[0].0.clone(), false));
    workers.detach();
}

#[tokio::test(start_paused = true)]
#[serial]
async fn kills_a_worker_when_timed_out_demand_cannot_be_reconciled() {
    let (_guard, killed) = KillHookGuard::capture();
    let coordinator = FakeCoordinator::new();
    let workers = SessionWorkerManager::new(Arc::new(coordinator.clone()), "/tmp", None, None);
    let peers = IndexSet::from(["worker-1".to_string()]);
    workers.discover(&peers).await.expect("discover");
    let handle = workers
        .open_session(&test_metadata(), BACKGROUND_CONTEXT.clone(), &[])
        .await
        .expect("open session");
    coordinator.clear_on_send();

    let attaching = tokio::spawn({
        let handle = Arc::clone(&handle);
        async move { handle.attach_client(BACKGROUND_CONTEXT.clone()).await }
    });
    yield_tasks().await;
    advance_and_yield(5_000).await;
    advance_and_yield(5_000).await;
    advance_and_yield(10_000).await;

    let err = attaching.await.expect("join").expect_err("attach must fail");
    assert!(
        err.message().contains("worker was terminated"),
        "unexpected error: {}",
        err.message()
    );
    assert_eq!(*killed.lock().expect("lock"), vec![123]);
    assert!(workers.worker_pids().is_empty());
    workers.detach();
}

#[tokio::test(start_paused = true)]
#[serial]
async fn bounds_harness_driven_worker_shutdown() {
    let (_guard, killed) = KillHookGuard::capture();
    let (coordinator, workers, handle, attachment) = create_attached_worker().await;
    attachment.release(BACKGROUND_CONTEXT.clone()).await.expect("release");
    coordinator.clear_on_send();

    let closing = tokio::spawn({
        let handle = Arc::clone(&handle);
        async move { handle.close(BACKGROUND_CONTEXT.clone()).await }
    });
    yield_tasks().await;
    advance_and_yield(10_000).await;

    closing.await.expect("join").expect("close succeeds");
    assert_eq!(*killed.lock().expect("lock"), vec![123]);
    assert!(workers.worker_pids().is_empty());
    workers.detach();
}

#[tokio::test]
async fn correlates_service_results_to_the_worker_generation_and_attachment() {
    let (coordinator, workers, _handle, attachment) = create_attached_worker().await;
    let service_call = ServiceCall {
        service_id: "test.session".to_string(),
        instance: None,
        member: "run".to_string(),
        args: vec![JsonValue::String("Hello".to_string())],
    };

    coordinator.set_on_send(move |coord, peer_id, payload| {
        if payload.get("type").and_then(Value::as_str) != Some("operation") {
            return;
        }
        let coord = coord.clone();
        let peer_id = peer_id.to_string();
        let request_id = payload
            .get("requestId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let scope: WorkerOperationScope =
            serde_json::from_value(payload.get("scope").cloned().unwrap_or(Value::Null)).expect("valid scope");
        tokio::spawn(async move {
            let msg = serde_json::to_value(SessionWorkerEvent::OperationResponse {
                token: "worker-token".to_string(),
                session_key: test_metadata().path,
                response: WorkerOperationResponse::OperationResult {
                    request_id,
                    scope,
                    result: Some(json!({ "accepted": true })),
                },
            })
            .expect("serialize response");
            coord.emit(CoordinatorConnectionEvent::Message {
                from: peer_id,
                payload: msg,
            });
        });
    });

    let res = attachment
        .invoke_service(
            &service_call,
            Arc::new(|_, _, _| Box::pin(futures::future::ready(Ok(())))),
            BACKGROUND_CONTEXT.clone(),
        )
        .await
        .expect("invoke service");
    assert_eq!(res, Some(json!({ "accepted": true })));

    let sent = coordinator.sent();
    let operation = sent
        .iter()
        .map(|(_, p)| p)
        .find(|p| p.get("type").and_then(Value::as_str) == Some("operation"))
        .expect("operation sent");
    assert_eq!(
        operation
            .get("scope")
            .and_then(|s| s.get("serverConnectionId"))
            .and_then(Value::as_str),
        Some("server-generation-1")
    );
    assert!(
        operation
            .get("scope")
            .and_then(|s| s.get("attachmentId"))
            .and_then(Value::as_str)
            .is_some()
    );
    assert_eq!(
        operation.get("call"),
        Some(&serde_json::to_value(&service_call).expect("serialize call"))
    );

    workers.detach();
    let _ = attachment.release(BACKGROUND_CONTEXT.clone()).await;
}

#[tokio::test]
async fn rejects_a_correlated_response_with_mismatched_worker_identity() {
    let (coordinator, workers, _handle, attachment) = create_attached_worker().await;
    let service_call = ServiceCall {
        service_id: "test.session".to_string(),
        instance: None,
        member: "run".to_string(),
        args: vec![JsonValue::String("Hello".to_string())],
    };

    coordinator.set_on_send(move |coord, peer_id, payload| {
        if payload.get("type").and_then(Value::as_str) != Some("operation") {
            return;
        }
        let coord = coord.clone();
        let peer_id = peer_id.to_string();
        let request_id = payload
            .get("requestId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let scope: WorkerOperationScope =
            serde_json::from_value(payload.get("scope").cloned().unwrap_or(Value::Null)).expect("valid scope");
        tokio::spawn(async move {
            let msg = serde_json::to_value(SessionWorkerEvent::OperationResponse {
                token: "wrong-token".to_string(),
                session_key: test_metadata().path,
                response: WorkerOperationResponse::OperationResult {
                    request_id,
                    scope,
                    result: Some(json!({ "accepted": true })),
                },
            })
            .expect("serialize response");
            coord.emit(CoordinatorConnectionEvent::Message {
                from: peer_id,
                payload: msg,
            });
        });
    });

    let err = attachment
        .invoke_service(
            &service_call,
            Arc::new(|_, _, _| Box::pin(futures::future::ready(Ok(())))),
            BACKGROUND_CONTEXT.clone(),
        )
        .await
        .expect_err("must reject mismatched identity");
    assert!(
        err.message().contains("mismatched operation response"),
        "unexpected error: {}",
        err.message()
    );

    workers.detach();
    let _ = attachment.release(BACKGROUND_CONTEXT.clone()).await;
}

#[tokio::test]
async fn rejects_a_null_request_scope() {
    let (coordinator, workers, _handle, attachment) = create_attached_worker().await;
    let service_call = ServiceCall {
        service_id: "test.session".to_string(),
        instance: None,
        member: "run".to_string(),
        args: vec![JsonValue::String("Hello".to_string())],
    };

    coordinator.set_on_send(move |coord, peer_id, payload| {
        if payload.get("type").and_then(Value::as_str) != Some("operation") {
            return;
        }
        let coord = coord.clone();
        let peer_id = peer_id.to_string();
        let request_id = payload
            .get("requestId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        tokio::spawn(async move {
            coord.emit(CoordinatorConnectionEvent::Message {
                from: peer_id,
                payload: json!({
                    "type": "operation_response",
                    "token": "worker-token",
                    "sessionKey": test_metadata().path,
                    "response": {
                        "type": "operation_result",
                        "requestId": request_id,
                        "scope": null,
                        "result": { "accepted": true },
                    }
                }),
            });
        });
    });

    let err = attachment
        .invoke_service(
            &service_call,
            Arc::new(|_, _, _| Box::pin(futures::future::ready(Ok(())))),
            BACKGROUND_CONTEXT.clone(),
        )
        .await
        .expect_err("must reject null scope");
    assert!(
        err.message().contains("invalid operation response"),
        "unexpected error: {}",
        err.message()
    );

    workers.detach();
    let _ = attachment.release(BACKGROUND_CONTEXT.clone()).await;
}

#[tokio::test]
async fn rejects_pending_service_calls_on_replacement_without_stopping_the_worker() {
    let (coordinator, workers, _handle, attachment) = create_attached_worker().await;
    coordinator.clear_on_send();
    let service_call = ServiceCall {
        service_id: "test.session".to_string(),
        instance: None,
        member: "run".to_string(),
        args: vec![JsonValue::String("Hello".to_string())],
    };

    let calling = tokio::spawn({
        let attachment = Arc::clone(&attachment);
        async move {
            attachment
                .invoke_service(
                    &service_call,
                    Arc::new(|_, _, _| Box::pin(futures::future::ready(Ok(())))),
                    BACKGROUND_CONTEXT.clone(),
                )
                .await
        }
    });
    yield_tasks().await;
    workers.detach();

    let err = calling.await.expect("join").expect_err("must reject pending operation");
    assert!(
        err.message().contains("replaced during a worker operation"),
        "unexpected error: {}",
        err.message()
    );
    assert!(
        !coordinator
            .sent()
            .iter()
            .any(|(_, p)| p.get("type").and_then(Value::as_str) == Some("shutdown"))
    );
    assert!(workers.worker_pids().is_empty());
}
