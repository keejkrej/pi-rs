//! Port of packages/server/src/session-router.ts

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::{IndexMap, IndexSet};
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::{Context, JsonValue, ServiceCall, ServiceUpdatePublisher};
use pi_js::crypto::random_uuid;
use pi_js::{BoxFuture, Error, Result};
use pi_protocol::{RpcTarget, SessionTarget};
use tokio::sync::{oneshot, watch};

use crate::connection::ConnectionState;
use crate::errors::{AggregateError, ServerDrainingError, SessionNotAttachedError, shared_js_error};
use crate::types::{RoutedSessionAttachment, RoutedSessionHandle, ServerHost, SessionMetadata};

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
struct SessionCleanupError {
    message: String,
    errors: Vec<Error>,
}

impl SessionCleanupError {
    fn new(errors: Vec<Error>, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            errors,
        }
    }

    fn into_error(self) -> Error {
        Error::typed("SessionCleanupError", self)
    }
}

#[derive(Clone)]
struct OpeningError {
    error: Arc<Error>,
    is_cleanup: bool,
}

struct OpeningEntry {
    tx: watch::Sender<Option<std::result::Result<Arc<HostedSession>, OpeningError>>>,
    rx: watch::Receiver<Option<std::result::Result<Arc<HostedSession>, OpeningError>>>,
}

impl OpeningEntry {
    fn new() -> Arc<Self> {
        let (tx, rx) = watch::channel(None);
        Arc::new(Self { tx, rx })
    }
}

struct OperationEntry {
    done_rx: watch::Receiver<bool>,
}

struct ClientAttachment {
    id: String,
    client: Arc<ConnectionState>,
    client_key: usize,
    session: Arc<HostedSession>,
    operations: Mutex<IndexMap<u64, OperationEntry>>,
    acquire_tx: watch::Sender<Option<std::result::Result<(), ()>>>,
    acquire_rx: watch::Receiver<Option<std::result::Result<(), ()>>>,
    lease: Mutex<Option<Arc<dyn RoutedSessionAttachment>>>,
    releasing: Mutex<Option<watch::Receiver<Option<std::result::Result<(), Arc<Error>>>>>>,
}

impl ClientAttachment {
    fn new(client: Arc<ConnectionState>, client_key: usize, session: Arc<HostedSession>) -> Arc<Self> {
        let (acquire_tx, acquire_rx) = watch::channel(None);
        Arc::new(Self {
            id: random_uuid(),
            client,
            client_key,
            session,
            operations: Mutex::new(IndexMap::new()),
            acquire_tx,
            acquire_rx,
            lease: Mutex::new(None),
            releasing: Mutex::new(None),
        })
    }
}

struct HostedSession {
    id: String,
    handle: Arc<dyn RoutedSessionHandle>,
    attachments: Mutex<IndexMap<String, Arc<ClientAttachment>>>,
    invalidated: AtomicBool,
}

impl HostedSession {
    fn new(id: String, handle: Arc<dyn RoutedSessionHandle>) -> Arc<Self> {
        Arc::new(Self {
            id,
            handle,
            attachments: Mutex::new(IndexMap::new()),
            invalidated: AtomicBool::new(false),
        })
    }
}

struct ClientOpTail {
    id: u64,
    done_rx: watch::Receiver<bool>,
}

pub(crate) type AttachmentPublisher =
    Arc<dyn Fn(Arc<ConnectionState>, Option<SessionTarget>, Context) -> BoxFuture<Result<()>> + Send + Sync>;

pub(crate) struct SessionRouterOptions<TMetadata: SessionMetadata> {
    pub host: Arc<dyn ServerHost<TMetadata>>,
    pub server_id: String,
    pub is_closing: Arc<dyn Fn() -> bool + Send + Sync>,
    pub publish_attachment: AttachmentPublisher,
    pub report_error: Arc<dyn Fn(&Error) + Send + Sync>,
}

struct RouterMutableState {
    hosted_sessions: IndexMap<String, Arc<HostedSession>>,
    opening_sessions: IndexMap<String, Arc<OpeningEntry>>,
    attachments_by_client: IndexMap<usize, Arc<ClientAttachment>>,
    disconnected_clients: IndexSet<usize>,
    client_operations: IndexMap<usize, ClientOpTail>,
    close_rx: Option<watch::Receiver<Option<std::result::Result<(), Arc<Error>>>>>,
}

