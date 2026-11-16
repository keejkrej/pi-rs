//! Port of packages/ai/test/retry.test.ts (unit ai-utils-errors)

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_ai::providers::faux::{FauxAssistantMessageContent, FauxAssistantMessageOptions, faux_assistant_message};
use pi_ai::types::{AssistantContentBlock, StopReason, TextContent};
use pi_ai::utils::retry::{
    RetryCallbacks, RetryPolicy, is_retryable_assistant_error, retry_assistant_call, retry_delay_ms,
};
use pi_js::abort::AbortController;

fn err_msg(msg: &str) -> pi_ai::types::AssistantMessage {
    faux_assistant_message(
        FauxAssistantMessageContent::Text(String::new()),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some(msg.to_string()),
            ..Default::default()
        }),
    )
}

fn aborted_msg() -> pi_ai::types::AssistantMessage {
    faux_assistant_message(
        FauxAssistantMessageContent::Text(String::new()),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Aborted),
            ..Default::default()
        }),
    )
}

#[derive(Default)]
struct RecordingCallbacks {
    events: Mutex<Vec<String>>,
    scheduled_delays: Mutex<Vec<u64>>,
    finished: Mutex<Vec<(bool, u32, Option<String>)>>,
}

#[async_trait::async_trait]
impl RetryCallbacks for RecordingCallbacks {
    async fn on_retry_scheduled(&self, attempt: u32, _max_attempts: u32, delay_ms: u64, _error_message: &str) {
        self.events.lock().unwrap().push(format!("retry:{attempt}"));
        self.scheduled_delays.lock().unwrap().push(delay_ms);
    }

    async fn on_retry_attempt_start(&self) {
        self.events.lock().unwrap().push("attempt-start".to_string());
    }

    async fn on_retry_finished(&self, success: bool, attempt: u32, final_error_message: Option<&str>) {
        self.finished
            .lock()
            .unwrap()
            .push((success, attempt, final_error_message.map(str::to_string)));
    }
}

#[test]
fn provider_retry_classification_and_delay_caps() {
    let retryable = [
        "An error occurred while processing your request. You can retry your request, or contact us through our help center at help.openai.com if the error persists. Please include the request ID req_******** in your message.",
        r#"{"message":"The system encountered an unexpected error during processing. Try your request again."}"#,
        "ResourceExhausted: Worker local total request limit reached (288/48)",
        "The socket connection was closed unexpectedly. For more information, pass `verbose: true` in the second argument to fetch()",
        "Error: exceeded request buffer limit while retrying upstream",
        "The pending stream has been canceled (caused by: getaddrinfo ENOTFOUND bedrock-runtime.us-east-1.amazonaws.com)",
        "connect ENOTFOUND api.example.com",
        "EAI_AGAIN api.example.com",
        "getaddrinfo failed for api.example.com",
        "OpenAI Responses stream ended before a terminal response event",
        "The system is currently experiencing high demand and cannot process your request. Your request exceeds the maximum usage size allowed during peak load. For improved capacity reliability, consider switching to Provisioned Throughput.",
        "subscription_sharing_usage_unavailable: Usage cannot be checked.",
        "subscription_sharing_user_unavailable: User cannot be loaded.",
        "overloaded_error",
        "520 status code (no body)",
        "524 status code (no body)",
    ];
    for msg in retryable {
        assert!(is_retryable_assistant_error(&err_msg(msg)), "expected retryable: {msg}");
    }

    let non_retryable = [
        "429 quota exceeded",
        r#"OpenAI API error (429): {"code":"subscription_sharing_usage_limit_exceeded","message":"Usage limit reached."}"#,
    ];
    for msg in non_retryable {
        assert!(
            !is_retryable_assistant_error(&err_msg(msg)),
            "expected non-retryable: {msg}"
        );
    }
    assert!(!is_retryable_assistant_error(&faux_assistant_message(
        FauxAssistantMessageContent::Text("not an error".to_string()),
        None
    )));

    assert_eq!(
        retry_delay_ms(
            &RetryPolicy {
                enabled: true,
                max_retries: 10,
                base_delay_ms: 2000,
                max_agent_delay_ms: None,
            },
            6
        ),
        60000
    );
    assert_eq!(
        retry_delay_ms(
            &RetryPolicy {
                enabled: true,
                max_retries: 10,
                base_delay_ms: 2000,
                max_agent_delay_ms: Some(5000),
            },
            5
        ),
        5000
    );
    assert_eq!(
        retry_delay_ms(
            &RetryPolicy {
                enabled: true,
                max_retries: 10,
                base_delay_ms: 2000,
                max_agent_delay_ms: Some(0),
            },
            5
        ),
        0
    );
}

