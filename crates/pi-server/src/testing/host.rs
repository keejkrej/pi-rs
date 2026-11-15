//! Port of packages/server/src/testing/host.ts

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_chord::{Context, JsonValue, ServiceCall, ServiceUpdatePublisher};
use pi_js::{BoxFuture, Error, Result};
use serde_json::json;
use tokio::sync::{Notify, watch};

use crate::errors::SessionNotFoundError;
use crate::types::{
    DefaultSessionMetadata, RoutedServerPresentation, RoutedServerServiceAttachment, RoutedServerServiceHost,
    RoutedSessionAttachment, RoutedSessionHandle, ServerHost,
};

pub struct Deferred<T> {
    inner: Arc<DeferredInner<T>>,
}

impl<T> Clone for Deferred<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

struct DeferredInner<T> {
    settled: AtomicBool,
    value: Mutex<Option<T>>,
    notify: Notify,
}

impl<T> Deferred<T> {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DeferredInner {
                settled: AtomicBool::new(false),
                value: Mutex::new(None),
                notify: Notify::new(),
            }),
        }
    }

    pub fn resolve(&self, value: T) {
        if self.inner.settled.swap(true, Ordering::SeqCst) {
            return;
        }
        *self.inner.value.lock().expect("deferred mutex poisoned") = Some(value);
        self.inner.notify.notify_waiters();
    }

    pub async fn take_wait(&self) -> T {
        loop {
            let notified = self.inner.notify.notified();
            {
                let mut guard = self.inner.value.lock().expect("deferred mutex poisoned");
                if let Some(val) = guard.take() {
                    return val;
                }
            }
            notified.await;
        }
    }
}

impl<T: Clone> Deferred<T> {
    pub async fn wait(&self) -> T {
        loop {
            let notified = self.inner.notify.notified();
            {
                let guard = self.inner.value.lock().expect("deferred mutex poisoned");
                if let Some(ref val) = *guard {
                    return val.clone();
                }
            }
            notified.await;
        }
    }
}

impl<T> Default for Deferred<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct OpenGate {
    pub entered: Deferred<()>,
    pub release: Deferred<()>,
}

impl OpenGate {
    pub fn new() -> Self {
        Self {
            entered: Deferred::new(),
            release: Deferred::new(),
        }
    }
}

impl Default for OpenGate {
    fn default() -> Self {
        Self::new()
    }
}

struct TestHarnessMutable {
    service_calls: Vec<ServiceCall>,
    fail_attachment_release: Option<Error>,
    fail_close: Option<Error>,
    next_service_error: Option<Error>,
    next_service_result: Option<JsonValue>,
    next_close_gate: Option<OpenGate>,
    next_service_gate: Option<OpenGate>,
    terminated_taken: bool,
}

pub struct TestHarness {
    pub metadata: DefaultSessionMetadata,
    pub closed: Deferred<()>,
    termination: Deferred<Option<Error>>,
    attached_clients: AtomicUsize,
    attachment_release_count: AtomicUsize,
    close_count: AtomicUsize,
    inner: Mutex<TestHarnessMutable>,
}

impl TestHarness {
    pub fn new(metadata: DefaultSessionMetadata) -> Arc<Self> {
        Arc::new(Self {
            metadata,
            closed: Deferred::new(),
            termination: Deferred::new(),
            attached_clients: AtomicUsize::new(0),
            attachment_release_count: AtomicUsize::new(0),
            close_count: AtomicUsize::new(0),
            inner: Mutex::new(TestHarnessMutable {
                service_calls: Vec::new(),
                fail_attachment_release: None,
                fail_close: None,
                next_service_error: None,
                next_service_result: Some(json!({ "ok": true })),
                next_close_gate: None,
                next_service_gate: None,
                terminated_taken: false,
            }),
        })
    }

    pub fn attached_clients(&self) -> usize {
        self.attached_clients.load(Ordering::SeqCst)
    }

    pub fn attachment_release_count(&self) -> usize {
        self.attachment_release_count.load(Ordering::SeqCst)
    }

    pub fn close_count(&self) -> usize {
        self.close_count.load(Ordering::SeqCst)
    }

    pub fn service_calls(&self) -> Vec<ServiceCall> {
        self.inner.lock().expect("harness mutex poisoned").service_calls.clone()
    }

    pub fn set_fail_attachment_release(&self, error: Option<Error>) {
        self.inner
            .lock()
            .expect("harness mutex poisoned")
            .fail_attachment_release = error;
    }

    pub fn set_fail_close(&self, error: Option<Error>) {
        self.inner.lock().expect("harness mutex poisoned").fail_close = error;
    }

    pub fn set_next_service_error(&self, error: Option<Error>) {
        self.inner.lock().expect("harness mutex poisoned").next_service_error = error;
    }

    pub fn set_next_service_result(&self, result: Option<JsonValue>) {
        self.inner.lock().expect("harness mutex poisoned").next_service_result = result;
    }

    pub fn terminate(&self, error: Error) {
        self.termination.resolve(Some(error));
    }

