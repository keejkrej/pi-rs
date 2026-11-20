//! Port of packages/evals/src/report.ts plus pure-Rust equivalents of `@vitest-evals/core` report schemas.

use std::cmp::Ordering;
use std::fmt;
use std::io::IsTerminal;

use indexmap::{IndexMap, IndexSet};
use pi_js::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::plan::{DocumentationVariant, EvalTask};

/// Artifact key under which `pi-evals` attaches the serialized JSONL session snapshot.
pub const PI_SESSION_SNAPSHOT_ARTIFACT: &str = "piSessionJsonl";

/// Report workspace schema version (from `@vitest-evals/core`).
pub const REPORT_WORKSPACE_SCHEMA_VERSION: i64 = 1;

// ---------------------------------------------------------------------------
// Pure-Rust `@vitest-evals/core` transcript, trace, run, and report types
// ---------------------------------------------------------------------------

/// Normalized error object captured in a tool result or trace span.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NormalizedError {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Message role in a normalized transcript.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TranscriptRole {
    System,
    User,
    Assistant,
}

/// Event captured in a normalized harness transcript.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptEvent {
    Message {
        role: TranscriptRole,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<IndexMap<String, Value>>,
    },
    ToolCall {
        id: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arguments: Option<IndexMap<String, Value>>,
        #[serde(default, rename = "startedAt", skip_serializing_if = "Option::is_none")]
        started_at: Option<String>,
        #[serde(default, rename = "finishedAt", skip_serializing_if = "Option::is_none")]
        finished_at: Option<String>,
        #[serde(default, rename = "durationMs", skip_serializing_if = "Option::is_none")]
        duration_ms: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<IndexMap<String, Value>>,
    },
    ToolResult {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<NormalizedError>,
        #[serde(default, rename = "startedAt", skip_serializing_if = "Option::is_none")]
        started_at: Option<String>,
        #[serde(default, rename = "finishedAt", skip_serializing_if = "Option::is_none")]
        finished_at: Option<String>,
        #[serde(default, rename = "durationMs", skip_serializing_if = "Option::is_none")]
        duration_ms: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<IndexMap<String, Value>>,
    },
}

/// Tool call observed in a normalized session, joined with its result status when known.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ToolCall {
    Pending {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arguments: Option<IndexMap<String, Value>>,
    },
    Ok {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arguments: Option<IndexMap<String, Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
    },
    Error {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arguments: Option<IndexMap<String, Value>>,
        error: NormalizedError,
    },
}

impl ToolCall {
    /// Returns the called tool name.
    pub fn name(&self) -> &str {
        match self {
            Self::Pending { name, .. } | Self::Ok { name, .. } | Self::Error { name, .. } => name,
        }
    }

    /// Returns the normalized tool arguments, if any.
    pub fn arguments(&self) -> Option<&IndexMap<String, Value>> {
        match self {
            Self::Pending { arguments, .. } | Self::Ok { arguments, .. } | Self::Error { arguments, .. } => {
                arguments.as_ref()
            }
        }
    }

    /// Returns the lifecycle status (`"pending"`, `"ok"`, or `"error"`).
    pub const fn status(&self) -> &'static str {
        match self {
            Self::Pending { .. } => "pending",
            Self::Ok { .. } => "ok",
            Self::Error { .. } => "error",
        }
    }

    /// Returns `true` when the tool call completed successfully.
    pub const fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }

    /// Returns `true` when the tool call resulted in an error.
    pub const fn is_error(&self) -> bool {
        matches!(self, Self::Error { .. })
    }

    /// Returns the result payload if status is `"ok"`.
    pub fn result(&self) -> Option<&Value> {
        match self {
            Self::Ok { result, .. } => result.as_ref(),
            _ => None,
        }
    }

    /// Returns the error payload if status is `"error"`.
    pub fn error(&self) -> Option<&NormalizedError> {
        match self {
            Self::Error { error, .. } => Some(error),
            _ => None,
        }
    }
}

/// Normalized transcript produced by an application harness.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NormalizedSession {
    pub events: Vec<TranscriptEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<IndexMap<String, Value>>,
}

/// Usage summary recorded by a harness run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<IndexMap<String, Value>>,
}

/// Timing summary captured for a harness run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TimingSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<IndexMap<String, Value>>,
}

/// Event attached to a normalized trace span.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NormalizedSpanEvent {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<IndexMap<String, Value>>,
}

/// Coarse operation kind for a normalized trace span.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NormalizedSpanKind {
    Custom,
    Tool,
    Model,
    Run,
    Agent,
    Guardrail,
    Handoff,
}

/// Outcome status of a normalized trace span.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NormalizedSpanStatus {
    Ok,
    Error,
}

/// Normalized operation span captured during a harness run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NormalizedSpan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<NormalizedSpanKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<NormalizedSpanStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<NormalizedError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<IndexMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<NormalizedSpanEvent>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<IndexMap<String, Value>>,
}

/// Normalized execution trace attached to a harness run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NormalizedTrace {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<IndexMap<String, Value>>,
    pub spans: Vec<NormalizedSpan>,
}

/// Normalized harness execution record produced by a single eval case run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessRun {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    pub session: NormalizedSession,
    pub usage: UsageSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timings: Option<TimingSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<IndexMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traces: Option<Vec<NormalizedTrace>>,
    pub errors: Vec<IndexMap<String, Value>>,
}

/// Returns every tool call observed in a normalized session, joined with its first matching tool result.
pub fn tool_calls_in_session(session: &NormalizedSession) -> Vec<ToolCall> {
    let mut results_by_id: IndexMap<&str, &TranscriptEvent> = IndexMap::new();
    for event in &session.events {
        if let TranscriptEvent::ToolResult { tool_call_id, .. } = event
            && !results_by_id.contains_key(tool_call_id.as_str())
        {
            results_by_id.insert(tool_call_id.as_str(), event);
        }
    }

    let mut calls = Vec::new();
    for event in &session.events {
        let TranscriptEvent::ToolCall {
            id, name, arguments, ..
        } = event
        else {
            continue;
        };
        let normalized_args = arguments.clone();
        match results_by_id.get(id.as_str()) {
            None => calls.push(ToolCall::Pending {
                name: name.clone(),
                arguments: normalized_args,
            }),
            Some(TranscriptEvent::ToolResult { error: Some(error), .. }) => calls.push(ToolCall::Error {
                name: name.clone(),
                arguments: normalized_args,
                error: error.clone(),
            }),
            Some(TranscriptEvent::ToolResult { content, .. }) => calls.push(ToolCall::Ok {
                name: name.clone(),
                arguments: normalized_args,
                result: content.clone(),
            }),
            Some(_) => {}
        }
    }
    calls
}

