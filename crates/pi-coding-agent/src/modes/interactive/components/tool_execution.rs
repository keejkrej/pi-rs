//! Port of packages/coding-agent/src/modes/interactive/components/tool-execution.ts

use std::any::Any;
use std::io::Cursor;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, LazyLock, Mutex};

use pi_agent_core::AgentToolResult;
use pi_ai::UserContentBlock;
use pi_tui::components::box_::TuiBox;
use pi_tui::components::image::{Image, ImageOptions, ImageTheme, set_image_transcoder};
use pi_tui::{
    Component, Container, ImageProtocol, MouseRegion, Spacer, TUI, Text, TuiMouseButton, TuiMouseEvent,
    TuiMouseEventResult, TuiMouseEventType, get_capabilities, get_image_dimensions, image_fallback,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::super::theme::theme::{Theme, ThemeBg, ThemeColor, theme};
use super::keybinding_hints::{key_hint, key_text};

const FALLBACK_PREVIEW_LINES: usize = 10;
const COLLAPSED_ARGS_CHARS: usize = 100;

// WIRE(@tui-w3): Replace local mirror types (`RenderShell`, `ToolRenderResultOptions`, `ToolExecutionRenderContext`, `ToolRenderers`) with `crate::core::extensions::types::{RenderShell, ToolRenderContext, ToolRenderResultOptions, ToolRenderers}` (once `duration_ms`, `output_pad`, `key_text`, `key_hint` land on `ToolRenderContext` and `@codemode` merges onto main).

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderShell {
    #[default]
    Default,
    #[serde(rename = "self")]
    Self_,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolRenderResultOptions {
    pub expanded: bool,
    pub is_partial: bool,
}

#[derive(Clone)]
pub struct ToolExecutionRenderContext {
    pub args: Value,
    pub tool_call_id: String,
    pub invalidate: Arc<dyn Fn() + Send + Sync>,
    pub last_component: Option<Arc<dyn Component>>,
    pub state: Arc<Mutex<Option<Box<dyn Any + Send>>>>,
    pub cwd: String,
    pub execution_started: bool,
    pub args_complete: bool,
    pub is_partial: bool,
    pub expanded: bool,
    pub show_images: bool,
    pub is_error: bool,
    pub duration_ms: Option<f64>,
    pub output_pad: i64,
    pub key_text: Arc<dyn Fn(&str) -> String + Send + Sync>,
    pub key_hint: Arc<dyn Fn(&str, &str) -> String + Send + Sync>,
}

pub type RenderCallFn = Arc<dyn Fn(Value, Theme, ToolExecutionRenderContext) -> Arc<dyn Component> + Send + Sync>;
pub type RenderResultFn = Arc<
    dyn Fn(AgentToolResult, ToolRenderResultOptions, Theme, ToolExecutionRenderContext) -> Arc<dyn Component>
        + Send
        + Sync,
>;

/// What this component needs from a tool: how to draw it, without executing it.
#[derive(Clone, Default)]
pub struct ToolRenderers {
    pub render_shell: Option<RenderShell>,
    pub render_call: Option<RenderCallFn>,
    pub render_result: Option<RenderResultFn>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ToolExecutionOptions {
    pub show_images: Option<bool>,
    pub image_width_cells: Option<i64>,
    pub output_pad: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionResult {
    pub content: Vec<UserContentBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
}

impl From<AgentToolResult> for ToolExecutionResult {
    fn from(result: AgentToolResult) -> Self {
        Self {
            content: result.content,
            details: result.details,
            is_error: result.is_error.unwrap_or(false),
            duration_ms: None,
        }
    }
}

// ---------------------------------------------------------------------------
// WIRE(@tui-w3): Local fallback helpers mirroring utils/ansi, utils/shell,
// utils/image_convert, and core/tools/render_utils until @mcp / @codemode merge.
// ---------------------------------------------------------------------------

static ANSI_REGEX: LazyLock<Option<fancy_regex::Regex>> = LazyLock::new(|| {
    let st = r"(?:\x07|\x1b\\|\u{009c})";
    let osc = format!(r"(?:\x1b\][\s\S]*?{st})");
    let csi = r"[\x1b\u{009b}][[\]()#;?]*(?:\d{1,4}(?:[;:]\d{0,4})*)?[\dA-PR-TZcf-nq-uy=><~]";
    fancy_regex::Regex::new(&format!("{osc}|{csi}")).ok()
});

// WIRE(@tui-w3): Replace with `crate::utils::ansi::strip_ansi` once @mcp merges onto main.
fn strip_ansi_local(value: &str) -> String {
    if !value.contains('\u{001B}') && !value.contains('\u{009B}') {
        return value.to_owned();
    }
    match ANSI_REGEX.as_ref() {
        Some(re) => re.replace_all(value, "").into_owned(),
        None => pi_tui::strip_terminal_sequences(value),
    }
}

// WIRE(@tui-w3): Replace with `crate::utils::shell::sanitize_binary_output` once @mcp merges onto main.
fn sanitize_binary_output_local(s: &str) -> String {
    s.chars()
        .filter(|&c| {
            let code = c as u32;
            !((code <= 0x08)
                || code == 0x0B
                || code == 0x0C
                || (0x0E..=0x1F).contains(&code)
                || (0xFFF9..=0xFFFB).contains(&code))
        })
        .collect()
}

fn replace_tabs(text: &str) -> String {
    text.replace('\t', "   ")
}

// WIRE(@tui-w3): Replace with `crate::core::tools::render_utils::get_text_output` once @codemode merges onto main.
pub fn get_text_output(result: Option<&ToolExecutionResult>, show_images: bool) -> String {
    let Some(res) = result else {
        return String::new();
    };

    let mut text_parts: Vec<String> = Vec::new();
    let mut image_parts: Vec<(&str, &str)> = Vec::new();

    for block in &res.content {
        match block {
            UserContentBlock::Text(text) => {
                let clean = sanitize_binary_output_local(&strip_ansi_local(&text.text)).replace('\r', "");
                text_parts.push(clean);
            }
            UserContentBlock::Image(img) => {
                image_parts.push((&img.data, &img.mime_type));
            }
        }
    }

    let mut output = text_parts.join("\n");
    let caps = get_capabilities();
    if !image_parts.is_empty() && (caps.images.is_none() || !show_images) {
        let indicators: Vec<String> = image_parts
            .into_iter()
            .map(|(data, mime)| {
                let dims = if !data.is_empty() && !mime.is_empty() {
                    get_image_dimensions(data, mime)
                } else {
                    None
                };
                let mime_label = if mime.is_empty() { "image/unknown" } else { mime };
                image_fallback(mime_label, dims, None)
            })
            .collect();
        let joined = indicators.join("\n");
        if output.is_empty() {
            output = joined;
        } else {
            output = format!("{output}\n{joined}");
        }
    }
    output
}

// WIRE(@tui-w3): Replace with `crate::core::tools::render_utils::format_tool_call_with_args` once @codemode merges onto main.
/// Generic tool call header: the title followed by the arguments. Collapsed, they are `key=value`
/// pairs on the title line, cut to `COLLAPSED_ARGS_CHARS`. Expanded, each is a `key: value`
/// line below the title, with strings shown raw and continuation lines indented.
pub fn format_tool_call_with_args(title: &str, args: Option<&Value>, theme: &Theme, expanded: bool) -> String {
    let header = theme.fg(super::super::theme::theme::ThemeColor::ToolTitle, &theme.bold(title));
    let Some(args_val) = args else {
        return header;
    };
    if args_val.is_null() {
        return header;
    }
    let entries: Vec<(String, &Value)> = match args_val {
        Value::Object(map) => map.iter().map(|(k, v)| (k.clone(), v)).collect(),
        other => vec![("args".to_string(), other)],
    };
    if entries.is_empty() {
        return header;
    }
    if expanded {
        let lines: Vec<String> = entries
            .into_iter()
            .map(|(key, value)| {
                let text = match value {
                    Value::String(s) => s.clone(),
                    other => pi_js::json::stringify_pretty(other, "  "),
                };
                let indented = replace_tabs(&text).replace('\r', "").replace('\n', "\n    ");
                format!("  {key}: {indented}")
            })
            .collect();
        return format!(
            "{header}\n{}",
            theme.fg(super::super::theme::theme::ThemeColor::Muted, &lines.join("\n"))
        );
    }
    let pairs: String = entries
        .into_iter()
        .map(|(key, value)| format!("{key}={}", pi_js::json::stringify(value)))
        .collect::<Vec<_>>()
        .join(" ");
    let preview = if pi_js::str16::len(&pairs) > COLLAPSED_ARGS_CHARS {
        format!(
            "{}...",
            pi_js::str16::slice(&pairs, 0, Some((COLLAPSED_ARGS_CHARS - 3) as i64))
        )
    } else {
        pairs
    };
    format!(
        "{header} {}",
        theme.fg(super::super::theme::theme::ThemeColor::Muted, &preview)
    )
}

fn read_orientation_from_tiff(bytes: &[u8], tiff_start: usize) -> u16 {
    if tiff_start + 8 > bytes.len() {
        return 1;
    }
    let byte_order = (u16::from(bytes[tiff_start]) << 8) | u16::from(bytes[tiff_start + 1]);
    let le = byte_order == 0x4949;
    let read16 = |pos: usize| -> u16 {
        if pos + 2 > bytes.len() {
            return 0;
        }
        if le {
            u16::from_le_bytes([bytes[pos], bytes[pos + 1]])
        } else {
            u16::from_be_bytes([bytes[pos], bytes[pos + 1]])
        }
    };
    let read32 = |pos: usize| -> u32 {
        if pos + 4 > bytes.len() {
            return 0;
        }
        if le {
            u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
        } else {
            u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
        }
    };
    let ifd_offset = read32(tiff_start + 4) as usize;
    let Some(ifd_start) = tiff_start.checked_add(ifd_offset) else {
        return 1;
    };
    if ifd_start + 2 > bytes.len() {
        return 1;
    }
    let entry_count = usize::from(read16(ifd_start));
    for i in 0..entry_count {
        let entry_pos = ifd_start + 2 + i * 12;
        if entry_pos + 12 > bytes.len() {
            return 1;
        }
        if read16(entry_pos) == 0x0112 {
            let value = read16(entry_pos + 8);
            return if (1..=8).contains(&value) { value } else { 1 };
        }
    }
    1
}

fn find_jpeg_tiff_offset(bytes: &[u8]) -> Option<usize> {
    let mut offset = 2usize;
    while offset < bytes.len().saturating_sub(1) {
        if bytes[offset] != 0xff {
            return None;
        }
        let marker = bytes[offset + 1];
        if marker == 0xff {
            offset += 1;
            continue;
        }
        if marker == 0xe1 {
            let segment_start = offset + 4;
            if segment_start + 6 <= bytes.len()
                && bytes.get(segment_start..segment_start + 6) == Some(&[0x45, 0x78, 0x69, 0x66, 0x00, 0x00])
            {
                return Some(segment_start + 6);
            }
        }
        if offset + 4 > bytes.len() {
            return None;
        }
        let length = ((usize::from(bytes[offset + 2])) << 8) | usize::from(bytes[offset + 3]);
        offset = offset.checked_add(2 + length)?;
    }
    None
}

fn apply_exif_orientation_local(img: image::DynamicImage, bytes: &[u8]) -> image::DynamicImage {
    let orientation = if bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] == 0xd8 {
        find_jpeg_tiff_offset(bytes)
            .map(|off| read_orientation_from_tiff(bytes, off))
            .unwrap_or(1)
    } else {
        1
    };
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

fn transcode_to_png_local(base64_data: &str, _mime_type: &str) -> Option<String> {
    let bytes = pi_js::b64::decode(base64_data);
    let img = image::load_from_memory(&bytes).ok()?;
    let img = apply_exif_orientation_local(img, &bytes);
    let mut cursor = Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Png).ok()?;
    Some(pi_js::b64::encode(&cursor.into_inner()))
}

#[derive(Default)]
struct PngTranscoderState {
    registered: bool,
    loading: bool,
    callbacks: Vec<Box<dyn FnOnce() + Send + Sync>>,
}

static PNG_TRANSCODER_STATE: LazyLock<Mutex<PngTranscoderState>> =
    LazyLock::new(|| Mutex::new(PngTranscoderState::default()));

// WIRE(@tui-w3): Replace with `crate::utils::image_convert::ensure_png_transcoder` once @mcp implements and merges it onto main.
fn ensure_png_transcoder_local(on_registered: impl FnOnce() + Send + Sync + 'static) {
    if get_capabilities().images != Some(ImageProtocol::Kitty) {
        return;
    }

    let should_start = {
        let mut state = PNG_TRANSCODER_STATE.lock().unwrap_or_else(|e| e.into_inner());
        if state.registered {
            return;
        }
        state.callbacks.push(Box::new(on_registered));
        if state.loading {
            return;
        }
        state.loading = true;
        true
    };
    if !should_start {
        return;
    }

    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            tokio::task::yield_now().await;
            set_image_transcoder(Some(Arc::new(transcode_to_png_local)));
            let callbacks = {
                let mut state = PNG_TRANSCODER_STATE.lock().unwrap_or_else(|e| e.into_inner());
                state.registered = true;
                state.loading = false;
                std::mem::take(&mut state.callbacks)
            };
            for cb in callbacks {
                cb();
            }
        });
    } else {
        set_image_transcoder(Some(Arc::new(transcode_to_png_local)));
        let mut state = PNG_TRANSCODER_STATE.lock().unwrap_or_else(|e| e.into_inner());
        state.registered = true;
        state.loading = false;
        state.callbacks.clear();
    }
}

