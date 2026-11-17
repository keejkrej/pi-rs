//! Bug report collection, redaction, archiving, and LLM summary generation.
//!
//! Port of `packages/coding-agent/src/core/bug-report.ts`.

use std::sync::{Arc, LazyLock};

use indexmap::IndexMap;
use pi_agent_core::{AgentMessage, StreamFn, ThinkingLevel};
use pi_ai::{
    AssistantContentBlock, Context, Message, Model, ProviderEnv, ProviderHeaders, RetryPolicy, SimpleStreamOptions,
    StopReason, UserMessage, UserMessageContent, content_text, uuidv7,
};
use pi_js::abort::AbortSignal;
use pi_js::error::{Error, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use url::Url;

use crate::config::VERSION;
use crate::core::compaction::compaction::{complete_summarization, estimate_tokens, get_summarization_failure};
use crate::core::compaction::utils::serialize_conversation;
use crate::core::crash_log::{CrashKind, CrashRecord};
use crate::core::messages::convert_to_llm;
use crate::core::model_registry::ModelRegistry;
use crate::core::model_runtime::ModelRuntime;
use crate::core::provider_composer::AuthStatusSource;
use crate::core::session_manager::{SessionEntry, SessionManager};
use crate::core::settings_manager::Settings;
use crate::utils::pi_user_agent::get_pi_user_agent;
use crate::utils::zip::{ZipEntry, write_zip_archive};

pub const BUG_REPORT_CUSTOM_ENTRY_TYPE: &str = "pi.bug-report";
const BUG_REPORT_SCHEMA_VERSION: u32 = 1;
const REDACTED: &str = "<redacted>";

static NESTED_URL_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^([a-z][a-z0-9+.-]*:)([a-z][a-z0-9+.-]*://.*)$").expect("valid nested URL regex")
});

static CAMEL_SPLIT_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([a-z0-9])([A-Z])").expect("valid camelCase regex"));

static SENSITIVE_KEY_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[-_])(api[-_]?key|secret|token|password|passwd|credential|authorization|cookie)(?:$|[-_])")
        .expect("valid sensitive key regex")
});

fn is_sensitive_key(key: &str) -> bool {
    let normalized = CAMEL_SPLIT_REGEX.replace_all(key, "${1}_${2}");
    SENSITIVE_KEY_REGEX.is_match(&normalized)
}

/// Strip credentials and secret-looking query parameters from a URL.
pub fn redact_url(value: &str) -> String {
    if let Some(caps) = NESTED_URL_REGEX.captures(value)
        && let (Some(prefix), Some(inner)) = (caps.get(1), caps.get(2))
    {
        return format!("{}{}", prefix.as_str(), redact_url(inner.as_str()));
    }

    let Ok(mut parsed) = Url::parse(value) else {
        return value.to_owned();
    };

    let mut changed = false;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        let _ = parsed.set_username("");
        let _ = parsed.set_password(None);
        changed = true;
    }

    if let Some(existing_query) = parsed.query().map(ToOwned::to_owned) {
        let pairs: Vec<(String, String)> = url::form_urlencoded::parse(existing_query.as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let mut query_changed = false;
        let updated: Vec<(String, String)> = pairs
            .into_iter()
            .map(|(k, v)| {
                if is_sensitive_key(&k) {
                    query_changed = true;
                    (k, REDACTED.to_owned())
                } else {
                    (k, v)
                }
            })
            .collect();
        if query_changed {
            let mut ser = url::form_urlencoded::Serializer::new(String::new());
            for (k, v) in &updated {
                ser.append_pair(k, v);
            }
            parsed.set_query(Some(&ser.finish()));
            changed = true;
        }
    }

    if changed { parsed.to_string() } else { value.to_owned() }
}

/// Copy a JSON value while removing values that may contain credentials.
pub fn redact_json_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::with_capacity(map.len());
            for (k, child) in map {
                if !child.is_null() && is_sensitive_key(k) {
                    out.insert(k.clone(), Value::String(REDACTED.to_owned()));
                } else {
                    out.insert(k.clone(), redact_json_value(child));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(redact_json_value).collect()),
        Value::String(s) => Value::String(redact_url(s)),
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
    }
}