#[tokio::test]
async fn retry_assistant_call_handles_success_abort_non_retryable_transient_and_caps() {
    let enabled = RetryPolicy {
        enabled: true,
        max_retries: 3,
        base_delay_ms: 0,
        max_agent_delay_ms: None,
    };
    let disabled = RetryPolicy {
        enabled: false,
        max_retries: 3,
        base_delay_ms: 0,
        max_agent_delay_ms: None,
    };

    // Immediate success
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);
    let res = retry_assistant_call(
        move || {
            c.fetch_add(1, Ordering::SeqCst);
            async {
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("ok".to_string()),
                    None,
                ))
            }
        },
        Some(&enabled),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        res.content,
        vec![AssistantContentBlock::Text(TextContent {
            text: "ok".to_string(),
            text_signature: None,
        })]
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);

    // Aborted message is not retried
    let c_ab = Arc::new(AtomicUsize::new(0));
    let cb_ab = RecordingCallbacks::default();
    let res_ab = retry_assistant_call(
        {
            let c = Arc::clone(&c_ab);
            move || {
                c.fetch_add(1, Ordering::SeqCst);
                async { Ok(aborted_msg()) }
            }
        },
        Some(&enabled),
        None,
        Some(&cb_ab),
    )
    .await
    .unwrap();
    assert_eq!(res_ab.stop_reason, StopReason::Aborted);
    assert_eq!(c_ab.load(Ordering::SeqCst), 1);
    assert!(cb_ab.scheduled_delays.lock().unwrap().is_empty());

    // Non-retryable quota error
    let c_nr = Arc::new(AtomicUsize::new(0));
    let cb_nr = RecordingCallbacks::default();
    let res_nr = retry_assistant_call(
        {
            let c = Arc::clone(&c_nr);
            move || {
                c.fetch_add(1, Ordering::SeqCst);
                async { Ok(err_msg("insufficient_quota")) }
            }
        },
        Some(&enabled),
        None,
        Some(&cb_nr),
    )
    .await
    .unwrap();
    assert_eq!(res_nr.stop_reason, StopReason::Error);
    assert_eq!(c_nr.load(Ordering::SeqCst), 1);
    assert!(cb_nr.finished.lock().unwrap().is_empty());

    // Transient exhaustion
    let c_tr = Arc::new(AtomicUsize::new(0));
    let cb_tr = RecordingCallbacks::default();
    let res_tr = retry_assistant_call(
        {
            let c = Arc::clone(&c_tr);
            move || {
                c.fetch_add(1, Ordering::SeqCst);
                async { Ok(err_msg("terminated")) }
            }
        },
        Some(&enabled),
        None,
        Some(&cb_tr),
    )
    .await
    .unwrap();
    assert_eq!(res_tr.stop_reason, StopReason::Error);
    assert_eq!(c_tr.load(Ordering::SeqCst), 4);
    assert_eq!(cb_tr.scheduled_delays.lock().unwrap().len(), 3);
    assert_eq!(
        *cb_tr.finished.lock().unwrap(),
        vec![(false, 3, Some("terminated".to_string()))]
    );

    // Capped retry delays & ordering of callbacks
    let c_cap = Arc::new(AtomicUsize::new(0));
    let cb_cap = RecordingCallbacks::default();
    let cap_policy = RetryPolicy {
        enabled: true,
        max_retries: 4,
        base_delay_ms: 10,
        max_agent_delay_ms: Some(15),
    };
    let res_cap = retry_assistant_call(
        {
            let c = Arc::clone(&c_cap);
            move || {
                let n = c.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n < 4 {
                        Ok(err_msg("terminated"))
                    } else {
                        Ok(faux_assistant_message(
                            FauxAssistantMessageContent::Text("recovered".to_string()),
                            None,
                        ))
                    }
                }
            }
        },
        Some(&cap_policy),
        None,
        Some(&cb_cap),
    )
    .await
    .unwrap();
    assert_eq!(res_cap.stop_reason, StopReason::Stop);
    assert_eq!(*cb_cap.scheduled_delays.lock().unwrap(), vec![10, 15, 15, 15]);
    assert_eq!(*cb_cap.finished.lock().unwrap(), vec![(true, 4, None)]);

    // Disabled policy
    let c_dis = Arc::new(AtomicUsize::new(0));
    let cb_dis = RecordingCallbacks::default();
    let _ = retry_assistant_call(
        {
            let c = Arc::clone(&c_dis);
            move || {
                c.fetch_add(1, Ordering::SeqCst);
                async { Ok(err_msg("terminated")) }
            }
        },
        Some(&disabled),
        None,
        Some(&cb_dis),
    )
    .await
    .unwrap();
    assert_eq!(c_dis.load(Ordering::SeqCst), 1);

    // Sequence of produce / retry / attempt-start
    let c_ord = Arc::new(AtomicUsize::new(0));
    let cb_ord = RecordingCallbacks::default();
    let _ = retry_assistant_call(
        {
            let c = Arc::clone(&c_ord);
            let evs = &cb_ord.events;
            move || {
                let n = c.fetch_add(1, Ordering::SeqCst);
                evs.lock().unwrap().push(format!("produce:{n}"));
                async move {
                    if n < 2 {
                        Ok(err_msg("terminated"))
                    } else {
                        Ok(faux_assistant_message(
                            FauxAssistantMessageContent::Text("recovered".to_string()),
                            None,
                        ))
                    }
                }
            }
        },
        Some(&enabled),
        None,
        Some(&cb_ord),
    )
    .await
    .unwrap();
    assert_eq!(
        *cb_ord.events.lock().unwrap(),
        vec![
            "produce:0",
            "retry:1",
            "attempt-start",
            "produce:1",
            "retry:2",
            "attempt-start",
            "produce:2",
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn aborts_backoff_sleep_via_signal() {
    let ctrl = AbortController::new();
    let sig = ctrl.signal();
    let calls = Arc::new(AtomicUsize::new(0));
    let cb = Arc::new(RecordingCallbacks::default());

    let c = Arc::clone(&calls);
    let cb_c = Arc::clone(&cb);
    let policy = RetryPolicy {
        enabled: true,
        max_retries: 5,
        base_delay_ms: 10_000,
        max_agent_delay_ms: None,
    };

    let handle = tokio::spawn(async move {
        retry_assistant_call(
            move || {
                c.fetch_add(1, Ordering::SeqCst);
                async { Ok(err_msg("terminated")) }
            },
            Some(&policy),
            Some(&sig),
            Some(cb_c.as_ref()),
        )
        .await
    });

    tokio::task::yield_now().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    ctrl.abort(None);
    let res = handle.await.unwrap().unwrap();
    assert_eq!(res.stop_reason, StopReason::Aborted);
    assert_eq!(res.error_message, None);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        *cb.finished.lock().unwrap(),
        vec![(false, 1, Some("terminated".to_string()))]
    );
}
