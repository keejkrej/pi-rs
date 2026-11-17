//! Port of packages/durable/test/tools.test.ts

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_chord::context::{BACKGROUND_CONTEXT, with_abort_signal};
use pi_chord::{Context, JsonValue};
use pi_durable::documents::ErasedDocDefinition;
use pi_durable::env::node::{NodeExecutionEnv, NodeExecutionEnvOptions};
use pi_durable::env::{
    CreateDirOptions, CreateTempFileOptions, EnvCleanup, ExecutionEnv, ExecutionError, ExecutionErrorCode, FileContent,
    FileError, FileInfo, FileSystem, ReadTextLinesOptions, RemoveOptions, Shell, ShellExecOptions, ShellExecResult,
    TextLineReader, get_or_throw,
};
use pi_durable::harness::types::{
    Agent, ConversationHandle, DocumentObserverBackend, DocumentReaderBackend, Extension, ExtensionSection,
    ExtensionTool, RegistrySnapshot, SettledTask, ToolDiagnostic, ToolExecutionApi, ToolExecutionApiBackend,
    ToolExecutionResult, ToolRegistration, ToolResultContentBlock,
};
use pi_durable::session::transaction::Tx;
use pi_durable::tasks::AnyTask;
use pi_durable::tools::file_mutation_queue::with_file_mutation_queue;
use pi_durable::tools::image::detect_supported_image_mime_type;
use pi_durable::tools::{BashToolOptions, create_bash_tool, create_edit_tool, create_read_tool, create_write_tool};
use pi_durable::truncate::DEFAULT_MAX_LINES;
use pi_durable::types::{ConversationId, DocumentWatch, EntryId, ErasedTaskRecord, JsonObject, TaskId, TaskOwnership};
use pi_js::abort::AbortController;
use pi_js::vendor::jsdiff::apply_patch;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::watch;

struct EmptyRegistrySnapshot;

impl RegistrySnapshot for EmptyRegistrySnapshot {
    fn installed(&self) -> &[Extension] {
        &[]
    }

    fn extension(&self, _name: &str) -> Option<&Extension> {
        None
    }

    fn tools(&self) -> Vec<ExtensionTool> {
        Vec::new()
    }

    fn sections(&self) -> Vec<ExtensionSection> {
        Vec::new()
    }

    fn tasks(&self) -> Vec<AnyTask> {
        Vec::new()
    }

    fn task(&self, _name: &str) -> Option<&AnyTask> {
        None
    }
}

struct FakeToolBackend {
    env: Option<Arc<dyn ExecutionEnv>>,
    output: Arc<Mutex<Vec<String>>>,
    diagnostics: Arc<Mutex<Vec<ToolDiagnostic>>>,
}

#[async_trait]
impl DocumentReaderBackend for FakeToolBackend {
    async fn snapshot_erased(
        &self,
        _def: &Arc<ErasedDocDefinition>,
        _conversation_id: Option<ConversationId>,
        _task_id: Option<TaskId>,
        _key: Option<&str>,
        _context: Context,
    ) -> pi_js::Result<Option<JsonObject>> {
        Ok(None)
    }

    async fn snapshot_as_of_erased(
        &self,
        _def: &Arc<ErasedDocDefinition>,
        _conversation_id: ConversationId,
        _key: Option<&str>,
        _at: EntryId,
        _context: Context,
    ) -> pi_js::Result<Option<JsonObject>> {
        Ok(None)
    }
}

#[async_trait]
impl DocumentObserverBackend for FakeToolBackend {
    async fn watch_erased(
        &self,
        _def: &Arc<ErasedDocDefinition>,
        _conversation_id: Option<ConversationId>,
        _task_id: Option<TaskId>,
        _key: Option<&str>,
        _context: Context,
    ) -> pi_js::Result<Option<DocumentWatch<JsonObject>>> {
        Ok(None)
    }
}

#[async_trait]
impl ToolExecutionApiBackend for FakeToolBackend {
    fn task_id(&self) -> TaskId {
        TaskId::new_unchecked(1)
    }

    fn conversation_id(&self) -> ConversationId {
        ConversationId::new_unchecked(1)
    }

    fn call_id(&self) -> &str {
        "call"
    }

    fn registry(&self) -> Arc<dyn RegistrySnapshot> {
        Arc::new(EmptyRegistrySnapshot)
    }

    async fn agent(&self, _context: Context) -> pi_js::Result<Agent> {
        Ok(Agent {
            model: None,
            thinking_level: pi_ai::types::ModelThinkingLevel::Off,
            extensions: Vec::new(),
            tools: Vec::new(),
            sections: Vec::new(),
            instructions: None,
            cwd: None,
        })
    }

    fn env(&self) -> Option<Arc<dyn ExecutionEnv>> {
        self.env.clone()
    }

    fn output_text(&self, chunk: &str) -> pi_js::Result<()> {
        self.output
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(chunk.to_owned());
        Ok(())
    }

    fn output_bytes(&self, chunk: &[u8]) -> pi_js::Result<()> {
        self.output
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(pi_js::text::decode_utf8(chunk));
        Ok(())
    }

    fn diagnostic(&self, diagnostic: ToolDiagnostic) -> pi_js::Result<()> {
        self.diagnostics
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(diagnostic);
        Ok(())
    }

    async fn details(&self, _value: JsonValue, _context: Context) -> pi_js::Result<()> {
        Ok(())
    }

    async fn commit_erased(
        &self,
        _change: Box<dyn FnOnce(Tx) -> pi_js::BoxFuture<pi_js::Result<JsonValue>> + Send>,
        _context: Context,
    ) -> pi_js::Result<JsonValue> {
        Err(pi_js::Error::msg("fakeApi does not support commit"))
    }

    async fn memo_get(&self, _name: &str, _context: Context) -> pi_js::Result<Option<JsonValue>> {
        Ok(None)
    }

    async fn memo_set(&self, _name: &str, candidate: JsonValue, _context: Context) -> pi_js::Result<JsonValue> {
        Ok(candidate)
    }

    async fn create_task_erased(
        &self,
        _task: &AnyTask,
        _input: JsonValue,
        _ownership: TaskOwnership,
        _background: bool,
        _context: Context,
    ) -> pi_js::Result<TaskId> {
        Err(pi_js::Error::msg("fakeApi does not support tasks"))
    }

    async fn get_task(&self, _id: TaskId, _context: Context) -> pi_js::Result<Option<ErasedTaskRecord>> {
        Ok(None)
    }

    async fn wait_for_task(&self, _id: TaskId, _context: Context) -> pi_js::Result<SettledTask<JsonValue>> {
        Err(pi_js::Error::msg("fakeApi does not support tasks"))
    }

    async fn conversation(&self, _id: ConversationId, _context: Context) -> pi_js::Result<Option<ConversationHandle>> {
        Ok(None)
    }
}