#[doc(hidden)]
pub mod testing {
    use super::*;

    /// Reset the local PNG transcoder registration state for tests.
    pub fn reset_png_transcoder_state() {
        let mut state = PNG_TRANSCODER_STATE.lock().unwrap_or_else(|e| e.into_inner());
        state.registered = false;
        state.loading = false;
        state.callbacks.clear();
        set_image_transcoder(None);
    }
}

// ---------------------------------------------------------------------------
// ToolExecutionComponent
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Eq)]
struct ImageSource {
    data: String,
    mime_type: String,
    width_cells: i64,
}

struct ToolExecutionState {
    args: Value,
    expanded: bool,
    show_images: bool,
    image_width_cells: i64,
    output_pad: i64,
    is_partial: bool,
    execution_started: bool,
    args_complete: bool,
    /// `update_args()` ran since the last display update; the next render applies it.
    args_pending: bool,
    result: Option<ToolExecutionResult>,
    hide_component: bool,
    self_render_height: i64,
    call_renderer_component: Option<Arc<dyn Component>>,
    result_renderer_component: Option<Arc<dyn Component>>,
    image_components: Vec<Image>,
    /// Exact `Arc<dyn Component>` handles added to `container` so `remove_child` matches by pointer identity.
    image_component_arcs: Vec<Arc<dyn Component>>,
    /// Inputs of `image_components`, so `update_display` can reuse images and keep their converted PNG data.
    image_sources: Vec<ImageSource>,
    image_spacers: Vec<Spacer>,
    image_spacer_arcs: Vec<Arc<dyn Component>>,
}

