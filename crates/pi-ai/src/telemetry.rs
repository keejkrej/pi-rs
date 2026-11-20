//! Port of packages/ai/src/telemetry.ts

use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

use indexmap::IndexMap;
use pi_telemetry::{
    AttributeValue, NOOP_TELEMETRY_CONTEXT, NOOP_TELEMETRY_METRICS, SpanAttributes, SpanCallback, SpanOptions,
    SpanStatus, SpanStatusError, SpanValue, TelemetryAttributeCardinality, TelemetryAttributeDefinition,
    TelemetryContext, TelemetryEventDefinition, TelemetryMetricDefinition, TelemetryMetrics, TelemetryParentDefinition,
    TelemetrySchemaDefinition, TelemetrySpan, TelemetrySpanDefinition, TelemetrySpanStatusDefault,
    TelemetrySpanStatusDefinition, TelemetryStartAttributeDefinition, create_typed_metrics, define_telemetry_schema,
};
use tokio::sync::Notify;

use crate::types::{
    AssistantMessage, AssistantMessageEvent, Model, StopReason, TelemetryContextHandle, TelemetrySpanHandle,
    ThinkingLevel,
};
use crate::utils::text::owned_slice;

fn req_str_attr(description: &str, required: bool, values: Option<&[&str]>) -> TelemetryStartAttributeDefinition {
    TelemetryStartAttributeDefinition::String {
        required,
        values: values.map(|v| v.iter().map(|s| (*s).to_owned()).collect()),
        examples: None,
        description: description.to_owned(),
        sensitive: None,
        cardinality: None,
    }
}

fn req_num_attr(description: &str, required: bool) -> TelemetryStartAttributeDefinition {
    TelemetryStartAttributeDefinition::Number {
        required,
        values: None,
        examples: None,
        description: description.to_owned(),
        sensitive: None,
        cardinality: None,
    }
}

fn opt_str_attr(
    description: &str,
    values: Option<&[&str]>,
    cardinality: Option<TelemetryAttributeCardinality>,
) -> TelemetryAttributeDefinition {
    TelemetryAttributeDefinition::String {
        values: values.map(|v| v.iter().map(|s| (*s).to_owned()).collect()),
        examples: None,
        description: description.to_owned(),
        sensitive: None,
        cardinality,
    }
}

fn opt_num_attr(description: &str) -> TelemetryAttributeDefinition {
    TelemetryAttributeDefinition::Number {
        values: None,
        examples: None,
        description: description.to_owned(),
        sensitive: None,
        cardinality: None,
    }
}

const STOP_VALUES: &[&str] = &["pending", "stop", "length", "toolUse", "error", "aborted", "deferred"];

