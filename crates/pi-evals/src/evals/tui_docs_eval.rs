//! Port of packages/evals/evals/tui.docs.eval.ts

use std::sync::{Arc, LazyLock, Mutex};

use indexmap::IndexMap;
use pi_agent_core::AgentMessage;
use pi_ai::{AssistantMessage, Message, ModelThinkingLevel, StopReason, Usage, UsageCost, faux_text};
use pi_coding_agent::core::agent_session::AgentSession;
use pi_coding_agent::core::agent_session_runtime::AgentSessionRuntime;
use pi_coding_agent::core::agent_session_services::AgentSessionServices;
use pi_coding_agent::core::sdk::{
    CreateAgentSessionFromServicesOptions, NoToolsMode, create_agent_session_from_services,
};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::settings_manager::FullscreenExitOutput;
use pi_coding_agent::modes::interactive::components::settings_selector::TuiMode;
use pi_coding_agent::pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
use pi_coding_agent::pi_tui::utils::strip_terminal_sequences;
use pi_coding_agent::{InteractiveMode, InteractiveModeOptions};
use pi_js::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::harness::{
    EvalCaseDef, EvalSuite, EvalSuiteKind, Judge, JudgeResult, PiCodingAgentHarnessOptions, PiCodingAgentInput,
    PiCodingAgentStep, create_judge, create_pi_documentation_eval_harness, run_eval_case, serialize_to_json_value,
};

const CONTEXT_WINDOW: i64 = 272_000;

static BUILT_IN_CONTEXT_PATTERN: LazyLock<Option<regex::Regex>> =
    LazyLock::new(|| regex::Regex::new(r"[0-9]+(?:\.[0-9]+)?%/272k(?: \(auto\))?").ok());

static WHITESPACE_COLLAPSE_PATTERN: LazyLock<Option<regex::Regex>> = LazyLock::new(|| regex::Regex::new(r"\s+").ok());

const CONTEXT_FIXTURES: [(f64, &str); 3] = [
    (42.2, "████░░░░░░ 42%"),
    (65.0, "███████░░░ 65%"),
    (120.0, "██████████ 100%"),
];

/// Normalized Levenshtein similarity: 1.0 for identical strings, 0.0 for completely different ones.
pub fn levenshtein_similarity(a: &str, b: &str) -> f64 {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let max_length = a_chars.len().max(b_chars.len());
    if max_length == 0 {
        return 1.0;
    }
    let mut previous: Vec<usize> = (0..=b_chars.len()).collect();
    for i in 1..=a_chars.len() {
        let mut current = vec![0usize; b_chars.len() + 1];
        current[0] = i;
        for j in 1..=b_chars.len() {
            let cost = usize::from(a_chars[i - 1] != b_chars[j - 1]);
            current[j] = (previous[j] + 1).min(current[j - 1] + 1).min(previous[j - 1] + cost);
        }
        previous = current;
    }
    1.0 - (previous[b_chars.len()] as f64 / max_length as f64)
}

/// Virtual terminal recording synchronized TUI frames for offline footer verification.
#[derive(Default)]
pub struct RecordingTerminal {
    writes: Mutex<String>,
    resize_handler: Mutex<Option<TerminalResizeHandler>>,
}

impl RecordingTerminal {
    /// Creates an empty [`RecordingTerminal`] (100x30 with Kitty protocol active).
    pub fn new() -> Self {
        Self::default()
    }

    /// Triggers a simulated terminal resize to force an immediate TUI render pass.
    pub fn request_resize(&self) {
        let handler = self.resize_handler.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(cb) = handler {
            cb();
        }
    }

    /// Drains pending synchronized frames (`\x1b[?2026h ... \x1b[?2026l`) after a brief render settle delay.
    pub async fn take_renders(&self) -> Result<Vec<String>> {
        pi_js::time::sleep(30, None).await?;
        let output = {
            let mut guard = self.writes.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *guard)
        };
        let start_marker = "\x1b[?2026h";
        let end_marker = "\x1b[?2026l";
        let mut renders = Vec::new();
        let mut offset = 0usize;
        loop {
            let Some(rel_start) = output[offset..].find(start_marker) else {
                return Ok(renders);
            };
            let content_start = offset + rel_start + start_marker.len();
            let Some(rel_end) = output[content_start..].find(end_marker) else {
                return Err(Error::msg("TUI emitted an incomplete synchronized render."));
            };
            let content_end = content_start + rel_end;
            renders.push(strip_terminal_sequences(&output[content_start..content_end]));
            offset = content_end + end_marker.len();
        }
    }
}

