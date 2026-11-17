//! HTML session export generator.
//!
//! Port of `packages/coding-agent/src/core/export-html/index.ts`.

// @generated-mods begin (scaffold-owned, do not edit)
pub mod ansi_to_html;
pub mod tool_renderer;
// @generated-mods end

use std::path::Path;
use std::sync::Arc;

use indexmap::{IndexMap, IndexSet};
use pi_agent_core::{AgentMessage, AgentState};
use pi_ai::{AssistantContentBlock, Message};
use pi_js::error::{Error, Result};
use pi_js::vendor::typebox::Schema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::APP_NAME;
use crate::core::session_manager::{SessionEntry, SessionHeader, SessionManager};
use crate::utils::paths::{PathInputOptions, normalize_path, resolve_path};

pub use tool_renderer::{
    CustomToolRenderer, HtmlTheme, RenderedToolResultHtml, ToolHtmlRenderer, ToolHtmlRendererDeps, ToolRenderContext,
    ToolResultRenderOptions, create_tool_html_renderer,
};

const TEMPLATE_HTML: &str = include_str!("export_html/template.html");
const TEMPLATE_CSS: &str = include_str!("export_html/template.css");
const TEMPLATE_JS: &str = include_str!("export_html/template.js");
const MARKED_JS: &str = include_str!("export_html/vendor/marked.min.js");
const HIGHLIGHT_JS: &str = include_str!("export_html/vendor/highlight.min.js");

/// Explicit export page/card/info background overrides from a theme.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeExportColors {
    pub page_bg: Option<String>,
    pub card_bg: Option<String>,
    pub info_bg: Option<String>,
}

/// Optional theme palette passed into HTML export when bypassing TUI theme lookup.
#[derive(Clone, Debug, Default)]
pub struct ResolvedExportTheme {
    pub colors: IndexMap<String, String>,
    pub export_colors: ThemeExportColors,
}