fn build_request_span_def(description: &str) -> TelemetrySpanDefinition {
    let mut start_attributes = IndexMap::new();
    start_attributes.insert("provider".to_owned(), req_str_attr("Provider identifier.", true, None));
    start_attributes.insert("api".to_owned(), req_str_attr("API identifier.", true, None));
    start_attributes.insert(
        "model".to_owned(),
        req_str_attr("Requested model identifier.", true, None),
    );
    start_attributes.insert(
        "reasoning".to_owned(),
        req_str_attr("Scalar requested reasoning level.", false, None),
    );

    let mut end_attributes = IndexMap::new();
    end_attributes.insert(
        "stop".to_owned(),
        opt_str_attr("Normalized stop reason.", Some(STOP_VALUES), None),
    );
    end_attributes.insert("usage.input".to_owned(), opt_num_attr("Uncached input tokens."));
    end_attributes.insert("usage.output".to_owned(), opt_num_attr("Output tokens."));
    end_attributes.insert("usage.cache_read".to_owned(), opt_num_attr("Cache-read tokens."));
    end_attributes.insert("usage.cache_write".to_owned(), opt_num_attr("Cache-write tokens."));
    end_attributes.insert("cost.total".to_owned(), opt_num_attr("Total request cost in USD."));
    end_attributes.insert(
        "response.id".to_owned(),
        opt_str_attr(
            "Provider response identifier.",
            None,
            Some(TelemetryAttributeCardinality::High),
        ),
    );

    let mut block_attrs = IndexMap::new();
    block_attrs.insert(
        "type".to_owned(),
        req_str_attr("Block kind.", true, Some(&["text", "thinking", "tool_call"])),
    );
    block_attrs.insert("index".to_owned(), req_num_attr("Content block index.", true));
    block_attrs.insert("chars".to_owned(), req_num_attr("UTF-16 character count.", true));
    block_attrs.insert(
        "tool".to_owned(),
        req_str_attr("Tool name, for tool-call blocks only.", false, None),
    );

    let mut events = IndexMap::new();
    events.insert(
        "pi.ai.first_chunk".to_owned(),
        TelemetryEventDefinition {
            description: "First normalized stream event delivered.".to_owned(),
            attributes: IndexMap::new(),
        },
    );
    events.insert(
        "pi.ai.block".to_owned(),
        TelemetryEventDefinition {
            description: "A completed content block; content is never recorded.".to_owned(),
            attributes: block_attrs,
        },
    );

    TelemetrySpanDefinition {
        description: description.to_owned(),
        parents: TelemetryParentDefinition::Any,
        start_attributes,
        end_attributes,
        events: Some(events),
        status: TelemetrySpanStatusDefinition {
            r#default: TelemetrySpanStatusDefault::Ok,
            error_when:
                "Stop reason is error: provider_error with at most 500 error-message characters. Aborted stays ok."
                    .to_owned(),
        },
    }
}

/// Pi-owned request metadata, bounded error status, events, and low-cardinality metrics.
pub static AI_TELEMETRY_SCHEMA: LazyLock<TelemetrySchemaDefinition> = LazyLock::new(|| {
    let mut spans = IndexMap::new();
    spans.insert(
        "pi.ai.request".to_owned(),
        build_request_span_def("One logical model request, including authentication and provider setup."),
    );
    spans.insert(
        "pi.ai.poll".to_owned(),
        build_request_span_def("One deferred-response poll, not the deferred request's lifetime."),
    );

    let mut req_dur_attrs = IndexMap::new();
    req_dur_attrs.insert("provider".to_owned(), req_str_attr("Provider identifier.", true, None));
    req_dur_attrs.insert(
        "model".to_owned(),
        req_str_attr("Requested model identifier.", true, None),
    );
    req_dur_attrs.insert(
        "stop".to_owned(),
        req_str_attr("Normalized stop reason.", true, Some(STOP_VALUES)),
    );

    let mut first_chunk_attrs = IndexMap::new();
    first_chunk_attrs.insert("provider".to_owned(), req_str_attr("Provider identifier.", true, None));
    first_chunk_attrs.insert(
        "model".to_owned(),
        req_str_attr("Requested model identifier.", true, None),
    );

    let mut token_attrs = IndexMap::new();
    token_attrs.insert("provider".to_owned(), req_str_attr("Provider identifier.", true, None));
    token_attrs.insert(
        "model".to_owned(),
        req_str_attr("Requested model identifier.", true, None),
    );
    token_attrs.insert(
        "kind".to_owned(),
        req_str_attr(
            "Token usage category.",
            true,
            Some(&["input", "output", "cache_read", "cache_write"]),
        ),
    );

    let mut cost_attrs = IndexMap::new();
    cost_attrs.insert("provider".to_owned(), req_str_attr("Provider identifier.", true, None));
    cost_attrs.insert(
        "model".to_owned(),
        req_str_attr("Requested model identifier.", true, None),
    );

    let mut metrics = IndexMap::new();
    metrics.insert(
        "pi.ai.request.duration".to_owned(),
        TelemetryMetricDefinition::Histogram {
            description: "Logical request or poll duration.".to_owned(),
            unit: Some("ms".to_owned()),
            boundaries: None,
            attributes: req_dur_attrs,
        },
    );
    metrics.insert(
        "pi.ai.first_chunk.duration".to_owned(),
        TelemetryMetricDefinition::Histogram {
            description: "Time to the first normalized stream event.".to_owned(),
            unit: Some("ms".to_owned()),
            boundaries: None,
            attributes: first_chunk_attrs,
        },
    );
    metrics.insert(
        "pi.ai.tokens".to_owned(),
        TelemetryMetricDefinition::Counter {
            description: "Reported model token usage; pending and deferred polls are excluded.".to_owned(),
            unit: Some("token".to_owned()),
            attributes: token_attrs,
        },
    );
    metrics.insert(
        "pi.ai.cost".to_owned(),
        TelemetryMetricDefinition::Counter {
            description: "Reported model cost; pending and deferred polls are excluded.".to_owned(),
            unit: Some("USD".to_owned()),
            attributes: cost_attrs,
        },
    );

    define_telemetry_schema(TelemetrySchemaDefinition {
        version: 1,
        spans,
        metrics: Some(metrics),
    })
});