/// Returns every tool call observed in a normalized harness run.
pub fn tool_calls(run: &HarnessRun) -> Vec<ToolCall> {
    tool_calls_in_session(&run.session)
}

/// Status values emitted by Vitest-style JSON reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VitestJsonStatus {
    Pending,
    Passed,
    Failed,
    Skipped,
    Todo,
    Disabled,
}

/// Overall test file pass/fail status in a Vitest JSON report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VitestJsonFileStatus {
    Passed,
    Failed,
}

/// Source location attached to an assertion record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VitestJsonLocation {
    pub line: f64,
    pub column: f64,
}

/// Score record attached inside eval task metadata.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvalScore {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<IndexMap<String, Value>>,
}

/// Eval scoring metadata attached to an assertion.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvalMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scores: Option<Vec<EvalScore>>,
    pub avg_score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold_failed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

/// Harness metadata attached to an assertion.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<HarnessRun>,
}

/// Combined eval and harness metadata extracted from an assertion's `meta` object.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvalTaskMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eval: Option<EvalMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<HarnessMeta>,
}

/// Parses optional `eval` and `harness` metadata from an assertion's `meta` object.
pub fn read_eval_task_meta(input: Option<&Value>) -> Option<EvalTaskMeta> {
    let obj = input?.as_object()?;
    let mut filtered = serde_json::Map::new();
    if let Some(eval) = obj.get("eval") {
        filtered.insert("eval".to_string(), eval.clone());
    }
    if let Some(harness) = obj.get("harness") {
        filtered.insert("harness".to_string(), harness.clone());
    }
    if filtered.is_empty() {
        return None;
    }
    let meta: EvalTaskMeta = serde_json::from_value(Value::Object(filtered)).ok()?;
    if let Some(eval) = &meta.eval {
        if let Some(avg) = eval.avg_score
            && !avg.is_finite()
        {
            return None;
        }
        if let Some(scores) = &eval.scores {
            for entry in scores {
                if let Some(score) = entry.score
                    && !score.is_finite()
                {
                    return None;
                }
            }
        }
    }
    Some(meta)
}

/// Assertion record read from a JSON test report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VitestJsonAssertion {
    #[serde(default)]
    pub ancestor_titles: Vec<String>,
    pub full_name: String,
    pub status: VitestJsonStatus,
    pub title: String,
    #[serde(default)]
    pub meta: Option<Value>,
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default)]
    pub failure_messages: Option<Vec<String>>,
    #[serde(default)]
    pub location: Option<VitestJsonLocation>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

/// Test-file record read from a JSON test report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VitestJsonFile {
    pub message: String,
    pub name: String,
    pub status: VitestJsonFileStatus,
    #[serde(default)]
    pub start_time: Option<f64>,
    #[serde(default)]
    pub end_time: Option<f64>,
    #[serde(default)]
    pub assertion_results: Vec<VitestJsonAssertion>,
}

/// Top-level JSON report payload compatible with `@vitest-evals/core`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VitestJsonReport {
    pub num_failed_tests: f64,
    pub num_passed_tests: f64,
    pub num_pending_tests: f64,
    pub num_todo_tests: f64,
    pub num_total_tests: f64,
    pub start_time: f64,
    pub success: bool,
    #[serde(default)]
    pub test_results: Vec<VitestJsonFile>,
}

/// Run-level aggregate counts in a collected report workspace.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportRunTotals {
    pub total: f64,
    pub passed: f64,
    pub failed: f64,
    pub skipped: f64,
    pub eval_total: f64,
    pub eval_passed: f64,
    pub eval_failed: f64,
}

/// One collected JSON report source in a report workspace.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportRun {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub status: VitestJsonFileStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
    pub totals: ReportRunTotals,
}

/// One eval or harness-backed test case collected from a JSON report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportCase {
    pub id: String,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub file: String,
    pub display_file: String,
    pub title: String,
    pub full_name: String,
    pub ancestor_titles: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    pub display_name: String,
    pub status: VitestJsonStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<VitestJsonLocation>,
    #[serde(default)]
    pub failure_messages: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eval: Option<EvalMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<HarnessMeta>,
}

/// Collected workspace of one or more report runs and cases.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportWorkspace {
    pub schema_version: i64,
    pub runs: Vec<ReportRun>,
    pub cases: Vec<ReportCase>,
}

/// Tagged report entry passed into [`collect_report_workspace`].
pub struct WorkspaceReportInput<'a> {
    pub report: &'a VitestJsonReport,
    pub source: Option<&'a str>,
}

/// Parses and validates a JSON value as a [`VitestJsonReport`].
pub fn parse_vitest_json_report(input: &Value) -> Result<VitestJsonReport> {
    let report: VitestJsonReport = serde_json::from_value(input.clone())
        .map_err(|err| Error::msg(format!("Invalid Vitest JSON report: {err}")))?;
    if !report.num_failed_tests.is_finite()
        || !report.num_passed_tests.is_finite()
        || !report.num_pending_tests.is_finite()
        || !report.num_todo_tests.is_finite()
        || !report.num_total_tests.is_finite()
        || !report.start_time.is_finite()
    {
        return Err(Error::msg("Invalid Vitest JSON report: non-finite summary number."));
    }
    Ok(report)
}

/// Reads and parses a single JSON report file from disk.
pub async fn read_vitest_json_report_file(result_file: &str) -> Result<VitestJsonReport> {
    let raw = pi_js::fs::promises::read_to_string(result_file).await?;
    let value = pi_js::json::parse(&raw)?;
    parse_vitest_json_report(&value)
}

