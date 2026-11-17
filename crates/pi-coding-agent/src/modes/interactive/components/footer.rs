//! Port of packages/coding-agent/src/modes/interactive/components/footer.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::{Model, Usage};
use pi_js::intl::locale_compare;
use pi_js::path::{is_absolute, relative, resolve, sep};
use pi_tui::{Component, TuiMouseEvent, TuiMouseEventResult, truncate_to_width, visible_width};
use serde::{Deserialize, Serialize};

use super::super::theme::theme::{ThemeColor, theme};

// WIRE(@tui-w3): Replace local UsageTotals / traits with `crate::core::usage_totals`, `crate::core::experimental`, `crate::core::agent_session::AgentSession`, and `crate::core::footer_data_provider::ReadonlyFooterDataProvider` once @mcp and @ca-cli merge onto main.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTotals {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub cost: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FooterContextUsage {
    pub context_window: i64,
    pub percent: Option<i64>,
    pub percent_f64: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct FooterRoutedModel {
    pub model_id: String,
    pub context_window: i64,
    pub thinking_level: Option<String>,
}

#[derive(Clone, Debug)]
pub enum FooterUsageRecord {
    Usage(Usage),
    AssistantUsage(Usage),
    ToolResultUsage(Usage),
    SummaryUsage(Usage),
}

pub trait FooterSession: Send + Sync {
    fn session_ptr_key(&self) -> usize;
    fn session_id(&self) -> String;
    fn leaf_id(&self) -> Option<String>;
    fn entry_count(&self) -> usize;
    fn cwd(&self) -> String;
    fn session_name(&self) -> Option<String>;
    fn model(&self) -> Option<Model>;
    fn routed_model(&self) -> Option<FooterRoutedModel>;
    fn thinking_level(&self) -> String;
    fn get_context_usage(&self) -> Option<FooterContextUsage>;
    fn is_using_subscription(&self, provider: &str) -> bool;
    fn usage_records(&self) -> Vec<FooterUsageRecord>;
}

pub trait ReadonlyFooterDataProvider: Send + Sync {
    fn get_git_branch(&self) -> Option<String>;
    fn get_extension_statuses(&self) -> IndexMap<String, String>;
    fn get_available_provider_count(&self) -> usize;
}

fn sanitize_status_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_space = false;
    for ch in text.chars() {
        let mapped = match ch {
            '\r' | '\n' | '\t' | ' ' => ' ',
            _ => ch,
        };
        if mapped == ' ' {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(mapped);
            last_was_space = false;
        }
    }
    out.trim().to_string()
}

/// Format token counts for compact footer display.
pub fn format_tokens(count: i64) -> String {
    let c = count as f64;
    if count < 1000 {
        return count.to_string();
    }
    if count < 10_000 {
        return format!("{:.1}k", c / 1000.0);
    }
    if count < 1_000_000 {
        return format!("{}k", (c / 1000.0).round() as i64);
    }
    if count < 10_000_000 {
        return format!("{:.1}M", c / 1_000_000.0);
    }
    format!("{}M", (c / 1_000_000.0).round() as i64)
}

pub fn format_cwd_for_footer(cwd: &str, home: Option<&str>) -> String {
    let Some(home) = home.filter(|s| !s.is_empty()) else {
        return cwd.to_string();
    };
    let resolved_cwd = resolve(&[cwd]);
    let resolved_home = resolve(&[home]);
    let relative_to_home = relative(&resolved_home, &resolved_cwd);
    let path_sep = sep();
    let is_inside_home = relative_to_home.is_empty()
        || (relative_to_home != ".."
            && !relative_to_home.starts_with(&format!("..{path_sep}"))
            && !is_absolute(&relative_to_home));

    if !is_inside_home {
        return cwd.to_string();
    }
    if relative_to_home.is_empty() {
        "~".to_string()
    } else {
        format!("~{path_sep}{relative_to_home}")
    }
}

#[derive(Clone)]
struct CachedSessionStats {
    session_ptr: usize,
    session_id: String,
    leaf_id: Option<String>,
    entry_count: usize,
    limits_model_key: Option<(String, String)>,
    usage_totals: UsageTotals,
    latest_cache_hit_rate: Option<f64>,
    context_usage: Option<FooterContextUsage>,
}

struct FooterState {
    auto_compact_enabled: bool,
    session: Arc<dyn FooterSession>,
    session_stats: Option<CachedSessionStats>,
}

struct FooterInner {
    footer_data: Arc<dyn ReadonlyFooterDataProvider>,
    state: Mutex<FooterState>,
}

/// Footer component that shows pwd, token stats, and context usage.
#[derive(Clone)]
pub struct FooterComponent {
    inner: Arc<FooterInner>,
}

impl FooterComponent {
    pub fn new(session: Arc<dyn FooterSession>, footer_data: Arc<dyn ReadonlyFooterDataProvider>) -> Self {
        Self {
            inner: Arc::new(FooterInner {
                footer_data,
                state: Mutex::new(FooterState {
                    auto_compact_enabled: true,
                    session,
                    session_stats: None,
                }),
            }),
        }
    }

