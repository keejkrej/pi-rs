//! Port of packages/coding-agent/src/core/tools/renderers/write.ts

use std::sync::Arc;

use pi_tui::Text;
use serde_json::Value;

use crate::core::extensions::types::{RenderCallFn, RenderResultFn, ToolRenderContext};
use crate::core::tools::render_utils::{normalize_display_text, replace_tabs, shorten_path, str_val};
use crate::core::tools::renderers::extract_text_output;
use crate::core::tools::renderers::reuse_or_init_result_text;
use crate::core::tools::renderers::w3b_wire::{CodeHighlighter, get_language_from_path, highlight_code, key_hint};
use crate::utils::paths::format_path_relative_to_cwd_or_absolute;

const WRITE_PREVIEW_LINES: usize = 10;
const WRITE_PARTIAL_FULL_HIGHLIGHT_LINES: usize = 50;

struct WriteHighlightCache {
    raw_path: String,
    lang: &'static str,
    raw_content: String,
    normalized_lines: Vec<String>,
    highlighted_lines: Vec<String>,
}

struct WriteCallRenderState {
    text: Text,
    cache: Option<WriteHighlightCache>,
    highlighter: CodeHighlighter,
}

fn highlight_single_line(line: &str, lang: &str) -> String {
    highlight_code(line, Some(lang)).into_iter().next().unwrap_or_default()
}

fn refresh_write_highlight_prefix(cache: &mut WriteHighlightCache, highlighter: &CodeHighlighter) {
    let prefix_count = WRITE_PARTIAL_FULL_HIGHLIGHT_LINES.min(cache.normalized_lines.len());
    if prefix_count == 0 {
        return;
    }
    let prefix_source = cache.normalized_lines[..prefix_count].join("\n");
    let prefix_highlighted = highlighter.highlight(&prefix_source, Some(cache.lang));
    for i in 0..prefix_count {
        cache.highlighted_lines[i] = prefix_highlighted
            .get(i)
            .cloned()
            .unwrap_or_else(|| highlight_single_line(&cache.normalized_lines[i], cache.lang));
    }
}

fn rebuild_write_highlight_cache_full(raw_path: &str, file_content: &str) -> Option<WriteHighlightCache> {
    let lang = if raw_path.is_empty() {
        None
    } else {
        get_language_from_path(raw_path)
    }?;
    let normalized = replace_tabs(&normalize_display_text(file_content));
    let normalized_lines: Vec<String> = normalized.split('\n').map(str::to_string).collect();
    let highlighted_lines = highlight_code(&normalized, Some(lang));
    Some(WriteHighlightCache {
        raw_path: raw_path.to_string(),
        lang,
        raw_content: file_content.to_string(),
        normalized_lines,
        highlighted_lines,
    })
}

fn update_write_highlight_cache_incremental(
    cache: Option<WriteHighlightCache>,
    raw_path: &str,
    file_content: &str,
    highlighter: &CodeHighlighter,
) -> Option<WriteHighlightCache> {
    let lang = if raw_path.is_empty() {
        None
    } else {
        get_language_from_path(raw_path)
    }?;
    let Some(mut cache) = cache else {
        return rebuild_write_highlight_cache_full(raw_path, file_content);
    };
    if cache.lang != lang || cache.raw_path != raw_path || !file_content.starts_with(&cache.raw_content) {
        return rebuild_write_highlight_cache_full(raw_path, file_content);
    }
    if file_content.len() == cache.raw_content.len() {
        return Some(cache);
    }
    let delta_raw = &file_content[cache.raw_content.len()..];
    let delta_normalized = replace_tabs(&normalize_display_text(delta_raw));
    cache.raw_content = file_content.to_string();
    if cache.normalized_lines.is_empty() {
        cache.normalized_lines.push(String::new());
        cache.highlighted_lines.push(String::new());
    }
    let mut segments = delta_normalized.split('\n');
    if let Some(first) = segments.next()
        && let Some(last_idx) = cache.normalized_lines.len().checked_sub(1)
    {
        cache.normalized_lines[last_idx].push_str(first);
        cache.highlighted_lines[last_idx] = highlight_single_line(&cache.normalized_lines[last_idx], cache.lang);
    }
    for seg in segments {
        cache.normalized_lines.push(seg.to_string());
        cache.highlighted_lines.push(highlight_single_line(seg, cache.lang));
    }
    refresh_write_highlight_prefix(&mut cache, highlighter);
    Some(cache)
}

