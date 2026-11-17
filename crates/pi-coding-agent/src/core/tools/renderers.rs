//! Port of packages/coding-agent/src/core/tools/renderers/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod bash;
pub mod edit;
pub mod find;
pub mod grep;
pub mod ls;
pub mod read;
pub mod write;
// @generated-mods end

use std::sync::Arc;

use pi_ai::types::UserContentBlock;
use pi_tui::{Component, Text};

use crate::core::extensions::types::ToolRenderContext;
use crate::core::tools::render_utils::get_text_output;

pub(crate) fn extract_text_output(content: &[UserContentBlock], show_images: bool) -> String {
    let vals: Vec<serde_json::Value> = content.iter().filter_map(|b| serde_json::to_value(b).ok()).collect();
    get_text_output(&vals, show_images)
}

/// Reuse or initialize a `pi_tui::Text` component stored inside `context.state`.
/// Avoids downcasting `dyn Component` and reuses cached component allocations across renders.
pub(crate) fn reuse_or_init_text(
    context: &ToolRenderContext,
    content: &str,
    padding_x: i64,
    padding_y: i64,
) -> Arc<dyn Component> {
    if let Ok(mut guard) = context.state.lock() {
        if let Some(existing) = guard.as_mut().and_then(|any| any.downcast_mut::<Text>()) {
            existing.set_text(content);
            return Arc::new(existing.clone());
        }
        let widget = Text::new(Some(content), Some(padding_x), Some(padding_y), None);
        *guard = Some(Box::new(widget.clone()));
        return Arc::new(widget);
    }
    Arc::new(Text::new(Some(content), Some(padding_x), Some(padding_y), None))
}

/// Preview-truncate lines cleanly when collapsed, appending a dim `key_hint` footer.
pub(crate) fn format_preview_lines(
    lines: &[String],
    max_collapsed_lines: usize,
    expanded: bool,
    expand_hint: &str,
) -> String {
    if expanded || lines.len() <= max_collapsed_lines {
        return lines.join("\n");
    }
    let shown = &lines[..max_collapsed_lines];
    let remaining = lines.len() - max_collapsed_lines;
    format!("{}\n... ({remaining} more lines, {expand_hint})", shown.join("\n"))
}
