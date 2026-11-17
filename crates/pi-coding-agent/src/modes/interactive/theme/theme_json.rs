//! Port of packages/coding-agent/src/modes/interactive/theme/theme-json.ts

use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ThemeColorValue {
    String(String),
    Indexed(i64),
}

impl ThemeColorValue {
    pub fn from_json(value: &Value) -> Option<Self> {
        match value {
            Value::String(s) => Some(Self::String(s.clone())),
            Value::Number(n) => n
                .as_i64()
                .or_else(|| {
                    n.as_f64()
                        .filter(|f| f.is_finite() && f.fract() == 0.0)
                        .map(|f| f as i64)
                })
                .filter(|&i| (0..=255).contains(&i))
                .map(Self::Indexed),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeJsonAppearance {
    Dark,
    Light,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeExportJson {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_bg: Option<ThemeColorValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_bg: Option<ThemeColorValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info_bg: Option<ThemeColorValue>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidatedThemeJson {
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<ThemeJsonAppearance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vars: Option<IndexMap<String, ThemeColorValue>>,
    pub colors: IndexMap<String, ThemeColorValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export: Option<ThemeExportJson>,
}

pub const REQUIRED_THEME_COLOR_TOKENS: &[&str] = &[
    "accent",
    "border",
    "borderAccent",
    "borderMuted",
    "success",
    "error",
    "warning",
    "muted",
    "dim",
    "text",
    "thinkingText",
    "selectedBg",
    "userMessageBg",
    "userMessageText",
    "customMessageBg",
    "customMessageText",
    "customMessageLabel",
    "toolPendingBg",
    "toolSuccessBg",
    "toolErrorBg",
    "toolTitle",
    "toolOutput",
    "mdHeading",
    "mdLink",
    "mdLinkUrl",
    "mdCode",
    "mdCodeBlock",
    "mdCodeBlockBorder",
    "mdQuote",
    "mdQuoteBorder",
    "mdHr",
    "mdListBullet",
    "toolDiffAdded",
    "toolDiffRemoved",
    "toolDiffContext",
    "syntaxComment",
    "syntaxKeyword",
    "syntaxFunction",
    "syntaxVariable",
    "syntaxString",
    "syntaxNumber",
    "syntaxType",
    "syntaxOperator",
    "syntaxPunctuation",
    "thinkingOff",
    "thinkingMinimal",
    "thinkingLow",
    "thinkingMedium",
    "thinkingHigh",
    "thinkingXhigh",
    "bashMode",
];

pub const OPTIONAL_THEME_COLOR_TOKENS: &[&str] = &[
    "scrollbarTrack",
    "scrollbarThumb",
    "searchMatchBg",
    "searchMatchText",
    "thinkingMax",
];

fn is_valid_token_key(key: &str) -> bool {
    REQUIRED_THEME_COLOR_TOKENS.contains(&key) || OPTIONAL_THEME_COLOR_TOKENS.contains(&key)
}

fn format_validation_error(label: &str, missing_colors: &IndexSet<String>, other_errors: &[String]) -> pi_js::Error {
    let mut msg = format!("Invalid theme \"{label}\":\n");
    if !missing_colors.is_empty() {
        msg.push_str("\nMissing required color tokens:\n");
        let mut sorted: Vec<&str> = missing_colors.iter().map(String::as_str).collect();
        sorted.sort_unstable();
        for color in sorted {
            msg.push_str(&format!("  - {color}\n"));
        }
        msg.pop();
        msg.push_str("\n\nPlease add these colors to your theme's \"colors\" object.");
        msg.push_str("\nSee the built-in themes (dark.json, light.json) for reference values.");
    }
    if !other_errors.is_empty() {
        msg.push_str("\n\nOther errors:\n");
        msg.push_str(&other_errors.join("\n"));
    }
    pi_js::Error::msg(msg)
}

/// Validate one theme document, returning a descriptive error naming offending tokens.
pub fn validate_theme_json(label: &str, json: &Value) -> pi_js::Result<ValidatedThemeJson> {
    let Some(obj) = json.as_object() else {
        let mut other = vec!["  - /: must be object".to_string()];
        let _ = &mut other;
        return Err(format_validation_error(label, &IndexSet::new(), &other));
    };

    if let Some(name) = obj.get("name").and_then(Value::as_str)
        && name.contains('/')
    {
        return Err(pi_js::Error::msg(format!(
            "Invalid theme name \"{name}\": theme names cannot contain \"/\" because it is reserved for automatic light/dark theme settings."
        )));
    }

    let mut missing_colors: IndexSet<String> = IndexSet::new();
    let mut other_errors: Vec<String> = Vec::new();

    for key in obj.keys() {
        if !matches!(
            key.as_str(),
            "$schema" | "name" | "appearance" | "vars" | "colors" | "export"
        ) {
            other_errors.push(format!("  - /: unexpected property \"{key}\""));
        }
    }

    let schema = match obj.get("$schema") {
        None => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => {
            other_errors.push("  - /$schema: must be string".to_string());
            None
        }
    };

    let name = match obj.get("name") {
        None => {
            other_errors.push("  - /: must have required property 'name'".to_string());
            String::new()
        }
        Some(Value::String(s)) => s.clone(),
        Some(_) => {
            other_errors.push("  - /name: must be string".to_string());
            String::new()
        }
    };

    let appearance = match obj.get("appearance") {
        None => None,
        Some(Value::String(s)) if s == "dark" => Some(ThemeJsonAppearance::Dark),
        Some(Value::String(s)) if s == "light" => Some(ThemeJsonAppearance::Light),
        Some(_) => {
            other_errors.push("  - /appearance: must be \"dark\" or \"light\"".to_string());
            None
        }
    };

    let vars = match obj.get("vars") {
        None => None,
        Some(Value::Object(map)) => {
            let mut parsed = IndexMap::new();
            for (k, v) in map {
                match ThemeColorValue::from_json(v) {
                    Some(cv) => {
                        parsed.insert(k.clone(), cv);
                    }
                    None => {
                        other_errors.push(format!("  - /vars/{k}: must be string or integer between 0 and 255"));
                    }
                }
            }
            Some(parsed)
        }
        Some(_) => {
            other_errors.push("  - /vars: must be object".to_string());
            None
        }
    };

    let colors = match obj.get("colors") {
        None => {
            other_errors.push("  - /: must have required property 'colors'".to_string());
            IndexMap::new()
        }
        Some(Value::Object(map)) => validate_colors_map(map, &mut missing_colors, &mut other_errors),
        Some(_) => {
            other_errors.push("  - /colors: must be object".to_string());
            IndexMap::new()
        }
    };

    let export = match obj.get("export") {
        None => None,
        Some(Value::Object(map)) => {
            let mut ex = ThemeExportJson::default();
            for (k, v) in map {
                match k.as_str() {
                    "pageBg" => match ThemeColorValue::from_json(v) {
                        Some(cv) => ex.page_bg = Some(cv),
                        None => other_errors.push("  - /export/pageBg: must be valid color value".to_string()),
                    },
                    "cardBg" => match ThemeColorValue::from_json(v) {
                        Some(cv) => ex.card_bg = Some(cv),
                        None => other_errors.push("  - /export/cardBg: must be valid color value".to_string()),
                    },
                    "infoBg" => match ThemeColorValue::from_json(v) {
                        Some(cv) => ex.info_bg = Some(cv),
                        None => other_errors.push("  - /export/infoBg: must be valid color value".to_string()),
                    },
                    other_key => {
                        other_errors.push(format!("  - /export: unexpected property \"{other_key}\""));
                    }
                }
            }
            Some(ex)
        }
        Some(_) => {
            other_errors.push("  - /export: must be object".to_string());
            None
        }
    };

    if !missing_colors.is_empty() || !other_errors.is_empty() {
        return Err(format_validation_error(label, &missing_colors, &other_errors));
    }

    Ok(ValidatedThemeJson {
        schema,
        name,
        appearance,
        vars,
        colors,
        export,
    })
}

fn validate_colors_map(
    map: &Map<String, Value>,
    missing_colors: &mut IndexSet<String>,
    other_errors: &mut Vec<String>,
) -> IndexMap<String, ThemeColorValue> {
    for &req in REQUIRED_THEME_COLOR_TOKENS {
        if !map.contains_key(req) {
            missing_colors.insert(req.to_string());
        }
    }
    let mut colors = IndexMap::new();
    for (k, v) in map {
        if !is_valid_token_key(k) {
            other_errors.push(format!("  - /colors: unexpected property \"{k}\""));
            continue;
        }
        match ThemeColorValue::from_json(v) {
            Some(cv) => {
                colors.insert(k.clone(), cv);
            }
            None => {
                other_errors.push(format!("  - /colors/{k}: must be string or integer between 0 and 255"));
            }
        }
    }
    colors
}