struct SessionRouterInner<TMetadata: SessionMetadata> {
    options: SessionRouterOptions<TMetadata>,
    next_op_id: AtomicU64,
    state: Mutex<RouterMutableState>,
}

pub(crate) struct SessionRouter<TMetadata: SessionMetadata> {
    inner: Arc<SessionRouterInner<TMetadata>>,
}

impl<TMetadata: SessionMetadata> Clone for SessionRouter<TMetadata> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

struct ClientOpGuard<TMetadata: SessionMetadata> {
    inner: Arc<SessionRouterInner<TMetadata>>,
    client_key: usize,
    op_id: u64,
    done_tx: watch::Sender<bool>,
}

impl<TMetadata: SessionMetadata> Drop for ClientOpGuard<TMetadata> {
    fn drop(&mut self) {
        self.done_tx.send_replace(true);
        let mut guard = self.inner.state.lock().expect("router mutex poisoned");
        if guard
            .client_operations
            .get(&self.client_key)
            .is_some_and(|tail| tail.id == self.op_id)
        {
            guard.client_operations.shift_remove(&self.client_key);
        }
    }
}

struct ActiveOpGuard {
    attachment: Arc<ClientAttachment>,
    op_id: u64,
    done_tx: watch::Sender<bool>,
}

impl Drop for ActiveOpGuard {
    fn drop(&mut self) {
        self.done_tx.send_replace(true);
        let mut ops = self
            .attachment
            .operations
            .lock()
            .expect("attachment operations mutex poisoned");
        ops.shift_remove(&self.op_id);
    }
}

impl<TMetadata: SessionMetadata> SessionRouter<TMetadata> {
    pub(crate) fn new(options: SessionRouterOptions<TMetadata>) -> Self {
        Self {
            inner: Arc::new(SessionRouterInner {
                options,
                next_op_id: AtomicU64::new(1),
                state: Mutex::new(RouterMutableState {
                    hosted_sessions: IndexMap::new(),
                    opening_sessions: IndexMap::new(),
                    attachments_by_client: IndexMap::new(),
                    disconnected_clients: IndexSet::new(),
                    client_operations: IndexMap::new(),
                    close_rx: None,
                }),
            }),
        }
    }

    pub(crate) async fn execute_service_call(
        &self,
        call: ServiceCall,
        target: RpcTarget,
        client: Arc<ConnectionState>,
        publish: ServiceUpdatePublisher,
        context: Context,
    ) -> Result<Option<JsonValue>> {
        let router = self.clone();
        let rx = self
            .run_for_client(client.clone(), move || async move {
                router.start_service_call(client, target, call, publish, context).await
            })
            .await?;
        rx.await
            .unwrap_or_else(|_| Err(Error::msg("Service operation dropped unexpectedly")))
    }

    pub(crate) fn attach_client(
        &self,
        client: Arc<ConnectionState>,
        session_id: String,
        context: Context,
    ) -> BoxFuture<Result<()>> {
        if (self.inner.options.is_closing)() {
            return Box::pin(async { Err(ServerDrainingError::new().into()) });
        }
        let router = self.clone();
        Box::pin(self.run_for_client(client.clone(), move || async move {
            router.attach_client_now(client, session_id, context).await
        }))
    }

    pub(crate) fn detach_client(&self, client: Arc<ConnectionState>, context: Context) -> BoxFuture<Result<()>> {
        let router = self.clone();
        Box::pin(self.run_for_client(client.clone(), move || async move {
            let key = client_key(&client);
            let attachment = {
                let guard = router.inner.state.lock().expect("router mutex poisoned");
                guard.attachments_by_client.get(&key).cloned()
            };
            if let Some(att) = attachment {
                router.release_attachment(att, context, true).await?;
            }
            Ok(())
        }))
    }

