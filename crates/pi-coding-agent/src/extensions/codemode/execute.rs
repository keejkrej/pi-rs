//! Port of packages/coding-agent/src/extensions/codemode/execute.ts

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use indexmap::IndexMap;
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::core::extensions::types::{
    ExecuteToolOptions, ExtensionToolContext, ModelRegistry, SessionEntry, ToolNamespace,
};
use crate::core::tools::truncate::{format_size, slice_head, slice_tail};
use crate::core::usage_totals::combine_usage;
use crate::extensions::codemode::tool::{
    CODEMODE_STORE_ENTRY_TYPE, CodemodeModelRuntime, CodemodeNestedCall, CodemodeNestedCallStatus,
    CodemodeStoreEntryData, CodemodeToolDetails, CodemodeToolInput, CodemodeToolOptions, codemode_docs_path,
    get_codemode_callable_tools, to_codemode_declaration,
};
use crate::extensions::tool_search::tool::{
    Bm25Ranker, DEFAULT_TOOL_SEARCH_LIMIT, ToolSearchDocumentInput, create_tool_search_document,
};
use crate::utils::output_files::write_output_file;
use pi_agent_core::{AgentTool, AgentToolCallOutcome, AgentToolResult};
use pi_ai::models::{ModelsClassifierOptions, ModelsImagesOptions};
use pi_ai::types::{
    AnyModel, ClassifierContext, ClassifierModel, ClassifierOptions, ImageModel, ImagesContext, ImagesOptions,
    ModelType,
};
use pi_ai::{AuthOperationOptions, ImageContent, TextContent, Usage, UserContentBlock};
use pi_codemode::declarations::{render_tool_sample, to_codemode_identifier};
use pi_codemode::runtime::host::CodemodeSandbox;
use pi_codemode::source::parse_codemode_source;
use pi_codemode::types::{
    CodemodeErrorKind, CodemodeExecuteOptions, CodemodeOutputItem, CodemodeResult, CodemodeSandboxOptions,
    CodemodeTool, CodemodeWorkerHooks,
};
use pi_js::abort::AbortSignal;
use pi_js::{Error, Result};

const ARGS_PREVIEW_CHARS: usize = 200;
const ERROR_PREVIEW_CHARS: usize = 500;
const MAX_CONCURRENT_MODEL_CALLS: usize = 4;
const CODEMODE_MEMORY_LIMIT_BYTES: f64 = 256.0 * 1024.0 * 1024.0;
const DEFAULT_MAX_OUTPUT_TOKENS: usize = 10_000;
const CHARS_PER_TOKEN: usize = 4;
// Shared across calls and sessions. Allow one replacement while a cancelled worker is still exiting.
const DEFAULT_MAX_CODEMODE_WORKERS: usize = 2;
/// How long a script waits for a free worker before failing.
const CODEMODE_WORKER_WAIT_MS: u64 = 60_000;

struct CodemodeWorkerPool {
    active: usize,
    next_id: u64,
    waiters: VecDeque<(u64, tokio::sync::oneshot::Sender<()>)>,
}

static CODEMODE_WORKER_POOL: LazyLock<Mutex<CodemodeWorkerPool>> = LazyLock::new(|| {
    Mutex::new(CodemodeWorkerPool {
        active: 0,
        next_id: 0,
        waiters: VecDeque::new(),
    })
});

/// `PI_CODEMODE_MAX_WORKERS` if it is a positive integer. Read per call so hosts can set it at runtime.
fn max_codemode_workers() -> usize {
    pi_js::env::var("PI_CODEMODE_MAX_WORKERS")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MAX_CODEMODE_WORKERS)
}

fn admit_codemode_workers(pool: &mut CodemodeWorkerPool) {
    let max = max_codemode_workers();
    while pool.active < max {
        let Some((_id, tx)) = pool.waiters.pop_front() else {
            break;
        };
        if tx.send(()).is_ok() {
            pool.active += 1;
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkerReservationOutcome {
    Reserved,
    Aborted,
    Timeout,
}

async fn wait_signal_aborted(signal: Option<&AbortSignal>) {
    match signal {
        Some(sig) => sig.cancelled().await,
        None => std::future::pending::<()>().await,
    }
}

async fn reserve_codemode_worker(signal: Option<&AbortSignal>, timeout_ms: f64) -> WorkerReservationOutcome {
    if signal.is_some_and(AbortSignal::aborted) {
        return WorkerReservationOutcome::Aborted;
    }
    let (id, rx) = {
        let mut pool = CODEMODE_WORKER_POOL.lock().unwrap_or_else(|e| e.into_inner());
        let max = max_codemode_workers();
        if pool.waiters.is_empty() && pool.active < max {
            pool.active += 1;
            return WorkerReservationOutcome::Reserved;
        }
        pool.next_id += 1;
        let id = pool.next_id;
        let (tx, rx) = tokio::sync::oneshot::channel();
        pool.waiters.push_back((id, tx));
        admit_codemode_workers(&mut pool);
        (id, rx)
    };

    struct WaiterGuard {
        id: u64,
        rx: Option<tokio::sync::oneshot::Receiver<()>>,
        reserved: bool,
    }

    impl Drop for WaiterGuard {
        fn drop(&mut self) {
            if self.reserved {
                return;
            }
            let mut pool = CODEMODE_WORKER_POOL.lock().unwrap_or_else(|e| e.into_inner());
            let before = pool.waiters.len();
            pool.waiters.retain(|(wid, _)| *wid != self.id);
            let removed_from_waiters = pool.waiters.len() < before;
            if !removed_from_waiters
                && let Some(ref mut rx) = self.rx
                && rx.try_recv().is_ok()
            {
                pool.active = pool.active.saturating_sub(1);
                admit_codemode_workers(&mut pool);
            }
        }
    }

    let mut guard = WaiterGuard {
        id,
        rx: Some(rx),
        reserved: false,
    };
    let delay = Duration::from_millis(timeout_ms.max(0.0).ceil() as u64);
    let rx_mut = guard.rx.as_mut().expect("receiver present");
    tokio::select! {
        res = rx_mut => {
            if res.is_ok() {
                guard.reserved = true;
                WorkerReservationOutcome::Reserved
            } else {
                WorkerReservationOutcome::Timeout
            }
        }
        _ = tokio::time::sleep(delay) => WorkerReservationOutcome::Timeout,
        _ = wait_signal_aborted(signal) => WorkerReservationOutcome::Aborted,
    }
}

fn release_codemode_worker() {
    let mut pool = CODEMODE_WORKER_POOL.lock().unwrap_or_else(|e| e.into_inner());
    pool.active = pool.active.saturating_sub(1);
    admit_codemode_workers(&mut pool);
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    if pi_js::str16::len(text) > max_chars {
        let prefix = slice_head(text, max_chars.saturating_sub(3));
        format!("{prefix}...")
    } else {
        text.to_string()
    }
}

fn preview_args(args: Option<&Value>) -> String {
    let Some(args) = args else {
        return String::new();
    };
    truncate_text(&pi_js::json::stringify(args), ARGS_PREVIEW_CHARS)
}

fn text_of(result: &AgentToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn to_model_type(value: Option<&Value>) -> Result<&'static str> {
    match value.and_then(Value::as_str) {
        Some("chat") => Ok("chat"),
        Some("image") => Ok("image"),
        Some("classifier") => Ok("classifier"),
        other => {
            let repr = match other {
                Some(s) => pi_js::json::stringify(&Value::String(s.to_string())),
                None => value
                    .map(pi_js::json::stringify)
                    .unwrap_or_else(|| "undefined".to_string()),
            };
            Err(Error::msg(format!(
                "Unknown model type {repr}. Use \"chat\", \"image\", or \"classifier\"."
            )))
        }
    }
}

fn to_provider(value: Option<&Value>) -> Result<Option<String>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(Error::msg("provider must be a string")),
    }
}

/// Drop `headers` because models.json headers can carry credentials.
fn redact_model_headers(mut value: Value) -> Value {
    if let Some(obj) = value.as_object_mut() {
        obj.shift_remove("headers");
    }
    value
}

fn with_article(word: &str) -> String {
    let starts_vowel = word
        .as_bytes()
        .first()
        .is_some_and(|b| matches!(b, b'a' | b'e' | b'i' | b'o' | b'u'));
    if starts_vowel {
        format!("an {word}")
    } else {
        format!("a {word}")
    }
}

fn describe_value(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return "undefined".to_string();
    };
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(_) => "a boolean".to_string(),
        Value::Number(_) => "a number".to_string(),
        Value::String(_) => "a string".to_string(),
        Value::Array(arr) => {
            if arr.is_empty() {
                "an empty array".to_string()
            } else {
                "an array".to_string()
            }
        }
        Value::Object(map) => {
            if map.is_empty() {
                "{}".to_string()
            } else {
                let mut keys: Vec<&str> = map.keys().take(6).map(String::as_str).collect();
                if map.len() > 6 {
                    keys.push("...");
                }
                format!("{{ {} }}", keys.join(", "))
            }
        }
    }
}