/// Options controlling [`export_session_to_html`] and [`export_from_file`].
#[derive(Clone, Default)]
pub struct ExportOptions {
    pub output_path: Option<String>,
    pub theme_name: Option<String>,
    /// Optional tool renderer for custom tools.
    pub tool_renderer: Option<Arc<dyn ToolHtmlRenderer>>,
    /// Optional resolved theme colors injected by the caller/TUI.
    // WIRE(@tui-w3): wire `get_resolved_theme_colors` and `get_theme_export_colors` from `crate::modes::interactive::theme`.
    pub theme_override: Option<ResolvedExportTheme>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct RenderedToolHtml {
    #[serde(skip_serializing_if = "Option::is_none")]
    call_html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_html_collapsed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_html_expanded: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportedToolDefinition {
    name: String,
    description: String,
    parameters: Schema,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionData {
    header: Option<SessionHeader>,
    entries: Vec<SessionEntry>,
    leaf_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ExportedToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rendered_tools: Option<IndexMap<String, RenderedToolHtml>>,
}

struct Rgb {
    r: u8,
    g: u8,
    b: u8,
}

fn parse_color(color: &str) -> Option<Rgb> {
    let trimmed = pi_js::str16::trim(color);
    if let Some(hex) = trimmed.strip_prefix('#')
        && hex.len() == 6
    {
        let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
        return Some(Rgb { r, g, b });
    }
    if let Some(rest) = trimmed.strip_prefix("rgb") {
        let rest = rest.trim_start();
        let inner = rest.strip_prefix('(')?.strip_suffix(')')?;
        let parts: Vec<&str> = inner.split(',').collect();
        if parts.len() == 3 {
            let r = parts[0].trim().parse::<u8>().ok()?;
            let g = parts[1].trim().parse::<u8>().ok()?;
            let b = parts[2].trim().parse::<u8>().ok()?;
            return Some(Rgb { r, g, b });
        }
    }
    None
}

fn get_luminance(rgb: &Rgb) -> f64 {
    let to_linear = |c: u8| {
        let s = f64::from(c) / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * to_linear(rgb.r) + 0.7152 * to_linear(rgb.g) + 0.0722 * to_linear(rgb.b)
}

fn adjust_brightness(color: &str, factor: f64) -> String {
    let Some(parsed) = parse_color(color) else {
        return color.to_owned();
    };
    let adjust = |c: u8| -> u8 { (f64::from(c) * factor).round().clamp(0.0, 255.0) as u8 };
    format!("rgb({}, {}, {})", adjust(parsed.r), adjust(parsed.g), adjust(parsed.b))
}

fn derive_export_colors(base_color: &str) -> (String, String, String) {
    let Some(parsed) = parse_color(base_color) else {
        return (
            "rgb(24, 24, 30)".to_owned(),
            "rgb(30, 30, 36)".to_owned(),
            "rgb(60, 55, 40)".to_owned(),
        );
    };

    if get_luminance(&parsed) > 0.5 {
        let info = format!(
            "rgb({}, {}, {})",
            parsed.r.saturating_add(10),
            parsed.g.saturating_add(5),
            parsed.b.saturating_sub(20)
        );
        (adjust_brightness(base_color, 0.96), base_color.to_owned(), info)
    } else {
        let info = format!(
            "rgb({}, {}, {})",
            parsed.r.saturating_add(20),
            parsed.g.saturating_add(15),
            parsed.b
        );
        (
            adjust_brightness(base_color, 0.7),
            adjust_brightness(base_color, 0.85),
            info,
        )
    }
}

fn default_theme_colors(theme_name: Option<&str>) -> ResolvedExportTheme {
    // Fallback dark/light palette mirrors default TUI themes until `@tui` theme module lands.
    let is_light = theme_name.is_some_and(|name| name.eq_ignore_ascii_case("light"));
    let mut colors = IndexMap::new();
    if is_light {
        colors.insert("accent".to_owned(), "#0066cc".to_owned());
        colors.insert("border".to_owned(), "#d0d7de".to_owned());
        colors.insert("borderAccent".to_owned(), "#0969da".to_owned());
        colors.insert("borderMuted".to_owned(), "#e5e7eb".to_owned());
        colors.insert("success".to_owned(), "#1a7f37".to_owned());
        colors.insert("error".to_owned(), "#cf222e".to_owned());
        colors.insert("warning".to_owned(), "#9a6700".to_owned());
        colors.insert("muted".to_owned(), "#656d76".to_owned());
        colors.insert("dim".to_owned(), "#8c959f".to_owned());
        colors.insert("text".to_owned(), "#1f2328".to_owned());
        colors.insert("thinkingText".to_owned(), "#6e7781".to_owned());
        colors.insert("userMessageBg".to_owned(), "#f3f4f6".to_owned());
        colors.insert("userMessageText".to_owned(), "#1f2328".to_owned());
        colors.insert("toolPendingBg".to_owned(), "#f6f8fa".to_owned());
        colors.insert("toolSuccessBg".to_owned(), "#eefaf1".to_owned());
        colors.insert("toolErrorBg".to_owned(), "#fff1f2".to_owned());
        colors.insert("toolTitle".to_owned(), "#0969da".to_owned());
        colors.insert("toolOutput".to_owned(), "#24292f".to_owned());
        colors.insert("mdHeading".to_owned(), "#0550ae".to_owned());
        colors.insert("mdLink".to_owned(), "#0969da".to_owned());
        colors.insert("mdCode".to_owned(), "#953800".to_owned());
        colors.insert("mdCodeBlock".to_owned(), "#f6f8fa".to_owned());
        colors.insert("mdCodeBlockBorder".to_owned(), "#d0d7de".to_owned());
        colors.insert("mdQuote".to_owned(), "#57606a".to_owned());
        colors.insert("mdQuoteBorder".to_owned(), "#d0d7de".to_owned());
        colors.insert("customMessageBg".to_owned(), "#eef2ff".to_owned());
        colors.insert("customMessageText".to_owned(), "#1e293b".to_owned());
        colors.insert("customMessageLabel".to_owned(), "#4338ca".to_owned());
        colors.insert("toolDiffAdded".to_owned(), "#1a7f37".to_owned());
        colors.insert("toolDiffRemoved".to_owned(), "#cf222e".to_owned());
        colors.insert("toolDiffContext".to_owned(), "#57606a".to_owned());
    } else {
        colors.insert("accent".to_owned(), "#8abeb7".to_owned());
        colors.insert("border".to_owned(), "#5f87af".to_owned());
        colors.insert("borderAccent".to_owned(), "#00d7ff".to_owned());
        colors.insert("borderMuted".to_owned(), "#505050".to_owned());
        colors.insert("success".to_owned(), "#b5bd68".to_owned());
        colors.insert("error".to_owned(), "#cc6666".to_owned());
        colors.insert("warning".to_owned(), "#ffff00".to_owned());
        colors.insert("muted".to_owned(), "#808080".to_owned());
        colors.insert("dim".to_owned(), "#666666".to_owned());
        colors.insert("text".to_owned(), "".to_owned());
        colors.insert("thinkingText".to_owned(), "#808080".to_owned());
        colors.insert("userMessageBg".to_owned(), "#343541".to_owned());
        colors.insert("userMessageText".to_owned(), "".to_owned());
        colors.insert("toolPendingBg".to_owned(), "#282832".to_owned());
        colors.insert("toolSuccessBg".to_owned(), "#283228".to_owned());
        colors.insert("toolErrorBg".to_owned(), "#3c2828".to_owned());
        colors.insert("toolTitle".to_owned(), "".to_owned());
        colors.insert("toolOutput".to_owned(), "#808080".to_owned());
        colors.insert("mdHeading".to_owned(), "#f0c674".to_owned());
        colors.insert("mdLink".to_owned(), "#81a2be".to_owned());
        colors.insert("mdCode".to_owned(), "#8abeb7".to_owned());
        colors.insert("mdCodeBlock".to_owned(), "#b5bd68".to_owned());
        colors.insert("mdCodeBlockBorder".to_owned(), "#808080".to_owned());
        colors.insert("mdQuote".to_owned(), "#808080".to_owned());
        colors.insert("mdQuoteBorder".to_owned(), "#808080".to_owned());
        colors.insert("customMessageBg".to_owned(), "#2d2838".to_owned());
        colors.insert("customMessageText".to_owned(), "".to_owned());
        colors.insert("customMessageLabel".to_owned(), "#9575cd".to_owned());
        colors.insert("toolDiffAdded".to_owned(), "#b5bd68".to_owned());
        colors.insert("toolDiffRemoved".to_owned(), "#cc6666".to_owned());
        colors.insert("toolDiffContext".to_owned(), "#808080".to_owned());
    }
    ResolvedExportTheme {
        colors,
        export_colors: ThemeExportColors::default(),
    }
}

fn generate_html(session_data: &SessionData, options: &ExportOptions) -> Result<String> {
    let resolved = options
        .theme_override
        .clone()
        .unwrap_or_else(|| default_theme_colors(options.theme_name.as_deref()));

    let user_message_bg = resolved
        .colors
        .get("userMessageBg")
        .filter(|s| !s.is_empty())
        .map(String::as_str)
        .unwrap_or("#343541");
    let (derived_page, derived_card, derived_info) = derive_export_colors(user_message_bg);
    let body_bg = resolved.export_colors.page_bg.unwrap_or(derived_page);
    let container_bg = resolved.export_colors.card_bg.unwrap_or(derived_card);
    let info_bg = resolved.export_colors.info_bg.unwrap_or(derived_info);

    let mut lines: Vec<String> = resolved.colors.iter().map(|(k, v)| format!("--{k}: {v};")).collect();
    lines.push(format!("--exportPageBg: {body_bg};"));
    lines.push(format!("--exportCardBg: {container_bg};"));
    lines.push(format!("--exportInfoBg: {info_bg};"));
    let theme_vars = lines.join("\n      ");

    let json = pi_js::json::try_stringify(session_data)?;
    let session_data_base64 = pi_js::b64::encode(json.as_bytes());

    let css = TEMPLATE_CSS
        .replacen("{{THEME_VARS}}", &theme_vars, 1)
        .replacen("{{BODY_BG}}", &body_bg, 1)
        .replacen("{{CONTAINER_BG}}", &container_bg, 1)
        .replacen("{{INFO_BG}}", &info_bg, 1);

    Ok(TEMPLATE_HTML
        .replacen("{{CSS}}", &css, 1)
        .replacen("{{JS}}", TEMPLATE_JS, 1)
        .replacen("{{SESSION_DATA}}", &session_data_base64, 1)
        .replacen("{{MARKED_JS}}", MARKED_JS, 1)
        .replacen("{{HIGHLIGHT_JS}}", HIGHLIGHT_JS, 1))
}

fn template_rendered_tools() -> IndexSet<&'static str> {
    ["bash", "read", "write", "edit", "ls"].into_iter().collect()
}

fn pre_render_custom_tools(
    entries: &[SessionEntry],
    tool_renderer: &dyn ToolHtmlRenderer,
) -> IndexMap<String, RenderedToolHtml> {
    let builtin = template_rendered_tools();
    let mut rendered_tools: IndexMap<String, RenderedToolHtml> = IndexMap::new();

    for entry in entries {
        let SessionEntry::Message(msg_entry) = entry else {
            continue;
        };
        let AgentMessage::Message(msg) = &msg_entry.message else {
            continue;
        };

        match msg {
            Message::Assistant(assistant) => {
                for block in &assistant.content {
                    if let AssistantContentBlock::ToolCall(tc) = block
                        && !builtin.contains(tc.name.as_str())
                    {
                        let args_obj = tc.arguments.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                        if let Some(call_html) = tool_renderer.render_call(&tc.id, &tc.name, &Value::Object(args_obj)) {
                            rendered_tools.insert(
                                tc.id.clone(),
                                RenderedToolHtml {
                                    call_html: Some(call_html),
                                    result_html_collapsed: None,
                                    result_html_expanded: None,
                                },
                            );
                        }
                    }
                }
            }
            Message::ToolResult(tr) => {
                if !tr.tool_call_id.is_empty() {
                    let existing = rendered_tools.get(&tr.tool_call_id).cloned();
                    if (existing.is_some() || !builtin.contains(tr.tool_name.as_str()))
                        && let Some(rendered) = tool_renderer.render_result(
                            &tr.tool_call_id,
                            &tr.tool_name,
                            &tr.content,
                            tr.details.as_ref(),
                            tr.is_error,
                        )
                    {
                        let mut entry_html = existing.unwrap_or_default();
                        entry_html.result_html_collapsed = rendered.collapsed;
                        entry_html.result_html_expanded = rendered.expanded;
                        rendered_tools.insert(tr.tool_call_id.clone(), entry_html);
                    }
                }
            }
            Message::User(_) | Message::System(_) => {}
        }
    }

    rendered_tools
}

fn default_export_filename(session_file: &str) -> String {
    let stem = Path::new(session_file)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("session");
    format!("{}-session-{stem}.html", *APP_NAME)
}

/// Export session to HTML using `SessionManager` and optional `AgentState`.
pub async fn export_session_to_html(
    sm: &SessionManager,
    state: Option<&AgentState>,
    options: Option<ExportOptions>,
) -> Result<String> {
    let opts = options.unwrap_or_default();
    let Some(session_file) = sm.get_session_file() else {
        return Err(Error::msg("Cannot export in-memory session to HTML"));
    };
    if !Path::new(&session_file).exists() {
        return Err(Error::msg("Nothing to export yet - start a conversation first"));
    }

    let entries = sm.get_entries();
    let rendered_tools = opts.tool_renderer.as_ref().and_then(|renderer| {
        let rendered = pre_render_custom_tools(&entries, renderer.as_ref());
        if rendered.is_empty() { None } else { Some(rendered) }
    });

    let session_data = SessionData {
        header: sm.get_header(),
        entries,
        leaf_id: sm.get_leaf_id(),
        system_prompt: state.map(|s| s.system_prompt.clone()),
        tools: state.map(|s| {
            s.tools
                .iter()
                .map(|t| ExportedToolDefinition {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters.clone(),
                })
                .collect()
        }),
        rendered_tools,
    };

    let html = generate_html(&session_data, &opts)?;
    let output_path = match &opts.output_path {
        Some(p) => normalize_path(p, Default::default()),
        None => default_export_filename(&session_file),
    };

    std::fs::write(&output_path, html).map_err(|e| Error::msg(e.to_string()))?;
    Ok(output_path)
}

/// Export session file to HTML (standalone, without `AgentState`).
pub async fn export_from_file(input_path: &str, options: Option<ExportOptions>) -> Result<String> {
    let opts = options.unwrap_or_default();
    let resolved_input_path = resolve_path(input_path, None, PathInputOptions::default());
    if !Path::new(&resolved_input_path).exists() {
        return Err(Error::msg(format!("File not found: {resolved_input_path}")));
    }

    let sm = SessionManager::open(&resolved_input_path, None, None)?;
    let session_data = SessionData {
        header: sm.get_header(),
        entries: sm.get_entries(),
        leaf_id: sm.get_leaf_id(),
        system_prompt: None,
        tools: None,
        rendered_tools: None,
    };

    let html = generate_html(&session_data, &opts)?;
    let output_path = match &opts.output_path {
        Some(p) => normalize_path(p, Default::default()),
        None => default_export_filename(&resolved_input_path),
    };

    std::fs::write(&output_path, html).map_err(|e| Error::msg(e.to_string()))?;
    Ok(output_path)
}
