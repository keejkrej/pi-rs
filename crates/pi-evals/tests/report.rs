//! Port of packages/evals/test/report.test.ts

use pi_evals::plan::{DocumentationVariant, EvalTask};
use pi_evals::report::{
    CaseStatusOutcome, EvalObservation, EvalOutcome, PI_SESSION_SNAPSHOT_ARTIFACT, VitestJsonStatus,
    classify_case_status, read_task_observation,
};
use serde_json::{Value, json};
use tempfile::TempDir;

const SESSION: &str = "{\"type\":\"session\"}\n";

fn sample_task() -> EvalTask {
    EvalTask {
        file: "evals/example.docs.eval.ts".to_string(),
        full_name: "Example workflow > handles the case".to_string(),
        eval_set: "Example workflow".to_string(),
        case_id: "handles the case".to_string(),
        variant: DocumentationVariant::WithoutDocs,
        model: "fixture/model".to_string(),
        run_number: 1,
    }
}

struct ReportAssertionSpec {
    case_id: Option<String>,
    status: &'static str,
    meta: Value,
}

impl Default for ReportAssertionSpec {
    fn default() -> Self {
        Self {
            case_id: None,
            status: "passed",
            meta: json!({}),
        }
    }
}

fn scored_meta(
    avg_score: Option<Option<f64>>,
    model: Option<&str>,
    errors: Option<Value>,
    artifacts: Option<Value>,
) -> Value {
    let score_val = match avg_score {
        None => json!(0.5),
        Some(None) => Value::Null,
        Some(Some(s)) => json!(s),
    };
    json!({
        "eval": {
            "avgScore": score_val,
            "scores": [{ "name": "StructuredOutputJudge", "score": 0.5 }],
            "thresholdFailed": false,
        },
        "harness": {
            "name": "without_docs",
            "run": {
                "output": { "ok": true },
                "session": { "events": [{ "type": "message", "role": "user", "content": "prompt" }] },
                "usage": {
                    "provider": "fixture",
                    "model": model.unwrap_or("model"),
                    "inputTokens": 10,
                    "outputTokens": 5,
                    "totalTokens": 15,
                    "toolCalls": 1,
                    "metadata": { "cacheReadTokens": 2, "cacheWriteTokens": 3, "estimatedCostUsd": 0.01 },
                },
                "timings": { "totalMs": 1234 },
                "artifacts": artifacts.unwrap_or_else(|| json!({ "runId": "run-1", PI_SESSION_SNAPSHOT_ARTIFACT: SESSION })),
                "errors": errors.unwrap_or_else(|| json!([])),
            },
        },
    })
}

async fn write_task_report(assertions: Vec<ReportAssertionSpec>) -> (TempDir, String, String) {
    let dir = TempDir::new().unwrap();
    let dir_path = dir.path().to_string_lossy().into_owned();
    let report_path = pi_js::path::join(&[&dir_path, "vitest.json"]);
    let task = sample_task();

    let results: Vec<Value> = assertions
        .iter()
        .map(|spec| {
            let cid = spec.case_id.as_deref().unwrap_or(&task.case_id);
            json!({
                "ancestorTitles": [task.eval_set],
                "fullName": format!("{} {}", task.eval_set, cid),
                "status": spec.status,
                "title": cid,
                "failureMessages": [],
                "meta": spec.meta,
            })
        })
        .collect();
    let count = |target: &str| results.iter().filter(|r| r["status"] == target).count();
    let payload = json!({
        "numFailedTests": count("failed"),
        "numPassedTests": count("passed"),
        "numPendingTests": count("pending") + count("skipped") + count("disabled"),
        "numTodoTests": count("todo"),
        "numTotalTests": results.len(),
        "startTime": 0,
        "success": count("failed") == 0,
        "testResults": [{
            "message": "",
            "name": "/repo/packages/evals/evals/example.docs.eval.ts",
            "status": if count("failed") == 0 { "passed" } else { "failed" },
            "assertionResults": results,
        }],
    });
    pi_js::fs::promises::write_file(&report_path, pi_js::json::stringify(&payload).as_bytes(), None)
        .await
        .unwrap();
    (dir, dir_path, report_path)
}

