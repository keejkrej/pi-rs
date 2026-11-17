//! Port of packages/coding-agent/src/utils/frontmatter.ts

use indexmap::IndexMap;
use serde_json::Value;

use crate::utils::text::strip_bom;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParsedFrontmatter<T = IndexMap<String, Value>> {
    pub frontmatter: T,
    pub body: String,
}

fn normalize_newlines(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}

fn extract_frontmatter(content: &str) -> (Option<String>, String) {
    let normalized = normalize_newlines(strip_bom(content));

    if !normalized.starts_with("---") {
        return (None, normalized);
    }

    let Some(rel_end) = normalized[3..].find("\n---") else {
        return (None, normalized);
    };
    let end_index = 3 + rel_end;

    let yaml_string = normalized.get(4..end_index).unwrap_or("").to_owned();
    let body = normalized.get(end_index + 4..).unwrap_or("").trim().to_owned();
    (Some(yaml_string), body)
}

pub fn parse_frontmatter(content: &str) -> ParsedFrontmatter<IndexMap<String, Value>> {
    let (yaml_string, body) = extract_frontmatter(content);
    let Some(yaml) = yaml_string else {
        return ParsedFrontmatter {
            frontmatter: IndexMap::new(),
            body,
        };
    };

    let parsed: Value = serde_yaml_ng::from_str(&yaml).unwrap_or(Value::Null);
    let frontmatter = match parsed {
        Value::Object(map) => map.into_iter().collect(),
        _ => IndexMap::new(),
    };
    ParsedFrontmatter { frontmatter, body }
}

pub fn try_parse_frontmatter(content: &str) -> pi_js::Result<ParsedFrontmatter<IndexMap<String, Value>>> {
    let (yaml_string, body) = extract_frontmatter(content);
    let Some(yaml) = yaml_string else {
        return Ok(ParsedFrontmatter {
            frontmatter: IndexMap::new(),
            body,
        });
    };

    let parsed: Value =
        serde_yaml_ng::from_str(&yaml).map_err(|err| pi_js::Error::msg(format!("YAMLParseError: {err}")))?;
    let frontmatter = match parsed {
        Value::Object(map) => map.into_iter().collect(),
        _ => IndexMap::new(),
    };
    Ok(ParsedFrontmatter { frontmatter, body })
}

pub fn strip_frontmatter(content: &str) -> String {
    parse_frontmatter(content).body
}
