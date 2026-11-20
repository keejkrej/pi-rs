//! Port of packages/ai/src/api/openai-decisions.ts

use std::sync::Arc;

use indexmap::IndexMap;
use pi_js::abort::AbortSignal;
use pi_js::error::{Error, Result};
use pi_js::{BoxFuture, time::now_ms};
use serde_json::{Map, Value};
use url::Url;

use crate::api::classifier_shared::{
    classifier_http_status, is_record, parse_classifier_usage, post_classifier_request, required_number,
};
use crate::types::{
    ClassifierAnswer, ClassifierBoolCriteria, ClassifierContext, ClassifierModel, ClassifierOptions,
    ClassifierQuestion, ClassifierResult, ClassifierStopReason, ProviderClassifier,
};
use crate::utils::error_body::{format_provider_error, normalize_provider_error};

/// OpenAI's Decisions API: `POST /v1/decisions` with `{ model, input, questions }`.
/// https://developers.openai.com/api/docs/guides/decisions
///
/// The state is sent as JSON text. With images, the input becomes one user message with the state
/// as `input_text` followed by `input_image` data URLs. Questions map to Decisions types:
/// `choice` to `choice`, `score` to `score`, and `bool` to `predicate`. Predicates have no
/// criteria field, so the meanings of true and false are appended to the instructions.
///
/// Only OpenAI API keys work: Sign in with ChatGPT tokens are rejected on this route.
const LABEL: &str = "OpenAI Decisions";

/// The endpoint accepts at most this many image parts per request.
const MAX_IMAGES: usize = 128;

/// Cloudflare in front of api.openai.com answers 504 with an HTML page when a request runs longer
/// than about five seconds. Large inputs, currently above roughly 600K tokens, hit this limit, and
/// retrying the same input runs into it again, so 504 is not retried.
const NO_RETRY_STATUSES: &[u16] = &[504];

