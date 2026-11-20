//! Port of packages/ai/src/api/system-one-shared.ts

use indexmap::IndexMap;
use serde::Serialize;
use serde_json::{Map, Value};
use url::Url;

use pi_js::abort::AbortSignal;
use pi_js::{Error, Result};

use crate::api::classifier_shared::{is_record, parse_classifier_usage, post_classifier_request, required_number};
pub(crate) use crate::api::classifier_shared::{model_headers_to_provider_headers, serialize_to_value};
use crate::types::{
    ClassifierAnswer, ClassifierBoolCriteria, ClassifierContext, ClassifierModel, ClassifierOptions,
    ClassifierQuestion, ClassifierResult, ClassifierStopReason, ImageContent, JsonValue, ModelModality,
    ProviderHeaders, SystemOneImageInput,
};
use crate::utils::error_body::{format_provider_error, normalize_provider_error};

/// A System One image asset: base64 file bytes, referenced by its name from the state.
#[derive(Clone, Debug, Serialize)]
pub struct SystemOneImageAsset {
    #[serde(rename = "type")]
    pub r#type: &'static str,
    pub format: &'static str,
    pub data_base64: String,
}

/// TypeSafe System One request body without the transport-specific envelope.
#[derive(Clone, Debug, Serialize)]
pub struct SystemOneWireRequest {
    pub state: JsonValue,
    pub questions: IndexMap<String, JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assets: Option<IndexMap<String, SystemOneImageAsset>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>,
}

/// System One compatibility settings with every field resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedSystemOneCompat {
    pub image_input: SystemOneImageInput,
    pub send_image_opt_in_header: bool,
}

/// TypeSafe's opt-in header for its experimental image input.
const IMAGE_OPT_IN_HEADER: (&str, &str) = ("TypeSafe-Danger-Zone-Images", "true");

/// Clef accepts at most this many images per request.
const MAX_DATA_URL_IMAGES: usize = 4;

/// Differences between services that serve System One models.
pub trait SystemOneTransport: Send + Sync {
    /// Classifier API implemented by this transport.
    fn api(&self) -> &str;

    /// Service name used in error messages.
    fn label(&self) -> &str;

    /// Compatibility settings for models that do not set them in `compat`.
    fn detect_compat(&self, model: &ClassifierModel) -> ResolvedSystemOneCompat;

    /// Absolute request URL.
    fn url(&self, model: &ClassifierModel) -> Result<Url>;

    /// Wraps the System One request in the service's request envelope.
    fn payload(&self, model: &ClassifierModel, request: SystemOneWireRequest) -> JsonValue;

    /// Extracts the System One output (`{ answers, usage }`) from the service's response envelope.
    fn output<'a>(&self, body: &'a JsonValue) -> Result<&'a Map<String, JsonValue>>;
}

fn parse_probabilities(label: &str, value: Option<&JsonValue>, id: &str) -> Result<IndexMap<String, f64>> {
    let Some(obj) = value.and_then(|v| is_record(v).then(|| v.as_object()).flatten()) else {
        return Err(Error::msg(format!("{label} returned invalid probabilities for {id}")));
    };
    let mut out = IndexMap::with_capacity(obj.len());
    for (key, prob) in obj {
        let parsed = required_number(label, Some(prob), &format!("probability for {id}.{key}"))?;
        out.insert(key.clone(), parsed);
    }
    Ok(out)
}