    pub(crate) async fn remove_session(&self, session_id: &str, context: Context) -> Result<()> {
        if (self.inner.options.is_closing)() {
            return Err(ServerDrainingError::new().into());
        }
        let hosted = {
            let guard = self.inner.state.lock().expect("router mutex poisoned");
            guard.hosted_sessions.get(session_id).cloned()
        };
        let Some(hosted) = hosted else {
            return Ok(());
        };
        let attachments: Vec<Arc<ClientAttachment>> = {
            let guard = hosted.attachments.lock().expect("hosted attachments mutex poisoned");
            guard.values().cloned().collect()
        };
        let mut errors = Vec::new();
        let release_futs: Vec<_> = attachments
            .into_iter()
            .map(|att| self.release_attachment(att, context.clone(), true))
            .collect();
        for res in futures::future::join_all(release_futs).await {
            if let Err(err) = res {
                errors.push(err);
            }
        }
        if let Err(err) = hosted.handle.close(context).await {
            errors.push(err);
        }
        {
            let mut guard = self.inner.state.lock().expect("router mutex poisoned");
            if guard
                .hosted_sessions
                .get(session_id)
                .is_some_and(|existing| Arc::ptr_eq(existing, &hosted))
            {
                guard.hosted_sessions.shift_remove(session_id);
            }
        }
        if errors.len() == 1 {
            return Err(errors.remove(0));
        }
        if errors.len() > 1 {
            return Err(AggregateError::new(errors, format!("Failed to close Session {session_id}")).into());
        }
        Ok(())
    }

    pub(crate) async fn disconnect(&self, client: Arc<ConnectionState>, context: Context) -> Result<()> {
        let key = client_key(&client);
        {
            let mut guard = self.inner.state.lock().expect("router mutex poisoned");
            guard.disconnected_clients.insert(key);
        }
        struct DisconnectGuard<TMetadata: SessionMetadata> {
            inner: Arc<SessionRouterInner<TMetadata>>,
            key: usize,
        }
        impl<TMetadata: SessionMetadata> Drop for DisconnectGuard<TMetadata> {
            fn drop(&mut self) {
                let mut guard = self.inner.state.lock().expect("router mutex poisoned");
                guard.disconnected_clients.shift_remove(&self.key);
            }
        }
        let _disconnect_guard = DisconnectGuard {
            inner: self.inner.clone(),
            key,
        };

        let router = self.clone();
        self.run_for_client(client.clone(), move || async move {
            let attachment = {
                let guard = router.inner.state.lock().expect("router mutex poisoned");
                guard.attachments_by_client.get(&key).cloned()
            };
            if let Some(att) = attachment {
                router.release_attachment(att, context, false).await?;
            }
            Ok(())
        })
        .await
    }

    pub(crate) fn close(&self, context: Context) -> BoxFuture<Result<()>> {
        let mut guard = self.inner.state.lock().expect("router mutex poisoned");
        if let Some(rx) = guard.close_rx.clone() {
            drop(guard);
            return Box::pin(await_watch_result(rx));
        }
        let (tx, rx) = watch::channel(None);
        guard.close_rx = Some(rx.clone());
        drop(guard);

        let router = self.clone();
        Box::pin(async move {
            let result = router.close_internal(context).await;
            match result {
                Ok(()) => {
                    tx.send_replace(Some(Ok(())));
                    Ok(())
                }
                Err(err) => {
                    let shared = Arc::new(err);
                    tx.send_replace(Some(Err(shared.clone())));
                    Err(shared_js_error(shared))
                }
            }
        })
    }

