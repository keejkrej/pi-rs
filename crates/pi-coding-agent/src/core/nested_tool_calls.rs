//! Tool calls that a tool makes while it runs (`ctx.execute_tool()`), for example from codemode scripts.
//!
//! Port of `packages/coding-agent/src/core/nested-tool-calls.ts`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_agent_core::{
    AgentTool, AgentToolCall, AgentToolCallOutcome, AgentToolResult, AgentToolUpdateCallback, ToolExecutionMode,
};
use pi_ai::{JsonObject, NestedToolCallRecord, NestedToolCallStatus, NestedToolCalls, Usage, UserContentBlock};
use pi_js::BoxFuture;
use pi_js::abort::AbortSignal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::usage_totals::combine_usage;

/// Limits of the nested-call record on a tool result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NestedCallLimits {
    pub max_calls: usize,
    pub max_argument_bytes_per_call: usize,
    pub max_argument_bytes_total: usize,
    pub max_error_chars: usize,
}

/// Default limits of the nested-call record on a tool result.
pub const NESTED_CALL_LIMITS: NestedCallLimits = NestedCallLimits {
    max_calls: 256,
    max_argument_bytes_per_call: 8 * 1024,
    max_argument_bytes_total: 32 * 1024,
    max_error_chars: 500,
};

/// What the nested calls of one model-issued tool call leave on its tool result message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedCallSummary {
    /// Becomes `nested_calls`. `None` when no nested call was made.
    pub calls: Option<NestedToolCalls>,
    /// Summed `usage` of the nested results, added to the message's `usage`.
    pub usage: Option<Usage>,
}

/// Collects the nested calls of one model-issued tool call, including calls made by nested tools.
#[derive(Debug, Default)]
pub struct NestedCallRecorder {
    calls: Vec<NestedToolCallRecord>,
    started_at: IndexMap<usize, f64>,
    complete: bool,
    argument_bytes: usize,
    /// Summed usage of every nested result, including calls dropped from the record.
    usage: Option<Usage>,
}

impl NestedCallRecorder {
    /// Create a fresh recorder marked complete with no calls yet.
    pub fn new() -> Self {
        Self {
            calls: Vec::new(),
            started_at: IndexMap::new(),
            complete: true,
            argument_bytes: 0,
            usage: None,
        }
    }

    /// Record a call as it starts. Returns an index handle, or `None` when the call is dropped.
    pub fn start(&mut self, tool_call: &AgentToolCall) -> Option<usize> {
        if self.calls.len() >= NESTED_CALL_LIMITS.max_calls {
            self.complete = false;
            return None;
        }
        let mut record = NestedToolCallRecord {
            id: tool_call.id.clone(),
            name: tool_call.name.clone(),
            arguments: None,
            arguments_bytes: None,
            status: NestedToolCallStatus::Unfinished,
            duration_ms: None,
            error: None,
        };
        let json = pi_js::json::stringify(&tool_call.arguments);
        let bytes = json.len();
        if bytes > NESTED_CALL_LIMITS.max_argument_bytes_per_call
            || self.argument_bytes + bytes > NESTED_CALL_LIMITS.max_argument_bytes_total
        {
            record.arguments_bytes = Some(bytes as i64);
            self.complete = false;
        } else {
            let parsed = pi_js::json::parse_as::<JsonObject>(&json).unwrap_or_default();
            record.arguments = Some(parsed);
            self.argument_bytes += bytes;
        }
        let idx = self.calls.len();
        self.calls.push(record);
        self.started_at.insert(idx, pi_js::time::performance_now());
        Some(idx)
    }

    /// Complete a previously started record with status, elapsed duration, and optional error text.
    pub fn finish(&mut self, handle: Option<usize>, is_error: bool, error_text: &str) {
        let Some(idx) = handle else {
            return;
        };
        let Some(record) = self.calls.get_mut(idx) else {
            return;
        };
        record.status = if is_error {
            NestedToolCallStatus::Error
        } else {
            NestedToolCallStatus::Ok
        };
        let now = pi_js::time::performance_now();
        let started = self.started_at.shift_remove(&idx).unwrap_or(now);
        record.duration_ms = Some((now - started).round());
        if is_error && !error_text.is_empty() {
            let end_byte = pi_js::str16::utf16_to_byte(error_text, NESTED_CALL_LIMITS.max_error_chars);
            record.error = Some(error_text[..end_byte].to_owned());
        }
    }