fn parse_answers(
    label: &str,
    value: Option<&JsonValue>,
    context: &ClassifierContext,
) -> Result<IndexMap<String, ClassifierAnswer>> {
    let Some(obj) = value.and_then(|v| is_record(v).then(|| v.as_object()).flatten()) else {
        return Err(Error::msg(format!("{label} returned an unexpected response")));
    };
    let mut answers = IndexMap::with_capacity(context.questions.len());
    for (id, question) in &context.questions {
        let Some(answer) = obj.get(id).and_then(Value::as_object) else {
            return Err(Error::msg(format!("{label} did not return an answer for {id}")));
        };
        let answer_type = answer.get("type").and_then(Value::as_str);
        match question {
            ClassifierQuestion::Choice { .. } => {
                let Some(choice) = (answer_type == Some("choice"))
                    .then(|| answer.get("choice").and_then(Value::as_str))
                    .flatten()
                else {
                    return Err(Error::msg(format!("{label} did not return a choice answer for {id}")));
                };
                answers.insert(
                    id.clone(),
                    ClassifierAnswer::Choice {
                        choice: choice.to_owned(),
                        probabilities: parse_probabilities(label, answer.get("probabilities"), id)?,
                        confidence: required_number(label, answer.get("confidence"), &format!("confidence for {id}"))?,
                    },
                );
            }
            ClassifierQuestion::Score { .. } => {
                if answer_type != Some("score") {
                    return Err(Error::msg(format!("{label} did not return a score answer for {id}")));
                }
                answers.insert(
                    id.clone(),
                    ClassifierAnswer::Score {
                        score: required_number(label, answer.get("score"), &format!("score for {id}"))?,
                        confidence: required_number(label, answer.get("confidence"), &format!("confidence for {id}"))?,
                    },
                );
            }
            ClassifierQuestion::Bool { .. } => {
                if answer_type != Some("noul") {
                    return Err(Error::msg(format!("{label} did not return a bool answer for {id}")));
                }
                answers.insert(
                    id.clone(),
                    ClassifierAnswer::Bool {
                        probability: required_number(label, answer.get("noul"), &format!("probability for {id}"))?,
                    },
                );
            }
        }
    }
    Ok(answers)
}

fn asset_format(mime_type: &str) -> Option<&'static str> {
    match mime_type {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpeg"),
        _ => None,
    }
}

fn is_supported_data_url_type(mime_type: &str) -> bool {
    matches!(mime_type, "image/png" | "image/jpeg" | "image/webp")
}

/// Adds the context images to `request` in the service's form.
fn add_images(
    label: &str,
    model: &ClassifierModel,
    input: SystemOneImageInput,
    images: &[ImageContent],
    request: &mut SystemOneWireRequest,
) -> Result<()> {
    match input {
        SystemOneImageInput::Images => {
            if images.len() > MAX_DATA_URL_IMAGES {
                return Err(Error::msg(format!(
                    "{label} accepts at most {MAX_DATA_URL_IMAGES} images for {}, got {}",
                    model.id,
                    images.len()
                )));
            }
            let mut urls = Vec::with_capacity(images.len());
            for image in images {
                if !is_supported_data_url_type(&image.mime_type) {
                    return Err(Error::msg(format!(
                        "{label} accepts PNG, JPEG, and WebP images for {}, got {}",
                        model.id, image.mime_type
                    )));
                }
                urls.push(format!("data:{};base64,{}", image.mime_type, image.data));
            }
            request.images = Some(urls);
            Ok(())
        }
        SystemOneImageInput::StateImageUrls => {
            // OpenRouter validates the images against the model that serves the request.
            let mut state_items = Vec::with_capacity(images.len() + 1);
            for image in images {
                let mut iu = Map::new();
                iu.insert(
                    "url".to_owned(),
                    Value::String(format!("data:{};base64,{}", image.mime_type, image.data)),
                );
                let mut part = Map::new();
                part.insert("type".to_owned(), Value::String("image_url".to_owned()));
                part.insert("image_url".to_owned(), Value::Object(iu));
                state_items.push(Value::Object(part));
            }
            state_items.push(request.state.clone());
            request.state = Value::Array(state_items);
            Ok(())
        }
        SystemOneImageInput::Assets => {
            let mut assets = IndexMap::with_capacity(images.len());
            let mut state_items = Vec::with_capacity(images.len() + 1);
            for (index, image) in images.iter().enumerate() {
                let Some(format) = asset_format(&image.mime_type) else {
                    return Err(Error::msg(format!(
                        "{label} accepts PNG and JPEG images for {}, got {}",
                        model.id, image.mime_type
                    )));
                };
                let name = format!("<image-{}>", index + 1);
                assets.insert(
                    name.clone(),
                    SystemOneImageAsset {
                        r#type: "image",
                        format,
                        data_base64: image.data.clone(),
                    },
                );
                state_items.push(Value::String(name));
            }
            // Every asset must be referenced from the request. The names go before the state, where Clef
            // also places its images.
            state_items.push(request.state.clone());
            request.assets = Some(assets);
            request.state = Value::Array(state_items);
            Ok(())
        }
    }
}