#[async_trait::async_trait]
impl Terminal for RecordingTerminal {
    fn start(&self, _on_input: TerminalInputHandler, on_resize: TerminalResizeHandler) {
        *self.resize_handler.lock().unwrap_or_else(|e| e.into_inner()) = Some(on_resize);
    }

    fn stop(&self) {
        *self.resize_handler.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}

    fn write(&self, data: &str) {
        self.writes.lock().unwrap_or_else(|e| e.into_inner()).push_str(data);
    }

    fn columns(&self) -> i64 {
        100
    }

    fn rows(&self) -> i64 {
        30
    }

    fn kitty_protocol_active(&self) -> bool {
        true
    }

    fn move_by(&self, _lines: i64) {}
    fn hide_cursor(&self) {}
    fn show_cursor(&self) {}
    fn clear_line(&self) {}
    fn clear_from_cursor(&self) {}
    fn clear_screen(&self) {}
    fn set_title(&self, _title: &str) {}
    fn set_progress(&self, _active: bool) {}
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContextUsageObservation {
    tokens: Option<i64>,
    context_window: i64,
    percent: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TuiFooterObservation {
    percent: f64,
    rendered_output: String,
    context_usage: Option<ContextUsageObservation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TuiFooterOutput {
    built_in_status_line: Option<String>,
    observations: Vec<TuiFooterObservation>,
    extension_errors: Vec<String>,
}

fn non_empty_lines(output: &str) -> Vec<&str> {
    output
        .split(['\r', '\n'])
        .filter(|line| !pi_js::str16::trim(line).is_empty())
        .collect()
}

fn collapse_whitespace(text: &str) -> String {
    let trimmed = pi_js::str16::trim(text);
    match WHITESPACE_COLLAPSE_PATTERN.as_ref() {
        Some(re) => re.replace_all(trimmed, " ").into_owned(),
        None => trimmed.to_string(),
    }
}

fn create_context_message(model: &pi_ai::Model, percent: f64) -> AssistantMessage {
    let tokens = pi_js::num::math_round((model.context_window as f64 * percent) / 100.0) as i64;
    AssistantMessage {
        content: vec![pi_ai::AssistantContentBlock::Text(faux_text("Context usage fixture"))],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            input: tokens,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: tokens,
            cost: UsageCost::default(),
        },
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: pi_js::time::now_ms(),
        duration_ms: None,
    }
}

struct OfflineEnvGuard {
    previous: Option<String>,
}

impl OfflineEnvGuard {
    fn set_offline() -> Self {
        let previous = pi_js::env::var("PI_OFFLINE");
        pi_js::env::set_var("PI_OFFLINE", "1");
        Self { previous }
    }
}

impl Drop for OfflineEnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(val) => pi_js::env::set_var("PI_OFFLINE", val),
            None => pi_js::env::remove_var("PI_OFFLINE"),
        }
    }
}

