//! Port of packages/tui/src/fuzzy.ts
//!
//! Fuzzy matching utilities.
//! Matches if all query characters appear in order (not necessarily consecutive).
//! Lower score = better match.

#![allow(dead_code, unused_variables)]

use std::sync::Arc;

/// Field order is `matches`, then `score`.
///
/// `score` is an `f64` because the matcher adds `i * 0.1`.
#[derive(Clone, Debug, PartialEq)]
pub struct FuzzyMatch {
    pub matches: bool,
    pub score: f64,
}

fn is_word_boundary_unit(unit: u16) -> bool {
    match char::from_u32(u32::from(unit)) {
        Some(ch) => matches!(ch, '-' | '_' | '.' | '/' | ':') || pi_js::str16::is_js_whitespace(ch),
        None => false,
    }
}

fn score_positions(text_units: &[u16], positions: &[usize]) -> f64 {
    let mut score = 0.0_f64;
    let mut last_match_index: i64 = -1;
    let mut consecutive_matches = 0_usize;

    for &i in positions {
        let i_i64 = i as i64;
        // Reward consecutive matches (including i == 0 where last_match_index == -1 == i - 1)
        if last_match_index == i_i64 - 1 {
            consecutive_matches += 1;
            score -= (consecutive_matches * 5) as f64;
        } else {
            consecutive_matches = 0;
            // Penalize gaps
            if last_match_index >= 0 {
                score += ((i_i64 - last_match_index - 1) * 2) as f64;
            }
        }

        // Reward word boundary matches
        if i == 0 || is_word_boundary_unit(text_units[i - 1]) {
            score -= 10.0;
        }

        // Slight penalty for later matches
        score += (i as f64) * 0.1;

        last_match_index = i_i64;
    }

    score
}

fn find_subslice_from(haystack: &[u16], needle: &[u16], start: usize) -> Option<usize> {
    if needle.is_empty() {
        return (start <= haystack.len()).then_some(start);
    }
    if start + needle.len() > haystack.len() {
        return None;
    }
    haystack[start..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|rel| start + rel)
}

fn match_query(query_units: &[u16], text_units: &[u16]) -> FuzzyMatch {
    if query_units.is_empty() {
        return FuzzyMatch {
            matches: true,
            score: 0.0,
        };
    }

    if query_units.len() > text_units.len() {
        return FuzzyMatch {
            matches: false,
            score: 0.0,
        };
    }

    // Greedy alignment: each query character at its first occurrence after the previous one.
    let mut positions = Vec::with_capacity(query_units.len());
    let mut search_start = 0_usize;
    for &qu in query_units {
        let Some(rel) = text_units[search_start..].iter().position(|&tu| tu == qu) else {
            return FuzzyMatch {
                matches: false,
                score: 0.0,
            };
        };
        let i = search_start + rel;
        positions.push(i);
        search_start = i + 1;
    }

    let mut score = score_positions(text_units, &positions);

    // Also score every contiguous occurrence and keep the best alignment.
    let mut start_opt = find_subslice_from(text_units, query_units, 0);
    while let Some(start) = start_opt {
        for (k, pos) in positions.iter_mut().enumerate() {
            *pos = start + k;
        }
        score = score.min(score_positions(text_units, &positions));
        start_opt = find_subslice_from(text_units, query_units, start + 1);
    }

    if query_units == text_units {
        score -= 100.0;
    }

    FuzzyMatch { matches: true, score }
}

fn swap_alpha_numeric_query(query_lower: &str) -> Option<String> {
    let bytes = query_lower.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let first_is_letter = bytes[0].is_ascii_lowercase();
    let first_is_digit = bytes[0].is_ascii_digit();
    if !first_is_letter && !first_is_digit {
        return None;
    }

    let split_idx = if first_is_letter {
        bytes.iter().position(|b| !b.is_ascii_lowercase())?
    } else {
        bytes.iter().position(|b| !b.is_ascii_digit())?
    };

    let rest = &bytes[split_idx..];
    if rest.is_empty() {
        return None;
    }
    let rest_valid = if first_is_letter {
        rest.iter().all(|b| b.is_ascii_digit())
    } else {
        rest.iter().all(|b| b.is_ascii_lowercase())
    };
    if !rest_valid {
        return None;
    }

    Some(format!("{}{}", &query_lower[split_idx..], &query_lower[..split_idx]))
}

pub fn fuzzy_match(query: &str, text: &str) -> FuzzyMatch {
    let query_lower = query.to_lowercase();
    let text_lower = text.to_lowercase();

    let query_units = pi_js::str16::to_utf16(&query_lower);
    let text_units = pi_js::str16::to_utf16(&text_lower);

    let primary_match = match_query(&query_units, &text_units);
    if primary_match.matches {
        return primary_match;
    }

    let Some(swapped_query) = swap_alpha_numeric_query(&query_lower) else {
        return primary_match;
    };

    let swapped_units = pi_js::str16::to_utf16(&swapped_query);
    let swapped_match = match_query(&swapped_units, &text_units);
    if !swapped_match.matches {
        return primary_match;
    }

    FuzzyMatch {
        matches: true,
        score: swapped_match.score + 5.0,
    }
}

/// Filter and sort items by fuzzy match quality (best matches first).
/// Supports whitespace- and slash-separated tokens: all tokens must match.
pub fn fuzzy_filter<T: Clone>(items: &[T], query: &str, get_text: Arc<dyn Fn(&T) -> String + Send + Sync>) -> Vec<T> {
    let trimmed = pi_js::str16::trim(query);
    if trimmed.is_empty() {
        return items.to_vec();
    }

    let tokens: Vec<&str> = trimmed
        .split(|c: char| c == '/' || pi_js::str16::is_js_whitespace(c))
        .filter(|t| !t.is_empty())
        .collect();

    if tokens.is_empty() {
        return items.to_vec();
    }

    let mut results: Vec<(T, f64)> = Vec::new();

    for item in items {
        let text = get_text(item);
        let mut total_score = 0.0_f64;
        let mut all_match = true;

        for token in &tokens {
            let m = fuzzy_match(token, &text);
            if m.matches {
                total_score += m.score;
            } else {
                all_match = false;
                break;
            }
        }

        if all_match {
            results.push((item.clone(), total_score));
        }
    }

    results.sort_by(|a, b| a.1.total_cmp(&b.1));
    results.into_iter().map(|(item, _)| item).collect()
}
