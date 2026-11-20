//! Port of packages/coding-agent/test/program-status-reporter.test.ts

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_agent_core::AgentMessage;
use pi_ai::{AssistantContentBlock, AssistantMessage, Message, StopReason, TextContent, Usage};
use pi_coding_agent::modes::interactive::program_status_reporter::{
    BlockedKind, BlockedStatus, ProgramStatus, ProgramStatusReporter, ProgramStatusState,
};
use pi_coding_agent::modes::json_event::{AgentSessionEvent, CompactionReason};
use pi_tui::Terminal;
use pi_tui::terminal::{TerminalInputHandler, TerminalResizeHandler};
use pretty_assertions::assert_eq;

struct RecordingTerminal {
    reports: Arc<Mutex<Vec<ProgramStatus>>>,
}

#[async_trait]
impl Terminal for RecordingTerminal {
    fn start(&self, _on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {}
    fn stop(&self) {}
    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
    fn write(&self, _data: &str) {}
    fn columns(&self) -> i64 {
        80
    }
    fn rows(&self) -> i64 {
        24
    }
    fn kitty_protocol_active(&self) -> bool {
        false
    }
    fn move_by(&self, _lines: i64) {}
    fn hide_cursor(&self) {}
    fn show_cursor(&self) {}
    fn clear_line(&self) {}
    fn clear_from_cursor(&self) {}
    fn clear_screen(&self) {}
    fn set_title(&self, _title: &str) {}
    fn set_progress(&self, _active: bool) {}
    fn set_program_status(&self, status: ProgramStatus) {
        self.reports.lock().unwrap().push(status);
    }
}

struct ReporterHarness {
    reporter: ProgramStatusReporter,
    reports: Arc<Mutex<Vec<ProgramStatus>>>,
    session_name: Arc<Mutex<Option<String>>>,
}

impl ReporterHarness {
    fn new(name: Option<&str>) -> Self {
        let reports = Arc::new(Mutex::new(Vec::new()));
        let terminal: Arc<dyn Terminal> = Arc::new(RecordingTerminal {
            reports: Arc::clone(&reports),
        });
        let session_name = Arc::new(Mutex::new(name.map(str::to_owned)));
        let name_ref = Arc::clone(&session_name);
        let reporter = ProgramStatusReporter::new(
            Arc::new(move || Arc::clone(&terminal)),
            Arc::new(move || name_ref.lock().unwrap().clone()),
        );
        Self {
            reporter,
            reports,
            session_name,
        }
    }

    fn send(&self, events: &[AgentSessionEvent]) {
        for event in events {
            self.reporter.handle_event(event);
        }
    }