async fn inspect_context_footer(session: &AgentSession, agent_dir: &str) -> Result<TuiFooterOutput> {
    let active_model = session
        .model()
        .ok_or_else(|| Error::msg("TUI footer oracle requires an active model."))?;
    let mut model = active_model;
    model.context_window = CONTEXT_WINDOW;

    let cwd = session.session_manager().get_cwd();
    let services = AgentSessionServices {
        cwd: cwd.clone(),
        agent_dir: agent_dir.to_string(),
        model_runtime: session.model_runtime().clone(),
        settings_manager: session.settings_manager().clone(),
        resource_loader: session.resource_loader().clone(),
        diagnostics: Vec::new(),
    };
    let oracle_session = create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
        services: services.clone(),
        session_manager: SessionManager::in_memory(Some(&cwd), None, None)?,
        session_start_event: None,
        model: Some(model.clone()),
        thinking_level: Some(ModelThinkingLevel::High),
        scoped_models: None,
        tools: None,
        exclude_tools: None,
        no_tools: Some(NoToolsMode::All),
        custom_tools: None,
    })
    .await?
    .session;

    let _offline_guard = OfflineEnvGuard::set_offline();
    let mut fixture_msg = create_context_message(&model, 0.0);
    fixture_msg.usage = Usage {
        input: 106_000,
        output: 5_800,
        cache_read: 606_000,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 717_800,
        cost: UsageCost {
            input: 1.008,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 1.008,
            total_from_provider: None,
        },
    };
    oracle_session
        .session_manager()
        .append_message(AgentMessage::Message(Message::Assistant(fixture_msg)))?;

    let runtime = Arc::new(AgentSessionRuntime::new(
        oracle_session.clone(),
        services,
        Arc::new(|_| Box::pin(async { Err(Error::msg("The TUI footer oracle does not replace sessions.")) })),
        Vec::new(),
        None,
    ));
    let terminal = Arc::new(RecordingTerminal::new());
    let mode = InteractiveMode::new(
        runtime.clone(),
        InteractiveModeOptions {
            tui_mode: Some(TuiMode::Regular),
            terminal: Some(terminal.clone()),
            ..Default::default()
        },
    );
    let mut built_in_status_line: Option<String> = None;
    let mut observations = Vec::with_capacity(CONTEXT_FIXTURES.len());
    let run_result: Result<()> = async {
        for (index, &(percent, _)) in CONTEXT_FIXTURES.iter().enumerate() {
            oracle_session
                .agent()
                .replace_messages(vec![AgentMessage::Message(Message::Assistant(create_context_message(
                    &model, percent,
                )))]);
            if index == 0 {
                mode.init().await;
            } else {
                terminal.request_resize();
            }
            let renders = terminal.take_renders().await?;
            if index == 0 {
                built_in_status_line = renders
                    .iter()
                    .flat_map(|render| non_empty_lines(render))
                    .find(|line| BUILT_IN_CONTEXT_PATTERN.as_ref().is_some_and(|re| re.is_match(line)))
                    .map(str::to_string);
            }
            let usage = oracle_session.get_context_usage().map(|u| ContextUsageObservation {
                tokens: u.tokens,
                context_window: u.context_window,
                percent: u.percent,
            });
            observations.push(TuiFooterObservation {
                percent,
                rendered_output: renders.last().cloned().unwrap_or_default(),
                context_usage: usage,
            });
        }
        Ok(())
    }
    .await;
    mode.stop(Some(FullscreenExitOutput::ResumeHint));
    runtime.dispose().await;
    run_result?;
    let extension_errors: Vec<String> = session
        .resource_loader()
        .get_extensions()
        .errors
        .into_iter()
        .map(|entry| entry.error)
        .collect();
    Ok(TuiFooterOutput {
        built_in_status_line,
        observations,
        extension_errors,
    })
}

fn context_footer_judge() -> Judge {
    create_judge("ContextFooterJudge", |ctx| {
        Box::pin(async move {
            let Some(output_val) = ctx.output else {
                let mut meta = IndexMap::new();
                meta.insert(
                    "rationale".to_string(),
                    Value::String("Context progress behavior was incorrect.".to_string()),
                );
                meta.insert("retainedPercent".to_string(), Value::from(0));
                return Ok(JudgeResult {
                    score: Some(0.0),
                    metadata: Some(meta),
                });
            };
            let output: TuiFooterOutput = serde_json::from_value(output_val)
                .map_err(|err| Error::msg(format!("Invalid TuiFooterOutput payload: {err}")))?;

            let mut valid = output.extension_errors.is_empty();
            let mut similarities = Vec::new();
            let context_marker = "{context}";
            let built_in_line = output.built_in_status_line.as_deref().map(collapse_whitespace);
            let reference = built_in_line.as_deref().and_then(|line| {
                let re = BUILT_IN_CONTEXT_PATTERN.as_ref()?;
                Some(re.replace(line, context_marker).into_owned())
            });
            if reference.as_deref().is_none_or(|r| !r.contains(context_marker)) {
                valid = false;
            }

            for (index, &(percent, expected_bar)) in CONTEXT_FIXTURES.iter().enumerate() {
                let Some(obs) = output.observations.get(index) else {
                    valid = false;
                    continue;
                };
                if (obs.percent - percent).abs() > f64::EPSILON {
                    valid = false;
                    continue;
                }
                let window_ok = obs
                    .context_usage
                    .as_ref()
                    .is_some_and(|u| u.context_window == CONTEXT_WINDOW);
                let percent_ok = obs
                    .context_usage
                    .as_ref()
                    .and_then(|u| u.percent)
                    .is_some_and(|p| (pi_js::num::math_round(p * 10.0) / 10.0 - percent).abs() < 1e-9);
                if !window_ok || !percent_ok {
                    valid = false;
                }
                let has_built_in = |line: &str| BUILT_IN_CONTEXT_PATTERN.as_ref().is_some_and(|re| re.is_match(line));
                let Some(actual_line) = non_empty_lines(&obs.rendered_output)
                    .into_iter()
                    .find(|line| line.contains(expected_bar) || has_built_in(line))
                else {
                    valid = false;
                    continue;
                };
                let normalized_line = collapse_whitespace(actual_line);
                if !normalized_line.contains(expected_bar) || has_built_in(&normalized_line) {
                    valid = false;
                    continue;
                }
                for &(_, other_bar) in &CONTEXT_FIXTURES {
                    if other_bar != expected_bar && normalized_line.contains(other_bar) {
                        valid = false;
                    }
                }
                if let Some(ref reference_str) = reference {
                    similarities.push(levenshtein_similarity(
                        &normalized_line.replacen(expected_bar, context_marker, 1),
                        reference_str,
                    ));
                }
            }

            let retained = if similarities.len() == CONTEXT_FIXTURES.len() {
                similarities.into_iter().fold(f64::INFINITY, f64::min)
            } else {
                0.0
            };
            let retained_pct = pi_js::num::math_round(retained * 100.0) as i64;
            let mut meta = IndexMap::new();
            meta.insert(
                "rationale".to_string(),
                Value::String(if valid {
                    format!("Footer retained {retained_pct}% of the built-in status text.")
                } else {
                    "Context progress behavior was incorrect.".to_string()
                }),
            );
            meta.insert("retainedPercent".to_string(), Value::from(retained_pct));
            Ok(JudgeResult {
                score: Some(if valid { 1.0 } else { 0.0 }),
                metadata: Some(meta),
            })
        })
    })
}

