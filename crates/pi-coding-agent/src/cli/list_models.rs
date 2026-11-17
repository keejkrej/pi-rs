//! Port of packages/coding-agent/src/cli/list-models.ts

//! List available models with optional fuzzy search

use std::sync::Arc;

use pi_ai::{Model, ModelInputType};
use pi_js::abort::AbortSignal;
use pi_tui::fuzzy::fuzzy_filter;

use crate::core::auth_guidance::format_no_models_available_message;
use crate::core::model_runtime::ModelRuntime;
use crate::vendor::chalk;

/// Format a number as human-readable (e.g., 200000 -> "200K", 1000000 -> "1M")
fn format_token_count(count: i64) -> String {
    if count >= 1_000_000 {
        let millions = count as f64 / 1_000_000.0;
        if count % 1_000_000 == 0 {
            format!("{millions:.0}M")
        } else {
            format!("{}M", pi_js::num::to_fixed(millions, 1))
        }
    } else if count >= 1_000 {
        let thousands = count as f64 / 1_000.0;
        if count % 1_000 == 0 {
            format!("{thousands:.0}K")
        } else {
            format!("{}K", pi_js::num::to_fixed(thousands, 1))
        }
    } else {
        count.to_string()
    }
}

struct ModelRow {
    provider: String,
    model: String,
    context: String,
    max_out: String,
    thinking: &'static str,
    images: &'static str,
}

/// List available models, optionally filtered by search pattern
pub async fn list_models(
    model_runtime: &ModelRuntime,
    search_pattern: Option<&str>,
    signal: Option<AbortSignal>,
) -> pi_js::Result<()> {
    if let Some(load_error) = model_runtime.get_error() {
        pi_js::console::error(&chalk::yellow(&format!(
            "Warning: errors loading models.json:\n{load_error}"
        )));
    }

    let models = model_runtime.get_available(None, signal).await?;
    if models.is_empty() {
        pi_js::console::log(&format_no_models_available_message());
        return Ok(());
    }

    let mut filtered_models: Vec<Model> = match search_pattern {
        Some(pattern) if !pattern.is_empty() => fuzzy_filter(
            &models,
            pattern,
            Arc::new(|m: &Model| format!("{} {}", m.provider, m.id)),
        ),
        _ => models,
    };

    if filtered_models.is_empty() {
        let pattern = search_pattern.unwrap_or("");
        pi_js::console::log(&format!("No models matching \"{pattern}\""));
        return Ok(());
    }

    filtered_models.sort_by(|a, b| {
        pi_js::intl::locale_compare(&a.provider, &b.provider).then_with(|| pi_js::intl::locale_compare(&a.id, &b.id))
    });

    let rows: Vec<ModelRow> = filtered_models
        .iter()
        .map(|m| ModelRow {
            provider: m.provider.clone(),
            model: m.id.clone(),
            context: format_token_count(m.context_window),
            max_out: format_token_count(m.max_tokens),
            thinking: if m.reasoning { "yes" } else { "no" },
            images: if m.input.contains(&ModelInputType::Image) {
                "yes"
            } else {
                "no"
            },
        })
        .collect();

    let hdr_provider = "provider";
    let hdr_model = "model";
    let hdr_context = "context";
    let hdr_max_out = "max-out";
    let hdr_thinking = "thinking";
    let hdr_images = "images";

    let w_provider = rows
        .iter()
        .map(|r| r.provider.len())
        .max()
        .unwrap_or(0)
        .max(hdr_provider.len());
    let w_model = rows
        .iter()
        .map(|r| r.model.len())
        .max()
        .unwrap_or(0)
        .max(hdr_model.len());
    let w_context = rows
        .iter()
        .map(|r| r.context.len())
        .max()
        .unwrap_or(0)
        .max(hdr_context.len());
    let w_max_out = rows
        .iter()
        .map(|r| r.max_out.len())
        .max()
        .unwrap_or(0)
        .max(hdr_max_out.len());
    let w_thinking = rows
        .iter()
        .map(|r| r.thinking.len())
        .max()
        .unwrap_or(0)
        .max(hdr_thinking.len());
    let w_images = rows
        .iter()
        .map(|r| r.images.len())
        .max()
        .unwrap_or(0)
        .max(hdr_images.len());

    let header_line = [
        pi_js::str16::pad_end(hdr_provider, w_provider, " "),
        pi_js::str16::pad_end(hdr_model, w_model, " "),
        pi_js::str16::pad_end(hdr_context, w_context, " "),
        pi_js::str16::pad_end(hdr_max_out, w_max_out, " "),
        pi_js::str16::pad_end(hdr_thinking, w_thinking, " "),
        pi_js::str16::pad_end(hdr_images, w_images, " "),
    ]
    .join("  ");
    pi_js::console::log(&header_line);

    for row in &rows {
        let line = [
            pi_js::str16::pad_end(&row.provider, w_provider, " "),
            pi_js::str16::pad_end(&row.model, w_model, " "),
            pi_js::str16::pad_end(&row.context, w_context, " "),
            pi_js::str16::pad_end(&row.max_out, w_max_out, " "),
            pi_js::str16::pad_end(row.thinking, w_thinking, " "),
            pi_js::str16::pad_end(row.images, w_images, " "),
        ]
        .join("  ");
        pi_js::console::log(&line);
    }

    Ok(())
}
