//! Port of packages/durable/src/harness/tool.ts

use std::sync::{Arc, LazyLock, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_ai::types::{AssistantContentBlock, Message, TextContent, ToolCall, ToolResultMessage, UserContentBlock};
use pi_ai::utils::validation::validate_tool_arguments;
use pi_chord::context::await_with_context;
use pi_chord::delta::{DraftInput, overlap};
use pi_chord::json::copy_json;
use pi_chord::{Context, JsonValue};
use serde::{Deserialize, Serialize};

use crate::documents::ErasedDocDefinition;
use crate::entries::{ASSISTANT_ENTRY, TOOL_RESULT_ENTRY, ToolResultEntryData, TypedEntryDraft};
use crate::env::ExecutionEnv;
use crate::harness::json::{JsonKey, assign_json};
use crate::harness::live::{LIVE_DOC, ToolSlot, ToolSlotStatus, clear_progress, finish_slot, tool_slot, tool_slot_mut};
use crate::harness::output::{OutputBuffer, OutputLimits, Progress};
use crate::harness::types::{
    Agent, BeforeToolDecision, ConversationHandle, DiagnosticSeverity, DocumentObserverBackend, DocumentReaderBackend,
    OutputRetainMode, RegistrySnapshot, SettledTask, TaskRuntimeBackend, ToolControl, ToolDiagnostic, ToolExecutionApi,
    ToolExecutionApiBackend, ToolExecutionResult, ToolHooks, ToolRegistration, ToolReplayMode,
};
use crate::harness::usage::{UsageBucket, record_usage};
use crate::session::transaction::{Tx, clone_pi_error};
use crate::tasks::{AnyTask, Task, TaskDefinition, TaskRuntimeHandle, define_task};
use crate::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, utf8_byte_length};
use crate::types::{
    ConversationId, DocumentWatch, EntryId, ErasedTaskRecord, JsonObject, NextTaskState, RunningTask, TaskId,
    TaskOptions, TaskOutcome, TaskOutcomeError, TaskOwnership, TypedEntry,
};