    pub fn gate_next_close(&self) -> OpenGate {
        let gate = OpenGate::new();
        self.inner.lock().expect("harness mutex poisoned").next_close_gate = Some(gate.clone());
        gate
    }

    pub fn gate_next_service_call(&self) -> OpenGate {
        let gate = OpenGate::new();
        self.inner.lock().expect("harness mutex poisoned").next_service_gate = Some(gate.clone());
        gate
    }

    pub async fn invoke_service(&self, call: &ServiceCall) -> Result<Option<JsonValue>> {
        let gate = {
            let mut guard = self.inner.lock().expect("harness mutex poisoned");
            guard.service_calls.push(call.clone());
            if let Some(error) = guard.next_service_error.take() {
                return Err(error);
            }
            guard.next_service_gate.take()
        };
        if let Some(gate) = gate {
            gate.entered.resolve(());
            gate.release.wait().await;
        }
        let mut guard = self.inner.lock().expect("harness mutex poisoned");
        let result = guard.next_service_result.take();
        guard.next_service_result = Some(json!({ "ok": true }));
        Ok(result)
    }
}

struct TestHarnessAttachment {
    harness: Arc<TestHarness>,
    released: AtomicBool,
}

#[async_trait]
impl RoutedSessionAttachment for TestHarnessAttachment {
    async fn invoke_service(
        &self,
        call: &ServiceCall,
        _publish: ServiceUpdatePublisher,
        _context: Context,
    ) -> Result<Option<JsonValue>> {
        self.harness.invoke_service(call).await
    }

    async fn release(&self, _context: Context) -> Result<()> {
        if self.released.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.harness.attachment_release_count.fetch_add(1, Ordering::SeqCst);
        let fail_err = self
            .harness
            .inner
            .lock()
            .expect("harness mutex poisoned")
            .fail_attachment_release
            .take();
        if let Some(err) = fail_err {
            return Err(err);
        }
        if !self.released.swap(true, Ordering::SeqCst) {
            self.harness.attached_clients.fetch_sub(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

#[async_trait]
impl RoutedSessionHandle for TestHarness {
    async fn attach_client(&self, _context: Context) -> Result<Arc<dyn RoutedSessionAttachment>> {
        panic!("Use Arc<TestHarness> attach_client implementation")
    }

    async fn close(&self, _context: Context) -> Result<()> {
        self.close_count.fetch_add(1, Ordering::SeqCst);
        let gate = {
            let mut guard = self.inner.lock().expect("harness mutex poisoned");
            guard.next_close_gate.take()
        };
        if let Some(gate) = gate {
            gate.entered.resolve(());
            gate.release.wait().await;
        }
        let fail = { self.inner.lock().expect("harness mutex poisoned").fail_close.take() };
        if let Some(err) = fail {
            return Err(err);
        }
        self.closed.resolve(());
        self.termination.resolve(None);
        Ok(())
    }
}

/// Wrapper allowing `Arc<TestHarness>` to satisfy [`RoutedSessionHandle`] while retaining shared
/// reference ownership inside attachments.
pub struct TestHarnessHandle(pub Arc<TestHarness>);

#[async_trait]
impl RoutedSessionHandle for TestHarnessHandle {
    async fn attach_client(&self, _context: Context) -> Result<Arc<dyn RoutedSessionAttachment>> {
        self.0.attached_clients.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(TestHarnessAttachment {
            harness: self.0.clone(),
            released: AtomicBool::new(false),
        }))
    }

    fn terminated(&self) -> Option<BoxFuture<Option<Error>>> {
        let mut guard = self.0.inner.lock().expect("harness mutex poisoned");
        if guard.terminated_taken {
            return None;
        }
        guard.terminated_taken = true;
        drop(guard);
        let termination = self.0.termination.clone();
        Some(Box::pin(async move { termination.take_wait().await }))
    }

    async fn close(&self, context: Context) -> Result<()> {
        self.0.close(context).await
    }
}

struct TestServerServicesHost;
struct TestServerServicesAttachment {
    presentation: Arc<dyn RoutedServerPresentation>,
}

#[async_trait]
impl RoutedServerServiceHost for TestServerServicesHost {
    async fn attach_client(
        &self,
        presentation: Arc<dyn RoutedServerPresentation>,
        _context: Context,
    ) -> Result<Arc<dyn RoutedServerServiceAttachment>> {
        Ok(Arc::new(TestServerServicesAttachment { presentation }))
    }
}

#[async_trait]
impl RoutedServerServiceAttachment for TestServerServicesAttachment {
    async fn invoke_service(
        &self,
        call: &ServiceCall,
        _publish: ServiceUpdatePublisher,
        context: Context,
    ) -> Result<Option<JsonValue>> {
        if call.instance.is_none()
            && call.service_id == "pi.session-management"
            && call.member == "attach"
            && call.args.len() == 1
            && let Some(session_id) = call.args[0].as_str()
        {
            self.presentation.attach_session(session_id, context).await?;
            return Ok(Some(JsonValue::Null));
        }
        if call.instance.is_none()
            && call.service_id == "pi.session-management"
            && call.member == "detach"
            && call.args.is_empty()
        {
            self.presentation.detach_session(context).await?;
            return Ok(Some(JsonValue::Null));
        }
        Err(Error::msg(format!(
            "Unsupported test server service {}.{}",
            call.service_id, call.member
        )))
    }

    async fn release(&self, _context: Context) -> Result<()> {
        Ok(())
    }
}

pub fn create_test_server_services() -> Arc<dyn RoutedServerServiceHost> {
    Arc::new(TestServerServicesHost)
}

struct TestServerHostMutable {
    sessions: IndexMap<String, DefaultSessionMetadata>,
    harnesses: IndexMap<String, Vec<Arc<TestHarness>>>,
    next_open_session_error: Option<Error>,
    next_harness_close_error: Option<Error>,
    next_open_session_gate: Option<OpenGate>,
}

pub struct TestServerHost {
    server_services: Arc<dyn RoutedServerServiceHost>,
    open_session_count: AtomicUsize,
    state: Mutex<TestServerHostMutable>,
}

impl TestServerHost {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            server_services: create_test_server_services(),
            open_session_count: AtomicUsize::new(0),
            state: Mutex::new(TestServerHostMutable {
                sessions: IndexMap::new(),
                harnesses: IndexMap::new(),
                next_open_session_error: None,
                next_harness_close_error: None,
                next_open_session_gate: None,
            }),
        })
    }

