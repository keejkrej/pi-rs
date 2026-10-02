/// Fuzzy matching utilities aligned with `@earendil-works/pi-tui`.
///
/// Matches when all query characters appear in order in the text. Lower scores
/// are better matches.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FuzzyMatch {
    pub matches: bool,
    pub score: f64,
}

pub fn fuzzy_match(query: &str, text: &str) -> FuzzyMatch {
    let query_lower = query.to_lowercase();
    let text_lower = text.to_lowercase();

    fn match_query(normalized_query: &str, text_lower: &str) -> FuzzyMatch {
        if normalized_query.is_empty() {
            return FuzzyMatch {
                matches: true,
                score: 0.0,
            };
        }
        if normalized_query.chars().count() > text_lower.chars().count() {
            return FuzzyMatch {
                matches: false,
                score: 0.0,
            };
        }

        let query_chars = normalized_query.chars().collect::<Vec<_>>();
        let text_chars = text_lower.chars().collect::<Vec<_>>();
        let mut query_index = 0;
        let mut score = 0.0;
        let mut last_match_index: isize = -1;
        let mut consecutive_matches = 0.0;

        for (i, ch) in text_chars.iter().enumerate() {
            if query_index >= query_chars.len() {
                break;
            }
            if *ch == query_chars[query_index] {
                let is_word_boundary = i == 0
                    || text_chars.get(i - 1).is_some_and(|prev| {
                        matches!(prev, ' ' | '\t' | '\n' | '-' | '_' | '.' | '/' | ':')
                    });

                if last_match_index == i as isize - 1 {
                    consecutive_matches += 1.0;
                    score -= consecutive_matches * 5.0;
                } else {
                    consecutive_matches = 0.0;
                    if last_match_index >= 0 {
                        score += (i as isize - last_match_index - 1) as f64 * 2.0;
                    }
                }

                if is_word_boundary {
                    score -= 10.0;
                }
                score += i as f64 * 0.1;
                last_match_index = i as isize;
                query_index += 1;
            }
        }

        if query_index < query_chars.len() {
            return FuzzyMatch {
                matches: false,
                score: 0.0,
            };
        }
        if normalized_query == text_lower {
            score -= 100.0;
        }
        FuzzyMatch {
            matches: true,
            score,
        }
    }

    let primary_match = match_query(&query_lower, &text_lower);
    if primary_match.matches {
        return primary_match;
    }

    let split_alpha_numeric = |s: &str| -> Option<String> {
        let chars = s.chars().collect::<Vec<_>>();
        let split = chars.iter().position(|ch| ch.is_ascii_digit())?;
        if split == 0 {
            return None;
        }
        if chars[..split].iter().all(|ch| ch.is_ascii_lowercase())
            && chars[split..].iter().all(|ch| ch.is_ascii_digit())
        {
            Some(format!(
                "{}{}",
                chars[split..].iter().collect::<String>(),
                chars[..split].iter().collect::<String>()
            ))
        } else {
            None
        }
    };
    let split_numeric_alpha = |s: &str| -> Option<String> {
        let chars = s.chars().collect::<Vec<_>>();
        let split = chars.iter().position(|ch| ch.is_ascii_lowercase())?;
        if split == 0 {
            return None;
        }
        if chars[..split].iter().all(|ch| ch.is_ascii_digit())
            && chars[split..].iter().all(|ch| ch.is_ascii_lowercase())
        {
            Some(format!(
                "{}{}",
                chars[split..].iter().collect::<String>(),
                chars[..split].iter().collect::<String>()
            ))
        } else {
            None
        }
    };

    let Some(swapped_query) =
        split_alpha_numeric(&query_lower).or_else(|| split_numeric_alpha(&query_lower))
    else {
        return primary_match;
    };
    let swapped_match = match_query(&swapped_query, &text_lower);
    if !swapped_match.matches {
        return primary_match;
    }
    FuzzyMatch {
        matches: true,
        score: swapped_match.score + 5.0,
    }
}

pub fn fuzzy_filter<T, F>(items: Vec<T>, query: &str, get_text: F) -> Vec<T>
where
    F: Fn(&T) -> String,
{
    if query.trim().is_empty() {
        return items;
    }

    let tokens = query.split_whitespace().collect::<Vec<_>>();
    if tokens.is_empty() {
        return items;
    }

    let mut results = Vec::new();
    for item in items {
        let text = get_text(&item);
        let mut total_score = 0.0;
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
            results.push((item, total_score));
        }
    }

    results.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    results.into_iter().map(|(item, _)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_filter_orders_best_score_first() {
        let matches = fuzzy_filter(
            vec!["src/session.rs", "settings.rs", "main.rs"],
            "sr",
            |item| item.to_string(),
        );
        assert_eq!(matches[0], "src/session.rs");
    }

    #[test]
    fn fuzzy_match_supports_token_swap() {
        assert!(fuzzy_match("abc123", "123abc").matches);
    }
}
