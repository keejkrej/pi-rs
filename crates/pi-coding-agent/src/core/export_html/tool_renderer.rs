//! Tool HTML renderer for custom tools in HTML export.
//!
//! Renders custom tool calls and results to HTML by invoking their TUI renderers
//! and converting the resulting ANSI output to HTML.
//!
//! Port of `packages/coding-agent/src/core/export-html/tool-renderer.ts`.

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::UserContentBlock;
use pi_tui::Component;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ansi_to_html::{ansi_lines_to_html, strip_ansi_escapes};

// WIRE(@tui-w3): replace local HtmlTheme adapter with `crate::modes::interactive::theme::Theme` once TUI theme merges.
/// Lightweight theme handle passed to extension tool renderers during HTML export.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HtmlTheme {
    pub name: Option<String>,
}

/// Pre-rendered HTML variants of a custom tool result.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderedToolResultHtml {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collapsed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expanded: Option<String>,
}

/// Tool execution state passed to custom TUI result renderers during HTML export.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolResultRenderOptions {
    pub expanded: bool,
    pub is_partial: bool,
}

/// Context passed to custom `render_call`/`render_result` implementations during HTML export.
#[derive(Clone)]
pub struct ToolRenderContext {
    pub args: Option<Value>,
    pub tool_call_id: String,
    pub last_component: Option<Arc<dyn Component>>,
    pub state: Arc<Mutex<Value>>,
    pub cwd: String,
    pub execution_started: bool,
    pub args_complete: bool,
    pub is_partial: bool,
    pub expanded: bool,
    pub show_images: bool,
    pub is_error: bool,
    pub output_pad: usize,
}

impl From<&ToolRenderContext> for crate::core::extensions::types::ToolRenderContext {
    fn from(ctx: &ToolRenderContext) -> Self {
        Self {
            args: ctx.args.clone().unwrap_or(Value::Null),
            tool_call_id: ctx.tool_call_id.clone(),
            invalidate: Arc::new(|| {}),
            last_component: ctx.last_component.clone(),
            state: Arc::new(Mutex::new(None)),
            cwd: ctx.cwd.clone(),
            execution_started: ctx.execution_started,
            args_complete: ctx.args_complete,
            is_partial: ctx.is_partial,
            expanded: ctx.expanded,
            show_images: ctx.show_images,
            is_error: ctx.is_error,
        }
    }
}

impl crate::core::extensions::types::ExtensionTheme for HtmlTheme {
    fn fg(&self, _color: &str, text: &str) -> String {
        text.to_owned()
    }

    fn bg(&self, _bg: &str, text: &str) -> String {
        text.to_owned()
    }

    fn bold(&self, text: &str) -> String {
        text.to_owned()
    }

    fn italic(&self, text: &str) -> String {
        text.to_owned()
    }

    fn underline(&self, text: &str) -> String {
        text.to_owned()
    }

    fn inverse(&self, text: &str) -> String {
        text.to_owned()
    }

    fn strikethrough(&self, text: &str) -> String {
        text.to_owned()
    }

    fn get_fg_ansi(&self, _color: &str) -> String {
        String::new()
    }

    fn get_bg_ansi(&self, _bg: &str) -> String {
        String::new()
    }
}

/// Adapter trait for custom tool call/result TUI rendering during HTML export.
pub trait CustomToolRenderer: Send + Sync {
    fn render_call(
        &self,
        _args: &Value,
        _theme: &HtmlTheme,
        _context: &ToolRenderContext,
    ) -> Option<Arc<dyn Component>> {
        None
    }

    fn render_result(
        &self,
        _content: &[UserContentBlock],
        _details: Option<&Value>,
        _is_error: bool,
        _options: ToolResultRenderOptions,
        _theme: &HtmlTheme,
        _context: &ToolRenderContext,
    ) -> Option<Arc<dyn Component>> {
        None
    }
}

impl CustomToolRenderer for crate::core::extensions::types::ToolDefinition {
    fn render_call(&self, args: &Value, theme: &HtmlTheme, context: &ToolRenderContext) -> Option<Arc<dyn Component>> {
        let render_fn = self.render_call.as_ref()?;
        let ext_theme = crate::core::extensions::types::Theme::new(Arc::new(theme.clone()));
        Some(render_fn(args.clone(), ext_theme, context.into()))
    }

    fn render_result(
        &self,
        content: &[UserContentBlock],
        details: Option<&Value>,
        is_error: bool,
        options: ToolResultRenderOptions,
        theme: &HtmlTheme,
        context: &ToolRenderContext,
    ) -> Option<Arc<dyn Component>> {
        let render_fn = self.render_result.as_ref()?;
        let ext_theme = crate::core::extensions::types::Theme::new(Arc::new(theme.clone()));
        let mut result = pi_agent_core::AgentToolResult::text("");
        result.content = content.to_vec();
        result.details = details.cloned();
        result.is_error = Some(is_error);
        let opts = crate::core::extensions::types::ToolRenderResultOptions {
            expanded: options.expanded,
            is_partial: options.is_partial,
        };
        Some(render_fn(result, opts, ext_theme, context.into()))
    }
}

/// Dependencies required to construct a [`ToolHtmlRenderer`].
#[derive(Clone)]
pub struct ToolHtmlRendererDeps {
    /// Function to look up a custom tool renderer by tool name.
    pub get_tool_definition: Arc<dyn Fn(&str) -> Option<Arc<dyn CustomToolRenderer>> + Send + Sync>,
    /// Theme for styling.
    pub theme: HtmlTheme,
    /// Working directory for render context.
    pub cwd: String,
    /// Terminal width for rendering (default: 100).
    pub width: Option<usize>,
}

