//! Port of packages/coding-agent/src/modes/interactive/components/session-selector-search.ts

use pi_tui::fuzzy_match;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

// WIRE(@tui-w3): Replace local mirror `SessionInfo` with `crate::core::session_manager::SessionInfo` once `mcp-w3` merges onto main.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub path: String,
    pub id: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_path: Option<String>,
    pub created: i64,
    pub modified: i64,
    pub message_count: usize,
    pub first_message: String,
    pub all_messages_text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortMode {
    Threaded,
    Recent,
    Relevance,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NameFilter {
    #[default]
    All,
    Named,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchQueryMode {
    Tokens,
    Regex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchTokenKind {
    Fuzzy,
    Phrase,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchToken {
    pub kind: SearchTokenKind,
    pub value: String,
}

#[derive(Clone, Debug)]
pub struct ParsedSearchQuery {
    pub mode: SearchQueryMode,
    pub tokens: Vec<SearchToken>,
    pub regex: Option<Regex>,
    /// If set, parsing failed and we should treat query as non-matching.
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatchResult {
    pub matches: bool,
    /// Lower is better; only meaningful when `matches == true`.
    pub score: f64,
}

const TIER_NAME: f64 = 0.0;
const TIER_TEXT: f64 = 1.0;
const TIER_FUZZY: f64 = 2.0;
const TIER_WEIGHT: f64 = 1_000_000.0;

fn normalize_whitespace_lower(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_ws = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !last_ws {
                out.push(' ');
                last_ws = true;
            }
        } else {
            for lc in ch.to_lowercase() {
                out.push(lc);
            }
            last_ws = false;
        }
    }
    out.trim().to_string()
}

fn tier_score(tier: f64, score: f64) -> f64 {
    tier * TIER_WEIGHT + score
}

struct SessionSearchView<'a> {
    session: &'a SessionInfo,
    messages_lower: Option<String>,
    messages_normalized: Option<String>,
}

impl<'a> SessionSearchView<'a> {
    fn new(session: &'a SessionInfo) -> Self {
        Self {
            session,
            messages_lower: None,
            messages_normalized: None,
        }
    }

    fn messages_lower(&mut self) -> &str {
        if self.messages_lower.is_none() {
            self.messages_lower = Some(self.session.all_messages_text.to_lowercase());
        }
        self.messages_lower.as_deref().unwrap_or("")
    }

    fn messages_normalized(&mut self) -> &str {
        if self.messages_normalized.is_none() {
            let lower = self.messages_lower().to_string();
            let norm = normalize_whitespace_lower(&lower);
            self.messages_normalized = Some(norm);
        }
        self.messages_normalized.as_deref().unwrap_or("")
    }
}

fn match_literal_token(view: &mut SessionSearchView<'_>, value_lower: &str) -> Option<f64> {
    let name_lower = view.session.name.as_deref().map(str::to_lowercase).unwrap_or_default();
    if name_lower.contains(value_lower) {
        return Some(tier_score(TIER_NAME, fuzzy_match(value_lower, &name_lower).score));
    }
    if view.session.id.to_lowercase().contains(value_lower) {
        return Some(tier_score(TIER_TEXT, 0.0));
    }
    if view.messages_lower().contains(value_lower) {
        return Some(tier_score(TIER_TEXT, 0.0));
    }
    if view.session.cwd.to_lowercase().contains(value_lower) {
        return Some(tier_score(TIER_TEXT, 0.0));
    }
    None
}

fn match_fuzzy_token(view: &mut SessionSearchView<'_>, value: &str) -> Option<f64> {
    let lower = value.to_lowercase();
    if let Some(literal) = match_literal_token(view, &lower) {
        return Some(literal);
    }

    let mut best: Option<f64> = None;
    for field in [view.session.name.as_deref().unwrap_or(""), view.session.cwd.as_str()] {
        let m = fuzzy_match(value, field);
        if m.matches && best.is_none_or(|b| m.score < b) {
            best = Some(m.score);
        }
    }
    best.map(|score| tier_score(TIER_FUZZY, score))
}

fn match_phrase_token(view: &mut SessionSearchView<'_>, phrase: &str) -> Option<f64> {
    let name_norm = normalize_whitespace_lower(view.session.name.as_deref().unwrap_or(""));
    if name_norm.contains(phrase) {
        return Some(tier_score(TIER_NAME, fuzzy_match(phrase, &name_norm).score));
    }
    if view.session.id.to_lowercase().contains(phrase) {
        return Some(tier_score(TIER_TEXT, 0.0));
    }
    if view.messages_normalized().contains(phrase) {
        return Some(tier_score(TIER_TEXT, 0.0));
    }
    if normalize_whitespace_lower(&view.session.cwd).contains(phrase) {
        return Some(tier_score(TIER_TEXT, 0.0));
    }
    None
}

fn match_regex(session: &SessionInfo, regex: &Regex) -> Option<f64> {
    if let Some(ref name) = session.name
        && !name.is_empty()
        && regex.is_match(name)
    {
        return Some(tier_score(TIER_NAME, 0.0));
    }
    if regex.is_match(&session.id) || regex.is_match(&session.all_messages_text) || regex.is_match(&session.cwd) {
        return Some(tier_score(TIER_TEXT, 0.0));
    }
    None
}

pub fn has_session_name(session: &SessionInfo) -> bool {
    session.name.as_deref().is_some_and(|n| !n.trim().is_empty())
}

fn matches_name_filter(session: &SessionInfo, filter: NameFilter) -> bool {
    match filter {
        NameFilter::All => true,
        NameFilter::Named => has_session_name(session),
    }
}

pub fn parse_search_query(query: &str) -> ParsedSearchQuery {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return ParsedSearchQuery {
            mode: SearchQueryMode::Tokens,
            tokens: Vec::new(),
            regex: None,
            error: None,
        };
    }

    if let Some(rest) = trimmed.strip_prefix("re:") {
        let pattern = rest.trim();
        if pattern.is_empty() {
            return ParsedSearchQuery {
                mode: SearchQueryMode::Regex,
                tokens: Vec::new(),
                regex: None,
                error: Some("Empty regex".to_string()),
            };
        }
        return match RegexBuilder::new(pattern).case_insensitive(true).build() {
            Ok(re) => ParsedSearchQuery {
                mode: SearchQueryMode::Regex,
                tokens: Vec::new(),
                regex: Some(re),
                error: None,
            },
            Err(err) => ParsedSearchQuery {
                mode: SearchQueryMode::Regex,
                tokens: Vec::new(),
                regex: None,
                error: Some(err.to_string()),
            },
        };
    }

    let mut tokens: Vec<SearchToken> = Vec::new();
    let mut buf = String::new();
    let mut in_quote = false;

    let flush = |kind: SearchTokenKind, buf: &mut String, tokens: &mut Vec<SearchToken>| {
        let v = buf.trim().to_string();
        buf.clear();
        if !v.is_empty() {
            tokens.push(SearchToken { kind, value: v });
        }
    };

    for ch in trimmed.chars() {
        if ch == '"' {
            if in_quote {
                flush(SearchTokenKind::Phrase, &mut buf, &mut tokens);
                in_quote = false;
            } else {
                flush(SearchTokenKind::Fuzzy, &mut buf, &mut tokens);
                in_quote = true;
            }
            continue;
        }
        if !in_quote && ch.is_whitespace() {
            flush(SearchTokenKind::Fuzzy, &mut buf, &mut tokens);
            continue;
        }
        buf.push(ch);
    }

    if in_quote {
        let fallback = trimmed
            .split_whitespace()
            .filter(|t| !t.is_empty())
            .map(|t| SearchToken {
                kind: SearchTokenKind::Fuzzy,
                value: t.to_string(),
            })
            .collect();
        return ParsedSearchQuery {
            mode: SearchQueryMode::Tokens,
            tokens: fallback,
            regex: None,
            error: None,
        };
    }

    flush(SearchTokenKind::Fuzzy, &mut buf, &mut tokens);
    ParsedSearchQuery {
        mode: SearchQueryMode::Tokens,
        tokens,
        regex: None,
        error: None,
    }
}

pub fn match_session(session: &SessionInfo, parsed: &ParsedSearchQuery) -> MatchResult {
    if parsed.mode == SearchQueryMode::Regex {
        let Some(ref re) = parsed.regex else {
            return MatchResult {
                matches: false,
                score: 0.0,
            };
        };
        return match match_regex(session, re) {
            Some(score) => MatchResult { matches: true, score },
            None => MatchResult {
                matches: false,
                score: 0.0,
            },
        };
    }

    let mut total_score = 0.0;
    let mut view = SessionSearchView::new(session);

    for token in &parsed.tokens {
        let score = match token.kind {
            SearchTokenKind::Phrase => {
                let phrase = normalize_whitespace_lower(&token.value);
                if phrase.is_empty() {
                    continue;
                }
                match_phrase_token(&mut view, &phrase)
            }
            SearchTokenKind::Fuzzy => match_fuzzy_token(&mut view, &token.value),
        };
        let Some(s) = score else {
            return MatchResult {
                matches: false,
                score: 0.0,
            };
        };
        total_score += s;
    }

    MatchResult {
        matches: true,
        score: total_score,
    }
}

pub fn filter_and_sort_sessions(
    sessions: &[SessionInfo],
    query: &str,
    sort_mode: SortMode,
    name_filter: Option<NameFilter>,
) -> Vec<SessionInfo> {
    let filter = name_filter.unwrap_or(NameFilter::All);
    let name_filtered: Vec<SessionInfo> = match filter {
        NameFilter::All => sessions.to_vec(),
        NameFilter::Named => sessions
            .iter()
            .filter(|s| matches_name_filter(s, filter))
            .cloned()
            .collect(),
    };

    if query.trim().is_empty() {
        return name_filtered;
    }

    let parsed = parse_search_query(query);
    if parsed.error.is_some() {
        return Vec::new();
    }

    if sort_mode == SortMode::Recent {
        return name_filtered
            .into_iter()
            .filter(|s| match_session(s, &parsed).matches)
            .collect();
    }

    let mut scored: Vec<(SessionInfo, f64)> = name_filtered
        .into_iter()
        .filter_map(|s| {
            let res = match_session(&s, &parsed);
            res.matches.then_some((s, res.score))
        })
        .collect();

    scored.sort_by(|(a, a_score), (b, b_score)| a_score.total_cmp(b_score).then_with(|| b.modified.cmp(&a.modified)));

    scored.into_iter().map(|(s, _)| s).collect()
}
