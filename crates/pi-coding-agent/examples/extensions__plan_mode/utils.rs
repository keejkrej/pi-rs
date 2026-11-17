//! Port of packages/coding-agent/examples/extensions/plan-mode/utils.ts

use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

static DESTRUCTIVE_PATTERNS: LazyLock<Vec<fancy_regex::Regex>> = LazyLock::new(|| {
    [
        r"(?i)\brm\b",
        r"(?i)\brmdir\b",
        r"(?i)\bmv\b",
        r"(?i)\bcp\b",
        r"(?i)\bmkdir\b",
        r"(?i)\btouch\b",
        r"(?i)\bchmod\b",
        r"(?i)\bchown\b",
        r"(?i)\bchgrp\b",
        r"(?i)\bln\b",
        r"(?i)\btee\b",
        r"(?i)\btruncate\b",
        r"(?i)\bdd\b",
        r"(?i)\bshred\b",
        r"(^|[^<])>(?!>)",
        r">>",
        r"(?i)\bnpm\s+(install|uninstall|update|ci|link|publish)",
        r"(?i)\byarn\s+(add|remove|install|publish)",
        r"(?i)\bpnpm\s+(add|remove|install|publish)",
        r"(?i)\bpip\s+(install|uninstall)",
        r"(?i)\bapt(-get)?\s+(install|remove|purge|update|upgrade)",
        r"(?i)\bbrew\s+(install|uninstall|upgrade)",
        r"(?i)\bgit\s+(add|commit|push|pull|merge|rebase|reset|checkout|branch\s+-[dD]|stash|cherry-pick|revert|tag|init|clone)",
        r"(?i)\bsudo\b",
        r"(?i)\bsu\b",
        r"(?i)\bkill\b",
        r"(?i)\bpkill\b",
        r"(?i)\bkillall\b",
        r"(?i)\breboot\b",
        r"(?i)\bshutdown\b",
        r"(?i)\bsystemctl\s+(start|stop|restart|enable|disable)",
        r"(?i)\bservice\s+\S+\s+(start|stop|restart)",
        r"(?i)\b(vim?|nano|emacs|code|subl)\b",
    ]
    .into_iter()
    .filter_map(|p| fancy_regex::Regex::new(p).ok())
    .collect()
});

static SAFE_PATTERNS: LazyLock<Vec<regex::Regex>> = LazyLock::new(|| {
    [
        r"^\s*cat\b",
        r"^\s*head\b",
        r"^\s*tail\b",
        r"^\s*less\b",
        r"^\s*more\b",
        r"^\s*grep\b",
        r"^\s*find\b",
        r"^\s*ls\b",
        r"^\s*pwd\b",
        r"^\s*echo\b",
        r"^\s*printf\b",
        r"^\s*wc\b",
        r"^\s*sort\b",
        r"^\s*uniq\b",
        r"^\s*diff\b",
        r"^\s*file\b",
        r"^\s*stat\b",
        r"^\s*du\b",
        r"^\s*df\b",
        r"^\s*tree\b",
        r"^\s*which\b",
        r"^\s*whereis\b",
        r"^\s*type\b",
        r"^\s*env\b",
        r"^\s*printenv\b",
        r"^\s*uname\b",
        r"^\s*whoami\b",
        r"^\s*id\b",
        r"^\s*date\b",
        r"^\s*cal\b",
        r"^\s*uptime\b",
        r"^\s*ps\b",
        r"^\s*top\b",
        r"^\s*htop\b",
        r"^\s*free\b",
        r"(?i)^\s*git\s+(status|log|diff|show|branch|remote|config\s+--get)",
        r"(?i)^\s*git\s+ls-",
        r"(?i)^\s*npm\s+(list|ls|view|info|search|outdated|audit)",
        r"(?i)^\s*yarn\s+(list|info|why|audit)",
        r"(?i)^\s*node\s+--version",
        r"(?i)^\s*python\s+--version",
        r"(?i)^\s*curl\s",
        r"(?i)^\s*wget\s+-O\s*-",
        r"^\s*jq\b",
        r"^\s*sed\s+-n",
        r"^\s*awk\b",
        r"^\s*rg\b",
        r"^\s*fd\b",
        r"^\s*bat\b",
        r"^\s*eza\b",
    ]
    .into_iter()
    .filter_map(|p| regex::Regex::new(p).ok())
    .collect()
});

static BOLD_ITALIC_RE: LazyLock<Option<regex::Regex>> =
    LazyLock::new(|| regex::Regex::new(r"\*{1,2}([^*]+)\*{1,2}").ok());