fn stop_reason_str(reason: StopReason) -> &'static str {
    match reason {
        StopReason::Pending => "pending",
        StopReason::Stop => "stop",
        StopReason::Length => "length",
        StopReason::ToolUse => "toolUse",
        StopReason::Error => "error",
        StopReason::Aborted => "aborted",
        StopReason::Deferred => "deferred",
    }
}

fn thinking_level_str(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::XHigh => "xhigh",
        ThinkingLevel::Max => "max",
    }
}

enum DeferredSpanOp {
    AddEvent {
        name: String,
        attrs: Option<SpanAttributes>,
    },
    FirstChunkDuration {
        ms: f64,
        attrs: SpanAttributes,
    },
    RequestDuration {
        ms: f64,
        attrs: SpanAttributes,
    },
    SetAttributes(SpanAttributes),
    SetStatus(SpanStatus),
}

struct ObserverInner {
    span: Option<Arc<dyn TelemetrySpan + Send + Sync>>,
    pending_ops: Vec<DeferredSpanOp>,
    first_chunk_recorded: bool,
    tool_chars: IndexMap<usize, usize>,
    observed_result: bool,
}

/// Synchronous, read-only observer for one model request stream (`v1.5.2` `LazyStreamObserver`).
#[derive(Clone)]
pub struct ModelRequestObserver {
    started: Instant,
    provider: String,
    model_id: String,
    poll: bool,
    request_duration: pi_telemetry::SchemaTelemetryHistogram,
    first_chunk_duration: pi_telemetry::SchemaTelemetryHistogram,
    tokens: pi_telemetry::SchemaTelemetryCounter,
    cost: pi_telemetry::SchemaTelemetryCounter,
    inner: Arc<Mutex<ObserverInner>>,
    finish_notify: Arc<Notify>,
}

impl ModelRequestObserver {
    fn base_attrs(&self) -> SpanAttributes {
        let mut attrs = IndexMap::new();
        attrs.insert("provider".to_owned(), AttributeValue::String(self.provider.clone()));
        attrs.insert("model".to_owned(), AttributeValue::String(self.model_id.clone()));
        attrs
    }

