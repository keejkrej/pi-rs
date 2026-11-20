//! Port of packages/coding-agent/src/core/tools/read.ts

use std::sync::Arc;

use pi_ai::ModelImageResizeOptions;
use pi_js::BoxFuture;
use serde::{Deserialize, Serialize};

use super::truncate::TruncationResult;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadToolInput {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadToolDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
}

pub type ReadByteStream = std::pin::Pin<Box<dyn futures::Stream<Item = pi_js::Result<Vec<u8>>> + Send>>;

/// Pluggable operations for the read tool.
pub trait ReadOperations: Send + Sync {
    fn read_file(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<Vec<u8>>>;
    fn access(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<()>>;
    fn detect_image_mime_type(&self, _absolute_path: &str) -> BoxFuture<pi_js::Result<Option<String>>> {
        Box::pin(async { Ok(None) })
    }
    /// Stream file contents in chunks. Only shown lines are buffered into memory when present.
    fn create_read_stream(
        &self,
        _absolute_path: &str,
        _signal: Option<pi_js::abort::AbortSignal>,
    ) -> Option<BoxFuture<pi_js::Result<ReadByteStream>>> {
        None
    }
}

#[derive(Clone, Default)]
pub struct ReadToolOptions {
    pub auto_resize_images: Option<bool>,
    pub resize_options: Option<ModelImageResizeOptions>,
    pub operations: Option<Arc<dyn ReadOperations>>,
}

use std::io::Cursor;
use std::sync::LazyLock;

use futures::StreamExt;
use image::codecs::png::PngEncoder;
use image::{ImageEncoder, ImageFormat};
use pi_agent_core::types::{AgentTool, AgentToolResult};
use pi_ai::types::{ImageContent, ModelModality, TextContent, UserContentBlock};
use pi_js::seam::{Guard, Slot};
use pi_js::text::{TextDecoderOptions, Utf8StreamDecoder};
use pi_js::vendor::typebox::{Schema, t};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use crate::core::extensions::types::ToolDefinition;
use crate::core::tools::edit::ToolSystemPromptContribution;
use crate::core::tools::path_utils::resolve_read_path;
use crate::core::tools::renderers;
use crate::core::tools::tool_definition_wrapper::{
    BuiltinToolDefSpec, build_builtin_tool_definition, resolve_effective_cwd, strict_prefer_sampling,
    wrap_tool_definition,
};
use crate::core::tools::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, format_size, truncate_head};
pub use crate::utils::image_resize::ImageResizeOptions as ResizeImageOptions;
use crate::utils::image_resize::{ResizedImage, format_dimension_note, resize_image as real_resize_image};
use crate::utils::mime::detect_supported_image_mime_type_from_file;

pub type ResizeImageOverrideFn =
    Arc<dyn Fn(ImageContent, ResizeImageOptions) -> BoxFuture<pi_js::Result<Option<ResizedImage>>> + Send + Sync>;
static RESIZE_IMAGE_SLOT: LazyLock<Slot<ResizeImageOverrideFn>> = LazyLock::new(Slot::new);

#[doc(hidden)]
pub mod testing {
    use super::*;

