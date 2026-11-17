//! Port of `packages/coding-agent/test/nested-tool-calls.test.ts`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::future::join_all;
use pi_agent_core::{
    AgentTool, AgentToolCall, AgentToolCallOutcome, AgentToolExecuteFn, AgentToolResult, ToolExecutionMode,
};
use pi_ai::{JsonObject, NestedToolCallStatus, Usage, UsageCost, UserContentBlock, faux_text};
use pi_coding_agent::core::nested_tool_calls::{
    NESTED_CALL_LIMITS, NestedCallRecorder, NestedToolCallHost, NestedToolCallOptions, NestedToolCallRunner,
    NestedToolExecutionEvent, NestedToolUpdateAsyncCallback,
};
use pi_js::abort::AbortSignal;
use pi_js::vendor::typebox::Schema;
use serde_json::{Value, json};

fn make_usage(input: i64, cost: f64) -> Usage {
    Usage {
        input,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: input,
        cost: UsageCost {
            input: cost,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: cost,
        },
    }
}

fn empty_tool(name: &str, mode: Option<ToolExecutionMode>, execute: AgentToolExecuteFn) -> AgentTool {
    AgentTool {
        name: name.to_owned(),
        label: name.to_owned(),
        description: name.to_owned(),
        parameters: pi_js::json::parse_as::<Schema>(r#"{"type":"object","properties":{}}"#).unwrap(),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: mode,
        prepare_arguments: None,
        execute,
        replay: None,
    }
}

struct MockHost {
    tools: Arc<Mutex<Vec<AgentTool>>>,
    sequential: bool,
    events: Arc<Mutex<Vec<NestedToolExecutionEvent>>>,
}

#[async_trait]
impl NestedToolCallHost for MockHost {
    fn get_tools(&self) -> Vec<AgentTool> {
        self.tools.lock().unwrap().clone()
    }

    fn is_sequential(&self) -> bool {
        self.sequential
    }

    async fn run_tool_call(
        &self,
        tool_call: AgentToolCall,
        _parent_tool_call_id: &str,
        signal: Option<AbortSignal>,
        on_update: NestedToolUpdateAsyncCallback,
    ) -> AgentToolCallOutcome {
        let maybe_tool = self
            .tools
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.name == tool_call.name)
            .cloned();
        let Some(tool) = maybe_tool else {
            return AgentToolCallOutcome {
                tool_call: tool_call.clone(),
                result: AgentToolResult::text(format!("Tool {} not found", tool_call.name)),
                is_error: true,
            };
        };

        let params = Value::Object(
            tool_call
                .arguments
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentToolResult>();
        let sync_cb = Arc::new(move |partial: AgentToolResult| {
            let _ = tx.send(partial);
        });
        let exec_res = (tool.execute)(tool_call.id.clone(), params, signal, Some(sync_cb)).await;
        while let Ok(partial) = rx.try_recv() {
            on_update(partial).await;
        }
        match exec_res {
            Ok(result) => {
                let is_err = result.is_error == Some(true);
                AgentToolCallOutcome {
                    tool_call,
                    result,
                    is_error: is_err,
                }
            }
            Err(err) => AgentToolCallOutcome {
                tool_call,
                result: AgentToolResult::text(err.to_string()),
                is_error: true,
            },
        }
    }

    async fn emit(&self, event: NestedToolExecutionEvent) {
        self.events.lock().unwrap().push(event);
    }
}

fn create_runner(
    tools: Vec<AgentTool>,
    sequential: bool,
) -> (
    Arc<NestedToolCallRunner>,
    Arc<Mutex<Vec<AgentTool>>>,
    Arc<Mutex<Vec<NestedToolExecutionEvent>>>,
) {
    let tools_arc = Arc::new(Mutex::new(tools));
    let events = Arc::new(Mutex::new(Vec::new()));
    let host = Arc::new(MockHost {
        tools: Arc::clone(&tools_arc),
        sequential,
        events: Arc::clone(&events),
    });
    (Arc::new(NestedToolCallRunner::new(host)), tools_arc, events)
}

#[tokio::test]
async fn assigns_ids_below_caller_emits_events_and_records_calls() {
    let echo = empty_tool(
        "echo",
        None,
        Arc::new(|_id, _params, _signal, on_update| {
            Box::pin(async move {
                if let Some(cb) = on_update {
                    cb(AgentToolResult::text("partial"));
                }
                Ok(AgentToolResult::text("ok"))
            })
        }),
    );

    let (runner, _, events) = create_runner(vec![echo], false);
    let updates = Arc::new(Mutex::new(Vec::new()));
    let updates_cb = Arc::clone(&updates);

    let first = runner
        .execute(
            "call",
            "echo",
            json!({ "a": 1 }),
            Some(NestedToolCallOptions {
                signal: None,
                on_update: Some(Arc::new(move |partial| updates_cb.lock().unwrap().push(partial))),
            }),
        )
        .await;
    let missing = runner.execute("call", "missing", json!({}), None).await;

    assert_eq!(first.tool_call.id, "call/1");
    assert_eq!(missing.tool_call.id, "call/2");
    assert!(missing.is_error);
    assert_eq!(updates.lock().unwrap().len(), 1);

    let ev_summary: Vec<(&'static str, String, String)> = events
        .lock()
        .unwrap()
        .iter()
        .map(|ev| match ev {
            NestedToolExecutionEvent::ToolExecutionStart {
                tool_call_id,
                parent_tool_call_id,
                ..
            } => (
                "tool_execution_start",
                tool_call_id.clone(),
                parent_tool_call_id.clone(),
            ),
            NestedToolExecutionEvent::ToolExecutionUpdate {
                tool_call_id,
                parent_tool_call_id,
                ..
            } => (
                "tool_execution_update",
                tool_call_id.clone(),
                parent_tool_call_id.clone(),
            ),
            NestedToolExecutionEvent::ToolExecutionEnd {
                tool_call_id,
                parent_tool_call_id,
                ..
            } => ("tool_execution_end", tool_call_id.clone(), parent_tool_call_id.clone()),
        })
        .collect();
    assert_eq!(
        ev_summary,
        vec![
            ("tool_execution_start", "call/1".to_owned(), "call".to_owned()),
            ("tool_execution_update", "call/1".to_owned(), "call".to_owned()),
            ("tool_execution_end", "call/1".to_owned(), "call".to_owned()),
            ("tool_execution_start", "call/2".to_owned(), "call".to_owned()),
            ("tool_execution_end", "call/2".to_owned(), "call".to_owned()),
        ]
    );

    let summary = runner.take_record("call").unwrap();
    let nested = summary.calls.unwrap();
    assert!(nested.complete);
    assert_eq!(nested.calls.len(), 2);
    assert_eq!(nested.calls[0].id, "call/1");
    assert_eq!(nested.calls[0].name, "echo");
    assert_eq!(nested.calls[0].status, NestedToolCallStatus::Ok);
    assert!(nested.calls[0].duration_ms.is_some());
    assert_eq!(nested.calls[0].arguments.as_ref().unwrap().get("a"), Some(&json!(1)));
    assert_eq!(nested.calls[1].id, "call/2");
    assert_eq!(nested.calls[1].name, "missing");
    assert_eq!(nested.calls[1].status, NestedToolCallStatus::Error);
    assert_eq!(nested.calls[1].error.as_deref(), Some("Tool missing not found"));

    assert!(runner.take_record("call").is_none());
    assert!(runner.take_record("other").is_none());
}

#[tokio::test]
async fn records_calls_of_nested_tools_on_the_model_issued_call() {
    let leaf = empty_tool(
        "leaf",
        None,
        Arc::new(|_, _, _, _| Box::pin(async { Ok(AgentToolResult::text("")) })),
    );
    let (runner, tools_arc, _) = create_runner(vec![leaf], false);
    let runner_for_middle = Arc::clone(&runner);
    tools_arc.lock().unwrap().push(empty_tool(
        "middle",
        None,
        Arc::new(move |id, _, _, _| {
            let r = Arc::clone(&runner_for_middle);
            Box::pin(async move {
                r.execute(&id, "leaf", json!({}), None).await;
                Ok(AgentToolResult::text(""))
            })
        }),
    ));

    runner.execute("call", "middle", json!({}), None).await;
    let ids: Vec<String> = runner
        .take_record("call")
        .unwrap()
        .calls
        .unwrap()
        .calls
        .into_iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(ids, vec!["call/1".to_owned(), "call/1/1".to_owned()]);
}

#[tokio::test]
async fn sums_the_usage_of_nested_results_at_every_depth() {
    let leaf = empty_tool(
        "leaf",
        None,
        Arc::new(|_, _, _, _| {
            Box::pin(async {
                let mut res = AgentToolResult::text("");
                res.usage = Some(make_usage(10, 0.01));
                Ok(res)
            })
        }),
    );
    let plain = empty_tool(
        "plain",
        None,
        Arc::new(|_, _, _, _| Box::pin(async { Ok(AgentToolResult::text("")) })),
    );
    let (runner, tools_arc, _) = create_runner(vec![leaf, plain], false);
    let runner_for_middle = Arc::clone(&runner);
    tools_arc.lock().unwrap().push(empty_tool(
        "middle",
        None,
        Arc::new(move |id, _, _, _| {
            let r = Arc::clone(&runner_for_middle);
            Box::pin(async move {
                r.execute(&id, "leaf", json!({}), None).await;
                let mut res = AgentToolResult::text("");
                res.usage = Some(make_usage(5, 0.005));
                Ok(res)
            })
        }),
    ));

    runner.execute("call", "middle", json!({}), None).await;
    runner.execute("call", "leaf", json!({}), None).await;
    runner.execute("call", "plain", json!({}), None).await;
    runner.execute("free", "plain", json!({}), None).await;

    let summary = runner.take_record("call").unwrap();
    let usage = summary.usage.unwrap();
    assert_eq!(usage.input, 25);
    assert!((usage.cost.total - 0.025).abs() < 1e-10);
    let free = runner.take_record("free").unwrap();
    assert!(free.calls.unwrap().complete);
    assert!(free.usage.is_none());
}

#[tokio::test]
async fn serializes_concurrent_calls_to_sequential_tools() {
    let active = Arc::new(Mutex::new(0usize));
    let max_seq = Arc::new(Mutex::new(0usize));
    let max_par = Arc::new(Mutex::new(0usize));

    let make_tool = |name: &'static str, mode: Option<ToolExecutionMode>, max_ref: Arc<Mutex<usize>>| {
        let active_ref = Arc::clone(&active);
        empty_tool(
            name,
            mode,
            Arc::new(move |_, _, _, _| {
                let a = Arc::clone(&active_ref);
                let m = Arc::clone(&max_ref);
                Box::pin(async move {
                    {
                        let mut ag = a.lock().unwrap();
                        *ag += 1;
                        let mut mg = m.lock().unwrap();
                        *mg = (*mg).max(*ag);
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    {
                        let mut ag = a.lock().unwrap();
                        *ag -= 1;
                    }
                    Ok(AgentToolResult {
                        content: vec![UserContentBlock::Text(faux_text(""))],
                        details: None,
                        structured_content: None,
                        usage: None,
                        is_error: None,
                        terminate: None,
                        control: None,
                    })
                })
            }),
        )
    };

    let (runner, _, _) = create_runner(
        vec![
            make_tool("sequential", Some(ToolExecutionMode::Sequential), Arc::clone(&max_seq)),
            make_tool("parallel", None, Arc::clone(&max_par)),
        ],
        false,
    );

    join_all((0..3).map(|_| runner.execute("call", "sequential", json!({}), None))).await;
    join_all((0..3).map(|_| runner.execute("call", "parallel", json!({}), None))).await;

    assert_eq!(*max_seq.lock().unwrap(), 1);
    assert_eq!(*max_par.lock().unwrap(), 3);
}

#[test]
fn recorder_omits_oversized_arguments_and_drops_calls_beyond_limit() {
    let call = |id: &str, args: Value| -> AgentToolCall {
        let map: JsonObject = match args {
            Value::Object(m) => m.into_iter().collect(),
            _ => JsonObject::new(),
        };
        AgentToolCall {
            id: id.to_owned(),
            name: "t".to_owned(),
            arguments: map,
        }
    };

    let mut recorder = NestedCallRecorder::new();
    assert!(recorder.snapshot().is_none());

    let small = recorder.start(&call("a", json!({ "x": 1 })));
    recorder.finish(small, false, "");
    let snap1 = recorder.snapshot().unwrap();
    assert!(snap1.complete);
    assert_eq!(snap1.calls.len(), 1);
    assert_eq!(snap1.calls[0].id, "a");
    assert_eq!(snap1.calls[0].status, NestedToolCallStatus::Ok);

    let big = recorder.start(&call(
        "b",
        json!({ "text": "x".repeat(NESTED_CALL_LIMITS.max_argument_bytes_per_call) }),
    ));
    recorder.finish(big, true, &"e".repeat(1000));
    let snap2 = recorder.snapshot().unwrap();
    assert!(!snap2.complete);
    assert_eq!(snap2.calls[1].id, "b");
    assert_eq!(snap2.calls[1].status, NestedToolCallStatus::Error);
    assert!(snap2.calls[1].arguments.is_none());
    assert!(snap2.calls[1].arguments_bytes.unwrap() as usize > NESTED_CALL_LIMITS.max_argument_bytes_per_call);
    assert_eq!(
        snap2.calls[1].error.as_ref().unwrap().len(),
        NESTED_CALL_LIMITS.max_error_chars
    );

    for i in 0..NESTED_CALL_LIMITS.max_calls {
        let h = recorder.start(&call(&format!("c{i}"), json!({})));
        recorder.finish(h, false, "");
    }
    assert_eq!(recorder.snapshot().unwrap().calls.len(), NESTED_CALL_LIMITS.max_calls);
}

#[test]
fn recorder_caps_total_argument_size_and_marks_unfinished_calls_incomplete() {
    let call = |id: &str, args: Value| -> AgentToolCall {
        let map: JsonObject = match args {
            Value::Object(m) => m.into_iter().collect(),
            _ => JsonObject::new(),
        };
        AgentToolCall {
            id: id.to_owned(),
            name: "t".to_owned(),
            arguments: map,
        }
    };

    let mut recorder = NestedCallRecorder::new();
    let chunk = json!({ "text": "x".repeat(7000) });
    let records: Vec<Option<usize>> = (0..6)
        .map(|i| recorder.start(&call(&format!("c{i}"), chunk.clone())))
        .collect();
    let snapshot = recorder.snapshot().unwrap();
    let with_args = snapshot.calls.iter().filter(|c| c.arguments.is_some()).count();
    assert_eq!(with_args, 4);
    assert!(
        snapshot
            .calls
            .iter()
            .all(|c| c.status == NestedToolCallStatus::Unfinished)
    );
    assert!(!snapshot.complete);
    assert_eq!(records.len(), 6);
}