fn normalize_report_path(path: &str, workspace: Option<&str>) -> String {
    let normalized = path.replace('\\', "/");
    let Some(workspace) = workspace else {
        return normalized;
    };
    let workspace_path = workspace.replace('\\', "/");
    let workspace_trimmed = workspace_path.trim_end_matches('/');
    if normalized != workspace_trimmed && !normalized.starts_with(&format!("{workspace_trimmed}/")) {
        return normalized;
    }
    normalized[workspace_trimmed.len()..]
        .trim_start_matches('/')
        .to_string()
}

fn resolve_run_duration(report: &VitestJsonReport) -> Option<f64> {
    let mut min_start: Option<f64> = None;
    let mut max_end: Option<f64> = None;
    for file in &report.test_results {
        let (Some(start), Some(end)) = (file.start_time, file.end_time) else {
            continue;
        };
        if !start.is_finite() || !end.is_finite() || end < start {
            continue;
        }
        min_start = Some(min_start.map_or(start, |cur| cur.min(start)));
        max_end = Some(max_end.map_or(end, |cur| cur.max(end)));
    }
    match (min_start, max_end) {
        (Some(start), Some(end)) => Some(end - start),
        _ => None,
    }
}

/// Collects one or more parsed reports into a [`ReportWorkspace`].
pub fn collect_report_workspace(inputs: &[WorkspaceReportInput<'_>], workspace_root: Option<&str>) -> ReportWorkspace {
    let mut runs = Vec::with_capacity(inputs.len());
    let mut cases = Vec::new();
    for (index, entry) in inputs.iter().enumerate() {
        let report = entry.report;
        let run_id = entry
            .source
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("run-{}", index + 1));
        let mut run_cases = Vec::new();
        for file in &report.test_results {
            for assertion in &file.assertion_results {
                let Some(meta) = read_eval_task_meta(assertion.meta.as_ref()) else {
                    continue;
                };
                let eval_meta = meta.eval.or_else(|| {
                    meta.harness.as_ref().map(|_| EvalMeta {
                        scores: Some(Vec::new()),
                        avg_score: match assertion.status {
                            VitestJsonStatus::Passed => Some(1.0),
                            VitestJsonStatus::Failed => Some(0.0),
                            _ => None,
                        },
                        output: None,
                        threshold_failed: Some(false),
                        tool_calls: None,
                    })
                });
                let line = assertion
                    .location
                    .as_ref()
                    .map(|loc| pi_js::num::to_js_string(loc.line))
                    .unwrap_or_else(|| "0".to_string());
                let id = format!("{run_id}:{}:{line}:{}", file.name, assertion.full_name);
                let mut display_parts: Vec<&str> = assertion
                    .ancestor_titles
                    .iter()
                    .map(String::as_str)
                    .filter(|s| !s.is_empty())
                    .collect();
                if !assertion.title.is_empty() {
                    display_parts.push(assertion.title.as_str());
                }
                let display_name = display_parts.join(" > ");
                run_cases.push(ReportCase {
                    id,
                    run_id: run_id.clone(),
                    source: entry.source.map(ToString::to_string),
                    file: file.name.clone(),
                    display_file: normalize_report_path(&file.name, workspace_root),
                    title: assertion.title.clone(),
                    full_name: assertion.full_name.clone(),
                    ancestor_titles: assertion.ancestor_titles.clone(),
                    tags: assertion.tags.clone(),
                    display_name,
                    status: assertion.status,
                    duration_ms: assertion.duration.filter(|d| d.is_finite()),
                    location: assertion.location.clone(),
                    failure_messages: assertion.failure_messages.clone().unwrap_or_default(),
                    eval: eval_meta,
                    harness: meta.harness,
                });
            }
        }
        let all_passed = report.success && run_cases.iter().all(|c| c.status != VitestJsonStatus::Failed);
        let eval_passed = run_cases
            .iter()
            .filter(|c| c.status == VitestJsonStatus::Passed)
            .count() as f64;
        let eval_failed = run_cases
            .iter()
            .filter(|c| c.status == VitestJsonStatus::Failed)
            .count() as f64;
        runs.push(ReportRun {
            id: run_id,
            source: entry.source.map(ToString::to_string),
            status: if all_passed {
                VitestJsonFileStatus::Passed
            } else {
                VitestJsonFileStatus::Failed
            },
            started_at: Some(report.start_time),
            duration_ms: resolve_run_duration(report),
            totals: ReportRunTotals {
                total: report.num_total_tests,
                passed: report.num_passed_tests,
                failed: report.num_failed_tests,
                skipped: report.num_pending_tests + report.num_todo_tests,
                eval_total: run_cases.len() as f64,
                eval_passed,
                eval_failed,
            },
        });
        cases.extend(run_cases);
    }
    ReportWorkspace {
        schema_version: REPORT_WORKSPACE_SCHEMA_VERSION,
        runs,
        cases,
    }
}

/// Reads report JSON files and collects them into a [`ReportWorkspace`].
pub async fn read_report_workspace(
    report_files: &[&str],
    workspace_root: Option<&str>,
) -> Result<(ReportWorkspace, Vec<String>)> {
    let mut reports = Vec::with_capacity(report_files.len());
    let mut resolved = Vec::with_capacity(report_files.len());
    for &path in report_files {
        let report = read_vitest_json_report_file(path).await?;
        reports.push(report);
        resolved.push(path.to_string());
    }
    let inputs: Vec<WorkspaceReportInput<'_>> = reports
        .iter()
        .zip(resolved.iter())
        .map(|(report, source)| WorkspaceReportInput {
            report,
            source: Some(source.as_str()),
        })
        .collect();
    let workspace = collect_report_workspace(&inputs, workspace_root);
    Ok((workspace, resolved))
}

// ---------------------------------------------------------------------------
// `packages/evals/src/report.ts` domain types & functions
// ---------------------------------------------------------------------------

/// Identity fields common to expected runs and observed eval results.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalRunIdentity {
    pub eval_set: String,
    pub case_id: String,
    pub variant: DocumentationVariant,
    pub model: String,
    pub run_number: i64,
}

