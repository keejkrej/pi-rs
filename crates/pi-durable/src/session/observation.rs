//! Port of packages/durable/src/session/observation.ts

use std::collections::VecDeque;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError, Weak};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_chord::context::without_abort_signal;
use pi_chord::delta::{Op, OpReplace, ReplaceVerb};
use pi_chord::types::ReplicatedStateSourceSnapshot;
use pi_chord::{
    Context, JsonValue, ReplicatedStateSource, ReplicatedStateSourceAttachment, ReplicatedStateSourceFrame,
};
use pi_js::abort::{AbortSignal, ListenerGuard};
use tokio::sync::watch;

use crate::types::{JsonObject, WatchEnd, WatchHandle, WatchHandleApi, WatchListener};

pub type ObservedDocumentValue = Option<JsonObject>;

/// Maximum exact committed frames retained behind one unavailable watch listener.
const MAX_PENDING_WATCH_FRAMES: usize = 100;

/// Canonical terminal update for a retired document incarnation.
pub static RETIREMENT_OPERATIONS: LazyLock<Vec<Op>> =
    LazyLock::new(|| vec![Op::Replace(OpReplace(ReplaceVerb::R, JsonValue::Null))]);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

type FrameListener<T> = Arc<dyn Fn(ReplicatedStateSourceFrame<T>) -> pi_js::Result<()> + Send + Sync>;

struct SourceState<T> {
    next_attachment_id: u64,
    attachments: IndexMap<u64, Arc<SessionSourceAttachmentInner<T>>>,
    release: Option<Box<dyn FnOnce() + Send + Sync>>,
    value: Option<T>,
    cursor: i64,
    retired: bool,
    closed: bool,
}

struct CommittedStateSourceInner<T> {
    is_retired: fn(&T) -> bool,
    state: Mutex<SourceState<T>>,
}

impl<T: Send + Sync + 'static> CommittedStateSourceInner<T> {
    fn remove_attachment(&self, id: u64) {
        let should_dispose = {
            let mut guard = lock(&self.state);
            guard.attachments.shift_remove(&id);
            guard.attachments.is_empty()
        };
        if should_dispose {
            self.finish_disposal();
        }
    }

    fn finish_disposal(&self) {
        let release = {
            let mut guard = lock(&self.state);
            if guard.closed {
                return;
            }
            guard.closed = true;
            guard.value = None;
            guard.release.take()
        };
        if let Some(release) = release {
            release();
        }
    }
}

/// Session-to-Chord bridge owned one-to-one by one attached state: a document, or a conversation view.
/// A retired sentinel value (e.g. `JsonValue::Null` or `None`) retires it.
pub struct CommittedStateSource<T = JsonValue> {
    inner: Arc<CommittedStateSourceInner<T>>,
}

impl CommittedStateSource<JsonValue> {
    pub fn for_json_value(value: JsonValue, release: impl FnOnce() + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self::new(value, |v| v.is_null(), Box::new(release)))
    }
}

impl<T: Clone + Send + Sync + 'static> CommittedStateSource<T> {
    pub fn new(value: T, is_retired: fn(&T) -> bool, release: Box<dyn FnOnce() + Send + Sync>) -> Self {
        Self {
            inner: Arc::new(CommittedStateSourceInner {
                is_retired,
                state: Mutex::new(SourceState {
                    next_attachment_id: 1,
                    attachments: IndexMap::new(),
                    release: Some(release),
                    value: Some(value),
                    cursor: 0,
                    retired: false,
                    closed: false,
                }),
            }),
        }
    }

    pub fn advance(&self, value: T, ops: Vec<Op>, context: Context) {
        let retired = (self.inner.is_retired)(&value);
        let (frame, targets) = {
            let mut guard = lock(&self.inner.state);
            if guard.closed || guard.retired {
                return;
            }
            guard.value = Some(value.clone());
            guard.cursor += 1;
            if retired {
                guard.retired = true;
            }
            let frame = ReplicatedStateSourceFrame {
                cursor: guard.cursor,
                value,
                ops,
                context,
            };
            let targets: Vec<Arc<SessionSourceAttachmentInner<T>>> = guard.attachments.values().cloned().collect();
            (frame, targets)
        };
        for attachment in targets {
            attachment.publish(frame.clone());
        }
    }

    pub fn close_session(&self) {
        let targets: Vec<Arc<SessionSourceAttachmentInner<T>>> = {
            let guard = lock(&self.inner.state);
            if guard.closed {
                return;
            }
            guard.attachments.values().cloned().collect()
        };
        for attachment in targets {
            attachment.dispose();
        }
        self.inner.finish_disposal();
    }
}

