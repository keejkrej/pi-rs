//! Port of packages/evals/src/harness.ts plus pure-Rust equivalents of `vitest-evals` harness & judge primitives.

use std::sync::{Arc, Mutex};

use indexmap::{IndexMap, IndexSet};
use pi_agent_core::AgentMessage;
use pi_ai::{
    AnyModel, AssistantContentBlock, AuthOperationOptions, ContentTextSource, CredentialStore, InMemoryCredentialStore,
    StopReason, UserContentBlock, content_text, utils::transcript::get_current_system_prompt,
};
use pi_coding_agent::config::get_agent_dir;
use pi_coding_agent::core::agent_session::AgentSession;
use pi_coding_agent::core::auth_storage::read_stored_credential;
use pi_coding_agent::core::extensions::types::{
    BeforeAgentStartEvent, BeforeAgentStartEventResult, ExtensionApi, InlineExtension, NamedInlineExtension,
    ToolDefinition,
};
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_coding_agent::core::resource_loader::DefaultResourceLoaderOptions;
use pi_coding_agent::core::sdk::{
    CreateAgentSessionFromServicesOptions, CreateAgentSessionServicesOptions, NoToolsMode,
    create_agent_session_from_services, create_agent_session_services,
};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_js::abort::{AbortReason, AbortSignal};
use pi_js::{BoxFuture, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::plan::DocumentationVariant;
pub use crate::report::{
    EvalMeta, EvalScore, HarnessMeta, HarnessRun, NormalizedError, NormalizedSession, NormalizedSpan,
    NormalizedSpanEvent, NormalizedSpanKind, NormalizedSpanStatus, NormalizedTrace, TimingSummary, ToolCall,
    TranscriptEvent, TranscriptRole, UsageSummary, VitestJsonAssertion, VitestJsonStatus, tool_calls,
    tool_calls_in_session,
};
use crate::report::{PI_SESSION_SNAPSHOT_ARTIFACT, read_eval_task_meta};

/// Documentation evals intentionally exclude shell and unrestricted network tools.
pub const DOCUMENTATION_EVAL_TOOLS: [&str; 6] = ["read", "write", "edit", "grep", "find", "ls"];

// ---------------------------------------------------------------------------
// Pure-Rust `vitest-evals/harness` normalization, tracing, and judge support
// ---------------------------------------------------------------------------

/// Custom error payload attaching a partial or completed [`HarnessRun`] to a failure.
#[derive(Debug)]
pub struct HarnessRunError {
    pub message: String,
    pub run: HarnessRun,
}

impl std::fmt::Display for HarnessRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HarnessRunError {}

/// Attaches a normalized [`HarnessRun`] onto `error` so eval runners keep telemetry on failure.
pub fn attach_harness_run_to_error(error: Error, run: HarnessRun) -> Error {
    Error::typed(
        "HarnessRunError",
        HarnessRunError {
            message: error.to_string(),
            run,
        },
    )
}

/// Extracts an attached [`HarnessRun`] from an error created by [`attach_harness_run_to_error`].
pub fn get_harness_run_from_error(error: &Error) -> Option<&HarnessRun> {
    error.downcast_ref::<HarnessRunError>().map(|inner| &inner.run)
}

/// Normalizes a [`Value`] to JSON-safe primitives (`pi_js::json` semantics).
pub fn to_json_value(value: Value) -> Value {
    match value {
        Value::Null => Value::Null,
        Value::Bool(b) => Value::Bool(b),
        Value::Number(n) => match n.as_f64() {
            Some(f) => pi_js::json::number_value(f),
            None => Value::Null,
        },
        Value::String(s) => Value::String(s),
        Value::Array(items) => Value::Array(items.into_iter().map(to_json_value).collect()),
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k, to_json_value(v));
            }
            Value::Object(out)
        }
    }
}

/// Normalizes any `Serialize` value into a JSON-safe [`Value`].
pub fn serialize_to_json_value<T: Serialize + ?Sized>(value: &T) -> Value {
    match pi_js::json::try_stringify(value) {
        Ok(raw) => pi_js::json::parse(&raw).unwrap_or(Value::Null),
        Err(_) => Value::Null,
    }
}

/// Normalizes an ordered map of arbitrary JSON values into JSON-safe values.
pub fn normalize_record(record: &IndexMap<String, Value>) -> IndexMap<String, Value> {
    let mut out = IndexMap::with_capacity(record.len());
    for (k, v) in record {
        out.insert(k.clone(), to_json_value(v.clone()));
    }
    out
}

/// Runtime context passed to a [`Harness`] execution callback.
#[derive(Clone)]
pub struct HarnessContext {
    pub signal: Option<AbortSignal>,
    artifacts: Arc<Mutex<IndexMap<String, Value>>>,
}

impl HarnessContext {
    /// Creates a new harness context with an optional abort signal and empty artifacts.
    pub fn new(signal: Option<AbortSignal>) -> Self {
        Self {
            signal,
            artifacts: Arc::new(Mutex::new(IndexMap::new())),
        }
    }

    /// Stores a JSON-safe artifact under `name`.
    pub fn set_artifact(&self, name: impl Into<String>, value: Value) {
        let mut guard = self.artifacts.lock().unwrap_or_else(|e| e.into_inner());
        guard.insert(name.into(), to_json_value(value));
    }

