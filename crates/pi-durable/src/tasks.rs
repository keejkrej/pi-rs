//! Port of packages/durable/src/tasks.ts

use std::marker::PhantomData;
use std::sync::Arc;

use indexmap::IndexMap;
use pi_chord::Context;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::types::{ErasedTaskRecord, JsonValue, NextTaskState, RunningTask, TaskId, TaskOutcomeError};

/// Object-safe erased task runtime passed into [`ErasedTaskDefinition`].
/// Implemented by the scheduler's per-invocation runtime in [`crate::harness`].
#[async_trait::async_trait]
pub trait ErasedTaskRuntime: Send + Sync {
    fn as_any(&self) -> &(dyn std::any::Any + Send + Sync);
}

/// Erased task phase callback operating over JSON inputs and checkpoints.
pub type ErasedPhaseFn = Arc<
    dyn Fn(
            RunningTask<JsonValue, JsonValue, JsonValue>,
            Arc<dyn ErasedTaskRuntime>,
            Context,
        ) -> pi_js::BoxFuture<pi_js::Result<()>>
        + Send
        + Sync,
>;

/// Erased task abort callback operating over JSON inputs and checkpoints.
pub type ErasedAbortFn = Arc<
    dyn Fn(
            RunningTask<JsonValue, JsonValue, JsonValue>,
            Arc<dyn ErasedTaskRuntime>,
            Context,
        ) -> pi_js::BoxFuture<pi_js::Result<()>>
        + Send
        + Sync,
>;

/// Erased migration callback `(input, checkpoint, from_version) -> (new_input, new_checkpoint)`.
pub type ErasedMigrateFn =
    Arc<dyn Fn(JsonValue, JsonValue, i64) -> pi_js::Result<(JsonValue, JsonValue)> + Send + Sync>;

/// Erased executable task definition stored in a registry and scheduled by [`crate::harness::scheduler`].
#[derive(Clone)]
pub struct ErasedTaskDefinition {
    pub name: String,
    pub version: i64,
    pub initial: Arc<dyn Fn(JsonValue) -> pi_js::Result<JsonValue> + Send + Sync>,
    pub phases: IndexMap<String, ErasedPhaseFn>,
    pub abort: ErasedAbortFn,
    pub migrate: Option<ErasedMigrateFn>,
}

/// Erased executable task definition token installed into [`crate::harness::registry::Registry`].
#[derive(Clone)]
pub struct AnyTask {
    pub definition: Arc<ErasedTaskDefinition>,
}

impl AnyTask {
    pub fn name(&self) -> &str {
        &self.definition.name
    }

    pub fn version(&self) -> i64 {
        self.definition.version
    }
}

/// Typed phase callback signature for [`TaskDefinition`].
pub type TypedPhaseHandler<I, S, R, H> = Arc<
    dyn Fn(RunningTask<I, S, R>, TaskRuntimeHandle<I, S, R, H>, Context) -> pi_js::BoxFuture<pi_js::Result<()>>
        + Send
        + Sync,
>;

/// Typed abort callback signature for [`TaskDefinition`].
pub type TypedAbortHandler<I, S, R, H> = Arc<
    dyn Fn(RunningTask<I, S, R>, TaskRuntimeHandle<I, S, R, H>, Context) -> pi_js::BoxFuture<pi_js::Result<()>>
        + Send
        + Sync,
>;

/// Typed migration callback signature for [`TaskDefinition`].
pub type TypedMigrateFn<I, S> = Arc<dyn Fn(JsonValue, JsonValue, i64) -> pi_js::Result<(I, S)> + Send + Sync>;

/// Strongly-typed handle wrapping [`ErasedTaskRuntime`] for user/built-in task definitions.
/// Actual methods live in [`crate::harness::types`] via concrete [`crate::harness::scheduler::TaskRuntime`].
#[derive(Clone)]
pub struct TaskRuntimeHandle<I, S, R, H> {
    pub erased: Arc<dyn ErasedTaskRuntime>,
    _marker: PhantomData<fn() -> (I, S, R, H)>,
}

impl<I, S, R, H> TaskRuntimeHandle<I, S, R, H> {
    pub fn from_erased(erased: Arc<dyn ErasedTaskRuntime>) -> Self {
        Self {
            erased,
            _marker: PhantomData,
        }
    }
}

/// Executable durable state machine definition, registered in the registry by `name`.
pub struct TaskDefinition<I, S, R, H = ()> {
    pub name: String,
    pub version: i64,
    pub initial: Arc<dyn Fn(&I) -> pi_js::Result<S> + Send + Sync>,
    pub phases: IndexMap<String, TypedPhaseHandler<I, S, R, H>>,
    pub abort: TypedAbortHandler<I, S, R, H>,
    pub migrate: Option<TypedMigrateFn<I, S>>,
    pub _hooks: PhantomData<fn() -> H>,
}

