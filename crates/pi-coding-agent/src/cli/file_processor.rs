//! Port of packages/coding-agent/src/cli/file-processor.ts

//! Process @file CLI arguments into text content and image attachments

use pi_ai::ImageContent;

use crate::core::tools::path_utils::resolve_read_path;
use crate::utils::image_process::{ProcessImageOptions, ProcessImageResult, process_image};
use crate::utils::mime::detect_supported_image_mime_type_from_file;
use crate::utils::text::strip_bom;
use crate::vendor::chalk;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcessedFiles {
    pub text: String,
    pub images: Vec<ImageContent>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessFileOptions {
    /// Whether to auto-resize images. Default: true
    pub auto_resize_images: Option<bool>,
}

/// Process @file arguments into text content and image attachments
pub async fn process_file_arguments(
    file_args: &[String],
    options: Option<ProcessFileOptions>,
) -> pi_js::Result<ProcessedFiles> {
    let auto_resize_images = options.and_then(|o| o.auto_resize_images).unwrap_or(true);
    let mut text = String::new();
    let mut images: Vec<ImageContent> = Vec::new();
    let cwd = pi_js::env::cwd();

    for file_arg in file_args {
        let resolved = resolve_read_path(file_arg, &cwd);
        let absolute_path = pi_js::path::resolve(&[&resolved]);

        // Check if file exists and inspect stats
        let stats = match pi_js::fs::promises::stat(&absolute_path).await {
            Ok(s) => s,
            Err(_) => {
                pi_js::console::error(&chalk::red(&format!("Error: File not found: {absolute_path}")));
                return Err(pi_js::Error::Exit(1));
            }
        };

        if stats.size == 0 {
            continue;
        }

        let mime_type = detect_supported_image_mime_type_from_file(&absolute_path).await?;
        if let Some(mime_type) = mime_type {
            let content = pi_js::fs::promises::read_file(&absolute_path).await?;
            let processed = process_image(
                &content,
                mime_type,
                Some(ProcessImageOptions {
                    auto_resize_images: Some(auto_resize_images),
                    resize_options: None,
                }),
            )
            .await;

            match processed {
                ProcessImageResult::Err { message } => {
                    text.push_str(&format!("<file name=\"{absolute_path}\">{message}</file>\n"));
                }
                ProcessImageResult::Ok { data, mime_type, hints } => {
                    images.push(ImageContent { data, mime_type });
                    if !hints.is_empty() {
                        text.push_str(&format!("<file name=\"{absolute_path}\">{}</file>\n", hints.join("\n")));
                    } else {
                        text.push_str(&format!("<file name=\"{absolute_path}\"></file>\n"));
                    }
                }
            }
        } else {
            match pi_js::fs::promises::read_to_string(&absolute_path).await {
                Ok(raw) => {
                    let content = strip_bom(&raw);
                    text.push_str(&format!("<file name=\"{absolute_path}\">\n{content}\n</file>\n"));
                }
                Err(err) => {
                    pi_js::console::error(&chalk::red(&format!(
                        "Error: Could not read file {absolute_path}: {err}"
                    )));
                    return Err(pi_js::Error::Exit(1));
                }
            }
        }
    }

    Ok(ProcessedFiles { text, images })
}