/// Expected execution unit in a paired comparison protocol.
pub type ExpectedEvalRun = EvalRunIdentity;

/// Classified outcome for an eval observation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum EvalOutcome {
    Scored { score: f64 },
    Unscored,
    Skipped,
    Pending,
    Errored,
}

/// Optional quantitative metrics collected from a harness run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalMetrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<f64>,
}

/// Observation collected for one executed (or skipped/errored) eval task.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalObservation {
    pub eval_set: String,
    pub case_id: String,
    pub variant: DocumentationVariant,
    pub model: String,
    pub run_number: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<f64>,
    #[serde(flatten)]
    pub outcome: EvalOutcome,
}

impl EvalObservation {
    /// Constructs an observation from an [`EvalRunIdentity`], optional metrics, and outcome.
    pub fn from_identity(identity: EvalRunIdentity, metrics: EvalMetrics, outcome: EvalOutcome) -> Self {
        Self {
            eval_set: identity.eval_set,
            case_id: identity.case_id,
            variant: identity.variant,
            model: identity.model,
            run_number: identity.run_number,
            input_tokens: metrics.input_tokens,
            output_tokens: metrics.output_tokens,
            cache_read_tokens: metrics.cache_read_tokens,
            cache_write_tokens: metrics.cache_write_tokens,
            total_tokens: metrics.total_tokens,
            tool_calls: metrics.tool_calls,
            total_ms: metrics.total_ms,
            estimated_cost_usd: metrics.estimated_cost_usd,
            outcome,
        }
    }

    /// Returns the observation's score when `outcome` is [`EvalOutcome::Scored`].
    pub const fn score(&self) -> Option<f64> {
        match self.outcome {
            EvalOutcome::Scored { score } => Some(score),
            _ => None,
        }
    }
}

/// Summary of a paired continuous metric across control and treatment runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedMetricSummary {
    pub eligible_pairs: i64,
    pub control_mean: Option<f64>,
    pub treatment_mean: Option<f64>,
    pub mean_delta: Option<f64>,
}

/// Diagnostic flags emitted on an [`EvalSetComparison`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ComparisonFlag {
    NoLift,
    NegativeDelta,
    ControlSaturated,
    TreatmentSaturated,
    Flaky,
}

impl ComparisonFlag {
    /// Returns the kebab-case label rendered in formatted reports.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::NoLift => "no-lift",
            Self::NegativeDelta => "negative-delta",
            Self::ControlSaturated => "control-saturated",
            Self::TreatmentSaturated => "treatment-saturated",
            Self::Flaky => "flaky",
        }
    }
}

impl fmt::Display for ComparisonFlag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Paired documentation-lift comparison for one eval set.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalSetComparison {
    pub eval_set: String,
    pub total_pairs: i64,
    pub eligible_pairs: i64,
    pub blocked_pairs: i64,
    pub control_pass_rate: Option<f64>,
    pub treatment_pass_rate: Option<f64>,
    pub lift: Option<f64>,
    pub flags: Vec<ComparisonFlag>,
    pub total_tokens: PairedMetricSummary,
    pub tool_calls: PairedMetricSummary,
    pub total_ms: PairedMetricSummary,
    pub estimated_cost_usd: PairedMetricSummary,
}

/// Reason record explaining why a control/treatment pair could not be scored together.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedPair {
    pub eval_set: String,
    pub case_id: String,
    pub model: String,
    pub run_number: i64,
    pub reasons: Vec<String>,
}

/// Operational metric aggregate across all runs of a single variant.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationalMetricTotal {
    pub available_runs: i64,
    pub total: Option<f64>,
}

/// Operational totals per variant, including errored and incomplete runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VariantTotals {
    pub variant: DocumentationVariant,
    pub runs: i64,
    pub input_tokens: OperationalMetricTotal,
    pub output_tokens: OperationalMetricTotal,
    pub cache_read_tokens: OperationalMetricTotal,
    pub cache_write_tokens: OperationalMetricTotal,
    pub total_tokens: OperationalMetricTotal,
    pub tool_calls: OperationalMetricTotal,
    pub total_ms: OperationalMetricTotal,
    pub estimated_cost_usd: OperationalMetricTotal,
}

/// Final paired documentation-lift comparison report written to `report.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalComparisonReport {
    pub schema_version: i64,
    pub protocol_digest: String,
    pub control: DocumentationVariant,
    pub treatment: DocumentationVariant,
    pub comparisons: Vec<EvalSetComparison>,
    pub blocked_pairs: Vec<BlockedPair>,
    pub operational_totals: Vec<VariantTotals>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaseStatusOutcome {
    Errored,
    Skipped,
    Pending,
}

const fn classify_case_status(status: VitestJsonStatus) -> Option<CaseStatusOutcome> {
    match status {
        VitestJsonStatus::Failed => Some(CaseStatusOutcome::Errored),
        VitestJsonStatus::Skipped | VitestJsonStatus::Todo | VitestJsonStatus::Disabled => {
            Some(CaseStatusOutcome::Skipped)
        }
        VitestJsonStatus::Pending => Some(CaseStatusOutcome::Pending),
        VitestJsonStatus::Passed => None,
    }
}

fn optional_metric_number(value: Option<f64>, name: &str) -> Result<Option<f64>> {
    match value {
        None => Ok(None),
        Some(v) if v.is_finite() && v >= 0.0 => Ok(Some(v)),
        Some(_) => Err(Error::js(
            "TypeError",
            format!("{name} must be a finite non-negative number."),
        )),
    }
}

fn optional_metric_metadata(metadata: Option<&IndexMap<String, Value>>, key: &str) -> Result<Option<f64>> {
    let Some(map) = metadata else {
        return Ok(None);
    };
    let Some(raw) = map.get(key) else {
        return Ok(None);
    };
    let Some(v) = raw.as_f64() else {
        return Err(Error::js(
            "TypeError",
            format!("{key} must be a finite non-negative number."),
        ));
    };
    optional_metric_number(Some(v), key)
}