const CLASSIFIER_CONTEXT_SHAPE: &str = "{ state: { ... }, images?: [{ type: \"image\", data: <base64>, mimeType }], questions: { <id>: { type: \"choice\", instructions, criteria: { <label>: <meaning> } } | { type: \"score\", instructions, criteria: [<lowest level>, ..., <highest level>] } | { type: \"bool\", instructions, criteria: { true: <meaning>, false: <meaning> } } } }";

fn check_classifier_context(context: Option<&Value>) -> Result<Value> {
    let docs = codemode_docs_path();
    let fail = |problem: String| {
        Error::msg(format!(
            "models.classify() {problem}. Expected context: {CLASSIFIER_CONTEXT_SHAPE}. See \"Classify\" in {docs}."
        ))
    };
    let Some(obj) = context.and_then(Value::as_object) else {
        return Err(fail(format!(
            "expects a context object as its second argument, got {}",
            describe_value(context)
        )));
    };
    if !obj.get("state").is_some_and(Value::is_object) {
        return Err(fail(format!(
            "context.state must be an object, got {}",
            describe_value(obj.get("state"))
        )));
    }
    if let Some(images) = obj.get("images") {
        let Some(arr) = images.as_array() else {
            return Err(fail(format!(
                "context.images must be an array, got {}",
                describe_value(Some(images))
            )));
        };
        for (index, image) in arr.iter().enumerate() {
            let valid = image.as_object().is_some_and(|img| {
                img.get("type").and_then(Value::as_str) == Some("image")
                    && img.get("data").is_some_and(Value::is_string)
                    && img.get("mimeType").is_some_and(Value::is_string)
            });
            if !valid {
                return Err(fail(format!(
                    "context.images[{index}] must be an image block, got {}",
                    describe_value(Some(image))
                )));
            }
        }
    }
    let Some(questions) = obj.get("questions").and_then(Value::as_object) else {
        return Err(fail(format!(
            "context.questions must map question IDs to questions, got {}",
            describe_value(obj.get("questions"))
        )));
    };
    if questions.is_empty() {
        return Err(fail(format!(
            "context.questions must map question IDs to questions, got {}",
            describe_value(obj.get("questions"))
        )));
    }
    let is_strings = |vals: &[Value]| !vals.is_empty() && vals.iter().all(Value::is_string);
    for (id, question) in questions {
        let at = format!("context.questions.{id}");
        let Some(q) = question.as_object() else {
            return Err(fail(format!(
                "{at} must be a question object, got {}",
                describe_value(Some(question))
            )));
        };
        if !q.get("instructions").is_some_and(Value::is_string) {
            return Err(fail(format!("{at}.instructions must be a string")));
        }
        let criteria = q.get("criteria");
        match q.get("type").and_then(Value::as_str) {
            Some("choice") => {
                let valid = criteria.and_then(Value::as_object).is_some_and(|c| {
                    let vals: Vec<Value> = c.values().cloned().collect();
                    is_strings(&vals)
                });
                if !valid {
                    return Err(fail(format!(
                        "{at} is a \"choice\" question, so criteria must map each label to its meaning"
                    )));
                }
            }
            Some("score") => {
                let valid = criteria.and_then(Value::as_array).is_some_and(|a| is_strings(a));
                if !valid {
                    return Err(fail(format!(
                        "{at} is a \"score\" question, so criteria must list the levels as strings, lowest first"
                    )));
                }
            }
            Some("bool") => {
                let valid = criteria.and_then(Value::as_object).is_some_and(|c| {
                    c.get("true").is_some_and(Value::is_string) && c.get("false").is_some_and(Value::is_string)
                });
                if !valid {
                    return Err(fail(format!(
                        "{at} is a \"bool\" question, so criteria must be {{ true: string, false: string }}"
                    )));
                }
            }
            other => {
                let repr = match other {
                    Some(s) => pi_js::json::stringify(&Value::String(s.to_string())),
                    None => q
                        .get("type")
                        .map(pi_js::json::stringify)
                        .unwrap_or_else(|| "undefined".to_string()),
                };
                return Err(fail(format!(
                    "{at}.type must be \"choice\", \"score\", or \"bool\", got {repr}"
                )));
            }
        }
    }
    Ok(Value::Object(obj.clone()))
}