/// Typed executable task definition token.
pub struct Task<I, S, R, H = ()> {
    pub erased: AnyTask,
    _marker: PhantomData<fn() -> (I, S, R, H)>,
}

impl<I, S, R, H> Clone for Task<I, S, R, H> {
    fn clone(&self) -> Self {
        Self {
            erased: self.erased.clone(),
            _marker: PhantomData,
        }
    }
}

impl<I, S, R, H> Task<I, S, R, H> {
    pub fn name(&self) -> &str {
        self.erased.name()
    }

    pub fn version(&self) -> i64 {
        self.erased.version()
    }

    pub fn into_any(self) -> AnyTask {
        self.erased
    }

    pub fn as_any(&self) -> &AnyTask {
        &self.erased
    }
}

impl<I, S, R, H> From<Task<I, S, R, H>> for AnyTask {
    fn from(task: Task<I, S, R, H>) -> Self {
        task.erased
    }
}

/// Define an executable task. Register it in the registry so a Harness can run tasks of its kind.
pub fn define_task<I, S, R, H>(definition: TaskDefinition<I, S, R, H>) -> Task<I, S, R, H>
where
    I: Serialize + DeserializeOwned + Send + Sync + 'static,
    S: Serialize + DeserializeOwned + Send + Sync + 'static,
    R: Serialize + DeserializeOwned + Send + Sync + 'static,
    H: Send + Sync + 'static,
{
    let initial_fn = definition.initial;
    let initial = Arc::new(move |raw_input: JsonValue| -> pi_js::Result<JsonValue> {
        let typed_input: I =
            serde_json::from_value(raw_input).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
        let checkpoint = initial_fn(&typed_input)?;
        serde_json::to_value(checkpoint).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))
    });

    let mut phases = IndexMap::with_capacity(definition.phases.len());
    for (phase_name, handler) in definition.phases {
        let erased_handler: ErasedPhaseFn = Arc::new(move |task, runtime, context| {
            let typed_task = match decode_running_task::<I, S, R>(task) {
                Ok(t) => t,
                Err(err) => return Box::pin(futures::future::ready(Err(err))),
            };
            handler(typed_task, TaskRuntimeHandle::from_erased(runtime), context)
        });
        phases.insert(phase_name, erased_handler);
    }

    let abort_fn = definition.abort;
    let abort: ErasedAbortFn = Arc::new(move |task, runtime, context| {
        let typed_task = match decode_running_task::<I, S, R>(task) {
            Ok(t) => t,
            Err(err) => return Box::pin(futures::future::ready(Err(err))),
        };
        abort_fn(typed_task, TaskRuntimeHandle::from_erased(runtime), context)
    });

    let migrate = definition.migrate.map(|mig| -> ErasedMigrateFn {
        Arc::new(move |raw_input, raw_checkpoint, from_version| {
            let (new_input, new_checkpoint) = mig(raw_input, raw_checkpoint, from_version)?;
            let encoded_input =
                serde_json::to_value(new_input).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
            let encoded_checkpoint =
                serde_json::to_value(new_checkpoint).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
            Ok((encoded_input, encoded_checkpoint))
        })
    });

    Task {
        erased: AnyTask {
            definition: Arc::new(ErasedTaskDefinition {
                name: definition.name,
                version: definition.version,
                initial,
                phases,
                abort,
                migrate,
            }),
        },
        _marker: PhantomData,
    }
}

fn decode_running_task<I, S, R>(
    raw: RunningTask<JsonValue, JsonValue, JsonValue>,
) -> pi_js::Result<RunningTask<I, S, R>>
where
    I: DeserializeOwned,
    S: DeserializeOwned,
{
    let input: I = serde_json::from_value(raw.input).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
    let checkpoint: S =
        serde_json::from_value(raw.checkpoint).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
    Ok(RunningTask {
        id: raw.id.cast::<R>(),
        conversation_id: raw.conversation_id,
        kind: raw.kind,
        version: raw.version,
        input,
        owner: raw.owner,
        background: raw.background,
        abort_requested: raw.abort_requested,
        checkpoint,
        memos: raw.memos,
    })
}

