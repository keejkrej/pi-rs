//! Port of packages/agent/test/utils/get-current-time.ts

use std::sync::Arc;

use chrono::{DateTime, FixedOffset, Local, SecondsFormat, TimeZone, Utc};
use pi_agent_core::{AgentTool, AgentToolResult};
use pi_ai::{UserContentBlock, faux_text};
use pi_js::time::now_ms;
use pi_js::{Error, Result};
use pi_schema::{Annotations, ObjOptions, StrOptions, obj_with, opt, str_with};
use serde_json::{Value, json};

pub fn get_current_time(timezone: Option<&str>) -> Result<AgentToolResult> {
    let utc_ms = now_ms();
    let utc_dt = DateTime::<Utc>::from_timestamp_millis(utc_ms).unwrap_or_else(Utc::now);

    let time_str = if let Some(tz_name) = timezone.filter(|s| !s.is_empty()) {
        match resolve_timezone(tz_name, utc_dt) {
            Some((offset, abbr)) => {
                let zoned = utc_dt.with_timezone(&offset);
                format!("{} {abbr}", zoned.format("%A, %B %-d, %Y at %-I:%M:%S %p"))
            }
            None => {
                let iso = utc_dt.to_rfc3339_opts(SecondsFormat::Millis, true);
                return Err(Error::msg(format!(
                    "Invalid timezone: {tz_name}. Current UTC time: {iso}"
                )));
            }
        }
    } else {
        let local_dt = utc_dt.with_timezone(&Local);
        local_dt.format("%A, %B %-d, %Y at %-I:%M:%S %p %Z").to_string()
    };

    Ok(AgentToolResult {
        content: vec![UserContentBlock::Text(faux_text(&time_str))],
        details: Some(json!({ "utcTimestamp": utc_ms })),
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
        control: None,
    })
}

fn resolve_timezone(tz: &str, utc_dt: DateTime<Utc>) -> Option<(FixedOffset, &'static str)> {
    // PORT: `pi-agent-core` does not pull `chrono-tz`; support common IANA/UTC zones used in tests
    // and fixed numeric offsets without shelling out or spawning OS commands.
    let _ = utc_dt;
    let (secs, abbr) = match tz {
        "UTC" | "Etc/UTC" | "GMT" | "Etc/GMT" => (0, "UTC"),
        "Europe/London" => (0, "GMT"),
        "Europe/Berlin" | "Europe/Paris" => (3600, "CET"),
        "America/New_York" => (-5 * 3600, "EST"),
        "America/Chicago" => (-6 * 3600, "CST"),
        "America/Denver" => (-7 * 3600, "MST"),
        "America/Los_Angeles" => (-8 * 3600, "PST"),
        "Asia/Tokyo" => (9 * 3600, "JST"),
        "Asia/Shanghai" => (8 * 3600, "CST"),
        "Australia/Sydney" => (10 * 3600, "AEST"),
        _ => {
            let offset = parse_fixed_offset(tz)?;
            return Some((offset, "UTC"));
        }
    };
    FixedOffset::east_opt(secs).map(|offset| (offset, abbr))
}

fn parse_fixed_offset(tz: &str) -> Option<FixedOffset> {
    let rest = tz.strip_prefix("UTC").or_else(|| tz.strip_prefix("GMT"))?;
    if rest.is_empty() {
        return FixedOffset::east_opt(0);
    }
    let (sign, digits) = if let Some(r) = rest.strip_prefix('+') {
        (1, r)
    } else if let Some(r) = rest.strip_prefix('-') {
        (-1, r)
    } else {
        return None;
    };
    let (hours, mins) = match digits.split_once(':') {
        Some((h, m)) => (h.parse::<i32>().ok()?, m.parse::<i32>().ok()?),
        None if digits.len() == 4 => (digits[..2].parse::<i32>().ok()?, digits[2..].parse::<i32>().ok()?),
        None => (digits.parse::<i32>().ok()?, 0),
    };
    if hours > 23 || mins > 59 {
        return None;
    }
    FixedOffset::east_opt(sign * (hours * 3600 + mins * 60))
}

pub fn get_current_time_tool() -> AgentTool {
    let parameters = obj_with(
        [(
            "timezone",
            opt(str_with(StrOptions {
                annotations: Annotations {
                    description: Some("Optional timezone (e.g., 'America/New_York', 'Europe/London')".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            })),
        )],
        // Open, as the tool accepted extra arguments before it moved from TypeBox.
        ObjOptions {
            open: true,
            ..Default::default()
        },
    );

    AgentTool {
        name: "get_current_time".to_string(),
        label: "Current Time".to_string(),
        description: "Get the current date and time".to_string(),
        parameters,
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(|_tool_call_id, args, _signal, _on_update| {
            Box::pin(async move {
                let timezone = match args.get("timezone") {
                    Some(Value::String(s)) => Some(s.as_str()),
                    _ => None,
                };
                get_current_time(timezone)
            })
        }),
        replay: None,
    }
}