fn redact_settings(settings: &Settings) -> Value {
    let raw_json = pi_js::json::stringify(settings);
    let mut parsed = pi_js::json::parse(&raw_json).unwrap_or(Value::Object(Map::new()));
    if let Value::Object(ref mut map) = parsed {
        map.shift_remove("trackingId");
        map.shift_remove("deviceId");
    }
    redact_json_value(&parsed)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BugReportTerminalInfo {
    pub term: Option<String>,
    pub program: Option<String>,
    pub program_version: Option<String>,
    pub colorterm: Option<String>,
    pub tmux: bool,
    pub ssh: bool,
    pub ci: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BugReportEnvironment {
    pub version: String,
    pub user_agent: String,
    pub runtime: String,
    pub platform: String,
    pub arch: String,
    pub os_release: String,
    pub os_version: String,
    pub shell: Option<String>,
    pub terminal: BugReportTerminalInfo,
    pub pi_environment_variables: Vec<String>,
}

fn collect_environment() -> BugReportEnvironment {
    let env_opt = |name: &str| pi_js::env::var(name).filter(|s| !s.is_empty());
    let shell = env_opt("SHELL").and_then(|s| {
        s.rsplit(['/', '\\'])
            .next()
            .filter(|p| !p.is_empty())
            .map(ToOwned::to_owned)
    });
    let mut pi_vars: Vec<String> = pi_js::env::vars()
        .keys()
        .filter(|name| name.starts_with("PI_"))
        .cloned()
        .collect();
    pi_vars.sort_by(|a, b| pi_js::str16::cmp(a, b));

    BugReportEnvironment {
        version: (*VERSION).to_owned(),
        user_agent: get_pi_user_agent(*VERSION),
        runtime: format!("rust/{}", *VERSION),
        platform: pi_js::env::platform().to_owned(),
        arch: pi_js::env::arch().to_owned(),
        os_release: pi_js::env::os_release(),
        os_version: pi_js::env::os_version(),
        shell,
        terminal: BugReportTerminalInfo {
            term: env_opt("TERM"),
            program: env_opt("TERM_PROGRAM"),
            program_version: env_opt("TERM_PROGRAM_VERSION"),
            colorterm: env_opt("COLORTERM"),
            tmux: env_opt("TMUX").is_some(),
            ssh: env_opt("SSH_CONNECTION").is_some() || env_opt("SSH_CLIENT").is_some() || env_opt("SSH_TTY").is_some(),
            ci: env_opt("CI").is_some(),
        },
        pi_environment_variables: pi_vars,
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BugReportModelDescription {
    pub provider: String,
    pub id: String,
    pub name: String,
    pub api: String,
    pub base_url: String,
    pub reasoning: bool,
    pub input: Vec<pi_ai::ModelInput>,
    pub context_window: i64,
    pub max_tokens: i64,
    pub sampling_params: Option<Value>,
    pub compat: Option<Value>,
    pub thinking_level_map: Option<pi_ai::ThinkingLevelMap>,
    pub header_names: Vec<String>,
}

fn describe_model(model: &Model) -> BugReportModelDescription {
    let mut header_names: Vec<String> = model
        .headers
        .as_ref()
        .map(|h| h.keys().cloned().collect())
        .unwrap_or_default();
    header_names.sort_by(|a, b| pi_js::str16::cmp(a, b));

    let sampling_params = model.sampling_params.as_ref().map(|sp| {
        let json = pi_js::json::stringify(sp);
        pi_js::json::parse(&json)
            .map(|v| redact_json_value(&v))
            .unwrap_or(Value::Null)
    });
    let compat = model.compat.as_ref().map(|c| {
        let json = pi_js::json::stringify(c);
        pi_js::json::parse(&json)
            .map(|v| redact_json_value(&v))
            .unwrap_or(Value::Null)
    });

    BugReportModelDescription {
        provider: model.provider.clone(),
        id: model.id.clone(),
        name: model.name.clone(),
        api: model.api.clone(),
        base_url: redact_url(&model.base_url),
        reasoning: model.reasoning,
        input: model.input.clone(),
        context_window: model.context_window,
        max_tokens: model.max_tokens,
        sampling_params,
        compat,
        thinking_level_map: model.thinking_level_map.clone(),
        header_names,
    }
}

/// Redacted provider descriptor supplied from `ModelRuntime` or tests.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BugReportProviderInfo {
    pub id: String,
    pub name: String,
    pub base_url: Option<String>,
    pub header_names: Vec<String>,
    pub auth_types: Vec<String>,
    pub auth_status: String,
    pub using_oauth: bool,
    pub registered_by_extension: bool,
}

/// Callback source returning provider info for a provider ID.
pub trait BugReportProviderSource: Send + Sync {
    fn get_provider_info(&self, provider_id: &str) -> Option<BugReportProviderInfo>;
}

impl BugReportProviderInfo {
    /// Build a redacted provider descriptor from [`ModelRuntime`].
    pub fn from_model_runtime(model_runtime: &ModelRuntime, provider_id: &str) -> Option<Self> {
        let provider = model_runtime.get_provider(provider_id)?;
        let mut header_names: Vec<String> = provider
            .headers()
            .map(|h| h.keys().cloned().collect())
            .unwrap_or_default();
        header_names.sort_by(|a, b| pi_js::str16::cmp(a, b));
        let auth = provider.auth();
        let mut auth_types = Vec::new();
        if auth.api_key.is_some() {
            auth_types.push("api_key".to_owned());
        }
        if auth.oauth.is_some() {
            auth_types.push("oauth".to_owned());
        }
        let status = model_runtime.get_provider_auth_status(provider_id);
        let auth_status = if !status.configured {
            "unconfigured".to_owned()
        } else {
            match status.source {
                Some(AuthStatusSource::Stored) => "stored".to_owned(),
                Some(AuthStatusSource::Runtime) => "runtime".to_owned(),
                Some(AuthStatusSource::Environment) => "environment".to_owned(),
                Some(AuthStatusSource::Fallback) => "fallback".to_owned(),
                Some(AuthStatusSource::ModelsJsonKey) => "models_json_key".to_owned(),
                Some(AuthStatusSource::ModelsJsonCommand) => "models_json_command".to_owned(),
                None => "configured".to_owned(),
            }
        };
        let registered_by_extension = model_runtime
            .get_registered_provider_ids()
            .iter()
            .any(|id| id == provider.id());
        Some(describe_provider(&Self {
            id: provider.id().to_owned(),
            name: provider.name().to_owned(),
            base_url: provider.base_url().map(ToOwned::to_owned),
            header_names,
            auth_types,
            auth_status,
            using_oauth: model_runtime.is_using_oauth(provider.id()),
            registered_by_extension,
        }))
    }
}

impl BugReportProviderSource for ModelRuntime {
    fn get_provider_info(&self, provider_id: &str) -> Option<BugReportProviderInfo> {
        BugReportProviderInfo::from_model_runtime(self, provider_id)
    }
}

impl BugReportProviderSource for ModelRegistry {
    fn get_provider_info(&self, provider_id: &str) -> Option<BugReportProviderInfo> {
        BugReportProviderInfo::from_model_runtime(self.runtime(), provider_id)
    }
}

fn describe_provider(info: &BugReportProviderInfo) -> BugReportProviderInfo {
    let mut header_names = info.header_names.clone();
    header_names.sort_by(|a, b| pi_js::str16::cmp(a, b));
    BugReportProviderInfo {
        id: info.id.clone(),
        name: info.name.clone(),
        base_url: info.base_url.as_deref().map(redact_url),
        header_names,
        auth_types: info.auth_types.clone(),
        auth_status: info.auth_status.clone(),
        using_oauth: info.using_oauth,
        registered_by_extension: info.registered_by_extension,
    }
}

/// Summary descriptor for one loaded extension in a bug report.
// WIRE(@codemode-w3): convert from `crate::core::extensions::types::Extension` when extensions land.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BugReportExtensionInput {
    pub path: String,
    pub source: String,
    pub scope: String,
    pub origin: String,
    pub hidden: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BugReportExtensionDescription {
    pub path: String,
    pub source: String,
    pub scope: String,
    pub origin: String,
    pub hidden: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BugReportExtensionError {
    pub path: String,
    pub error: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BugReportSessionMetadata {
    pub id: String,
    pub included: bool,
    pub summary_included: bool,
    pub message_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BugReportSettingsMetadata {
    pub global: Value,
    pub project: Value,
}

/// Complete sanitized metadata recorded in `report.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BugReportMetadata {
    pub schema_version: u32,
    pub id: String,
    pub created_at: String,
    pub hint: Option<String>,
    pub environment: BugReportEnvironment,
    pub session: BugReportSessionMetadata,
    pub model: Option<BugReportModelDescription>,
    pub provider: Option<BugReportProviderInfo>,
    pub thinking_level: ThinkingLevel,
    pub extensions: Vec<BugReportExtensionDescription>,
    pub extension_errors: Vec<BugReportExtensionError>,
    pub settings: BugReportSettingsMetadata,
}

/// Inputs for [`collect_bug_report_metadata`].
#[derive(Clone)]
pub struct CollectBugReportMetadataOptions {
    pub id: Option<String>,
    pub hint: Option<String>,
    pub session_id: String,
    pub cwd: String,
    pub include_session: bool,
    pub include_summary: bool,
    pub message_count: usize,
    pub model: Option<Model>,
    /// Pre-fetched provider info (`modelRuntime` adapter).
    pub provider_info: Option<BugReportProviderInfo>,
    /// Optional lazy provider lookup source.
    pub provider_source: Option<Arc<dyn BugReportProviderSource>>,
    pub thinking_level: ThinkingLevel,
    pub extensions: Vec<BugReportExtensionInput>,
    pub extension_errors: Vec<BugReportExtensionError>,
    pub global_settings: Settings,
    pub project_settings: Settings,
}

/// Collect sanitized bug report metadata.
pub fn collect_bug_report_metadata(options: CollectBugReportMetadataOptions) -> BugReportMetadata {
    let hint = options
        .hint
        .as_deref()
        .map(pi_js::str16::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    let provider_info = options.provider_info.or_else(|| {
        let model = options.model.as_ref()?;
        let source = options.provider_source.as_ref()?;
        source.get_provider_info(&model.provider)
    });

    BugReportMetadata {
        schema_version: BUG_REPORT_SCHEMA_VERSION,
        id: options.id.unwrap_or_else(|| uuidv7(None).unwrap_or_default()),
        created_at: pi_js::time::iso_now(),
        hint,
        environment: collect_environment(),
        session: BugReportSessionMetadata {
            id: options.session_id,
            included: options.include_session,
            summary_included: options.include_summary,
            message_count: options.message_count,
            cwd: if options.include_session {
                Some(options.cwd)
            } else {
                None
            },
        },
        model: options.model.as_ref().map(describe_model),
        provider: provider_info.as_ref().map(describe_provider),
        thinking_level: options.thinking_level,
        extensions: options
            .extensions
            .into_iter()
            .map(|ext| BugReportExtensionDescription {
                path: ext.path,
                source: redact_url(&ext.source),
                scope: ext.scope,
                origin: ext.origin,
                hidden: ext.hidden,
            })
            .collect(),
        extension_errors: options.extension_errors,
        settings: BugReportSettingsMetadata {
            global: redact_settings(&options.global_settings),
            project: redact_settings(&options.project_settings),
        },
    }
}

/// Sanitized assistant failure entry captured in `diagnostics.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BugReportAssistantDiagnosticEntry {
    pub entry_id: String,
    pub timestamp: String,
    pub provider: String,
    pub model: String,
    pub api: String,
    pub stop_reason: StopReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub diagnostics: Vec<pi_ai::AssistantMessageDiagnostic>,
}

/// Redacted crash record included in `diagnostics.json` (omits `notified`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BugReportCrashEntry {
    pub timestamp: String,
    pub version: String,
    pub kind: CrashKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_file: Option<String>,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<IndexMap<String, Value>>,
}

/// Diagnostics report persisted in `diagnostics.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BugReportDiagnostics {
    pub schema_version: u32,
    pub session_id: String,
    pub entry_count: usize,
    pub assistant_message_count: usize,
    pub assistant: Vec<BugReportAssistantDiagnosticEntry>,
    pub crashes: Vec<BugReportCrashEntry>,
}

/// Collect failed assistant turns without collecting conversation content.
pub fn collect_bug_report_diagnostics(
    session_manager: &SessionManager,
    crashes: &[CrashRecord],
) -> BugReportDiagnostics {
    let entries = session_manager.get_entries();
    let mut assistant = Vec::new();
    let mut assistant_message_count = 0usize;

    for entry in &entries {
        let SessionEntry::Message(msg_entry) = entry else {
            continue;
        };
        let AgentMessage::Message(Message::Assistant(message)) = &msg_entry.message else {
            continue;
        };
        assistant_message_count += 1;

        let diagnostics = message.diagnostics.clone().unwrap_or_default();
        if diagnostics.is_empty()
            && message.stop_reason != StopReason::Error
            && message.stop_reason != StopReason::Aborted
            && message.error_message.as_deref().is_none_or(str::is_empty)
        {
            continue;
        }

        assistant.push(BugReportAssistantDiagnosticEntry {
            entry_id: msg_entry.id.clone(),
            timestamp: msg_entry.timestamp.clone(),
            provider: message.provider.clone(),
            model: message.model.clone(),
            api: message.api.clone(),
            stop_reason: message.stop_reason,
            raw_stop_reason: message.raw_stop_reason.clone(),
            error_message: message.error_message.clone(),
            diagnostics,
        });
    }

    let crash_entries = crashes
        .iter()
        .map(|record| BugReportCrashEntry {
            timestamp: record.timestamp.clone(),
            version: record.version.clone(),
            kind: record.kind.clone(),
            message: record.message.clone(),
            stack: record.stack.clone(),
            session_file: record.session_file.clone(),
            cwd: record.cwd.clone(),
            extra: None,
        })
        .collect();

    BugReportDiagnostics {
        schema_version: BUG_REPORT_SCHEMA_VERSION,
        session_id: session_manager.get_session_id(),
        entry_count: entries.len(),
        assistant_message_count,
        assistant,
        crashes: crash_entries,
    }
}

/// Complete bug report payload written to zip archives or uploaded via HTTP.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BugReportBundle {
    pub metadata: BugReportMetadata,
    pub diagnostics: BugReportDiagnostics,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_jsonl: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Delivery mode for a persisted custom bug-report session entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BugReportDelivery {
    Zip,
    Upload,
}

/// Custom entry payload recorded in sessions for bug reports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BugReportSessionEntryData {
    pub id: String,
    pub created_at: String,
    pub hint: Option<String>,
    pub session_included: bool,
    pub summary_included: bool,
    pub delivery: BugReportDelivery,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// File part shared between archive export and multipart HTTP upload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BugReportFile {
    pub name: String,
    pub content_type: String,
    pub data: String,
}

/// Materialize individual files shared by upload and zip export.
pub fn bug_report_files(bundle: &BugReportBundle) -> Vec<BugReportFile> {
    let mut files = vec![
        BugReportFile {
            name: "report.json".to_owned(),
            content_type: "application/json".to_owned(),
            data: format!("{}\n", pi_js::json::stringify_pretty(&bundle.metadata, "  ")),
        },
        BugReportFile {
            name: "diagnostics.json".to_owned(),
            content_type: "application/json".to_owned(),
            data: format!("{}\n", pi_js::json::stringify_pretty(&bundle.diagnostics, "  ")),
        },
    ];

    if let Some(session_jsonl) = &bundle.session_jsonl {
        files.push(BugReportFile {
            name: "session.jsonl".to_owned(),
            content_type: "application/x-ndjson".to_owned(),
            data: session_jsonl.clone(),
        });
    }
    if let Some(summary) = &bundle.summary {
        let normalized = if summary.ends_with('\n') {
            summary.clone()
        } else {
            format!("{summary}\n")
        };
        files.push(BugReportFile {
            name: "summary.md".to_owned(),
            content_type: "text/markdown".to_owned(),
            data: normalized,
        });
    }

    files
}

/// Write a bug report bundle to a zip archive on disk.
pub async fn write_bug_report_archive(bundle: &BugReportBundle, file_path: &str) -> Result<()> {
    let entries: Vec<ZipEntry> = bug_report_files(bundle)
        .into_iter()
        .map(|f| ZipEntry::from_str(f.name, &f.data))
        .collect();
    write_zip_archive(file_path, &entries).await
}

/// Default zip archive filename for a bug report ID.
pub fn bug_report_archive_file_name(id: &str) -> String {
    format!("pi-bug-report-{id}.zip")
}

const BUG_SUMMARY_SYSTEM_PROMPT: &str = "You are helping a user file a bug report about pi, the coding agent they are talking to. You will be shown the conversation transcript. Write a report for the pi developers describing what the user was doing and what went wrong.

Do NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the report.";

const BUG_SUMMARY_INSTRUCTIONS: &str = "Write the bug report in Markdown with these sections:

## What the user was doing
One short paragraph.

## What went wrong
Concrete description of the failure: wrong output, errors, hangs, tool failures, unexpected behavior. Quote error messages and tool output verbatim where they exist.

## Steps to reproduce
Numbered list, as specific as the transcript allows.

## Relevant details
Tool calls involved, files touched, model behavior, anything else that helps a developer reproduce or locate the problem.

Do not include file contents, secrets, or credentials from the transcript; refer to files by path only. Keep the report factual and concise.";

fn select_messages(messages: &[AgentMessage], token_budget: i64) -> Vec<AgentMessage> {
    let mut selected = Vec::new();
    let mut tokens = 0i64;
    for msg in messages.iter().rev() {
        let next = estimate_tokens(msg);
        if !selected.is_empty() && tokens + next > token_budget {
            break;
        }
        selected.push(msg.clone());
        tokens += next;
    }
    selected.reverse();
    selected
}

/// Inputs for [`generate_bug_report_summary`].
#[derive(Clone)]
pub struct GenerateBugReportSummaryOptions {
    pub messages: Vec<AgentMessage>,
    pub hint: Option<String>,
    pub model: Model,
    pub api_key: Option<String>,
    pub headers: Option<ProviderHeaders>,
    pub env: Option<ProviderEnv>,
    pub signal: Option<AbortSignal>,
    pub thinking_level: Option<ThinkingLevel>,
    pub stream_fn: Option<StreamFn>,
    pub retry: Option<RetryPolicy>,
    pub session_id: Option<String>,
}

/// Ask the session model for a report when the user does not share the transcript.
pub async fn generate_bug_report_summary(options: GenerateBugReportSummaryOptions) -> Result<String> {
    let context_window = if options.model.context_window > 0 {
        options.model.context_window
    } else {
        128_000
    };
    let budget = ((context_window as f64) * 0.6).floor() as i64;
    let selected = select_messages(&options.messages, budget);
    let hint = options
        .hint
        .as_deref()
        .map(pi_js::str16::trim)
        .filter(|s| !s.is_empty());

    let mut prompt_parts = Vec::new();
    if selected.len() < options.messages.len() {
        prompt_parts.push(format!(
            "Note: only the last {} of {} messages are shown.",
            selected.len(),
            options.messages.len()
        ));
    }
    prompt_parts.push(format!(
        "<conversation>\n{}\n</conversation>",
        serialize_conversation(&convert_to_llm(&selected))
    ));
    if let Some(h) = hint {
        prompt_parts.push(format!("<user-report>\n{h}\n</user-report>"));
    }
    prompt_parts.push(BUG_SUMMARY_INSTRUCTIONS.to_owned());
    let prompt = prompt_parts.join("\n\n");

    let max_tokens = if options.model.max_tokens > 0 {
        4096.min(options.model.max_tokens)
    } else {
        4096
    };

    let mut request_options = SimpleStreamOptions::default();
    request_options.base.max_tokens = Some(max_tokens);
    request_options.base.signal = options.signal;
    request_options.base.api_key = options.api_key;
    request_options.base.headers = options.headers;
    request_options.base.env = options.env;
    request_options.base.session_id = options.session_id;
    if options.model.reasoning
        && let Some(level) = options.thinking_level
    {
        request_options.reasoning = level.to_ai_thinking_level();
    }

    let context = Context {
        system_prompt: Some(BUG_SUMMARY_SYSTEM_PROMPT.to_owned()),
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text(prompt),
            timestamp: pi_js::time::now_ms(),
        })],
        tools: None,
    };

    let response = complete_summarization(
        &options.model,
        &context,
        request_options,
        options.stream_fn.as_ref(),
        options.retry.as_ref(),
        None,
    )
    .await?;

    if response.stop_reason == StopReason::Aborted {
        return Err(Error::msg("Bug report summary was cancelled"));
    }
    if let Some(failure) = get_summarization_failure(&response, "Bug report summary") {
        return Err(Error::msg(failure));
    }
    if response
        .content
        .iter()
        .any(|block| matches!(block, AssistantContentBlock::ToolCall(_)))
    {
        return Err(Error::msg("Bug report summary attempted to call a tool"));
    }

    let raw_text = content_text(&response.content, None);
    let trimmed = pi_js::str16::trim(&raw_text);
    if trimmed.is_empty() {
        return Err(Error::msg("Bug report summary was empty"));
    }

    Ok(trimmed.to_owned())
}