    pub fn open_session_count(&self) -> usize {
        self.open_session_count.load(Ordering::SeqCst)
    }

    pub fn harnesses_count(&self) -> usize {
        self.state.lock().expect("test host mutex poisoned").harnesses.len()
    }

    pub fn harnesses_for(&self, id: &str) -> Vec<Arc<TestHarness>> {
        self.state
            .lock()
            .expect("test host mutex poisoned")
            .harnesses
            .get(id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn latest_harness(&self, id: &str) -> Result<Arc<TestHarness>> {
        self.state
            .lock()
            .expect("test host mutex poisoned")
            .harnesses
            .get(id)
            .and_then(|list| list.last().cloned())
            .ok_or_else(|| Error::msg(format!("No harness for {id}")))
    }

    pub fn set_next_open_session_error(&self, error: Option<Error>) {
        self.state
            .lock()
            .expect("test host mutex poisoned")
            .next_open_session_error = error;
    }

    pub fn set_next_harness_close_error(&self, error: Option<Error>) {
        self.state
            .lock()
            .expect("test host mutex poisoned")
            .next_harness_close_error = error;
    }

    pub async fn seed(&self, id: &str) -> DefaultSessionMetadata {
        let metadata = DefaultSessionMetadata::new(id);
        self.state
            .lock()
            .expect("test host mutex poisoned")
            .sessions
            .insert(id.to_string(), metadata.clone());
        metadata
    }

    pub fn gate_next_open_session(&self) -> OpenGate {
        let gate = OpenGate::new();
        self.state
            .lock()
            .expect("test host mutex poisoned")
            .next_open_session_gate = Some(gate.clone());
        gate
    }
}

#[async_trait]
impl ServerHost<DefaultSessionMetadata> for TestServerHost {
    fn server_services(&self) -> Arc<dyn RoutedServerServiceHost> {
        self.server_services.clone()
    }

    async fn resolve_session(&self, session_id: &str, _context: Context) -> Result<DefaultSessionMetadata> {
        let guard = self.state.lock().expect("test host mutex poisoned");
        guard
            .sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| SessionNotFoundError::new(format!("Unknown session: {session_id}")).into())
    }

    async fn open_session(
        &self,
        metadata: DefaultSessionMetadata,
        _context: Context,
    ) -> Result<Arc<dyn RoutedSessionHandle>> {
        self.open_session_count.fetch_add(1, Ordering::SeqCst);
        let gate = {
            self.state
                .lock()
                .expect("test host mutex poisoned")
                .next_open_session_gate
                .take()
        };
        if let Some(gate) = gate {
            gate.entered.resolve(());
            gate.release.wait().await;
        }
        let mut guard = self.state.lock().expect("test host mutex poisoned");
        if !guard.sessions.contains_key(&metadata.id) {
            return Err(SessionNotFoundError::new(format!("Unknown session: {}", metadata.id)).into());
        }
        if let Some(error) = guard.next_open_session_error.take() {
            return Err(error);
        }
        let harness = TestHarness::new(metadata.clone());
        if let Some(close_error) = guard.next_harness_close_error.take() {
            harness.set_fail_close(Some(close_error));
        }
        guard.harnesses.entry(metadata.id).or_default().push(harness.clone());
        Ok(Arc::new(TestHarnessHandle(harness)))
    }
}

/// Polls a condition until it returns `true` within a reasonable test deadline.
pub async fn poll_until(mut check: impl FnMut() -> bool) -> Result<()> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if check() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::msg("Timed out polling condition"));
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

#[allow(dead_code)]
fn _watch_type_check(_w: &watch::Sender<()>) {}