fn validate_score(value: Option<f64>) -> Result<Option<f64>> {
    match value {
        None => Ok(None),
        Some(v) if v.is_finite() && (0.0..=1.0).contains(&v) => Ok(Some(v)),
        Some(_) => Err(Error::js("TypeError", "Eval score must be between 0 and 1.")),
    }
}

fn task_identity(task: &EvalTask) -> EvalRunIdentity {
    EvalRunIdentity {
        eval_set: task.eval_set.clone(),
        case_id: task.case_id.clone(),
        variant: task.variant,
        model: task.model.clone(),
        run_number: task.run_number,
    }
}

/// Creates an errored [`EvalObservation`] carrying only the task identity.
pub fn errored_observation(task: &EvalTask) -> EvalObservation {
    EvalObservation::from_identity(task_identity(task), EvalMetrics::default(), EvalOutcome::Errored)
}

async fn persist_session(case_result: &ReportCase, task: &EvalTask, artifact_directory: &str) -> Result<()> {
    let Some(artifacts) = case_result
        .harness
        .as_ref()
        .and_then(|h| h.run.as_ref())
        .and_then(|r| r.artifacts.as_ref())
    else {
        return Ok(());
    };
    let Some(session) = artifacts.get(PI_SESSION_SNAPSHOT_ARTIFACT).and_then(Value::as_str) else {
        return Ok(());
    };
    let identity = pi_js::json::try_stringify(&(
        &task.eval_set,
        &task.case_id,
        task.variant,
        &task.model,
        task.run_number,
    ))?;
    let hash = pi_js::crypto::sha256_hex(identity.as_bytes());
    let variant_str = task.variant.as_str();
    let directory = pi_js::path::join(&[artifact_directory, variant_str, "sessions", &hash]);
    pi_js::fs::promises::mkdir(&directory, true, Some(0o700)).await?;
    let session_path = pi_js::path::join(&[&directory, "session.jsonl"]);
    pi_js::fs::promises::write_file(&session_path, session.as_bytes(), Some(0o600)).await?;
    Ok(())
}

/// Reads an executed task's JSON report, persists its session snapshot artifact, and returns its observation.
pub async fn read_task_observation(
    task: &EvalTask,
    report_path: &str,
    artifact_directory: &str,
) -> Result<EvalObservation> {
    let identity = task_identity(task);
    let raw_report = match read_vitest_json_report_file(report_path).await {
        Ok(report) => report,
        Err(_) => return Ok(errored_observation(task)),
    };
    let inputs = [WorkspaceReportInput {
        report: &raw_report,
        source: Some(report_path),
    }];
    let workspace = collect_report_workspace(&inputs, None);
    let reported_full_name = format!("{} {}", task.eval_set, task.case_id);
    let assertions: Vec<&VitestJsonAssertion> = raw_report
        .test_results
        .iter()
        .flat_map(|file| file.assertion_results.iter())
        .filter(|assertion| assertion.full_name == reported_full_name)
        .collect();
    if assertions.len() != 1 {
        return Ok(EvalObservation::from_identity(
            identity,
            EvalMetrics::default(),
            EvalOutcome::Errored,
        ));
    }
    let assertion = assertions[0];
    let status_outcome = classify_case_status(assertion.status);
    match status_outcome {
        Some(CaseStatusOutcome::Skipped) => {
            return Ok(EvalObservation::from_identity(
                identity,
                EvalMetrics::default(),
                EvalOutcome::Skipped,
            ));
        }
        Some(CaseStatusOutcome::Pending) => {
            return Ok(EvalObservation::from_identity(
                identity,
                EvalMetrics::default(),
                EvalOutcome::Pending,
            ));
        }
        Some(CaseStatusOutcome::Errored) | None => {}
    }

    let case_results: Vec<&ReportCase> = workspace
        .cases
        .iter()
        .filter(|case_item| case_item.full_name == reported_full_name)
        .collect();
    if case_results.len() != 1 {
        return Ok(EvalObservation::from_identity(
            identity,
            EvalMetrics::default(),
            EvalOutcome::Errored,
        ));
    }
    let case_result = case_results[0];
    if case_result.status != assertion.status {
        return Ok(EvalObservation::from_identity(
            identity,
            EvalMetrics::default(),
            EvalOutcome::Errored,
        ));
    }
    let Some(run) = case_result.harness.as_ref().and_then(|h| h.run.as_ref()) else {
        return Ok(EvalObservation::from_identity(
            identity,
            EvalMetrics::default(),
            EvalOutcome::Errored,
        ));
    };

    persist_session(case_result, task, artifact_directory).await?;

    let actual_model = match (run.usage.provider.as_deref(), run.usage.model.as_deref()) {
        (Some(provider), Some(model)) if !provider.is_empty() && !model.is_empty() => format!("{provider}/{model}"),
        _ => String::new(),
    };
    if actual_model != task.model {
        return Ok(EvalObservation::from_identity(
            identity,
            EvalMetrics::default(),
            EvalOutcome::Errored,
        ));
    }

    let metrics_result = (|| -> Result<EvalMetrics> {
        let usage_meta = run.usage.metadata.as_ref();
        Ok(EvalMetrics {
            input_tokens: optional_metric_number(run.usage.input_tokens, "inputTokens")?,
            output_tokens: optional_metric_number(run.usage.output_tokens, "outputTokens")?,
            cache_read_tokens: optional_metric_metadata(usage_meta, "cacheReadTokens")?,
            cache_write_tokens: optional_metric_metadata(usage_meta, "cacheWriteTokens")?,
            total_tokens: optional_metric_number(run.usage.total_tokens, "totalTokens")?,
            tool_calls: optional_metric_number(run.usage.tool_calls, "toolCalls")?,
            total_ms: optional_metric_number(run.timings.as_ref().and_then(|t| t.total_ms), "totalMs")?,
            estimated_cost_usd: optional_metric_metadata(usage_meta, "estimatedCostUsd")?,
        })
    })();
    let Ok(metrics) = metrics_result else {
        return Ok(EvalObservation::from_identity(
            identity,
            EvalMetrics::default(),
            EvalOutcome::Errored,
        ));
    };

    if status_outcome == Some(CaseStatusOutcome::Errored) || !run.errors.is_empty() {
        return Ok(EvalObservation::from_identity(identity, metrics, EvalOutcome::Errored));
    }

    let Ok(score) = validate_score(case_result.eval.as_ref().and_then(|eval| eval.avg_score)) else {
        return Ok(EvalObservation::from_identity(identity, metrics, EvalOutcome::Errored));
    };

    match score {
        Some(score) => Ok(EvalObservation::from_identity(
            identity,
            metrics,
            EvalOutcome::Scored { score },
        )),
        None => Ok(EvalObservation::from_identity(identity, metrics, EvalOutcome::Unscored)),
    }
}