    pub fn override_resize_image(func: ResizeImageOverrideFn) -> Guard {
        RESIZE_IMAGE_SLOT.set(Arc::new(func))
    }
}

struct LocalReadOperations;

impl ReadOperations for LocalReadOperations {
    fn read_file(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<Vec<u8>>> {
        let path = absolute_path.to_string();
        Box::pin(async move {
            tokio::fs::read(&path)
                .await
                .map_err(|e| pi_js::Error::msg(e.to_string()))
        })
    }

    fn access(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<()>> {
        let path = absolute_path.to_string();
        Box::pin(async move { pi_js::fs::promises::access(&path, pi_js::fs::constants::R_OK).await })
    }

    fn detect_image_mime_type(&self, absolute_path: &str) -> BoxFuture<pi_js::Result<Option<String>>> {
        let path = absolute_path.to_string();
        Box::pin(async move {
            detect_supported_image_mime_type_from_file(&path)
                .await
                .map(|opt| opt.map(str::to_owned))
        })
    }

    fn create_read_stream(
        &self,
        absolute_path: &str,
        signal: Option<pi_js::abort::AbortSignal>,
    ) -> Option<BoxFuture<pi_js::Result<ReadByteStream>>> {
        let path = absolute_path.to_string();
        Some(Box::pin(async move {
            let file = tokio::fs::File::open(&path)
                .await
                .map_err(|e| pi_js::Error::msg(e.to_string()))?;
            let meta = file.metadata().await.map_err(|e| pi_js::Error::msg(e.to_string()))?;
            let remaining = if meta.is_file() && meta.len() > 0 {
                Some(meta.len() as usize)
            } else {
                None
            };
            let stream = futures::stream::unfold((file, remaining, signal), |(mut file, mut rem, sig)| async move {
                if sig.as_ref().is_some_and(|s| s.aborted()) {
                    return Some((Err(pi_js::Error::msg("Operation aborted")), (file, rem, sig)));
                }
                if rem == Some(0) {
                    return None;
                }
                let want = rem.map_or(65536, |r| r.min(65536));
                let mut buf = vec![0u8; want];
                match file.read(&mut buf).await {
                    Ok(0) => None,
                    Ok(n) => {
                        buf.truncate(n);
                        if let Some(ref mut r) = rem {
                            *r = r.saturating_sub(n);
                        }
                        Some((Ok(buf), (file, rem, sig)))
                    }
                    Err(e) => Some((Err(pi_js::Error::msg(e.to_string())), (file, Some(0), sig))),
                }
            });
            Ok(Box::pin(stream) as ReadByteStream)
        }))
    }
}

pub fn create_local_read_operations() -> Arc<dyn ReadOperations> {
    Arc::new(LocalReadOperations)
}

pub fn read_tool_schema() -> Schema {
    t::object([
        (
            "path",
            t::string().with("description", "Path to the file to read (relative or absolute)"),
        ),
        (
            "offset",
            t::number()
                .optional()
                .with("description", "Line number to start reading from (1-indexed)"),
        ),
        (
            "limit",
            t::number()
                .optional()
                .with("description", "Maximum number of lines to read"),
        ),
    ])
    .with("additionalProperties", false)
}

pub fn read_tool_output_schema() -> Schema {
    t::union([
        t::string(),
        t::object([
            ("type", t::literal("image")),
            ("data", t::string()),
            ("mimeType", t::string()),
            ("note", t::string()),
        ])
        .with("additionalProperties", false),
    ])
}

pub fn read_tool_system_prompt_contribution() -> ToolSystemPromptContribution {
    ToolSystemPromptContribution {
        prompt_snippet: "Read the contents of a file (supports text line offset/limit and images)".to_string(),
        prompt_guidelines: vec![
            "Use read to examine files before editing them instead of running bash cat, head, or tail.".to_string(),
        ],
    }
}

fn is_bmp_header(bytes: &[u8]) -> bool {
    bytes.len() >= 14 && &bytes[..2] == b"BM"
}

fn convert_bmp_to_png(bytes: &[u8]) -> Result<Vec<u8>, pi_js::Error> {
    let reader = image::ImageReader::with_format(Cursor::new(bytes), ImageFormat::Bmp);
    let img = reader
        .decode()
        .map_err(|e| pi_js::Error::msg(format!("Failed to decode BMP image: {e}")))?;
    let rgba = img.to_rgba8();
    let mut out = Vec::new();
    let encoder = PngEncoder::new(&mut out);
    encoder
        .write_image(&rgba, img.width(), img.height(), image::ColorType::Rgba8.into())
        .map_err(|e| pi_js::Error::msg(format!("Failed to encode BMP as PNG: {e}")))?;
    Ok(out)
}

fn apply_model_image_resize_options(opts: &mut ResizeImageOptions, src: &ModelImageResizeOptions) {
    if let Some(w) = src.max_width {
        opts.max_width = Some(w as u32);
    }
    if let Some(h) = src.max_height {
        opts.max_height = Some(h as u32);
    }
    if let Some(b) = src.max_bytes {
        opts.max_bytes = Some(b as usize);
    }
    if let Some(q) = src.jpeg_quality {
        opts.jpeg_quality = Some(q as u8);
    }
}

fn resolve_resize_options(
    ctx: &crate::core::extensions::types::ExtensionToolContext,
    tool_resize_options: Option<&ModelImageResizeOptions>,
) -> ResizeImageOptions {
    let mut opts = ResizeImageOptions::default();
    if let Ok(Some(model)) = ctx.model()
        && let Some(limits) = model.input_limits
        && let Some(model_resize) = limits.images.and_then(|img| img.resize)
    {
        apply_model_image_resize_options(&mut opts, &model_resize);
    }
    if let Some(explicit) = tool_resize_options {
        apply_model_image_resize_options(&mut opts, explicit);
    }
    opts
}

async fn finish_image_read(
    bytes: &[u8],
    mime_type: &str,
    conversion_note: Option<&str>,
    auto_resize_images: bool,
    ctx: &crate::core::extensions::types::ExtensionToolContext,
    tool_resize_options: Option<&ModelImageResizeOptions>,
) -> Result<AgentToolResult, pi_js::Error> {
    let encoded = pi_js::b64::encode(bytes);
    let mut notes = Vec::new();
    if let Some(conv) = conversion_note {
        notes.push(conv.to_string());
    }

    let (final_data, final_mime, omitted) = if auto_resize_images {
        let resize_opts = resolve_resize_options(ctx, tool_resize_options);
        let resized = if let Some(func) = RESIZE_IMAGE_SLOT.get() {
            let raw_img = ImageContent::new(encoded.clone(), mime_type);
            func(raw_img, resize_opts).await?
        } else {
            real_resize_image(bytes, mime_type, Some(resize_opts)).await
        };
        match resized {
            Some(resized) => {
                if let Some(dim) = format_dimension_note(&resized) {
                    notes.push(dim);
                }
                (Some(resized.data), resized.mime_type, false)
            }
            None => (None, mime_type.to_string(), true),
        }
    } else {
        (Some(encoded), mime_type.to_string(), false)
    };

    if omitted {
        notes.push("[Image omitted: image exceeds size limits and could not be resized.]".to_string());
        let text_note = format!("Read image file [{final_mime}]\n{}", notes.join("\n"));
        return Ok(AgentToolResult {
            content: vec![UserContentBlock::Text(TextContent::new(text_note))],
            details: None,
            ..Default::default()
        });
    }

    let data = final_data.unwrap_or_default();
    if let Ok(Some(model)) = ctx.model()
        && !model.input.contains(&ModelModality::Image)
    {
        notes.push("[Current model does not support images. The image will be omitted from this request.]".to_string());
    }

    let text_note = if notes.is_empty() {
        format!("Read image file [{final_mime}]")
    } else {
        format!("Read image file [{final_mime}]\n{}", notes.join("\n"))
    };

    let structured = json!({
        "type": "image",
        "data": data,
        "mimeType": final_mime,
        "note": text_note,
    });

    Ok(AgentToolResult {
        content: vec![
            UserContentBlock::Text(TextContent::new(text_note)),
            UserContentBlock::Image(ImageContent::new(data, final_mime)),
        ],
        details: None,
        structured_content: Some(structured),
        ..Default::default()
    })
}

pub fn create_read_tool_definition(cwd: &str, options: Option<ReadToolOptions>) -> ToolDefinition {
    let default_cwd = cwd.to_string();
    let auto_resize_images = options.as_ref().and_then(|o| o.auto_resize_images).unwrap_or(true);
    let tool_resize_options = options.as_ref().and_then(|o| o.resize_options.clone());
    let ops = options
        .and_then(|o| o.operations)
        .unwrap_or_else(create_local_read_operations);
    let contribution = read_tool_system_prompt_contribution();
    let desc = format!(
        "Read the contents of a file. Supports text files and images (jpg, png, gif, webp). For text files, output is truncated to {} lines or {}KB (whichever is hit first). Use offset and limit for large files.",
        DEFAULT_MAX_LINES,
        DEFAULT_MAX_BYTES / 1024
    );

    build_builtin_tool_definition(BuiltinToolDefSpec {
        name: "read",
        label: "read",
        description: desc,
        prompt_snippet: Some(contribution.prompt_snippet),
        prompt_guidelines: Some(contribution.prompt_guidelines),
        parameters: read_tool_schema(),
        constrained_sampling: strict_prefer_sampling(),
        render_shell: None,
        prepare_arguments: None,
        output_schema: Some(read_tool_output_schema()),
        execute: Arc::new(move |_tool_call_id, params, signal, _on_update, ctx| {
            let ops = Arc::clone(&ops);
            let tool_resize_options = tool_resize_options.clone();
            let effective_cwd = resolve_effective_cwd(&ctx, &default_cwd);
            Box::pin(async move {
                if signal.as_ref().is_some_and(|s| s.aborted()) {
                    return Err(pi_js::Error::msg("Operation aborted"));
                }

                let input: ReadToolInput = serde_json::from_value(params)
                    .map_err(|e| pi_js::Error::msg(format!("Invalid read parameters: {e}")))?;

                let absolute_path = resolve_read_path(&input.path, &effective_cwd);
                ops.access(&absolute_path).await?;

                if signal.as_ref().is_some_and(|s| s.aborted()) {
                    return Err(pi_js::Error::msg("Operation aborted"));
                }

                let detected_mime = ops.detect_image_mime_type(&absolute_path).await?;

                if let Some(mime_type) = detected_mime {
                    let buffer = ops.read_file(&absolute_path).await?;
                    if signal.as_ref().is_some_and(|s| s.aborted()) {
                        return Err(pi_js::Error::msg("Operation aborted"));
                    }
                    if mime_type == "image/bmp"
                        && let Ok(png_bytes) = convert_bmp_to_png(&buffer)
                    {
                        return finish_image_read(
                            &png_bytes,
                            "image/png",
                            Some("[Image converted from image/bmp to image/png.]"),
                            auto_resize_images,
                            &ctx,
                            tool_resize_options.as_ref(),
                        )
                        .await;
                    }
                    return finish_image_read(
                        &buffer,
                        &mime_type,
                        None,
                        auto_resize_images,
                        &ctx,
                        tool_resize_options.as_ref(),
                    )
                    .await;
                }

                let is_streamable = input.offset.is_none_or(|o| o.fract() == 0.0)
                    && input.limit.is_none_or(|l| l.fract() == 0.0 && l >= 0.0);
                let offset_val = input.offset.map(|o| o.floor().max(1.0) as usize).unwrap_or(1);
                let start_line = offset_val - 1;

                if is_streamable && let Some(stream_fut) = ops.create_read_stream(&absolute_path, signal.clone()) {
                    let mut stream = Some(stream_fut.await?);
                    // Check first chunk for BMP header before streaming lines as UTF-8 text.
                    let mut first_chunk_checked = false;
                    let mut buffered_first: Option<Vec<u8>> = None;
                    while let Some(chunk_res) = stream.as_mut().expect("stream active").next().await {
                        let chunk = chunk_res?;
                        if !chunk.is_empty() {
                            if is_bmp_header(&chunk) {
                                drop(stream.take());
                                let buffer = ops.read_file(&absolute_path).await?;
                                if signal.as_ref().is_some_and(|s| s.aborted()) {
                                    return Err(pi_js::Error::msg("Operation aborted"));
                                }
                                if let Ok(png_bytes) = convert_bmp_to_png(&buffer) {
                                    return finish_image_read(
                                        &png_bytes,
                                        "image/png",
                                        Some("[Image converted from image/bmp to image/png.]"),
                                        auto_resize_images,
                                        &ctx,
                                        tool_resize_options.as_ref(),
                                    )
                                    .await;
                                }
                            }
                            first_chunk_checked = true;
                            buffered_first = Some(chunk);
                            break;
                        }
                    }

                    let selected = select_lines_from_stream(
                        buffered_first,
                        if first_chunk_checked { stream } else { None },
                        start_line,
                        input.limit.map(|l| l.floor().max(0.0) as usize),
                        signal.clone(),
                    )
                    .await?;
                    if signal.as_ref().is_some_and(|s| s.aborted()) {
                        return Err(pi_js::Error::msg("Operation aborted"));
                    }
                    return format_text_read_result(
                        &input.path,
                        start_line,
                        input.limit.map(|l| l.floor().max(0.0) as usize),
                        offset_val,
                        selected,
                    );
                }

                let buffer = ops.read_file(&absolute_path).await?;
                if signal.as_ref().is_some_and(|s| s.aborted()) {
                    return Err(pi_js::Error::msg("Operation aborted"));
                }

                if is_bmp_header(&buffer)
                    && let Ok(png_bytes) = convert_bmp_to_png(&buffer)
                {
                    return finish_image_read(
                        &png_bytes,
                        "image/png",
                        Some("[Image converted from image/bmp to image/png.]"),
                        auto_resize_images,
                        &ctx,
                        tool_resize_options.as_ref(),
                    )
                    .await;
                }

                let text_content = String::from_utf8_lossy(&buffer).into_owned();
                let all_lines: Vec<&str> = text_content.split('\n').collect();
                let total_file_lines = all_lines.len();
                if start_line >= total_file_lines {
                    return Err(pi_js::Error::msg(format!(
                        "Offset {offset_val} is beyond end of file ({total_file_lines} lines total)"
                    )));
                }

                let limit_opt = input.limit.map(|l| l.floor().max(0.0) as usize);
                let selected_lines = if let Some(limit_val) = limit_opt {
                    let end_line = (start_line + limit_val).min(total_file_lines);
                    &all_lines[start_line..end_line]
                } else {
                    &all_lines[start_line..]
                };

                let selected_content = selected_lines.join("\n");
                let truncation = truncate_head(&selected_content, None);
                let first_line_bytes = if truncation.first_line_exceeds_limit {
                    all_lines[start_line].len()
                } else {
                    0
                };
                format_text_read_result(
                    &input.path,
                    start_line,
                    limit_opt,
                    offset_val,
                    SelectedReadLines {
                        total_file_lines,
                        truncation,
                        first_line_bytes,
                    },
                )
            })
        }),
        render_call: Some(renderers::read::render_call()),
        render_result: Some(renderers::read::render_result()),
    })
}

struct SelectedReadLines {
    total_file_lines: usize,
    truncation: TruncationResult,
    first_line_bytes: usize,
}

async fn select_lines_from_stream(
    first_chunk: Option<Vec<u8>>,
    mut stream: Option<ReadByteStream>,
    start_line: usize,
    limit: Option<usize>,
    signal: Option<pi_js::abort::AbortSignal>,
) -> pi_js::Result<SelectedReadLines> {
    let end_line = limit.map(|l| start_line.saturating_add(l));
    let mut decoder = Utf8StreamDecoder::with_options(TextDecoderOptions {
        fatal: false,
        ignore_bom: true,
    });
    let mut stored_lines: Vec<String> = Vec::new();
    let mut stored_bytes = 0usize;
    let mut stored_past_caps = false;
    let mut line = 0usize;
    let mut selected_lines = 0usize;
    let mut selected_bytes = 0usize;
    let mut first_line_bytes = 0usize;
    let mut last_line_empty = true;
    let mut pieces: Option<Vec<String>> = Some(Vec::new());
    let mut line_bytes = 0usize;
    let mut line_has_text = false;

    let mut feed_text = |text: &str| {
        let bytes = text.as_bytes();
        let mut position = 0usize;
        let mut selection_start: Option<usize> = None;
        loop {
            let selected = line >= start_line && end_line.is_none_or(|e| line < e);
            if selected && selection_start.is_none() {
                selection_start = Some(position);
            }
            let newline = text[position..].find('\n').map(|rel| position + rel);
            let end = newline.unwrap_or(text.len());
            if selected && end > position {
                let piece = &text[position..end];
                line_has_text = true;
                if let Some(ref mut p) = pieces {
                    if line_bytes <= DEFAULT_MAX_BYTES {
                        p.push(piece.to_string());
                    }
                    line_bytes += piece.len();
                }
            }
            let Some(nl) = newline else {
                break;
            };
            if selected {
                if let Some(p) = pieces.take() {
                    if selected_lines == 0 {
                        first_line_bytes = line_bytes;
                    }
                    if stored_lines.len() > DEFAULT_MAX_LINES || stored_bytes > DEFAULT_MAX_BYTES {
                        stored_past_caps = true;
                    }
                    let joined = p.concat();
                    stored_bytes += joined.len() + usize::from(!stored_lines.is_empty());
                    stored_lines.push(joined);
                }
                selected_lines += 1;
                last_line_empty = !line_has_text;
                pieces = if stored_past_caps { None } else { Some(Vec::new()) };
                line_bytes = 0;
                line_has_text = false;
                if end_line == Some(line + 1)
                    && let Some(sel_start) = selection_start.take()
                {
                    selected_bytes += bytes[sel_start..nl].len();
                }
            }
            line += 1;
            position = nl + 1;
        }
        if let Some(sel_start) = selection_start {
            selected_bytes += bytes[sel_start..].len();
        }
    };

    if let Some(first) = first_chunk {
        let decoded = decoder.decode(&first, true);
        if !decoded.is_empty() {
            feed_text(&decoded);
        }
    }
    if let Some(ref mut s) = stream {
        while let Some(chunk_res) = s.next().await {
            if signal.as_ref().is_some_and(|sig| sig.aborted()) {
                return Err(pi_js::Error::msg("Operation aborted"));
            }
            let chunk = chunk_res?;
            let decoded = decoder.decode(&chunk, true);
            if !decoded.is_empty() {
                feed_text(&decoded);
            }
        }
    }
    let tail = decoder.decode(&[], false);
    if !tail.is_empty() {
        feed_text(&tail);
    }
    if line >= start_line && end_line.is_none_or(|e| line < e) {
        if let Some(p) = pieces.take() {
            if selected_lines == 0 {
                first_line_bytes = line_bytes;
            }
            stored_lines.push(p.concat());
        }
        selected_lines += 1;
        last_line_empty = !line_has_text;
    }

    let mut truncation = truncate_head(&stored_lines.join("\n"), None);
    truncation.total_lines = if selected_bytes == 0 {
        0
    } else {
        (selected_lines - usize::from(last_line_empty)) as i64
    };
    truncation.total_bytes = selected_bytes as i64;
    Ok(SelectedReadLines {
        total_file_lines: line + 1,
        truncation,
        first_line_bytes,
    })
}

fn format_text_read_result(
    display_path: &str,
    start_line: usize,
    limit: Option<usize>,
    offset_val: usize,
    selected: SelectedReadLines,
) -> pi_js::Result<AgentToolResult> {
    let SelectedReadLines {
        total_file_lines,
        truncation,
        first_line_bytes,
    } = selected;
    if start_line >= total_file_lines {
        return Err(pi_js::Error::msg(format!(
            "Offset {offset_val} is beyond end of file ({total_file_lines} lines total)"
        )));
    }
    let user_limited_lines = limit.map(|l| (start_line + l).min(total_file_lines) - start_line);
    let start_line_display = start_line + 1;
    let (output_text, details) = if truncation.first_line_exceeds_limit {
        let first_line_size = format_size(first_line_bytes);
        let msg = format!(
            "[Line {start_line_display} is {first_line_size}, exceeds {} limit. Use bash: sed -n '{start_line_display}p' {display_path} | head -c {}]",
            format_size(DEFAULT_MAX_BYTES),
            DEFAULT_MAX_BYTES
        );
        (
            msg,
            Some(ReadToolDetails {
                truncation: Some(truncation),
            }),
        )
    } else if truncation.truncated {
        let output_lines_usize = truncation.output_lines.max(0) as usize;
        let end_line_display = start_line_display + output_lines_usize.saturating_sub(1);
        let next_offset = end_line_display + 1;
        let mut text = truncation.content.clone();
        if truncation.truncated_by == Some(TruncatedBy::Lines) {
            text.push_str(&format!(
                "\n\n[Showing lines {start_line_display}-{end_line_display} of {total_file_lines}. Use offset={next_offset} to continue.]"
            ));
        } else {
            text.push_str(&format!(
                "\n\n[Showing lines {start_line_display}-{end_line_display} of {total_file_lines} ({} limit). Use offset={next_offset} to continue.]",
                format_size(DEFAULT_MAX_BYTES)
            ));
        }
        (
            text,
            Some(ReadToolDetails {
                truncation: Some(truncation),
            }),
        )
    } else if let Some(limited_len) = user_limited_lines
        && start_line + limited_len < total_file_lines
    {
        let remaining = total_file_lines - (start_line + limited_len);
        let next_offset = start_line + limited_len + 1;
        let text = format!(
            "{}\n\n[{remaining} more lines in file. Use offset={next_offset} to continue.]",
            truncation.content
        );
        (text, None)
    } else {
        (truncation.content, None)
    };

    let details_val = details.and_then(|d| serde_json::to_value(&d).ok());

    Ok(AgentToolResult {
        content: vec![UserContentBlock::Text(TextContent::new(output_text.clone()))],
        details: details_val,
        structured_content: Some(Value::String(output_text)),
        ..Default::default()
    })
}

pub fn create_read_tool(cwd: &str, options: Option<ReadToolOptions>) -> AgentTool {
    wrap_tool_definition(create_read_tool_definition(cwd, options), None)
}
