//! Port of packages/coding-agent/src/modes/interactive/components/markdown-transform.ts

use std::sync::Arc;

pub use crate::core::extensions::types::{MarkdownMessageType, MarkdownTransformContext, MarkdownTransformer};

pub fn create_markdown_transform(
    message_type: MarkdownMessageType,
    is_streaming: bool,
    transformers: Vec<MarkdownTransformer>,
) -> Arc<dyn Fn(&str, i64) -> String + Send + Sync> {
    create_markdown_transform_dynamic_with_meta(message_type, Arc::new(move || is_streaming), transformers, None, None)
}

pub fn create_markdown_transform_with_meta(
    message_type: MarkdownMessageType,
    is_streaming: bool,
    transformers: Vec<MarkdownTransformer>,
    message_id: Option<String>,
    timestamp: Option<String>,
) -> Arc<dyn Fn(&str, i64) -> String + Send + Sync> {
    create_markdown_transform_dynamic_with_meta(
        message_type,
        Arc::new(move || is_streaming),
        transformers,
        message_id,
        timestamp,
    )
}

pub fn create_markdown_transform_dynamic(
    message_type: MarkdownMessageType,
    is_streaming: Arc<dyn Fn() -> bool + Send + Sync>,
    transformers: Vec<MarkdownTransformer>,
) -> Arc<dyn Fn(&str, i64) -> String + Send + Sync> {
    create_markdown_transform_dynamic_with_meta(message_type, is_streaming, transformers, None, None)
}

pub fn create_markdown_transform_dynamic_with_meta(
    message_type: MarkdownMessageType,
    is_streaming: Arc<dyn Fn() -> bool + Send + Sync>,
    transformers: Vec<MarkdownTransformer>,
    message_id: Option<String>,
    timestamp: Option<String>,
) -> Arc<dyn Fn(&str, i64) -> String + Send + Sync> {
    Arc::new(move |markdown: &str, available_width: i64| {
        let context = MarkdownTransformContext {
            message_type,
            is_streaming: is_streaming(),
            available_width,
            message_id: message_id.clone(),
            timestamp: timestamp.clone(),
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
