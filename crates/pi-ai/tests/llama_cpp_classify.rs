//! Port of packages/ai/test/llama-cpp-classify.test.ts

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde_json::{Value, json};

use pi_ai::api::llama_cpp_classify::{
    answer_from_probabilities, classify, label_probabilities, llama_server_root, peak_confidence, render_question,
};
use pi_ai::types::{
    ClassifierAnswer, ClassifierBoolCriteria, ClassifierContext, ClassifierModel, ClassifierOptions,
    ClassifierQuestion, ClassifierStopReason, FetchFunction, ModelCost, ModelModality, ProviderResponse,
};
use pi_js::abort::AbortController;
use pi_js::fetch::{Headers, Response};

static SERVER_COUNT: AtomicUsize = AtomicUsize::new(0);

/// A fresh server URL per test: label tokens are cached per server and model.
fn model() -> ClassifierModel {
    let n = SERVER_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
    ClassifierModel {
        id: "qwen".to_owned(),
        name: "qwen".to_owned(),
        api: "llama-cpp-classify".to_owned(),
        provider: "llama.cpp".to_owned(),
        base_url: format!("http://llama-{n}.test:8080/v1"),
        input: vec![ModelModality::Text],
        input_limits: None,
        context_window: 32768,
        cost: ModelCost::default(),
        headers: None,
    }
}

#[derive(Clone, Debug)]
struct RecordedRequest {
    url: String,
    body: Value,
    headers: Headers,
}

type NextFn = Arc<dyn Fn(&str, usize) -> Vec<(&'static str, f64)> + Send + Sync>;
type TemplateFn = Arc<dyn Fn(&[(String, String)]) -> String + Send + Sync>;
type TokenizeFn = Arc<dyn Fn(&str) -> Vec<i64> + Send + Sync>;

#[derive(Default)]
struct FakeServerOptions {
    next: Option<NextFn>,
    template: Option<TemplateFn>,
    tokenize: Option<TokenizeFn>,
}

fn char_tokens(content: &str) -> Vec<i64> {
    content.chars().map(|ch| i64::from(ch as u32)).collect()
}

fn word_tokens(content: &str) -> Vec<i64> {
    let mut tokens = Vec::new();
    for segment in content.split_inclusive('\n') {
        if let Some(before) = segment.strip_suffix('\n') {
            match before {
                "" => {}
                "Yes" => tokens.push(89),
                "No" => tokens.push(78),
                other => tokens.extend(char_tokens(other)),
            }
            tokens.extend(char_tokens("\n"));
        } else {
            match segment {
                "Yes" => tokens.push(89),
                "No" => tokens.push(78),
                other => tokens.extend(char_tokens(other)),
            }
        }
    }
    tokens
}

fn fake_server(options: FakeServerOptions) -> (FetchFunction, Arc<Mutex<Vec<RecordedRequest>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let requests_ref = Arc::clone(&requests);
    let next_fn = options.next;
    let template_fn = options.template;
    let tokenize_fn = options.tokenize;

    let fetch_fn: FetchFunction = Arc::new(move |req| {
        let requests_ref = Arc::clone(&requests_ref);
        let next_fn = next_fn.clone();
        let template_fn = template_fn.clone();
        let tokenize_fn = tokenize_fn.clone();
        Box::pin(async move {
            let url = req.url.clone();
            let body = req.body_json().unwrap_or(Value::Null);
            requests_ref.lock().unwrap().push(RecordedRequest {
                url: url.clone(),
                body: body.clone(),
                headers: req.headers.clone(),
            });

            let parsed = url::Url::parse(&url).unwrap();
            let path = parsed.path();
            if path == "/tokenize" {
                let content = body["content"].as_str().unwrap_or_default();
                let tokens = match &tokenize_fn {
                    Some(f) => f(content),
                    None => char_tokens(content),
                };
                return Ok(Response::new(
                    200,
                    Headers::from_pairs([("content-type", "application/json")]),
                    pi_js::json::stringify(&json!({ "tokens": tokens })),
                ));
            }
            if path == "/apply-template" {
                let messages: Vec<(String, String)> = body["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|m| {
                        (
                            m["role"].as_str().unwrap_or_default().to_owned(),
                            m["content"].as_str().unwrap_or_default().to_owned(),
                        )
                    })
                    .collect();
                let prompt = match &template_fn {
                    Some(f) => f(&messages),
                    None => {
                        let mut rendered = String::new();
                        for (role, content) in &messages {
                            rendered.push_str(&format!("<|{role}|>\n{content}\n"));
                        }
                        rendered.push_str("<|assistant|>\n");
                        rendered
                    }
                };
                return Ok(Response::new(
                    200,
                    Headers::from_pairs([("content-type", "application/json")]),
                    pi_js::json::stringify(&json!({ "prompt": prompt })),
                ));
            }
            if path == "/completion" {
                let prompt = body["prompt"].as_str().unwrap_or_default();
                let n_probs = body["n_probs"].as_u64().unwrap_or(0) as usize;
                let entries = match &next_fn {
                    Some(f) => f(prompt, n_probs),
                    None => vec![("A", -0.1), ("B", -2.5)],
                };
                let top_logprobs: Vec<Value> = entries
                    .into_iter()
                    .map(|(token, logprob)| {
                        let id = token.chars().next().map(|c| i64::from(c as u32)).unwrap_or(0);
                        json!({
                            "id": id,
                            "token": token,
                            "bytes": [],
                            "logprob": logprob,
                        })
                    })
                    .collect();
                return Ok(Response::new(
                    200,
                    Headers::from_pairs([("content-type", "application/json")]),
                    pi_js::json::stringify(&json!({
                        "content": "A",
                        "completion_probabilities": [{ "id": 65, "token": "A", "top_logprobs": top_logprobs }],
                    })),
                ));
            }
            Ok(Response::new(404, Headers::new(), "not found"))
        })
    });

    (fetch_fn, requests)
}