async fn read_observation(spec: ReportAssertionSpec) -> (TempDir, String, EvalObservation) {
    let task = sample_task();
    let (dir, dir_path, report_path) = write_task_report(vec![spec]).await;
    let observation = read_task_observation(&task, &report_path, &dir_path).await.unwrap();
    (dir, dir_path, observation)
}

mod classify_case_status_tests {
    use super::*;

    #[test]
    fn maps_skipped_todo_disabled_to_skipped() {
        for status in [
            VitestJsonStatus::Skipped,
            VitestJsonStatus::Todo,
            VitestJsonStatus::Disabled,
        ] {
            assert_eq!(classify_case_status(status), Some(CaseStatusOutcome::Skipped));
        }
    }

    #[test]
    fn maps_failed_infrastructure_to_errored() {
        assert_eq!(
            classify_case_status(VitestJsonStatus::Failed),
            Some(CaseStatusOutcome::Errored)
        );
    }
}

mod read_task_observation_tests {
    use super::*;

    #[tokio::test]
    async fn preserves_a_skipped_outcome_for_skipped_todo_disabled_cases() {
        for status in ["skipped", "todo", "disabled"] {
            let (_dir, _path, observation) = read_observation(ReportAssertionSpec {
                status,
                ..Default::default()
            })
            .await;
            assert_eq!(observation.outcome, EvalOutcome::Skipped, "status={status}");
        }
    }

    #[tokio::test]
    async fn preserves_a_pending_outcome_when_no_harness_run_exists() {
        let (_dir, _path, observation) = read_observation(ReportAssertionSpec {
            status: "pending",
            ..Default::default()
        })
        .await;
        assert_eq!(observation.outcome, EvalOutcome::Pending);
    }

    #[tokio::test]
    async fn preserves_metrics_from_a_failed_eval_with_a_partial_harness_run() {
        let task = sample_task();
        let (_dir, dir_path, observation) = read_observation(ReportAssertionSpec {
            status: "failed",
            meta: scored_meta(
                None,
                None,
                Some(json!([{ "message": "Prompt verification failed" }])),
                None,
            ),
            ..Default::default()
        })
        .await;
        assert_eq!(
            observation,
            EvalObservation {
                eval_set: task.eval_set.clone(),
                case_id: task.case_id.clone(),
                variant: task.variant,
                model: task.model.clone(),
                run_number: task.run_number,
                input_tokens: Some(10.0),
                output_tokens: Some(5.0),
                cache_read_tokens: Some(2.0),
                cache_write_tokens: Some(3.0),
                total_tokens: Some(15.0),
                tool_calls: Some(1.0),
                total_ms: Some(1234.0),
                estimated_cost_usd: Some(0.01),
                outcome: EvalOutcome::Errored,
            }
        );
        let sessions_dir = pi_js::path::join(&[&dir_path, task.variant.as_str(), "sessions"]);
        let hashes = pi_js::fs::promises::readdir(&sessions_dir).await.unwrap();
        assert_eq!(hashes.len(), 1);
        let session_file = pi_js::path::join(&[&sessions_dir, &hashes[0], "session.jsonl"]);
        assert_eq!(
            pi_js::fs::promises::read_to_string(&session_file).await.unwrap(),
            SESSION
        );
    }

    #[tokio::test]
    async fn records_an_errored_outcome_when_a_passed_eval_has_no_harness_run() {
        let (_dir, _path, observation) = read_observation(ReportAssertionSpec {
            status: "passed",
            ..Default::default()
        })
        .await;
        assert_eq!(observation.outcome, EvalOutcome::Errored);
    }

