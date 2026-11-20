//! Port of packages/ai/src/api/lazy.ts

use std::future::Future;
use std::sync::Arc;

use futures::StreamExt;
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Error, Result};

use crate::types::{AssistantMessage, AssistantMessageEvent, Model, ProviderStreams, StopReason, Usage};
use crate::utils::event_stream::AssistantMessageEventStream;

fn create_setup_error_message(
    model: &Model,
    error: &Error,
    timestamp: i64,
    stop_reason: StopReason,
) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason,
        deferred: None,
        error_message: Some(error.to_string()),
        raw_stop_reason: None,
        end_turn: None,
        timestamp,
        duration_ms: None,
    }
}

/// Synchronous, read-only observation of `lazy_stream_with_options`'s forwarding path.
#[derive(Clone, Default)]
pub struct LazyStreamObserver {
    pub on_event: Option<Arc<dyn Fn(&AssistantMessageEvent) + Send + Sync>>,
    pub on_result: Option<Arc<dyn Fn(Option<&AssistantMessage>) + Send + Sync>>,
}

async fn forward_stream<F>(
    target: AssistantMessageEventStream,
    mut source: AssistantMessageEventStream,
    observer: Option<&LazyStreamObserver>,
    observe_result: &mut F,
) where
    F: FnMut(Option<&AssistantMessage>),
{
    let mut result: Option<AssistantMessage> = None;
    while let Some(event) = source.next().await {
        match &event {
            AssistantMessageEvent::Done { message, .. } => result = Some(message.clone()),
            AssistantMessageEvent::Error { error, .. } => result = Some(error.clone()),
            _ => {}
        }
        target.push(event.clone());
        if let Some(cb) = observer.and_then(|o| o.on_event.as_ref()) {
            cb(&event);
        }
    }
    if let Some(src_result) = source.try_result() {
        result = Some(src_result);
    }
    target.end(result.clone());
    observe_result(result.as_ref());
}

/// Returns a stream synchronously while running async setup (auth resolution,
/// lazy module loading) behind it. Setup failures terminate the stream with an
/// error event.
pub fn lazy_stream<F>(model: &Model, setup: F) -> AssistantMessageEventStream
where
    F: Future<Output = Result<AssistantMessageEventStream>> + Send + 'static,
{
    lazy_stream_with_options(model, setup, None, None)
}

/// Variant of [`lazy_stream`] accepting an optional observer and abort signal so setup failures
/// occurring after cancellation terminate with [`StopReason::Aborted`].
pub fn lazy_stream_with_options<F>(
    model: &Model,
    setup: F,
    observer: Option<LazyStreamObserver>,
    signal: Option<AbortSignal>,
) -> AssistantMessageEventStream
where
    F: Future<Output = Result<AssistantMessageEventStream>> + Send + 'static,
{
    let started_at = pi_js::time::now_ms();
    let outer = AssistantMessageEventStream::new();
    let outer_clone = outer.clone();
    let model_owned = model.clone();

    tokio::spawn(async move {
        let mut observed_result = false;
        let mut observe_result = |result: Option<&AssistantMessage>| {
            if observed_result {
                return;
            }
            observed_result = true;
            if let Some(cb) = observer.as_ref().and_then(|o| o.on_result.as_ref()) {
                cb(result);
            }
        };

        match setup.await {
            Ok(inner) => {
                forward_stream(outer_clone, inner, observer.as_ref(), &mut observe_result).await;
            }
            Err(error) => {
                let reason = if signal.as_ref().is_some_and(AbortSignal::aborted) {
                    StopReason::Aborted
                } else {
                    StopReason::Error
                };
                let message = create_setup_error_message(&model_owned, &error, started_at, reason);
                let event = AssistantMessageEvent::Error {
                    reason,
                    error: message.clone(),
                };
                outer_clone.push(event.clone());
                if let Some(cb) = observer.as_ref().and_then(|o| o.on_event.as_ref()) {
                    cb(&event);
                }
                outer_clone.end(Some(message.clone()));
                observe_result(Some(&message));
            }
        }
    });

    outer
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LazyApiCapabilities {
    pub fetch_deferred: bool,
    pub cancel_deferred: bool,
}

pub type LoadProviderStreams = Arc<dyn Fn() -> BoxFuture<Result<ProviderStreams>> + Send + Sync>;

/// Wraps a dynamically loaded API implementation provider as `ProviderStreams`.
/// Load failures terminate the returned stream with an error event.
pub fn lazy_api(load: LoadProviderStreams, capabilities: Option<LazyApiCapabilities>) -> ProviderStreams {
    let caps = capabilities.unwrap_or_default();

    let load_for_stream = Arc::clone(&load);
    let stream: crate::types::StreamFunction = Arc::new(move |model, context, options| {
        let load_fn = Arc::clone(&load_for_stream);
        let model_owned = model.clone();
        let context_owned = context.clone();
        let signal = options.as_ref().and_then(|o| o.base.signal.clone());
        lazy_stream_with_options(
            model,
            async move {
                let api = load_fn().await?;
                Ok(api.stream(&model_owned, &context_owned, options))
            },
            None,
            signal,
        )
    });

    let load_for_simple = Arc::clone(&load);
    let stream_simple: crate::types::SimpleStreamFunction = Arc::new(move |model, context, options| {
        let load_fn = Arc::clone(&load_for_simple);
        let model_owned = model.clone();
        let context_owned = context.clone();
        let signal = options.as_ref().and_then(|o| o.base.signal.clone());
        lazy_stream_with_options(
            model,
            async move {
                let api = load_fn().await?;
                Ok(api.stream_simple(&model_owned, &context_owned, options))
            },
            None,
            signal,
        )
    });

    let fetch_deferred = if caps.fetch_deferred {
        let load_for_fetch = Arc::clone(&load);
        Some(Arc::new(
            move |model: &Model,
                  handle: &crate::types::DeferredHandle,
                  options: Option<crate::types::DeferredFetchOptions>| {
                let load_fn = Arc::clone(&load_for_fetch);
                let model_owned = model.clone();
                let handle_owned = handle.clone();
                let signal = options.as_ref().and_then(|o| o.base.signal.clone());
                lazy_stream_with_options(
                    model,
                    async move {
                        let api = load_fn().await?;
                        let fetch_fn = api
                            .fetch_deferred
                            .as_ref()
                            .ok_or_else(|| Error::msg("API does not support deferred responses"))?;
                        Ok(fetch_fn(&model_owned, &handle_owned, options))
                    },
                    None,
                    signal,
                )
            },
        ) as crate::types::FetchDeferredFunction)
    } else {
        None
    };

    let cancel_deferred = if caps.cancel_deferred {
        let load_for_cancel = Arc::clone(&load);
        Some(Arc::new(
            move |model: &Model,
                  handle: &crate::types::DeferredHandle,
                  options: Option<crate::types::DeferredCancelOptions>| {
                let load_fn = Arc::clone(&load_for_cancel);
                let model_owned = model.clone();
                let handle_owned = handle.clone();
                Box::pin(async move {
                    let api = load_fn().await?;
                    let cancel_fn = api
                        .cancel_deferred
                        .as_ref()
                        .ok_or_else(|| Error::msg("API cannot cancel deferred responses"))?;
                    cancel_fn(&model_owned, &handle_owned, options).await
                }) as BoxFuture<Result<()>>
            },
        ) as crate::types::CancelDeferredFunction)
    } else {
        None
    };

    ProviderStreams {
        stream,
        stream_simple,
        fetch_deferred,
        cancel_deferred,
    }
}