const CONTROL: DocumentationVariant = DocumentationVariant::WithoutDocs;
const TREATMENT: DocumentationVariant = DocumentationVariant::WithDocs;
const VARIANTS: [DocumentationVariant; 2] = [CONTROL, TREATMENT];

struct ScoredPair<'a> {
    control: &'a EvalObservation,
    control_score: f64,
    treatment: &'a EvalObservation,
    treatment_score: f64,
}

struct PairGroup<'a> {
    eval_set: String,
    case_id: String,
    model: String,
    run_number: i64,
    expected: IndexMap<DocumentationVariant, usize>,
    observations: IndexMap<DocumentationVariant, Vec<&'a EvalObservation>>,
}

fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f64>() / values.len() as f64)
    }
}

fn difference(treatment: f64, control: f64) -> f64 {
    let raw = pi_js::num::number(&pi_js::num::to_precision(treatment - control, 15));
    if raw == 0.0 { 0.0 } else { raw }
}

fn pair_key(eval_set: &str, case_id: &str, model: &str, run_number: i64) -> String {
    pi_js::json::stringify(&(eval_set, case_id, model, run_number))
}

fn group_pairs<'a>(expected_runs: &[ExpectedEvalRun], observations: &'a [EvalObservation]) -> Vec<PairGroup<'a>> {
    let mut groups: IndexMap<String, PairGroup<'a>> = IndexMap::new();
    for expected in expected_runs {
        let key = pair_key(
            &expected.eval_set,
            &expected.case_id,
            &expected.model,
            expected.run_number,
        );
        let group = groups.entry(key).or_insert_with(|| PairGroup {
            eval_set: expected.eval_set.clone(),
            case_id: expected.case_id.clone(),
            model: expected.model.clone(),
            run_number: expected.run_number,
            expected: IndexMap::new(),
            observations: IndexMap::new(),
        });
        *group.expected.entry(expected.variant).or_insert(0) += 1;
    }
    for observation in observations {
        let key = pair_key(
            &observation.eval_set,
            &observation.case_id,
            &observation.model,
            observation.run_number,
        );
        let group = groups.entry(key).or_insert_with(|| PairGroup {
            eval_set: observation.eval_set.clone(),
            case_id: observation.case_id.clone(),
            model: observation.model.clone(),
            run_number: observation.run_number,
            expected: IndexMap::new(),
            observations: IndexMap::new(),
        });
        group
            .observations
            .entry(observation.variant)
            .or_default()
            .push(observation);
    }
    let mut list: Vec<PairGroup<'a>> = groups.into_values().collect();
    list.sort_by(|left, right| {
        pi_js::intl::locale_compare(&left.eval_set, &right.eval_set)
            .then_with(|| pi_js::intl::locale_compare(&left.case_id, &right.case_id))
            .then_with(|| pi_js::intl::locale_compare(&left.model, &right.model))
            .then_with(|| left.run_number.cmp(&right.run_number))
    });
    list
}

fn outcome_label(outcome: EvalOutcome) -> &'static str {
    match outcome {
        EvalOutcome::Scored { .. } => "scored",
        EvalOutcome::Unscored => "unscored",
        EvalOutcome::Skipped => "skipped",
        EvalOutcome::Pending => "pending",
        EvalOutcome::Errored => "errored",
    }
}

fn resolve_pair<'a>(group: &PairGroup<'a>) -> std::result::Result<ScoredPair<'a>, BlockedPair> {
    let mut reasons = Vec::new();
    for variant in VARIANTS {
        let expected = group.expected.get(&variant).copied().unwrap_or(0);
        let observed = group.observations.get(&variant).map(Vec::as_slice).unwrap_or(&[]);
        if expected != 1 {
            reasons.push(format!("{variant}: design expected 1 run, found {expected}"));
        }
        if observed.len() != expected {
            let suffix = if expected == 1 { "" } else { "s" };
            reasons.push(format!(
                "{variant}: expected {expected} observation{suffix}, found {}",
                observed.len()
            ));
        }
        if expected == 1 && observed.len() == 1 && !matches!(observed[0].outcome, EvalOutcome::Scored { .. }) {
            reasons.push(format!("{variant}: {}", outcome_label(observed[0].outcome)));
        }
    }
    if !reasons.is_empty() {
        return Err(BlockedPair {
            eval_set: group.eval_set.clone(),
            case_id: group.case_id.clone(),
            model: group.model.clone(),
            run_number: group.run_number,
            reasons,
        });
    }
    let control = group.observations.get(&CONTROL).and_then(|v| v.first()).copied();
    let treatment = group.observations.get(&TREATMENT).and_then(|v| v.first()).copied();
    if let (Some(control), Some(treatment)) = (control, treatment)
        && let (EvalOutcome::Scored { score: control_score }, EvalOutcome::Scored { score: treatment_score }) =
            (control.outcome, treatment.outcome)
    {
        return Ok(ScoredPair {
            control,
            control_score,
            treatment,
            treatment_score,
        });
    }
    Err(BlockedPair {
        eval_set: group.eval_set.clone(),
        case_id: group.case_id.clone(),
        model: group.model.clone(),
        run_number: group.run_number,
        reasons: vec!["invalid pair state".to_string()],
    })
}