const OVERLAP_SCAN_BYTES: i64 = 65_536;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolTaskInput {
    pub assistant: EntryId,
    pub call_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub enum ToolTaskCheckpoint {
    Call,
    /// Durable intent: the final arguments and the replay policy recorded before execution.
    Execute {
        arguments: JsonObject,
        replay: ToolReplayMode,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolTaskResult {
    pub entry_id: EntryId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<ToolControl>,
}

type Runtime = TaskRuntimeHandle<ToolTaskInput, ToolTaskCheckpoint, ToolTaskResult, ToolHooks>;

/// How a tool task ends; the result entry is appended either way. `Failed` (execution threw or was interrupted)
/// records cancellation intent for the conversations the call owns; a result with `is_error` still completes.
#[derive(Clone, Debug)]
enum Ending {
    Completed,
    Aborted,
    Failed { message: String },
}

enum CheckedArgs {
    Valid(JsonObject),
    Error(String),
}

struct ReportedInner {
    output: OutputBuffer,
    limits: OutputLimits,
    diagnostics: Vec<ToolDiagnostic>,
    details: Option<JsonValue>,
    ended: bool,
}

#[derive(Clone)]
struct ToolExecutionBackend {
    runtime: Runtime,
    registry: Arc<dyn RegistrySnapshot>,
    call_id: String,
    env: Option<Arc<dyn ExecutionEnv>>,
    reported: Arc<Mutex<ReportedInner>>,
    progress: Progress,
}

impl ToolExecutionBackend {
    fn lock_reported(&self) -> std::sync::MutexGuard<'_, ReportedInner> {
        self.reported.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn backend(&self) -> pi_js::Result<Arc<dyn TaskRuntimeBackend>> {
        self.runtime
            .erased
            .as_any()
            .downcast_ref::<Arc<dyn TaskRuntimeBackend>>()
            .cloned()
            .ok_or_else(|| pi_js::Error::msg("Invalid erased task runtime backend"))
    }
}

#[async_trait]
impl DocumentReaderBackend for ToolExecutionBackend {
    async fn snapshot_erased(
        &self,
        def: &Arc<ErasedDocDefinition>,
        conversation_id: Option<ConversationId>,
        task_id: Option<TaskId>,
        key: Option<&str>,
        context: Context,
    ) -> pi_js::Result<Option<JsonObject>> {
        self.backend()?
            .snapshot_erased(def, conversation_id, task_id, key, context)
            .await
    }

    async fn snapshot_as_of_erased(
        &self,
        def: &Arc<ErasedDocDefinition>,
        conversation_id: ConversationId,
        key: Option<&str>,
        at: EntryId,
        context: Context,
    ) -> pi_js::Result<Option<JsonObject>> {
        self.backend()?
            .snapshot_as_of_erased(def, conversation_id, key, at, context)
            .await
    }
}

#[async_trait]
impl DocumentObserverBackend for ToolExecutionBackend {
    async fn watch_erased(
        &self,
        def: &Arc<ErasedDocDefinition>,
        conversation_id: Option<ConversationId>,
        task_id: Option<TaskId>,
        key: Option<&str>,
        context: Context,
    ) -> pi_js::Result<Option<DocumentWatch<JsonObject>>> {
        self.backend()?
            .watch_erased(def, conversation_id, task_id, key, context)
            .await
    }
}

#[async_trait]
impl ToolExecutionApiBackend for ToolExecutionBackend {
    fn task_id(&self) -> TaskId {
        self.runtime
            .task_id()
            .map(TaskId::erased)
            .unwrap_or_else(|_| TaskId::new_unchecked(0))
    }

    fn conversation_id(&self) -> ConversationId {
        self.runtime
            .conversation_id()
            .unwrap_or_else(|_| ConversationId::new_unchecked(0))
    }

    fn call_id(&self) -> &str {
        &self.call_id
    }

    fn registry(&self) -> Arc<dyn RegistrySnapshot> {
        Arc::clone(&self.registry)
    }

    async fn agent(&self, context: Context) -> pi_js::Result<Agent> {
        self.runtime.agent(context).await
    }

    fn env(&self) -> Option<Arc<dyn ExecutionEnv>> {
        self.env.clone()
    }

    fn output_text(&self, chunk: &str) -> pi_js::Result<()> {
        let accepted = {
            let mut reported = self.lock_reported();
            if reported.ended {
                return Err(pi_js::Error::msg(format!("Tool call {} has settled", self.call_id)));
            }
            reported.output.push_str(chunk)
        };
        if accepted {
            self.progress.mark();
        }
        Ok(())
    }

    fn output_bytes(&self, chunk: &[u8]) -> pi_js::Result<()> {
        let accepted = {
            let mut reported = self.lock_reported();
            if reported.ended {
                return Err(pi_js::Error::msg(format!("Tool call {} has settled", self.call_id)));
            }
            reported.output.push_bytes(chunk)
        };
        if accepted {
            self.progress.mark();
        }
        Ok(())
    }

    fn diagnostic(&self, diagnostic: ToolDiagnostic) -> pi_js::Result<()> {
        {
            let mut reported = self.lock_reported();
            if reported.ended {
                return Err(pi_js::Error::msg(format!("Tool call {} has settled", self.call_id)));
            }
            let raw =
                serde_json::to_value(&diagnostic).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
            let copied = copy_json(&raw, None)?;
            let cleaned: ToolDiagnostic =
                serde_json::from_value(copied).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
            reported.diagnostics.push(cleaned);
        }
        self.progress.mark();
        Ok(())
    }

    async fn details(&self, value: JsonValue, context: Context) -> pi_js::Result<()> {
        {
            let mut reported = self.lock_reported();
            if reported.ended {
                return Err(pi_js::Error::msg(format!("Tool call {} has settled", self.call_id)));
            }
            if let Some(signal) = context.abort_signal() {
                signal.throw_if_aborted()?;
            }
            reported.details = Some(copy_json(&value, None)?);
        }
        let committed = self.progress.mark_and_wait();
        await_with_context(committed, &context).await
    }

    async fn commit_erased(
        &self,
        change: Box<dyn FnOnce(Tx) -> pi_js::BoxFuture<pi_js::Result<JsonValue>> + Send>,
        context: Context,
    ) -> pi_js::Result<JsonValue> {
        let out = Arc::new(Mutex::new(None::<JsonValue>));
        let slot = Arc::clone(&out);
        self.runtime
            .commit(
                move |tx, _current| {
                    Box::pin(async move {
                        let val = change(tx).await?;
                        *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(val);
                        Ok(None)
                    })
                },
                context,
            )
            .await?;
        let val = out
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or(JsonValue::Null);
        Ok(val)
    }

    async fn memo_get(&self, name: &str, context: Context) -> pi_js::Result<Option<JsonValue>> {
        self.runtime.memo_get(name, context).await
    }

    async fn memo_set(&self, name: &str, candidate: JsonValue, context: Context) -> pi_js::Result<JsonValue> {
        self.runtime.memo_set(name, &candidate, context).await
    }

    async fn create_task_erased(
        &self,
        task: &AnyTask,
        input: JsonValue,
        ownership: TaskOwnership,
        background: bool,
        context: Context,
    ) -> pi_js::Result<TaskId> {
        let task = task.clone();
        let created = Arc::new(Mutex::new(None::<TaskId>));
        let slot = Arc::clone(&created);
        self.runtime
            .commit(
                move |tx, _current| {
                    Box::pin(async move {
                        let id = tx
                            .create_task_erased(
                                &task,
                                input,
                                TaskOptions {
                                    ownership,
                                    conversation_id: None,
                                    background,
                                },
                            )
                            .await?;
                        *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(id);
                        Ok(None)
                    })
                },
                context,
            )
            .await?;
        created
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .ok_or_else(|| pi_js::Error::msg("Tool createTask did not yield a task id"))
    }

    async fn get_task(&self, id: TaskId, context: Context) -> pi_js::Result<Option<ErasedTaskRecord>> {
        self.runtime.get_task(id, context).await
    }

    async fn wait_for_task(&self, id: TaskId, context: Context) -> pi_js::Result<SettledTask<JsonValue>> {
        self.runtime.wait_for_task(id, context).await
    }

    async fn conversation(&self, id: ConversationId, context: Context) -> pi_js::Result<Option<ConversationHandle>> {
        self.runtime.conversation(id, context).await
    }
}

/// Built-in tool task: resolves the called tool among its phase agent's tools, validates, runs `before_tool`,
/// records intent, executes, runs `after_tool`, and appends the result, all in one `call` handler so nothing
/// separates resolution from settlement. `execute` is reached only by recovery and applies the replay rule.
pub static TOOL_TASK: LazyLock<Task<ToolTaskInput, ToolTaskCheckpoint, ToolTaskResult, ToolHooks>> =
    LazyLock::new(|| {
        let mut phases = IndexMap::new();
        phases.insert(
            "call".to_owned(),
            Arc::new(
                |task: RunningTask<ToolTaskInput, ToolTaskCheckpoint, ToolTaskResult>,
                 runtime: Runtime,
                 context: Context| {
                    Box::pin(async move { phase_call(task, runtime, context).await })
                },
            )
                as crate::tasks::TypedPhaseHandler<ToolTaskInput, ToolTaskCheckpoint, ToolTaskResult, ToolHooks>,
        );
        phases.insert(
            "execute".to_owned(),
            Arc::new(
                |task: RunningTask<ToolTaskInput, ToolTaskCheckpoint, ToolTaskResult>,
                 runtime: Runtime,
                 context: Context| {
                    Box::pin(async move { phase_execute(task, runtime, context).await })
                },
            ),
        );

        define_task(TaskDefinition {
            name: "pi.tool".to_owned(),
            version: 1,
            initial: Arc::new(|_input| Ok(ToolTaskCheckpoint::Call)),
            phases,
            abort: Arc::new(|task, runtime, context| {
                Box::pin(async move { phase_abort(task, runtime, context).await })
            }),
            migrate: None,
            _hooks: std::marker::PhantomData,
        })
    });

async fn phase_call(
    task: RunningTask<ToolTaskInput, ToolTaskCheckpoint, ToolTaskResult>,
    runtime: Runtime,
    context: Context,
) -> pi_js::Result<()> {
    let call = read_call(&runtime, &task.input, context.clone()).await?;
    let agent = runtime.agent(context.clone()).await?;
    let Some(tool) = agent.tools.into_iter().find(|each| each.name == call.name) else {
        let error = harness_error("tool_unavailable", format!("Tool {} is not available", call.name));
        return settle(&runtime, &call, Ending::Completed, move |_slot| error, context).await;
    };

    let prepared = prepare_args(&tool, &call.arguments);
    let checked = match prepared {
        CheckedArgs::Valid(args) => validate_args(&tool, &call, args),
        err @ CheckedArgs::Error(_) => err,
    };
    let mut args = match checked {
        CheckedArgs::Valid(args) => args,
        CheckedArgs::Error(message) => {
            let err = invalid_args(message);
            return settle(&runtime, &call, Ending::Completed, move |_slot| err, context).await;
        }
    };

    let signal = runtime.signal()?;
    let hook_state = Arc::new(Mutex::new((args, None::<String>)));
    runtime
        .hooks(TOOL_TASK.name())?
        .each(|hooks, api| {
            let state = Arc::clone(&hook_state);
            let handler = hooks.before_tool.clone();
            let call_template = call.clone();
            let ctx = context.clone();
            let signal = signal.clone();
            async move {
                let (cur_args, should_skip) = {
                    let guard = state.lock().unwrap_or_else(|e| e.into_inner());
                    (guard.0.clone(), guard.1.is_some())
                };
                if should_skip {
                    return Ok(());
                }
                let Some(before_tool) = handler else {
                    return Ok(());
                };
                let hook_call = ToolCall {
                    id: call_template.id,
                    name: call_template.name,
                    arguments: cur_args,
                    thought_signature: call_template.thought_signature,
                    namespace: call_template.namespace,
                };
                match before_tool(hook_call, api, ctx).await {
                    Ok(Some(BeforeToolDecision::Block(reason))) => {
                        state.lock().unwrap_or_else(|e| e.into_inner()).1 = Some(reason);
                    }
                    Ok(Some(BeforeToolDecision::Arguments(next_args))) => {
                        state.lock().unwrap_or_else(|e| e.into_inner()).0 = next_args;
                    }
                    Ok(None) => {}
                    Err(err) => {
                        if signal.aborted() {
                            return Err(err);
                        }
                        state.lock().unwrap_or_else(|e| e.into_inner()).1 = Some(err.message());
                    }
                }
                Ok(())
            }
        })
        .await?;

    let (args, block) = {
        let mut guard = hook_state.lock().unwrap_or_else(|e| e.into_inner());
        (std::mem::take(&mut guard.0), guard.1.take())
    };

    if let Some(reason) = block {
        let blocked = harness_error("blocked", format!("Tool call blocked: {reason}"));
        return settle(&runtime, &call, Ending::Completed, move |_slot| blocked, context).await;
    }

    let final_args = match validate_args(&tool, &call, args) {
        CheckedArgs::Valid(validated) => validated,
        CheckedArgs::Error(message) => {
            let err = invalid_args(message);
            return settle(&runtime, &call, Ending::Completed, move |_slot| err, context).await;
        }
    };

    let conversation_id = runtime.conversation_id()?;
    let task_id = runtime.task_id()?.erased();
    let replay = tool.replay.unwrap_or(ToolReplayMode::Unsafe);
    let intent_args = final_args.clone();
    runtime
        .commit(
            move |tx, _current| {
                Box::pin(async move {
                    let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
                    live_draft.update(|live| {
                        if let Some(slot) = tool_slot_mut(live, task_id) {
                            slot.status = ToolSlotStatus::Running;
                        }
                    })?;
                    Ok(Some(NextTaskState::Running {
                        checkpoint: ToolTaskCheckpoint::Execute {
                            arguments: intent_args,
                            replay,
                        },
                    }))
                })
            },
            context.clone(),
        )
        .await?;

    run_tool(&runtime, &call, &tool, final_args, context).await
}

/// Recovery after intent: rerun only when the stored and the current policy both say `Safe`.
async fn phase_execute(
    task: RunningTask<ToolTaskInput, ToolTaskCheckpoint, ToolTaskResult>,
    runtime: Runtime,
    context: Context,
) -> pi_js::Result<()> {
    let ToolTaskCheckpoint::Execute { arguments, replay } = task.checkpoint.clone() else {
        return Err(pi_js::Error::msg("Invalid tool task checkpoint in execute phase"));
    };
    let call = read_call(&runtime, &task.input, context.clone()).await?;
    let agent = runtime.agent(context.clone()).await?;
    let tool = agent.tools.into_iter().find(|each| each.name == call.name);

    if replay == ToolReplayMode::Safe
        && let Some(ref resolved) = tool
        && resolved.replay == Some(ToolReplayMode::Safe)
    {
        let conversation_id = runtime.conversation_id()?;
        let task_id = runtime.task_id()?.erased();
        runtime
            .commit(
                move |tx, _current| {
                    Box::pin(async move {
                        let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
                        live_draft.update(|live| {
                            if let Some(slot) = tool_slot_mut(live, task_id) {
                                clear_progress(slot);
                            }
                        })?;
                        Ok(None)
                    })
                },
                context.clone(),
            )
            .await?;
        return run_tool(&runtime, &call, resolved, arguments, context).await;
    }

    let message = format!("Tool {} was interrupted and may have partially run", call.name);
    let ending = Ending::Failed {
        message: message.clone(),
    };
    settle(
        &runtime,
        &call,
        ending,
        move |slot| from_slot(slot, "interrupted", &message),
        context,
    )
    .await
}

async fn phase_abort(
    task: RunningTask<ToolTaskInput, ToolTaskCheckpoint, ToolTaskResult>,
    runtime: Runtime,
    context: Context,
) -> pi_js::Result<()> {
    let call = read_call(&runtime, &task.input, context.clone()).await?;
    let message = format!("Tool {} was aborted", call.name);
    settle(
        &runtime,
        &call,
        Ending::Aborted,
        move |slot| from_slot(slot, "aborted", &message),
        context,
    )
    .await
}

/// The tool call `call_id` of the assistant entry.
async fn read_call(runtime: &Runtime, input: &ToolTaskInput, context: Context) -> pi_js::Result<ToolCall> {
    let entry = runtime.typed_entry(&ASSISTANT_ENTRY, input.assistant, context).await?;
    let message = entry
        .as_ref()
        .and_then(|typed| typed.record.model.as_deref())
        .and_then(|msgs| msgs.first());
    let found = match message {
        Some(Message::Assistant(assistant)) => assistant.content.iter().find_map(|block| match block {
            AssistantContentBlock::ToolCall(call) if call.id == input.call_id => Some(call.clone()),
            _ => None,
        }),
        _ => None,
    };
    found.ok_or_else(|| pi_js::Error::msg(format!("Entry {} has no tool call {}", input.assistant, input.call_id)))
}

fn prepare_args(tool: &ToolRegistration, args: &JsonObject) -> CheckedArgs {
    let Some(prepare) = &tool.prepare_arguments else {
        return CheckedArgs::Valid(args.clone());
    };
    let mut map = serde_json::Map::with_capacity(args.len());
    for (k, v) in args {
        map.insert(k.clone(), v.clone());
    }
    match prepare(&JsonValue::Object(map)) {
        Ok(JsonValue::Object(out)) => {
            let mut obj = IndexMap::with_capacity(out.len());
            for (k, v) in out {
                obj.insert(k, v);
            }
            CheckedArgs::Valid(obj)
        }
        Ok(other) => CheckedArgs::Error(format!(
            "Tool {} prepareArguments must return an object, got {}",
            tool.name, other
        )),
        Err(err) => CheckedArgs::Error(err.message()),
    }
}

fn validate_args(tool: &ToolRegistration, call: &ToolCall, args: JsonObject) -> CheckedArgs {
    let pi_tool = tool.as_pi_tool();
    let candidate = ToolCall {
        id: call.id.clone(),
        name: call.name.clone(),
        arguments: args,
        thought_signature: call.thought_signature.clone(),
        namespace: call.namespace.clone(),
    };
    match validate_tool_arguments(&pi_tool, &candidate) {
        Ok(JsonValue::Object(map)) => {
            let mut obj = IndexMap::with_capacity(map.len());
            for (k, v) in map {
                obj.insert(k, v);
            }
            CheckedArgs::Valid(obj)
        }
        Ok(_) => CheckedArgs::Valid(candidate.arguments),
        Err(err) => CheckedArgs::Error(err.message()),
    }
}

fn invalid_args(message: String) -> ToolExecutionResult {
    harness_error("invalid_arguments", message)
}

async fn run_tool(
    runtime: &Runtime,
    call: &ToolCall,
    tool: &ToolRegistration,
    args: JsonObject,
    context: Context,
) -> pi_js::Result<()> {
    let registry = runtime.registry()?;
    let limits = OutputLimits {
        max_bytes: tool
            .output_limits
            .and_then(|l| l.max_bytes)
            .unwrap_or(DEFAULT_MAX_BYTES),
        max_lines: tool
            .output_limits
            .and_then(|l| l.max_lines)
            .unwrap_or(DEFAULT_MAX_LINES),
        retain: tool
            .output_limits
            .and_then(|l| l.retain)
            .unwrap_or(OutputRetainMode::Head),
    };
    let reported = Arc::new(Mutex::new(ReportedInner {
        output: OutputBuffer::new(limits),
        limits,
        diagnostics: Vec::new(),
        details: None,
        ended: false,
    }));
    let progress = publish_progress(runtime.clone(), Arc::clone(&reported), context.clone());

    let env = match runtime.env(context.clone()).await {
        Ok(env) => Ok(env),
        Err(err) => Err(err),
    };

    let (result, ending) = match env {
        Err(err) => {
            if runtime.signal()?.aborted() {
                reported.lock().unwrap_or_else(|e| e.into_inner()).ended = true;
                for waiter in progress.stop().await {
                    let _ = waiter.send(Err(clone_pi_error(&err)));
                }
                return Err(err);
            }
            let err_res = ToolExecutionResult {
                is_error: Some(true),
                diagnostics: Some(vec![tool_diagnostic("tool_error", err.message())]),
                ..Default::default()
            };
            (
                err_res,
                Ending::Failed {
                    message: format!("Tool {} threw", call.name),
                },
            )
        }
        Ok(resolved_env) => {
            let backend = Arc::new(ToolExecutionBackend {
                runtime: runtime.clone(),
                registry: Arc::clone(&registry),
                call_id: call.id.clone(),
                env: resolved_env,
                reported: Arc::clone(&reported),
                progress: progress.clone(),
            });
            let api = ToolExecutionApi::from_backend(backend);
            let mut map = serde_json::Map::with_capacity(args.len());
            for (k, v) in &args {
                map.insert(k.clone(), v.clone());
            }
            match (tool.execute)(JsonValue::Object(map), api, context.clone()).await {
                Ok(res) => (res, Ending::Completed),
                Err(err) => {
                    if runtime.signal()?.aborted() {
                        reported.lock().unwrap_or_else(|e| e.into_inner()).ended = true;
                        for waiter in progress.stop().await {
                            let _ = waiter.send(Err(clone_pi_error(&err)));
                        }
                        return Err(err);
                    }
                    let err_res = ToolExecutionResult {
                        is_error: Some(true),
                        diagnostics: Some(vec![tool_diagnostic("tool_error", err.message())]),
                        ..Default::default()
                    };
                    (
                        err_res,
                        Ending::Failed {
                            message: format!("Tool {} threw", call.name),
                        },
                    )
                }
            }
        }
    };

    {
        let mut guard = reported.lock().unwrap_or_else(|e| e.into_inner());
        guard.ended = true;
        guard.output.end();
    }
    let pending = progress.stop().await;

    let settle_res = async {
        let settled = final_result(runtime, call, result, &reported, context.clone()).await?;
        settle(runtime, call, ending, move |_slot| settled, context).await
    }
    .await;

    match settle_res {
        Ok(()) => {
            for waiter in pending {
                let _ = waiter.send(Ok(()));
            }
            Ok(())
        }
        Err(err) => {
            for waiter in pending {
                let _ = waiter.send(Err(clone_pi_error(&err)));
            }
            Err(err)
        }
    }
}

/// Throttled commits of what the tool reported into its `pi.live.tools` slot, each writing only what changed
/// since the last one.
fn publish_progress(runtime: Runtime, reported: Arc<Mutex<ReportedInner>>, context: Context) -> Progress {
    struct WrittenProgress {
        text: String,
        details: Option<JsonValue>,
        diagnostics: usize,
    }

    let written = Arc::new(Mutex::new(WrittenProgress {
        text: String::new(),
        details: None,
        diagnostics: 0,
    }));

    let runtime_for_write = runtime.clone();
    let write_fn = Arc::new(move || -> pi_js::BoxFuture<pi_js::Result<usize>> {
        let runtime = runtime_for_write.clone();
        let reported = Arc::clone(&reported);
        let written = Arc::clone(&written);
        let context = context.clone();
        Box::pin(async move {
            let (snapshot, current_details, current_diag_len, added, prev_text, details_changed) = {
                let mut rep = reported.lock().unwrap_or_else(|e| e.into_inner());
                let prev = written.lock().unwrap_or_else(|e| e.into_inner());
                let snapshot = rep.output.snapshot();
                let current_details = rep.details.clone();
                let current_diag_len = rep.diagnostics.len();
                let added: Vec<ToolDiagnostic> = rep.diagnostics[prev.diagnostics..current_diag_len].to_vec();
                let details_changed = current_details != prev.details;
                (
                    snapshot,
                    current_details,
                    current_diag_len,
                    added,
                    prev.text.clone(),
                    details_changed,
                )
            };

            let mut bytes = 0usize;
            if snapshot.text != prev_text {
                let shared = if snapshot.text.starts_with(&prev_text) {
                    prev_text.len()
                } else {
                    overlap(&prev_text, &snapshot.text, OVERLAP_SCAN_BYTES, None, None).max(0) as usize
                };
                if shared <= snapshot.text.len() {
                    bytes += utf8_byte_length(&snapshot.text[shared..]);
                } else {
                    bytes += utf8_byte_length(&snapshot.text);
                }
            }
            if details_changed {
                let serialized = match &current_details {
                    Some(v) => pi_js::json::stringify(v),
                    None => "null".to_owned(),
                };
                bytes += utf8_byte_length(&serialized);
            }
            if !added.is_empty() {
                let serialized = pi_js::json::stringify(&added);
                bytes += utf8_byte_length(&serialized);
            }

            let conversation_id = runtime.conversation_id()?;
            let task_id = runtime.task_id()?.erased();
            let snapshot_for_commit = snapshot.clone();
            let details_for_commit = current_details.clone();
            let added_for_commit = added.clone();

            runtime
                .commit(
                    move |tx, _current| {
                        Box::pin(async move {
                            let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
                            let root = live_draft.overlay()?;
                            let tools_val = root.get("tools")?;
                            let Some(tools_ov) = tools_val.as_overlay() else {
                                return Ok(None);
                            };
                            let len = tools_ov.len()?;
                            let task_id_val = JsonValue::from(task_id.as_i64());
                            let mut found_index: Option<i64> = None;
                            for idx in 0..len {
                                let item = tools_ov.get_index(idx as i64)?;
                                if let Some(ov) = item.as_overlay()
                                    && ov.get("taskId")?.to_json()?.as_ref() == Some(&task_id_val)
                                {
                                    found_index = Some(idx as i64);
                                    break;
                                }
                            }
                            let Some(idx) = found_index else {
                                return Ok(None);
                            };
                            let slot_val = tools_ov.get_index(idx)?;
                            let Some(slot_ov) = slot_val.as_overlay() else {
                                return Ok(None);
                            };

                            let prev_slot_output = slot_ov
                                .get("output")?
                                .to_json()?
                                .and_then(|v| v.as_str().map(str::to_owned))
                                .unwrap_or_default();
                            if prev_slot_output != snapshot_for_commit.text {
                                slot_ov.set("output", JsonValue::String(snapshot_for_commit.text.clone()))?;
                            }
                            if snapshot_for_commit.dropped_bytes > 0 {
                                slot_ov.set(
                                    "droppedBytes",
                                    JsonValue::from(snapshot_for_commit.dropped_bytes as u64),
                                )?;
                            }
                            if snapshot_for_commit.dropped_lines > 0 {
                                slot_ov.set(
                                    "droppedLines",
                                    JsonValue::from(snapshot_for_commit.dropped_lines as u64),
                                )?;
                            }
                            if details_changed && let Some(ref val) = details_for_commit {
                                assign_json(slot_ov, JsonKey::Prop("details"), val)?;
                            }
                            if !added_for_commit.is_empty() {
                                let encoded_inputs: Vec<DraftInput> = added_for_commit
                                    .iter()
                                    .map(|diag| {
                                        serde_json::to_value(diag)
                                            .map(DraftInput::Json)
                                            .map_err(|err| pi_js::Error::js("TypeError", err.to_string()))
                                    })
                                    .collect::<pi_js::Result<Vec<_>>>()?;
                                let existing = slot_ov.get("diagnostics")?;
                                if let Some(diag_ov) = existing.as_overlay()
                                    && diag_ov.is_array()?
                                {
                                    diag_ov.push(&encoded_inputs)?;
                                } else {
                                    let arr: Vec<JsonValue> = encoded_inputs
                                        .into_iter()
                                        .filter_map(|inp| match inp {
                                            DraftInput::Json(v) => Some(v),
                                            _ => None,
                                        })
                                        .collect();
                                    slot_ov.set("diagnostics", JsonValue::Array(arr))?;
                                }
                            }
                            Ok(None)
                        })
                    },
                    context,
                )
                .await?;

            let mut prev = written.lock().unwrap_or_else(|e| e.into_inner());
            prev.text = snapshot.text;
            prev.details = current_details;
            prev.diagnostics = current_diag_len;
            Ok(bytes)
        })
    });

    let on_error = Arc::new(move |err: pi_js::Error| {
        if let Ok(signal) = runtime.signal()
            && !signal.aborted()
        {
            runtime.report(err);
        }
    });

    Progress::new(write_fn, on_error)
}

async fn final_result(
    runtime: &Runtime,
    call: &ToolCall,
    result: ToolExecutionResult,
    reported: &Arc<Mutex<ReportedInner>>,
    context: Context,
) -> pi_js::Result<ToolExecutionResult> {
    let (retained, limits, reported_diagnostics, reported_details) = {
        let mut rep = reported.lock().unwrap_or_else(|e| e.into_inner());
        let retained = if result.content.is_none() {
            Some(rep.output.snapshot())
        } else {
            None
        };
        (retained, rep.limits, rep.diagnostics.clone(), rep.details.clone())
    };

    let initial_content: Vec<UserContentBlock> = match &retained {
        None => result.content.clone().unwrap_or_default(),
        Some(snap) => {
            if snap.text.is_empty() {
                Vec::new()
            } else {
                vec![UserContentBlock::Text(TextContent::new(snap.text.clone()))]
            }
        }
    };

    let mut diagnostics = reported_diagnostics;
    if let Some(res_diags) = result.diagnostics {
        diagnostics.extend(res_diags);
    }

    let final_res = ToolExecutionResult {
        content: Some(initial_content.clone()),
        is_error: result.is_error,
        details: result.details.or(reported_details),
        diagnostics: Some(diagnostics),
        usage: result.usage,
        control: result.control,
    };

    let after_state = Arc::new(Mutex::new(final_res));
    runtime
        .hooks(TOOL_TASK.name())?
        .each(|hooks, api| {
            let handler = hooks.after_tool.clone();
            let state = Arc::clone(&after_state);
            let call_for_hook = call.clone();
            let ctx = context.clone();
            async move {
                let Some(after_tool) = handler else {
                    return Ok(());
                };
                let current_res = state.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if let Some(updated) = after_tool(call_for_hook, current_res, api, ctx).await? {
                    *state.lock().unwrap_or_else(|e| e.into_inner()) = updated;
                }
                Ok(())
            }
        })
        .await?;
    let final_res = after_state.lock().unwrap_or_else(|e| e.into_inner()).clone();

    let mut harness_diags: Vec<ToolDiagnostic> = Vec::new();
    // The retained output's truncation applies only while afterTool kept that content.
    if let Some(ref snap) = retained
        && snap.dropped_bytes > 0
        && final_res.content.as_deref() == Some(initial_content.as_slice())
    {
        harness_diags.push(truncated_diag(
            snap.dropped_lines,
            snap.dropped_bytes,
            Some(limits.retain),
        ));
    }

    let bounded = bound_content(final_res.content.unwrap_or_default(), limits);
    if bounded.dropped_bytes > 0 {
        harness_diags.push(truncated_diag(
            bounded.dropped_lines,
            bounded.dropped_bytes,
            Some(limits.retain),
        ));
    }

    let mut combined_diags = final_res.diagnostics.unwrap_or_default();
    combined_diags.extend(harness_diags);

    Ok(ToolExecutionResult {
        content: Some(bounded.content),
        diagnostics: Some(combined_diags),
        ..final_res
    })
}

async fn settle<F>(runtime: &Runtime, call: &ToolCall, ending: Ending, build: F, context: Context) -> pi_js::Result<()>
where
    F: FnOnce(Option<&ToolSlot>) -> ToolExecutionResult + Send + 'static,
{
    let conversation_id = runtime.conversation_id()?;
    let task_id = runtime.task_id()?.erased();
    let now = runtime.now()?;
    let call = call.clone();
    runtime
        .commit(
            move |tx, _current| {
                Box::pin(async move {
                    let live_draft = tx.doc_conversation(&*LIVE_DOC, conversation_id).await?;
                    let mut live = live_draft.get()?;
                    let result = build(tool_slot(&live, task_id));
                    let entry = append_tool_result(&tx, conversation_id, &call, &result, now).await?;
                    let entry_id = entry.record.id;
                    if let Some(slot) = tool_slot_mut(&mut live, task_id) {
                        finish_slot(slot, Some(entry_id));
                    }
                    live_draft.replace(live)?;

                    let outcome = match ending {
                        Ending::Aborted => TaskOutcome::Aborted {
                            reason: None,
                            result: Some(ToolTaskResult {
                                entry_id,
                                control: None,
                            }),
                        },
                        Ending::Failed { message } => TaskOutcome::Failed {
                            error: TaskOutcomeError { message, detail: None },
                            result: Some(ToolTaskResult {
                                entry_id,
                                control: None,
                            }),
                        },
                        Ending::Completed => {
                            let control = match result.control {
                                None => None,
                                Some(c) => {
                                    let raw = serde_json::to_value(&c)
                                        .map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
                                    let copied = copy_json(&raw, None)?;
                                    Some(
                                        serde_json::from_value(copied)
                                            .map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?,
                                    )
                                }
                            };
                            TaskOutcome::Completed {
                                result: ToolTaskResult { entry_id, control },
                            }
                        }
                    };
                    Ok(Some(NextTaskState::Terminal { outcome }))
                })
            },
            context,
        )
        .await
}

/// An error result from the slot's durable partial output, details, and diagnostics.
fn from_slot(slot: Option<&ToolSlot>, code: &str, message: &str) -> ToolExecutionResult {
    let mut diagnostics = slot.and_then(|s| s.diagnostics.clone()).unwrap_or_default();
    let dropped_bytes = slot.and_then(|s| s.dropped_bytes).unwrap_or(0);
    if dropped_bytes > 0 {
        diagnostics.push(truncated_diag(
            slot.and_then(|s| s.dropped_lines).unwrap_or(0),
            dropped_bytes,
            None,
        ));
    }
    diagnostics.push(tool_diagnostic(code, message));
    let content = match slot.and_then(|s| s.output.as_deref()) {
        None | Some("") => Vec::new(),
        Some(text) => vec![UserContentBlock::Text(TextContent::new(text))],
    };
    ToolExecutionResult {
        content: Some(content),
        is_error: Some(true),
        details: slot.and_then(|s| s.details.clone()),
        diagnostics: Some(diagnostics),
        usage: None,
        control: None,
    }
}

/// An error result the Harness writes itself: no content and one `Error` diagnostic with `code`.
pub fn harness_error(code: impl Into<String>, message: impl Into<String>) -> ToolExecutionResult {
    ToolExecutionResult {
        content: Some(Vec::new()),
        is_error: Some(true),
        details: None,
        diagnostics: Some(vec![tool_diagnostic(code, message)]),
        usage: None,
        control: None,
    }
}

fn tool_diagnostic(code: impl Into<String>, message: impl Into<String>) -> ToolDiagnostic {
    ToolDiagnostic {
        severity: DiagnosticSeverity::Error,
        code: Some(code.into()),
        message: message.into(),
    }
}

/// The Harness's truncation diagnostic; `retain` is unknown when rebuilt from a slot after recovery.
fn truncated_diag(dropped_lines: usize, dropped_bytes: usize, retain: Option<OutputRetainMode>) -> ToolDiagnostic {
    let kept = match retain {
        None => "",
        Some(OutputRetainMode::Head) => " to its beginning",
        Some(OutputRetainMode::Tail) => " to its end",
    };
    ToolDiagnostic {
        severity: DiagnosticSeverity::Warn,
        code: Some("truncated".to_owned()),
        message: format!("Output truncated{kept}: {dropped_lines} lines, {dropped_bytes} bytes dropped"),
    }
}

/// Append a `pi.tool-result` entry. The content ends with the rendered diagnostics, so the stored message is
/// exactly what the model sees; `data` keeps the structured list. A result's usage is added to `pi.usage` in
/// the same commit.
pub async fn append_tool_result(
    tx: &Tx,
    conversation_id: ConversationId,
    call: &ToolCall,
    result: &ToolExecutionResult,
    timestamp: i64,
) -> pi_js::Result<TypedEntry<ToolResultEntryData>> {
    let diagnostics = result.diagnostics.clone().unwrap_or_default();
    let mut content = result.content.clone().unwrap_or_default();
    if !diagnostics.is_empty() {
        content.push(UserContentBlock::Text(TextContent::new(render_diagnostics(
            &diagnostics,
        ))));
    }
    let message = Message::ToolResult(ToolResultMessage {
        tool_call_id: call.id.clone(),
        tool_name: call.name.clone(),
        content,
        details: result.details.clone(),
        usage: result.usage.clone(),
        nested_calls: None,
        is_error: result.is_error.unwrap_or(false),
        timestamp,
    });
    if let Some(ref usage) = result.usage {
        record_usage(tx, conversation_id, UsageBucket::Tools, &call.name, usage).await?;
    }
    tx.append_typed_entry(
        &TOOL_RESULT_ENTRY,
        conversation_id,
        TypedEntryDraft {
            model: Some(vec![message]),
            data: ToolResultEntryData { diagnostics },
            head: None,
            edits: None,
        },
    )
    .await
}

fn render_diagnostics(diagnostics: &[ToolDiagnostic]) -> String {
    let lines: Vec<String> = diagnostics
        .iter()
        .map(|d| {
            let sev = match d.severity {
                DiagnosticSeverity::Info => "info",
                DiagnosticSeverity::Warn => "warn",
                DiagnosticSeverity::Error => "error",
            };
            format!("[{sev}] {}", d.message)
        })
        .collect();
    format!("<harness>\n{}\n</harness>", lines.join("\n"))
}

struct BoundedContent {
    content: Vec<UserContentBlock>,
    dropped_bytes: usize,
    dropped_lines: usize,
}

/// Bound the text of result content. When the joined text exceeds the limits, the text items are replaced by
/// one bounded item at the position of the first (`Head`) or last (`Tail`) text item; other content is kept.
fn bound_content(content: Vec<UserContentBlock>, limits: OutputLimits) -> BoundedContent {
    let mut joined = String::new();
    let mut first_text_idx: Option<usize> = None;
    let mut last_text_idx: Option<usize> = None;
    for (idx, item) in content.iter().enumerate() {
        if let UserContentBlock::Text(t) = item {
            if first_text_idx.is_none() {
                first_text_idx = Some(idx);
            }
            last_text_idx = Some(idx);
            joined.push_str(&t.text);
        }
    }
    let bounded = crate::harness::output::bound_output(&joined, limits);
    if bounded.dropped_bytes == 0 {
        return BoundedContent {
            content,
            dropped_bytes: 0,
            dropped_lines: 0,
        };
    }
    let keep_idx = match limits.retain {
        OutputRetainMode::Head => first_text_idx,
        OutputRetainMode::Tail => last_text_idx,
    };
    let mut result = Vec::with_capacity(content.len());
    for (idx, item) in content.into_iter().enumerate() {
        match item {
            other @ UserContentBlock::Image(_) => result.push(other),
            UserContentBlock::Text(t) if Some(idx) == keep_idx => {
                result.push(UserContentBlock::Text(TextContent {
                    text: bounded.text.clone(),
                    text_signature: t.text_signature,
                }));
            }
            UserContentBlock::Text(_) => {}
        }
    }
    BoundedContent {
        content: result,
        dropped_bytes: bounded.dropped_bytes,
        dropped_lines: bounded.dropped_lines,
    }
}