    /// Returns a cloned snapshot of all recorded artifacts.
    pub fn artifacts(&self) -> IndexMap<String, Value> {
        self.artifacts.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Lightweight harness execution result accepted by [`normalize_harness_run`].
#[derive(Clone, Debug, Default)]
pub struct SimpleHarnessResult {
    pub output: Option<Value>,
    pub events: Vec<TranscriptEvent>,
    pub session_provider: Option<String>,
    pub session_model: Option<String>,
    pub session_metadata: Option<IndexMap<String, Value>>,
    pub usage: UsageSummary,
    pub timings: Option<TimingSummary>,
    pub artifacts: Option<IndexMap<String, Value>>,
    pub traces: Option<Vec<NormalizedTrace>>,
    pub errors: Vec<IndexMap<String, Value>>,
}

/// Converts a [`SimpleHarnessResult`] and accumulated artifacts into a canonical [`HarnessRun`].
pub fn normalize_harness_run(
    result: SimpleHarnessResult,
    context_artifacts: Option<&IndexMap<String, Value>>,
) -> HarnessRun {
    let artifacts = match (result.artifacts, context_artifacts) {
        (Some(mut a), Some(b)) => {
            for (k, v) in b {
                a.entry(k.clone()).or_insert_with(|| v.clone());
            }
            if a.is_empty() { None } else { Some(a) }
        }
        (Some(a), None) => (!a.is_empty()).then_some(a),
        (None, Some(b)) => (!b.is_empty()).then(|| b.clone()),
        (None, None) => None,
    };
    HarnessRun {
        output: result.output.map(to_json_value),
        session: NormalizedSession {
            events: result.events,
            provider: result.session_provider,
            model: result.session_model,
            metadata: result.session_metadata,
        },
        usage: result.usage,
        timings: result.timings,
        artifacts,
        traces: result.traces,
        errors: result.errors,
    }
}

/// Builds a normalized [`HarnessRun`] representing a failed execution before partial telemetry exists.
pub fn create_failed_harness_run(error: &Error, artifacts: Option<&IndexMap<String, Value>>) -> HarnessRun {
    let mut err_map = IndexMap::new();
    err_map.insert("message".to_string(), Value::String(error.to_string()));
    err_map.insert("name".to_string(), Value::String(error.name().to_string()));
    HarnessRun {
        output: None,
        session: NormalizedSession {
            events: Vec::new(),
            provider: None,
            model: None,
            metadata: None,
        },
        usage: UsageSummary::default(),
        timings: None,
        artifacts: artifacts.filter(|m| !m.is_empty()).cloned(),
        traces: None,
        errors: vec![err_map],
    }
}

/// Attaches a fallback root trace span when `run` recorded none.
pub fn ensure_run_trace(run: &mut HarnessRun, name: &str, started_at: &str, finished_at: &str, duration_ms: f64) {
    if run.traces.as_ref().is_some_and(|t| !t.is_empty()) {
        return;
    }
    let trace_id = pi_js::crypto::random_uuid();
    let span_status = if run.errors.is_empty() {
        NormalizedSpanStatus::Ok
    } else {
        NormalizedSpanStatus::Error
    };
    let span_error = run.errors.first().map(|err| NormalizedError {
        message: err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Eval harness failed")
            .to_string(),
        r#type: err.get("name").and_then(Value::as_str).map(ToString::to_string),
        extra: IndexMap::new(),
    });
    run.traces = Some(vec![NormalizedTrace {
        id: Some(trace_id.clone()),
        name: Some(name.to_string()),
        started_at: Some(started_at.to_string()),
        finished_at: Some(finished_at.to_string()),
        duration_ms: Some(duration_ms),
        metadata: None,
        spans: vec![NormalizedSpan {
            id: Some(pi_js::crypto::random_uuid()),
            trace_id: Some(trace_id),
            parent_id: None,
            name: name.to_string(),
            kind: Some(NormalizedSpanKind::Run),
            started_at: Some(started_at.to_string()),
            finished_at: Some(finished_at.to_string()),
            duration_ms: Some(duration_ms),
            status: Some(span_status),
            error: span_error,
            attributes: None,
            events: None,
            metadata: None,
        }],
    }]);
}

/// Asynchronous eval harness adapter producing a [`HarnessRun`].
#[derive(Clone)]
pub struct Harness<I> {
    pub name: String,
    runner: Arc<dyn Fn(I, HarnessContext) -> BoxFuture<Result<HarnessRun>> + Send + Sync>,
}

impl<I: Send + Sync + 'static> Harness<I> {
    /// Executes the harness for `input` within `context`.
    pub async fn run(&self, input: I, context: &HarnessContext) -> Result<HarnessRun> {
        (self.runner)(input, context.clone()).await
    }
}

/// Creates a named [`Harness`] wrapping an async callback returning [`SimpleHarnessResult`].
pub fn create_harness<I: Send + Sync + 'static>(
    name: impl Into<String>,
    run_fn: impl Fn(I, HarnessContext) -> BoxFuture<Result<SimpleHarnessResult>> + Send + Sync + 'static,
) -> Harness<I> {
    let name_str = name.into();
    let name_for_trace = name_str.clone();
    Harness {
        name: name_str,
        runner: Arc::new(move |input, ctx| {
            let fn_ref = &run_fn;
            let trace_name = name_for_trace.clone();
            let started_ms = pi_js::time::performance_now();
            let started_iso = pi_js::time::iso_now();
            let fut = fn_ref(input, ctx.clone());
            Box::pin(async move {
                match fut.await {
                    Ok(simple) => {
                        let duration_ms = (pi_js::time::performance_now() - started_ms).max(0.0);
                        let finished_iso = pi_js::time::iso_now();
                        let ctx_artifacts = ctx.artifacts();
                        let mut run = normalize_harness_run(simple, Some(&ctx_artifacts));
                        if run.timings.is_none() {
                            run.timings = Some(TimingSummary {
                                total_ms: Some(duration_ms),
                                metadata: None,
                            });
                        }
                        ensure_run_trace(&mut run, &trace_name, &started_iso, &finished_iso, duration_ms);
                        Ok(run)
                    }
                    Err(err) => {
                        let duration_ms = (pi_js::time::performance_now() - started_ms).max(0.0);
                        let finished_iso = pi_js::time::iso_now();
                        if let Some(partial) = get_harness_run_from_error(&err) {
                            let mut patched = partial.clone();
                            let ctx_artifacts = ctx.artifacts();
                            if !ctx_artifacts.is_empty() && patched.artifacts.is_none() {
                                patched.artifacts = Some(ctx_artifacts);
                            }
                            ensure_run_trace(&mut patched, &trace_name, &started_iso, &finished_iso, duration_ms);
                            return Err(attach_harness_run_to_error(err, patched));
                        }
                        Err(err)
                    }
                }
            })
        }),
    }
}

// ---------------------------------------------------------------------------
// Judges & structured/tool-call scoring (`vitest-evals` parity)
// ---------------------------------------------------------------------------

/// Input passed to a [`Judge`] evaluator.
#[derive(Clone, Debug)]
pub struct JudgeContext {
    pub input: Value,
    pub output: Option<Value>,
    pub tool_calls: Vec<ToolCall>,
    pub run: HarnessRun,
    pub session: NormalizedSession,
    pub signal: Option<AbortSignal>,
}

/// Evaluation score and metadata produced by a [`Judge`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JudgeResult {
    pub score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<IndexMap<String, Value>>,
}

/// Named judge runnable against a completed [`HarnessRun`].
#[derive(Clone)]
pub struct Judge {
    pub name: String,
    assess_fn: Arc<dyn Fn(JudgeContext) -> BoxFuture<Result<JudgeResult>> + Send + Sync>,
}

impl Judge {
    /// Evaluates a harness execution and returns a [`JudgeResult`].
    pub async fn assess(&self, context: JudgeContext) -> Result<JudgeResult> {
        (self.assess_fn)(context).await
    }
}

/// Constructs a custom [`Judge`] from a name and async assessment closure.
pub fn create_judge(
    name: impl Into<String>,
    assess: impl Fn(JudgeContext) -> BoxFuture<Result<JudgeResult>> + Send + Sync + 'static,
) -> Judge {
    Judge {
        name: name.into(),
        assess_fn: Arc::new(assess),
    }
}

/// Matching mode for [`structured_output_judge`] and [`tool_call_judge`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum StructuredOutputMatchMode {
    #[default]
    Strict,
    Fuzzy,
}

/// Fuzzy matcher configuration matching `vitest-evals` defaults.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FuzzyMatchOptions {
    pub substring: bool,
    pub case_insensitive: bool,
    pub ignore_array_order: bool,
    pub numeric_tolerance: f64,
    pub coerce_types: bool,
}

impl Default for FuzzyMatchOptions {
    fn default() -> Self {
        Self {
            substring: true,
            case_insensitive: true,
            ignore_array_order: true,
            numeric_tolerance: 1e-3,
            coerce_types: false,
        }
    }
}

fn values_match(expected: &Value, actual: &Value, mode: StructuredOutputMatchMode, fuzzy: &FuzzyMatchOptions) -> bool {
    match mode {
        StructuredOutputMatchMode::Strict => expected == actual,
        StructuredOutputMatchMode::Fuzzy => fuzzy_values_match(expected, actual, fuzzy),
    }
}

fn fuzzy_values_match(expected: &Value, actual: &Value, opts: &FuzzyMatchOptions) -> bool {
    match (expected, actual) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Number(a), Value::Number(b)) => match (a.as_f64(), b.as_f64()) {
            (Some(x), Some(y)) => (x - y).abs() <= opts.numeric_tolerance,
            _ => a == b,
        },
        (Value::String(exp), Value::String(act)) => {
            let (e, a) = if opts.case_insensitive {
                (exp.to_lowercase(), act.to_lowercase())
            } else {
                (exp.clone(), act.clone())
            };
            if opts.substring { a.contains(&e) } else { a == e }
        }
        (Value::Array(exp), Value::Array(act)) => {
            if exp.len() != act.len() {
                return false;
            }
            if !opts.ignore_array_order {
                return exp.iter().zip(act.iter()).all(|(e, a)| fuzzy_values_match(e, a, opts));
            }
            let mut used = vec![false; act.len()];
            for exp_item in exp {
                let mut found = false;
                for (idx, act_item) in act.iter().enumerate() {
                    if !used[idx] && fuzzy_values_match(exp_item, act_item, opts) {
                        used[idx] = true;
                        found = true;
                        break;
                    }
                }
                if !found {
                    return false;
                }
            }
            true
        }
        (Value::Object(exp), Value::Object(act)) => exp
            .iter()
            .all(|(k, exp_v)| act.get(k).is_some_and(|act_v| fuzzy_values_match(exp_v, act_v, opts))),
        _ => expected == actual,
    }
}

/// Configuration for [`structured_output_judge`].
#[derive(Clone, Debug)]
pub struct StructuredOutputJudgeOptions {
    pub expected: Value,
    pub match_mode: StructuredOutputMatchMode,
    pub require_all: bool,
    pub allow_extras: bool,
    pub error_field: Option<String>,
    pub fuzzy: FuzzyMatchOptions,
}