    pub fn on_event(&self, event: &AssistantMessageEvent) {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if !guard.first_chunk_recorded {
            guard.first_chunk_recorded = true;
            let elapsed_ms = self.started.elapsed().as_secs_f64() * 1000.0;
            let base = self.base_attrs();
            if let Some(span) = &guard.span {
                span.add_event("pi.ai.first_chunk", None);
                self.first_chunk_duration.record(elapsed_ms, Some(base), Some(span));
            } else {
                guard.pending_ops.push(DeferredSpanOp::AddEvent {
                    name: "pi.ai.first_chunk".to_owned(),
                    attrs: None,
                });
                guard.pending_ops.push(DeferredSpanOp::FirstChunkDuration {
                    ms: elapsed_ms,
                    attrs: base,
                });
            }
        }

        let block_event: Option<(&str, usize, usize, Option<&str>)> = match event {
            AssistantMessageEvent::ToolCallDelta {
                content_index, delta, ..
            } => {
                let delta_len = pi_js::str16::len(delta);
                *guard.tool_chars.entry(*content_index).or_insert(0) += delta_len;
                None
            }
            AssistantMessageEvent::TextEnd {
                content_index, content, ..
            } => Some(("text", *content_index, pi_js::str16::len(content), None)),
            AssistantMessageEvent::ThinkingEnd {
                content_index, content, ..
            } => Some(("thinking", *content_index, pi_js::str16::len(content), None)),
            AssistantMessageEvent::ToolCallEnd {
                content_index,
                tool_call,
                ..
            } => {
                let chars = guard.tool_chars.shift_remove(content_index).unwrap_or(0);
                Some(("tool_call", *content_index, chars, Some(tool_call.name.as_str())))
            }
            _ => None,
        };

        if let Some((kind, index, chars, tool)) = block_event {
            let mut attrs = IndexMap::new();
            attrs.insert("type".to_owned(), AttributeValue::String(kind.to_owned()));
            attrs.insert("index".to_owned(), AttributeValue::Number(index as f64));
            attrs.insert("chars".to_owned(), AttributeValue::Number(chars as f64));
            if let Some(tool_name) = tool {
                attrs.insert("tool".to_owned(), AttributeValue::String(tool_name.to_owned()));
            }
            if let Some(span) = &guard.span {
                span.add_event("pi.ai.block", Some(attrs));
            } else {
                guard.pending_ops.push(DeferredSpanOp::AddEvent {
                    name: "pi.ai.block".to_owned(),
                    attrs: Some(attrs),
                });
            }
        }
    }

    pub fn on_result(&self, message: Option<&AssistantMessage>) {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if guard.observed_result {
            return;
        }
        guard.observed_result = true;

        if let Some(msg) = message {
            let stop = stop_reason_str(msg.stop_reason);
            let mut end_attrs = IndexMap::new();
            end_attrs.insert("stop".to_owned(), AttributeValue::String(stop.to_owned()));
            end_attrs.insert("usage.input".to_owned(), AttributeValue::Number(msg.usage.input as f64));
            end_attrs.insert(
                "usage.output".to_owned(),
                AttributeValue::Number(msg.usage.output as f64),
            );
            end_attrs.insert(
                "usage.cache_read".to_owned(),
                AttributeValue::Number(msg.usage.cache_read as f64),
            );
            end_attrs.insert(
                "usage.cache_write".to_owned(),
                AttributeValue::Number(msg.usage.cache_write as f64),
            );
            end_attrs.insert("cost.total".to_owned(), AttributeValue::Number(msg.usage.cost.total));
            if let Some(response_id) = &msg.response_id {
                end_attrs.insert("response.id".to_owned(), AttributeValue::String(response_id.clone()));
            }

            let err_status = if msg.stop_reason == StopReason::Error {
                Some(SpanStatus::Error {
                    error: Some(SpanStatusError {
                        name: "provider_error".to_owned(),
                        message: owned_slice(msg.error_message.as_deref().unwrap_or(""), 0, Some(500)),
                    }),
                })
            } else {
                None
            };

            let elapsed_ms = self.started.elapsed().as_secs_f64() * 1000.0;
            let mut dur_attrs = self.base_attrs();
            dur_attrs.insert("stop".to_owned(), AttributeValue::String(stop.to_owned()));

            if let Some(span) = &guard.span {
                span.set_attributes(end_attrs);
                if let Some(status) = err_status {
                    span.set_status(status);
                }
                self.request_duration.record(elapsed_ms, Some(dur_attrs), Some(span));
            } else {
                guard.pending_ops.push(DeferredSpanOp::SetAttributes(end_attrs));
                if let Some(status) = err_status {
                    guard.pending_ops.push(DeferredSpanOp::SetStatus(status));
                }
                guard.pending_ops.push(DeferredSpanOp::RequestDuration {
                    ms: elapsed_ms,
                    attrs: dur_attrs,
                });
            }

            if !self.poll || !matches!(msg.stop_reason, StopReason::Pending | StopReason::Deferred) {
                for (kind, value) in [
                    ("input", msg.usage.input),
                    ("output", msg.usage.output),
                    ("cache_read", msg.usage.cache_read),
                    ("cache_write", msg.usage.cache_write),
                ] {
                    let mut token_attrs = self.base_attrs();
                    token_attrs.insert("kind".to_owned(), AttributeValue::String(kind.to_owned()));
                    self.tokens.add(value as f64, Some(token_attrs));
                }
                self.cost.add(msg.usage.cost.total, Some(self.base_attrs()));
            }
        }

        drop(guard);
        self.finish_notify.notify_one();
    }
}

