//! Port of packages/ai/src/utils/model-operations.ts

use indexmap::IndexMap;

use crate::types::{
    AnyModel, AssistantImages, ClassifierModel, ClassifierResult, ClassifierStopReason, ImageModel, ImagesStopReason,
    Model, ModelType,
};
use crate::utils::diagnostics::format_thrown_value;
use crate::utils::models_error::{ModelsError, ModelsErrorCode};

/// The type of a model. Models without explicit `type` are chat models.
pub fn get_model_type(model: &AnyModel) -> ModelType {
    model.model_type().unwrap_or(ModelType::Chat)
}

/// Runtime-checked model type check, including legacy chat models without `type`.
pub fn is_model_type(model: &AnyModel, expected: ModelType) -> bool {
    get_model_type(model) == expected
}

pub fn assert_chat_model(model: &AnyModel) -> pi_js::Result<&Model> {
    if is_model_type(model, ModelType::Chat) {
        if let Some(chat) = model.as_chat() {
            return Ok(chat);
        }
    }
    Err(ModelsError::new(
        ModelsErrorCode::Provider,
        format!("Model {}/{} is not a chat model", model.provider(), model.id()),
    )
    .into())
}

pub fn assert_image_model(model: &AnyModel) -> pi_js::Result<&ImageModel> {
    if is_model_type(model, ModelType::Image) {
        if let Some(image) = model.as_image() {
            return Ok(image);
        }
    }
    Err(ModelsError::new(
        ModelsErrorCode::Provider,
        format!("Model {}/{} is not an image model", model.provider(), model.id()),
    )
    .into())
}

pub fn assert_classifier_model(model: &AnyModel) -> pi_js::Result<&ClassifierModel> {
    if is_model_type(model, ModelType::Classifier) {
        if let Some(classifier) = model.as_classifier() {
            return Ok(classifier);
        }
    }
    Err(ModelsError::new(
        ModelsErrorCode::Provider,
        format!("Model {}/{} is not a classifier model", model.provider(), model.id()),
    )
    .into())
}

pub fn image_error_result(model: &ImageModel, error: &pi_js::Error, aborted: bool) -> AssistantImages {
    image_error_result_message(model, format_thrown_value(error), aborted)
}

pub fn image_error_result_message(
    model: &ImageModel,
    error_message: impl Into<String>,
    aborted: bool,
) -> AssistantImages {
    AssistantImages {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        output: Vec::new(),
        response_id: None,
        usage: None,
        stop_reason: if aborted {
            ImagesStopReason::Aborted
        } else {
            ImagesStopReason::Error
        },
        error_message: Some(error_message.into()),
        timestamp: pi_js::time::now_ms(),
    }
}

pub fn classifier_error_result(model: &ClassifierModel, error: &pi_js::Error, aborted: bool) -> ClassifierResult {
    classifier_error_result_message(model, format_thrown_value(error), aborted)
}

pub fn classifier_error_result_message(
    model: &ClassifierModel,
    error_message: impl Into<String>,
    aborted: bool,
) -> ClassifierResult {
    ClassifierResult {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        answers: IndexMap::new(),
        usage: None,
        stop_reason: if aborted {
            ClassifierStopReason::Aborted
        } else {
            ClassifierStopReason::Error
        },
        error_message: Some(error_message.into()),
        timestamp: pi_js::time::now_ms(),
    }
}