impl StructuredOutputJudgeOptions {
    /// Creates options comparing output strictly or fuzzily against `expected`.
    pub fn new(expected: Value) -> Self {
        Self {
            expected,
            match_mode: StructuredOutputMatchMode::Strict,
            require_all: true,
            allow_extras: true,
            error_field: Some("error".to_string()),
            fuzzy: FuzzyMatchOptions::default(),
        }
    }
}

fn format_judge_json_value(value: &Value) -> String {
    match value {
        Value::String(s) => format!("\"{s}\""),
        other => pi_js::json::stringify(other),
    }
}

/// Creates a judge checking structured JSON output against expected keys and values.
pub fn structured_output_judge(options: StructuredOutputJudgeOptions) -> Judge {
    create_judge("StructuredOutputJudge", move |ctx| {
        let options = options.clone();
        Box::pin(async move {
            let output_text = match &ctx.output {
                None => String::new(),
                Some(Value::String(s)) => s.clone(),
                Some(other) => pi_js::json::stringify(other),
            };
            let parsed = match pi_js::json::parse(&output_text) {
                Ok(v) => v,
                Err(err) => {
                    let mut meta = IndexMap::new();
                    meta.insert(
                        "rationale".to_string(),
                        Value::String(format!("Failed to parse output as JSON: {err}")),
                    );
                    meta.insert("output".to_string(), Value::String(output_text));
                    return Ok(JudgeResult {
                        score: Some(0.0),
                        metadata: Some(meta),
                    });
                }
            };
            let Some(expected_map) = options.expected.as_object() else {
                let matched = values_match(&options.expected, &parsed, options.match_mode, &options.fuzzy);
                let mut meta = IndexMap::new();
                meta.insert(
                    "rationale".to_string(),
                    Value::String(if matched {
                        "Structured output matched".to_string()
                    } else {
                        format!(
                            "Expected {}, got {}",
                            format_judge_json_value(&options.expected),
                            format_judge_json_value(&parsed)
                        )
                    }),
                );
                return Ok(JudgeResult {
                    score: Some(if matched { 1.0 } else { 0.0 }),
                    metadata: Some(meta),
                });
            };
            if expected_map.is_empty() {
                let mut meta = IndexMap::new();
                meta.insert(
                    "rationale".to_string(),
                    Value::String("Valid JSON output (no expected fields specified)".to_string()),
                );
                return Ok(JudgeResult {
                    score: Some(1.0),
                    metadata: Some(meta),
                });
            }
            let Some(actual_map) = parsed.as_object() else {
                let mut meta = IndexMap::new();
                meta.insert(
                    "rationale".to_string(),
                    Value::String("Output JSON is not an object".to_string()),
                );
                return Ok(JudgeResult {
                    score: Some(0.0),
                    metadata: Some(meta),
                });
            };
            if let Some(error_key) = &options.error_field
                && let Some(err_val) = actual_map.get(error_key)
                && !err_val.is_null()
                && err_val != &Value::String(String::new())
                && err_val != &Value::Bool(false)
            {
                let mut meta = IndexMap::new();
                meta.insert(
                    "rationale".to_string(),
                    Value::String(format!("Output contains error: {err_val}")),
                );
                meta.insert("output".to_string(), Value::String(output_text));
                return Ok(JudgeResult {
                    score: Some(0.0),
                    metadata: Some(meta),
                });
            }

            let mut matches = Vec::new();
            let mut mismatches = Vec::new();
            let mut extras = Vec::new();
            for (key, expected_val) in expected_map {
                match actual_map.get(key) {
                    Some(actual_val) if values_match(expected_val, actual_val, options.match_mode, &options.fuzzy) => {
                        matches.push(key.clone());
                    }
                    actual_opt => {
                        let actual_repr = actual_opt
                            .map(format_judge_json_value)
                            .unwrap_or_else(|| "undefined".to_string());
                        mismatches.push((key.clone(), format_judge_json_value(expected_val), actual_repr));
                    }
                }
            }
            for key in actual_map.keys() {
                if !expected_map.contains_key(key) {
                    extras.push(key.clone());
                }
            }

            let total_expected = expected_map.len();
            let total_matched = matches.len();
            if options.require_all && !mismatches.is_empty() {
                let keys: Vec<&str> = mismatches.iter().map(|(k, _, _)| k.as_str()).collect();
                let details: Vec<String> = mismatches
                    .iter()
                    .map(|(k, exp, act)| format!("{k}: expected {exp}, got {act}"))
                    .collect();
                let mut meta = IndexMap::new();
                meta.insert(
                    "rationale".to_string(),
                    Value::String(format!(
                        "Missing required fields: {} - {}",
                        keys.join(", "),
                        details.join("; ")
                    )),
                );
                return Ok(JudgeResult {
                    score: Some(0.0),
                    metadata: Some(meta),
                });
            }
            if !options.allow_extras && !extras.is_empty() {
                let mut meta = IndexMap::new();
                meta.insert(
                    "rationale".to_string(),
                    Value::String(format!("Unexpected extra fields: {}", extras.join(", "))),
                );
                return Ok(JudgeResult {
                    score: Some(0.0),
                    metadata: Some(meta),
                });
            }
            let score = total_matched as f64 / total_expected as f64;
            let mut meta = IndexMap::new();
            if (score - 1.0).abs() < f64::EPSILON {
                let extra_info = if extras.is_empty() {
                    String::new()
                } else {
                    format!(" (plus extra fields: {})", extras.join(", "))
                };
                meta.insert(
                    "rationale".to_string(),
                    Value::String(format!("All expected fields match{extra_info}")),
                );
                return Ok(JudgeResult {
                    score: Some(1.0),
                    metadata: Some(meta),
                });
            }
            let details: Vec<String> = mismatches
                .iter()
                .map(|(k, exp, act)| format!("{k}: expected {exp}, got {act}"))
                .collect();
            meta.insert(
                "rationale".to_string(),
                Value::String(format!(
                    "Matched {total_matched}/{total_expected} fields - {}",
                    details.join("; ")
                )),
            );
            meta.insert("matched".to_string(), Value::from(total_matched as i64));
            meta.insert("total".to_string(), Value::from(total_expected as i64));
            Ok(JudgeResult {
                score: Some(score),
                metadata: Some(meta),
            })
        })
    })
}

/// Expected tool invocation assertion passed to [`tool_call_judge`].
#[derive(Clone, Debug, PartialEq)]
pub struct ExpectedToolCall {
    pub name: String,
    pub arguments: Option<IndexMap<String, Value>>,
}

/// Configuration for [`tool_call_judge`].
#[derive(Clone, Debug)]
pub struct ToolCallJudgeOptions {
    pub expected_tools: Vec<ExpectedToolCall>,
    pub ordered: bool,
    pub require_all: bool,
    pub allow_extras: bool,
    pub params: StructuredOutputMatchMode,
    pub fuzzy: FuzzyMatchOptions,
}

impl Default for ToolCallJudgeOptions {
    fn default() -> Self {
        Self {
            expected_tools: Vec::new(),
            ordered: false,
            require_all: true,
            allow_extras: true,
            params: StructuredOutputMatchMode::Strict,
            fuzzy: FuzzyMatchOptions::default(),
        }
    }
}

fn tool_args_match(
    expected: Option<&IndexMap<String, Value>>,
    actual: Option<&IndexMap<String, Value>>,
    mode: StructuredOutputMatchMode,
    fuzzy: &FuzzyMatchOptions,
) -> bool {
    let Some(exp_map) = expected else {
        return true;
    };
    let empty = IndexMap::new();
    let act_map = actual.unwrap_or(&empty);
    let exp_val = serialize_to_json_value(exp_map);
    let act_val = serialize_to_json_value(act_map);
    values_match(&exp_val, &act_val, mode, fuzzy)
}

