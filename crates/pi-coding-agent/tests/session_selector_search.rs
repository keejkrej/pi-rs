//! Port of packages/coding-agent/test/session-selector-search.test.ts

use pi_coding_agent::modes::interactive::components::session_selector_search::{
    NameFilter, SessionInfo, SortMode, filter_and_sort_sessions,
};
use pi_js::time::testing::parse_date_utc;

fn make_session(id: &str, name: Option<&str>, modified_iso: &str, all_messages_text: &str) -> SessionInfo {
    SessionInfo {
        path: format!("/tmp/{id}.jsonl"),
        id: id.to_string(),
        cwd: String::new(),
        name: name.map(ToString::to_string),
        parent_session_path: None,
        created: 0,
        modified: parse_date_utc(modified_iso).expect("valid date"),
        message_count: 1,
        first_message: "(no messages)".to_string(),
        all_messages_text: all_messages_text.to_string(),
    }
}

#[test]
fn filters_by_quoted_phrase_with_whitespace_normalization() {
    let sessions = vec![
        make_session("a", None, "2026-01-01T00:00:00.000Z", "node\n\n   cve was discussed"),
        make_session("b", None, "2026-01-02T00:00:00.000Z", "node something else"),
    ];
    let result = filter_and_sort_sessions(&sessions, "\"node cve\"", SortMode::Recent, None);
    let ids: Vec<&str> = result.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["a"]);
}

#[test]
fn filters_by_regex_re_and_is_case_insensitive() {
    let sessions = vec![
        make_session("a", None, "2026-01-02T00:00:00.000Z", "Brave is great"),
        make_session("b", None, "2026-01-03T00:00:00.000Z", "bravery is not the same"),
    ];
    let result = filter_and_sort_sessions(&sessions, "re:\\bbrave\\b", SortMode::Recent, None);
    let ids: Vec<&str> = result.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["a"]);
}

#[test]
fn recent_sort_preserves_input_order() {
    let sessions = vec![
        make_session("newer", None, "2026-01-03T00:00:00.000Z", "brave"),
        make_session("older", None, "2026-01-01T00:00:00.000Z", "brave"),
        make_session("nomatch", None, "2026-01-04T00:00:00.000Z", "something else"),
    ];
    let result = filter_and_sort_sessions(&sessions, "\"brave\"", SortMode::Recent, None);
    let ids: Vec<&str> = result.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["newer", "older"]);
}

#[test]
fn relevance_sort_orders_by_score_and_tie_breaks_by_modified_desc() {
    let sessions = vec![
        make_session("message", None, "2026-01-03T00:00:00.000Z", "brave"),
        make_session("name", Some("Brave work"), "2026-01-01T00:00:00.000Z", "xxxx"),
    ];
    let result1 = filter_and_sort_sessions(&sessions, "\"brave\"", SortMode::Relevance, None);
    let ids1: Vec<&str> = result1.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids1, vec!["name", "message"]);

    let tie_sessions = vec![
        make_session("newer", None, "2026-01-03T00:00:00.000Z", "brave"),
        make_session("older", None, "2026-01-01T00:00:00.000Z", "brave"),
    ];
    let result2 = filter_and_sort_sessions(&tie_sessions, "\"brave\"", SortMode::Relevance, None);
    let ids2: Vec<&str> = result2.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids2, vec!["newer", "older"]);
}

#[test]
fn ignores_the_position_of_a_match_in_message_text() {
    let filler = "lorem ipsum dolor sit amet ".repeat(400);
    let sessions = vec![
        make_session("early", None, "2026-01-01T00:00:00.000Z", &format!("opus {filler}")),
        make_session(
            "deep",
            None,
            "2026-01-02T00:00:00.000Z",
            &format!("{filler} we discussed the opus model"),
        ),
        make_session("scattered", None, "2026-01-03T00:00:00.000Z", &filler),
    ];
    let result = filter_and_sort_sessions(&sessions, "opus", SortMode::Relevance, None);
    let ids: Vec<&str> = result.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["deep", "early"]);
}