    pub fn set_session(&self, session: Arc<dyn FooterSession>) {
        let mut state = self.inner.state.lock().unwrap();
        state.session = session;
        state.session_stats = None;
    }

    pub fn set_auto_compact_enabled(&self, enabled: bool) {
        self.inner.state.lock().unwrap().auto_compact_enabled = enabled;
    }

    pub fn invalidate(&self) {}

    pub fn dispose(&self) {}

    fn get_session_stats(&self) -> (CachedSessionStats, Arc<dyn FooterSession>, bool) {
        let mut state = self.inner.state.lock().unwrap();
        let session = Arc::clone(&state.session);
        let auto_compact = state.auto_compact_enabled;
        let session_ptr = session.session_ptr_key();
        let session_id = session.session_id();
        let leaf_id = session.leaf_id();
        let entry_count = session.entry_count();
        let routed = session.routed_model();
        let base_model = session.model();
        let limits_model_key = if let Some(ref r) = routed {
            Some(("__routed__".to_string(), r.model_id.clone()))
        } else {
            base_model.as_ref().map(|m| (m.provider.to_string(), m.id.clone()))
        };

        if let Some(ref cached) = state.session_stats
            && cached.session_ptr == session_ptr
            && cached.session_id == session_id
            && cached.leaf_id == leaf_id
            && cached.entry_count == entry_count
            && cached.limits_model_key == limits_model_key
        {
            return (cached.clone(), session, auto_compact);
        }

        let mut usage_totals = UsageTotals::default();
        let mut latest_cache_hit_rate: Option<f64> = None;

        for record in session.usage_records() {
            match record {
                FooterUsageRecord::AssistantUsage(u) => {
                    usage_totals.input += u.input;
                    usage_totals.output += u.output;
                    usage_totals.cache_read += u.cache_read;
                    usage_totals.cache_write += u.cache_write;
                    usage_totals.cost += u.cost.total;

                    let latest_prompt_tokens = u.input + u.cache_read + u.cache_write;
                    latest_cache_hit_rate = if latest_prompt_tokens > 0 {
                        Some(((u.cache_read as f64) / (latest_prompt_tokens as f64)) * 100.0)
                    } else {
                        None
                    };
                }
                FooterUsageRecord::Usage(u)
                | FooterUsageRecord::ToolResultUsage(u)
                | FooterUsageRecord::SummaryUsage(u) => {
                    usage_totals.input += u.input;
                    usage_totals.output += u.output;
                    usage_totals.cache_read += u.cache_read;
                    usage_totals.cache_write += u.cache_write;
                    usage_totals.cost += u.cost.total;
                }
            }
        }

        let context_usage = session.get_context_usage();
        let computed = CachedSessionStats {
            session_ptr,
            session_id,
            leaf_id,
            entry_count,
            limits_model_key,
            usage_totals,
            latest_cache_hit_rate,
            context_usage,
        };
        state.session_stats = Some(computed.clone());
        (computed, session, auto_compact)
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let (stats, session, auto_compact_enabled) = self.get_session_stats();
        let model = session.model();
        let context_window = stats
            .context_usage
            .as_ref()
            .map(|c| c.context_window)
            .or_else(|| model.as_ref().map(|m| m.context_window))
            .unwrap_or(0);
        let context_percent_value = stats
            .context_usage
            .as_ref()
            .and_then(|c| c.percent_f64.or_else(|| c.percent.map(|p| p as f64)))
            .unwrap_or(0.0);
        let has_percent = stats
            .context_usage
            .as_ref()
            .is_some_and(|c| c.percent_f64.is_some() || c.percent.is_some());

        let home = pi_js::env::var("HOME").or_else(|| pi_js::env::var("USERPROFILE"));
        let mut pwd = format_cwd_for_footer(&session.cwd(), home.as_deref());
        if let Some(branch) = self.inner.footer_data.get_git_branch()
            && !branch.is_empty()
        {
            pwd = format!("{pwd} ({branch})");
        }
        if let Some(session_name) = session.session_name()
            && !session_name.is_empty()
        {
            pwd = format!("{pwd} • {session_name}");
        }

        let t = theme();
        let mut stats_parts: Vec<String> = Vec::new();
        if stats.usage_totals.input != 0 {
            stats_parts.push(format!("↑{}", format_tokens(stats.usage_totals.input)));
        }
        if stats.usage_totals.output != 0 {
            stats_parts.push(format!("↓{}", format_tokens(stats.usage_totals.output)));
        }
        if stats.usage_totals.cache_read != 0 {
            stats_parts.push(format!("R{}", format_tokens(stats.usage_totals.cache_read)));
        }
        if stats.usage_totals.cache_write != 0 {
            stats_parts.push(format!("W{}", format_tokens(stats.usage_totals.cache_write)));
        }
        if (stats.usage_totals.cache_read > 0 || stats.usage_totals.cache_write > 0)
            && let Some(rate) = stats.latest_cache_hit_rate
        {
            stats_parts.push(format!("CH{rate:.1}%"));
        }

        let using_subscription = match model {
            Some(ref m) => m.provider == "kimi-coding" || session.is_using_subscription(&m.provider),
            None => false,
        };
        if stats.usage_totals.cost != 0.0 || using_subscription {
            stats_parts.push(format!(
                "${:.3}{}",
                stats.usage_totals.cost,
                if using_subscription { " (sub)" } else { "" }
            ));
        }

        let auto_indicator = if auto_compact_enabled { " (auto)" } else { "" };
        let context_percent_display = if has_percent {
            format!(
                "{context_percent_value:.1}%/{}{auto_indicator}",
                format_tokens(context_window)
            )
        } else {
            format!("?/{}{auto_indicator}", format_tokens(context_window))
        };
        let context_percent_str = if context_percent_value > 90.0 {
            t.fg(ThemeColor::Error, &context_percent_display)
        } else if context_percent_value > 70.0 {
            t.fg(ThemeColor::Warning, &context_percent_display)
        } else {
            context_percent_display
        };
        stats_parts.push(context_percent_str);

        if pi_js::env::var("PI_EXPERIMENTAL").as_deref() == Some("1") {
            stats_parts.push(format!(
                "{} {}",
                t.fg(ThemeColor::Dim, "•"),
                t.bold(&t.fg(ThemeColor::Warning, "xp"))
            ));
        }

        let mut stats_left = stats_parts.join(" ");
        let width_i64 = width as i64;
        let mut stats_left_width = visible_width(&stats_left);
        if stats_left_width > width_i64 {
            stats_left = truncate_to_width(&stats_left, width_i64, Some("..."), None);
            stats_left_width = visible_width(&stats_left);
        }

        let min_padding: i64 = 2;
        let model_name = model.as_ref().map(|m| m.id.as_str()).unwrap_or("no-model");
        let mut right_side_without_provider = model_name.to_string();
        if let Some(ref m) = model
            && m.reasoning
        {
            let lvl = session.thinking_level();
            if lvl.is_empty() || lvl == "off" {
                right_side_without_provider = format!("{model_name} • thinking off");
            } else {
                right_side_without_provider = format!("{model_name} • {lvl}");
            }
        }
        if let Some(routed) = session.routed_model() {
            let lvl_suffix = routed
                .thinking_level
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| format!(" • {s}"))
                .unwrap_or_default();
            right_side_without_provider.push_str(&format!(" → {}{lvl_suffix}", routed.model_id));
        }