/// Creates a judge checking observed tool calls and optional arguments.
pub fn tool_call_judge(options: ToolCallJudgeOptions) -> Judge {
    create_judge("ToolCallJudge", move |ctx| {
        let options = options.clone();
        Box::pin(async move {
            let expected = &options.expected_tools;
            let actual = &ctx.tool_calls;
            let mut meta = IndexMap::new();
            if expected.is_empty() {
                meta.insert(
                    "rationale".to_string(),
                    Value::String("No tool calls expected".to_string()),
                );
                return Ok(JudgeResult {
                    score: Some(1.0),
                    metadata: Some(meta),
                });
            }
            if actual.is_empty() {
                meta.insert(
                    "rationale".to_string(),
                    Value::String(format!("Expected {} tool(s) but none were called", expected.len())),
                );
                return Ok(JudgeResult {
                    score: Some(0.0),
                    metadata: Some(meta),
                });
            }
            if options.ordered {
                let mut exp_idx = 0usize;
                let mut act_idx = 0usize;
                while exp_idx < expected.len() && act_idx < actual.len() {
                    let exp_tool = &expected[exp_idx];
                    let act_tool = &actual[act_idx];
                    if exp_tool.name == act_tool.name() {
                        if !tool_args_match(
                            exp_tool.arguments.as_ref(),
                            act_tool.arguments(),
                            options.params,
                            &options.fuzzy,
                        ) {
                            meta.insert(
                                "rationale".to_string(),
                                Value::String(format!(
                                    "Tool '{}' called with incorrect arguments at position {} ({}/{} tools matched correctly)",
                                    exp_tool.name,
                                    exp_idx + 1,
                                    exp_idx,
                                    expected.len()
                                )),
                            );
                            meta.insert("matched".to_string(), Value::from(exp_idx as i64));
                            meta.insert("total".to_string(), Value::from(expected.len() as i64));
                            return Ok(JudgeResult {
                                score: Some(exp_idx as f64 / expected.len() as f64),
                                metadata: Some(meta),
                            });
                        }
                        exp_idx += 1;
                    } else if !options.allow_extras {
                        meta.insert(
                            "rationale".to_string(),
                            Value::String(format!(
                                "Unexpected tool call '{}' while waiting for '{}'",
                                act_tool.name(),
                                exp_tool.name
                            )),
                        );
                        return Ok(JudgeResult {
                            score: Some(0.0),
                            metadata: Some(meta),
                        });
                    }
                    act_idx += 1;
                }
                if exp_idx < expected.len() && options.require_all {
                    meta.insert(
                        "rationale".to_string(),
                        Value::String(format!("Matched {exp_idx}/{} expected tools in order", expected.len())),
                    );
                    return Ok(JudgeResult {
                        score: Some(0.0),
                        metadata: Some(meta),
                    });
                }
                let score = exp_idx as f64 / expected.len() as f64;
                meta.insert(
                    "rationale".to_string(),
                    Value::String(format!("Matched {exp_idx}/{} expected tools in order", expected.len())),
                );
                return Ok(JudgeResult {
                    score: Some(score),
                    metadata: Some(meta),
                });
            }

            let mut used = vec![false; actual.len()];
            let mut matched = 0usize;
            for exp_tool in expected {
                if let Some((idx, _)) = actual.iter().enumerate().find(|(i, act)| {
                    !used[*i]
                        && act.name() == exp_tool.name
                        && tool_args_match(
                            exp_tool.arguments.as_ref(),
                            act.arguments(),
                            options.params,
                            &options.fuzzy,
                        )
                }) {
                    used[idx] = true;
                    matched += 1;
                }
            }
            if !options.allow_extras && used.iter().any(|u| !*u) {
                meta.insert(
                    "rationale".to_string(),
                    Value::String("Unexpected extra tool calls observed".to_string()),
                );
                return Ok(JudgeResult {
                    score: Some(0.0),
                    metadata: Some(meta),
                });
            }
            if options.require_all && matched < expected.len() {
                meta.insert(
                    "rationale".to_string(),
                    Value::String(format!("Matched {matched}/{} expected tools", expected.len())),
                );
                return Ok(JudgeResult {
                    score: Some(0.0),
                    metadata: Some(meta),
                });
            }
            let score = matched as f64 / expected.len() as f64;
            meta.insert(
                "rationale".to_string(),
                Value::String(format!("Matched {matched}/{} expected tools", expected.len())),
            );
            Ok(JudgeResult {
                score: Some(score),
                metadata: Some(meta),
            })
        })
    })
}

/// Evaluates all judges on a completed `run` and attaches `eval` metadata alongside `harness` metadata.
pub async fn evaluate_run_with_judges(
    harness_name: &str,
    input: Value,
    run: HarnessRun,
    judges: &[Judge],
    judge_threshold: Option<Option<f64>>,
    signal: Option<AbortSignal>,
) -> Result<(EvalTaskMetaOption, Option<String>)> {
    if judges.is_empty() {
        let harness_meta = HarnessMeta {
            name: Some(harness_name.to_string()),
            run: Some(run),
        };
        return Ok((
            EvalTaskMetaOption {
                eval: None,
                harness: Some(harness_meta),
            },
            None,
        ));
    }

    let run_tool_calls = tool_calls(&run);
    let mut scores = Vec::with_capacity(judges.len());
    for judge in judges {
        let res = judge
            .assess(JudgeContext {
                input: input.clone(),
                output: run.output.clone(),
                tool_calls: run_tool_calls.clone(),
                run: run.clone(),
                session: run.session.clone(),
                signal: signal.clone(),
            })
            .await?;
        scores.push(EvalScore {
            name: Some(judge.name.clone()),
            score: res.score,
            metadata: res.metadata,
        });
    }
    let avg_score = scores.iter().map(|s| s.score.unwrap_or(0.0)).sum::<f64>() / scores.len() as f64;
    let threshold = match judge_threshold {
        None => Some(1.0),
        Some(inner) => inner,
    };
    let threshold_failed = threshold.is_some_and(|min| avg_score < min);
    let failure_msg = if threshold_failed {
        let min = threshold.unwrap_or(1.0);
        Some(format!(
            "Score: {} below threshold: {}",
            pi_js::num::to_fixed(avg_score, 2),
            pi_js::num::to_fixed(min, 2)
        ))
    } else {
        None
    };

    let output_value = run.output.clone().or_else(|| Some(Value::String(String::new())));
    let eval_meta = EvalMeta {
        scores: Some(scores),
        avg_score: Some(avg_score),
        output: output_value,
        threshold_failed: Some(threshold_failed),
        tool_calls: Some(run_tool_calls),
    };
    let harness_meta = HarnessMeta {
        name: Some(harness_name.to_string()),
        run: Some(run),
    };
    Ok((
        EvalTaskMetaOption {
            eval: Some(eval_meta),
            harness: Some(harness_meta),
        },
        failure_msg,
    ))
}

/// Meta container serialized onto `VitestJsonAssertion.meta`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalTaskMetaOption {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eval: Option<EvalMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<HarnessMeta>,
}

/// Target suite classification matching `vitest.evals.config.ts` projects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvalSuiteKind {
    Docs,
    Host,
    Audit,
}

/// Discovered entry serialized into `discovered-tests.json` for `--discover`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredTestEntry {
    pub name: String,
    pub file: String,
}

/// Declarative executable eval case registered inside an [`EvalSuite`].
#[derive(Clone)]
pub struct EvalCaseDef {
    pub eval_set: String,
    pub case_id: String,
    pub file: String,
    pub execute: Arc<dyn Fn(Option<AbortSignal>) -> BoxFuture<Result<VitestJsonAssertion>> + Send + Sync>,
}

impl EvalCaseDef {
    /// Returns `"<eval_set> > <case_id>"` discovery identity.
    pub fn discovered_name(&self) -> String {
        format!("{} > {}", self.eval_set, self.case_id)
    }

    /// Returns `"<eval_set> <case_id>"` reported assertion full name.
    pub fn full_name(&self) -> String {
        format!("{} {}", self.eval_set, self.case_id)
    }
}

/// Collection of related [`EvalCaseDef`] cases sharing a suite file and kind.
#[derive(Clone)]
pub struct EvalSuite {
    pub file: String,
    pub kind: EvalSuiteKind,
    pub cases: Vec<EvalCaseDef>,
}

