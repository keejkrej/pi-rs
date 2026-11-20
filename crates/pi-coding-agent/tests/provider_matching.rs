//! Port of packages/coding-agent/test/provider-matching.test.ts

use pi_coding_agent::core::provider_matching::{NamedProvider, find_matching_providers};
use pretty_assertions::assert_eq;

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProviderSpec {
    id: &'static str,
    name: &'static str,
    auth_type: &'static str,
}

impl NamedProvider for ProviderSpec {
    fn provider_id(&self) -> &str {
        self.id
    }

    fn provider_name(&self) -> &str {
        self.name
    }
}

fn sample_providers() -> Vec<ProviderSpec> {
    vec![
        ProviderSpec {
            id: "first",
            name: "Shared Name",
            auth_type: "oauth",
        },
        ProviderSpec {
            id: "first",
            name: "Shared Name",
            auth_type: "api_key",
        },
        ProviderSpec {
            id: "other",
            name: "FIRST",
            auth_type: "api_key",
        },
        ProviderSpec {
            id: "second",
            name: "Second Provider",
            auth_type: "oauth",
        },
    ]
}

#[test]
fn matches_ids_and_display_names_case_insensitively_with_surrounding_whitespace() {
    let providers = sample_providers();
    assert_eq!(find_matching_providers(&providers, "  sEcOnD  "), vec![&providers[3]]);
    assert_eq!(
        find_matching_providers(&providers, "\tSECOND PROVIDER\n"),
        vec![&providers[3]]
    );
}

#[test]
fn keeps_all_matching_providers_and_auth_methods_in_input_order() {
    let providers = sample_providers();
    let matched = find_matching_providers(&providers, "first");
    assert_eq!(matched, vec![&providers[0], &providers[1], &providers[2]]);
    assert_eq!(matched[0], &providers[0]);
    assert_eq!(matched[1].auth_type, "api_key");
    assert_eq!(
        find_matching_providers(&providers, "shared name"),
        vec![&providers[0], &providers[1]]
    );
}

#[test]
fn does_not_fuzzy_match_unknown_or_partial_references() {
    let providers = sample_providers();
    assert!(find_matching_providers(&providers, "unknown").is_empty());
    assert!(find_matching_providers(&providers, "sec").is_empty());
    let empty: Vec<ProviderSpec> = Vec::new();
    assert!(find_matching_providers(&empty, "first").is_empty());
}

#[test]
fn never_matches_an_empty_reference_even_against_empty_display_names() {
    let providers = sample_providers();
    let unnamed = vec![ProviderSpec {
        id: "unnamed",
        name: "",
        auth_type: "oauth",
    }];
    assert!(find_matching_providers(&unnamed, "").is_empty());
    assert!(find_matching_providers(&providers, " \t ").is_empty());
}