fn sample_context() -> ClassifierContext {
    let mut state = IndexMap::new();
    state.insert(
        "message".to_owned(),
        Value::String("Help! My payouts have been failing for 3 days.".to_owned()),
    );

    let mut team_criteria = IndexMap::new();
    team_criteria.insert("billing".to_owned(), "Payments and refunds".to_owned());
    team_criteria.insert("technical".to_owned(), "Bugs and outages".to_owned());
    team_criteria.insert("sales".to_owned(), String::new());

    let mut questions = IndexMap::new();
    questions.insert(
        "team".to_owned(),
        ClassifierQuestion::Choice {
            instructions: "Which team should handle this?".to_owned(),
            criteria: team_criteria,
        },
    );
    questions.insert(
        "urgent".to_owned(),
        ClassifierQuestion::Bool {
            instructions: "Does this convey urgency?".to_owned(),
            criteria: ClassifierBoolCriteria {
                r#true: "The user needs help soon".to_owned(),
                r#false: "No time pressure".to_owned(),
            },
        },
    );
    questions.insert(
        "severity".to_owned(),
        ClassifierQuestion::Score {
            instructions: "How severe is this?".to_owned(),
            criteria: vec!["low".to_owned(), "medium".to_owned(), "high".to_owned()],
        },
    );

    ClassifierContext { state, questions }
}

fn completion_depths(requests: &[RecordedRequest]) -> Vec<u64> {
    requests
        .iter()
        .filter(|req| req.url.ends_with("/completion"))
        .map(|req| req.body["n_probs"].as_u64().unwrap_or(0))
        .collect()
}

fn assert_approx(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-12, "expected {expected}, got {actual}");
}