impl<T: Clone + Send + Sync + 'static> ReplicatedStateSource<T> for CommittedStateSource<T> {
    fn attach(&self) -> pi_js::Result<Box<dyn ReplicatedStateSourceAttachment<T> + Send + Sync>> {
        let mut guard = lock(&self.inner.state);
        if guard.closed {
            return Err(pi_js::Error::msg("State source is closed"));
        }
        let value = guard
            .value
            .clone()
            .ok_or_else(|| pi_js::Error::msg("State source is closed"))?;
        let snapshot = ReplicatedStateSourceSnapshot {
            value,
            cursor: guard.cursor,
        };
        let id = guard.next_attachment_id;
        guard.next_attachment_id += 1;
        let weak_source = Arc::downgrade(&self.inner);
        let attachment_inner = Arc::new(SessionSourceAttachmentInner::new(move || {
            if let Some(src) = weak_source.upgrade() {
                src.remove_attachment(id);
            }
        }));
        guard.attachments.insert(id, Arc::clone(&attachment_inner));
        Ok(Box::new(SessionSourceAttachment {
            snapshot,
            inner: attachment_inner,
        }))
    }
}

struct AttachmentState<T> {
    release: Option<Box<dyn FnOnce() + Send + Sync>>,
    frames: VecDeque<ReplicatedStateSourceFrame<T>>,
    listener: Option<FrameListener<T>>,
    activated: bool,
    disposed: bool,
    scheduled: bool,
    delivering: bool,
}

struct SessionSourceAttachmentInner<T> {
    state: Mutex<AttachmentState<T>>,
}

struct DeliveringReset<'a, T> {
    state: &'a Mutex<AttachmentState<T>>,
    armed: bool,
}

impl<T> Drop for DeliveringReset<'_, T> {
    fn drop(&mut self) {
        if self.armed {
            lock(self.state).delivering = false;
        }
    }
}