    fn last(&self) -> ProgramStatus {
        self.reports.lock().unwrap().last().cloned().expect("status reported")
    }
}

fn assistant_end(stop_reason: StopReason, error_message: Option<&str>) -> AgentSessionEvent {
    let msg = AssistantMessage {
        content: vec![AssistantContentBlock::Text(TextContent::new("secret assistant output"))],
        api: "anthropic-messages".to_owned(),
        provider: "anthropic".to_owned(),
        model: "claude".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason,
        deferred: None,
        error_message: error_message.map(str::to_owned),
        raw_stop_reason: None,
        end_turn: None,
        duration_ms: None,
        timestamp: 0,
    };
    AgentSessionEvent::MessageEnd {
        message: AgentMessage::Message(Message::Assistant(msg)),
    }
}

const SETTLED: AgentSessionEvent = AgentSessionEvent::AgentSettled { aborted: false };

fn compaction_end(reason: CompactionReason, aborted: bool, error_message: Option<&str>) -> AgentSessionEvent {
    AgentSessionEvent::CompactionEnd {
        reason,
        result: None,
        aborted,
        will_retry: false,
        error_message: error_message.map(str::to_owned),
    }
}

fn status(state: ProgramStatusState, kind: Option<BlockedKind>, message: Option<&str>) -> ProgramStatus {
    ProgramStatus {
        state,
        app: Some("pi".to_owned()),
        kind,
        message: message.map(str::to_owned),
    }
}

#[test]
fn reports_idle_working_during_run_and_done_once_settled() {
    let h = ReporterHarness::new(Some("Fix login"));
    h.reporter.report();
    assert_eq!(h.last(), status(ProgramStatusState::Idle, None, None));

    h.send(&[AgentSessionEvent::AgentStart]);
    assert_eq!(h.last(), status(ProgramStatusState::Working, None, Some("Fix login")));

    h.send(&[
        assistant_end(StopReason::ToolUse, None),
        assistant_end(StopReason::Stop, None),
    ]);
    assert_eq!(h.last(), status(ProgramStatusState::Working, None, Some("Fix login")));

    h.send(&[SETTLED]);
    assert_eq!(h.last(), status(ProgramStatusState::Done, None, Some("Fix login")));
    let serialized = pi_js::json::stringify(&*h.reports.lock().unwrap());
    assert!(!serialized.contains("secret assistant output"));
}

#[test]
fn reports_only_the_outcome_of_the_run_retried_errors_final_errors_and_aborts() {
    let h = ReporterHarness::new(None);
    h.send(&[
        AgentSessionEvent::AgentStart,
        assistant_end(StopReason::Error, Some("overloaded")),
        assistant_end(StopReason::Stop, None),
        SETTLED,
    ]);
    assert_eq!(h.last(), status(ProgramStatusState::Done, None, None));

    h.send(&[
        AgentSessionEvent::AgentStart,
        assistant_end(StopReason::Error, Some("Invalid API key\n{details}")),
        SETTLED,
    ]);
    assert_eq!(
        h.last(),
        status(ProgramStatusState::Error, None, Some("Invalid API key"))
    );

    h.send(&[
        AgentSessionEvent::AgentStart,
        assistant_end(StopReason::Aborted, None),
        AgentSessionEvent::AgentSettled { aborted: true },
    ]);
    assert_eq!(h.last(), status(ProgramStatusState::Idle, None, None));

    h.send(&[
        AgentSessionEvent::AgentStart,
        assistant_end(StopReason::Stop, None),
        AgentSessionEvent::AgentSettled { aborted: true },
    ]);
    assert_eq!(h.last(), status(ProgramStatusState::Idle, None, None));
}

#[test]
fn reports_a_failed_recovery_compaction_as_the_runs_error_unless_a_later_response_succeeds() {
    let h = ReporterHarness::new(None);
    h.send(&[
        AgentSessionEvent::AgentStart,
        assistant_end(StopReason::Length, None),
        AgentSessionEvent::CompactionStart {
            reason: CompactionReason::Overflow,
        },
        compaction_end(CompactionReason::Overflow, false, Some("Compaction failed\nstack")),
        SETTLED,
    ]);
    assert_eq!(
        h.last(),
        status(ProgramStatusState::Error, None, Some("Compaction failed"))
    );

    h.send(&[
        AgentSessionEvent::AgentStart,
        AgentSessionEvent::CompactionStart {
            reason: CompactionReason::Threshold,
        },
        compaction_end(CompactionReason::Threshold, false, Some("Compaction failed")),
        assistant_end(StopReason::Stop, None),
        SETTLED,
    ]);
    assert_eq!(h.last(), status(ProgramStatusState::Done, None, None));
}

#[test]
fn reports_compaction_inside_run_and_result_of_manual_compaction() {
    let h = ReporterHarness::new(Some("Session"));
    h.send(&[
        AgentSessionEvent::AgentStart,
        AgentSessionEvent::CompactionStart {
            reason: CompactionReason::Threshold,
        },
    ]);
    assert_eq!(
        h.last(),
        status(ProgramStatusState::Working, None, Some("Compacting context"))
    );
    h.send(&[compaction_end(CompactionReason::Threshold, false, None)]);
    assert_eq!(h.last(), status(ProgramStatusState::Working, None, Some("Session")));
    h.send(&[assistant_end(StopReason::Stop, None), SETTLED]);

    h.send(&[
        AgentSessionEvent::CompactionStart {
            reason: CompactionReason::Manual,
        },
        compaction_end(CompactionReason::Manual, false, None),
    ]);
    assert_eq!(h.last(), status(ProgramStatusState::Done, None, Some("Session")));

    h.send(&[
        AgentSessionEvent::CompactionStart {
            reason: CompactionReason::Manual,
        },
        compaction_end(CompactionReason::Manual, false, Some("No model")),
    ]);
    assert_eq!(h.last(), status(ProgramStatusState::Error, None, Some("No model")));

    h.send(&[
        AgentSessionEvent::CompactionStart {
            reason: CompactionReason::Manual,
        },
        compaction_end(CompactionReason::Manual, true, None),
    ]);
    assert_eq!(h.last(), status(ProgramStatusState::Idle, None, None));
}

#[test]
fn reports_the_most_recent_open_dialog_and_underlying_state_once_all_close() {
    let h = ReporterHarness::new(None);
    h.send(&[AgentSessionEvent::AgentStart]);
    h.reporter.set_blocked(
        "extension-selector",
        Some(BlockedStatus {
            kind: BlockedKind::Permission,
            message: "Allow bash?".to_owned(),
        }),
    );
    h.reporter.set_blocked(
        "login",
        Some(BlockedStatus {
            kind: BlockedKind::Auth,
            message: "Log in to Anthropic".to_owned(),
        }),
    );
    assert_eq!(
        h.last(),
        status(
            ProgramStatusState::Blocked,
            Some(BlockedKind::Auth),
            Some("Log in to Anthropic")
        )
    );

    h.reporter.set_blocked("login", None);
    h.send(&[assistant_end(StopReason::Stop, None), SETTLED]);
    assert_eq!(
        h.last(),
        status(
            ProgramStatusState::Blocked,
            Some(BlockedKind::Permission),
            Some("Allow bash?")
        )
    );

    h.reporter.set_blocked(
        "extension-selector",
        Some(BlockedStatus {
            kind: BlockedKind::Question,
            message: "Pick one".to_owned(),
        }),
    );
    assert_eq!(
        h.last(),
        status(
            ProgramStatusState::Blocked,
            Some(BlockedKind::Question),
            Some("Pick one")
        )
    );
    h.reporter.set_blocked("extension-selector", None);
    assert_eq!(h.last(), status(ProgramStatusState::Done, None, None));
}

#[test]
fn sends_each_status_once_and_follows_session_name_changes() {
    let h = ReporterHarness::new(Some("Old"));
    h.send(&[
        AgentSessionEvent::AgentStart,
        AgentSessionEvent::TurnStart,
        assistant_end(StopReason::ToolUse, None),
    ]);
    h.reporter.report();
    assert_eq!(h.reports.lock().unwrap().len(), 1);

    *h.session_name.lock().unwrap() = Some("New".to_owned());
    h.send(&[AgentSessionEvent::SessionInfoChanged {
        name: Some("New".to_owned()),
    }]);
    assert_eq!(h.last(), status(ProgramStatusState::Working, None, Some("New")));
}

#[test]
fn returns_to_idle_when_session_is_replaced() {
    let h = ReporterHarness::new(None);
    h.send(&[
        AgentSessionEvent::AgentStart,
        assistant_end(StopReason::Stop, None),
        SETTLED,
    ]);
    h.reporter.reset();
    assert_eq!(h.last(), status(ProgramStatusState::Idle, None, None));
}