fn predicate_instructions(instructions: &str, criteria: &ClassifierBoolCriteria) -> String {
    let mut meanings = Vec::new();
    if !criteria.r#true.is_empty() {
        meanings.push(format!("True means: {}", criteria.r#true));
    }
    if !criteria.r#false.is_empty() {
        meanings.push(format!("False means: {}", criteria.r#false));
    }
    if meanings.is_empty() {
        instructions.to_owned()
    } else {
        format!("{instructions}\n\n{}", meanings.join("\n"))
    }
}

fn wire_question(name: &str, question: &ClassifierQuestion) -> Value {
    let mut obj = Map::new();
    match question {
        ClassifierQuestion::Choice { instructions, criteria } => {
            obj.insert("type".to_owned(), Value::String("choice".to_owned()));
            obj.insert("name".to_owned(), Value::String(name.to_owned()));
            obj.insert("instructions".to_owned(), Value::String(instructions.clone()));
            let choices: Vec<Value> = criteria
                .iter()
                .map(|(value, description)| {
                    let mut c = Map::new();
                    c.insert("value".to_owned(), Value::String(value.clone()));
                    if !description.is_empty() {
                        c.insert("description".to_owned(), Value::String(description.clone()));
                    }
                    Value::Object(c)
                })
                .collect();
            obj.insert("choices".to_owned(), Value::Array(choices));
        }
        ClassifierQuestion::Score { instructions, criteria } => {
            obj.insert("type".to_owned(), Value::String("score".to_owned()));
            obj.insert("name".to_owned(), Value::String(name.to_owned()));
            obj.insert("instructions".to_owned(), Value::String(instructions.clone()));
            let levels: Vec<Value> = criteria
                .iter()
                .map(|label| {
                    let mut lvl = Map::new();
                    lvl.insert("label".to_owned(), Value::String(label.clone()));
                    Value::Object(lvl)
                })
                .collect();
            obj.insert("levels".to_owned(), Value::Array(levels));
        }
        ClassifierQuestion::Bool { instructions, criteria } => {
            obj.insert("type".to_owned(), Value::String("predicate".to_owned()));
            obj.insert("name".to_owned(), Value::String(name.to_owned()));
            obj.insert(
                "instructions".to_owned(),
                Value::String(predicate_instructions(instructions, criteria)),
            );
        }
    }
    Value::Object(obj)
}

fn wire_input(context: &ClassifierContext) -> Result<Value> {
    let state = pi_js::json::stringify(&context.state);
    let images = context.images.as_deref().unwrap_or(&[]);
    if images.is_empty() {
        return Ok(Value::String(state));
    }
    if images.len() > MAX_IMAGES {
        return Err(Error::msg(format!(
            "{LABEL} accepts at most {MAX_IMAGES} images, got {}",
            images.len()
        )));
    }
    let mut parts = Vec::with_capacity(1 + images.len());
    let mut text_part = Map::new();
    text_part.insert("type".to_owned(), Value::String("input_text".to_owned()));
    text_part.insert("text".to_owned(), Value::String(state));
    parts.push(Value::Object(text_part));
    for image in images {
        let mut img_part = Map::new();
        img_part.insert("type".to_owned(), Value::String("input_image".to_owned()));
        img_part.insert(
            "image_url".to_owned(),
            Value::String(format!("data:{};base64,{}", image.mime_type, image.data)),
        );
        parts.push(Value::Object(img_part));
    }
    let mut msg = Map::new();
    msg.insert("role".to_owned(), Value::String("user".to_owned()));
    msg.insert("content".to_owned(), Value::Array(parts));
    Ok(Value::Array(vec![Value::Object(msg)]))
}

fn choice_probabilities(value: Option<&Value>, id: &str) -> Result<IndexMap<String, f64>> {
    let Some(entries) = value.and_then(Value::as_array) else {
        return Err(Error::msg(format!("{LABEL} returned invalid probabilities for {id}")));
    };
    let mut map = IndexMap::with_capacity(entries.len());
    for entry in entries {
        let Some(obj) = entry.as_object() else {
            return Err(Error::msg(format!("{LABEL} returned invalid probabilities for {id}")));
        };
        let Some(val) = obj.get("value").and_then(Value::as_str) else {
            return Err(Error::msg(format!("{LABEL} returned invalid probabilities for {id}")));
        };
        let prob = required_number(LABEL, obj.get("probability"), &format!("probability for {id}.{val}"))?;
        map.insert(val.to_owned(), prob);
    }
    Ok(map)
}

fn parse_answer(id: &str, question: &ClassifierQuestion, answer: &Map<String, Value>) -> Result<ClassifierAnswer> {
    if answer.get("type").and_then(Value::as_str) == Some("refusal") {
        return Err(Error::msg(format!("{LABEL} refused to answer {id}")));
    }
    let answer_type = answer.get("type").and_then(Value::as_str);
    match question {
        ClassifierQuestion::Choice { .. } => {
            if answer_type != Some("choice") {
                return Err(Error::msg(format!("{LABEL} did not return a choice answer for {id}")));
            }
            let Some(choice) = answer.get("choice").and_then(Value::as_str) else {
                return Err(Error::msg(format!("{LABEL} did not return a choice answer for {id}")));
            };
            let probabilities = choice_probabilities(answer.get("probabilities"), id)?;
            let confidence = required_number(LABEL, answer.get("confidence"), &format!("confidence for {id}"))?;
            Ok(ClassifierAnswer::Choice {
                choice: choice.to_owned(),
                probabilities,
                confidence,
            })
        }
        ClassifierQuestion::Score { .. } => {
            if answer_type != Some("score") {
                return Err(Error::msg(format!("{LABEL} did not return a score answer for {id}")));
            }
            let score = required_number(LABEL, answer.get("score"), &format!("score for {id}"))?;
            let confidence = required_number(LABEL, answer.get("confidence"), &format!("confidence for {id}"))?;
            Ok(ClassifierAnswer::Score { score, confidence })
        }
        ClassifierQuestion::Bool { .. } => {
            if answer_type != Some("predicate") {
                return Err(Error::msg(format!(
                    "{LABEL} did not return a predicate answer for {id}"
                )));
            }
            let probability = required_number(LABEL, answer.get("probability"), &format!("probability for {id}"))?;
            Ok(ClassifierAnswer::Bool { probability })
        }
    }
}

fn parse_answers(value: Option<&Value>, context: &ClassifierContext) -> Result<IndexMap<String, ClassifierAnswer>> {
    let Some(arr) = value.and_then(Value::as_array) else {
        return Err(Error::msg(format!("{LABEL} returned an unexpected response")));
    };
    let mut by_name: IndexMap<&str, &Map<String, Value>> = IndexMap::new();
    for item in arr {
        if let Some(obj) = item.as_object()
            && let Some(name) = obj.get("name").and_then(Value::as_str)
        {
            by_name.insert(name, obj);
        }
    }
    let mut answers = IndexMap::with_capacity(context.questions.len());
    for (id, question) in &context.questions {
        let Some(answer) = by_name.get(id.as_str()) else {
            return Err(Error::msg(format!("{LABEL} did not return an answer for {id}")));
        };
        answers.insert(id.clone(), parse_answer(id, question, answer)?);
    }
    Ok(answers)
}

fn format_error_message(error: &Error) -> String {
    if classifier_http_status(error) == Some(504) {
        return format!(
            "{LABEL} error (504): the request timed out at the gateway. Very large inputs (above roughly 600K tokens) currently exceed its time limit."
        );
    }
    format_provider_error(&normalize_provider_error(error), Some(&format!("{LABEL} error")))
}

async fn classify_impl(
    model: ClassifierModel,
    context: ClassifierContext,
    options: Option<ClassifierOptions>,
) -> ClassifierResult {
    let mut output = ClassifierResult {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        answers: IndexMap::new(),
        usage: None,
        stop_reason: ClassifierStopReason::Stop,
        error_message: None,
        timestamp: now_ms(),
    };

    let res: Result<()> = async {
        if model.api != "openai-decisions" {
            return Err(Error::msg(format!("Unsupported classifier API: {}", model.api)));
        }
        let base_url_with_slash = format!("{}/", model.base_url.trim_end_matches('/'));
        let url = Url::parse(&base_url_with_slash)
            .and_then(|u| u.join("decisions"))
            .map_err(|err| Error::msg(format!("{LABEL} invalid baseUrl \"{}\": {err}", model.base_url)))?;

        let input = wire_input(&context)?;
        let questions: Vec<Value> = context.questions.iter().map(|(id, q)| wire_question(id, q)).collect();
        let mut payload = Map::new();
        payload.insert("model".to_owned(), Value::String(model.id.clone()));
        payload.insert("input".to_owned(), input);
        payload.insert("questions".to_owned(), Value::Array(questions));

        let body = post_classifier_request(
            LABEL,
            &url,
            &model,
            Value::Object(payload),
            options.as_ref(),
            Some(NO_RETRY_STATUSES),
        )
        .await?;
        if !is_record(&body) {
            return Err(Error::msg(format!("{LABEL} returned an unexpected response")));
        }
        // Set before parsing answers: a request with malformed or refused answers was still billed.
        output.usage = parse_classifier_usage(body.get("usage"), &model);
        output.answers = parse_answers(body.get("answers"), &context)?;
        Ok(())
    }
    .await;

    if let Err(err) = res {
        output.answers.clear();
        let aborted = options
            .as_ref()
            .and_then(|o| o.base.signal.as_ref())
            .is_some_and(AbortSignal::aborted);
        output.stop_reason = if aborted {
            ClassifierStopReason::Aborted
        } else {
            ClassifierStopReason::Error
        };
        output.error_message = Some(format_error_message(&err));
    }

    output
}

/// Classification through OpenAI's Decisions API.
pub fn classify(
    model: &ClassifierModel,
    context: &ClassifierContext,
    options: Option<ClassifierOptions>,
) -> BoxFuture<ClassifierResult> {
    let model = model.clone();
    let context = context.clone();
    Box::pin(classify_impl(model, context, options))
}

pub fn provider_classifier() -> ProviderClassifier {
    ProviderClassifier {
        classify: Arc::new(|model, context, options| classify(model, context, options)),
    }
}