fn summarize_metric<'a>(
    pairs: &[ScoredPair<'a>],
    select: impl Fn(&EvalObservation) -> Option<f64>,
) -> PairedMetricSummary {
    let mut control = Vec::new();
    let mut treatment = Vec::new();
    for pair in pairs {
        let (Some(control_value), Some(treatment_value)) = (select(pair.control), select(pair.treatment)) else {
            continue;
        };
        control.push(control_value);
        treatment.push(treatment_value);
    }
    let control_mean = mean(&control);
    let treatment_mean = mean(&treatment);
    PairedMetricSummary {
        eligible_pairs: control.len() as i64,
        control_mean,
        treatment_mean,
        mean_delta: match (control_mean, treatment_mean) {
            (Some(c), Some(t)) => Some(difference(t, c)),
            _ => None,
        },
    }
}

fn operational_total(
    runs: &[&EvalObservation],
    select: impl Fn(&EvalObservation) -> Option<f64>,
) -> OperationalMetricTotal {
    let values: Vec<f64> = runs.iter().filter_map(|run| select(run)).collect();
    OperationalMetricTotal {
        available_runs: values.len() as i64,
        total: if values.is_empty() {
            None
        } else {
            Some(values.iter().sum())
        },
    }
}

fn variant_totals(observations: &[EvalObservation], variant: DocumentationVariant) -> VariantTotals {
    let runs: Vec<&EvalObservation> = observations
        .iter()
        .filter(|observation| observation.variant == variant)
        .collect();
    VariantTotals {
        variant,
        runs: runs.len() as i64,
        input_tokens: operational_total(&runs, |obs| obs.input_tokens),
        output_tokens: operational_total(&runs, |obs| obs.output_tokens),
        cache_read_tokens: operational_total(&runs, |obs| obs.cache_read_tokens),
        cache_write_tokens: operational_total(&runs, |obs| obs.cache_write_tokens),
        total_tokens: operational_total(&runs, |obs| obs.total_tokens),
        tool_calls: operational_total(&runs, |obs| obs.tool_calls),
        total_ms: operational_total(&runs, |obs| obs.total_ms),
        estimated_cost_usd: operational_total(&runs, |obs| obs.estimated_cost_usd),
    }
}

fn comparison_flags<'a>(
    pairs: &[ScoredPair<'a>],
    control_pass_rate: Option<f64>,
    treatment_pass_rate: Option<f64>,
) -> Vec<ComparisonFlag> {
    let mut flags = Vec::new();
    if let (Some(c), Some(t)) = (control_pass_rate, treatment_pass_rate) {
        if c == t {
            flags.push(ComparisonFlag::NoLift);
        }
        if t < c {
            flags.push(ComparisonFlag::NegativeDelta);
        }
        if c == 1.0 {
            flags.push(ComparisonFlag::ControlSaturated);
        }
        if t == 1.0 {
            flags.push(ComparisonFlag::TreatmentSaturated);
        }
    }
    let mut outcomes: IndexMap<String, IndexSet<bool>> = IndexMap::new();
    for pair in pairs {
        let control_key = pi_js::json::stringify(&(&pair.control.case_id, pair.control.variant));
        outcomes
            .entry(control_key)
            .or_default()
            .insert(pair.control_score >= 1.0);
        let treatment_key = pi_js::json::stringify(&(&pair.treatment.case_id, pair.treatment.variant));
        outcomes
            .entry(treatment_key)
            .or_default()
            .insert(pair.treatment_score >= 1.0);
    }
    if outcomes.values().any(|values| values.len() > 1) {
        flags.push(ComparisonFlag::Flaky);
    }
    flags
}

/// Aggregates paired control/treatment observations into an [`EvalComparisonReport`].
pub fn summarize_eval_observations(
    protocol_digest: &str,
    expected_runs: &[ExpectedEvalRun],
    observations: &[EvalObservation],
) -> EvalComparisonReport {
    let groups = group_pairs(expected_runs, observations);
    let mut blocked_pairs = Vec::new();
    let mut pairs_by_eval_set: IndexMap<String, Vec<ScoredPair<'_>>> = IndexMap::new();
    let mut totals_by_eval_set: IndexMap<String, i64> = IndexMap::new();

    for group in groups {
        *totals_by_eval_set.entry(group.eval_set.clone()).or_insert(0) += 1;
        match resolve_pair(&group) {
            Ok(pair) => {
                pairs_by_eval_set.entry(group.eval_set).or_default().push(pair);
            }
            Err(blocked) => blocked_pairs.push(blocked),
        }
    }

    let mut eval_sets: Vec<(String, i64)> = totals_by_eval_set.into_iter().collect();
    eval_sets.sort_by(|(left, _), (right, _)| pi_js::intl::locale_compare(left, right));

    let comparisons = eval_sets
        .into_iter()
        .map(|(eval_set, total_pairs)| {
            let pairs = pairs_by_eval_set.get(&eval_set).map(Vec::as_slice).unwrap_or(&[]);
            let eligible_pairs = pairs.len() as i64;
            let blocked_pair_count = total_pairs - eligible_pairs;
            let publish_headline = blocked_pair_count == 0 && !pairs.is_empty();
            let control_pass_rate = if publish_headline {
                Some(pairs.iter().filter(|pair| pair.control_score >= 1.0).count() as f64 / pairs.len() as f64)
            } else {
                None
            };
            let treatment_pass_rate = if publish_headline {
                Some(pairs.iter().filter(|pair| pair.treatment_score >= 1.0).count() as f64 / pairs.len() as f64)
            } else {
                None
            };
            EvalSetComparison {
                eval_set,
                total_pairs,
                eligible_pairs,
                blocked_pairs: blocked_pair_count,
                control_pass_rate,
                treatment_pass_rate,
                lift: match (control_pass_rate, treatment_pass_rate) {
                    (Some(c), Some(t)) => Some(difference(t, c)),
                    _ => None,
                },
                flags: comparison_flags(pairs, control_pass_rate, treatment_pass_rate),
                total_tokens: summarize_metric(pairs, |obs| obs.total_tokens),
                tool_calls: summarize_metric(pairs, |obs| obs.tool_calls),
                total_ms: summarize_metric(pairs, |obs| obs.total_ms),
                estimated_cost_usd: summarize_metric(pairs, |obs| obs.estimated_cost_usd),
            }
        })
        .collect();

    EvalComparisonReport {
        schema_version: 3,
        protocol_digest: protocol_digest.to_string(),
        control: CONTROL,
        treatment: TREATMENT,
        comparisons,
        blocked_pairs,
        operational_totals: vec![
            variant_totals(observations, CONTROL),
            variant_totals(observations, TREATMENT),
        ],
    }
}