#[tokio::test]
async fn answers_choice_bool_and_score_questions_from_label_log_probabilities() {
    let (fetch_fn, requests) = fake_server(FakeServerOptions {
        next: Some(Arc::new(|prompt, _depth| {
            if prompt.contains("Answer Yes or No.") {
                vec![("Y", -0.05), ("N", -3.0)]
            } else if prompt.contains("Answer with one level number.") {
                vec![("2", -0.2), ("1", -1.8), ("0", -4.0)]
            } else {
                vec![("B", -0.3), ("A", -1.5), ("C", -3.0)]
            }
        })),
        tokenize: Some(Arc::new(|content| word_tokens(content))),
        template: None,
    });
    let classifier_model = model();

    let mut opts = ClassifierOptions::default();
    opts.base.api_key = Some("local".to_owned());
    opts.base.fetch = Some(fetch_fn);
    let result = classify(&classifier_model, &sample_context(), Some(opts)).await;

    assert_eq!(result.error_message, None);
    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);

    let choice = label_probabilities(&[-1.5, -0.3, -3.0], 1.0);
    match result.answers.get("team") {
        Some(ClassifierAnswer::Choice {
            choice: picked,
            probabilities,
            confidence,
        }) => {
            assert_eq!(picked, "technical");
            assert_approx(probabilities["billing"], choice[0]);
            assert_approx(probabilities["technical"], choice[1]);
            assert_approx(probabilities["sales"], choice[2]);
            assert_approx(*confidence, peak_confidence(&choice));
        }
        other => panic!("expected choice answer, got {other:?}"),
    }

    match result.answers.get("urgent") {
        Some(ClassifierAnswer::Bool { probability }) => {
            assert_approx(*probability, label_probabilities(&[-0.05, -3.0], 1.0)[0]);
        }
        other => panic!("expected bool answer, got {other:?}"),
    }

    let levels = label_probabilities(&[-4.0, -1.8, -0.2], 1.0);
    match result.answers.get("severity") {
        Some(ClassifierAnswer::Score { score, confidence }) => {
            assert_approx(*score, levels[1] + 2.0 * levels[2]);
            assert_approx(*confidence, peak_confidence(&levels));
        }
        other => panic!("expected score answer, got {other:?}"),
    }

    let root = llama_server_root(&classifier_model.base_url);
    let reqs = requests.lock().unwrap();
    for req in reqs.iter() {
        assert!(req.url.starts_with(&format!("{root}/")));
        assert_eq!(req.body["model"], "qwen");
        assert_eq!(req.headers.get("authorization").as_deref(), Some("Bearer local"));
    }
    let completion = reqs.iter().find(|r| r.url.ends_with("/completion")).unwrap();
    assert_eq!(completion.body["n_predict"], 1);
    assert_eq!(completion.body["n_probs"], 256);
    assert_eq!(completion.body["post_sampling_probs"], false);
    assert_eq!(completion.body["cache_prompt"], true);

    let template = reqs.iter().find(|r| r.url.ends_with("/apply-template")).unwrap();
    assert_eq!(
        template.body["chat_template_kwargs"],
        json!({ "enable_thinking": false })
    );
}

#[test]
fn repeats_the_state_around_all_questions_and_ends_with_this_questions_labels() {
    let rendered = render_question(&sample_context(), "team").unwrap();
    let state = "State:\n{\n \"message\": \"Help! My payouts have been failing for 3 days.\"\n}";
    assert_eq!(rendered.labels, vec!["A", "B", "C"]);
    assert_eq!(rendered.keys, vec!["billing", "technical", "sales"]);
    assert_eq!(
        rendered.content,
        [
            state,
            "",
            "Task: answer each of the following questions about the state.",
            "",
            "Question: Which team should handle this?",
            "",
            "Options:",
            "- billing: Payments and refunds",
            "- technical: Bugs and outages",
            "- sales",
            "",
            "Question: Does this convey urgency?",
            "",
            "Yes means: The user needs help soon",
            "No means: No time pressure",
            "",
            "Question: How severe is this?",
            "",
            "Levels:",
            "0. low",
            "1. medium",
            "2. high",
            "",
            state,
            "",
            "Question: Which team should handle this?",
            "",
            "Options:",
            "A. billing: Payments and refunds",
            "B. technical: Bugs and outages",
            "C. sales",
            "",
            "Answer with one letter.",
        ]
        .join("\n")
    );
}