/// Returns the documentation-lift eval suite for interactive TUI context-footer customization.
pub fn suite() -> EvalSuite {
    let eval_set = "Customize the interactive context footer".to_string();
    let case_id = "replaces numeric context usage with a progress bar".to_string();
    let file = "evals/tui.docs.eval.ts".to_string();
    let eval_set_clone = eval_set.clone();
    let case_id_clone = case_id.clone();

    EvalSuite {
        file: file.clone(),
        kind: EvalSuiteKind::Docs,
        cases: vec![EvalCaseDef {
            eval_set,
            case_id,
            file,
            execute: Arc::new(move |signal| {
                let eval_set = eval_set_clone.clone();
                let case_id = case_id_clone.clone();
                Box::pin(async move {
                    let harness = create_pi_documentation_eval_harness(PiCodingAgentHarnessOptions {
                        output: Some(Arc::new(|session, _response, _system_prompt, agent_dir| {
                            let session = session.clone();
                            let agent_dir = agent_dir.to_string();
                            Box::pin(async move {
                                let out = inspect_context_footer(&session, &agent_dir).await?;
                                Ok(serialize_to_json_value(&out))
                            })
                        })),
                        ..Default::default()
                    })?;

                    let judge = context_footer_judge();
                    let steps = vec![
                        PiCodingAgentStep::Prompt {
                            content: "Change the Pi footer replacing the built-in context info, e.g. `42.2%/272k (auto)`, with a ten-cell progress bar to make it show ████░░░░░░ 42% instead. Clamp percentages to the 0-100 range. Just replace the context. Otherwise leave everything else the same.".to_string(),
                        },
                        PiCodingAgentStep::Reload,
                    ];
                    let input_json = serialize_to_json_value(&steps);
                    run_eval_case::<_, fn(&crate::report::HarnessRun) -> pi_js::Result<()>>(
                        &eval_set,
                        &case_id,
                        &harness,
                        PiCodingAgentInput::Steps(steps),
                        input_json,
                        &[judge],
                        Some(None),
                        signal,
                        None,
                    )
                    .await
                })
            }),
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::providers::faux::faux_provider;
    use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
    use pi_coding_agent::core::resource_loader::DefaultResourceLoaderOptions;
    use pi_coding_agent::core::sdk::{CreateAgentSessionServicesOptions, create_agent_session_services};
    use pi_coding_agent::core::settings_manager::SettingsManager;
    use tempfile::TempDir;

    #[test]
    fn levenshtein_similarity_handles_identical_empty_and_partial_strings() {
        assert_eq!(levenshtein_similarity("", ""), 1.0);
        assert_eq!(levenshtein_similarity("footer", "footer"), 1.0);
        assert_eq!(levenshtein_similarity("abc", "xyz"), 0.0);
        let sim = levenshtein_similarity("a b c", "a x c");
        assert!(sim > 0.5 && sim < 1.0, "sim={sim}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn recording_terminal_captures_synchronized_renders_and_rejects_incomplete() {
        let terminal = RecordingTerminal::new();
        let resized = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let resized_flag = Arc::clone(&resized);
        terminal.start(
            Arc::new(|_| {}),
            Arc::new(move || {
                resized_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            }),
        );
        terminal.request_resize();
        assert!(resized.load(std::sync::atomic::Ordering::SeqCst));

        terminal.write("\x1b[?2026h\x1b[32mhello\x1b[0m\x1b[?2026l");
        let renders = terminal.take_renders().await.unwrap();
        assert_eq!(renders, vec!["hello"]);

        terminal.write("\x1b[?2026hpartial");
        let err = terminal.take_renders().await.unwrap_err();
        assert!(err.to_string().contains("incomplete synchronized render"), "{err}");
        terminal.stop();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inspect_context_footer_captures_built_in_status_line_offline() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let workspace = pi_js::path::join(&[&root, "workspace"]);
        let agent_dir = pi_js::path::join(&[&root, "agent"]);
        pi_js::fs::promises::mkdir(&workspace, true, None).await.unwrap();
        pi_js::fs::promises::mkdir(&agent_dir, true, None).await.unwrap();

        let faux = faux_provider(None);
        let model = faux.get_model();
        let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
            models_path: Some(None),
            auth_path: Some(pi_js::path::join(&[&agent_dir, "auth.json"])),
            models_store_path: Some(pi_js::path::join(&[&agent_dir, "models-store.json"])),
            allow_model_network: Some(false),
            refresh_on_create: Some(false),
            ..Default::default()
        })
        .await
        .unwrap();
        runtime.register_native_provider(faux.provider()).unwrap();
        let settings_manager = SettingsManager::in_memory(None, None);
        let services = create_agent_session_services(CreateAgentSessionServicesOptions {
            cwd: workspace.clone(),
            agent_dir: Some(agent_dir.clone()),
            settings_manager: Some(settings_manager),
            model_runtime: Some(runtime),
            resource_loader_options: Some(DefaultResourceLoaderOptions {
                no_extensions: true,
                no_skills: true,
                no_prompt_templates: true,
                no_themes: true,
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .unwrap();
        let created = create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
            services,
            session_manager: SessionManager::in_memory(Some(&workspace), None, None).unwrap(),
            session_start_event: None,
            model: Some(model),
            thinking_level: Some(ModelThinkingLevel::Off),
            scoped_models: None,
            tools: None,
            exclude_tools: None,
            no_tools: Some(NoToolsMode::All),
            custom_tools: None,
        })
        .await
        .unwrap();

        let output = inspect_context_footer(&created.session, &agent_dir).await.unwrap();
        assert!(output.extension_errors.is_empty());
        assert_eq!(output.observations.len(), CONTEXT_FIXTURES.len());
        assert!(
            output
                .built_in_status_line
                .as_deref()
                .is_some_and(|line| BUILT_IN_CONTEXT_PATTERN.as_ref().is_some_and(|re| re.is_match(line))),
            "built_in_status_line={:?}",
            output.built_in_status_line
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn context_footer_judge_scores_valid_and_invalid_outputs() {
        let judge = context_footer_judge();
        let valid_output = TuiFooterOutput {
            built_in_status_line: Some("↑106k ↓5.8k R606k $1.008 263.9%/272k (auto)".to_string()),
            observations: CONTEXT_FIXTURES
                .iter()
                .map(|&(percent, bar)| TuiFooterObservation {
                    percent,
                    rendered_output: format!("~/workspace\n↑106k ↓5.8k R606k $1.008 {bar}\n"),
                    context_usage: Some(ContextUsageObservation {
                        tokens: Some(100),
                        context_window: CONTEXT_WINDOW,
                        percent: Some(percent),
                    }),
                })
                .collect(),
            extension_errors: Vec::new(),
        };
        let run = crate::report::HarnessRun::default();
        let pass = judge
            .assess(crate::harness::JudgeContext {
                input: Value::Null,
                output: Some(serialize_to_json_value(&valid_output)),
                tool_calls: Vec::new(),
                run: run.clone(),
                session: run.session.clone(),
                signal: None,
            })
            .await
            .unwrap();
        assert_eq!(pass.score, Some(1.0));
        assert_eq!(
            pass.metadata.as_ref().and_then(|m| m.get("retainedPercent")),
            Some(&Value::from(100))
        );

        let mut invalid_output = valid_output;
        invalid_output.extension_errors.push("boom".to_string());
        let fail = judge
            .assess(crate::harness::JudgeContext {
                input: Value::Null,
                output: Some(serialize_to_json_value(&invalid_output)),
                tool_calls: Vec::new(),
                run: run.clone(),
                session: run.session,
                signal: None,
            })
            .await
            .unwrap();
        assert_eq!(fail.score, Some(0.0));
    }
}
