//! Port of packages/durable/src/tools/path-utils.ts

use std::sync::LazyLock;

use indexmap::IndexSet;
use pi_chord::Context;
use regex::Regex;

use crate::env::{ExecutionEnv, get_or_throw};

const NARROW_NO_BREAK_SPACE: &str = "\u{202F}";

static UNICODE_SPACES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("[\u{00A0}\u{2000}-\u{200A}\u{202F}\u{205F}\u{3000}]").expect("valid regex"));

static AM_PM_DOT: LazyLock<Regex> = LazyLock::new(|| Regex::new("(?i) (AM|PM)\\.").expect("valid regex"));

fn normalize_tool_path(path: &str) -> String {
    let normalized = UNICODE_SPACES.replace_all(path, " ");
    normalized.strip_prefix('@').unwrap_or(&normalized).to_owned()
}

pub async fn resolve_tool_path(env: &dyn ExecutionEnv, path: &str, context: &Context) -> pi_js::Result<String> {
    let normalized = normalize_tool_path(path);
    get_or_throw(env.absolute_path(&normalized, context).await)
}

pub async fn resolve_read_tool_path(env: &dyn ExecutionEnv, path: &str, context: &Context) -> pi_js::Result<String> {
    let resolved = resolve_tool_path(env, path, context).await?;
    let mut variants = IndexSet::new();
    variants.insert(resolved.clone());
    variants.insert(
        AM_PM_DOT
            .replace_all(&resolved, |caps: &regex::Captures<'_>| {
                format!("{NARROW_NO_BREAK_SPACE}{}.", &caps[1])
            })
            .into_owned(),
    );
    let nfd = pi_js::str16::normalize_nfd(&resolved);
    variants.insert(nfd.clone());
    variants.insert(resolved.replace('\'', "\u{2019}"));
    variants.insert(nfd.replace('\'', "\u{2019}"));

    for variant in variants {
        if get_or_throw(env.exists(&variant, context).await)? {
            return Ok(variant);
        }
    }
    Ok(resolved)
}