    #[tokio::test]
    async fn reads_a_scored_harness_run_and_persists_the_session_artifact() {
        let task = sample_task();
        let (_dir, dir_path, observation) = read_observation(ReportAssertionSpec {
            meta: scored_meta(None, None, None, None),
            ..Default::default()
        })
        .await;
        assert_eq!(
            observation,
            EvalObservation {
                eval_set: task.eval_set.clone(),
                case_id: task.case_id.clone(),
                variant: task.variant,
                model: task.model.clone(),
                run_number: task.run_number,
                input_tokens: Some(10.0),
                output_tokens: Some(5.0),
                cache_read_tokens: Some(2.0),
                cache_write_tokens: Some(3.0),
                total_tokens: Some(15.0),
                tool_calls: Some(1.0),
                total_ms: Some(1234.0),
                estimated_cost_usd: Some(0.01),
                outcome: EvalOutcome::Scored { score: 0.5 },
            }
        );
        let sessions_dir = pi_js::path::join(&[&dir_path, task.variant.as_str(), "sessions"]);
        let hashes = pi_js::fs::promises::readdir(&sessions_dir).await.unwrap();
        assert_eq!(hashes.len(), 1);
        let session_file = pi_js::path::join(&[&sessions_dir, &hashes[0], "session.jsonl"]);
        assert_eq!(
            pi_js::fs::promises::read_to_string(&session_file).await.unwrap(),
            SESSION
        );
    }

    #[tokio::test]
    async fn selects_the_requested_case_when_a_sibling_case_is_skipped() {
        let task = sample_task();
        let (_dir, dir_path, report_path) = write_task_report(vec![
            ReportAssertionSpec {
                case_id: Some("sibling case".to_string()),
                status: "skipped",
                meta: json!({}),
            },
            ReportAssertionSpec {
                case_id: Some(task.case_id.clone()),
                status: "passed",
                meta: scored_meta(None, None, None, None),
            },
        ])
        .await;
        let observation = read_task_observation(&task, &report_path, &dir_path).await.unwrap();
        assert_eq!(observation.outcome, EvalOutcome::Scored { score: 0.5 });
        assert_eq!(observation.total_tokens, Some(15.0));
    }

    #[tokio::test]
    async fn treats_a_zero_score_as_scored_data() {
        let (_dir, _path, observation) = read_observation(ReportAssertionSpec {
            meta: scored_meta(Some(Some(0.0)), None, None, None),
            ..Default::default()
        })
        .await;
        assert_eq!(observation.outcome, EvalOutcome::Scored { score: 0.0 });
    }

    #[tokio::test]
    async fn records_an_unscored_outcome_when_a_completed_eval_has_no_score() {
        let (_dir, _path, observation) = read_observation(ReportAssertionSpec {
            meta: scored_meta(Some(None), None, None, None),
            ..Default::default()
        })
        .await;
        assert_eq!(observation.outcome, EvalOutcome::Unscored);
    }

    #[tokio::test]
    async fn records_an_errored_outcome_when_the_reported_model_does_not_match_the_task() {
        let (_dir, _path, observation) = read_observation(ReportAssertionSpec {
            meta: scored_meta(None, Some("other"), None, None),
            ..Default::default()
        })
        .await;
        assert_eq!(observation.outcome, EvalOutcome::Errored);
    }

    #[tokio::test]
    async fn records_an_errored_outcome_when_a_completed_harness_run_contains_errors() {
        let (_dir, _path, observation) = read_observation(ReportAssertionSpec {
            meta: scored_meta(None, None, Some(json!([{ "message": "boom" }])), None),
            ..Default::default()
        })
        .await;
        assert_eq!(observation.outcome, EvalOutcome::Errored);
    }

    #[tokio::test]
    async fn still_scores_a_completed_eval_when_the_session_artifact_is_missing() {
        let (_dir, _path, observation) = read_observation(ReportAssertionSpec {
            meta: scored_meta(None, None, None, Some(json!({ "runId": "run-1" }))),
            ..Default::default()
        })
        .await;
        assert_eq!(observation.outcome, EvalOutcome::Scored { score: 0.5 });
    }
}
