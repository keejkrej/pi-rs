//! Port of packages/telemetry/src/render.ts

use indexmap::IndexMap;

use crate::{SpanAttributes, SpanStatus, StoredSpan};

// C0 and C1 control characters and DEL: a stored id, name or message may come from another process (a carrier), so
// none reaches a terminal or a viewer raw.
fn is_control_char(ch: char) -> bool {
    matches!(ch as u32, 0x0000..=0x001f | 0x007f..=0x009f)
}

/// `text` with every control character written as a `\uXXXX` escape.
fn escaped(text: &str) -> String {
    if !text.chars().any(is_control_char) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if is_control_char(ch) {
            out.push_str(&format!("\\u{:04x}", ch as u32));
        } else {
            out.push(ch);
        }
    }
    out
}

/// `ms` rounded to the microsecond, as the adapters record span times.
fn microseconds(ms: f64) -> f64 {
    pi_js::num::math_round(ms * 1000.0) / 1000.0
}

fn format_attributes(attributes: &SpanAttributes) -> String {
    let mut out = String::new();
    for (key, value) in attributes {
        out.push(' ');
        out.push_str(&escaped(key));
        out.push('=');
        out.push_str(&pi_js::json::stringify(value));
    }
    out
}

/// Render stored spans as a chronological forest, treating absent parents as roots. Lengths of time are milliseconds to
/// the microsecond. Control characters in ids, names, attribute keys and error messages are escaped (attribute values
/// are JSON).
pub fn render_trace(spans: &[StoredSpan]) -> String {
    let mut by_id: IndexMap<&str, &StoredSpan> = IndexMap::new();
    for span in spans {
        by_id.insert(span.id.as_str(), span);
    }

    let mut ordered: Vec<&StoredSpan> = by_id.values().copied().collect();
    ordered.sort_by(|left, right| left.started_ms.total_cmp(&right.started_ms));

    let mut roots: Vec<&StoredSpan> = Vec::new();
    let mut children: IndexMap<&str, Vec<&StoredSpan>> = IndexMap::new();
    for span in ordered {
        match span.parent_id.as_deref() {
            Some(parent_id) if by_id.contains_key(parent_id) => {
                children.entry(parent_id).or_default().push(span);
            }
            _ => roots.push(span),
        }
    }

    let mut lines: Vec<String> = Vec::new();
    let mut pending: Vec<(&StoredSpan, usize)> = roots.into_iter().rev().map(|span| (span, 0)).collect();
    while let Some((span, depth)) = pending.pop() {
        let indent = "  ".repeat(depth);
        let mut error = String::new();
        if let SpanStatus::Error { error: details } = &span.status {
            error.push_str(" error");
            if let Some(err) = details {
                error.push(' ');
                error.push_str(&escaped(&err.name));
                error.push_str(": ");
                error.push_str(&escaped(&err.message));
            }
        }
        let duration = pi_js::json::number_to_string(microseconds(span.ended_ms - span.started_ms));
        lines.push(format!(
            "{indent}{}{} ({duration}ms){error}",
            escaped(&span.name),
            format_attributes(&span.attributes),
        ));
        for event in &span.events {
            let time = pi_js::json::number_to_string(microseconds(event.time_ms));
            lines.push(format!(
                "{indent}  \u{00b7} {}{} (+{time}ms)",
                escaped(&event.name),
                format_attributes(&event.attributes),
            ));
        }
        for link in &span.links {
            let ids = format!("trace {} span {}", escaped(&link.trace_id), escaped(&link.span_id));
            let attrs = link.attributes.as_ref().map_or_else(String::new, format_attributes);
            lines.push(format!("{indent}  \u{2192} link {ids}{attrs}"));
        }
        if let Some(descendants) = children.get(span.id.as_str()) {
            for child in descendants.iter().rev() {
                pending.push((*child, depth + 1));
            }
        }
    }
    lines.join("\n")
}