#[derive(Serialize)]
struct WireNoulQuestion<'a> {
    #[serde(rename = "type")]
    r#type: &'static str,
    instructions: &'a str,
    criteria: &'a ClassifierBoolCriteria,
}

/// Maps public `bool` questions to TypeSafe's wire-level `noul` type.
fn wire_request(context: &ClassifierContext) -> SystemOneWireRequest {
    let mut questions = IndexMap::with_capacity(context.questions.len());
    for (id, question) in &context.questions {
        let wired = match question {
            ClassifierQuestion::Bool { instructions, criteria } => serialize_to_value(&WireNoulQuestion {
                r#type: "noul",
                instructions,
                criteria,
            }),
            other => serialize_to_value(other),
        };
        questions.insert(id.clone(), wired);
    }
    SystemOneWireRequest {
        state: serialize_to_value(&context.state),
        questions,
        assets: None,
        images: None,
    }
}

/// Runs one System One classification over the given transport.
pub async fn classify_system_one(
    transport: &dyn SystemOneTransport,
    model: &ClassifierModel,
    context: &ClassifierContext,
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
        timestamp: pi_js::time::now_ms(),
    };

    let caller_signal = options.as_ref().and_then(|o| o.base.signal.clone());

    let run: Result<()> = async {
        if model.api != transport.api() {
            return Err(Error::msg(format!("Unsupported classifier API: {}", model.api)));
        }
        let mut request = wire_request(context);
        let mut request_options = options.clone();
        if let Some(images) = context.images.as_deref().filter(|imgs| !imgs.is_empty()) {
            if !model.input.contains(&ModelModality::Image) {
                return Err(Error::msg(format!(
                    "{} does not support image input for {}",
                    transport.label(),
                    model.id
                )));
            }
            let detected = transport.detect_compat(model);
            let image_input = model
                .compat
                .as_ref()
                .and_then(|c| c.image_input)
                .unwrap_or(detected.image_input);
            add_images(transport.label(), model, image_input, images, &mut request)?;
            let send_opt_in = model
                .compat
                .as_ref()
                .and_then(|c| c.send_image_opt_in_header)
                .unwrap_or(detected.send_image_opt_in_header);
            if send_opt_in {
                let mut opts = request_options.unwrap_or_default();
                let mut merged_headers = ProviderHeaders::new();
                merged_headers.insert(IMAGE_OPT_IN_HEADER.0.to_owned(), Some(IMAGE_OPT_IN_HEADER.1.to_owned()));
                if let Some(existing) = &opts.base.headers {
                    for (k, v) in existing {
                        merged_headers.insert(k.clone(), v.clone());
                    }
                }
                opts.base.headers = Some(merged_headers);
                request_options = Some(opts);
            }
        }

        let url = transport.url(model)?;
        let body = post_classifier_request(
            transport.label(),
            &url,
            model,
            transport.payload(model, request),
            request_options.as_ref(),
            None,
        )
        .await?;

        let result = transport.output(&body)?;
        // Set before parsing answers: a request with malformed answers was still billed.
        output.usage = parse_classifier_usage(result.get("usage"), model);
        output.answers = parse_answers(transport.label(), result.get("answers"), context)?;
        Ok(())
    }
    .await;

    match run {
        Ok(()) => output,
        Err(error) => {
            output.answers.clear();
            output.stop_reason = if caller_signal.as_ref().is_some_and(AbortSignal::aborted) {
                ClassifierStopReason::Aborted
            } else {
                ClassifierStopReason::Error
            };
            let prefix = format!("{} error", transport.label());
            output.error_message = Some(format_provider_error(&normalize_provider_error(&error), Some(&prefix)));
            output
        }
    }
}