pub fn render_call() -> RenderCallFn {
    Arc::new(|args: Value, theme, context: ToolRenderContext| {
        let raw_path = str_val(args.get("file_path"))
            .filter(|s| !s.is_empty())
            .or_else(|| str_val(args.get("path")))
            .unwrap_or_default();
        let display_path = if raw_path.is_empty() {
            if context.args_complete {
                "(invalid path)".to_string()
            } else {
                "...".to_string()
            }
        } else {
            let rel = format_path_relative_to_cwd_or_absolute(&raw_path, &context.cwd);
            shorten_path(Some(&rel))
        };

        let mut text = format!(
            "{} {}",
            theme.fg("toolTitle", &theme.bold("write")),
            theme.fg("accent", &display_path)
        );

        let file_content = str_val(args.get("content"));
        let mut guard = context.state.lock().ok();
        let state = guard.as_mut().map(|g| {
            g.get_or_insert_with(|| {
                Box::new(WriteCallRenderState {
                    text: Text::new(Some(""), Some(0), Some(0), None),
                    cache: None,
                    highlighter: CodeHighlighter::new(),
                })
            })
        });
        let mut render_state = state.and_then(|any| any.downcast_mut::<WriteCallRenderState>());

        if let Some(content) = file_content {
            if !content.is_empty() {
                let lang = if raw_path.is_empty() {
                    None
                } else {
                    get_language_from_path(&raw_path)
                };
                let mut lines: Vec<String> = if let Some(lang) = lang {
                    if let Some(st) = render_state.as_deref_mut() {
                        st.cache = if !context.args_complete {
                            update_write_highlight_cache_incremental(
                                st.cache.take(),
                                &raw_path,
                                &content,
                                &st.highlighter,
                            )
                        } else {
                            None
                        };
                        if let Some(ref c) = st.cache {
                            c.highlighted_lines.clone()
                        } else {
                            let normalized = replace_tabs(&normalize_display_text(&content));
                            (*st.highlighter.highlight(&normalized, Some(lang))).clone()
                        }
                    } else {
                        let normalized = replace_tabs(&normalize_display_text(&content));
                        highlight_code(&normalized, Some(lang))
                    }
                } else {
                    let normalized = replace_tabs(&normalize_display_text(&content));
                    normalized
                        .split('\n')
                        .map(|line| theme.fg("toolOutput", line))
                        .collect()
                };
                while lines.last().is_some_and(String::is_empty) {
                    lines.pop();
                }
                if !lines.is_empty() {
                    let total_lines = lines.len();
                    let max_lines = if context.expanded {
                        lines.len()
                    } else {
                        WRITE_PREVIEW_LINES
                    };
                    let display_count = lines.len().min(max_lines);
                    let remaining = lines.len().saturating_sub(display_count);
                    text.push_str("\n\n");
                    text.push_str(&lines[..display_count].join("\n"));
                    if remaining > 0 {
                        text.push_str(&format!(
                            "{} {}{}",
                            theme.fg("muted", &format!("\n... ({remaining} more lines, {total_lines} total,")),
                            key_hint("expandTools", "to expand"),
                            theme.fg("muted", ")"),
                        ));
                    }
                }
            } else if let Some(st) = render_state.as_deref_mut() {
                st.cache = None;
            }
        } else if args.get("content").is_some_and(|v| !v.is_null() && !v.is_string()) {
            text.push_str(&format!(
                "\n\n{}",
                theme.fg("error", "[invalid content arg - expected string]")
            ));
        }

        if let Some(st) = render_state {
            st.text.set_padding_x(0);
            st.text.set_text(&text);
            return Arc::new(st.text.clone());
        }
        Arc::new(Text::new(Some(&text), Some(0), Some(0), None))
    })
}

pub fn render_result() -> RenderResultFn {
    Arc::new(|result, _options, theme, context: ToolRenderContext| {
        if !context.is_error {
            return Arc::new(pi_tui::Container::new());
        }
        let msg = extract_text_output(&result.content, context.show_images);
        if msg.is_empty() {
            return Arc::new(pi_tui::Container::new());
        }
        let formatted = format!("\n{}", theme.fg("error", &msg));
        reuse_or_init_result_text(&context, &formatted, 0, 0)
    })
}