fn create_temp_dir() -> (TempDir, String) {
    let dir = tempfile::Builder::new()
        .prefix("pi-durable-tools-")
        .tempdir()
        .expect("create temp dir");
    let path = dir.path().to_string_lossy().into_owned();
    (dir, path)
}

fn create_env() -> (TempDir, Arc<NodeExecutionEnv>) {
    let (dir, path) = create_temp_dir();
    (dir, Arc::new(NodeExecutionEnv::new(path)))
}

struct FakeApiResult {
    api: ToolExecutionApi,
    output: Arc<Mutex<Vec<String>>>,
    diagnostics: Arc<Mutex<Vec<ToolDiagnostic>>>,
}

fn fake_api(env: Option<Arc<dyn ExecutionEnv>>) -> FakeApiResult {
    let output = Arc::new(Mutex::new(Vec::new()));
    let diagnostics = Arc::new(Mutex::new(Vec::new()));
    let backend = Arc::new(FakeToolBackend {
        env,
        output: Arc::clone(&output),
        diagnostics: Arc::clone(&diagnostics),
    });
    FakeApiResult {
        api: ToolExecutionApi::from_backend(backend),
        output,
        diagnostics,
    }
}

struct RunOutcome {
    result: ToolExecutionResult,
    output: Vec<String>,
    reported: Vec<ToolDiagnostic>,
}

struct FailingOutcome {
    error: pi_js::Error,
    output: Vec<String>,
    reported: Vec<ToolDiagnostic>,
}

async fn run_tool(
    tool: &ToolRegistration,
    args: JsonValue,
    env: Option<Arc<dyn ExecutionEnv>>,
    context: Context,
) -> pi_js::Result<RunOutcome> {
    let fake = fake_api(env);
    let result = (tool.execute)(args, fake.api, context).await?;
    let output = fake.output.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let reported = fake.diagnostics.lock().unwrap_or_else(|e| e.into_inner()).clone();
    Ok(RunOutcome {
        result,
        output,
        reported,
    })
}

async fn run_failing(tool: &ToolRegistration, args: JsonValue, env: Arc<dyn ExecutionEnv>) -> FailingOutcome {
    let fake = fake_api(Some(env));
    let err = (tool.execute)(args, fake.api, BACKGROUND_CONTEXT.clone())
        .await
        .expect_err("Expected the tool to throw");
    let output = fake.output.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let reported = fake.diagnostics.lock().unwrap_or_else(|e| e.into_inner()).clone();
    FailingOutcome {
        error: err,
        output,
        reported,
    }
}