    async fn close_internal(&self, context: Context) -> Result<()> {
        let (operation_tails, opening_entries) = {
            let guard = self.inner.state.lock().expect("router mutex poisoned");
            let ops: Vec<watch::Receiver<bool>> = guard
                .client_operations
                .values()
                .map(|tail| tail.done_rx.clone())
                .collect();
            let openings: Vec<Arc<OpeningEntry>> = guard.opening_sessions.values().cloned().collect();
            (ops, openings)
        };

        let op_futs = operation_tails.into_iter().map(|mut rx| async move {
            let _ = rx.wait_for(|done| *done).await;
        });
        let opening_futs = opening_entries.into_iter().map(|entry| async move {
            let mut rx = entry.rx.clone();
            let _ = rx.wait_for(|val| val.is_some()).await;
            let val = rx.borrow().clone();
            val
        });
        let (_, opening_results) = tokio::join!(
            futures::future::join_all(op_futs),
            futures::future::join_all(opening_futs),
        );

        let mut close_errors: Vec<Error> = Vec::new();
        for res in opening_results {
            if let Some(Err(opening_err)) = res {
                (self.inner.options.report_error)(&opening_err.error);
                if opening_err.is_cleanup {
                    close_errors.push(shared_js_error(opening_err.error));
                }
            }
        }

        let attachments: Vec<Arc<ClientAttachment>> = {
            let guard = self.inner.state.lock().expect("router mutex poisoned");
            guard
                .hosted_sessions
                .values()
                .flat_map(|session| {
                    session
                        .attachments
                        .lock()
                        .expect("hosted attachments mutex poisoned")
                        .values()
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .collect()
        };

        let release_futs = attachments
            .into_iter()
            .map(|att| self.release_attachment(att, context.clone(), true));
        for res in futures::future::join_all(release_futs).await {
            if let Err(err) = res {
                close_errors.push(err);
            }
        }

        let hosted: Vec<Arc<HostedSession>> = {
            let guard = self.inner.state.lock().expect("router mutex poisoned");
            guard.hosted_sessions.values().cloned().collect()
        };

        let close_futs = hosted.iter().map(|session| session.handle.close(context.clone()));
        let close_results = futures::future::join_all(close_futs).await;
        for (session, result) in hosted.into_iter().zip(close_results) {
            match result {
                Ok(()) => {
                    let mut guard = self.inner.state.lock().expect("router mutex poisoned");
                    if guard
                        .hosted_sessions
                        .get(&session.id)
                        .is_some_and(|existing| Arc::ptr_eq(existing, &session))
                    {
                        guard.hosted_sessions.shift_remove(&session.id);
                    }
                }
                Err(err) => {
                    (self.inner.options.report_error)(&err);
                    close_errors.push(err);
                }
            }
        }

        {
            let mut guard = self.inner.state.lock().expect("router mutex poisoned");
            guard.attachments_by_client.clear();
            guard.client_operations.clear();
        }

        if !close_errors.is_empty() {
            return Err(AggregateError::new(close_errors, "Failed to close routed Sessions").into());
        }
        Ok(())
    }

    fn run_for_client<T, F, Fut>(&self, client: Arc<ConnectionState>, operation: F) -> BoxFuture<Result<T>>
    where
        T: Send + 'static,
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        let key = client_key(&client);
        let op_id = self.inner.next_op_id.fetch_add(1, Ordering::Relaxed);
        let (done_tx, done_rx) = watch::channel(false);
        let prev_rx = {
            let mut guard = self.inner.state.lock().expect("router mutex poisoned");
            let prev = guard.client_operations.get(&key).map(|tail| tail.done_rx.clone());
            guard.client_operations.insert(key, ClientOpTail { id: op_id, done_rx });
            prev
        };
        let guard = ClientOpGuard {
            inner: self.inner.clone(),
            client_key: key,
            op_id,
            done_tx,
        };
        Box::pin(async move {
            let _guard = guard;
            if let Some(mut prev) = prev_rx {
                let _ = prev.wait_for(|done| *done).await;
            }
            operation().await
        })
    }

    async fn attach_client_now(
        &self,
        client: Arc<ConnectionState>,
        session_id: String,
        context: Context,
    ) -> Result<()> {
        let key = client_key(&client);
        {
            let guard = self.inner.state.lock().expect("router mutex poisoned");
            if (self.inner.options.is_closing)() || guard.disconnected_clients.contains(&key) {
                return Err(ServerDrainingError::new().into());
            }
            if let Some(current) = guard.attachments_by_client.get(&key)
                && current.session.id == session_id
            {
                return Ok(());
            }
        }

        let current = {
            let guard = self.inner.state.lock().expect("router mutex poisoned");
            guard.attachments_by_client.get(&key).cloned()
        };

        let hosted = self.acquire(&session_id, context.clone()).await?;

        {
            let guard = self.inner.state.lock().expect("router mutex poisoned");
            if (self.inner.options.is_closing)() || guard.disconnected_clients.contains(&key) {
                return Err(ServerDrainingError::new().into());
            }
        }

        if let Some(prev) = current {
            self.release_attachment(prev, context.clone(), false).await?;
        }

        let attachment = ClientAttachment::new(client.clone(), key, hosted.clone());
        {
            let mut atts = hosted.attachments.lock().expect("hosted attachments mutex poisoned");
            atts.insert(attachment.id.clone(), attachment.clone());
        }

        match hosted.handle.attach_client(context.clone()).await {
            Ok(lease) => {
                *attachment.lease.lock().expect("lease mutex poisoned") = Some(lease);
                attachment.acquire_tx.send_replace(Some(Ok(())));
            }
            Err(err) => {
                attachment.acquire_tx.send_replace(Some(Err(())));
                hosted
                    .attachments
                    .lock()
                    .expect("hosted attachments mutex poisoned")
                    .shift_remove(&attachment.id);
                return Err(err);
            }
        }

        let invalidated_or_draining = {
            let guard = self.inner.state.lock().expect("router mutex poisoned");
            let hosted_current = guard
                .hosted_sessions
                .get(&hosted.id)
                .is_some_and(|existing| Arc::ptr_eq(existing, &hosted));
            let att_present = hosted
                .attachments
                .lock()
                .expect("hosted attachments mutex poisoned")
                .contains_key(&attachment.id);
            hosted.invalidated.load(Ordering::SeqCst)
                || !hosted_current
                || !att_present
                || guard.disconnected_clients.contains(&key)
                || (self.inner.options.is_closing)()
        };

        if invalidated_or_draining {
            self.release_attachment(attachment, context, true).await?;
            return Err(ServerDrainingError::new().into());
        }

        {
            let mut guard = self.inner.state.lock().expect("router mutex poisoned");
            guard.attachments_by_client.insert(key, attachment.clone());
        }

        (self.inner.options.publish_attachment)(
            client,
            Some(SessionTarget {
                server_id: self.inner.options.server_id.clone(),
                session_id,
                attachment_id: attachment.id.clone(),
            }),
            context,
        )
        .await
    }

    async fn start_service_call(
        &self,
        client: Arc<ConnectionState>,
        target: RpcTarget,
        call: ServiceCall,
        publish: ServiceUpdatePublisher,
        context: Context,
    ) -> Result<oneshot::Receiver<Result<Option<JsonValue>>>> {
        let key = client_key(&client);
        let attachment = {
            let guard = self.inner.state.lock().expect("router mutex poisoned");
            if (self.inner.options.is_closing)() || guard.disconnected_clients.contains(&key) {
                return Err(ServerDrainingError::new().into());
            }
            let session_target = match &target {
                RpcTarget::Session(st) => st,
                RpcTarget::Server(_) => return Err(SessionNotAttachedError::new().into()),
            };
            let Some(attachment) = guard.attachments_by_client.get(&key) else {
                return Err(SessionNotAttachedError::new().into());
            };
            if attachment.session.id != session_target.session_id || attachment.id != session_target.attachment_id {
                return Err(SessionNotAttachedError::new().into());
            }
            attachment.clone()
        };

        let lease = {
            attachment
                .lease
                .lock()
                .expect("lease mutex poisoned")
                .clone()
                .ok_or_else(SessionNotAttachedError::new)?
        };

        let op_id = self.inner.next_op_id.fetch_add(1, Ordering::Relaxed);
        let (done_tx, done_rx) = watch::channel(false);
        {
            let mut ops = attachment
                .operations
                .lock()
                .expect("attachment operations mutex poisoned");
            ops.insert(op_id, OperationEntry { done_rx });
        }

        let op_guard = ActiveOpGuard {
            attachment,
            op_id,
            done_tx,
        };
        let (res_tx, res_rx) = oneshot::channel();
        tokio::spawn(async move {
            let _guard = op_guard;
            let result = lease.invoke_service(&call, publish, context).await;
            let _ = res_tx.send(result);
        });
        Ok(res_rx)
    }

    fn release_attachment(
        &self,
        attachment: Arc<ClientAttachment>,
        context: Context,
        publish: bool,
    ) -> BoxFuture<Result<()>> {
        let mut guard = attachment
            .releasing
            .lock()
            .expect("attachment releasing mutex poisoned");
        if let Some(rx) = guard.clone() {
            drop(guard);
            return Box::pin(await_watch_result(rx));
        }
        let (tx, rx) = watch::channel(None);
        *guard = Some(rx);
        drop(guard);

        let router = self.clone();
        Box::pin(async move {
            let result = router.release_attachment_internal(attachment, context, publish).await;
            match result {
                Ok(()) => {
                    tx.send_replace(Some(Ok(())));
                    Ok(())
                }
                Err(err) => {
                    let shared = Arc::new(err);
                    tx.send_replace(Some(Err(shared.clone())));
                    Err(shared_js_error(shared))
                }
            }
        })
    }

    async fn release_attachment_internal(
        &self,
        attachment: Arc<ClientAttachment>,
        context: Context,
        publish: bool,
    ) -> Result<()> {
        let mut errors: Vec<Error> = Vec::new();
        loop {
            let pending: Vec<watch::Receiver<bool>> = {
                let ops = attachment
                    .operations
                    .lock()
                    .expect("attachment operations mutex poisoned");
                if ops.is_empty() {
                    break;
                }
                ops.values().map(|entry| entry.done_rx.clone()).collect()
            };
            let wait_futs = pending.into_iter().map(|mut rx| async move {
                let _ = rx.wait_for(|done| *done).await;
            });
            futures::future::join_all(wait_futs).await;
        }

        let lease = {
            let existing = attachment.lease.lock().expect("lease mutex poisoned").clone();
            if existing.is_some() {
                existing
            } else {
                let mut acquire_rx = attachment.acquire_rx.clone();
                let _ = acquire_rx.wait_for(|val| val.is_some()).await;
                attachment.lease.lock().expect("lease mutex poisoned").clone()
            }
        };

        if let Some(lease) = lease
            && let Err(err) = lease.release(context.clone()).await
        {
            errors.push(err);
        }

        self.clear_attachment(attachment, context, publish).await?;

        if errors.len() == 1 {
            return Err(errors.remove(0));
        }
        if errors.len() > 1 {
            return Err(AggregateError::new(errors, "Failed to release Session attachment").into());
        }
        Ok(())
    }

    async fn clear_attachment(&self, attachment: Arc<ClientAttachment>, context: Context, publish: bool) -> Result<()> {
        attachment
            .session
            .attachments
            .lock()
            .expect("hosted attachments mutex poisoned")
            .shift_remove(&attachment.id);
        let removed_current = {
            let mut guard = self.inner.state.lock().expect("router mutex poisoned");
            if guard
                .attachments_by_client
                .get(&attachment.client_key)
                .is_some_and(|existing| Arc::ptr_eq(existing, &attachment))
            {
                guard.attachments_by_client.shift_remove(&attachment.client_key);
                true
            } else {
                false
            }
        };
        if removed_current && publish {
            (self.inner.options.publish_attachment)(attachment.client.clone(), None, context).await?;
        }
        Ok(())
    }

    async fn acquire(&self, session_id: &str, context: Context) -> Result<Arc<HostedSession>> {
        let entry = {
            let mut guard = self.inner.state.lock().expect("router mutex poisoned");
            if let Some(existing) = guard.hosted_sessions.get(session_id) {
                return Ok(existing.clone());
            }
            if let Some(opening) = guard.opening_sessions.get(session_id) {
                Some((false, opening.clone()))
            } else {
                let entry = OpeningEntry::new();
                guard.opening_sessions.insert(session_id.to_string(), entry.clone());
                Some((true, entry))
            }
        };

        let (is_owner, entry) = entry.expect("entry initialized above");
        if !is_owner {
            let mut rx = entry.rx.clone();
            let _ = rx.wait_for(|val| val.is_some()).await;
            let outcome = rx.borrow().clone().expect("watch reported Some before borrow");
            return match outcome {
                Ok(hosted) => Ok(hosted),
                Err(opening_err) => Err(shared_js_error(opening_err.error)),
            };
        }

        struct OpeningGuard<TMetadata: SessionMetadata> {
            inner: Arc<SessionRouterInner<TMetadata>>,
            session_id: String,
            entry: Arc<OpeningEntry>,
            settled: bool,
        }
        impl<TMetadata: SessionMetadata> Drop for OpeningGuard<TMetadata> {
            fn drop(&mut self) {
                if !self.settled {
                    self.entry.tx.send_replace(Some(Err(OpeningError {
                        error: Arc::new(Error::msg("Session open aborted")),
                        is_cleanup: false,
                    })));
                }
                let mut guard = self.inner.state.lock().expect("router mutex poisoned");
                if guard
                    .opening_sessions
                    .get(&self.session_id)
                    .is_some_and(|existing| Arc::ptr_eq(existing, &self.entry))
                {
                    guard.opening_sessions.shift_remove(&self.session_id);
                }
            }
        }

        let mut guard = OpeningGuard {
            inner: self.inner.clone(),
            session_id: session_id.to_string(),
            entry: entry.clone(),
            settled: false,
        };

        match self.open(session_id, context).await {
            Ok(hosted) => {
                entry.tx.send_replace(Some(Ok(hosted.clone())));
                guard.settled = true;
                Ok(hosted)
            }
            Err((err, is_cleanup)) => {
                let shared = Arc::new(err);
                entry.tx.send_replace(Some(Err(OpeningError {
                    error: shared.clone(),
                    is_cleanup,
                })));
                guard.settled = true;
                Err(shared_js_error(shared))
            }
        }
    }

    async fn open(&self, session_id: &str, context: Context) -> std::result::Result<Arc<HostedSession>, (Error, bool)> {
        let metadata = self
            .inner
            .options
            .host
            .resolve_session(session_id, context.clone())
            .await
            .map_err(|err| (err, false))?;
        let hosted_id = metadata.id().to_string();
        let handle = self
            .inner
            .options
            .host
            .open_session(metadata, context.clone())
            .await
            .map_err(|err| (err, false))?;
        if (self.inner.options.is_closing)() {
            if let Err(err) = handle.close(context).await {
                (self.inner.options.report_error)(&err);
                let cleanup_err = SessionCleanupError::new(
                    vec![ServerDrainingError::new().into(), err],
                    "Failed to close routed Session acquired while draining",
                )
                .into_error();
                return Err((cleanup_err, true));
            }
            return Err((ServerDrainingError::new().into(), false));
        }

        let hosted = HostedSession::new(hosted_id, handle.clone());
        {
            let mut guard = self.inner.state.lock().expect("router mutex poisoned");
            guard.hosted_sessions.insert(hosted.id.clone(), hosted.clone());
        }

        if let Some(terminated_fut) = handle.terminated() {
            let router = self.clone();
            let hosted_clone = hosted.clone();
            tokio::spawn(async move {
                let err = terminated_fut.await;
                router.invalidate(hosted_clone, err);
            });
        }

        Ok(hosted)
    }

    fn invalidate(&self, hosted: Arc<HostedSession>, error: Option<Error>) {
        hosted.invalidated.store(true, Ordering::SeqCst);
        let removed = {
            let mut guard = self.inner.state.lock().expect("router mutex poisoned");
            if guard
                .hosted_sessions
                .get(&hosted.id)
                .is_some_and(|existing| Arc::ptr_eq(existing, &hosted))
            {
                guard.hosted_sessions.shift_remove(&hosted.id);
                true
            } else {
                false
            }
        };
        if !removed {
            return;
        }
        let attachments: Vec<Arc<ClientAttachment>> = {
            hosted
                .attachments
                .lock()
                .expect("hosted attachments mutex poisoned")
                .values()
                .cloned()
                .collect()
        };
        for attachment in attachments {
            let router = self.clone();
            tokio::spawn(async move {
                if let Err(release_err) = router
                    .release_attachment(attachment, BACKGROUND_CONTEXT.clone(), true)
                    .await
                {
                    (router.inner.options.report_error)(&release_err);
                }
            });
        }
        if let Some(err) = error {
            (self.inner.options.report_error)(&err);
        }
    }
}

async fn await_watch_result(mut rx: watch::Receiver<Option<std::result::Result<(), Arc<Error>>>>) -> Result<()> {
    let _ = rx.wait_for(|val| val.is_some()).await;
    let outcome = rx.borrow().clone().expect("watch reported Some before borrow");
    match outcome {
        Ok(()) => Ok(()),
        Err(shared) => Err(shared_js_error(shared)),
    }
}

fn client_key(client: &Arc<ConnectionState>) -> usize {
    Arc::as_ptr(client) as usize
}