impl<T: Send + Sync + 'static> SessionSourceAttachmentInner<T> {
    fn new(release: impl FnOnce() + Send + Sync + 'static) -> Self {
        Self {
            state: Mutex::new(AttachmentState {
                release: Some(Box::new(release)),
                frames: VecDeque::new(),
                listener: None,
                activated: false,
                disposed: false,
                scheduled: false,
                delivering: false,
            }),
        }
    }

    fn activate(self: &Arc<Self>, listener: FrameListener<T>) -> pi_js::Result<()> {
        {
            let mut guard = lock(&self.state);
            if guard.activated {
                return Err(pi_js::Error::msg("State attachment is already active"));
            }
            if guard.disposed {
                return Err(pi_js::Error::msg("State attachment is disposed"));
            }
            guard.activated = true;
            guard.listener = Some(listener);
        }
        self.drain()
    }

    fn publish(self: &Arc<Self>, frame: ReplicatedStateSourceFrame<T>) {
        let should_schedule = {
            let mut guard = lock(&self.state);
            if guard.disposed {
                return;
            }
            guard.frames.push_back(frame);
            if !guard.activated || guard.delivering || guard.scheduled {
                return;
            }
            guard.scheduled = true;
            true
        };
        if !should_schedule {
            return;
        }
        let this = Arc::clone(self);
        let run = move || {
            {
                let mut guard = lock(&this.state);
                guard.scheduled = false;
                if guard.disposed {
                    return;
                }
            }
            if this.drain().is_err() {
                // Chord's frame listener contains source-contract failures. Isolate an
                // unexpected direct listener failure to this attachment as well.
                this.dispose();
            }
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                run();
            });
        } else {
            run();
        }
    }

    fn dispose(&self) {
        let release = {
            let mut guard = lock(&self.state);
            if guard.disposed {
                return;
            }
            guard.disposed = true;
            guard.frames.clear();
            guard.listener = None;
            guard.release.take()
        };
        if let Some(release) = release {
            release();
        }
    }

    fn drain(&self) -> pi_js::Result<()> {
        {
            let mut guard = lock(&self.state);
            if guard.listener.is_none() || guard.delivering || guard.disposed {
                return Ok(());
            }
            guard.delivering = true;
        }
        let mut reset = DeliveringReset {
            state: &self.state,
            armed: true,
        };
        loop {
            let next = {
                let mut guard = lock(&self.state);
                if guard.disposed {
                    guard.delivering = false;
                    reset.armed = false;
                    return Ok(());
                }
                let Some(listener) = guard.listener.clone() else {
                    guard.delivering = false;
                    reset.armed = false;
                    return Ok(());
                };
                match guard.frames.pop_front() {
                    Some(frame) => (listener, frame),
                    None => {
                        guard.delivering = false;
                        reset.armed = false;
                        return Ok(());
                    }
                }
            };
            next.0(next.1)?;
        }
    }
}

struct SessionSourceAttachment<T> {
    snapshot: ReplicatedStateSourceSnapshot<T>,
    inner: Arc<SessionSourceAttachmentInner<T>>,
}

impl<T: Send + Sync + 'static> ReplicatedStateSourceAttachment<T> for SessionSourceAttachment<T> {
    fn snapshot(&self) -> &ReplicatedStateSourceSnapshot<T> {
        &self.snapshot
    }

    fn activate(&self, listener: FrameListener<T>) -> pi_js::Result<()> {
        self.inner.activate(listener)
    }

    fn dispose(&self) -> pi_js::Result<()> {
        self.inner.dispose();
        Ok(())
    }
}

struct WatchFrame<T> {
    value: T,
    ops: Vec<Op>,
    context: Context,
}

struct WatchState<T> {
    value: T,
    detach: Option<Box<dyn FnOnce() + Send + Sync>>,
    pending: VecDeque<WatchFrame<T>>,
    listener: Option<WatchListener<T>>,
    started: bool,
    scheduled: bool,
    running: bool,
    detached: bool,
    retired: bool,
    end: Option<WatchEnd>,
    cancellation_installed: bool,
    cancellation_guard: Option<ListenerGuard>,
}

struct WatchRunningReset<'a, T> {
    state: &'a Mutex<WatchState<T>>,
    armed: bool,
}

impl<T> Drop for WatchRunningReset<'_, T> {
    fn drop(&mut self) {
        if self.armed {
            lock(self.state).running = false;
        }
    }
}

/// Serialized exact-frame watch bound to one document incarnation or conversation view.
pub struct CommittedWatch<T> {
    self_weak: Weak<Self>,
    is_retired: fn(&T) -> bool,
    to_json: fn(&T) -> JsonValue,
    replace: Option<Box<dyn Fn() -> T + Send + Sync>>,
    closed_tx: watch::Sender<Option<WatchEnd>>,
    _closed_rx: watch::Receiver<Option<WatchEnd>>,
    state: Mutex<WatchState<T>>,
}