/// Builds a [`VitestJsonAssertion`] from executing a [`Harness`] with judges and optional assertion hook.
#[allow(clippy::too_many_arguments)]
pub async fn run_eval_case<I, F>(
    eval_set: &str,
    case_id: &str,
    harness: &Harness<I>,
    input: I,
    input_json: Value,
    judges: &[Judge],
    judge_threshold: Option<Option<f64>>,
    signal: Option<AbortSignal>,
    verify: Option<F>,
) -> Result<VitestJsonAssertion>
where
    I: Send + Sync + 'static,
    F: FnOnce(&HarnessRun) -> Result<()>,
{
    let full_name = format!("{eval_set} {case_id}");
    let ctx = HarnessContext::new(signal.clone());
    let started_ms = pi_js::time::performance_now();
    let run_result = harness.run(input, &ctx).await;
    let duration_ms = (pi_js::time::performance_now() - started_ms).max(0.0);

    match run_result {
        Ok(run) => {
            let mut verify_err: Option<String> = None;
            if let Some(check) = verify
                && let Err(err) = check(&run)
            {
                verify_err = Some(err.to_string());
            }
            let (meta_obj, judge_err) =
                evaluate_run_with_judges(&harness.name, input_json, run, judges, judge_threshold, signal).await?;
            let mut failures = Vec::new();
            if let Some(msg) = verify_err {
                failures.push(msg);
            }
            if let Some(msg) = judge_err {
                failures.push(msg);
            }
            let status = if failures.is_empty() {
                VitestJsonStatus::Passed
            } else {
                VitestJsonStatus::Failed
            };
            let meta_val = serialize_to_json_value(&meta_obj);
            let _ = read_eval_task_meta(Some(&meta_val));
            Ok(VitestJsonAssertion {
                ancestor_titles: vec![eval_set.to_string()],
                full_name,
                status,
                title: case_id.to_string(),
                meta: Some(meta_val),
                duration: Some(duration_ms),
                failure_messages: Some(failures),
                location: None,
                tags: None,
            })
        }
        Err(err) => {
            let err_message = err.to_string();
            let partial = get_harness_run_from_error(&err).cloned();
            let meta_val = if let Some(run) = partial {
                let meta_obj = EvalTaskMetaOption {
                    eval: None,
                    harness: Some(HarnessMeta {
                        name: Some(harness.name.clone()),
                        run: Some(run),
                    }),
                };
                Some(serialize_to_json_value(&meta_obj))
            } else {
                let failed = create_failed_harness_run(&err, Some(&ctx.artifacts()));
                let meta_obj = EvalTaskMetaOption {
                    eval: None,
                    harness: Some(HarnessMeta {
                        name: Some(harness.name.clone()),
                        run: Some(failed),
                    }),
                };
                Some(serialize_to_json_value(&meta_obj))
            };
            Ok(VitestJsonAssertion {
                ancestor_titles: vec![eval_set.to_string()],
                full_name,
                status: VitestJsonStatus::Failed,
                title: case_id.to_string(),
                meta: meta_val,
                duration: Some(duration_ms),
                failure_messages: Some(vec![err_message]),
                location: None,
                tags: None,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// `packages/evals/src/harness.ts` Pi Coding Agent harness & sandbox
// ---------------------------------------------------------------------------

/// Input step for multi-turn [`PiCodingAgentInput::Steps`] evals.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PiCodingAgentStep {
    Prompt { content: String },
    Reload,
}

/// Input accepted by [`create_pi_coding_agent_harness`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PiCodingAgentInput {
    Text(String),
    Steps(Vec<PiCodingAgentStep>),
}

impl From<&str> for PiCodingAgentInput {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<String> for PiCodingAgentInput {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<Vec<PiCodingAgentStep>> for PiCodingAgentInput {
    fn from(value: Vec<PiCodingAgentStep>) -> Self {
        Self::Steps(value)
    }
}

/// Explicit provider + model identifier passed into harness configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PiCodingAgentModelSelection {
    pub provider: String,
    pub id: String,
}

/// Environment view used by [`resolve_model_selection`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelSelectionEnvironment {
    pub pi_provider: Option<String>,
    pub pi_model: Option<String>,
}

/// Context passed into a custom harness output extractor.
pub struct PiOutputExtractorArgs<'a> {
    pub response: &'a str,
    pub session: &'a AgentSession,
    pub system_prompt: &'a str,
    pub agent_dir: &'a str,
}

/// Extractor callback transforming a completed Pi session into structured eval output.
pub type PiOutputExtractor = Arc<dyn Fn(&AgentSession, &str, &str, &str) -> BoxFuture<Result<Value>> + Send + Sync>;

/// Transform function rewriting the default Pi system prompt in `without_docs` runs.
pub type SystemPromptTransform = Arc<dyn Fn(&str) -> Result<String> + Send + Sync>;

/// Options controlling [`create_pi_coding_agent_harness`].
#[derive(Clone, Default)]
pub struct PiCodingAgentHarnessOptions {
    pub name: Option<String>,
    pub model: Option<PiCodingAgentModelSelection>,
    pub no_tools: Option<NoToolsMode>,
    pub tools: Option<Vec<String>>,
    pub custom_tools: Option<Vec<ToolDefinition>>,
    pub workspace_files: Option<IndexMap<String, String>>,
    pub transform_system_prompt: Option<SystemPromptTransform>,
    pub expected_pi_documentation: Option<bool>,
    pub output: Option<PiOutputExtractor>,
}

/// Resolves the model selection from an explicit override or `PI_PROVIDER` / `PI_MODEL`.
pub fn resolve_model_selection(
    explicit_model: Option<&PiCodingAgentModelSelection>,
    environment: Option<&ModelSelectionEnvironment>,
) -> Result<PiCodingAgentModelSelection> {
    let owned_env;
    let env = match environment {
        Some(e) => e,
        None => {
            owned_env = ModelSelectionEnvironment {
                pi_provider: pi_js::env::var("PI_PROVIDER"),
                pi_model: pi_js::env::var("PI_MODEL"),
            };
            &owned_env
        }
    };
    let provider = explicit_model
        .map(|m| m.provider.as_str())
        .or(env.pi_provider.as_deref())
        .map(|s| pi_js::str16::trim(s).to_string())
        .filter(|s| !s.is_empty());
    let id = explicit_model
        .map(|m| m.id.as_str())
        .or(env.pi_model.as_deref())
        .map(|s| pi_js::str16::trim(s).to_string())
        .filter(|s| !s.is_empty());
    match (provider, id) {
        (Some(provider), Some(id)) => Ok(PiCodingAgentModelSelection { provider, id }),
        _ => Err(Error::msg(
            "Select a harness model explicitly or set both PI_PROVIDER and PI_MODEL as defaults.",
        )),
    }
}

/// RAII guard restoring isolated environment overrides (`HOME`, `USERPROFILE`, `PI_CODING_AGENT_DIR`, `PI_EVAL_*`).
#[must_use = "the previous process environment is restored when the guard drops or restore() is called"]
pub struct IsolatedEnvironmentGuard {
    previous: IndexMap<String, Option<String>>,
    restored: bool,
}

impl IsolatedEnvironmentGuard {
    /// Immediately restores the previous environment variables.
    pub fn restore(mut self) {
        self.apply_restore();
    }

    fn apply_restore(&mut self) {
        if self.restored {
            return;
        }
        self.restored = true;
        for (name, value) in &self.previous {
            match value {
                Some(val) => pi_js::env::set_var(name, val),
                None => pi_js::env::remove_var(name),
            }
        }
    }
}

impl Drop for IsolatedEnvironmentGuard {
    fn drop(&mut self) {
        self.apply_restore();
    }
}

/// Strips `PI_EVAL_*` runner metadata and isolates `HOME`, `USERPROFILE`, and `PI_CODING_AGENT_DIR`.
pub fn apply_isolated_environment(home: &str, agent_dir: &str) -> IsolatedEnvironmentGuard {
    let mut previous = IndexMap::new();
    for (name, val) in pi_js::env::vars() {
        if name.starts_with("PI_EVAL_") {
            previous.insert(name.clone(), Some(val));
            pi_js::env::remove_var(&name);
        }
    }
    for (name, value) in [
        ("HOME", home),
        ("USERPROFILE", home),
        ("PI_CODING_AGENT_DIR", agent_dir),
    ] {
        if !previous.contains_key(name) {
            previous.insert(name.to_string(), pi_js::env::var(name));
        }
        pi_js::env::set_var(name, value);
    }
    IsolatedEnvironmentGuard {
        previous,
        restored: false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SandboxIdentity {
    uid: u32,
    gid: u32,
}

fn parse_sandbox_id(name: &str) -> Result<Option<u32>> {
    let Some(value) = pi_js::env::var(name) else {
        return Ok(None);
    };
    let parsed = value.parse::<i64>().ok();
    match parsed {
        Some(id) if id >= 1 && u32::try_from(id).is_ok() => Ok(Some(id as u32)),
        _ => Err(Error::msg(format!("{name} must be a positive integer."))),
    }
}

fn resolve_sandbox_identity() -> Result<Option<SandboxIdentity>> {
    let uid = parse_sandbox_id("PI_EVAL_SANDBOX_UID")?;
    let gid = parse_sandbox_id("PI_EVAL_SANDBOX_GID")?;
    match (uid, gid) {
        (None, None) => Ok(None),
        (Some(uid), Some(gid)) => Ok(Some(SandboxIdentity { uid, gid })),
        _ => Err(Error::msg(
            "Set both PI_EVAL_SANDBOX_UID and PI_EVAL_SANDBOX_GID, or neither.",
        )),
    }
}

#[cfg(unix)]
fn chown_tree<'a>(path: &'a str, identity: SandboxIdentity) -> BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        let stats = pi_js::fs::promises::lstat(path).await?;
        if stats.is_directory() && !stats.is_symbolic_link() {
            for entry in pi_js::fs::promises::readdir(path).await? {
                let child = pi_js::path::join(&[path, &entry]);
                chown_tree(&child, identity).await?;
            }
        }
        pi_js::fs::promises::chown(path, identity.uid, identity.gid).await
    })
}

