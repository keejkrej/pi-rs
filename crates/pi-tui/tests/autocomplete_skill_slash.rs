//! Port of packages/tui/test/autocomplete-skill-slash.test.ts

use std::sync::Arc;

use pi_js::abort::AbortController;
use pi_tui::autocomplete::{CombinedCommand, GetSuggestionsOptions};
use pi_tui::{AutocompleteItem, CombinedAutocompleteProvider, SlashCommand};

fn make_commands() -> Vec<CombinedCommand> {
    vec![
        CombinedCommand::SlashCommand(SlashCommand {
            name: "skill:deep-research".to_string(),
            description: Some("Multi-agent deep research".to_string()),
            argument_hint: None,
            get_argument_completions: None,
        }),
        CombinedCommand::SlashCommand(SlashCommand {
            name: "skill:research-idea".to_string(),
            description: Some("Refine a raw idea into a falsifiable seed".to_string()),
            argument_hint: None,
            get_argument_completions: None,
        }),
        CombinedCommand::SlashCommand(SlashCommand {
            name: "skill:to-sidecar".to_string(),
            description: Some("Route work to a sidecar".to_string()),
            argument_hint: None,
            get_argument_completions: None,
        }),
        CombinedCommand::SlashCommand(SlashCommand {
            name: "skill:brainstorm".to_string(),
            description: Some("Generate ideas".to_string()),
            argument_hint: None,
            get_argument_completions: None,
        }),
        CombinedCommand::SlashCommand(SlashCommand {
            name: "model".to_string(),
            description: Some("Select the active model".to_string()),
            argument_hint: None,
            get_argument_completions: None,
        }),
    ]
}

async fn suggestions_for(prefix: &str) -> Vec<String> {
    let cwd = pi_js::env::cwd();
    let provider = CombinedAutocompleteProvider::new(Some(make_commands()), &cwd, None);
    let line = format!("/{prefix}");
    let controller = AbortController::new();
    let result = provider
        .get_suggestions(
            &[line.clone()],
            0,
            pi_js::str16::len(&line) as i64,
            GetSuggestionsOptions {
                signal: controller.signal(),
                force: None,
            },
        )
        .await
        .unwrap_or_else(|| panic!("expected suggestions for \"/{prefix}\""));
    result.items.into_iter().map(|item| item.value).collect()
}

#[tokio::test]
async fn ranks_skill_research_idea_first_for_query_idea() {
    let items = suggestions_for("idea").await;
    assert_eq!(items[0], "skill:research-idea");
    let deep_pos = items
        .iter()
        .position(|item| item == "skill:deep-research")
        .expect("skill:deep-research should be present");
    let idea_pos = items
        .iter()
        .position(|item| item == "skill:research-idea")
        .expect("skill:research-idea should be present");
    assert!(deep_pos > idea_pos);
}

#[tokio::test]
async fn keeps_ordinary_slash_commands_matching() {
    let items = suggestions_for("mod").await;
    assert!(items.iter().any(|item| item == "model"));
}

#[tokio::test]
async fn completes_commands_after_leading_whitespace_and_preserves_it() {
    let cwd = pi_js::env::cwd();
    let provider = CombinedAutocompleteProvider::new(
        Some(vec![CombinedCommand::SlashCommand(SlashCommand {
            name: "model".to_string(),
            description: None,
            argument_hint: None,
            get_argument_completions: None,
        })]),
        &cwd,
        None,
    );

    for (line, expected) in [(" /", " /model "), ("  /mod", "  /model "), ("\t/mod", "\t/model ")] {
        let controller = AbortController::new();
        let line_string = line.to_string();
        let col = pi_js::str16::len(line) as i64;
        let result = provider
            .get_suggestions(
                &[line_string.clone()],
                0,
                col,
                GetSuggestionsOptions {
                    signal: controller.signal(),
                    force: None,
                },
            )
            .await
            .expect("expected suggestions");
        assert_eq!(result.prefix, pi_js::str16::trim_start(line));
        let values: Vec<String> = result.items.iter().map(|item| item.value.clone()).collect();
        assert_eq!(values, vec!["model".to_string()]);
        let applied = provider.apply_completion(&[line_string], 0, col, &result.items[0], &result.prefix);
        assert_eq!(applied.lines[0], expected);
        assert_eq!(applied.cursor_col, pi_js::str16::len(expected) as i64);
    }
}

#[tokio::test]
async fn completes_command_arguments_after_leading_whitespace() {
    let cwd = pi_js::env::cwd();
    let provider = CombinedAutocompleteProvider::new(
        Some(vec![CombinedCommand::SlashCommand(SlashCommand {
            name: "model".to_string(),
            description: None,
            argument_hint: None,
            get_argument_completions: Some(Arc::new(|prefix| {
                let prefix = prefix.to_string();
                Box::pin(async move {
                    assert_eq!(prefix, "son");
                    Some(vec![AutocompleteItem {
                        value: "sonnet".to_string(),
                        label: "sonnet".to_string(),
                        description: None,
                    }])
                })
            })),
        })]),
        &cwd,
        None,
    );

    let line = "  /model son".to_string();
    let col = pi_js::str16::len(&line) as i64;
    let controller = AbortController::new();
    let result = provider
        .get_suggestions(
            &[line.clone()],
            0,
            col,
            GetSuggestionsOptions {
                signal: controller.signal(),
                force: None,
            },
        )
        .await
        .expect("expected argument suggestions");
    assert_eq!(result.prefix, "son");
    let applied = provider.apply_completion(&[line], 0, col, &result.items[0], &result.prefix);
    assert_eq!(applied.lines[0], "  /model sonnet");
}

#[tokio::test]
async fn keeps_explicit_skill_queries_working() {
    let items = suggestions_for("skill:side").await;
    assert!(items.iter().any(|item| item == "skill:to-sidecar"));
}

// Regression test for #9944.
#[tokio::test]
async fn lists_skills_while_typing_the_skill_prefix() {
    let items = suggestions_for("skill").await;
    let skill_items: Vec<String> = items.into_iter().filter(|item| item.starts_with("skill:")).collect();
    let expected: Vec<String> = make_commands()
        .into_iter()
        .filter_map(|cmd| match cmd {
            CombinedCommand::SlashCommand(sc) if sc.name.starts_with("skill:") => Some(sc.name),
            _ => None,
        })
        .collect();
    assert_eq!(skill_items, expected);
}

#[tokio::test]
async fn keeps_fuzzy_skill_prefix_shorthand_working() {
    let items = suggestions_for("skbra").await;
    assert!(items.iter().any(|item| item == "skill:brainstorm"));
}
