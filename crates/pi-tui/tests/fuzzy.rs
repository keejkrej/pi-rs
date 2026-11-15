//! Port of packages/tui/test/fuzzy.test.ts

use std::sync::Arc;

use pi_tui::{fuzzy_filter, fuzzy_match};

mod fuzzy_match_tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn empty_query_matches_everything_with_score_0() {
        let result = fuzzy_match("", "anything");
        assert_eq!(result.matches, true);
        assert_eq!(result.score, 0.0);
    }

    #[test]
    fn query_longer_than_text_does_not_match() {
        let result = fuzzy_match("longquery", "short");
        assert_eq!(result.matches, false);
    }

    #[test]
    fn exact_match_has_good_score() {
        let result = fuzzy_match("test", "test");
        assert_eq!(result.matches, true);
        assert!(result.score < 0.0); // Should be negative due to consecutive bonuses
    }

    #[test]
    fn characters_must_appear_in_order() {
        let match_in_order = fuzzy_match("abc", "aXbXc");
        assert_eq!(match_in_order.matches, true);

        let match_out_of_order = fuzzy_match("abc", "cba");
        assert_eq!(match_out_of_order.matches, false);
    }

    #[test]
    fn case_insensitive_matching() {
        let result = fuzzy_match("ABC", "abc");
        assert_eq!(result.matches, true);

        let result2 = fuzzy_match("abc", "ABC");
        assert_eq!(result2.matches, true);
    }

    #[test]
    fn consecutive_matches_score_better_than_scattered_matches() {
        let consecutive = fuzzy_match("foo", "foobar");
        let scattered = fuzzy_match("foo", "f_o_o_bar");

        assert_eq!(consecutive.matches, true);
        assert_eq!(scattered.matches, true);
        assert!(consecutive.score < scattered.score);
    }

    #[test]
    fn word_boundary_matches_score_better() {
        let at_boundary = fuzzy_match("fb", "foo-bar");
        let not_at_boundary = fuzzy_match("fb", "afbx");

        assert_eq!(at_boundary.matches, true);
        assert_eq!(not_at_boundary.matches, true);
        assert!(at_boundary.score < not_at_boundary.score);
    }

    // #11220: greedy alignment took o, p, u from "openrouter", so the real "opus" scored worst
    #[test]
    fn scores_a_contiguous_occurrence_instead_of_an_earlier_scattered_alignment() {
        let opus = fuzzy_match("opus", "openrouter openrouter/anthropic/claude-opus-4.1");
        let sonnet = fuzzy_match("opus", "openrouter openrouter/anthropic/claude-sonnet-4.5");
        let gpt_oss = fuzzy_match("opus", "openrouter openrouter/openai/gpt-oss-120b");

        assert!(opus.score < sonnet.score);
        assert!(opus.score < gpt_oss.score);
    }

    #[test]
    fn uses_the_best_contiguous_occurrence_not_the_first() {
        let embedded_first = fuzzy_match("opus", "xxopus opus");
        let boundary_only = fuzzy_match("opus", "xxxxxx opus");

        assert_eq!(embedded_first.score, boundary_only.score);
    }

    #[test]
    fn matches_swapped_alpha_numeric_tokens() {
        let result = fuzzy_match("codex52", "gpt-5.2-codex");
        assert_eq!(result.matches, true);
    }
}

mod fuzzy_filter_tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn empty_query_returns_all_items_unchanged() {
        let items = vec!["apple", "banana", "cherry"];
        let result = fuzzy_filter(&items, "", Arc::new(|x: &&str| (*x).to_string()));
        assert_eq!(result, items);
    }

    #[test]
    fn filters_out_non_matching_items() {
        let items = vec!["apple", "banana", "cherry"];
        let result = fuzzy_filter(&items, "an", Arc::new(|x: &&str| (*x).to_string()));
        assert!(result.contains(&"banana"));
        assert!(!result.contains(&"apple"));
        assert!(!result.contains(&"cherry"));
    }

    #[test]
    fn sorts_results_by_match_quality() {
        let items = vec!["a_p_p", "app", "application"];
        let result = fuzzy_filter(&items, "app", Arc::new(|x: &&str| (*x).to_string()));

        // "app" should be first (exact consecutive match at start)
        assert_eq!(result[0], "app");
    }

    #[test]
    fn prioritizes_exact_matches_over_longer_prefix_matches() {
        let items = vec!["clone", "cl"];
        let result = fuzzy_filter(&items, "cl", Arc::new(|x: &&str| (*x).to_string()));

        assert_eq!(result, vec!["cl", "clone"]);
    }

    #[test]
    fn works_with_custom_get_text_function() {
        #[derive(Clone, Debug, PartialEq)]
        struct Item {
            name: &'static str,
            id: i64,
        }
        let items = vec![
            Item { name: "foo", id: 1 },
            Item { name: "bar", id: 2 },
            Item { name: "foobar", id: 3 },
        ];
        let result = fuzzy_filter(&items, "foo", Arc::new(|item: &Item| item.name.to_string()));

        assert_eq!(result.len(), 2);
        let names: Vec<&str> = result.iter().map(|r| r.name).collect();
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"foobar"));
    }

    // #11220: "radius radius/gpt" contains a, s, t, r, a in order, so every radius model matched there
    #[test]
    fn ranks_a_model_containing_the_query_above_models_matching_a_shared_provider_prefix() {
        let items = vec![
            "radius radius/gpt-5.4 radius gpt-5.4 GPT 5.4",
            "radius radius/agentic radius agentic Agentic",
            "radius radius/gpt-6-sol radius gpt-6-sol GPT 6 Sol",
            "radius radius/gpt-6-astra radius gpt-6-astra GPT 6 Astra",
        ];
        let result = fuzzy_filter(&items, "astra", Arc::new(|x: &&str| (*x).to_string()));

        assert_eq!(result[0], items[3]);
    }

    #[test]
    fn matches_slash_separated_provider_model_queries_against_reordered_text() {
        #[derive(Clone, Debug, PartialEq, Eq)]
        struct Model {
            id: &'static str,
            provider: &'static str,
        }
        let item = Model {
            id: "gpt-5.5",
            provider: "openai-codex",
        };
        let result = fuzzy_filter(
            std::slice::from_ref(&item),
            "openai-codex/gpt-5.5",
            Arc::new(|model: &Model| format!("{} {}", model.id, model.provider)),
        );

        assert_eq!(result, vec![item]);
    }
}