struct ToolExecutionInner {
    container: Container,
    content_box: TuiBox,
    content_text: Text,
    content_text_region: Arc<dyn Component>,
    self_render_container: Container,
    renderer_state: Arc<Mutex<Option<Box<dyn Any + Send>>>>,
    tool_name: String,
    tool_call_id: String,
    tool_definition: Option<ToolRenderers>,
    ui: Arc<dyn TUI + Send + Sync>,
    cwd: String,
    state: Mutex<ToolExecutionState>,
}

/// Component that renders a tool call and its streaming/final execution result.
#[derive(Clone)]
pub struct ToolExecutionComponent {
    inner: Arc<ToolExecutionInner>,
}

impl ToolExecutionComponent {
    pub fn new(
        tool_name: &str,
        tool_call_id: &str,
        args: Value,
        options: Option<ToolExecutionOptions>,
        tool_definition: Option<ToolRenderers>,
        ui: Arc<dyn TUI + Send + Sync>,
        cwd: &str,
    ) -> Self {
        let opts = options.unwrap_or_default();
        let show_images = opts.show_images.unwrap_or(true);
        let image_width_cells = opts.image_width_cells.unwrap_or(60);
        let output_pad = opts.output_pad.unwrap_or(1);

        let container = Container::new();
        container.add_child(Arc::new(Spacer::new(Some(1))));

        // Always create all shell variants. content_box is used for default renderer-based composition.
        // self_render_container is used when the tool renders its own framing.
        // content_text is reserved for generic fallback rendering when no tool definition exists.
        let pending_bg: Arc<dyn Fn(&str) -> String + Send + Sync> =
            Arc::new(|text: &str| theme().bg(ThemeBg::ToolPendingBg, text));
        let content_box = TuiBox::new(Some(1), Some(1), Some(Arc::clone(&pending_bg)));
        let content_text = Text::new(Some(""), Some(1), Some(1), Some(pending_bg));
        let self_render_container = Container::new();

        let inner = Arc::new_cyclic(|weak| {
            let weak_for_region = std::sync::Weak::clone(weak);
            let content_text_region: Arc<dyn Component> = Arc::new(MouseRegion::new(
                Arc::new(content_text.clone()),
                Arc::new(move |event| Self::handle_result_region_click(&weak_for_region, event)),
            ));
            ToolExecutionInner {
                container: container.clone(),
                content_box: content_box.clone(),
                content_text,
                content_text_region: Arc::clone(&content_text_region),
                self_render_container: self_render_container.clone(),
                renderer_state: Arc::new(Mutex::new(None)),
                tool_name: tool_name.to_string(),
                tool_call_id: tool_call_id.to_string(),
                tool_definition: tool_definition.clone(),
                ui,
                cwd: cwd.to_string(),
                state: Mutex::new(ToolExecutionState {
                    args,
                    expanded: false,
                    show_images,
                    image_width_cells,
                    output_pad,
                    is_partial: true,
                    execution_started: false,
                    args_complete: false,
                    args_pending: false,
                    result: None,
                    hide_component: false,
                    self_render_height: 0,
                    call_renderer_component: None,
                    result_renderer_component: None,
                    image_components: Vec::new(),
                    image_component_arcs: Vec::new(),
                    image_sources: Vec::new(),
                    image_spacers: Vec::new(),
                    image_spacer_arcs: Vec::new(),
                }),
            }
        });

        let comp = Self { inner };
        if comp.has_renderer_definition() {
            if comp.get_render_shell() == RenderShell::Self_ {
                comp.inner.container.add_child(Arc::new(self_render_container));
            } else {
                comp.inner.container.add_child(Arc::new(content_box));
            }
        } else {
            comp.inner
                .container
                .add_child(Arc::clone(&comp.inner.content_text_region));
        }

        comp.update_display();
        comp
    }