/// Interface for rendering custom tools to HTML.
pub trait ToolHtmlRenderer: Send + Sync {
    /// Render a tool call to HTML. Returns `None` if the tool has no custom renderer.
    fn render_call(&self, tool_call_id: &str, tool_name: &str, args: &Value) -> Option<String>;

    /// Render a tool result to collapsed/expanded HTML. Returns `None` if no custom renderer.
    fn render_result(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        result: &[UserContentBlock],
        details: Option<&Value>,
        is_error: bool,
    ) -> Option<RenderedToolResultHtml>;
}

fn is_blank_rendered_line(line: &str) -> bool {
    let stripped = strip_ansi_escapes(line);
    pi_js::str16::trim(&stripped).is_empty()
}

fn trim_rendered_result_lines(lines: &[String]) -> &[String] {
    let mut start = 0usize;
    let mut end = lines.len();
    while start < end && is_blank_rendered_line(&lines[start]) {
        start += 1;
    }
    while end > start && is_blank_rendered_line(&lines[end - 1]) {
        end -= 1;
    }
    &lines[start..end]
}

struct DefaultToolHtmlRenderer {
    deps: ToolHtmlRendererDeps,
    width: usize,
    rendered_call_components: Mutex<IndexMap<String, Arc<dyn Component>>>,
    rendered_result_components: Mutex<IndexMap<String, Arc<dyn Component>>>,
    rendered_states: Mutex<IndexMap<String, Arc<Mutex<Value>>>>,
    rendered_args: Mutex<IndexMap<String, Value>>,
}

impl DefaultToolHtmlRenderer {
    fn get_state(&self, tool_call_id: &str) -> Arc<Mutex<Value>> {
        let mut states = self.rendered_states.lock().unwrap_or_else(|e| e.into_inner());
        states
            .entry(tool_call_id.to_owned())
            .or_insert_with(|| Arc::new(Mutex::new(Value::Object(Default::default()))))
            .clone()
    }

    fn create_render_context(
        &self,
        tool_call_id: &str,
        last_component: Option<Arc<dyn Component>>,
        expanded: bool,
        is_partial: bool,
        is_error: bool,
    ) -> ToolRenderContext {
        let args = self
            .rendered_args
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(tool_call_id)
            .cloned();
        ToolRenderContext {
            args,
            tool_call_id: tool_call_id.to_owned(),
            last_component,
            state: self.get_state(tool_call_id),
            cwd: self.deps.cwd.clone(),
            execution_started: true,
            args_complete: true,
            is_partial,
            expanded,
            show_images: false,
            is_error,
            output_pad: 1,
        }
    }
}

impl ToolHtmlRenderer for DefaultToolHtmlRenderer {
    fn render_call(&self, tool_call_id: &str, tool_name: &str, args: &Value) -> Option<String> {
        self.rendered_args
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(tool_call_id.to_owned(), args.clone());

        let tool_def = (self.deps.get_tool_definition)(tool_name)?;
        let last = self
            .rendered_call_components
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(tool_call_id)
            .cloned();
        let ctx = self.create_render_context(tool_call_id, last, false, true, false);
        let component = tool_def.render_call(args, &self.deps.theme, &ctx)?;
        self.rendered_call_components
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(tool_call_id.to_owned(), Arc::clone(&component));
        let lines = component.render(self.width);
        Some(ansi_lines_to_html(&lines))
    }

    fn render_result(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        result: &[UserContentBlock],
        details: Option<&Value>,
        is_error: bool,
    ) -> Option<RenderedToolResultHtml> {
        let tool_def = (self.deps.get_tool_definition)(tool_name)?;

        let last_collapsed = self
            .rendered_result_components
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(tool_call_id)
            .cloned();
        let ctx_collapsed = self.create_render_context(tool_call_id, last_collapsed, false, false, is_error);
        let collapsed_component = tool_def.render_result(
            result,
            details,
            is_error,
            ToolResultRenderOptions {
                expanded: false,
                is_partial: false,
            },
            &self.deps.theme,
            &ctx_collapsed,
        )?;
        self.rendered_result_components
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(tool_call_id.to_owned(), Arc::clone(&collapsed_component));
        let collapsed_lines = collapsed_component.render(self.width);
        let collapsed = ansi_lines_to_html(trim_rendered_result_lines(&collapsed_lines));

        let ctx_expanded = self.create_render_context(
            tool_call_id,
            Some(Arc::clone(&collapsed_component)),
            true,
            false,
            is_error,
        );
        let expanded_component = tool_def.render_result(
            result,
            details,
            is_error,
            ToolResultRenderOptions {
                expanded: true,
                is_partial: false,
            },
            &self.deps.theme,
            &ctx_expanded,
        )?;
        self.rendered_result_components
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(tool_call_id.to_owned(), Arc::clone(&expanded_component));
        let expanded_lines = expanded_component.render(self.width);
        let expanded = ansi_lines_to_html(trim_rendered_result_lines(&expanded_lines));

        let collapsed_opt = if !collapsed.is_empty() && collapsed != expanded {
            Some(collapsed)
        } else {
            None
        };
        Some(RenderedToolResultHtml {
            collapsed: collapsed_opt,
            expanded: Some(expanded),
        })
    }
}

/// Create a tool HTML renderer backed by `CustomToolRenderer` definitions.
pub fn create_tool_html_renderer(deps: ToolHtmlRendererDeps) -> Arc<dyn ToolHtmlRenderer> {
    let width = deps.width.unwrap_or(100);
    Arc::new(DefaultToolHtmlRenderer {
        deps,
        width,
        rendered_call_components: Mutex::new(IndexMap::new()),
        rendered_result_components: Mutex::new(IndexMap::new()),
        rendered_states: Mutex::new(IndexMap::new()),
        rendered_args: Mutex::new(IndexMap::new()),
    })
}