#[test]
fn does_not_match_scattered_characters_in_session_ids() {
    let sessions = vec![
        make_session(
            "019e9024-aaaa-7bbb-8ccc-dddddddddddd",
            None,
            "2026-01-03T00:00:00.000Z",
            "unrelated",
        ),
        make_session(
            "01a1ffff-aaaa-7bbb-8ccc-dddddddddddd",
            Some("#1024 TDD bare env"),
            "2026-01-01T00:00:00.000Z",
            "unrelated",
        ),
    ];

    let r1 = filter_and_sort_sessions(&sessions, "1024", SortMode::Relevance, None);
    assert_eq!(
        r1.iter().map(|s| s.name.as_deref()).collect::<Vec<_>>(),
        vec![Some("#1024 TDD bare env")]
    );
    let r2 = filter_and_sort_sessions(&sessions, "9024-aa", SortMode::Relevance, None);
    assert_eq!(
        r2.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        vec!["019e9024-aaaa-7bbb-8ccc-dddddddddddd"]
    );
}

#[test]
fn ranks_name_text_then_message_text_then_scattered_name_matches() {
    let sessions = vec![
        make_session(
            "scattered-name",
            Some("c-a-c-h-e notes"),
            "2026-01-04T00:00:00.000Z",
            "unrelated",
        ),
        make_session(
            "message",
            Some("Other work"),
            "2026-01-03T00:00:00.000Z",
            "A discussion mentioning the cache.",
        ),
        make_session("name", Some("Cache work"), "2026-01-01T00:00:00.000Z", "Summary"),
    ];
    let result = filter_and_sort_sessions(&sessions, "cache", SortMode::Relevance, None);
    let ids: Vec<&str> = result.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["name", "message", "scattered-name"]);
}

#[test]
fn matches_each_token_within_a_single_field() {
    let sessions = vec![make_session(
        "split",
        Some("Fuzzy"),
        "2026-01-01T00:00:00.000Z",
        "search ranking",
    )];

    let r1 = filter_and_sort_sessions(&sessions, "fuzzy ranking", SortMode::Relevance, None);
    assert_eq!(r1.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), vec!["split"]);
    assert!(filter_and_sort_sessions(&sessions, "zzysea", SortMode::Relevance, None).is_empty());
    assert!(filter_and_sort_sessions(&sessions, "re:fuzzy search", SortMode::Relevance, None).is_empty());
}

#[test]
fn ranks_regex_name_matches_first() {
    let sessions = vec![
        make_session("message", None, "2026-01-02T00:00:00.000Z", "brave"),
        make_session("name", Some("Brave"), "2026-01-01T00:00:00.000Z", "xxxx"),
    ];
    let result = filter_and_sort_sessions(&sessions, "re:\\bbrave\\b", SortMode::Relevance, None);
    let ids: Vec<&str> = result.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["name", "message"]);
}

#[test]
fn returns_empty_list_for_invalid_regex() {
    let sessions = vec![make_session("a", None, "2026-01-01T00:00:00.000Z", "brave")];
    assert!(filter_and_sort_sessions(&sessions, "re:(", SortMode::Recent, None).is_empty());
}

#[test]
fn name_filter_all_and_named() {
    let sessions = vec![
        make_session("named1", Some("My Project"), "2026-01-03T00:00:00.000Z", "blueberry"),
        make_session("named2", Some("Another Named"), "2026-01-02T00:00:00.000Z", "blueberry"),
        make_session("other1", None, "2026-01-04T00:00:00.000Z", "blueberry"),
        make_session("other2", None, "2026-01-01T00:00:00.000Z", "blueberry"),
    ];

    let all_ids: Vec<String> = filter_and_sort_sessions(&sessions, "", SortMode::Recent, Some(NameFilter::All))
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(all_ids, vec!["named1", "named2", "other1", "other2"]);

    let named_ids: Vec<String> = filter_and_sort_sessions(&sessions, "", SortMode::Recent, Some(NameFilter::Named))
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(named_ids, vec!["named1", "named2"]);

    let searched_named: Vec<String> =
        filter_and_sort_sessions(&sessions, "blueberry", SortMode::Recent, Some(NameFilter::Named))
            .into_iter()
            .map(|s| s.id)
            .collect();
    assert_eq!(searched_named, vec!["named1", "named2"]);
}

#[test]
fn excludes_whitespace_only_names_from_named_filter() {
    let sessions = vec![
        make_session("whitespace", Some("   "), "2026-01-01T00:00:00.000Z", "test"),
        make_session("empty", Some(""), "2026-01-02T00:00:00.000Z", "test"),
        make_session("named", Some("Real Name"), "2026-01-03T00:00:00.000Z", "test"),
    ];
    let ids: Vec<String> = filter_and_sort_sessions(&sessions, "", SortMode::Recent, Some(NameFilter::Named))
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(ids, vec!["named"]);
}