        let mut right_side = right_side_without_provider.clone();
        if self.inner.footer_data.get_available_provider_count() > 1
            && let Some(ref m) = model
        {
            right_side = format!("({}) {right_side_without_provider}", m.provider);
            if stats_left_width + min_padding + visible_width(&right_side) > width_i64 {
                right_side = right_side_without_provider;
            }
        }

        let right_side_width = visible_width(&right_side);
        let total_needed = stats_left_width + min_padding + right_side_width;
        let stats_line = if total_needed <= width_i64 {
            let padding = " ".repeat(usize::try_from(width_i64 - stats_left_width - right_side_width).unwrap_or(0));
            format!("{stats_left}{padding}{right_side}")
        } else {
            let available_for_right = width_i64 - stats_left_width - min_padding;
            if available_for_right > 0 {
                let truncated_right = truncate_to_width(&right_side, available_for_right, Some(""), None);
                let truncated_right_width = visible_width(&truncated_right);
                let padding = " ".repeat(
                    usize::try_from((width_i64 - stats_left_width - truncated_right_width).max(0)).unwrap_or(0),
                );
                format!("{stats_left}{padding}{truncated_right}")
            } else {
                stats_left.clone()
            }
        };

        let dim_stats_left = t.fg(ThemeColor::Dim, &stats_left);
        let remainder = &stats_line[stats_left.len()..];
        let dim_remainder = t.fg(ThemeColor::Dim, remainder);

        let pwd_line = truncate_to_width(
            &t.fg(ThemeColor::Dim, &pwd),
            width_i64,
            Some(&t.fg(ThemeColor::Dim, "...")),
            None,
        );
        let mut lines = vec![pwd_line, format!("{dim_stats_left}{dim_remainder}")];

        let extension_statuses = self.inner.footer_data.get_extension_statuses();
        if !extension_statuses.is_empty() {
            let mut entries: Vec<(String, String)> = extension_statuses.into_iter().collect();
            entries.sort_by(|(a, _), (b, _)| locale_compare(a, b));
            let status_line = entries
                .into_iter()
                .map(|(_, text)| sanitize_status_text(&text))
                .collect::<Vec<_>>()
                .join(" ");
            lines.push(truncate_to_width(
                &status_line,
                width_i64,
                Some(&t.fg(ThemeColor::Dim, "...")),
                None,
            ));
        }

        lines
    }
}

impl Component for FooterComponent {
    fn render(&self, width: usize) -> Vec<String> {
        FooterComponent::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        FooterComponent::invalidate(self)
    }
}
