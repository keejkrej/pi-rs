//! Port of packages/coding-agent/src/modes/interactive/program-status-reporter.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_agent_core::AgentMessage;
use pi_tui::Terminal;
pub use pi_tui::program_status::{ProgramStatus, ProgramStatusKind, ProgramStatusState, format_program_status};

use crate::config::APP_NAME;
use crate::modes::json_event::{AgentSessionEvent, CompactionReason};

pub type BlockedKind = ProgramStatusKind;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockedStatus {
    pub kind: BlockedKind,
    pub message: String,
}

fn first_line_or_error(text: Option<&str>) -> String {
    text.and_then(|t| t.split(['\r', '\n']).next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Error")
        .to_string()
}

struct ProgramStatusStateInner {
    run_active: bool,
    compacting: bool,
    run_result: ProgramStatus,
    resting_status: ProgramStatus,
    blocked: IndexMap<String, BlockedStatus>,
    last_report_key: Option<String>,
    last_status: Option<ProgramStatus>,
}

/// Reports interactive-mode state to the terminal (OSC 7501).
#[derive(Clone)]
pub struct ProgramStatusReporter {
    get_terminal: Arc<dyn Fn() -> Arc<dyn Terminal> + Send + Sync>,
    get_session_name: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    state: Arc<Mutex<ProgramStatusStateInner>>,
}

impl ProgramStatusReporter {
    pub fn new(
        get_terminal: Arc<dyn Fn() -> Arc<dyn Terminal> + Send + Sync>,
        get_session_name: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    ) -> Self {
        Self {
            get_terminal,
            get_session_name,
            state: Arc::new(Mutex::new(ProgramStatusStateInner {
                run_active: false,
                compacting: false,
                run_result: ProgramStatus {
                    state: ProgramStatusState::Done,
                    app: None,
                    kind: None,
                    message: None,
                },
                resting_status: ProgramStatus {
                    state: ProgramStatusState::Idle,
                    app: None,
                    kind: None,
                    message: None,
                },
                blocked: IndexMap::new(),
                last_report_key: None,
                last_status: None,
            })),
        }
    }

    pub fn handle_event(&self, event: &AgentSessionEvent) {
        let should_report = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            match event {
                AgentSessionEvent::AgentStart => {
                    st.run_active = true;
                    st.run_result = ProgramStatus {
                        state: ProgramStatusState::Done,
                        app: None,
                        kind: None,
                        message: None,
                    };
                    true
                }
                AgentSessionEvent::MessageEnd { message } => {
                    if let AgentMessage::Message(pi_ai::Message::Assistant(assistant)) = message {
                        st.run_result = if assistant.stop_reason == pi_ai::StopReason::Error {
                            ProgramStatus {
                                state: ProgramStatusState::Error,
                                app: None,
                                kind: None,
                                message: Some(first_line_or_error(assistant.error_message.as_deref())),
                            }
                        } else {
                            ProgramStatus {
                                state: ProgramStatusState::Done,
                                app: None,
                                kind: None,
                                message: None,
                            }
                        };
                        true
                    } else {
                        false
                    }
                }
                AgentSessionEvent::CompactionStart { .. } => {
                    st.compacting = true;
                    true
                }
                AgentSessionEvent::CompactionEnd {
                    reason,
                    aborted,
                    error_message,
                    ..
                } => {
                    st.compacting = false;
                    if st.run_active {
                        if *aborted {
                            st.run_result = ProgramStatus {
                                state: ProgramStatusState::Idle,
                                app: None,
                                kind: None,
                                message: None,
                            };
                        } else if let Some(err) = error_message {
                            st.run_result = ProgramStatus {
                                state: ProgramStatusState::Error,
                                app: None,
                                kind: None,
                                message: Some(first_line_or_error(Some(err))),
                            };
                        }
                    } else if *aborted {
                        st.resting_status = ProgramStatus {
                            state: ProgramStatusState::Idle,
                            app: None,
                            kind: None,
                            message: None,
                        };
                    } else if *reason == CompactionReason::Manual {
                        st.resting_status = if let Some(err) = error_message {
                            ProgramStatus {
                                state: ProgramStatusState::Error,
                                app: None,
                                kind: None,
                                message: Some(first_line_or_error(Some(err))),
                            }
                        } else {
                            ProgramStatus {
                                state: ProgramStatusState::Done,
                                app: None,
                                kind: None,
                                message: None,
                            }
                        };
                    }
                    true
                }
                AgentSessionEvent::AgentSettled { aborted } => {
                    st.run_active = false;
                    st.resting_status = if *aborted {
                        ProgramStatus {
                            state: ProgramStatusState::Idle,
                            app: None,
                            kind: None,
                            message: None,
                        }
                    } else {
                        st.run_result.clone()
                    };
                    true
                }
                AgentSessionEvent::SessionInfoChanged { .. } => true,
                _ => false,
            }
        };
        if should_report {
            self.report();
        }
    }

    /// Report `blocked` for a dialog until it is cleared with `None`. Reopening a source replaces it.
    pub fn set_blocked(&self, source: &str, status: Option<BlockedStatus>) {
        {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.blocked.shift_remove(source);
            if let Some(status) = status {
                st.blocked.insert(source.to_string(), status);
            }
        }
        self.report();
    }

    /// Forget the previous session's run, for example after switching sessions.
    pub fn reset(&self) {
        {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.run_active = false;
            st.compacting = false;
            st.run_result = ProgramStatus {
                state: ProgramStatusState::Done,
                app: None,
                kind: None,
                message: None,
            };
            st.resting_status = ProgramStatus {
                state: ProgramStatusState::Idle,
                app: None,
                kind: None,
                message: None,
            };
        }
        self.report();
    }

    pub fn last_status(&self) -> Option<ProgramStatus> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).last_status.clone()
    }

    pub fn report(&self) {
        let session_name = (self.get_session_name)();
        let status_opt = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let mut status = if let Some(blocked) = st.blocked.values().last() {
                ProgramStatus {
                    state: ProgramStatusState::Blocked,
                    app: Some((*APP_NAME).to_string()),
                    kind: Some(blocked.kind),
                    message: Some(blocked.message.clone()),
                }
            } else if st.compacting {
                ProgramStatus {
                    state: ProgramStatusState::Working,
                    app: Some((*APP_NAME).to_string()),
                    kind: None,
                    message: Some("Compacting context".to_string()),
                }
            } else if st.run_active {
                ProgramStatus {
                    state: ProgramStatusState::Working,
                    app: Some((*APP_NAME).to_string()),
                    kind: None,
                    message: session_name,
                }
            } else {
                let mut resting = st.resting_status.clone();
                resting.app = Some((*APP_NAME).to_string());
                if resting.state == ProgramStatusState::Done {
                    resting.message = session_name;
                }
                resting
            };
            if status.app.is_none() {
                status.app = Some((*APP_NAME).to_string());
            }
            let key = pi_js::json::stringify(&status);
            if st.last_report_key.as_deref() == Some(key.as_str()) {
                None
            } else {
                st.last_report_key = Some(key);
                st.last_status = Some(status.clone());
                Some(status)
            }
        };
        if let Some(status) = status_opt {
            (self.get_terminal)().set_program_status(status);
        }
    }
}