/// Start observing one model request or deferred poll under `telemetry` (falling back to `telemetry_context` or noop).
pub fn observe_model_request(
    requested_model: &Model,
    telemetry: Option<&TelemetrySpanHandle>,
    telemetry_context: Option<&TelemetryContextHandle>,
    reasoning: Option<ThinkingLevel>,
    poll: bool,
) -> ModelRequestObserver {
    let context: TelemetryContextHandle = telemetry
        .map(|s| Arc::clone(s) as TelemetryContextHandle)
        .or_else(|| telemetry_context.map(Arc::clone))
        .unwrap_or_else(|| Arc::new(NOOP_TELEMETRY_CONTEXT));

    let metrics_adapter: Arc<dyn TelemetryMetrics + Send + Sync> =
        context.metrics().unwrap_or_else(|| Arc::new(NOOP_TELEMETRY_METRICS));
    let typed = create_typed_metrics(metrics_adapter, std::slice::from_ref(&*AI_TELEMETRY_SCHEMA));

    let request_duration = typed.histogram("pi.ai.request.duration");
    let first_chunk_duration = typed.histogram("pi.ai.first_chunk.duration");
    let tokens = typed.counter("pi.ai.tokens");
    let cost = typed.counter("pi.ai.cost");

    let inner = Arc::new(Mutex::new(ObserverInner {
        span: None,
        pending_ops: Vec::new(),
        first_chunk_recorded: false,
        tool_chars: IndexMap::new(),
        observed_result: false,
    }));
    let finish_notify = Arc::new(Notify::new());

    let mut start_attrs = IndexMap::new();
    start_attrs.insert(
        "provider".to_owned(),
        AttributeValue::String(requested_model.provider.clone()),
    );
    start_attrs.insert("api".to_owned(), AttributeValue::String(requested_model.api.clone()));
    start_attrs.insert("model".to_owned(), AttributeValue::String(requested_model.id.clone()));
    if let Some(level) = reasoning {
        start_attrs.insert(
            "reasoning".to_owned(),
            AttributeValue::String(thinking_level_str(level).to_owned()),
        );
    }

    let span_name = if poll { "pi.ai.poll" } else { "pi.ai.request" };
    let observer = ModelRequestObserver {
        started: Instant::now(),
        provider: requested_model.provider.clone(),
        model_id: requested_model.id.clone(),
        poll,
        request_duration: request_duration.clone(),
        first_chunk_duration: first_chunk_duration.clone(),
        tokens,
        cost,
        inner: Arc::clone(&inner),
        finish_notify: Arc::clone(&finish_notify),
    };

    if tokio::runtime::Handle::try_current().is_ok() {
        let cb_inner = Arc::clone(&inner);
        let cb_notify = Arc::clone(&finish_notify);
        let cb_req_dur = request_duration;
        let cb_first_dur = first_chunk_duration;
        tokio::spawn(async move {
            let options = SpanOptions {
                name: span_name.to_owned(),
                attributes: Some(start_attrs),
                links: None,
            };
            let span_callback: SpanCallback = Arc::new(move |active_span| {
                let cb_inner = Arc::clone(&cb_inner);
                let cb_notify = Arc::clone(&cb_notify);
                let cb_req_dur = cb_req_dur.clone();
                let cb_first_dur = cb_first_dur.clone();
                Box::pin(async move {
                    let already_finished = {
                        let mut guard = cb_inner.lock().unwrap_or_else(|p| p.into_inner());
                        let pending = std::mem::take(&mut guard.pending_ops);
                        for op in pending {
                            match op {
                                DeferredSpanOp::AddEvent { name, attrs } => active_span.add_event(&name, attrs),
                                DeferredSpanOp::FirstChunkDuration { ms, attrs } => {
                                    cb_first_dur.record(ms, Some(attrs), Some(&active_span));
                                }
                                DeferredSpanOp::RequestDuration { ms, attrs } => {
                                    cb_req_dur.record(ms, Some(attrs), Some(&active_span));
                                }
                                DeferredSpanOp::SetAttributes(attrs) => active_span.set_attributes(attrs),
                                DeferredSpanOp::SetStatus(status) => active_span.set_status(status),
                            }
                        }
                        let done = guard.observed_result;
                        guard.span = Some(Arc::clone(&active_span));
                        done
                    };
                    if !already_finished {
                        cb_notify.notified().await;
                    }
                    Ok(Box::new(()) as SpanValue)
                })
            });
            let _ = context.start_span(options, span_callback).await;
        });
    }

    observer
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ModelCost, ModelInput, Usage, UsageCost};
    use pi_telemetry::InMemoryTelemetryContext;

    fn sample_model() -> Model {
        Model {
            r#type: None,
            id: "claude-opus".to_owned(),
            name: "Claude Opus".to_owned(),
            api: "anthropic-messages".to_owned(),
            provider: "anthropic".to_owned(),
            base_url: "https://api.anthropic.com".to_owned(),
            reasoning: true,
            thinking_level_map: None,
            prompt_cache: None,
            input: vec![ModelInput::Text],
            input_limits: None,
            cost: ModelCost::default(),
            context_window: 200_000,
            max_tokens: 16_384,
            headers: None,
            sampling_params: None,
            sampling_params_by_thinking_level: None,
            compat: None,
        }
    }

    #[tokio::test]
    async fn records_request_span_events_and_metrics() {
        let ctx = InMemoryTelemetryContext::new();
        let ctx_handle: TelemetryContextHandle = Arc::new(ctx.clone());
        let model = sample_model();

        let observer = observe_model_request(&model, None, Some(&ctx_handle), Some(ThinkingLevel::High), false);
        tokio::task::yield_now().await;

        let msg = AssistantMessage {
            content: vec![],
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            response_model: None,
            response_id: Some("resp_1".to_owned()),
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage {
                input: 10,
                output: 5,
                cache_read: 2,
                cache_write: 1,
                cache_write_1h: None,
                reasoning: None,
                total_tokens: 18,
                cost: UsageCost {
                    input: 0.01,
                    output: 0.02,
                    cache_read: 0.001,
                    cache_write: 0.002,
                    total: 0.033,
                    total_from_provider: Some(false),
                },
            },
            stop_reason: StopReason::Error,
            deferred: None,
            error_message: Some("x".repeat(600)),
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 0,
            duration_ms: None,
        };
        observer.on_event(&AssistantMessageEvent::TextEnd {
            content_index: 0,
            content: "hello".to_owned(),
            partial: msg.clone(),
        });
        observer.on_result(Some(&msg));

        for _ in 0..10 {
            if ctx.get_spans().iter().any(|s| s.settled) {
                break;
            }
            tokio::task::yield_now().await;
        }

        let spans = ctx.get_spans();
        assert_eq!(spans.len(), 1);
        assert!(spans[0].settled);
        assert_eq!(spans[0].name, "pi.ai.request");
        assert_eq!(
            spans[0].attributes.get("reasoning"),
            Some(&AttributeValue::String("high".to_owned()))
        );
        assert_eq!(
            spans[0].attributes.get("response.id"),
            Some(&AttributeValue::String("resp_1".to_owned()))
        );
        assert!(matches!(
            &spans[0].status,
            SpanStatus::Error { error: Some(err) } if err.name == "provider_error" && err.message.len() == 500
        ));
        assert_eq!(spans[0].events.len(), 2);
    }
}
