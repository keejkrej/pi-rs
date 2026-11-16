//! Port of packages/durable/test/examples/17-coding-tools.ts
#![allow(dead_code)]

// A coding-agent tool turn on JSONL storage: the model reads, edits, and runs commands, then answers. A hook times a
// `cat` of a large/missing file (here a nonexistent path in the temp dir, so that call ends in an error result).
// Run via `spec_usage::examples_smoke`.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use indexmap::IndexMap;
use pi_ai::types::{Message, StopReason, UserContentBlock};
use pi_ai::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep, FauxToolCallOptions,
    create_models, faux_assistant_message, faux_provider, faux_text, faux_tool_call,
};
use pi_chord::Context;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::env::ExecutionEnv;
use pi_durable::env::node::NodeExecutionEnv;
use pi_durable::harness::types::EnvTarget;
use pi_durable::storage::jsonl::node::open_node_jsonl_storage;
use pi_durable::storage::jsonl::storage::JsonlStorageOptions;
use pi_durable::tools::coding_tools;
use pi_durable::types::{InputSubmissionState, JsonValue, Storage, SubmissionState};
use pi_durable::{
    AgentChange, ConversationEntriesQuery, Extension, FieldChange, Harness, HarnessOptions, ModelRef, Registry,
    RegistryReader, RootConversationOptions, SubmissionDraft, TOOL_RESULT_ENTRY, TOOL_TASK, ToolHooks, UserInput,
    create_registry, define_extension, hook,
};