#[test]
fn shares_everything_before_the_final_question_across_the_questions_of_a_request() {
    let context = sample_context();
    let prefix = |id: &str| {
        let content = render_question(&context, id).unwrap().content;
        let idx = content.rfind("Question:").unwrap();
        content[..idx].to_owned()
    };
    assert_eq!(prefix("urgent"), prefix("team"));
    assert_eq!(prefix("severity"), prefix("team"));
    assert!(
        render_question(&context, "urgent")
            .unwrap()
            .content
            .ends_with("No means: No time pressure\n\nAnswer Yes or No.")
    );
    assert!(
        render_question(&context, "severity")
            .unwrap()
            .content
            .ends_with("2. high\n\nAnswer with one level number.")
    );
}

#[tokio::test]
async fn divides_label_log_probabilities_by_the_temperature() {
    let (fetch_fn, _requests) = fake_server(FakeServerOptions {
        next: Some(Arc::new(|_p, _d| vec![("A", -0.1), ("B", -2.5)])),
        ..Default::default()
    });
    let mut criteria = IndexMap::new();
    criteria.insert("a".to_owned(), String::new());
    criteria.insert("b".to_owned(), String::new());
    let mut questions = IndexMap::new();
    questions.insert(
        "pick".to_owned(),
        ClassifierQuestion::Choice {
            instructions: "Pick one".to_owned(),
            criteria,
        },
    );

    let mut opts = ClassifierOptions::default();
    opts.base.fetch = Some(fetch_fn);
    opts.temperature = Some(2.0);
    let result = classify(
        &model(),
        &ClassifierContext {
            state: IndexMap::new(),
            questions,
        },
        Some(opts),
    )
    .await;

    let expected = label_probabilities(&[-0.1 / 2.0, -2.5 / 2.0], 1.0);
    match result.answers.get("pick") {
        Some(ClassifierAnswer::Choice { probabilities, .. }) => {
            assert_approx(probabilities["a"], expected[0]);
            assert_approx(probabilities["b"], expected[1]);
        }
        other => panic!("expected choice, got {other:?}"),
    }
    let scaled = label_probabilities(&[-0.1, -2.5], 2.0);
    assert_approx(scaled[0], expected[0]);
    assert_approx(scaled[1], expected[1]);
}