static CODE_RE: LazyLock<Option<regex::Regex>> = LazyLock::new(|| regex::Regex::new(r"`([^`]+)`").ok());
static PREFIX_VERB_RE: LazyLock<Option<regex::Regex>> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)^(Use|Run|Execute|Create|Write|Read|Check|Verify|Update|Modify|Add|Remove|Delete|Install)\s+(the\s+)?",
    )
    .ok()
});
static WHITESPACE_RE: LazyLock<Option<regex::Regex>> = LazyLock::new(|| regex::Regex::new(r"\s+").ok());
static PLAN_HEADER_RE: LazyLock<Option<regex::Regex>> =
    LazyLock::new(|| regex::Regex::new(r"(?i)\*{0,2}Plan:\*{0,2}\s*\n").ok());
static NUMBERED_STEP_RE: LazyLock<Option<regex::Regex>> =
    LazyLock::new(|| regex::Regex::new(r"(?m)^\s*(\d+)[.)]\s+\*{0,2}([^*\n]+)").ok());
static TRAILING_STARS_RE: LazyLock<Option<regex::Regex>> = LazyLock::new(|| regex::Regex::new(r"\*{1,2}$").ok());
static DONE_TAG_RE: LazyLock<Option<regex::Regex>> = LazyLock::new(|| regex::Regex::new(r"(?i)\[DONE:(\d+)\]").ok());

pub fn is_safe_command(command: &str) -> bool {
    let is_destructive = DESTRUCTIVE_PATTERNS
        .iter()
        .any(|p| matches!(p.is_match(command), Ok(true)));
    let is_safe = SAFE_PATTERNS.iter().any(|p| p.is_match(command));
    !is_destructive && is_safe
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub step: usize,
    pub text: String,
    pub completed: bool,
}

pub fn clean_step_text(text: &str) -> String {
    let mut cleaned = text.to_string();
    if let Some(re) = BOLD_ITALIC_RE.as_ref() {
        cleaned = re.replace_all(&cleaned, "$1").into_owned();
    }
    if let Some(re) = CODE_RE.as_ref() {
        cleaned = re.replace_all(&cleaned, "$1").into_owned();
    }
    if let Some(re) = PREFIX_VERB_RE.as_ref() {
        cleaned = re.replace(&cleaned, "").into_owned();
    }
    if let Some(re) = WHITESPACE_RE.as_ref() {
        cleaned = re.replace_all(&cleaned, " ").into_owned();
    }
    cleaned = cleaned.trim().to_string();

    let mut chars = cleaned.chars();
    if let Some(first) = chars.next() {
        cleaned = format!("{}{}", first.to_uppercase(), chars.as_str());
    }
    if cleaned.chars().count() > 50 {
        let prefix: String = cleaned.chars().take(47).collect();
        cleaned = format!("{prefix}...");
    }
    cleaned
}

pub fn extract_todo_items(message: &str) -> Vec<TodoItem> {
    let mut items = Vec::new();
    let Some(header_re) = PLAN_HEADER_RE.as_ref() else {
        return items;
    };
    let Some(header_match) = header_re.find(message) else {
        return items;
    };

    let plan_section = &message[header_match.end()..];
    let Some(numbered_re) = NUMBERED_STEP_RE.as_ref() else {
        return items;
    };

    for caps in numbered_re.captures_iter(plan_section) {
        let Some(raw_group) = caps.get(2) else {
            continue;
        };
        let mut text = raw_group.as_str().trim().to_string();
        if let Some(stars_re) = TRAILING_STARS_RE.as_ref() {
            text = stars_re.replace(&text, "").trim().to_string();
        }
        if text.chars().count() > 5 && !text.starts_with('`') && !text.starts_with('/') && !text.starts_with('-') {
            let cleaned = clean_step_text(&text);
            if cleaned.chars().count() > 3 {
                items.push(TodoItem {
                    step: items.len() + 1,
                    text: cleaned,
                    completed: false,
                });
            }
        }
    }
    items
}

pub fn extract_done_steps(message: &str) -> Vec<usize> {
    let mut steps = Vec::new();
    let Some(re) = DONE_TAG_RE.as_ref() else {
        return steps;
    };
    for caps in re.captures_iter(message) {
        if let Some(m) = caps.get(1)
            && let Ok(step) = m.as_str().parse::<usize>()
        {
            steps.push(step);
        }
    }
    steps
}

pub fn mark_completed_steps(text: &str, items: &mut [TodoItem]) -> usize {
    let done_steps = extract_done_steps(text);
    for step in &done_steps {
        if let Some(item) = items.iter_mut().find(|t| t.step == *step) {
            item.completed = true;
        }
    }
    done_steps.len()
}
