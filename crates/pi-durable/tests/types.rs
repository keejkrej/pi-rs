//! Port of packages/durable/test/types.test.ts

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use indexmap::IndexMap;
use pi_chord::Context;
use pi_chord::delta::Op;
use pi_durable::documents::{
    DocDefinitionInput, address_id, check_create_scope, check_record_scope, check_record_version,
    define_conversation_doc, define_rewindable_conversation_doc, define_session_doc,
    document_create, materialize_create_value, materialize_document,
};
use pi_durable::errors::ReadAfterWrite;
use pi_durable::harness::types::TaskRuntimeBackend;
use pi_durable::ids::{DocumentId, EntryId, TaskId, id_from_number, seq_from_number};
use pi_durable::tasks::{
    ErasedTaskDefinition, TaskDefinition, checkpoint_phase, decode_outcome, decode_task_record,
    define_task, task_outcome_error,
};
use pi_durable::types::{
    DocumentCreate, DocumentPoint, DocumentRecord, DocumentScope, JsonObject, LatestFork,
    NextTaskState, RewindableFork, StoredDocument, TaskOutcome, TaskOutcomeError, TaskRecord,
    TaskState, WatchEnd, WatchHandle, WatchHandleApi, WatchListener, mint_typed_id,
};
use pretty_assertions::assert_eq;
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SettingsDoc {
    model: String,
    verbose: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PlanDoc {
    steps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ReviewTaskInput {
    target: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ReviewTaskState {
    phase: String,
    count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ReviewTaskResult {
    approved: bool,
}

struct MockRuntimeHandle {
    id: TaskId<ReviewTaskResult>,
    outcome: TaskOutcome,
}

#[async_trait::async_trait]
impl TaskRuntimeBackend for MockRuntimeHandle {
    async fn checkpoint(
        &self,
        _checkpoint: JsonValue,
        _context: &Context,
    ) -> pi_js::Result<()> {
        Ok(())
    }

    async fn wait(
        &self,
        _ids: Vec<TaskId>,
        _policy: pi_durable::types::JoinPolicy,
        _checkpoint: JsonValue,
        _context: &Context,
    ) -> pi_js::Result<()> {
        Ok(())
    }

    async fn finish(&self, _result: JsonValue, _context: &Context) -> pi_js::Result<()> {
        Ok(())
    }

    async fn fail(
        &self,
        _error: TaskOutcomeError,
        _result: Option<JsonValue>,
        _context: &Context,
    ) -> pi_js::Result<()> {
        Ok(())
    }

    async fn abort_task(
        &self,
        _reason: Option<String>,
        _result: Option<JsonValue>,
        _context: &Context,
    ) -> pi_js::Result<()> {
        Ok(())
    }

    async fn outcome(&self, _context: &Context) -> pi_js::Result<TaskOutcome> {
        Ok(self.outcome.clone())
    }

    fn id(&self) -> TaskId {
        self.id.cast()
    }
}

struct DummyWatchApi<T>(T);

#[async_trait::async_trait]
impl<T: Clone + Send + Sync + 'static> WatchHandleApi<T> for DummyWatchApi<T> {
    fn value(&self) -> T {
        self.0.clone()
    }

    fn start(&self, _listener: WatchListener<T>) -> pi_js::Result<()> {
        Ok(())
    }

    async fn stop(&self) -> WatchEnd {
        WatchEnd::Stopped
    }

    async fn closed(&self) -> WatchEnd {
        WatchEnd::Stopped
    }
}

fn json_obj(val: JsonValue) -> JsonObject {
    match val {
        JsonValue::Object(map) => map,
        _ => panic!("expected json object"),
    }
}

#[test]
fn read_after_write_error_formats_and_converts() {
    let err = ReadAfterWrite::new("entry");
    assert_eq!(
        err.to_string(),
        "Tx.entry() cannot read tables after the first table write"
    );
    let js_err: pi_js::Error = err.into();
    assert_eq!(js_err.name(), "ReadAfterWrite");
    assert!(js_err.downcast_ref::<ReadAfterWrite>().is_some());
}

#[test]
fn document_tokens_validate_scope_version_and_materialize() {
    let session_doc = define_session_doc(DocDefinitionInput {
        kind: "settings",
        version: 1,
        initial: || SettingsDoc {
            model: "sonnet".to_string(),
            verbose: false,
        },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("session doc");

    let conv_doc = define_rewindable_conversation_doc(
        RewindableFork::AsOf,
        DocDefinitionInput {
            kind: "plan",
            version: 2,
            initial: || PlanDoc {
                steps: vec!["one".to_string()],
            },
            migrate: Some(|val, _from| {
                let mut map = val.clone();
                map.entry("steps".to_string()).or_insert_with(|| json!([]));
                serde_json::from_value(JsonValue::Object(map))
                    .map_err(|e| pi_js::Error::msg(e.to_string()))
            }),
            checkpoint_when: None,
        },
    )
    .expect("conv doc");

    let _latest_doc = define_conversation_doc(
        LatestFork::Current,
        DocDefinitionInput {
            kind: "notes",
            version: 1,
            initial: || PlanDoc { steps: Vec::new() },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("latest doc");

    let session_addr = pi_durable::types::DocumentAddress {
        kind: "settings".to_string(),
        scope: DocumentScope::Session,
        key: None,
    };
    let conv_addr = pi_durable::types::DocumentAddress {
        kind: "plan".to_string(),
        scope: DocumentScope::Conversation {
            conversation_id: id_from_number(1),
        },
        key: None,
    };
    assert_eq!(address_id(&session_addr), address_id(&session_addr));
    assert_ne!(address_id(&session_addr), address_id(&conv_addr));

    let scope_conv = DocumentScope::Conversation {
        conversation_id: id_from_number(1),
    };
    let record = DocumentRecord {
        id: id_from_number(10),
        kind: "plan".to_string(),
        scope: scope_conv,
        key: None,
        history: conv_doc.definition.semantics.history(),
        fork: conv_doc.definition.semantics.fork(),
        created_at: seq_from_number(1),
        retired_at: None,
    };
    assert!(check_record_scope(&conv_doc.definition, &record).is_ok());
    assert!(check_record_scope(&session_doc.definition, &record).is_err());

    let create_rec = document_create(&conv_doc.definition, &conv_addr, id_from_number(10));
    assert!(check_create_scope(&conv_doc.definition, &create_rec).is_ok());
    assert!(check_create_scope(&session_doc.definition, &create_rec).is_err());

    let create_obj = json_obj(json!({ "steps": ["a", "b"] }));
    let materialized =
        materialize_create_value(&conv_doc.definition, &create_rec, 2, &create_obj)
            .expect("create value");
    assert_eq!(materialized, create_obj);

    assert!(check_record_version(&conv_doc.definition, record.id, &record.kind, 2).is_ok());
    assert!(check_record_version(&conv_doc.definition, record.id, &record.kind, 3).is_err());

    let stored = StoredDocument {
        record,
        version: 2,
        value: create_obj.clone(),
        deltas_since_base: 0,
    };
    let doc_value = materialize_document(&conv_doc.definition, &stored).expect("materialize");
    assert_eq!(doc_value, create_obj);
}

#[test]
fn task_definitions_decode_and_encode_outcomes() {
    let mut phases = IndexMap::new();
    let phase_fn: pi_durable::tasks::TypedPhaseHandler<
        ReviewTaskInput,
        ReviewTaskState,
        ReviewTaskResult,
        (),
    > = Arc::new(
        |_task, _rt, _ctx| -> Pin<Box<dyn Future<Output = pi_js::Result<()>> + Send>> {
            Box::pin(async { Ok(()) })
        },
    );
    phases.insert("start".to_string(), phase_fn);

    let def = define_task(TaskDefinition::<ReviewTaskInput, ReviewTaskState, ReviewTaskResult> {
        name: "review".to_string(),
        version: 1,
        initial: Arc::new(|input| {
            Ok(ReviewTaskState {
                phase: input.target.clone(),
                count: 0,
            })
        }),
        phases,
        abort: Arc::new(|_task, _rt, _ctx| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: std::marker::PhantomData,
    });
    let erased: &ErasedTaskDefinition = &def.erased.definition;
    let init = (erased.initial)(json!({ "target": "diff" })).expect("initial");
    assert_eq!(checkpoint_phase(&init), Some("diff"));

    let rec = TaskRecord {
        id: id_from_number(5),
        conversation_id: id_from_number(1),
        kind: "review".to_string(),
        version: 1,
        input: json!({ "target": "src" }),
        owner: None,
        background: false,
        state: TaskState::Running {
            checkpoint: json!({ "phase": "done", "count": 3 }),
        },
        abort_requested: false,
        memos: None,
    };
    let decoded = decode_task_record::<ReviewTaskInput, ReviewTaskState, ReviewTaskResult>(&rec)
        .expect("decode task record");
    assert_eq!(decoded.input.target, "src");
    assert_eq!(
        decoded.state.checkpoint().map(|c| c.count),
        Some(3)
    );

    let completed = TaskOutcome::Completed {
        result: json!({ "approved": true }),
    };
    let typed_outcome = decode_outcome::<ReviewTaskResult>(&completed).expect("decode");
    assert_eq!(
        typed_outcome,
        TaskOutcome::Completed {
            result: ReviewTaskResult { approved: true }
        }
    );

    let outcome_err = task_outcome_error(&pi_js::Error::msg("boom"));
    assert_eq!(outcome_err.message, "boom");

    let _next: NextTaskState<ReviewTaskState, ReviewTaskResult> = NextTaskState::Terminal {
        outcome: TaskOutcome::Completed {
            result: ReviewTaskResult { approved: false },
        },
    };
    let _op = Op::Set {
        path: Vec::new(),
        value: json!(1),
    };
    let _mock = MockRuntimeHandle {
        id: id_from_number(1),
        outcome: TaskOutcome::Aborted {
            reason: None,
            result: None,
        },
    };
    let _doc_id: DocumentId = id_from_number(1);
    let _entry_id: EntryId = id_from_number(2);
    assert_eq!(DocumentPoint::Current, DocumentPoint::Current);

    let handle = WatchHandle::from_inner(Arc::new(DummyWatchApi(10u32)));
    assert_eq!(handle.value(), 10);
    let _ = mint_typed_id::<pi_durable::ids::EntryKind, JsonValue>;
}
