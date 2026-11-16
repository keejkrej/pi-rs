//! Port of packages/ai/src/utils/event-stream.ts

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use futures::Stream;

use crate::types::{AssistantMessage, AssistantMessageEvent, StopReason, Usage, UsageCost};

struct State<T, R> {
    queue: VecDeque<T>,
    done: bool,
    final_result: Option<R>,
    ended_without_result: bool,
    stream_wakers: Vec<Waker>,
    result_wakers: Vec<Waker>,
}

struct Inner<T, R> {
    state: Mutex<State<T, R>>,
    is_complete: Box<dyn Fn(&T) -> bool + Send + Sync>,
    extract_result: Box<dyn Fn(&T) -> R + Send + Sync>,
}

/// Async event stream paired with a resolved final result future.
pub struct EventStream<T, R = T> {
    inner: Arc<Inner<T, R>>,
}

impl<T, R> Clone for EventStream<T, R> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T, R> EventStream<T, R> {
    pub fn new(
        is_complete: impl Fn(&T) -> bool + Send + Sync + 'static,
        extract_result: impl Fn(&T) -> R + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    queue: VecDeque::new(),
                    done: false,
                    final_result: None,
                    ended_without_result: false,
                    stream_wakers: Vec::new(),
                    result_wakers: Vec::new(),
                }),
                is_complete: Box::new(is_complete),
                extract_result: Box::new(extract_result),
            }),
        }
    }

    pub fn push(&self, event: T) {
        let complete = (self.inner.is_complete)(&event);
        let extracted = if complete {
            Some((self.inner.extract_result)(&event))
        } else {
            None
        };

        let (stream_wakers, result_wakers) = {
            let mut state = self.inner.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.done {
                return;
            }
            if let Some(res) = extracted {
                state.done = true;
                if state.final_result.is_none() {
                    state.final_result = Some(res);
                }
            }
            state.queue.push_back(event);
            let sw = std::mem::take(&mut state.stream_wakers);
            let rw = if state.final_result.is_some() {
                std::mem::take(&mut state.result_wakers)
            } else {
                Vec::new()
            };
            (sw, rw)
        };

        for w in stream_wakers {
            w.wake();
        }
        for w in result_wakers {
            w.wake();
        }
    }

    pub fn end(&self, result: Option<R>) {
        let (stream_wakers, result_wakers) = {
            let mut state = self.inner.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            state.done = true;
            if let Some(res) = result {
                if state.final_result.is_none() {
                    state.final_result = Some(res);
                }
            } else if state.final_result.is_none() {
                state.ended_without_result = true;
            }
            (
                std::mem::take(&mut state.stream_wakers),
                std::mem::take(&mut state.result_wakers),
            )
        };

        for w in stream_wakers {
            w.wake();
        }
        for w in result_wakers {
            w.wake();
        }
    }

    pub fn result(&self) -> EventStreamResult<T, R>
    where
        R: Clone,
    {
        EventStreamResult {
            inner: Arc::clone(&self.inner),
        }
    }

    pub fn try_result(&self) -> Option<R>
    where
        R: Clone,
    {
        let state = self.inner.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.final_result.clone()
    }
}

impl<T, R> Stream for EventStream<T, R> {
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut state = self.inner.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(item) = state.queue.pop_front() {
            return Poll::Ready(Some(item));
        }
        if state.done {
            return Poll::Ready(None);
        }
        state.stream_wakers.push(cx.waker().clone());
        Poll::Pending
    }
}

pub struct EventStreamResult<T, R> {
    inner: Arc<Inner<T, R>>,
}

impl<T, R: Clone> Future for EventStreamResult<T, R> {
    type Output = Option<R>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.inner.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(res) = &state.final_result {
            return Poll::Ready(Some(res.clone()));
        }
        if state.ended_without_result {
            return Poll::Ready(None);
        }
        state.result_wakers.push(cx.waker().clone());
        Poll::Pending
    }
}

/// Specialized [`EventStream`] carrying [`AssistantMessageEvent`] values and settling to
/// an [`AssistantMessage`].
#[derive(Clone)]
pub struct AssistantMessageEventStream {
    inner: EventStream<AssistantMessageEvent, AssistantMessage>,
}

impl AssistantMessageEventStream {
    pub fn new() -> Self {
        Self {
            inner: EventStream::new(
                |event| {
                    matches!(
                        event,
                        AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
                    )
                },
                |event| match event {
                    AssistantMessageEvent::Done { message, .. } => message.clone(),
                    AssistantMessageEvent::Error { error, .. } => error.clone(),
                    _ => fallback_unexpected_result_message("Unexpected event type for final result"),
                },
            ),
        }
    }

    pub fn push(&self, event: AssistantMessageEvent) {
        self.inner.push(event);
    }

    pub fn end(&self, result: Option<AssistantMessage>) {
        self.inner.end(result);
    }

    pub async fn result(&self) -> AssistantMessage {
        self.inner
            .result()
            .await
            .unwrap_or_else(|| fallback_unexpected_result_message("Stream ended without a final message"))
    }

    pub fn try_result(&self) -> Option<AssistantMessage> {
        self.inner.try_result()
    }
}

impl Default for AssistantMessageEventStream {
    fn default() -> Self {
        Self::new()
    }
}

impl Stream for AssistantMessageEventStream {
    type Item = AssistantMessageEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

fn fallback_unexpected_result_message(reason: &str) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: String::new(),
        provider: String::new(),
        model: String::new(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 0,
            cost: UsageCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                total: 0.0,
            },
        },
        stop_reason: StopReason::Error,
        deferred: None,
        error_message: Some(reason.to_string()),
        raw_stop_reason: None,
        end_turn: None,
        timestamp: pi_js::time::now_ms(),
    }
}

/// Factory function for `AssistantMessageEventStream` (for use in extensions).
pub fn create_assistant_message_event_stream() -> AssistantMessageEventStream {
    AssistantMessageEventStream::new()
}