fn check_images_context(context: Option<&Value>) -> Result<Value> {
    let docs = codemode_docs_path();
    let fail = |problem: String| {
        Error::msg(format!(
            "models.generateImages() {problem}. Expected context: {{ input: [{{ type: \"text\", text: <prompt> }}, ...optional {{ type: \"image\", data: <base64>, mimeType }} references] }}. See \"Generate images\" in {docs}."
        ))
    };
    let Some(obj) = context.and_then(Value::as_object) else {
        return Err(fail(format!(
            "expects a context object as its second argument, got {}",
            describe_value(context)
        )));
    };
    let Some(input) = obj.get("input").and_then(Value::as_array) else {
        return Err(fail(format!(
            "context.input must be a non-empty array of blocks, got {}",
            describe_value(obj.get("input"))
        )));
    };
    if input.is_empty() {
        return Err(fail(format!(
            "context.input must be a non-empty array of blocks, got {}",
            describe_value(obj.get("input"))
        )));
    }
    for (index, block) in input.iter().enumerate() {
        let valid_text = block.as_object().is_some_and(|b| {
            b.get("type").and_then(Value::as_str) == Some("text") && b.get("text").is_some_and(Value::is_string)
        });
        let valid_image = block.as_object().is_some_and(|b| {
            b.get("type").and_then(Value::as_str) == Some("image")
                && b.get("data").is_some_and(Value::is_string)
                && b.get("mimeType").is_some_and(Value::is_string)
        });
        if !valid_text && !valid_image {
            return Err(fail(format!(
                "context.input[{index}] must be a text or image block, got {}",
                describe_value(Some(block))
            )));
        }
    }
    Ok(Value::Object(obj.clone()))
}

/// Values of `load()`: the `codemode-store` entries on the branch, applied from the root.
pub fn read_codemode_store(branch: &[SessionEntry]) -> IndexMap<String, Value> {
    let mut store = IndexMap::new();
    for entry in branch {
        let SessionEntry::Custom(custom) = entry else {
            continue;
        };
        if custom.custom_type != CODEMODE_STORE_ENTRY_TYPE {
            continue;
        }
        let Some(ref data) = custom.data else {
            continue;
        };
        let Ok(parsed) = pi_js::json::parse_as::<CodemodeStoreEntryData>(&pi_js::json::stringify(data)) else {
            continue;
        };
        for key in parsed.r#delete {
            store.shift_remove(&key);
        }
        for (k, v) in parsed.set {
            store.insert(k, v);
        }
    }
    store
}

/// Like the script's `text()`: strings as is, other values as compact JSON.
fn value_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => pi_js::json::stringify(other),
    }
}

fn format_call_summary(calls: &[CodemodeNestedCall]) -> String {
    if calls.is_empty() {
        return "No tool calls were made.".to_string();
    }
    let summary: Vec<String> = calls
        .iter()
        .map(|call| {
            let status = match call.status {
                CodemodeNestedCallStatus::Running => "running",
                CodemodeNestedCallStatus::Ok => "ok",
                CodemodeNestedCallStatus::Error => "error",
                CodemodeNestedCallStatus::Cancelled => "cancelled",
            };
            format!("{} ({status})", call.name)
        })
        .collect();
    format!(
        "Tool calls made before the failure (they are not undone): {}",
        summary.join(", ")
    )
}

fn format_error(result: &CodemodeResult, calls: &[CodemodeNestedCall]) -> String {
    let CodemodeResult::Failure { error, .. } = result else {
        return String::new();
    };
    let head = match error.kind {
        CodemodeErrorKind::Script => error.stack.clone().unwrap_or_else(|| {
            let name = error.name.as_deref().unwrap_or("Error");
            format!("{name}: {}", error.message)
        }),
        CodemodeErrorKind::Timeout => format!("Script timed out: {}", error.message),
        CodemodeErrorKind::Aborted => format!("Script aborted: {}", error.message),
        CodemodeErrorKind::Sandbox => format!("Script sandbox failed: {}", error.message),
    };
    format!("{head}\n\n{}", format_call_summary(calls))
}

/// Lay out the script's output so the model can tell items apart. With more than one text item
/// (`text()` or the returned value), each starts with `==> text N/M <==`. `console.*` lines follow
/// all other output in one `<console_output>` block.
fn format_output(output: &[(CodemodeOutputItem, bool)]) -> Vec<UserContentBlock> {
    let total = output
        .iter()
        .filter(|(item, is_console)| matches!(item, CodemodeOutputItem::Text { .. }) && !is_console)
        .count();
    let mut items = Vec::new();
    let mut console_lines = Vec::new();
    let mut index = 0usize;
    for (item, is_console) in output {
        match item {
            CodemodeOutputItem::Image { data, mime_type } => {
                items.push(UserContentBlock::Image(ImageContent::new(
                    data.clone(),
                    mime_type.clone(),
                )));
            }
            CodemodeOutputItem::Text { text } if *is_console => {
                console_lines.push(text.as_str());
            }
            CodemodeOutputItem::Text { text } => {
                index += 1;
                let formatted = if total > 1 {
                    format!("==> text {index}/{total} <==\n{text}")
                } else {
                    text.clone()
                };
                items.push(UserContentBlock::Text(TextContent::new(formatted)));
            }
        }
    }
    if !console_lines.is_empty() {
        items.push(UserContentBlock::Text(TextContent::new(format!(
            "<console_output>\n{}\n</console_output>",
            console_lines.join("\n")
        ))));
    }
    items
}

/// Join adjacent text items into one, each part starting on its own line.
fn join_adjacent_text(items: Vec<UserContentBlock>) -> Vec<UserContentBlock> {
    let mut joined = Vec::new();
    let mut parts = String::new();
    let mut last_non_empty_ends_newline = false;
    let mut has_last = false;
    let flush =
        |joined: &mut Vec<UserContentBlock>, parts: &mut String, has_last: &mut bool, last_ends_nl: &mut bool| {
            if !parts.is_empty() {
                joined.push(UserContentBlock::Text(TextContent::new(std::mem::take(parts))));
            }
            *has_last = false;
            *last_ends_nl = false;
        };
    for item in items {
        match item {
            UserContentBlock::Text(t) => {
                if has_last && !last_non_empty_ends_newline {
                    parts.push('\n');
                    last_non_empty_ends_newline = true;
                }
                if !t.text.is_empty() {
                    last_non_empty_ends_newline = t.text.ends_with('\n');
                    has_last = true;
                    parts.push_str(&t.text);
                }
            }
            other => {
                flush(&mut joined, &mut parts, &mut has_last, &mut last_non_empty_ends_newline);
                joined.push(other);
            }
        }
    }
    flush(&mut joined, &mut parts, &mut has_last, &mut last_non_empty_ends_newline);
    joined
}

fn image_extension(mime_type: &str) -> Option<&'static str> {
    match mime_type {
        "image/png" => Some(".png"),
        "image/jpeg" => Some(".jpg"),
        "image/gif" => Some(".gif"),
        "image/webp" => Some(".webp"),
        _ => None,
    }
}