async fn enter_tool_sandbox(root: &str, identity: Option<SandboxIdentity>) -> Result<()> {
    let Some(identity) = identity else {
        return Ok(());
    };
    #[cfg(not(unix))]
    {
        let _ = (root, identity);
        Err(Error::msg("The eval filesystem sandbox requires POSIX user APIs."))
    }
    #[cfg(unix)]
    {
        if !nix::unistd::getuid().is_root() || !nix::unistd::geteuid().is_root() {
            return Err(Error::msg(
                "The eval runner must start as root before entering the unprivileged tool sandbox.",
            ));
        }
        // PORT: TS calls protectTransformedModules() for Vitest /tmp/*/ssr JS transform caches;
        // native Rust binaries contain no /tmp JS SSR cache.
        chown_tree(root, identity).await?;
        #[cfg(all(
            unix,
            not(any(target_os = "macos", target_os = "ios", target_os = "tvos", target_os = "watchos"))
        ))]
        nix::unistd::setgroups(&[])
            .map_err(|err| Error::msg(format!("Failed to clear supplementary groups: {err}")))?;
        nix::unistd::setgid(nix::unistd::Gid::from_raw(identity.gid))
            .map_err(|err| Error::msg(format!("Failed to drop sandbox GID: {err}")))?;
        nix::unistd::setuid(nix::unistd::Uid::from_raw(identity.uid))
            .map_err(|err| Error::msg(format!("Failed to drop sandbox UID: {err}")))?;
        if nix::unistd::getuid().as_raw() != identity.uid || nix::unistd::geteuid().as_raw() != identity.uid {
            return Err(Error::msg("Failed to enter the unprivileged eval tool sandbox."));
        }
        Ok(())
    }
}

fn to_transcript_events(messages: &[AgentMessage]) -> Vec<TranscriptEvent> {
    let mut events = Vec::new();
    for message in messages {
        match message {
            AgentMessage::Message(pi_ai::Message::User(user)) => {
                events.push(TranscriptEvent::Message {
                    role: TranscriptRole::User,
                    content: Some(Value::String(content_text(
                        ContentTextSource::User(&user.content),
                        None,
                    ))),
                    metadata: None,
                });
            }
            AgentMessage::Message(pi_ai::Message::Assistant(assistant)) => {
                let text = content_text(ContentTextSource::Assistant(&assistant.content), None);
                if !text.is_empty() {
                    events.push(TranscriptEvent::Message {
                        role: TranscriptRole::Assistant,
                        content: Some(Value::String(text)),
                        metadata: None,
                    });
                }
                for block in &assistant.content {
                    if let AssistantContentBlock::ToolCall(tc) = block {
                        events.push(TranscriptEvent::ToolCall {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            arguments: Some(normalize_record(&tc.arguments)),
                            started_at: None,
                            finished_at: None,
                            duration_ms: None,
                            metadata: None,
                        });
                    }
                }
            }
            AgentMessage::Message(pi_ai::Message::ToolResult(tr)) => {
                let text = content_text(ContentTextSource::ToolResult(&tr.content), None);
                let all_text = tr.content.iter().all(|part| matches!(part, UserContentBlock::Text(_)));
                let content = if all_text {
                    Value::String(text.clone())
                } else {
                    serialize_to_json_value(&tr.content)
                };
                events.push(TranscriptEvent::ToolResult {
                    tool_call_id: tr.tool_call_id.clone(),
                    name: Some(tr.tool_name.clone()),
                    content: Some(content),
                    error: tr.is_error.then(|| NormalizedError {
                        message: if text.is_empty() {
                            "Tool failed".to_string()
                        } else {
                            text
                        },
                        r#type: None,
                        extra: IndexMap::new(),
                    }),
                    started_at: None,
                    finished_at: None,
                    duration_ms: None,
                    metadata: None,
                });
            }
            _ => {}
        }
    }
    events
}

async fn seed_workspace(workspace: &str, files: Option<&IndexMap<String, String>>) -> Result<()> {
    let Some(files) = files else {
        return Ok(());
    };
    let sep = pi_js::path::sep();
    let escape_prefix = format!("..{sep}");
    for (name, content) in files {
        if name.is_empty() || pi_js::path::is_absolute(name) {
            return Err(Error::js(
                "TypeError",
                format!("Invalid workspace fixture path: {name}"),
            ));
        }
        let path = pi_js::path::resolve(&[workspace, name]);
        let rel = pi_js::path::relative(workspace, &path);
        if rel == ".." || rel.starts_with(&escape_prefix) || pi_js::path::is_absolute(&rel) {
            return Err(Error::js(
                "TypeError",
                format!("Workspace fixture escapes the workspace: {name}"),
            ));
        }
        let parent = pi_js::path::dirname(&path);
        pi_js::fs::promises::mkdir(&parent, true, None).await?;
        pi_js::fs::promises::write_file(&path, content.as_bytes(), Some(0o600)).await?;
    }
    Ok(())
}

async fn prompt_agent(session: &AgentSession, input: &str, signal: Option<&AbortSignal>) -> Result<String> {
    if let Some(sig) = signal {
        sig.throw_if_aborted()?;
    }
    let previous_count = session.messages().len();
    session.prompt(input, None).await?;
    let messages = session.messages();
    let assistant = messages
        .get(previous_count..)
        .unwrap_or(&[])
        .iter()
        .rev()
        .find_map(AgentMessage::as_assistant);
    let Some(assistant) = assistant else {
        return Err(Error::msg("Agent run completed without an assistant message."));
    };
    if assistant.stop_reason != StopReason::Stop && assistant.stop_reason != StopReason::ToolUse {
        let msg = assistant.error_message.clone().unwrap_or_else(|| {
            format!(
                "Agent run ended with unexpected stop reason: {:?}.",
                assistant.stop_reason
            )
        });
        return Err(Error::msg(msg));
    }
    let output = session.get_last_assistant_text();
    let empty_output = output.as_deref().is_none_or(str::is_empty);
    if empty_output && assistant.stop_reason == StopReason::Stop {
        return Err(Error::msg("Agent run produced no assistant text."));
    }
    Ok(output.unwrap_or_default())
}

/// Validates that the system prompt matches the variant's expected Pi documentation section.
pub fn verify_system_prompt(
    system_prompt: &str,
    name: Option<&str>,
    expected_pi_documentation: Option<bool>,
) -> Result<String> {
    let Some(expected) = expected_pi_documentation else {
        return Ok(system_prompt.to_string());
    };
    let variant_label = name.unwrap_or("unknown");
    if !system_prompt.contains("\n<rules>\n") {
        return Err(Error::msg(format!(
            "Pi system prompt lost its rules in the {variant_label} eval variant."
        )));
    }
    let has_documentation = system_prompt.contains("\n<docs>\nPi documentation (read only");
    if has_documentation != expected {
        return Err(Error::msg(format!(
            "Pi system prompt does not match the {variant_label} eval variant."
        )));
    }
    Ok(system_prompt.to_string())
}

fn is_ascii_env_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes[0].is_ascii_uppercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || *b == b'_')
}