    fn handle_result_region_click(
        weak: &std::sync::Weak<ToolExecutionInner>,
        event: &TuiMouseEvent,
    ) -> Option<TuiMouseEventResult> {
        if event.r#type != TuiMouseEventType::Click || event.button != TuiMouseButton::Left {
            return None;
        }
        let inner = weak.upgrade()?;
        let comp = Self { inner };
        let next_expanded = {
            let state = comp.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.result.as_ref()?;
            !state.expanded
        };
        comp.set_expanded(next_expanded);
        Some(TuiMouseEventResult {
            handled: Some(true),
            ..Default::default()
        })
    }

    fn has_renderer_definition(&self) -> bool {
        self.inner.tool_definition.is_some()
    }

    fn get_render_shell(&self) -> RenderShell {
        self.inner
            .tool_definition
            .as_ref()
            .and_then(|d| d.render_shell)
            .unwrap_or(RenderShell::Default)
    }

    fn create_result_region(&self, component: Arc<dyn Component>) -> Arc<dyn Component> {
        let weak = Arc::downgrade(&self.inner);
        Arc::new(MouseRegion::new(
            component,
            Arc::new(move |event| Self::handle_result_region_click(&weak, event)),
        ))
    }

    fn get_render_context(
        &self,
        last_component: Option<Arc<dyn Component>>,
        args: Value,
        execution_started: bool,
        args_complete: bool,
        is_partial: bool,
        expanded: bool,
        show_images: bool,
        is_error: bool,
        duration_ms: Option<f64>,
        output_pad: i64,
    ) -> ToolExecutionRenderContext {
        let weak = Arc::downgrade(&self.inner);
        ToolExecutionRenderContext {
            args,
            tool_call_id: self.inner.tool_call_id.clone(),
            invalidate: Arc::new(move || {
                if let Some(inner) = weak.upgrade() {
                    let comp = Self { inner };
                    comp.invalidate();
                    comp.inner.ui.request_render(None);
                }
            }),
            last_component,
            state: Arc::clone(&self.inner.renderer_state),
            cwd: self.inner.cwd.clone(),
            execution_started,
            args_complete,
            is_partial,
            expanded,
            show_images,
            is_error,
            duration_ms: if is_partial { None } else { duration_ms },
            output_pad,
            key_text: Arc::new(|kb: &str| key_text(&kb.to_string())),
            key_hint: Arc::new(|kb: &str, desc: &str| key_hint(&kb.to_string(), desc)),
        }
    }

    fn create_call_fallback(&self, args: &Value, expanded: bool) -> Arc<dyn Component> {
        let text = format_tool_call_with_args(&self.inner.tool_name, Some(args), &theme(), expanded);
        Arc::new(Text::new(Some(&text), Some(0), Some(0), None))
    }

    fn create_result_fallback(
        &self,
        result: Option<&ToolExecutionResult>,
        show_images: bool,
        expanded: bool,
    ) -> Option<Arc<dyn Component>> {
        let output = get_text_output(result, show_images);
        if output.is_empty() {
            return None;
        }

        let t = theme();
        let lines: Vec<&str> = output.split('\n').collect();
        let display_count = if expanded {
            lines.len()
        } else {
            lines.len().min(FALLBACK_PREVIEW_LINES)
        };
        let display_lines = &lines[..display_count];
        let remaining = lines.len().saturating_sub(display_lines.len());
        let mut text = display_lines
            .iter()
            .map(|line| t.fg(ThemeColor::ToolOutput, line))
            .collect::<Vec<_>>()
            .join("\n");
        if remaining > 0 {
            text.push_str(&format!(
                "{} {}{}",
                t.fg(ThemeColor::Muted, &format!("\n... ({remaining} more lines,")),
                key_hint(&"app.tools.expand".to_string(), "to expand"),
                t.fg(ThemeColor::Muted, ")"),
            ));
        }
        Some(Arc::new(Text::new(Some(&text), Some(0), Some(0), None)))
    }

    /// Show new partial arguments. Applied at the next render, so a call streaming many argument deltas between two
    /// frames runs its renderer once per frame, not once per delta (#9265).
    pub fn update_args(&self, args: Value) {
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.args = args;
            state.args_pending = true;
        }
        self.inner.ui.request_render(None);
    }

    pub fn mark_execution_started(&self) {
        {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .execution_started = true;
        }
        self.update_display();
        self.inner.ui.request_render(None);
    }

    pub fn set_args_complete(&self) {
        {
            self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).args_complete = true;
        }
        self.update_display();
        self.inner.ui.request_render(None);
    }

    pub fn update_result(&self, result: ToolExecutionResult, is_partial: Option<bool>) {
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.result = Some(result);
            state.is_partial = is_partial.unwrap_or(false);
        }
        self.update_display();
    }

    pub fn set_expanded(&self, expanded: bool) {
        {
            self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).expanded = expanded;
        }
        self.update_display();
    }

    pub fn set_output_pad(&self, output_pad: i64) {
        {
            self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).output_pad = output_pad;
        }
        self.update_display();
    }

    pub fn set_show_images(&self, show: bool) {
        {
            self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).show_images = show;
        }
        self.update_display();
    }

    pub fn set_image_width_cells(&self, width: i64) {
        {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .image_width_cells = width.max(1);
        }
        self.update_display();
    }

    pub fn invalidate(&self) {
        self.inner.container.invalidate();
        self.update_display();
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let args_pending = self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).args_pending;
        if args_pending {
            self.update_display();
        }

        let (hide_component, image_spacers, image_components) = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                state.hide_component,
                state.image_spacers.clone(),
                state.image_components.clone(),
            )
        };
        if hide_component {
            return Vec::new();
        }

        if self.has_renderer_definition() && self.get_render_shell() == RenderShell::Self_ {
            let content_lines = self.inner.self_render_container.render(width);
            {
                self.inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .self_render_height = content_lines.len() as i64;
            }
            if content_lines.is_empty() && image_components.is_empty() {
                return Vec::new();
            }

            let mut lines: Vec<String> = Vec::new();
            if !content_lines.is_empty() {
                lines.push(String::new());
                for line in content_lines {
                    lines.push(line);
                }
            }
            for (spacer, image_component) in image_spacers.iter().zip(image_components.iter()) {
                lines.extend(spacer.render(width));
                lines.extend(image_component.render(width));
            }
            return lines;
        }

        self.inner.container.render(width)
    }

    pub fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        if !self.has_renderer_definition() || self.get_render_shell() != RenderShell::Self_ {
            return self.inner.container.handle_mouse(event);
        }
        let self_render_height = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .self_render_height;
        if event.y <= 0 || event.y > self_render_height {
            return None;
        }
        self.inner.self_render_container.handle_mouse(&TuiMouseEvent {
            y: event.y - 1,
            height: self_render_height,
            ..*event
        })
    }

    fn update_display(&self) {
        let (
            args,
            expanded,
            show_images,
            image_width_cells,
            output_pad,
            is_partial,
            execution_started,
            args_complete,
            result,
            prev_call_component,
            prev_result_component,
            previous_images,
            previous_image_arcs,
            previous_sources,
            _previous_spacers,
            previous_spacer_arcs,
        ) = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.args_pending = false;
            (
                state.args.clone(),
                state.expanded,
                state.show_images,
                state.image_width_cells,
                state.output_pad,
                state.is_partial,
                state.execution_started,
                state.args_complete,
                state.result.clone(),
                state.call_renderer_component.clone(),
                state.result_renderer_component.clone(),
                std::mem::take(&mut state.image_components),
                std::mem::take(&mut state.image_component_arcs),
                std::mem::take(&mut state.image_sources),
                std::mem::take(&mut state.image_spacers),
                std::mem::take(&mut state.image_spacer_arcs),
            )
        };

        let is_error = result.as_ref().is_some_and(|r| r.is_error);
        let duration_ms = result.as_ref().and_then(|r| r.duration_ms);
        let bg_token = if is_partial {
            ThemeBg::ToolPendingBg
        } else if is_error {
            ThemeBg::ToolErrorBg
        } else {
            ThemeBg::ToolSuccessBg
        };
        let bg_fn: Arc<dyn Fn(&str) -> String + Send + Sync> = Arc::new(move |text: &str| theme().bg(bg_token, text));

        let mut has_content;
        let mut next_call_component = prev_call_component.clone();
        let mut next_result_component = prev_result_component.clone();

        if let Some(ref tool_def) = self.inner.tool_definition {
            let render_shell = tool_def.render_shell.unwrap_or(RenderShell::Default);
            if render_shell == RenderShell::Default {
                self.inner.content_box.set_bg_fn(Some(bg_fn));
                self.inner.content_box.set_padding_x(output_pad);
                self.inner.content_box.clear();
            } else {
                self.inner.self_render_container.clear();
            }

            let add_child_to_shell = |child: Arc<dyn Component>| {
                if render_shell == RenderShell::Self_ {
                    self.inner.self_render_container.add_child(child);
                } else {
                    self.inner.content_box.add_child(child);
                }
            };

            if let Some(ref call_renderer) = tool_def.render_call {
                let ctx = self.get_render_context(
                    prev_call_component,
                    args.clone(),
                    execution_started,
                    args_complete,
                    is_partial,
                    expanded,
                    show_images,
                    is_error,
                    duration_ms,
                    output_pad,
                );
                let args_c = args.clone();
                let t = theme();
                let rendered = catch_unwind(AssertUnwindSafe(move || (call_renderer)(args_c, t, ctx)));
                match rendered {
                    Ok(component) => {
                        next_call_component = Some(Arc::clone(&component));
                        add_child_to_shell(self.create_result_region(component));
                        has_content = true;
                    }
                    Err(_) => {
                        next_call_component = None;
                        add_child_to_shell(self.create_result_region(self.create_call_fallback(&args, expanded)));
                        has_content = true;
                    }
                }
            } else {
                add_child_to_shell(self.create_result_region(self.create_call_fallback(&args, expanded)));
                has_content = true;
            }

            if let Some(ref res) = result {
                if let Some(ref result_renderer) = tool_def.render_result {
                    let ctx = self.get_render_context(
                        prev_result_component,
                        args.clone(),
                        execution_started,
                        args_complete,
                        is_partial,
                        expanded,
                        show_images,
                        is_error,
                        duration_ms,
                        output_pad,
                    );
                    let agent_res = AgentToolResult {
                        content: res.content.clone(),
                        details: res.details.clone(),
                        structured_content: None,
                        usage: None,
                        is_error: Some(res.is_error),
                        terminate: None,
                        control: None,
                    };
                    let render_opts = ToolRenderResultOptions { expanded, is_partial };
                    let t = theme();
                    let rendered = catch_unwind(AssertUnwindSafe(move || {
                        (result_renderer)(agent_res, render_opts, t, ctx)
                    }));
                    match rendered {
                        Ok(component) => {
                            next_result_component = Some(Arc::clone(&component));
                            add_child_to_shell(self.create_result_region(component));
                            has_content = true;
                        }
                        Err(_) => {
                            next_result_component = None;
                            if let Some(fallback) = self.create_result_fallback(Some(res), show_images, expanded) {
                                add_child_to_shell(self.create_result_region(fallback));
                                has_content = true;
                            }
                        }
                    }
                } else if let Some(fallback) = self.create_result_fallback(Some(res), show_images, expanded) {
                    add_child_to_shell(self.create_result_region(fallback));
                    has_content = true;
                }
            }
        } else {
            self.inner.content_text.set_custom_bg_fn(Some(bg_fn));
            self.inner.content_text.set_padding_x(output_pad);
            self.inner
                .content_text
                .set_text(&self.format_tool_execution(&args, result.as_ref(), show_images));
            has_content = true;
        }

        for img_arc in &previous_image_arcs {
            self.inner.container.remove_child(img_arc);
        }
        for spacer_arc in &previous_spacer_arcs {
            self.inner.container.remove_child(spacer_arc);
        }

        let mut new_images: Vec<Image> = Vec::new();
        let mut new_image_arcs: Vec<Arc<dyn Component>> = Vec::new();
        let mut new_sources: Vec<ImageSource> = Vec::new();
        let mut new_spacers: Vec<Spacer> = Vec::new();
        let mut new_spacer_arcs: Vec<Arc<dyn Component>> = Vec::new();
        let mut need_transcoder = false;

        if let Some(ref res) = result {
            let caps = get_capabilities();
            if caps.images.is_some() && show_images {
                for block in &res.content {
                    if let UserContentBlock::Image(img) = block
                        && !img.data.is_empty()
                        && !img.mime_type.is_empty()
                    {
                        let spacer = Spacer::new(Some(1));
                        let spacer_arc: Arc<dyn Component> = Arc::new(spacer.clone());
                        self.inner.container.add_child(Arc::clone(&spacer_arc));
                        new_spacers.push(spacer);
                        new_spacer_arcs.push(spacer_arc);

                        let source = ImageSource {
                            data: img.data.clone(),
                            mime_type: img.mime_type.clone(),
                            width_cells: image_width_cells,
                        };
                        let index = new_images.len();
                        let (image_component, image_arc) = if previous_sources.get(index) == Some(&source) {
                            (previous_images[index].clone(), Arc::clone(&previous_image_arcs[index]))
                        } else {
                            let img = Image::new(
                                &source.data,
                                &source.mime_type,
                                ImageTheme {
                                    fallback_color: Arc::new(|s: &str| theme().fg(ThemeColor::ToolOutput, s)),
                                },
                                Some(ImageOptions {
                                    max_width_cells: Some(source.width_cells),
                                    ..Default::default()
                                }),
                                None,
                            );
                            let arc: Arc<dyn Component> = Arc::new(img.clone());
                            (img, arc)
                        };
                        if source.mime_type != "image/png" {
                            need_transcoder = true;
                        }
                        new_images.push(image_component);
                        new_image_arcs.push(Arc::clone(&image_arc));
                        new_sources.push(source);
                        self.inner.container.add_child(image_arc);
                    }
                }
            }
        }

        let hide_component = self.has_renderer_definition() && !has_content && new_images.is_empty();
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.call_renderer_component = next_call_component;
            state.result_renderer_component = next_result_component;
            state.image_components = new_images;
            state.image_component_arcs = new_image_arcs;
            state.image_sources = new_sources;
            state.image_spacers = new_spacers;
            state.image_spacer_arcs = new_spacer_arcs;
            state.hide_component = hide_component;
        }

        if need_transcoder {
            let weak = Arc::downgrade(&self.inner);
            ensure_png_transcoder_local(move || {
                if let Some(inner) = weak.upgrade() {
                    let comp = Self { inner };
                    comp.invalidate();
                    comp.inner.ui.request_render(None);
                }
            });
        }
    }

    fn format_tool_execution(&self, args: &Value, result: Option<&ToolExecutionResult>, show_images: bool) -> String {
        let t = theme();
        let mut text = t.fg(ThemeColor::ToolTitle, &t.bold(&self.inner.tool_name));
        let content = pi_js::json::stringify_pretty(args, "  ");
        if !content.is_empty() {
            text.push_str(&format!("\n\n{content}"));
        }
        let output = get_text_output(result, show_images);
        if !output.is_empty() {
            text.push_str(&format!("\n{output}"));
        }
        text
    }
}

impl Component for ToolExecutionComponent {
    fn render(&self, width: usize) -> Vec<String> {
        ToolExecutionComponent::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        ToolExecutionComponent::handle_mouse(self, event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        ToolExecutionComponent::invalidate(self)
    }

    fn is_container_handle_mouse(&self) -> bool {
        !self.has_renderer_definition() || self.get_render_shell() != RenderShell::Self_
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.inner.container.children())
    }
}