#[tokio::test]
async fn rejects_non_positive_temperatures_before_sending_requests() {
    let (fetch_fn, requests) = fake_server(FakeServerOptions::default());
    let mut opts = ClassifierOptions::default();
    opts.base.fetch = Some(fetch_fn);
    opts.temperature = Some(0.0);

    let result = classify(&model(), &sample_context(), Some(opts)).await;
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("Temperature must be a positive number, got 0")
    );
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn retries_deeper_readouts_when_a_label_is_missing_and_fails_without_inventing_zeros() {
    let mut criteria = IndexMap::new();
    criteria.insert("a".to_owned(), String::new());
    criteria.insert("b".to_owned(), String::new());
    let mut questions = IndexMap::new();
    questions.insert(
        "pick".to_owned(),
        ClassifierQuestion::Choice {
            instructions: "Pick one".to_owned(),
            criteria,
        },
    );
    let ctx = ClassifierContext {
        state: IndexMap::new(),
        questions,
    };

    let (deep_fetch, deep_reqs) = fake_server(FakeServerOptions {
        next: Some(Arc::new(|_p, depth| {
            if depth < 4096 {
                vec![("A", -0.1)]
            } else {
                vec![("A", -0.1), ("B", -9.0)]
            }
        })),
        ..Default::default()
    });
    let mut opts1 = ClassifierOptions::default();
    opts1.base.fetch = Some(deep_fetch);
    let recovered = classify(&model(), &ctx, Some(opts1)).await;
    assert_eq!(recovered.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(completion_depths(&deep_reqs.lock().unwrap()), vec![256, 4096]);

    let (never_fetch, never_reqs) = fake_server(FakeServerOptions {
        next: Some(Arc::new(|_p, _d| vec![("A", -0.1)])),
        ..Default::default()
    });
    let mut opts2 = ClassifierOptions::default();
    opts2.base.fetch = Some(never_fetch);
    let failed = classify(&model(), &ctx, Some(opts2)).await;
    assert_eq!(failed.stop_reason, ClassifierStopReason::Error);
    assert!(failed.answers.is_empty());
    assert!(
        failed
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("did not rank labels B for pick within the top 32768 tokens")
    );
    assert_eq!(completion_depths(&never_reqs.lock().unwrap()), vec![256, 4096, 32768]);
}

#[tokio::test]
async fn closes_a_reasoning_block_the_template_leaves_open() {
    let (fetch_fn, requests) = fake_server(FakeServerOptions {
        template: Some(Arc::new(|_messages| "<|assistant|>\n<think>".to_owned())),
        ..Default::default()
    });
    let mut criteria = IndexMap::new();
    criteria.insert("a".to_owned(), String::new());
    criteria.insert("b".to_owned(), String::new());
    let mut questions = IndexMap::new();
    questions.insert(
        "pick".to_owned(),
        ClassifierQuestion::Choice {
            instructions: "Pick".to_owned(),
            criteria,
        },
    );
    let mut opts = ClassifierOptions::default();
    opts.base.fetch = Some(fetch_fn);
    classify(
        &model(),
        &ClassifierContext {
            state: IndexMap::new(),
            questions,
        },
        Some(opts),
    )
    .await;

    let reqs = requests.lock().unwrap();
    let completion = reqs.iter().find(|r| r.url.ends_with("/completion")).unwrap();
    assert_eq!(completion.body["prompt"], "<|assistant|>\n<think></think>");
}

#[tokio::test]
async fn reads_labels_in_reply_position_and_rejects_labels_that_are_not_one_token() {
    let (merging_fetch, _) = fake_server(FakeServerOptions {
        tokenize: Some(Arc::new(|content| {
            if content.starts_with('\n') && content.len() > 1 {
                vec![1000]
            } else {
                char_tokens(content)
            }
        })),
        ..Default::default()
    });
    let mut criteria = IndexMap::new();
    criteria.insert("a".to_owned(), String::new());
    criteria.insert("b".to_owned(), String::new());
    let mut q_choice = IndexMap::new();
    q_choice.insert(
        "pick".to_owned(),
        ClassifierQuestion::Choice {
            instructions: "Pick".to_owned(),
            criteria,
        },
    );
    let mut opts1 = ClassifierOptions::default();
    opts1.base.fetch = Some(merging_fetch);
    let merged = classify(
        &model(),
        &ClassifierContext {
            state: IndexMap::new(),
            questions: q_choice,
        },
        Some(opts1),
    )
    .await;
    assert_eq!(merged.stop_reason, ClassifierStopReason::Stop);

    let (split_fetch, _) = fake_server(FakeServerOptions::default());
    let mut q_bool = IndexMap::new();
    q_bool.insert(
        "ok".to_owned(),
        ClassifierQuestion::Bool {
            instructions: "OK?".to_owned(),
            criteria: ClassifierBoolCriteria {
                r#true: String::new(),
                r#false: String::new(),
            },
        },
    );
    let mut opts2 = ClassifierOptions::default();
    opts2.base.fetch = Some(split_fetch);
    let result = classify(
        &model(),
        &ClassifierContext {
            state: IndexMap::new(),
            questions: q_bool,
        },
        Some(opts2),
    )
    .await;
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("Label \"Yes\" is not a single token for qwen")
    );
}