fn clean_zero(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}

fn percentage(value: Option<f64>) -> String {
    match value {
        None => "unavailable".to_string(),
        Some(v) => format!("{}%", pi_js::num::to_fixed(clean_zero(v * 100.0), 1)),
    }
}

fn signed(value: f64, digits: u32) -> String {
    let v = clean_zero(value);
    let prefix = if v >= 0.0 { "+" } else { "" };
    format!("{prefix}{}", pi_js::num::to_fixed(v, digits))
}

fn paired_metric(label: &str, metric: &PairedMetricSummary, unit: &str) -> String {
    let (Some(mean_delta), Some(control_mean), Some(treatment_mean)) =
        (metric.mean_delta, metric.control_mean, metric.treatment_mean)
    else {
        return format!("    {label:>10}  unavailable");
    };
    format!(
        "    {label:>10}  {}{unit} (with {}{unit}, without {}{unit}, {} pairs)",
        signed(mean_delta, 1),
        pi_js::num::to_fixed(clean_zero(treatment_mean), 1),
        pi_js::num::to_fixed(clean_zero(control_mean), 1),
        metric.eligible_pairs
    )
}

fn operational_metric(metric: &OperationalMetricTotal, runs: i64, format_total: impl FnOnce(f64) -> String) -> String {
    let Some(total) = metric.total else {
        return format!("unavailable (0/{runs} measured)");
    };
    let coverage = if metric.available_runs == runs {
        String::new()
    } else {
        format!(" ({}/{runs} measured)", metric.available_runs)
    };
    format!("{}{coverage}", format_total(clean_zero(total)))
}

fn style_bold(text: &str) -> String {
    let force = pi_js::env::var("FORCE_COLOR").is_some_and(|v| !v.is_empty() && v != "0");
    let no_color = pi_js::env::var("NO_COLOR").is_some_and(|v| !v.is_empty())
        || pi_js::env::var("NODE_DISABLE_COLORS").is_some_and(|v| !v.is_empty());
    if force || (std::io::stdout().is_terminal() && !no_color) {
        format!("\x1b[1m{text}\x1b[22m")
    } else {
        text.to_string()
    }
}

/// Formats an [`EvalComparisonReport`] as a human-readable CLI summary.
pub fn format_eval_comparison_report(report: &EvalComparisonReport) -> String {
    if report.comparisons.is_empty() {
        return String::new();
    }
    let mut lines = vec![style_bold("Documentation Eval Comparisons")];
    for comparison in &report.comparisons {
        lines.push(format!("  {}", comparison.eval_set));
        lines.push(format!(
            "         Pairs  {}/{} eligible",
            comparison.eligible_pairs, comparison.total_pairs
        ));
        match comparison.lift {
            None => lines.push(if comparison.blocked_pairs > 0 {
                "     Pass rate  withheld because pairs are blocked".to_string()
            } else {
                "     Pass rate  unavailable".to_string()
            }),
            Some(lift) => lines.push(format!(
                "     Pass rate  {} pp (with {}, without {})",
                signed(lift * 100.0, 1),
                percentage(comparison.treatment_pass_rate),
                percentage(comparison.control_pass_rate),
            )),
        }
        if !comparison.flags.is_empty() {
            let joined: Vec<&str> = comparison.flags.iter().map(ComparisonFlag::as_str).collect();
            lines.push(format!("         Flags  {}", joined.join(", ")));
        }
        lines.push(paired_metric("Tokens", &comparison.total_tokens, ""));
        lines.push(paired_metric("Tools", &comparison.tool_calls, ""));
        lines.push(paired_metric("Latency", &comparison.total_ms, "ms"));
        let cost = &comparison.estimated_cost_usd;
        match (cost.mean_delta, cost.control_mean, cost.treatment_mean) {
            (Some(delta), Some(control_mean), Some(treatment_mean)) => {
                let sign = if delta >= 0.0 { "+" } else { "-" };
                lines.push(format!(
                    "     Est. cost  {sign}${} (with ${}, without ${}, {} pairs)",
                    pi_js::num::to_fixed(delta.abs(), 4),
                    pi_js::num::to_fixed(clean_zero(treatment_mean), 4),
                    pi_js::num::to_fixed(clean_zero(control_mean), 4),
                    cost.eligible_pairs,
                ));
            }
            _ => lines.push("     Est. cost  unavailable".to_string()),
        }
    }
    lines.push("  Operational totals".to_string());
    for totals in &report.operational_totals {
        let tokens = operational_metric(&totals.total_tokens, totals.runs, |total| {
            format!("{} tokens", pi_js::num::to_js_string(total))
        });
        let tools = operational_metric(&totals.tool_calls, totals.runs, |total| {
            format!("{} tools", pi_js::num::to_js_string(total))
        });
        let latency = operational_metric(&totals.total_ms, totals.runs, |total| {
            format!("{}s", pi_js::num::to_fixed(total / 1000.0, 2))
        });
        let cost = operational_metric(&totals.estimated_cost_usd, totals.runs, |total| {
            format!("${} cost", pi_js::num::to_fixed(total, 4))
        });
        lines.push(format!(
            "    {}: {} runs, {tokens}, {tools}, {latency}, {cost}",
            totals.variant, totals.runs
        ));
    }
    if !report.blocked_pairs.is_empty() {
        lines.push("  Blocked pairs".to_string());
        for blocked in &report.blocked_pairs {
            lines.push(format!(
                "    {}/{}/{}/run-{}: {}",
                blocked.eval_set,
                blocked.case_id,
                blocked.model,
                blocked.run_number,
                blocked.reasons.join("; ")
            ));
        }
    }
    lines.join("\n")
}

/// Ensures [`Ordering`] compatibility helper is exercised cleanly.
#[allow(dead_code)]
fn _cmp_f64(a: f64, b: f64) -> Ordering {
    a.total_cmp(&b)
}