impl CommittedWatch<ObservedDocumentValue> {
    pub fn for_document(
        value: ObservedDocumentValue,
        detach: impl FnOnce() + Send + Sync + 'static,
        replace: Option<Box<dyn Fn() -> ObservedDocumentValue + Send + Sync>>,
    ) -> Arc<Self> {
        Self::new_arc(
            value,
            |v| v.is_none(),
            |v| match v {
                None => JsonValue::Null,
                Some(obj) => JsonValue::Object(obj.clone()),
            },
            Box::new(detach),
            replace,
        )
    }
}

impl<T: Clone + Send + Sync + 'static> CommittedWatch<T> {
    /// `replace` gives the value an overflow delivers; by default the newest value.
    pub fn new_arc(
        value: T,
        is_retired: fn(&T) -> bool,
        to_json: fn(&T) -> JsonValue,
        detach: Box<dyn FnOnce() + Send + Sync>,
        replace: Option<Box<dyn Fn() -> T + Send + Sync>>,
    ) -> Arc<Self> {
        let (closed_tx, closed_rx) = watch::channel(None);
        Arc::new_cyclic(|weak| Self {
            self_weak: weak.clone(),
            is_retired,
            to_json,
            replace,
            closed_tx,
            _closed_rx: closed_rx,
            state: Mutex::new(WatchState {
                value,
                detach: Some(detach),
                pending: VecDeque::new(),
                listener: None,
                started: false,
                scheduled: false,
                running: false,
                detached: false,
                retired: false,
                end: None,
                cancellation_installed: false,
                cancellation_guard: None,
            }),
        })
    }

    pub fn into_handle(self: Arc<Self>) -> WatchHandle<T> {
        WatchHandle::from_inner(self)
    }

    pub fn value(&self) -> T {
        lock(&self.state).value.clone()
    }

    pub async fn closed(&self) -> WatchEnd {
        let mut rx = self.closed_tx.subscribe();
        loop {
            if let Some(end) = rx.borrow_and_update().clone() {
                return end;
            }
            if rx.changed().await.is_err() {
                return lock(&self.state).end.clone().unwrap_or(WatchEnd::Stopped);
            }
        }
    }

    pub fn start(&self, listener: WatchListener<T>) -> pi_js::Result<()> {
        let should_schedule = {
            let mut guard = lock(&self.state);
            if guard.started {
                return Err(pi_js::Error::msg("Watch is already started"));
            }
            if guard.end.is_some() {
                return Err(pi_js::Error::msg("Watch is stopped"));
            }
            guard.started = true;
            guard.listener = Some(listener);
            !guard.pending.is_empty() && !guard.scheduled && !guard.running
        };
        if should_schedule {
            self.schedule();
        }
        Ok(())
    }

    pub async fn stop(&self) -> WatchEnd {
        self.terminate(WatchEnd::Stopped);
        self.closed().await
    }

    pub fn observe_cancellation(&self, signal: &AbortSignal) -> pi_js::Result<()> {
        let weak = self.self_weak.clone();
        {
            let mut guard = lock(&self.state);
            if guard.cancellation_installed {
                return Err(pi_js::Error::msg("Watch cancellation is already installed"));
            }
            if guard.end.is_some() {
                return Ok(());
            }
            guard.cancellation_installed = true;
            let guard_listener = signal.on_abort(move |_| {
                if let Some(watch) = weak.upgrade() {
                    watch.cancel();
                }
            });
            guard.cancellation_guard = Some(guard_listener);
        }
        if signal.aborted() {
            self.cancel();
        }
        Ok(())
    }

    pub fn cancel(&self) {
        self.terminate(WatchEnd::Cancelled);
    }

    pub fn close_session(&self) {
        self.terminate(WatchEnd::SessionClosed);
    }

    pub fn advance(&self, value: T, ops: Vec<Op>, context: Context) {
        let retired = (self.is_retired)(&value);
        let should_schedule = {
            let mut guard = lock(&self.state);
            if guard.end.is_some() || guard.retired {
                return;
            }
            if retired {
                guard.retired = true;
            }
            if guard.pending.len() >= MAX_PENDING_WATCH_FRAMES {
                guard.pending.clear();
                let replacement = match &self.replace {
                    Some(replace) => replace(),
                    None => value,
                };
                let replacement_json = (self.to_json)(&replacement);
                guard.pending.push_back(WatchFrame {
                    value: replacement,
                    ops: vec![Op::Replace(OpReplace(ReplaceVerb::R, replacement_json))],
                    context,
                });
            } else {
                guard.pending.push_back(WatchFrame { value, ops, context });
            }
            guard.started && !guard.scheduled && !guard.running && guard.end.is_none()
        };
        if should_schedule {
            self.schedule();
        }
    }

    fn schedule(&self) {
        let Some(this) = self.self_weak.upgrade() else {
            return;
        };
        {
            let mut guard = lock(&self.state);
            if guard.scheduled || guard.running || guard.end.is_some() {
                return;
            }
            guard.scheduled = true;
        }
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                {
                    lock(&this.state).scheduled = false;
                }
                this.drain().await;
            });
        } else {
            lock(&self.state).scheduled = false;
        }
    }

    async fn drain(&self) {
        {
            let mut guard = lock(&self.state);
            if guard.running || guard.end.is_some() || !guard.started {
                return;
            }
            guard.running = true;
        }
        let mut reset = WatchRunningReset {
            state: &self.state,
            armed: true,
        };
        loop {
            let (listener, frame) = {
                let mut guard = lock(&self.state);
                if guard.end.is_some() {
                    guard.running = false;
                    reset.armed = false;
                    break;
                }
                let Some(listener) = guard.listener.clone() else {
                    guard.running = false;
                    reset.armed = false;
                    break;
                };
                let Some(frame) = guard.pending.pop_front() else {
                    guard.running = false;
                    reset.armed = false;
                    break;
                };
                guard.value = frame.value.clone();
                (listener, frame)
            };

            let delivery_context = without_abort_signal(frame.context);
            let is_retired = (self.is_retired)(&frame.value);
            match listener(frame.value, frame.ops, delivery_context).await {
                Ok(()) => {
                    if is_retired {
                        self.terminate(WatchEnd::Retired);
                        let mut guard = lock(&self.state);
                        guard.running = false;
                        reset.armed = false;
                        break;
                    }
                }
                Err(error) => {
                    self.terminate(WatchEnd::ListenerError { error: Arc::new(error) });
                    let mut guard = lock(&self.state);
                    guard.running = false;
                    reset.armed = false;
                    break;
                }
            }
        }
        if reset.armed {
            lock(&self.state).running = false;
            reset.armed = false;
        }
    }

    fn terminate(&self, end: WatchEnd) {
        let (detach, cancel_guard, resolved_end) = {
            let mut guard = lock(&self.state);
            if guard.end.is_some() {
                return;
            }
            guard.end = Some(end.clone());
            guard.pending.clear();
            guard.listener = None;
            let detach = if guard.detached {
                None
            } else {
                guard.detached = true;
                guard.detach.take()
            };
            let cancel_guard = guard.cancellation_guard.take();
            (detach, cancel_guard, end)
        };
        drop(cancel_guard);
        if let Some(detach) = detach {
            detach();
        }
        let _ = self.closed_tx.send_replace(Some(resolved_end));
    }
}

#[async_trait]
impl<T: Clone + Send + Sync + 'static> WatchHandleApi<T> for CommittedWatch<T> {
    fn value(&self) -> T {
        CommittedWatch::value(self)
    }

    fn start(&self, listener: WatchListener<T>) -> pi_js::Result<()> {
        CommittedWatch::start(self, listener)
    }

    fn stop_now(&self) {
        CommittedWatch::terminate(self, WatchEnd::Stopped);
    }

    async fn stop(&self) -> WatchEnd {
        CommittedWatch::stop(self).await
    }

    async fn closed(&self) -> WatchEnd {
        CommittedWatch::closed(self).await
    }
}