/// Save each image to a temp file and put a text item with its path before it.
async fn save_images(items: Vec<UserContentBlock>) -> Result<Vec<UserContentBlock>> {
    let mut labels: IndexMap<String, String> = IndexMap::new();
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item {
            UserContentBlock::Image(ref img) => {
                let label = if let Some(cached) = labels.get(&img.data) {
                    cached.clone()
                } else {
                    let ext = image_extension(&img.mime_type)
                        .ok_or_else(|| Error::msg(format!("No file extension for image type {}", img.mime_type)))?;
                    let decoded = base64::engine::general_purpose::STANDARD.decode(&img.data);
                    let built = match decoded {
                        Ok(bytes) => {
                            let kind = format!("{}, {}", img.mime_type, format_size(bytes.len()));
                            match write_output_file("pi-codemode", ext, &bytes).await {
                                Ok(path) => format!("[Image saved to {path} ({kind})]"),
                                Err(err) => {
                                    format!("[Image ({kind}) could not be saved: {}]", err.message())
                                }
                            }
                        }
                        Err(err) => {
                            let kind = format!("{}, 0B", img.mime_type);
                            format!("[Image ({kind}) could not be saved: {err}]")
                        }
                    };
                    labels.insert(img.data.clone(), built.clone());
                    built
                };
                out.push(UserContentBlock::Text(TextContent::new(label)));
                out.push(item);
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

async fn spill_output(text: &str) -> std::result::Result<String, String> {
    write_output_file("pi-codemode", ".txt", text.as_bytes())
        .await
        .map_err(|e| e.message().to_string())
}

async fn truncate_output(items: Vec<UserContentBlock>, max_tokens: usize) -> (Vec<UserContentBlock>, Option<String>) {
    let texts: Vec<&str> = items
        .iter()
        .filter_map(|item| match item {
            UserContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    let combined = texts.join("\n");
    let combined_len = pi_js::str16::len(&combined);
    let budget = max_tokens.saturating_mul(CHARS_PER_TOKEN);
    if texts.is_empty() || combined_len <= budget {
        return (items, None);
    }
    let head_chars = budget / 2;
    let tail_chars = budget - head_chars;
    let removed = combined_len.saturating_sub(head_chars + tail_chars);
    let head = slice_head(&combined, head_chars);
    let tail = slice_tail(&combined, tail_chars);
    let original_tokens = combined_len.div_ceil(CHARS_PER_TOKEN);
    let total_lines = combined.split('\n').count();
    let removed_tokens = removed.div_ceil(CHARS_PER_TOKEN);
    let mut text = format!(
        "Warning: truncated output (original token count: {original_tokens})\nTotal output lines: {total_lines}\n\n{head}…{removed_tokens} tokens truncated…{tail}"
    );
    let (full_output_path, suffix) = match spill_output(&combined).await {
        Ok(path) => (
            Some(path.clone()),
            format!("\n\n[Full output: {path} (read with offset/limit)]"),
        ),
        Err(err) => (None, format!("\n\n[Could not save the full output: {err}]")),
    };
    text.push_str(&suffix);
    let mut out = vec![UserContentBlock::Text(TextContent::new(text))];
    for item in items {
        if matches!(item, UserContentBlock::Image(_)) {
            out.push(item);
        }
    }
    (out, full_output_path)
}

fn to_script_value(tool: &AgentTool, outcome: &AgentToolCallOutcome) -> Result<Option<Value>> {
    if tool.output_schema.is_some()
        && let Some(ref structured) = outcome.result.structured_content
    {
        return Ok(Some(structured.clone()));
    }
    let text = text_of(&outcome.result);
    if outcome.is_error {
        let msg = if text.is_empty() {
            format!("Tool \"{}\" failed", tool.name)
        } else {
            text
        };
        return Err(Error::msg(msg));
    }
    Ok(Some(Value::String(text)))
}

fn is_namespace_name(namespace: &ToolNamespace, query: &str) -> bool {
    let query_id = to_codemode_identifier(query);
    if namespace.name == query || to_codemode_identifier(&namespace.name) == query_id {
        return true;
    }
    namespace
        .aliases
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .any(|alias| alias == query || to_codemode_identifier(alias) == query_id)
}

fn create_discovery_globals(
    tools: Vec<AgentTool>,
    samples: IndexMap<String, String>,
    options: &CodemodeToolOptions,
) -> Vec<CodemodeTool> {
    let samples_arc = Arc::new(samples);
    let get_ns = options.get_tool_namespace.clone();
    let tools_search = tools.clone();
    let samples_search = Arc::clone(&samples_arc);

    let search_tools = CodemodeTool {
        name: "searchTools".to_string(),
        description: None,
        input_schema: None,
        output_schema: None,
        spread: Some(true),
        signature: None,
        execute: Arc::new(move |args, _ctx| {
            let tools = tools_search.clone();
            let samples = Arc::clone(&samples_search);
            let get_ns = get_ns.clone();
            Box::pin(async move {
                let arr = args.and_then(|v| v.as_array().cloned()).unwrap_or_default();
                let query = arr
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::msg("searchTools() expects a query string"))?
                    .to_string();
                let search_opts = arr.get(1).and_then(Value::as_object);
                let limit = match search_opts.and_then(|o| o.get("limit")) {
                    None | Some(Value::Null) => DEFAULT_TOOL_SEARCH_LIMIT,
                    Some(v) => {
                        let Some(n) = v.as_f64() else {
                            return Err(Error::msg("searchTools() limit must be a positive integer"));
                        };
                        if !n.is_finite() || n.fract() != 0.0 || n <= 0.0 {
                            return Err(Error::msg("searchTools() limit must be a positive integer"));
                        }
                        n as usize
                    }
                };
                let namespace_filter = match search_opts.and_then(|o| o.get("namespace")) {
                    None | Some(Value::Null) => None,
                    Some(Value::String(s)) => Some(s.clone()),
                    Some(_) => return Err(Error::msg("searchTools() namespace must be a string")),
                };

                let documents: Vec<_> = tools
                    .iter()
                    .filter_map(|tool| {
                        let tool_ns = get_ns.as_ref().and_then(|f| f(&tool.name));
                        if let Some(ref ns_query) = namespace_filter
                            && !ns_query.is_empty()
                        {
                            let Some(ref ns) = tool_ns else {
                                return None;
                            };
                            if !is_namespace_name(ns, ns_query) {
                                return None;
                            }
                        }
                        Some(create_tool_search_document(
                            ToolSearchDocumentInput {
                                name: &tool.name,
                                description: &tool.description,
                                parameters: &tool.parameters,
                            },
                            tool_ns.as_ref(),
                        ))
                    })
                    .collect();

                let matches = Bm25Ranker::default().rank(&query, &documents, limit);
                let results: Vec<Value> = matches
                    .into_iter()
                    .map(|m| {
                        json!({
                            "name": to_codemode_identifier(&m.name),
                            "description": samples.get(&m.name).cloned().unwrap_or_default(),
                        })
                    })
                    .collect();
                Ok(Some(Value::Array(results)))
            })
        }),
    };

    let tools_describe = tools.clone();
    let samples_describe = Arc::clone(&samples_arc);
    let describe_tool = CodemodeTool {
        name: "describeTool".to_string(),
        description: None,
        input_schema: None,
        output_schema: None,
        spread: Some(true),
        signature: None,
        execute: Arc::new(move |args, _ctx| {
            let tools = tools_describe.clone();
            let samples = Arc::clone(&samples_describe);
            Box::pin(async move {
                let arr = args.and_then(|v| v.as_array().cloned()).unwrap_or_default();
                let name = arr
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::msg("describeTool() expects a tool name"))?;
                let found = tools
                    .iter()
                    .find(|c| c.name == name || to_codemode_identifier(&c.name) == name);
                Ok(found.and_then(|t| samples.get(&t.name).cloned()).map(Value::String))
            })
        }),
    };

    let get_ns_dir = options.get_tool_namespace.clone();
    let describe_namespace = CodemodeTool {
        name: "describeNamespace".to_string(),
        description: None,
        input_schema: None,
        output_schema: None,
        spread: Some(true),
        signature: None,
        execute: Arc::new(move |args, _ctx| {
            let tools = tools.clone();
            let get_ns = get_ns_dir.clone();
            Box::pin(async move {
                let arr = args.and_then(|v| v.as_array().cloned()).unwrap_or_default();
                let name = arr
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::msg("describeNamespace() expects a namespace name"))?;
                let mut matched_ns = None;
                let mut names = Vec::new();
                for tool in &tools {
                    let Some(tool_ns) = get_ns.as_ref().and_then(|f| f(&tool.name)) else {
                        continue;
                    };
                    if !is_namespace_name(&tool_ns, name) {
                        continue;
                    }
                    if matched_ns.is_none() {
                        matched_ns = Some(tool_ns);
                    }
                    names.push(Value::String(to_codemode_identifier(&tool.name)));
                }
                let Some(ns) = matched_ns else {
                    return Ok(None);
                };
                let mut obj = serde_json::Map::new();
                obj.insert("name".to_string(), Value::String(ns.name));
                if let Some(desc) = ns.description.filter(|d| !d.is_empty()) {
                    obj.insert("description".to_string(), Value::String(desc));
                }
                if let Some(inst) = ns.instructions.filter(|i| !i.is_empty()) {
                    obj.insert("instructions".to_string(), Value::String(inst));
                }
                obj.insert("tools".to_string(), Value::Array(names));
                Ok(Some(Value::Object(obj)))
            })
        }),
    };

    vec![search_tools, describe_tool, describe_namespace]
}

fn parse_model_type(model_type: &str) -> Option<ModelType> {
    match model_type {
        "chat" => Some(ModelType::Chat),
        "image" => Some(ModelType::Image),
        "classifier" => Some(ModelType::Classifier),
        _ => None,
    }
}

fn any_model_to_value(model: &AnyModel) -> Option<Value> {
    pi_js::json::parse(&pi_js::json::stringify(model)).ok()
}

struct RegistryCodemodeModelRuntime(ModelRegistry);

#[async_trait]
impl CodemodeModelRuntime for RegistryCodemodeModelRuntime {
    fn get_models_of_type(&self, model_type: &str, provider: Option<&str>) -> Vec<Value> {
        let Some(ty) = parse_model_type(model_type) else {
            return Vec::new();
        };
        self.0
            .get_models_of_type(ty, provider)
            .iter()
            .filter_map(any_model_to_value)
            .collect()
    }

    async fn get_available_of_type(
        &self,
        model_type: &str,
        provider: Option<&str>,
        signal: Option<AbortSignal>,
    ) -> Result<Vec<Value>> {
        let Some(ty) = parse_model_type(model_type) else {
            return Ok(Vec::new());
        };
        let opts = signal.map(|s| AuthOperationOptions { signal: Some(s) });
        Ok(self
            .0
            .get_available_of_type(ty, provider, opts)
            .await?
            .iter()
            .filter_map(any_model_to_value)
            .collect())
    }

    fn get_model_of_type(&self, model_type: &str, provider: &str, id: &str) -> Option<Value> {
        let ty = parse_model_type(model_type)?;
        self.0
            .get_model_of_type(ty, provider, id)
            .as_ref()
            .and_then(any_model_to_value)
    }

    async fn classify(&self, model: Value, context: Value, signal: Option<AbortSignal>) -> Result<Value> {
        let parsed_model: ClassifierModel = pi_js::json::parse_as(&pi_js::json::stringify(&model))?;
        if context
            .get("images")
            .and_then(Value::as_array)
            .is_some_and(|images| !images.is_empty())
            && !parsed_model.input.contains(&pi_ai::types::ModelInput::Image)
        {
            let aborted = signal.as_ref().is_some_and(AbortSignal::aborted);
            let res = pi_ai::utils::model_operations::classifier_error_result(
                &parsed_model,
                &Error::msg(format!(
                    "Model {}/{} does not accept image input",
                    parsed_model.provider, parsed_model.id
                )),
                aborted,
            );
            return pi_js::json::parse(&pi_js::json::stringify(&res));
        }
        let parsed_ctx: ClassifierContext = pi_js::json::parse_as(&pi_js::json::stringify(&context))?;
        let mut base = ClassifierOptions::default();
        base.base.signal = signal;
        let opts = ModelsClassifierOptions {
            base,
            transform_headers: None,
        };
        let res = self.0.classify(&parsed_model, &parsed_ctx, Some(opts)).await;
        pi_js::json::parse(&pi_js::json::stringify(&res))
    }

    async fn generate_images(&self, model: Value, context: Value, signal: Option<AbortSignal>) -> Result<Value> {
        let parsed_model: ImageModel = pi_js::json::parse_as(&pi_js::json::stringify(&model))?;
        let parsed_ctx: ImagesContext = pi_js::json::parse_as(&pi_js::json::stringify(&context))?;
        let mut base = ImagesOptions::default();
        base.base.signal = signal;
        let opts = ModelsImagesOptions {
            base,
            transform_headers: None,
        };
        let res = self.0.generate_images(&parsed_model, &parsed_ctx, Some(opts)).await;
        pi_js::json::parse(&pi_js::json::stringify(&res))
    }
}

#[derive(Clone)]
struct SharedExecutionTracker {
    calls: Arc<Mutex<Vec<CodemodeNestedCall>>>,
    on_update: Option<pi_agent_core::AgentToolUpdateCallback>,
    model_usage: Arc<Mutex<Option<Usage>>>,
    generated_images: Arc<AtomicUsize>,
}

impl SharedExecutionTracker {
    fn snapshot(&self) -> CodemodeToolDetails {
        CodemodeToolDetails {
            calls: self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            full_output_path: None,
        }
    }

    fn publish(&self) {
        if let Some(ref cb) = self.on_update {
            let mut result = AgentToolResult::text("");
            result.content.clear();
            result.details = pi_js::json::parse(&pi_js::json::stringify(&self.snapshot())).ok();
            cb(result);
        }
    }

    fn push_call(&self, call: CodemodeNestedCall) -> usize {
        let mut guard = self.calls.lock().unwrap_or_else(|e| e.into_inner());
        let idx = guard.len();
        guard.push(call);
        drop(guard);
        self.publish();
        idx
    }

    fn update_call<F: FnOnce(&mut CodemodeNestedCall)>(&self, idx: usize, f: F) {
        let mut guard = self.calls.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(call) = guard.get_mut(idx) {
            f(call);
        }
        drop(guard);
        self.publish();
    }

    fn add_usage(&self, usage: &Usage) {
        let mut guard = self.model_usage.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(match guard.as_ref() {
            Some(prev) => combine_usage(prev, usage),
            None => *usage,
        });
    }
}

fn create_model_globals(
    models: Arc<dyn CodemodeModelRuntime>,
    tool_call_id: String,
    tracker: SharedExecutionTracker,
) -> Vec<CodemodeTool> {
    let limiter = Arc::new(Semaphore::new(MAX_CONCURRENT_MODEL_CALLS));
    let call_counter = Arc::new(AtomicUsize::new(0));

    let run_model_call = {
        let models = Arc::clone(&models);
        let tracker = tracker.clone();
        let limiter = Arc::clone(&limiter);
        let call_counter = Arc::clone(&call_counter);
        let tool_call_id = tool_call_id.clone();
        Arc::new(
            move |name: &'static str,
                  model_type: &'static str,
                  args: Option<Value>,
                  signal: AbortSignal|
                  -> pi_js::BoxFuture<Result<Option<Value>>> {
                let models = Arc::clone(&models);
                let tracker = tracker.clone();
                let limiter = Arc::clone(&limiter);
                let call_counter = Arc::clone(&call_counter);
                let tool_call_id = tool_call_id.clone();
                Box::pin(async move {
                    let arr = args.and_then(|v| v.as_array().cloned()).unwrap_or_default();
                    let model_arg = arr.first();
                    let ctx_arg = arr.get(1);
                    let list_hint = format!(
                        "List the {model_type} models you can use with models.getAvailableOfType(\"{model_type}\")."
                    );
                    let Some(model_obj) = model_arg.and_then(Value::as_object) else {
                        let undef_hint = if model_arg.is_none() || model_arg == Some(&Value::Null) {
                            " models.getModelOfType() returns undefined for an unknown provider or id."
                        } else {
                            ""
                        };
                        return Err(Error::msg(format!(
                            "{name}() expects {} model as its first argument, got {}.{undef_hint} {list_hint}",
                            with_article(model_type),
                            describe_value(model_arg),
                        )));
                    };
                    let (Some(provider), Some(id)) = (
                        model_obj.get("provider").and_then(Value::as_str),
                        model_obj.get("id").and_then(Value::as_str),
                    ) else {
                        return Err(Error::msg(format!(
                            "{name}() expects {} model as its first argument, got {}. {list_hint}",
                            with_article(model_type),
                            describe_value(model_arg),
                        )));
                    };
                    let ref_str = format!("{provider}/{id}");
                    let Some(resolved) = models.get_model_of_type(model_type, provider, id) else {
                        let actual_type = ["chat", "image", "classifier"].into_iter().find(|other| {
                            *other != model_type && models.get_model_of_type(other, provider, id).is_some()
                        });
                        return Err(Error::msg(match actual_type {
                            Some(actual) => format!(
                                "\"{ref_str}\" is {} model, not {} model. {list_hint}",
                                with_article(actual),
                                with_article(model_type)
                            ),
                            None => format!("Unknown {model_type} model \"{ref_str}\". {list_hint}"),
                        }));
                    };
                    let checked = if model_type == "classifier" {
                        check_classifier_context(ctx_arg)?
                    } else {
                        check_images_context(ctx_arg)?
                    };

                    let resolved_provider = resolved
                        .get("provider")
                        .and_then(Value::as_str)
                        .unwrap_or(provider)
                        .to_string();
                    let resolved_id = resolved.get("id").and_then(Value::as_str).unwrap_or(id).to_string();
                    let num = call_counter.fetch_add(1, Ordering::Relaxed) + 1;
                    let idx = tracker.push_call(CodemodeNestedCall {
                        id: format!("{tool_call_id}/{name}/{num}"),
                        name: name.to_string(),
                        args: format!("{resolved_provider}/{resolved_id}"),
                        status: CodemodeNestedCallStatus::Running,
                        duration_ms: None,
                        error: None,
                        cost: None,
                    });
                    let started_at = pi_js::time::performance_now();
                    let _permit = limiter
                        .acquire()
                        .await
                        .map_err(|_| Error::msg("Model call limiter closed"))?;
                    let res_value = if model_type == "classifier" {
                        models.classify(resolved, checked, Some(signal)).await?
                    } else {
                        let out = models.generate_images(resolved, checked, Some(signal)).await?;
                        if let Some(arr) = out.get("output").and_then(Value::as_array) {
                            let img_count = arr
                                .iter()
                                .filter(|b| b.get("type").and_then(Value::as_str) == Some("image"))
                                .count();
                            tracker.generated_images.fetch_add(img_count, Ordering::Relaxed);
                        }
                        out
                    };
                    let elapsed = pi_js::time::performance_now() - started_at;
                    let stop_reason = res_value.get("stopReason").and_then(Value::as_str).unwrap_or("stop");
                    let status = match stop_reason {
                        "stop" => CodemodeNestedCallStatus::Ok,
                        "aborted" => CodemodeNestedCallStatus::Cancelled,
                        _ => CodemodeNestedCallStatus::Error,
                    };
                    let err_msg = res_value
                        .get("errorMessage")
                        .and_then(Value::as_str)
                        .map(|s| truncate_text(s, ERROR_PREVIEW_CHARS));
                    let usage_opt: Option<Usage> = res_value
                        .get("usage")
                        .and_then(|u| pi_js::json::parse_as(&pi_js::json::stringify(u)).ok());
                    tracker.update_call(idx, |call| {
                        call.duration_ms = Some(elapsed);
                        call.status = status;
                        call.error = err_msg;
                        if let Some(ref u) = usage_opt {
                            call.cost = Some(u.cost.total);
                            tracker.add_usage(u);
                        }
                    });
                    Ok(Some(res_value))
                })
            },
        )
    };

    let m_list = Arc::clone(&models);
    let get_models_of_type = CodemodeTool {
        name: "models.getModelsOfType".to_string(),
        description: None,
        input_schema: None,
        output_schema: None,
        spread: Some(true),
        signature: None,
        execute: Arc::new(move |args, _ctx| {
            let m = Arc::clone(&m_list);
            Box::pin(async move {
                let arr = args.and_then(|v| v.as_array().cloned()).unwrap_or_default();
                let ty = to_model_type(arr.first())?;
                let prov = to_provider(arr.get(1))?;
                let list: Vec<Value> = m
                    .get_models_of_type(ty, prov.as_deref())
                    .into_iter()
                    .map(redact_model_headers)
                    .collect();
                Ok(Some(Value::Array(list)))
            })
        }),
    };

    let m_avail = Arc::clone(&models);
    let get_available_of_type = CodemodeTool {
        name: "models.getAvailableOfType".to_string(),
        description: None,
        input_schema: None,
        output_schema: None,
        spread: Some(true),
        signature: None,
        execute: Arc::new(move |args, ctx| {
            let m = Arc::clone(&m_avail);
            Box::pin(async move {
                let arr = args.and_then(|v| v.as_array().cloned()).unwrap_or_default();
                let ty = to_model_type(arr.first())?;
                let prov = to_provider(arr.get(1))?;
                let list: Vec<Value> = m
                    .get_available_of_type(ty, prov.as_deref(), Some(ctx.signal))
                    .await?
                    .into_iter()
                    .map(redact_model_headers)
                    .collect();
                Ok(Some(Value::Array(list)))
            })
        }),
    };

    let m_get = Arc::clone(&models);
    let get_model_of_type = CodemodeTool {
        name: "models.getModelOfType".to_string(),
        description: None,
        input_schema: None,
        output_schema: None,
        spread: Some(true),
        signature: None,
        execute: Arc::new(move |args, _ctx| {
            let m = Arc::clone(&m_get);
            Box::pin(async move {
                let arr = args.and_then(|v| v.as_array().cloned()).unwrap_or_default();
                let ty = to_model_type(arr.first())?;
                let (Some(provider), Some(id)) =
                    (arr.get(1).and_then(Value::as_str), arr.get(2).and_then(Value::as_str))
                else {
                    let arg_descs: Vec<String> = arr.iter().map(|v| describe_value(Some(v))).collect();
                    return Err(Error::msg(format!(
                        "models.getModelOfType(type, provider, id) expects three strings, got ({}). The provider and the id are separate arguments, for example models.getModelOfType(\"classifier\", \"typesafe\", \"jev-latest\").",
                        arg_descs.join(", ")
                    )));
                };
                Ok(m.get_model_of_type(ty, provider, id).map(redact_model_headers))
            })
        }),
    };

    let r_classify = Arc::clone(&run_model_call);
    let classify = CodemodeTool {
        name: "models.classify".to_string(),
        description: None,
        input_schema: None,
        output_schema: None,
        spread: Some(true),
        signature: None,
        execute: Arc::new(move |args, ctx| r_classify("models.classify", "classifier", args, ctx.signal)),
    };

    let r_images = Arc::clone(&run_model_call);
    let generate_images = CodemodeTool {
        name: "models.generateImages".to_string(),
        description: None,
        input_schema: None,
        output_schema: None,
        spread: Some(true),
        signature: None,
        execute: Arc::new(move |args, ctx| r_images("models.generateImages", "image", args, ctx.signal)),
    };

    vec![
        get_models_of_type,
        get_available_of_type,
        get_model_of_type,
        classify,
        generate_images,
    ]
}

/// Run one codemode script. Without a session context, scripts cannot call nested tools,
/// `store()` starts empty, and writes are dropped.
pub async fn execute_codemode(
    tool_call_id: String,
    input: CodemodeToolInput,
    signal: Option<AbortSignal>,
    on_update: Option<pi_agent_core::AgentToolUpdateCallback>,
    ctx: Option<ExtensionToolContext>,
    options: CodemodeToolOptions,
) -> Result<AgentToolResult> {
    let started_at = pi_js::time::performance_now();
    let parsed = parse_codemode_source(&input.code)?;
    let calls = Arc::new(Mutex::new(Vec::<CodemodeNestedCall>::new()));
    let model_usage = Arc::new(Mutex::new(None::<Usage>));
    let generated_images = Arc::new(AtomicUsize::new(0));
    let tracker = SharedExecutionTracker {
        calls: Arc::clone(&calls),
        on_update,
        model_usage: Arc::clone(&model_usage),
        generated_images: Arc::clone(&generated_images),
    };
    // Show the script as running before its first nested call, so the row's timer starts.
    tracker.publish();

    let callable = match &ctx {
        Some(c) => get_codemode_callable_tools(&c.tools()?),
        None => Vec::new(),
    };
    let guidelines = options.get_tool_guidelines.as_ref().map(|f| f()).unwrap_or_default();
    let mut samples = IndexMap::new();
    for tool in &callable {
        let tool_guidelines = guidelines.get(&tool.name).map(Vec::as_slice).unwrap_or(&[]);
        let decl = to_codemode_declaration(tool, tool_guidelines);
        samples.insert(tool.name.clone(), render_tool_sample(&decl, None));
    }

    let sandbox_tools: Vec<CodemodeTool> = callable
        .iter()
        .map(|tool| {
            let tool = tool.clone();
            let desc = samples.get(&tool.name).cloned();
            let tracker = tracker.clone();
            let ctx = ctx.clone();
            let tool_call_id = tool_call_id.clone();
            CodemodeTool {
                name: tool.name.clone(),
                description: desc,
                input_schema: None,
                output_schema: None,
                spread: None,
                signature: None,
                execute: Arc::new(move |args, call_ctx| {
                    let tool = tool.clone();
                    let tracker = tracker.clone();
                    let ctx = ctx.clone();
                    let tool_call_id = tool_call_id.clone();
                    Box::pin(async move {
                        let idx = tracker.push_call(CodemodeNestedCall {
                            id: format!("{tool_call_id}/?"),
                            name: tool.name.clone(),
                            args: preview_args(args.as_ref()),
                            status: CodemodeNestedCallStatus::Running,
                            duration_ms: None,
                            error: None,
                            cost: None,
                        });
                        let call_started_at = pi_js::time::performance_now();
                        let Some(ref ctx) = ctx else {
                            return Err(Error::msg("Tool calls need a session"));
                        };
                        let outcome = ctx
                            .execute_tool(
                                &tool.name,
                                args.unwrap_or(Value::Null),
                                Some(ExecuteToolOptions {
                                    signal: Some(call_ctx.signal.clone()),
                                    on_update: None,
                                }),
                            )
                            .await?;
                        let elapsed = pi_js::time::performance_now() - call_started_at;
                        let call_id = outcome.tool_call.id.clone();
                        let (status, err_msg) = if outcome.is_error {
                            let st = if call_ctx.signal.aborted() {
                                CodemodeNestedCallStatus::Cancelled
                            } else {
                                CodemodeNestedCallStatus::Error
                            };
                            let text = text_of(&outcome.result);
                            let raw_err = if text.is_empty() {
                                format!("Tool \"{}\" failed", tool.name)
                            } else {
                                text
                            };
                            (st, Some(truncate_text(&raw_err, ERROR_PREVIEW_CHARS)))
                        } else {
                            (CodemodeNestedCallStatus::Ok, None)
                        };
                        tracker.update_call(idx, |c| {
                            c.id = call_id;
                            c.duration_ms = Some(elapsed);
                            c.status = status;
                            c.error = err_msg;
                        });
                        to_script_value(&tool, &outcome)
                    })
                }),
            }
        })
        .collect();

    let mut globals = create_discovery_globals(callable, samples, &options);
    if options.models {
        let model_runtime: Option<Arc<dyn CodemodeModelRuntime>> = if let Some(ref rt) = options.model_runtime {
            Some(Arc::clone(rt))
        } else if let Some(ref c) = ctx {
            c.model_registry()
                .ok()
                .map(|reg| Arc::new(RegistryCodemodeModelRuntime(reg)) as Arc<dyn CodemodeModelRuntime>)
        } else {
            None
        };
        if let Some(rt) = model_runtime {
            globals.extend(create_model_globals(rt, tool_call_id.clone(), tracker.clone()));
        }
    }

    // The wait for a worker counts toward the script's deadline.
    let deadline = parsed.options.timeout_ms.map(|ms| ms as f64).unwrap_or(f64::INFINITY);
    let wait_started_at = pi_js::time::performance_now();
    let admission = reserve_codemode_worker(signal.as_ref(), deadline.min(CODEMODE_WORKER_WAIT_MS as f64)).await;
    let waited_ms = pi_js::time::performance_now() - wait_started_at;
    if admission != WorkerReservationOutcome::Reserved {
        let waited = (pi_js::time::performance_now() - started_at) / 1000.0;
        let error = if admission == WorkerReservationOutcome::Aborted {
            let reason = signal
                .as_ref()
                .and_then(AbortSignal::reason)
                .map(|r| {
                    if r.message.is_empty() {
                        "Execution aborted".to_string()
                    } else {
                        r.message
                    }
                })
                .unwrap_or_else(|| "Execution aborted".to_string());
            format!("Script aborted: {reason}")
        } else if deadline <= CODEMODE_WORKER_WAIT_MS as f64 {
            format!(
                "Script timed out: Execution timed out after {} ms while waiting for a free codemode worker",
                pi_js::num::to_string_radix(deadline, 10)
            )
        } else {
            format!(
                "No codemode worker became free within {} seconds. All {} workers, shared by every session in this process, are held by other scripts. Retry after they finish.",
                CODEMODE_WORKER_WAIT_MS / 1000,
                max_codemode_workers()
            )
        };
        let mut out = AgentToolResult::text("");
        out.content = vec![
            UserContentBlock::Text(TextContent::new(format!(
                "Script failed\nWall time {waited:.1} seconds\nOutput:\n"
            ))),
            UserContentBlock::Text(TextContent::new(format!(
                "Script error:\n{error}\n\nThe script did not start."
            ))),
        ];
        out.details = pi_js::json::parse(&pi_js::json::stringify(&tracker.snapshot())).ok();
        out.is_error = Some(true);
        return Ok(out);
    }

    let reserved = Arc::new(AtomicBool::new(true));
    struct ReservedSlotGuard(Arc<AtomicBool>);
    impl Drop for ReservedSlotGuard {
        fn drop(&mut self) {
            if self.0.swap(false, Ordering::AcqRel) {
                release_codemode_worker();
            }
        }
    }
    let _reserved_guard = ReservedSlotGuard(Arc::clone(&reserved));
    let acquire_reserved = Arc::clone(&reserved);
    let remaining_timeout_ms = if deadline.is_finite() {
        (deadline - waited_ms).round().max(1.0)
    } else {
        f64::INFINITY
    };

    let sandbox = CodemodeSandbox::with_worker_hooks(
        Some(CodemodeSandboxOptions {
            tools: Some(sandbox_tools),
            globals: Some(globals),
            timeout_ms: Some(remaining_timeout_ms),
            memory_limit_bytes: Some(CODEMODE_MEMORY_LIMIT_BYTES),
            wasm: None,
            worker_url: None,
        }),
        CodemodeWorkerHooks {
            acquire_worker: Some(Arc::new(move || {
                if !acquire_reserved.swap(false, Ordering::AcqRel) {
                    return Err(Error::msg("The script already settled"));
                }
                // From here the sandbox releases the slot when the worker actually exits.
                Ok(Box::new(|| release_codemode_worker()))
            })),
            on_worker_started: None,
        },
    )?;

    let initial_store = match &ctx {
        Some(c) => c
            .session_manager()
            .ok()
            .map(|sm| read_codemode_store(&sm.get_branch(None))),
        None => None,
    };
    let result = sandbox
        .execute(
            &parsed.code,
            Some(CodemodeExecuteOptions {
                signal,
                timeout_ms: None,
                store: initial_store,
            }),
        )
        .await;
    sandbox.close().await;
    drop(_reserved_guard);
    let result = result?;

    // Calls still marked running were cut off by script termination/timeout/abort.
    {
        let mut guard = calls.lock().unwrap_or_else(|e| e.into_inner());
        for call in guard.iter_mut() {
            if call.status == CodemodeNestedCallStatus::Running {
                call.status = CodemodeNestedCallStatus::Cancelled;
            }
        }
    }
    let snapshot_calls = calls.lock().unwrap_or_else(|e| e.into_inner()).clone();

    let (ok, mut items) = match &result {
        CodemodeResult::Success {
            value,
            output,
            store_writes,
            ..
        } => {
            if (!store_writes.set.is_empty() || !store_writes.r#delete.is_empty())
                && let Some(ref append) = options.append_entry
            {
                append(
                    CODEMODE_STORE_ENTRY_TYPE,
                    CodemodeStoreEntryData {
                        set: store_writes.set.clone(),
                        r#delete: store_writes.r#delete.clone(),
                    },
                );
            }
            let mut tagged: Vec<(CodemodeOutputItem, bool)> = output
                .iter()
                .enumerate()
                .map(|(i, item)| (item.clone(), result.is_console_output(i)))
                .collect();
            if let Some(val) = value {
                tagged.push((CodemodeOutputItem::Text { text: value_text(val) }, false));
            }
            (true, format_output(&tagged))
        }
        CodemodeResult::Failure { output, .. } => {
            let tagged: Vec<(CodemodeOutputItem, bool)> = output
                .iter()
                .enumerate()
                .map(|(i, item)| (item.clone(), result.is_console_output(i)))
                .collect();
            let mut formatted = format_output(&tagged);
            formatted.push(UserContentBlock::Text(TextContent::new(format!(
                "Script error:\n{}",
                format_error(&result, &snapshot_calls)
            ))));
            (false, formatted)
        }
    };

    let img_total = generated_images.load(Ordering::Relaxed);
    if img_total > 0 && !items.iter().any(|item| matches!(item, UserContentBlock::Image(_))) {
        let plural = if img_total == 1 { "" } else { "s" };
        items.push(UserContentBlock::Text(TextContent::new(format!(
            "Note: models.generateImages() returned {img_total} image{plural} that the script did not show. Show each image block of result.output with image(block)."
        ))));
    }

    let max_tokens = parsed
        .options
        .max_output_tokens
        .map(|n| n.max(0) as usize)
        .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
    let (truncated_items, full_output_path) = truncate_output(join_adjacent_text(items), max_tokens).await;
    // After truncation, which joins the text items and moves images after them, so each path stays
    // next to its image and is never cut.
    let output = join_adjacent_text(save_images(truncated_items).await?);
    let wall_time = (pi_js::time::performance_now() - started_at) / 1000.0;
    let status_word = if ok { "completed" } else { "failed" };
    let header = format!("Script {status_word}\nWall time {wall_time:.1} seconds\nOutput:\n");

    let mut content = Vec::with_capacity(output.len() + 1);
    content.push(UserContentBlock::Text(TextContent::new(header)));
    content.extend(output);

    let details = CodemodeToolDetails {
        calls: snapshot_calls,
        full_output_path,
    };

    let mut out = AgentToolResult::text("");
    out.content = content;
    out.details = pi_js::json::parse(&pi_js::json::stringify(&details)).ok();
    out.usage = *model_usage.lock().unwrap_or_else(|e| e.into_inner());
    out.is_error = if ok { None } else { Some(true) };
    Ok(out)
}