    /// Accumulate usage from a nested tool result (even if its call record was dropped).
    pub fn add_usage(&mut self, usage: &Usage) {
        self.usage = Some(match &self.usage {
            Some(prev) => combine_usage(prev, usage),
            None => *usage,
        });
    }

    /// Summed usage across all recorded and dropped nested calls so far.
    pub fn total_usage(&self) -> Option<Usage> {
        self.usage
    }

    /// Copy of the record so far, or `None` when no nested call was made.
    pub fn snapshot(&self) -> Option<NestedToolCalls> {
        if self.calls.is_empty() && self.complete {
            return None;
        }
        let calls = self.calls.clone();
        let all_finished = calls.iter().all(|call| call.status != NestedToolCallStatus::Unfinished);
        Some(NestedToolCalls {
            calls,
            complete: self.complete && all_finished,
        })
    }
}

/// Options passed to [`NestedToolCallRunner::execute`].
#[derive(Clone, Default)]
pub struct NestedToolCallOptions {
    /// Defaults to the calling tool's signal.
    pub signal: Option<AbortSignal>,
    /// Receives partial results of the nested tool, in addition to `tool_execution_update` events.
    pub on_update: Option<AgentToolUpdateCallback>,
}

/// `tool_execution_*` events emitted for nested tool calls.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NestedToolExecutionEvent {
    #[serde(rename_all = "camelCase")]
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: Value,
        parent_tool_call_id: String,
    },
    #[serde(rename_all = "camelCase")]
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        args: Value,
        partial_result: AgentToolResult,
        parent_tool_call_id: String,
    },
    #[serde(rename_all = "camelCase")]
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: AgentToolResult,
        is_error: bool,
        parent_tool_call_id: String,
    },
}

/// Async update callback forwarded from [`NestedToolCallRunner`] into [`NestedToolCallHost::run_tool_call`].
pub type NestedToolUpdateAsyncCallback = Arc<dyn Fn(AgentToolResult) -> BoxFuture<()> + Send + Sync>;

/// Host interface provided by the agent/session to run nested tool calls.
#[async_trait]
pub trait NestedToolCallHost: Send + Sync {
    /// Tools nested calls resolve against.
    fn get_tools(&self) -> Vec<AgentTool>;
    /// Whether every nested call runs exclusively, as when the agent executes tool calls sequentially.
    fn is_sequential(&self) -> bool;
    /// Run the call through the tool pipeline, with hooks that report `parent_tool_call_id`.
    async fn run_tool_call(
        &self,
        tool_call: AgentToolCall,
        parent_tool_call_id: &str,
        signal: Option<AbortSignal>,
        on_update: NestedToolUpdateAsyncCallback,
    ) -> AgentToolCallOutcome;
    /// Emit a nested tool execution lifecycle event.
    async fn emit(&self, event: NestedToolExecutionEvent);
}

#[derive(Clone)]
struct CallScope {
    recorder: Arc<Mutex<NestedCallRecorder>>,
    next_id: usize,
    holds_queue: bool,
}