async fn run_pi_coding_agent(
    input: PiCodingAgentInput,
    signal: Option<AbortSignal>,
    ctx: HarnessContext,
    options: PiCodingAgentHarnessOptions,
) -> Result<SimpleHarnessResult> {
    let started_at = pi_js::time::performance_now();
    if let Some(sig) = &signal {
        sig.throw_if_aborted()?;
    }
    let selection = resolve_model_selection(options.model.as_ref(), None)?;
    let host_agent_dir = get_agent_dir();
    let sandbox_identity = resolve_sandbox_identity()?;
    let tmp_prefix = pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-eval-"]);
    let root = pi_js::fs::promises::mkdtemp(&tmp_prefix).await?;
    let workspace = pi_js::path::join(&[&root, "workspace"]);
    let isolated_home = pi_js::path::join(&[&root, "home"]);
    let agent_dir = pi_js::path::join(&[&isolated_home, ".pi", "agent"]);

    let forced_system_prompt: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let mut extension_factories: Vec<InlineExtension> = Vec::new();
    if let Some(transform) = options.transform_system_prompt.clone() {
        let captured = Arc::clone(&forced_system_prompt);
        extension_factories.push(InlineExtension::Named(NamedInlineExtension {
            name: "eval-system-prompt-transform".to_string(),
            hidden: true,
            replaceable: false,
            builtin: false,
            factory: Arc::new(move |pi: ExtensionApi| {
                let transform = Arc::clone(&transform);
                let captured = Arc::clone(&captured);
                Box::pin(async move {
                    pi.on::<BeforeAgentStartEvent>(move |event, _ctx| {
                        let transform = Arc::clone(&transform);
                        let captured = Arc::clone(&captured);
                        Box::pin(async move {
                            let next_prompt = transform(&event.system_prompt)?;
                            *captured.lock().unwrap_or_else(|e| e.into_inner()) = Some(next_prompt.clone());
                            Ok(Some(BeforeAgentStartEventResult {
                                system_prompt: Some(next_prompt),
                                ..Default::default()
                            }))
                        })
                    });
                    Ok(())
                })
            }),
        }));
    }

    let mut session_manager: Option<SessionManager> = None;
    let mut session: Option<AgentSession> = None;
    let mut run_diagnostics: Option<(Vec<TranscriptEvent>, IndexMap<String, Value>, UsageSummary)> = None;
    let mut hidden_credential_env: Option<(String, String)> = None;
    let isolated_guard = apply_isolated_environment(&isolated_home, &agent_dir);

    let run_result: Result<SimpleHarnessResult> = async {
        let auth_path = pi_js::path::join(&[&host_agent_dir, "auth.json"]);
        let credentials = Arc::new(InMemoryCredentialStore::new());
        let stored_credential = read_stored_credential(&selection.provider, Some(&auth_path));
        if let Some(ref cred) = stored_credential {
            let cloned = cred.clone();
            credentials
                .modify_with(&selection.provider, move |_| async move { Ok(Some(cloned)) }, None)
                .await?;
        }

        let model_runtime = ModelRuntime::create(Some(CreateModelRuntimeOptions {
            credentials: Some(credentials as Arc<dyn CredentialStore>),
            ..Default::default()
        }))
        .await?;

        pi_js::fs::promises::mkdir(&workspace, true, None).await?;
        pi_js::fs::promises::mkdir(&agent_dir, true, None).await?;
        seed_workspace(&workspace, options.workspace_files.as_ref()).await?;

        let model = model_runtime
            .get_model(&selection.provider, &selection.id)
            .ok_or_else(|| Error::msg(format!("Eval model not found: {}/{}", selection.provider, selection.id)))?;
        let auth = model_runtime
            .get_auth_model(&AnyModel::Chat(model.clone()), None)
            .await?
            .ok_or_else(|| {
                Error::msg(format!(
                    "Eval model has no configured authentication: {}/{}",
                    selection.provider, selection.id
                ))
            })?;
        if stored_credential.is_none()
            && let Some(api_key) = auth.auth.api_key.as_deref()
        {
            model_runtime
                .set_runtime_api_key(&selection.provider, api_key, Some(AuthOperationOptions::default()))
                .await?;
        }
        if sandbox_identity.is_some() {
            pi_js::fs::promises::rm(&auth_path, false, true).await?;
            if let Some(source) = auth.source.as_deref()
                && is_ascii_env_name(source)
                && let Some(val) = pi_js::env::var(source)
            {
                hidden_credential_env = Some((source.to_string(), val));
                pi_js::env::remove_var(source);
            }
        }

        let services = create_agent_session_services(CreateAgentSessionServicesOptions {
            cwd: workspace.clone(),
            model_runtime: Some(model_runtime),
            resource_loader_options: Some(DefaultResourceLoaderOptions {
                extension_factories: Some(extension_factories.clone()),
                ..Default::default()
            }),
            ..Default::default()
        })
        .await?;
        if let Some(sig) = &signal {
            sig.throw_if_aborted()?;
        }

        let sessions_dir = pi_js::path::join(&[&root, "sessions"]);
        let mgr = SessionManager::create(&workspace, Some(&sessions_dir))?;
        ctx.set_artifact("runId", Value::String(mgr.get_session_id()));
        session_manager = Some(mgr.clone());

        let created = create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
            services,
            session_manager: mgr,
            model: Some(model.clone()),
            thinking_level: Some(pi_agent_core::ThinkingLevel::Off),
            tools: options.tools.clone(),
            no_tools: options.no_tools,
            custom_tools: options.custom_tools.clone(),
            ..Default::default()
        })
        .await?;
        let active_session = created.session;
        session = Some(active_session);

        if let Some(active_session) = session.as_ref() {
            let expected_inline: IndexSet<String> = extension_factories
                .iter()
                .filter_map(|ext| match ext {
                    InlineExtension::Named(named) => Some(format!("<inline:{}>", named.name)),
                    InlineExtension::Factory(_) => None,
                })
                .collect();
            let unexpected: Vec<String> = active_session
                .extension_runner()
                .get_extension_paths()
                .into_iter()
                .filter(|p| !expected_inline.contains(p))
                .collect();
            if !unexpected.is_empty() {
                return Err(Error::msg(format!(
                    "Isolated eval loaded unexpected extensions: {}",
                    unexpected.join(", ")
                )));
            }
        }

        enter_tool_sandbox(&root, sandbox_identity).await?;

        let steps = match &input {
            PiCodingAgentInput::Text(content) => vec![PiCodingAgentStep::Prompt {
                content: content.clone(),
            }],
            PiCodingAgentInput::Steps(steps) => steps.clone(),
        };

        let mut last_response: Option<String> = None;
        if let Some(active_session) = session.as_ref() {
            for step in steps {
                if let Some(sig) = &signal {
                    sig.throw_if_aborted()?;
                }
                match step {
                    PiCodingAgentStep::Reload => {
                        if let Some(sig) = &signal {
                            tokio::select! {
                                biased;
                                _ = sig.cancelled() => {
                                    let _ = active_session.abort().await;
                                    return Err(Error::Abort(sig.reason().unwrap_or_else(AbortReason::abort)));
                                }
                                res = active_session.reload() => res?,
                            }
                        } else {
                            active_session.reload().await?;
                        }
                    }
                    PiCodingAgentStep::Prompt { content } => {
                        if let Some(sig) = &signal {
                            let response = tokio::select! {
                                biased;
                                _ = sig.cancelled() => {
                                    let _ = active_session.abort().await;
                                    return Err(Error::Abort(sig.reason().unwrap_or_else(AbortReason::abort)));
                                }
                                res = prompt_agent(active_session, &content, Some(sig)) => res?,
                            };
                            last_response = Some(response);
                        } else {
                            let response = prompt_agent(active_session, &content, None).await?;
                            last_response = Some(response);
                        }
                    }
                }
            }
        }

        let response =
            last_response.ok_or_else(|| Error::msg("Pi eval input must include at least one prompt step."))?;
        let active_session = session
            .as_ref()
            .ok_or_else(|| Error::msg("Pi eval session missing after execution."))?;

        let session_messages = active_session.messages();
        let llm_messages: Vec<pi_ai::Message> = session_messages
            .iter()
            .filter_map(|m| m.as_message().cloned())
            .collect();
        let system_prompt = {
            let forced = forced_system_prompt.lock().unwrap_or_else(|e| e.into_inner()).clone();
            forced.unwrap_or_else(|| get_current_system_prompt(&llm_messages))
        };
        let stats = active_session.get_session_stats();
        let has_pricing = std::iter::once(model.cost.rates())
            .chain(model.cost.tiers.iter().flatten().map(|tier| tier.rates()))
            .any(|rate| rate.input > 0.0 || rate.output > 0.0 || rate.cache_read > 0.0 || rate.cache_write > 0.0);

        let events = to_transcript_events(&session_messages);
        let mut metadata = IndexMap::new();
        metadata.insert(
            "systemPromptSha256".to_string(),
            Value::String(pi_js::crypto::sha256_hex(system_prompt.as_bytes())),
        );
        let mut usage_metadata = IndexMap::new();
        usage_metadata.insert("cacheReadTokens".to_string(), Value::from(stats.tokens.cache_read));
        usage_metadata.insert("cacheWriteTokens".to_string(), Value::from(stats.tokens.cache_write));
        if has_pricing {
            usage_metadata.insert("estimatedCostUsd".to_string(), pi_js::json::number_value(stats.cost));
        }
        let usage = UsageSummary {
            provider: Some(model.provider.as_str().to_string()),
            model: Some(model.id.clone()),
            input_tokens: Some(stats.tokens.input as f64),
            output_tokens: Some(stats.tokens.output as f64),
            reasoning_tokens: None,
            total_tokens: Some(stats.tokens.total as f64),
            tool_calls: Some(stats.tool_calls as f64),
            retries: None,
            metadata: Some(usage_metadata),
        };
        run_diagnostics = Some((events.clone(), metadata, usage.clone()));

        verify_system_prompt(
            &system_prompt,
            options.name.as_deref(),
            options.expected_pi_documentation,
        )?;

        let output = match &options.output {
            Some(extractor) => extractor(active_session, &response, &system_prompt, &agent_dir).await?,
            None => Value::String(response),
        };
        Ok(SimpleHarnessResult {
            output: Some(output),
            events,
            session_provider: Some(model.provider.as_str().to_string()),
            session_model: Some(model.id.clone()),
            session_metadata: None,
            usage,
            timings: Some(TimingSummary {
                total_ms: Some((pi_js::time::performance_now() - started_at).max(0.0)),
                metadata: None,
            }),
            artifacts: None,
            traces: None,
            errors: Vec::new(),
        })
    }
    .await;

    // Cleanup block mirroring TS `finally`
    let mut cleanup_errors: Vec<Error> = Vec::new();
    if let Some(ref mgr) = session_manager {
        match mgr.get_session_file() {
            Some(path) if pi_js::fs::exists(&path) => match pi_js::fs::promises::read_to_string(&path).await {
                Ok(snapshot) => ctx.set_artifact(PI_SESSION_SNAPSHOT_ARTIFACT, Value::String(snapshot)),
                Err(err) => cleanup_errors.push(err),
            },
            _ => cleanup_errors.push(Error::msg("Pi eval produced no session file.")),
        }
    }
    if let Some(active_session) = session.take() {
        active_session.dispose();
    }
    if let Err(err) = pi_js::fs::promises::rm(&root, true, true).await {
        cleanup_errors.push(err);
    }
    isolated_guard.restore();
    if let Some((name, value)) = hidden_credential_env {
        pi_js::env::set_var(&name, &value);
    }

    let failure = match (run_result, cleanup_errors.is_empty()) {
        (Ok(result), true) => return Ok(result),
        (Ok(_), false) => {
            if cleanup_errors.len() == 1 {
                cleanup_errors.remove(0)
            } else {
                let messages: Vec<String> = cleanup_errors.iter().map(ToString::to_string).collect();
                Error::msg(format!("Agent cleanup failed: {}", messages.join("; ")))
            }
        }
        (Err(err), true) => err,
        (Err(err), false) => {
            let messages: Vec<String> = std::iter::once(err.to_string())
                .chain(cleanup_errors.iter().map(ToString::to_string))
                .collect();
            Error::msg(format!(
                "Agent run failed and cleanup also failed: {}",
                messages.join("; ")
            ))
        }
    };

    if let Some((events, metadata, usage)) = run_diagnostics {
        let mut err_map = IndexMap::new();
        err_map.insert("message".to_string(), Value::String(failure.to_string()));
        let partial_artifacts = ctx.artifacts();
        let partial_run = normalize_harness_run(
            SimpleHarnessResult {
                output: None,
                events,
                session_provider: usage.provider.clone(),
                session_model: usage.model.clone(),
                session_metadata: None,
                usage,
                timings: Some(TimingSummary {
                    total_ms: Some((pi_js::time::performance_now() - started_at).max(0.0)),
                    metadata: Some(metadata),
                }),
                artifacts: None,
                traces: None,
                errors: vec![err_map],
            },
            Some(&partial_artifacts),
        );
        return Err(attach_harness_run_to_error(failure, partial_run));
    }

    Err(failure)
}