/// Encode a typed [`NextTaskState`] to its erased JSON form.
pub fn encode_next_task_state<S, R>(next: NextTaskState<S, R>) -> pi_js::Result<NextTaskState<JsonValue, JsonValue>>
where
    S: Serialize,
    R: Serialize,
{
    Ok(match next {
        NextTaskState::Running { checkpoint } => NextTaskState::Running {
            checkpoint: serde_json::to_value(checkpoint).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?,
        },
        NextTaskState::Waiting { checkpoint, on, policy } => NextTaskState::Waiting {
            checkpoint: serde_json::to_value(checkpoint).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?,
            on,
            policy,
        },
        NextTaskState::Terminal { outcome } => NextTaskState::Terminal {
            outcome: match outcome {
                crate::types::TaskOutcome::Completed { result } => crate::types::TaskOutcome::Completed {
                    result: serde_json::to_value(result)
                        .map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?,
                },
                crate::types::TaskOutcome::Failed { error, result } => crate::types::TaskOutcome::Failed {
                    error,
                    result: result
                        .map(|r| serde_json::to_value(r).map_err(|err| pi_js::Error::js("TypeError", err.to_string())))
                        .transpose()?,
                },
                crate::types::TaskOutcome::Aborted { reason, result } => crate::types::TaskOutcome::Aborted {
                    reason,
                    result: result
                        .map(|r| serde_json::to_value(r).map_err(|err| pi_js::Error::js("TypeError", err.to_string())))
                        .transpose()?,
                },
                crate::types::TaskOutcome::Orphaned { reason } => crate::types::TaskOutcome::Orphaned { reason },
                crate::types::TaskOutcome::Faulted { error } => crate::types::TaskOutcome::Faulted { error },
            },
        },
    })
}

/// Convert an error into a persisted [`TaskOutcomeError`].
pub fn task_outcome_error(err: &pi_js::Error) -> TaskOutcomeError {
    TaskOutcomeError {
        message: err.message(),
        detail: None,
    }
}

/// Extract phase name from a checkpoint JSON object.
pub fn checkpoint_phase(checkpoint: &JsonValue) -> Option<&str> {
    checkpoint.get("phase").and_then(JsonValue::as_str)
}

/// Helper to cast an erased task record back into a typed result if it has terminal outcome.
pub fn decode_task_record<I, S, R>(record: &ErasedTaskRecord) -> pi_js::Result<crate::types::TaskRecord<I, S, R>>
where
    I: DeserializeOwned,
    S: DeserializeOwned,
    R: DeserializeOwned,
{
    let input: I = serde_json::from_value(record.input.clone())
        .map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
    let state = match &record.state {
        crate::types::TaskState::Pending { checkpoint } => crate::types::TaskState::Pending {
            checkpoint: serde_json::from_value(checkpoint.clone())
                .map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?,
        },
        crate::types::TaskState::Running { checkpoint } => crate::types::TaskState::Running {
            checkpoint: serde_json::from_value(checkpoint.clone())
                .map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?,
        },
        crate::types::TaskState::Waiting { checkpoint, on, policy } => crate::types::TaskState::Waiting {
            checkpoint: serde_json::from_value(checkpoint.clone())
                .map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?,
            on: on.clone(),
            policy: *policy,
        },
        crate::types::TaskState::Completing { outcome } => crate::types::TaskState::Completing {
            outcome: decode_outcome(outcome)?,
        },
        crate::types::TaskState::Terminal { outcome } => crate::types::TaskState::Terminal {
            outcome: decode_outcome(outcome)?,
        },
    };
    Ok(crate::types::TaskRecord {
        id: record.id.cast::<R>(),
        conversation_id: record.conversation_id,
        kind: record.kind.clone(),
        version: record.version,
        input,
        owner: record.owner,
        background: record.background,
        abort_requested: record.abort_requested,
        state,
        memos: record.memos.clone(),
    })
}

pub fn decode_outcome<R: DeserializeOwned>(
    outcome: &crate::types::TaskOutcome<JsonValue>,
) -> pi_js::Result<crate::types::TaskOutcome<R>> {
    Ok(match outcome {
        crate::types::TaskOutcome::Completed { result } => crate::types::TaskOutcome::Completed {
            result: serde_json::from_value(result.clone())
                .map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?,
        },
        crate::types::TaskOutcome::Failed { error, result } => crate::types::TaskOutcome::Failed {
            error: error.clone(),
            result: result
                .as_ref()
                .map(|r| serde_json::from_value(r.clone()).map_err(|err| pi_js::Error::js("TypeError", err.to_string())))
                .transpose()?,
        },
        crate::types::TaskOutcome::Aborted { reason, result } => crate::types::TaskOutcome::Aborted {
            reason: reason.clone(),
            result: result
                .as_ref()
                .map(|r| serde_json::from_value(r.clone()).map_err(|err| pi_js::Error::js("TypeError", err.to_string())))
                .transpose()?,
        },
        crate::types::TaskOutcome::Orphaned { reason } => crate::types::TaskOutcome::Orphaned {
            reason: reason.clone(),
        },
        crate::types::TaskOutcome::Faulted { error } => crate::types::TaskOutcome::Faulted { error: error.clone() },
    })
}

pub fn typed_task_id<R>(raw: i64) -> TaskId<R> {
    TaskId::new_unchecked(raw)
}