fn text_of(result: &AgentToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(t) => Some(t.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Coordinates nested `ctx.executeTool()` execution, event emission, exclusivity, and recording.
pub struct NestedToolCallRunner {
    host: Arc<dyn NestedToolCallHost>,
    scopes: Arc<Mutex<IndexMap<String, CallScope>>>,
    exclusive_lock: Arc<tokio::sync::Mutex<()>>,
}

impl NestedToolCallRunner {
    pub fn new(host: Arc<dyn NestedToolCallHost>) -> Self {
        Self {
            host,
            scopes: Arc::new(Mutex::new(IndexMap::new())),
            exclusive_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Run `name` on behalf of the call `caller_id`. The nested call gets id `<caller_id>/<n>`.
    pub async fn execute(
        &self,
        caller_id: &str,
        name: &str,
        args: Value,
        options: Option<NestedToolCallOptions>,
    ) -> AgentToolCallOutcome {
        let opts = options.unwrap_or_default();

        let (recorder, child_id, holds_queue) = {
            let mut scopes = self.scopes.lock().unwrap_or_else(|e| e.into_inner());
            let scope = scopes.entry(caller_id.to_owned()).or_insert_with(|| CallScope {
                recorder: Arc::new(Mutex::new(NestedCallRecorder::new())),
                next_id: 1,
                holds_queue: false,
            });
            let n = scope.next_id;
            scope.next_id += 1;
            (
                Arc::clone(&scope.recorder),
                format!("{caller_id}/{n}"),
                scope.holds_queue,
            )
        };

        let arg_map: JsonObject = match args {
            Value::Object(map) => map.into_iter().collect(),
            _ => JsonObject::new(),
        };
        let tool_call = AgentToolCall {
            id: child_id.clone(),
            name: name.to_owned(),
            arguments: arg_map,
        };
        let args_value = Value::Object(
            tool_call
                .arguments
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        );

        let record_handle = {
            let mut rec = recorder.lock().unwrap_or_else(|e| e.into_inner());
            rec.start(&tool_call)
        };

        self.host
            .emit(NestedToolExecutionEvent::ToolExecutionStart {
                tool_call_id: child_id.clone(),
                tool_name: name.to_owned(),
                args: args_value.clone(),
                parent_tool_call_id: caller_id.to_owned(),
            })
            .await;

        let exclusive = !holds_queue
            && (self.host.is_sequential()
                || self
                    .host
                    .get_tools()
                    .iter()
                    .find(|tool| tool.name == name)
                    .and_then(|tool| tool.execution_mode)
                    == Some(ToolExecutionMode::Sequential));

        let _exclusive_guard = if exclusive {
            Some(self.exclusive_lock.lock().await)
        } else {
            None
        };

        {
            let mut scopes = self.scopes.lock().unwrap_or_else(|e| e.into_inner());
            scopes.insert(
                child_id.clone(),
                CallScope {
                    recorder: Arc::clone(&recorder),
                    next_id: 1,
                    holds_queue: holds_queue || exclusive,
                },
            );
        }

        struct ScopeGuard {
            scopes: Arc<Mutex<IndexMap<String, CallScope>>>,
            child_id: String,
        }
        impl Drop for ScopeGuard {
            fn drop(&mut self) {
                let mut scopes = self.scopes.lock().unwrap_or_else(|e| e.into_inner());
                scopes.shift_remove(&self.child_id);
            }
        }
        let _scope_guard = ScopeGuard {
            scopes: Arc::clone(&self.scopes),
            child_id: child_id.clone(),
        };

        let host_for_update = Arc::clone(&self.host);
        let on_update_sync = opts.on_update;
        let child_id_for_update = child_id.clone();
        let name_for_update = name.to_owned();
        let args_for_update = args_value.clone();
        let caller_for_update = caller_id.to_owned();

        let on_update: NestedToolUpdateAsyncCallback = Arc::new(move |partial_result: AgentToolResult| {
            let host = Arc::clone(&host_for_update);
            let on_update_sync = on_update_sync.clone();
            let tool_call_id = child_id_for_update.clone();
            let tool_name = name_for_update.clone();
            let args = args_for_update.clone();
            let parent_tool_call_id = caller_for_update.clone();
            Box::pin(async move {
                if let Some(cb) = &on_update_sync {
                    cb(partial_result.clone());
                }
                host.emit(NestedToolExecutionEvent::ToolExecutionUpdate {
                    tool_call_id,
                    tool_name,
                    args,
                    partial_result,
                    parent_tool_call_id,
                })
                .await;
            })
        });

        let outcome = self
            .host
            .run_tool_call(tool_call, caller_id, opts.signal, on_update)
            .await;

        drop(_scope_guard);
        drop(_exclusive_guard);

        {
            let error_text = text_of(&outcome.result);
            let mut rec = recorder.lock().unwrap_or_else(|e| e.into_inner());
            rec.finish(record_handle, outcome.is_error, &error_text);
            if let Some(usage) = &outcome.result.usage {
                rec.add_usage(usage);
            }
        }

        self.host
            .emit(NestedToolExecutionEvent::ToolExecutionEnd {
                tool_call_id: child_id,
                tool_name: name.to_owned(),
                result: outcome.result.clone(),
                is_error: outcome.is_error,
                parent_tool_call_id: caller_id.to_owned(),
            })
            .await;

        outcome
    }

    /// Remove and return the record of the nested calls a model-issued call made.
    pub fn take_record(&self, tool_call_id: &str) -> Option<NestedCallSummary> {
        let scope = {
            let mut scopes = self.scopes.lock().unwrap_or_else(|e| e.into_inner());
            scopes.shift_remove(tool_call_id)
        }?;
        let rec = scope.recorder.lock().unwrap_or_else(|e| e.into_inner());
        Some(NestedCallSummary {
            calls: rec.snapshot(),
            usage: rec.total_usage(),
        })
    }

    /// Drop all active scopes.
    pub fn clear(&self) {
        let mut scopes = self.scopes.lock().unwrap_or_else(|e| e.into_inner());
        scopes.clear();
    }
}
