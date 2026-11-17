//! Port of packages/coding-agent/src/modes/interactive/components/visual-truncate.ts

use std::sync::{Arc, Mutex};

use pi_tui::{Component, Text, TuiMouseEvent, TuiMouseEventResult, truncate_to_width};

/// Which visual lines to keep when truncating.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VisualTruncateKeep {
    Start,
    #[default]
    End,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisualTruncateResult {
    /// The visual lines to display.
    pub visual_lines: Vec<String>,
    /// Number of visual lines that were skipped (hidden).
    pub skipped_count: usize,
}

/// Truncate text to a maximum number of visual lines.
/// This accounts for line wrapping based on terminal width.
pub fn truncate_to_visual_lines(
    text: &str,
    max_visual_lines: usize,
    width: usize,
    padding_x: Option<i64>,
    keep: Option<VisualTruncateKeep>,
) -> VisualTruncateResult {
    if text.is_empty() {
        return VisualTruncateResult {
            visual_lines: Vec::new(),
            skipped_count: 0,
        };
    }

    let padding_x = padding_x.unwrap_or(0);
    let keep = keep.unwrap_or_default();
    let temp_text = Text::new(Some(text), Some(padding_x), Some(0), None);
    let all_visual_lines = temp_text.render(width);

    if all_visual_lines.len() <= max_visual_lines {
        return VisualTruncateResult {
            visual_lines: all_visual_lines,
            skipped_count: 0,
        };
    }

    let skipped_count = all_visual_lines.len() - max_visual_lines;
    let truncated_lines = match keep {
        VisualTruncateKeep::Start => all_visual_lines[..max_visual_lines].to_vec(),
        VisualTruncateKeep::End => all_visual_lines[skipped_count..].to_vec(),
    };

    VisualTruncateResult {
        visual_lines: truncated_lines,
        skipped_count,
    }
}

#[derive(Clone)]
pub struct VisualLinePreviewOptions {
    /// Styled text; may contain newlines.
    pub text: String,
    pub max_visual_lines: usize,
    /// Which visual lines to keep. The hint goes before kept end lines and after kept start lines.
    pub keep: VisualTruncateKeep,
    /// Styled hint line for the given number of hidden visual lines.
    pub format_hint: Arc<dyn Fn(usize) -> String + Send + Sync>,
}

struct VisualLinePreviewState {
    options: VisualLinePreviewOptions,
    cached_width: Option<usize>,
    cached_lines: Option<Vec<String>>,
}

/// Collapsed tool output limited to a number of visual lines, like bash output. Limiting logical
/// lines instead lets a single long line (such as minified JSON) wrap across the whole screen.
/// Caches its lines per width, since it renders on every frame for every result in the transcript.
#[derive(Clone)]
pub struct VisualLinePreview {
    state: Arc<Mutex<VisualLinePreviewState>>,
}

impl VisualLinePreview {
    pub fn new(options: VisualLinePreviewOptions) -> Self {
        Self {
            state: Arc::new(Mutex::new(VisualLinePreviewState {
                options,
                cached_width: None,
                cached_lines: None,
            })),
        }
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let (options, cached) = {
            let state = self.state.lock().unwrap();
            if let Some(ref lines) = state.cached_lines
                && state.cached_width == Some(width)
            {
                return lines.clone();
            }
            (state.options.clone(), state.cached_lines.clone())
        };
        let _ = cached;

        let preview = truncate_to_visual_lines(
            &options.text,
            options.max_visual_lines,
            width,
            Some(0),
            Some(options.keep),
        );
        let mut lines = preview.visual_lines;
        let result = if preview.skipped_count > 0 {
            let hint_raw = (options.format_hint)(preview.skipped_count);
            let hint = truncate_to_width(&hint_raw, width as i64, Some("..."), None);
            match options.keep {
                VisualTruncateKeep::Start => {
                    lines.push(hint);
                    lines
                }
                VisualTruncateKeep::End => {
                    let mut out = Vec::with_capacity(lines.len() + 1);
                    out.push(hint);
                    out.extend(lines);
                    out
                }
            }
        } else {
            lines
        };

        let mut state = self.state.lock().unwrap();
        state.cached_width = Some(width);
        state.cached_lines = Some(result.clone());
        result
    }

    pub fn invalidate(&self) {
        let mut state = self.state.lock().unwrap();
        state.cached_width = None;
        state.cached_lines = None;
    }
}

impl Component for VisualLinePreview {
    fn render(&self, width: usize) -> Vec<String> {
        VisualLinePreview::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        VisualLinePreview::invalidate(self)
    }
}