/// Creates a Pi Coding Agent eval harness from [`PiCodingAgentHarnessOptions`].
pub fn create_pi_coding_agent_harness(options: PiCodingAgentHarnessOptions) -> Harness<PiCodingAgentInput> {
    let name = options.name.clone().unwrap_or_else(|| "pi-coding-agent".to_string());
    create_harness(name, move |input, ctx| {
        let opts = options.clone();
        Box::pin(async move { run_pi_coding_agent(input, ctx.signal.clone(), ctx, opts).await })
    })
}

/// Resolves `"without_docs"` / `"with_docs"` documentation variant from `value` or `PI_EVAL_VARIANT`.
pub fn resolve_documentation_variant(value: Option<&str>) -> Result<DocumentationVariant> {
    let owned = value
        .map(ToString::to_string)
        .or_else(|| pi_js::env::var("PI_EVAL_VARIANT"));
    match owned.as_deref() {
        Some("without_docs") => Ok(DocumentationVariant::WithoutDocs),
        Some("with_docs") => Ok(DocumentationVariant::WithDocs),
        _ => Err(Error::js(
            "TypeError",
            "PI_EVAL_VARIANT must be \"without_docs\" or \"with_docs\".",
        )),
    }
}

/// Strips the `<docs>...</docs>` Pi documentation block from `default_prompt`.
pub fn exclude_pi_documentation(default_prompt: &str) -> Result<String> {
    let start_marker = "\n<docs>\n";
    let end_marker = "\n</docs>";
    let Some(doc_start) = default_prompt.find(start_marker) else {
        return Err(Error::msg("Default Pi system prompt has no Pi documentation section."));
    };
    let Some(doc_end_rel) = default_prompt[doc_start..].find(end_marker) else {
        return Err(Error::msg(
            "Default Pi system prompt has no complete Pi documentation section.",
        ));
    };
    let doc_end = doc_start + doc_end_rel;
    let Some(cwd_start) = default_prompt.rfind("\n<cwd>\n") else {
        return Err(Error::msg("Default Pi system prompt has no working-directory section."));
    };
    if cwd_start < doc_end {
        return Err(Error::msg("Default Pi system prompt has no working-directory section."));
    }
    Ok(format!(
        "{}{}",
        &default_prompt[..doc_start],
        &default_prompt[doc_end + end_marker.len()..]
    ))
}

/// Creates a container-sandboxed documentation-lift eval harness paired to the active `PI_EVAL_VARIANT`.
pub fn create_pi_documentation_eval_harness(
    mut options: PiCodingAgentHarnessOptions,
) -> Result<Harness<PiCodingAgentInput>> {
    if pi_js::env::var("PI_EVAL_CONTAINER").as_deref() != Some("1") || resolve_sandbox_identity()?.is_none() {
        return Err(Error::msg(
            "Documentation evals must run in the isolated container sandbox.",
        ));
    }
    let variant = resolve_documentation_variant(None)?;
    let tools = options
        .tools
        .take()
        .unwrap_or_else(|| DOCUMENTATION_EVAL_TOOLS.iter().map(|s| (*s).to_string()).collect());
    options.name = Some(variant.as_str().to_string());
    options.tools = Some(tools);
    options.transform_system_prompt = match variant {
        DocumentationVariant::WithoutDocs => Some(Arc::new(|prompt: &str| exclude_pi_documentation(prompt))),
        DocumentationVariant::WithDocs => None,
    };
    options.expected_pi_documentation = Some(variant == DocumentationVariant::WithDocs);
    Ok(create_pi_coding_agent_harness(options))
}