fn text_output(result: &ToolExecutionResult) -> String {
    result
        .content
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter_map(|part| match part {
            ToolResultContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn diagnostic_text(result: &ToolExecutionResult) -> String {
    result
        .diagnostics
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|d| d.message.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Clone)]
struct SignalFlag {
    tx: watch::Sender<bool>,
    rx: watch::Receiver<bool>,
}

impl SignalFlag {
    fn new() -> Self {
        let (tx, rx) = watch::channel(false);
        Self { tx, rx }
    }

    fn resolve(&self) {
        let _ = self.tx.send(true);
    }

    async fn wait(&self) {
        let mut rx = self.rx.clone();
        let _ = rx.wait_for(|v| *v).await;
    }
}

async fn delay_ms(ms: u64) {
    let _ = pi_js::time::sleep(ms, None).await;
}

macro_rules! impl_delegating_fs {
    ($ty:ty, id: |$id_self:ident| $id_expr:expr, read: |$r_self:ident, $r_path:ident, $r_ctx:ident| $read_expr:expr, write: |$w_self:ident, $w_path:ident, $w_content:ident, $w_ctx:ident| $write_expr:expr) => {
        #[async_trait]
        impl FileSystem for $ty {
            fn id(&$id_self) -> &str {
                $id_expr
            }
            fn cwd(&self) -> &str {
                self.inner.cwd()
            }
            async fn read_text_file(&$r_self, $r_path: &str, $r_ctx: &Context) -> Result<String, FileError> {
                $read_expr
            }
            async fn write_file(
                &$w_self,
                $w_path: &str,
                $w_content: FileContent<'_>,
                $w_ctx: &Context,
            ) -> Result<(), FileError> {
                $write_expr
            }
            async fn absolute_path(&self, path: &str, context: &Context) -> Result<String, FileError> {
                self.inner.absolute_path(path, context).await
            }
            async fn join_path(&self, parts: &[&str], context: &Context) -> Result<String, FileError> {
                self.inner.join_path(parts, context).await
            }
            async fn open_text_line_reader(
                &self,
                path: &str,
                context: &Context,
            ) -> Result<Box<dyn TextLineReader>, FileError> {
                self.inner.open_text_line_reader(path, context).await
            }
            async fn read_text_lines(
                &self,
                path: &str,
                options: Option<ReadTextLinesOptions>,
                context: &Context,
            ) -> Result<Vec<String>, FileError> {
                self.inner.read_text_lines(path, options, context).await
            }
            async fn read_binary_file(&self, path: &str, context: &Context) -> Result<Vec<u8>, FileError> {
                self.inner.read_binary_file(path, context).await
            }
            async fn append_file(
                &self,
                path: &str,
                content: FileContent<'_>,
                context: &Context,
            ) -> Result<(), FileError> {
                self.inner.append_file(path, content, context).await
            }
            async fn truncate_file(&self, path: &str, size: u64, context: &Context) -> Result<(), FileError> {
                self.inner.truncate_file(path, size, context).await
            }
            async fn flush_file(&self, path: &str, context: &Context) -> Result<(), FileError> {
                self.inner.flush_file(path, context).await
            }
            async fn rename_file(
                &self,
                source_path: &str,
                destination_path: &str,
                context: &Context,
            ) -> Result<(), FileError> {
                self.inner.rename_file(source_path, destination_path, context).await
            }
            async fn file_info(&self, path: &str, context: &Context) -> Result<FileInfo, FileError> {
                self.inner.file_info(path, context).await
            }
            async fn list_dir(&self, path: &str, context: &Context) -> Result<Vec<FileInfo>, FileError> {
                self.inner.list_dir(path, context).await
            }
            async fn canonical_path(&self, path: &str, context: &Context) -> Result<String, FileError> {
                self.inner.canonical_path(path, context).await
            }
            async fn exists(&self, path: &str, context: &Context) -> Result<bool, FileError> {
                self.inner.exists(path, context).await
            }
            async fn create_dir(
                &self,
                path: &str,
                options: Option<CreateDirOptions>,
                context: &Context,
            ) -> Result<(), FileError> {
                self.inner.create_dir(path, options, context).await
            }
            async fn remove(
                &self,
                path: &str,
                options: Option<RemoveOptions>,
                context: &Context,
            ) -> Result<(), FileError> {
                self.inner.remove(path, options, context).await
            }
            async fn create_temp_dir(&self, prefix: Option<&str>, context: &Context) -> Result<String, FileError> {
                self.inner.create_temp_dir(prefix, context).await
            }
            async fn create_temp_file(
                &self,
                options: Option<CreateTempFileOptions>,
                context: &Context,
            ) -> Result<String, FileError> {
                self.inner.create_temp_file(options, context).await
            }
        }
    };
}

struct SlowReadExecutionEnv {
    inner: NodeExecutionEnv,
}

impl SlowReadExecutionEnv {
    fn new(cwd: &str) -> Self {
        Self {
            inner: NodeExecutionEnv::new(cwd),
        }
    }
}

#[async_trait]
impl EnvCleanup for SlowReadExecutionEnv {
    async fn cleanup(&self, context: &Context) {
        self.inner.cleanup(context).await;
    }
}

impl_delegating_fs!(
    SlowReadExecutionEnv,
    id: |self| self.inner.id(),
    read: |self, path, context| {
        delay_ms(20).await;
        self.inner.read_text_file(path, context).await
    },
    write: |self, path, content, context| self.inner.write_file(path, content, context).await
);

#[async_trait]
impl Shell for SlowReadExecutionEnv {
    async fn exec(
        &self,
        command: &str,
        options: Option<ShellExecOptions>,
        context: &Context,
    ) -> Result<ShellExecResult, ExecutionError> {
        self.inner.exec(command, options, context).await
    }
}

struct BlockingWriteExecutionEnv {
    inner: NodeExecutionEnv,
    custom_id: Option<String>,
    first_write_started: SignalFlag,
    finish_first_write: SignalFlag,
    second_write_started: AtomicBool,
}

impl BlockingWriteExecutionEnv {
    fn new(cwd: &str, custom_id: Option<String>) -> Self {
        Self {
            inner: NodeExecutionEnv::new(cwd),
            custom_id,
            first_write_started: SignalFlag::new(),
            finish_first_write: SignalFlag::new(),
            second_write_started: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl EnvCleanup for BlockingWriteExecutionEnv {
    async fn cleanup(&self, context: &Context) {
        self.inner.cleanup(context).await;
    }
}

impl_delegating_fs!(
    BlockingWriteExecutionEnv,
    id: |self| self.custom_id.as_deref().unwrap_or_else(|| self.inner.id()),
    read: |self, path, context| self.inner.read_text_file(path, context).await,
    write: |self, path, content, context| {
        if let FileContent::Text(text) = content {
            if text == "first\n" {
                self.first_write_started.resolve();
                self.finish_first_write.wait().await;
            } else if text == "second\n" {
                self.second_write_started.store(true, Ordering::SeqCst);
            }
        }
        self.inner.write_file(path, content, context).await
    }
);

#[async_trait]
impl Shell for BlockingWriteExecutionEnv {
    async fn exec(
        &self,
        command: &str,
        options: Option<ShellExecOptions>,
        context: &Context,
    ) -> Result<ShellExecResult, ExecutionError> {
        self.inner.exec(command, options, context).await
    }
}

struct BlockingEditExecutionEnv {
    inner: NodeExecutionEnv,
    first_edit_write_started: SignalFlag,
    finish_first_edit_write: SignalFlag,
    first_edit_write_settled: AtomicBool,
    second_edit_write_started: AtomicBool,
}

impl BlockingEditExecutionEnv {
    fn new(cwd: &str) -> Self {
        Self {
            inner: NodeExecutionEnv::new(cwd),
            first_edit_write_started: SignalFlag::new(),
            finish_first_edit_write: SignalFlag::new(),
            first_edit_write_settled: AtomicBool::new(false),
            second_edit_write_started: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl EnvCleanup for BlockingEditExecutionEnv {
    async fn cleanup(&self, context: &Context) {
        self.inner.cleanup(context).await;
    }
}

impl_delegating_fs!(
    BlockingEditExecutionEnv,
    id: |self| self.inner.id(),
    read: |self, path, context| self.inner.read_text_file(path, context).await,
    write: |self, path, content, context| {
        if let FileContent::Text(text) = content {
            if text == "ALPHA\nbeta\n" {
                self.first_edit_write_started.resolve();
                self.finish_first_edit_write.wait().await;
                let res = self.inner.write_file(path, content, &BACKGROUND_CONTEXT).await;
                self.first_edit_write_settled.store(true, Ordering::SeqCst);
                return res;
            }
            if text == "ALPHA\nBETA\n" || text == "alpha\nBETA\n" {
                self.second_edit_write_started.store(true, Ordering::SeqCst);
            }
        }
        self.inner.write_file(path, content, context).await
    }
);

#[async_trait]
impl Shell for BlockingEditExecutionEnv {
    async fn exec(
        &self,
        command: &str,
        options: Option<ShellExecOptions>,
        context: &Context,
    ) -> Result<ShellExecResult, ExecutionError> {
        self.inner.exec(command, options, context).await
    }
}

const TRUNCATED_OUTPUT_LINES: usize = DEFAULT_MAX_LINES + 1;

struct TimeoutOutputExecutionEnv {
    inner: NodeExecutionEnv,
}

impl TimeoutOutputExecutionEnv {
    fn new(cwd: &str) -> Self {
        Self {
            inner: NodeExecutionEnv::new(cwd),
        }
    }
}

#[async_trait]
impl EnvCleanup for TimeoutOutputExecutionEnv {
    async fn cleanup(&self, context: &Context) {
        self.inner.cleanup(context).await;
    }
}

impl_delegating_fs!(
    TimeoutOutputExecutionEnv,
    id: |self| self.inner.id(),
    read: |self, path, context| self.inner.read_text_file(path, context).await,
    write: |self, path, content, context| self.inner.write_file(path, content, context).await
);

#[async_trait]
impl Shell for TimeoutOutputExecutionEnv {
    async fn exec(
        &self,
        _command: &str,
        options: Option<ShellExecOptions>,
        context: &Context,
    ) -> Result<ShellExecResult, ExecutionError> {
        let output = format!(
            "{}\n",
            (1..=TRUNCATED_OUTPUT_LINES)
                .map(|idx| format!("line-{idx}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let spill_path = get_or_throw(
            self.inner
                .create_temp_file(
                    Some(CreateTempFileOptions {
                        prefix: Some("timeout-".to_owned()),
                        suffix: Some(".log".to_owned()),
                    }),
                    context,
                )
                .await,
        )
        .map_err(|e| ExecutionError::new(ExecutionErrorCode::SpawnError, e.to_string()))?;
        get_or_throw(
            self.inner
                .write_file(&spill_path, FileContent::Text(&output), context)
                .await,
        )
        .map_err(|e| ExecutionError::new(ExecutionErrorCode::SpawnError, e.to_string()))?;
        if let Some(on_output) = options.as_ref().and_then(|o| o.on_output.as_ref()) {
            let _ = on_output(&output, context);
        }
        let timeout_str = options
            .and_then(|o| o.timeout)
            .map(pi_js::num::to_js_string)
            .unwrap_or_else(|| "undefined".to_owned());
        Err(
            ExecutionError::new(ExecutionErrorCode::Timeout, format!("timeout:{timeout_str}"))
                .with_spill_path(Some(spill_path)),
        )
    }
}

#[tokio::test]
async fn fail_with_ordinary_error_when_no_environment_is_configured() {
    let err = run_tool(
        &create_read_tool(),
        json!({ "path": "x" }),
        None,
        BACKGROUND_CONTEXT.clone(),
    )
    .await
    .err()
    .expect("should reject");
    assert!(err.to_string().contains("No execution environment"), "got: {err}");
}

#[test]
fn read_detects_complete_gif_signatures() {
    for sig in ["GIF87a", "GIF89a"] {
        assert_eq!(detect_supported_image_mime_type(sig.as_bytes()), Some("image/gif"));
    }
}

#[tokio::test]
async fn read_reads_text_with_offsets_and_limits_and_reports_continuation_as_diagnostic() {
    let (_dir, env) = create_env();
    let lines = (1..=100).map(|i| format!("Line {i}")).collect::<Vec<_>>().join("\n");
    get_or_throw(
        env.write_file("test.txt", FileContent::Text(&lines), &BACKGROUND_CONTEXT)
            .await,
    )
    .expect("write");

    let out = run_tool(
        &create_read_tool(),
        json!({ "path": "test.txt", "offset": 41, "limit": 20 }),
        Some(env),
        BACKGROUND_CONTEXT.clone(),
    )
    .await
    .expect("read");
    let text = text_output(&out.result);
    assert!(!text.contains("Line 40"));
    assert!(text.contains("Line 41"));
    assert!(text.contains("Line 60"));
    assert!(!text.contains("Line 61"));
    assert!(!text.contains("more lines"));
    assert_eq!(
        diagnostic_text(&out.result),
        "40 more lines in file. Use offset=61 to continue."
    );
}

#[tokio::test]
async fn read_truncates_large_text_by_line_count() {
    let (_dir, env) = create_env();
    let lines = (1..=2500).map(|i| format!("Line {i}")).collect::<Vec<_>>().join("\n");
    get_or_throw(
        env.write_file("large.txt", FileContent::Text(&lines), &BACKGROUND_CONTEXT)
            .await,
    )
    .expect("write");

    let out = run_tool(
        &create_read_tool(),
        json!({ "path": "large.txt" }),
        Some(env),
        BACKGROUND_CONTEXT.clone(),
    )
    .await
    .expect("read");
    assert_eq!(
        diagnostic_text(&out.result),
        "Showing lines 1-2000 of 2500. Use offset=2001 to continue."
    );
    assert_eq!(
        out.result
            .diagnostics
            .as_ref()
            .and_then(|d| d.first())
            .and_then(|d| d.code.as_deref()),
        Some("truncated")
    );
    let truncation = out
        .result
        .details
        .as_ref()
        .and_then(|d| d.get("truncation"))
        .expect("truncation details");
    assert_eq!(truncation["truncated"], true);
    assert_eq!(truncation["truncatedBy"], "lines");
    assert_eq!(truncation["totalLines"], 2500);
    assert_eq!(truncation["outputLines"], 2000);
}

#[tokio::test]
async fn read_does_not_count_trailing_newline_as_extra_line_at_truncation_limit() {
    let (_dir, env) = create_env();
    let lines = format!("{}\n", vec!["x"; 2000].join("\n"));
    get_or_throw(
        env.write_file("exact.txt", FileContent::Text(&lines), &BACKGROUND_CONTEXT)
            .await,
    )
    .expect("write");

    let out = run_tool(
        &create_read_tool(),
        json!({ "path": "exact.txt" }),
        Some(env),
        BACKGROUND_CONTEXT.clone(),
    )
    .await
    .expect("read");
    assert_eq!(out.result.details, None);
    assert!(out.result.diagnostics.unwrap_or_default().is_empty());
}

#[tokio::test]
async fn read_shows_start_of_line_longer_than_byte_limit() {
    let (_dir, env) = create_env();
    let content = format!("{}\nnext\n", "é".repeat(40_000));
    get_or_throw(
        env.write_file("long.txt", FileContent::Text(&content), &BACKGROUND_CONTEXT)
            .await,
    )
    .expect("write");

    let out = run_tool(
        &create_read_tool(),
        json!({ "path": "long.txt" }),
        Some(env),
        BACKGROUND_CONTEXT.clone(),
    )
    .await
    .expect("read");
    assert_eq!(text_output(&out.result), "é".repeat(25_600));
    assert_eq!(
        diagnostic_text(&out.result),
        "Line 1 is 78.1KB, exceeds the 50.0KB limit; showing its first 50.0KB. Use bash: sed -n '1p' long.txt | tail -c +51201"
    );
    let details = out.result.details.as_ref().expect("details");
    let truncation = details.get("truncation").expect("truncation");
    assert_eq!(truncation["truncated"], true);
    assert_eq!(truncation["firstLineExceedsLimit"], true);
    assert_eq!(truncation["outputBytes"], 51_200);
    assert_eq!(truncation["outputLines"], 1);
    assert!(truncation.get("content").is_none());
}

#[tokio::test]
async fn read_rejects_offsets_beyond_file() {
    let (_dir, env) = create_env();
    get_or_throw(
        env.write_file("short.txt", FileContent::Text("one\ntwo\nthree"), &BACKGROUND_CONTEXT)
            .await,
    )
    .expect("write");
    let err = run_tool(
        &create_read_tool(),
        json!({ "path": "short.txt", "offset": 100 }),
        Some(env),
        BACKGROUND_CONTEXT.clone(),
    )
    .await
    .err()
    .expect("should reject");
    assert!(
        err.to_string()
            .contains("Offset 100 is beyond end of file (3 lines total)"),
        "got: {err}"
    );
}

#[tokio::test]
async fn read_reports_images_by_content_as_unsupported() {
    let (_dir, env) = create_env();
    // 1x1 PNG
    let png: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00,
        0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00,
        0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x60, 0x60, 0x60, 0xF8, 0x0F, 0x00, 0x01, 0x04, 0x01, 0x00,
        0xAF, 0xE5, 0xC3, 0x4B, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    get_or_throw(
        env.write_file("image.txt", FileContent::Bytes(&png), &BACKGROUND_CONTEXT)
            .await,
    )
    .expect("write");

    let out = run_tool(
        &create_read_tool(),
        json!({ "path": "image.txt" }),
        Some(env),
        BACKGROUND_CONTEXT.clone(),
    )
    .await
    .expect("read");
    assert_eq!(out.result.content, Some(Vec::new()));
    assert_eq!(out.result.is_error, Some(true));
    assert_eq!(
        diagnostic_text(&out.result),
        "image.txt is an image (image/png); reading images is not supported"
    );
}

#[tokio::test]
async fn write_writes_files_and_creates_parent_directories() {
    let (_dir, env) = create_env();
    let out = run_tool(
        &create_write_tool(),
        json!({ "path": "nested/dir/file.txt", "content": "hello" }),
        Some(Arc::clone(&env) as Arc<dyn ExecutionEnv>),
        BACKGROUND_CONTEXT.clone(),
    )
    .await
    .expect("write");
    assert_eq!(text_output(&out.result), "Successfully wrote to nested/dir/file.txt");
    let read_back = get_or_throw(env.read_text_file("nested/dir/file.txt", &BACKGROUND_CONTEXT).await).expect("read");
    assert_eq!(read_back, "hello");
}

#[tokio::test]
async fn write_keeps_mutation_queue_locked_until_aborted_write_settles() {
    let (_dir, path) = create_temp_dir();
    let env = Arc::new(BlockingWriteExecutionEnv::new(&path, None));
    let tool = Arc::new(create_write_tool());
    let controller = AbortController::new();
    let aborted_ctx = with_abort_signal(controller.signal(), BACKGROUND_CONTEXT.clone());

    let tool1 = Arc::clone(&tool);
    let env1: Arc<dyn ExecutionEnv> = Arc::clone(&env) as Arc<dyn ExecutionEnv>;
    let first_write = tokio::spawn(async move {
        run_tool(
            &tool1,
            json!({ "path": "file.txt", "content": "first\n" }),
            Some(env1),
            aborted_ctx,
        )
        .await
    });

    env.first_write_started.wait().await;
    controller.abort(None);

    let tool2 = Arc::clone(&tool);
    let env2: Arc<dyn ExecutionEnv> = Arc::clone(&env) as Arc<dyn ExecutionEnv>;
    let second_write = tokio::spawn(async move {
        run_tool(
            &tool2,
            json!({ "path": "file.txt", "content": "second\n" }),
            Some(env2),
            BACKGROUND_CONTEXT.clone(),
        )
        .await
    });

    delay_ms(20).await;
    assert!(!env.second_write_started.load(Ordering::SeqCst));
    env.finish_first_write.resolve();
    assert!(first_write.await.expect("join first").is_err());
    second_write.await.expect("join second").expect("second write");
    let read_back = get_or_throw(env.read_text_file("file.txt", &BACKGROUND_CONTEXT).await).expect("read");
    assert_eq!(read_back, "second\n");
}

#[tokio::test]
async fn edit_applies_disjoint_edits_and_returns_both_diff_formats() {
    let (_dir, env) = create_env();
    let original = "alpha\nbeta\ngamma\ndelta\n";
    get_or_throw(
        env.write_file("edit.txt", FileContent::Text(original), &BACKGROUND_CONTEXT)
            .await,
    )
    .expect("write");

    let out = run_tool(
        &create_edit_tool(),
        json!({
            "path": "edit.txt",
            "edits": [
                { "oldText": "alpha\n", "newText": "ALPHA\n" },
                { "oldText": "gamma\n", "newText": "GAMMA\n" },
            ]
        }),
        Some(Arc::clone(&env) as Arc<dyn ExecutionEnv>),
        BACKGROUND_CONTEXT.clone(),
    )
    .await
    .expect("edit");
    assert_eq!(
        text_output(&out.result),
        "Successfully replaced 2 block(s) in edit.txt."
    );
    let details = out.result.details.as_ref().expect("details");
    let diff = details["diff"].as_str().expect("diff");
    let patch = details["patch"].as_str().expect("patch");
    assert!(diff.contains("ALPHA"));
    assert!(diff.contains("GAMMA"));
    assert_eq!(
        apply_patch(original, patch, Default::default()).expect("apply patch"),
        Some("ALPHA\nbeta\nGAMMA\ndelta\n".to_owned())
    );
    let read_back = get_or_throw(env.read_text_file("edit.txt", &BACKGROUND_CONTEXT).await).expect("read");
    assert_eq!(read_back, "ALPHA\nbeta\nGAMMA\ndelta\n");
}

#[test]
fn edit_repairs_edits_sent_as_json_string_single_object_or_top_level_without_mutating() {
    let prepare = create_edit_tool().prepare_arguments.expect("prepare_arguments");
    let edit = json!({ "oldText": "a", "newText": "b" });
    let edits_json = pi_js::json::stringify(&json!([edit]));
    let as_string = json!({ "path": "f", "edits": edits_json });
    assert_eq!(
        prepare(&as_string).expect("prepare"),
        json!({ "path": "f", "edits": [edit] })
    );
    assert_eq!(as_string["edits"], json!(edits_json));

    let single_json = pi_js::json::stringify(&edit);
    assert_eq!(
        prepare(&json!({ "path": "f", "edits": single_json })).expect("prepare"),
        json!({ "path": "f", "edits": [edit] })
    );
    assert_eq!(
        prepare(&json!({ "path": "f", "edits": edit })).expect("prepare"),
        json!({ "path": "f", "edits": [edit] })
    );
    assert_eq!(
        prepare(&json!({ "path": "f", "edits": [edit], "oldText": "c", "newText": "d" })).expect("prepare"),
        json!({ "path": "f", "edits": [edit, { "oldText": "c", "newText": "d" }] })
    );
    assert_eq!(
        prepare(&json!({ "path": "f", "edits": "not json" })).expect("prepare"),
        json!({ "path": "f", "edits": "not json" })
    );
}

#[tokio::test]
async fn edit_matches_all_edits_against_original_and_rejects_overlaps() {
    let (_dir, env) = create_env();
    get_or_throw(
        env.write_file("edit.txt", FileContent::Text("one\ntwo\nthree\n"), &BACKGROUND_CONTEXT)
            .await,
    )
    .expect("write");
    let err = run_tool(
        &create_edit_tool(),
        json!({
            "path": "edit.txt",
            "edits": [
                { "oldText": "one\ntwo\n", "newText": "ONE\nTWO\n" },
                { "oldText": "two\nthree\n", "newText": "TWO\nTHREE\n" },
            ]
        }),
        Some(Arc::clone(&env) as Arc<dyn ExecutionEnv>),
        BACKGROUND_CONTEXT.clone(),
    )
    .await
    .err()
    .expect("overlap should fail");
    assert!(err.to_string().contains("overlap"), "got: {err}");
    let read_back = get_or_throw(env.read_text_file("edit.txt", &BACKGROUND_CONTEXT).await).expect("read");
    assert_eq!(read_back, "one\ntwo\nthree\n");
}

#[tokio::test]
async fn edit_rejects_missing_and_duplicate_target_text() {
    let (_dir, env) = create_env();
    get_or_throw(
        env.write_file("edit.txt", FileContent::Text("foo foo foo"), &BACKGROUND_CONTEXT)
            .await,
    )
    .expect("write");
    let tool = create_edit_tool();
    let err1 = run_tool(
        &tool,
        json!({ "path": "edit.txt", "edits": [{ "oldText": "bar", "newText": "baz" }] }),
        Some(Arc::clone(&env) as Arc<dyn ExecutionEnv>),
        BACKGROUND_CONTEXT.clone(),
    )
    .await
    .err()
    .expect("missing should fail");
    assert!(
        err1.to_string().contains("Could not find the exact text"),
        "got: {err1}"
    );

    let err2 = run_tool(
        &tool,
        json!({ "path": "edit.txt", "edits": [{ "oldText": "foo", "newText": "bar" }] }),
        Some(env as Arc<dyn ExecutionEnv>),
        BACKGROUND_CONTEXT.clone(),
    )
    .await
    .err()
    .expect("duplicate should fail");
    assert!(err2.to_string().contains("Found 3 occurrences"), "got: {err2}");
}

#[tokio::test]
async fn edit_keeps_mutation_queue_locked_until_aborted_edit_write_settles() {
    let (_dir, path) = create_temp_dir();
    let env = Arc::new(BlockingEditExecutionEnv::new(&path));
    get_or_throw(
        env.inner
            .write_file("file.txt", FileContent::Text("alpha\nbeta\n"), &BACKGROUND_CONTEXT)
            .await,
    )
    .expect("write");

    let tool = Arc::new(create_edit_tool());
    let controller = AbortController::new();
    let aborted_ctx = with_abort_signal(controller.signal(), BACKGROUND_CONTEXT.clone());

    let tool1 = Arc::clone(&tool);
    let env1: Arc<dyn ExecutionEnv> = Arc::clone(&env) as Arc<dyn ExecutionEnv>;
    let first_edit = tokio::spawn(async move {
        run_tool(
            &tool1,
            json!({ "path": "file.txt", "edits": [{ "oldText": "alpha", "newText": "ALPHA" }] }),
            Some(env1),
            aborted_ctx,
        )
        .await
    });

    env.first_edit_write_started.wait().await;
    controller.abort(None);

    let tool2 = Arc::clone(&tool);
    let env2: Arc<dyn ExecutionEnv> = Arc::clone(&env) as Arc<dyn ExecutionEnv>;
    let second_edit = tokio::spawn(async move {
        run_tool(
            &tool2,
            json!({ "path": "file.txt", "edits": [{ "oldText": "beta", "newText": "BETA" }] }),
            Some(env2),
            BACKGROUND_CONTEXT.clone(),
        )
        .await
    });

    delay_ms(20).await;
    assert!(!env.second_edit_write_started.load(Ordering::SeqCst));
    env.finish_first_edit_write.resolve();

    let err = first_edit
        .await
        .expect("join first")
        .err()
        .expect("first edit should fail");
    assert!(err.to_string().contains("Operation aborted"), "got: {err}");
    second_edit.await.expect("join second").expect("second edit");
    assert!(env.first_edit_write_settled.load(Ordering::SeqCst));
    let read_back = get_or_throw(env.read_text_file("file.txt", &BACKGROUND_CONTEXT).await).expect("read");
    assert_eq!(read_back, "ALPHA\nBETA\n");
}

#[cfg(unix)]
#[tokio::test]
async fn edit_serializes_concurrent_edits_through_canonical_and_symlink_paths() {
    let (_dir, path) = create_temp_dir();
    let env = Arc::new(SlowReadExecutionEnv::new(&path));
    get_or_throw(
        env.write_file(
            "target.txt",
            FileContent::Text("alpha\nbeta\ngamma\n"),
            &BACKGROUND_CONTEXT,
        )
        .await,
    )
    .expect("write");
    std::os::unix::fs::symlink("target.txt", format!("{path}/link.txt")).expect("symlink");

    let tool = create_edit_tool();
    let (r1, r2) = tokio::join!(
        run_tool(
            &tool,
            json!({ "path": "target.txt", "edits": [{ "oldText": "alpha", "newText": "ALPHA" }] }),
            Some(Arc::clone(&env) as Arc<dyn ExecutionEnv>),
            BACKGROUND_CONTEXT.clone()
        ),
        run_tool(
            &tool,
            json!({ "path": "link.txt", "edits": [{ "oldText": "beta", "newText": "BETA" }] }),
            Some(Arc::clone(&env) as Arc<dyn ExecutionEnv>),
            BACKGROUND_CONTEXT.clone()
        )
    );
    r1.expect("r1");
    r2.expect("r2");
    let read_back = get_or_throw(env.read_text_file("target.txt", &BACKGROUND_CONTEXT).await).expect("read");
    assert_eq!(read_back, "ALPHA\nBETA\ngamma\n");
}

#[tokio::test]
async fn edit_serializes_edits_of_one_file_across_environment_objects_of_one_filesystem() {
    let (_dir, dir) = create_temp_dir();
    let first = Arc::new(SlowReadExecutionEnv::new(&dir));
    let second = Arc::new(SlowReadExecutionEnv::new(&dir));
    get_or_throw(
        first
            .write_file("file.txt", FileContent::Text("alpha\nbeta\n"), &BACKGROUND_CONTEXT)
            .await,
    )
    .expect("write");
    let tool = create_edit_tool();
    let (r1, r2) = tokio::join!(
        run_tool(
            &tool,
            json!({ "path": "file.txt", "edits": [{ "oldText": "alpha", "newText": "ALPHA" }] }),
            Some(Arc::clone(&first) as Arc<dyn ExecutionEnv>),
            BACKGROUND_CONTEXT.clone()
        ),
        run_tool(
            &tool,
            json!({ "path": "file.txt", "edits": [{ "oldText": "beta", "newText": "BETA" }] }),
            Some(second as Arc<dyn ExecutionEnv>),
            BACKGROUND_CONTEXT.clone()
        )
    );
    r1.expect("r1");
    r2.expect("r2");
    let read_back = get_or_throw(first.read_text_file("file.txt", &BACKGROUND_CONTEXT).await).expect("read");
    assert_eq!(read_back, "ALPHA\nBETA\n");
}

#[cfg(unix)]
#[tokio::test]
async fn edit_serializes_new_file_created_through_symlinked_directory_with_canonical_path() {
    let (_dir, path) = create_temp_dir();
    let env = Arc::new(BlockingWriteExecutionEnv::new(&path, None));
    std::fs::create_dir(format!("{path}/real")).expect("mkdir real");
    std::os::unix::fs::symlink(format!("{path}/real"), format!("{path}/link")).expect("symlink");

    let tool = Arc::new(create_write_tool());
    let tool1 = Arc::clone(&tool);
    let env1: Arc<dyn ExecutionEnv> = Arc::clone(&env) as Arc<dyn ExecutionEnv>;
    let first = tokio::spawn(async move {
        run_tool(
            &tool1,
            json!({ "path": "link/new.txt", "content": "first\n" }),
            Some(env1),
            BACKGROUND_CONTEXT.clone(),
        )
        .await
    });
    env.first_write_started.wait().await;

    let tool2 = Arc::clone(&tool);
    let env2: Arc<dyn ExecutionEnv> = Arc::clone(&env) as Arc<dyn ExecutionEnv>;
    let second = tokio::spawn(async move {
        run_tool(
            &tool2,
            json!({ "path": "real/new.txt", "content": "second\n" }),
            Some(env2),
            BACKGROUND_CONTEXT.clone(),
        )
        .await
    });
    delay_ms(20).await;
    assert!(!env.second_write_started.load(Ordering::SeqCst));
    env.finish_first_write.resolve();
    first.await.expect("join first").expect("first");
    second.await.expect("join second").expect("second");
    let read_back = get_or_throw(env.read_text_file("real/new.txt", &BACKGROUND_CONTEXT).await).expect("read");
    assert_eq!(read_back, "second\n");
}

#[cfg(not(windows))]
#[tokio::test]
async fn edit_keys_missing_file_whose_name_contains_backslash_like_created_file() {
    let (_dir, env) = create_env();
    let created = SignalFlag::new();
    let release = SignalFlag::new();

    let env1 = Arc::clone(&env) as Arc<dyn ExecutionEnv>;
    let env1_inner = Arc::clone(&env);
    let created_sig = created.clone();
    let release_sig = release.clone();
    let first = tokio::spawn(async move {
        with_file_mutation_queue(
            env1.as_ref(),
            "a\\b.txt",
            || async move {
                get_or_throw(
                    env1_inner
                        .write_file("a\\b.txt", FileContent::Text("first\n"), &BACKGROUND_CONTEXT)
                        .await,
                )?;
                created_sig.resolve();
                release_sig.wait().await;
                Ok(())
            },
            &BACKGROUND_CONTEXT,
        )
        .await
    });

    created.wait().await;
    let entered = Arc::new(AtomicBool::new(false));
    let entered_clone = Arc::clone(&entered);
    let env2 = Arc::clone(&env) as Arc<dyn ExecutionEnv>;
    let second = tokio::spawn(async move {
        with_file_mutation_queue(
            env2.as_ref(),
            "a\\b.txt",
            || async move {
                entered_clone.store(true, Ordering::SeqCst);
                Ok(())
            },
            &BACKGROUND_CONTEXT,
        )
        .await
    });

    delay_ms(20).await;
    assert!(!entered.load(Ordering::SeqCst));
    release.resolve();
    first.await.expect("join first").expect("first");
    second.await.expect("join second").expect("second");
    assert!(entered.load(Ordering::SeqCst));
}

#[tokio::test]
async fn edit_does_not_serialize_same_path_on_different_filesystems() {
    let (_dir, dir) = create_temp_dir();
    let local = Arc::new(BlockingWriteExecutionEnv::new(&dir, None));
    let other = Arc::new(BlockingWriteExecutionEnv::new(&dir, Some("other".to_owned())));
    let tool = Arc::new(create_write_tool());

    let tool1 = Arc::clone(&tool);
    let local_env: Arc<dyn ExecutionEnv> = Arc::clone(&local) as Arc<dyn ExecutionEnv>;
    let blocked = tokio::spawn(async move {
        run_tool(
            &tool1,
            json!({ "path": "file.txt", "content": "first\n" }),
            Some(local_env),
            BACKGROUND_CONTEXT.clone(),
        )
        .await
    });
    local.first_write_started.wait().await;

    run_tool(
        &tool,
        json!({ "path": "file.txt", "content": "second\n" }),
        Some(Arc::clone(&other) as Arc<dyn ExecutionEnv>),
        BACKGROUND_CONTEXT.clone(),
    )
    .await
    .expect("other");
    assert!(other.second_write_started.load(Ordering::SeqCst));
    local.finish_first_write.resolve();
    blocked.await.expect("join blocked").expect("blocked");
}

#[cfg(unix)]
#[tokio::test]
async fn edit_edits_regular_files_through_symlinks() {
    let (_dir, env) = create_env();
    get_or_throw(
        env.write_file("target.txt", FileContent::Text("before\n"), &BACKGROUND_CONTEXT)
            .await,
    )
    .expect("write");
    std::os::unix::fs::symlink("target.txt", format!("{}/link.txt", env.cwd())).expect("symlink");
    run_tool(
        &create_edit_tool(),
        json!({ "path": "link.txt", "edits": [{ "oldText": "before", "newText": "after" }] }),
        Some(Arc::clone(&env) as Arc<dyn ExecutionEnv>),
        BACKGROUND_CONTEXT.clone(),
    )
    .await
    .expect("edit");
    let read_back = get_or_throw(env.read_text_file("target.txt", &BACKGROUND_CONTEXT).await).expect("read");
    assert_eq!(read_back, "after\n");
}

#[tokio::test]
async fn edit_preserves_bom_and_crlf_line_endings() {
    let (_dir, env) = create_env();
    get_or_throw(
        env.write_file(
            "edit.txt",
            FileContent::Text("\u{FEFF}one\r\ntwo\r\n"),
            &BACKGROUND_CONTEXT,
        )
        .await,
    )
    .expect("write");
    run_tool(
        &create_edit_tool(),
        json!({ "path": "edit.txt", "edits": [{ "oldText": "two", "newText": "TWO" }] }),
        Some(Arc::clone(&env) as Arc<dyn ExecutionEnv>),
        BACKGROUND_CONTEXT.clone(),
    )
    .await
    .expect("edit");
    let read_back = get_or_throw(env.read_text_file("edit.txt", &BACKGROUND_CONTEXT).await).expect("read");
    assert_eq!(read_back, "\u{FEFF}one\r\nTWO\r\n");
}

#[tokio::test]
async fn bash_streams_combined_stdout_and_stderr_and_returns_no_content_of_its_own() {
    let (_dir, env) = create_env();
    let out = run_tool(
        &create_bash_tool(None),
        json!({ "command": "printf out; printf err >&2" }),
        Some(env as Arc<dyn ExecutionEnv>),
        BACKGROUND_CONTEXT.clone(),
    )
    .await
    .expect("bash");
    let joined = out.output.join("");
    assert!(joined.contains("out"), "joined: {joined}");
    assert!(joined.contains("err"), "joined: {joined}");
    assert_eq!(out.result.content, None);
}

#[tokio::test]
async fn bash_throws_on_nonzero_exits_and_timeouts_after_streaming_output() {
    let (_dir, env) = create_env();
    let tool = create_bash_tool(None);
    let failed = run_failing(
        &tool,
        json!({ "command": "printf failed; exit 7" }),
        Arc::clone(&env) as Arc<dyn ExecutionEnv>,
    )
    .await;
    assert_eq!(failed.error.to_string(), "Command exited with code 7");
    assert_eq!(failed.output.join(""), "failed");

    let slow = run_failing(
        &tool,
        json!({ "command": "sleep 2", "timeout": 0.01 }),
        env as Arc<dyn ExecutionEnv>,
    )
    .await;
    assert_eq!(slow.error.to_string(), "Command timed out after 0.01 seconds");
}

#[tokio::test]
async fn bash_reports_spill_of_command_that_times_out() {
    let (_dir, path) = create_temp_dir();
    let env = Arc::new(TimeoutOutputExecutionEnv::new(&path));
    let failed = run_failing(
        &create_bash_tool(None),
        json!({ "command": "emit-output-then-time-out", "timeout": 0.05 }),
        Arc::clone(&env) as Arc<dyn ExecutionEnv>,
    )
    .await;
    assert_eq!(failed.error.to_string(), "Command timed out after 0.05 seconds");
    let msg = failed.reported.first().map(|d| d.message.as_str()).unwrap_or("");
    let full_output_path = msg.strip_prefix("Full output: ").expect("Full output diagnostic");
    let full_output =
        get_or_throw(env.read_text_file(full_output_path, &BACKGROUND_CONTEXT).await).expect("read spill");
    assert!(full_output.contains("line-1\nline-2"));
    assert!(full_output.contains(&format!("line-{DEFAULT_MAX_LINES}\nline-{TRUNCATED_OUTPUT_LINES}")));
}

#[tokio::test]
async fn bash_prepares_command_cwd_and_explicit_environment_with_calls_api() {
    let (_dir, path) = create_temp_dir();
    let mut shell_env = IndexMap::new();
    shell_env.insert("PI_BASH_PREPARE_INHERITED".to_owned(), "inherited".to_owned());
    let env = Arc::new(NodeExecutionEnv::with_options(NodeExecutionEnvOptions {
        cwd: path,
        shell_path: None,
        shell_env: Some(shell_env),
    }));
    get_or_throw(env.create_dir("workspace", None, &BACKGROUND_CONTEXT).await).expect("mkdir");
    let workspace = format!("{}/workspace", env.cwd());
    let controller = AbortController::new();
    let received_env_id = Arc::new(Mutex::new(None::<String>));
    let received_aborted = Arc::new(Mutex::new(None::<bool>));
    let received_env_id_cb = Arc::clone(&received_env_id);
    let received_aborted_cb = Arc::clone(&received_aborted);
    let workspace_cb = workspace.clone();
    let tool = create_bash_tool(Some(BashToolOptions {
        command_prefix: Some("prefix=ready".to_owned()),
        prepare: Some(Arc::new(move |execution, api, call_context| {
            *received_env_id_cb.lock().unwrap_or_else(|e| e.into_inner()) = api.env().map(|e| e.cwd().to_owned());
            *received_aborted_cb.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(call_context.abort_signal().is_some());
            execution.cwd = workspace_cb.clone();
            let mut explicit = IndexMap::new();
            explicit.insert("PI_BASH_PREPARE_EXPLICIT".to_owned(), "explicit".to_owned());
            execution.env = explicit;
            execution.inherit_env = false;
            execution.command.push_str(
                "\nprintf '%s:%s:%s:%s' \"$prefix\" \"${PI_BASH_PREPARE_INHERITED-}\" \"$PI_BASH_PREPARE_EXPLICIT\" \"$PWD\"",
            );
            Box::pin(async { Ok(()) })
        })),
    }));

    let out = run_tool(
        &tool,
        json!({ "command": ":" }),
        Some(Arc::clone(&env) as Arc<dyn ExecutionEnv>),
        with_abort_signal(controller.signal(), BACKGROUND_CONTEXT.clone()),
    )
    .await
    .expect("bash");
    assert_eq!(
        received_env_id.lock().unwrap_or_else(|e| e.into_inner()).as_deref(),
        Some(env.cwd())
    );
    assert_eq!(*received_aborted.lock().unwrap_or_else(|e| e.into_inner()), Some(true));
    let canonical = get_or_throw(env.canonical_path(&workspace, &BACKGROUND_CONTEXT).await).expect("canonical");
    assert_eq!(out.output.join(""), format!("ready::explicit:{canonical}"));
}

#[tokio::test]
async fn bash_supports_command_prefixes() {
    let (_dir, env) = create_env();
    let out = run_tool(
        &create_bash_tool(Some(BashToolOptions {
            command_prefix: Some("value=hello".to_owned()),
            prepare: None,
        })),
        json!({ "command": "printf $value" }),
        Some(env as Arc<dyn ExecutionEnv>),
        BACKGROUND_CONTEXT.clone(),
    )
    .await
    .expect("bash");
    assert_eq!(out.output.join(""), "hello");
}

#[tokio::test]
async fn bash_streams_every_byte_and_spills_complete_output_beyond_default_limits() {
    let (_dir, env) = create_env();
    let out = run_tool(
        &create_bash_tool(None),
        json!({ "command": "i=1; while [ $i -le 3000 ]; do echo line-$i; i=$((i + 1)); done" }),
        Some(Arc::clone(&env) as Arc<dyn ExecutionEnv>),
        BACKGROUND_CONTEXT.clone(),
    )
    .await
    .expect("bash");
    let expected = (1..=3000).map(|i| format!("line-{i}\n")).collect::<Vec<_>>().join("");
    assert_eq!(out.output.join(""), expected);
    let msg = out.reported.first().map(|d| d.message.as_str()).unwrap_or("");
    let full_output_path = msg.strip_prefix("Full output: ").expect("Full output diagnostic");
    let read_back = get_or_throw(env.read_text_file(full_output_path, &BACKGROUND_CONTEXT).await).expect("read spill");
    assert_eq!(read_back, expected);
}

#[tokio::test]
async fn bash_does_not_spill_output_within_limits() {
    let (_dir, env) = create_env();
    let out = run_tool(
        &create_bash_tool(None),
        json!({ "command": "printf small" }),
        Some(env as Arc<dyn ExecutionEnv>),
        BACKGROUND_CONTEXT.clone(),
    )
    .await
    .expect("bash");
    assert!(out.reported.is_empty());
}