fn tool_call_msg(name: &str, args: pi_ai::types::JsonObject, id: &str) -> pi_ai::types::AssistantMessage {
    faux_assistant_message(
        FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
            name,
            args,
            Some(FauxToolCallOptions {
                id: Some(id.to_owned()),
            }),
        ))),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        }),
    )
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();
    let directory = tempfile::TempDir::new().map_err(|err| pi_js::Error::msg(err.to_string()))?;
    let notes_path = directory.path().join("notes.txt");
    tokio::fs::write(&notes_path, "hello world\n")
        .await
        .map_err(|err| pi_js::Error::msg(err.to_string()))?;

    // PORT: Use a nonexistent file inside the temp directory rather than `/tmp/1gb.txt`
    // so runs never read or depend on state outside the test's temp directory.
    let missing_cat_target = directory.path().join("1gb.txt").to_string_lossy().into_owned();

    // The faux provider plays the model: four tool-calling answers, then a final answer.
    let faux = faux_provider(None);
    let models = create_models(None);
    models.set_provider(faux.provider());
    faux.set_responses(vec![
        FauxResponseStep::Message(tool_call_msg(
            "read",
            IndexMap::from([("path".to_owned(), JsonValue::String("notes.txt".to_owned()))]),
            "r",
        )),
        FauxResponseStep::Message(tool_call_msg(
            "edit",
            IndexMap::from([
                ("path".to_owned(), JsonValue::String("notes.txt".to_owned())),
                (
                    "edits".to_owned(),
                    serde_json::json!([{ "oldText": "world", "newText": "durable" }]),
                ),
            ]),
            "e",
        )),
        FauxResponseStep::Message(tool_call_msg(
            "bash",
            IndexMap::from([("command".to_owned(), JsonValue::String("cat notes.txt".to_owned()))]),
            "b",
        )),
        FauxResponseStep::Message(tool_call_msg(
            "bash",
            IndexMap::from([(
                "command".to_owned(),
                JsonValue::String(format!("cat {missing_cat_target}")),
            )]),
            "c",
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("The file now greets durable."))),
            None,
        )),
    ]);

    // Hooks see every call before and after execution; these time the `cat ...1gb.txt`.
    let is_big_cat = |call: &pi_ai::types::ToolCall| {
        call.name == "bash"
            && call
                .arguments
                .get("command")
                .and_then(|v| v.as_str())
                .is_some_and(|cmd| cmd.contains("1gb.txt"))
    };
    let start = Arc::new(AtomicI64::new(0));
    let start_before = Arc::clone(&start);
    let start_after = Arc::clone(&start);
    let timing = define_extension(Extension {
        name: "timing".to_owned(),
        hooks: vec![hook(
            &TOOL_TASK,
            ToolHooks {
                before_tool: Some(Arc::new(move |call, _api, _ctx| {
                    if is_big_cat(&call) {
                        start_before.store(pi_js::time::now_ms(), Ordering::SeqCst);
                    }
                    Box::pin(async { Ok(None) })
                })),
                after_tool: Some(Arc::new(move |call, _result, _api, _ctx| {
                    if is_big_cat(&call) {
                        let elapsed = pi_js::time::now_ms() - start_after.load(Ordering::SeqCst);
                        pi_js::console::log(&format!("cat 1gb.txt took {elapsed} ms"));
                    }
                    Box::pin(async { Ok(None) })
                })),
            },
        )],
        ..Default::default()
    });

    let registry = create_registry();
    // coding_tools() brings read, write, edit, and bash. Tools reach files and processes only
    // through the environment the Harness builds for each call.
    registry.install(coding_tools())?;
    registry.install(timing)?;

    let dir_str = directory.path().to_string_lossy().into_owned();
    let storage = open_node_jsonl_storage(&dir_str, context.clone(), JsonlStorageOptions { fsync: false }).await?;
    let default_cwd = dir_str.clone();
    let env = Arc::new(move |target: EnvTarget, _ctx: Context| {
        let cwd = target.cwd.unwrap_or_else(|| default_cwd.clone());
        Box::pin(async move { Ok(Some(Arc::new(NodeExecutionEnv::new(cwd)) as Arc<dyn ExecutionEnv>)) })
            as pi_js::BoxFuture<pi_js::Result<Option<Arc<dyn ExecutionEnv>>>>
    });
    let harness = Harness::open(
        Arc::new(storage) as Arc<dyn Storage>,
        HarnessOptions {
            models,
            registry: Arc::clone(&registry) as Arc<dyn RegistryReader>,
            settings: None,
            env: Some(env),
            conversation_created: None,
            now: None,
            on_report: None,
        },
        context.clone(),
    )
    .await?;
    let root = harness
        .root(
            context.clone(),
            Some(RootConversationOptions {
                agent: Some(AgentChange {
                    model: FieldChange::Set(ModelRef {
                        provider: "faux".to_owned(),
                        model_id: "faux-1".to_owned(),
                    }),
                    cwd: FieldChange::Set(dir_str.clone()),
                    ..Default::default()
                }),
                init: None,
            }),
        )
        .await?;

    // Each tool call runs as a durable pi.tool task owned by the generation, which waits for them
    // and continues the run with the next generation.
    let settled = root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("Greet durable instead.".to_owned()),
                when_busy: None,
            },
            context.clone(),
        )
        .await?
        .wait(context.clone())
        .await?;
    pi_js::console::log(&format!("status: {:?}", settled.record.status()));

    let transcript = root
        .entries(ConversationEntriesQuery::default(), 20, None, context.clone())
        .await?;
    for entry in transcript.items.iter().rev() {
        if !TOOL_RESULT_ENTRY.matches(Some(entry)) {
            pi_js::console::log(&entry.kind);
            continue;
        }
        if let Some(Message::ToolResult(result)) = entry.model.as_ref().and_then(|m| m.first()) {
            let text: String = result
                .content
                .iter()
                .filter_map(|item| match item {
                    UserContentBlock::Text(t) => Some(t.text.as_str()),
                    UserContentBlock::Image(_) => None,
                })
                .collect();
            let preview = if text.len() > 200 {
                format!("{}…", &text[..text.floor_char_boundary(200)])
            } else {
                text
            };
            pi_js::console::log(&format!(
                "{} {}: {}",
                entry.kind,
                result.tool_name,
                pi_js::json::stringify(&preview)
            ));
        }
    }

    if let SubmissionState::Input(InputSubmissionState::Done { answer, .. }) = settled.record.state {
        let entry = root
            .commit(move |tx| async move { tx.entry(answer).await }, context.clone())
            .await?;
        if let Some(Message::Assistant(msg)) = entry.and_then(|e| e.model).and_then(|mut m| m.drain(..).next()) {
            pi_js::console::log(&format!("answer: {:?}", msg.content));
        }
    }

    let file_contents = tokio::fs::read_to_string(&notes_path)
        .await
        .map_err(|err| pi_js::Error::msg(err.to_string()))?;
    pi_js::console::log(&format!("file: {}", pi_js::json::stringify(&file_contents)));

    harness.close(context).await?;
    pi_js::console::log(&format!("storage: {dir_str}"));
    Ok(())
}
