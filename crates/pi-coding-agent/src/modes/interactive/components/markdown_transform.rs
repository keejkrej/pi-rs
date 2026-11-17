//! Port of packages/coding-agent/src/modes/interactive/components/markdown-transform.ts

use std::sync::Arc;

use serde::{Deserialize, Serialize};

// WIRE(@tui-w3): Replace these local mirror types with `crate::core::extensions::types::{MarkdownMessageType, MarkdownTransformContext, MarkdownTransformer}` once @codemode merges onto main.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MarkdownMessageType {
    User,
    Assistant,
    AssistantThinking,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownTransformContext {
    pub message_type: MarkdownMessageType,
    pub is_streaming: bool,
    pub available_width: i64,
}

pub type MarkdownTransformer = Arc<dyn Fn(&str, MarkdownTransformContext) -> String + Send + Sync>;

pub fn create_markdown_transform(
    message_type: MarkdownMessageType,
    is_streaming: bool,
    transformers: Vec<MarkdownTransformer>,
) -> Arc<dyn Fn(&str, i64) -> String + Send + Sync> {
    create_markdown_transform_dynamic(message_type, Arc::new(move || is_streaming), transformers)
}

pub fn create_markdown_transform_dynamic(
    message_type: MarkdownMessageType,
    is_streaming: Arc<dyn Fn() -> bool + Send + Sync>,
    transformers: Vec<MarkdownTransformer>,
) -> Arc<dyn Fn(&str, i64) -> String + Send + Sync> {
    Arc::new(move |markdown: &str, available_width: i64| {
        let context = MarkdownTransformContext {
            message_type,
            is_streaming: is_streaming(),
            available_width,
        };
        apply_markdown_transformers(markdown, &context, &transformers)
    })
}

fn apply_markdown_transformers(
    markdown: &str,
    context: &MarkdownTransformContext,
    transformers: &[MarkdownTransformer],
) -> String {
    let mut transformed_markdown = markdown.to_string();
    for transformer in transformers {
        transformed_markdown = transformer(&transformed_markdown, context.clone());
    }
    transformed_markdown
}