#[tokio::test]
async fn caches_label_tokens_per_server_and_model() {
    let classifier_model = model();
    let (fetch_fn, requests) = fake_server(FakeServerOptions::default());
    let mut criteria = IndexMap::new();
    criteria.insert("a".to_owned(), String::new());
    criteria.insert("b".to_owned(), String::new());
    let mut questions = IndexMap::new();
    questions.insert(
        "pick".to_owned(),
        ClassifierQuestion::Choice {
            instructions: "Pick".to_owned(),
            criteria,
        },
    );
    let ctx = ClassifierContext {
        state: IndexMap::new(),
        questions,
    };

    let mut opts1 = ClassifierOptions::default();
    opts1.base.fetch = Some(Arc::clone(&fetch_fn));
    classify(&classifier_model, &ctx, Some(opts1)).await;
    let first_tokenizations = requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.url.ends_with("/tokenize"))
        .count();

    let mut opts2 = ClassifierOptions::default();
    opts2.base.fetch = Some(fetch_fn);
    classify(&classifier_model, &ctx, Some(opts2)).await;

    assert!(first_tokenizations > 0);
    let total_tokenizations = requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.url.ends_with("/tokenize"))
        .count();
    assert_eq!(total_tokenizations, first_tokenizations);
}

#[tokio::test]
async fn validates_option_counts_before_sending_requests() {
    let (fetch_fn, requests) = fake_server(FakeServerOptions::default());
    let mut criteria = IndexMap::new();
    for index in 0..63 {
        criteria.insert(format!("option{index}"), String::new());
    }
    let mut q_many = IndexMap::new();
    q_many.insert(
        "pick".to_owned(),
        ClassifierQuestion::Choice {
            instructions: "Pick".to_owned(),
            criteria,
        },
    );
    let mut opts1 = ClassifierOptions::default();
    opts1.base.fetch = Some(Arc::clone(&fetch_fn));
    let too_many = classify(
        &model(),
        &ClassifierContext {
            state: IndexMap::new(),
            questions: q_many,
        },
        Some(opts1),
    )
    .await;

    let mut q_few = IndexMap::new();
    q_few.insert(
        "rate".to_owned(),
        ClassifierQuestion::Score {
            instructions: "Rate".to_owned(),
            criteria: vec!["only".to_owned()],
        },
    );
    let mut opts2 = ClassifierOptions::default();
    opts2.base.fetch = Some(fetch_fn);
    let too_few = classify(
        &model(),
        &ClassifierContext {
            state: IndexMap::new(),
            questions: q_few,
        },
        Some(opts2),
    )
    .await;

    assert!(
        too_many
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("A choice question needs 2 to 62 options, got 63")
    );
    assert!(
        too_few
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("A score question needs 2 to 10 levels, got 1")
    );
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn passes_completion_payloads_and_responses_through_the_request_hooks() {
    let (fetch_fn, requests) = fake_server(FakeServerOptions::default());
    let payloads = Arc::new(Mutex::new(Vec::<Value>::new()));
    let payloads_ref = Arc::clone(&payloads);
    let statuses = Arc::new(Mutex::new(Vec::<u16>::new()));
    let statuses_ref = Arc::clone(&statuses);

    let mut criteria = IndexMap::new();
    criteria.insert("a".to_owned(), String::new());
    criteria.insert("b".to_owned(), String::new());
    let mut questions = IndexMap::new();
    questions.insert(
        "pick".to_owned(),
        ClassifierQuestion::Choice {
            instructions: "Pick".to_owned(),
            criteria,
        },
    );

    let mut opts = ClassifierOptions::default();
    opts.base.fetch = Some(fetch_fn);
    opts.base.on_payload = Some(Arc::new(move |payload: Value, _m: &ClassifierModel| {
        let payloads_ref = Arc::clone(&payloads_ref);
        Box::pin(async move {
            payloads_ref.lock().unwrap().push(payload.clone());
            let mut obj = payload.as_object().cloned().unwrap_or_default();
            obj.insert("id_slot".to_owned(), json!(1));
            Ok(Some(Value::Object(obj)))
        })
    }));
    opts.base.on_response = Some(Arc::new(move |response: ProviderResponse, _m: &ClassifierModel| {
        let statuses_ref = Arc::clone(&statuses_ref);
        Box::pin(async move {
            statuses_ref.lock().unwrap().push(response.status);
            Ok(())
        })
    }));

    classify(
        &model(),
        &ClassifierContext {
            state: IndexMap::new(),
            questions,
        },
        Some(opts),
    )
    .await;

    let recorded_payloads = payloads.lock().unwrap();
    assert_eq!(recorded_payloads.len(), 1);
    assert_eq!(recorded_payloads[0]["n_predict"], 1);
    assert_eq!(*statuses.lock().unwrap(), vec![200]);
    let reqs = requests.lock().unwrap();
    let completion = reqs.iter().find(|r| r.url.ends_with("/completion")).unwrap();
    assert_eq!(completion.body["id_slot"], 1);
}

#[tokio::test]
async fn reports_server_errors_and_cancellation() {
    let mut opts1 = ClassifierOptions::default();
    opts1.base.max_retries = Some(0);
    opts1.base.fetch = Some(Arc::new(|_req| {
        Box::pin(async move {
            Ok(Response::new(
                400,
                Headers::from_pairs([("content-type", "application/json")]),
                r#"{"error":{"message":"context overflow"}}"#,
            ))
        })
    }));
    let failing = classify(&model(), &sample_context(), Some(opts1)).await;
    assert_eq!(failing.stop_reason, ClassifierStopReason::Error);
    let err_msg = failing.error_message.as_deref().unwrap_or_default();
    assert!(err_msg.contains("llama.cpp error (400)"));
    assert!(err_msg.contains("context overflow"));

    let controller = AbortController::new();
    controller.abort(None);
    let mut opts2 = ClassifierOptions::default();
    opts2.base.signal = Some(controller.signal());
    opts2.base.fetch = Some(Arc::new(|req| {
        Box::pin(async move {
            if let Some(sig) = &req.signal {
                sig.throw_if_aborted()?;
            }
            Ok(Response::new(200, Headers::new(), "{}"))
        })
    }));
    let aborted = classify(&model(), &sample_context(), Some(opts2)).await;
    assert_eq!(aborted.stop_reason, ClassifierStopReason::Aborted);
}

#[tokio::test]
async fn rejects_models_for_other_classifier_apis() {
    let (fetch_fn, requests) = fake_server(FakeServerOptions::default());
    let mut wrong_model = model();
    wrong_model.api = "typesafe-system-one".to_owned();
    let mut opts = ClassifierOptions::default();
    opts.base.fetch = Some(fetch_fn);

    let result = classify(&wrong_model, &sample_context(), Some(opts)).await;
    assert!(
        result
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("Unsupported classifier API: typesafe-system-one")
    );
    assert!(requests.lock().unwrap().is_empty());
}

#[test]
fn derives_the_server_root_from_openai_compatible_base_urls() {
    assert_eq!(llama_server_root("http://127.0.0.1:8080/v1/"), "http://127.0.0.1:8080");
    assert_eq!(
        llama_server_root("https://example.com/prefix/v1"),
        "https://example.com/prefix"
    );
    assert_eq!(llama_server_root("http://127.0.0.1:8080"), "http://127.0.0.1:8080");
}

#[test]
fn computes_typesafe_confidence_and_expected_scores() {
    let c1 = peak_confidence(&[0.89, 0.06, 0.05]);
    assert!((c1 - 0.835).abs() < 1e-6);
    assert_eq!(peak_confidence(&[0.5, 0.5]), 0.0);
    assert_eq!(peak_confidence(&[1.0, 0.0, 0.0]), 1.0);

    let ans = answer_from_probabilities(
        &ClassifierQuestion::Score {
            instructions: String::new(),
            criteria: vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
        },
        &["0".to_owned(), "1".to_owned(), "2".to_owned()],
        &[0.2, 0.3, 0.5],
    );
    assert_eq!(
        ans,
        ClassifierAnswer::Score {
            score: 1.3,
            confidence: peak_confidence(&[0.2, 0.3, 0.5]),
        }
    );
}
