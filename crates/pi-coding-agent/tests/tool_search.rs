//! Port of packages/coding-agent/test/tool-search.test.ts (+ #11803 / 6d3e342b2 $ref coverage)

use std::sync::Arc;

use indexmap::{IndexMap, IndexSet};
use pi_agent_core::AgentTool;
use pi_coding_agent::core::extensions::types::{ToolDefinition, ToolExposure, ToolLoadoutBackend, ToolNamespace};
use pi_coding_agent::core::tools::tool_definition_wrapper::wrap_tool_definition;
use pi_coding_agent::extensions::codemode::tool::{
    CodemodeDescriptionOptions, CodemodeToolOptions, create_codemode_description, create_codemode_tool_definition,
};
use pi_coding_agent::extensions::tool_search::tool::{
    Bm25Ranker, ToolSearchDocumentInput, create_tool_search_document, tokenize,
};
use pi_js::vendor::typebox::Schema;
use serde_json::{Value, json};

fn make_tool(name: &str, description: &str, properties: Value) -> AgentTool {
    let def = ToolDefinition {
        name: name.to_string(),
        label: name.to_string(),
        description: description.to_string(),
        prompt_snippet: None,
        prompt_guidelines: None,
        parameters: Schema::from(json!({
            "type": "object",
            "properties": properties,
        })),
        constrained_sampling: None,
        render_shell: None,
        prepare_arguments: None,
        output_schema: None,
        exposure: None,
        namespace: None,
        annotations: None,
        default_active: None,
        prepare_loadout: None,
        execution_mode: None,
        execute: Arc::new(|_, _, _, _, _| Box::pin(async { Ok(pi_agent_core::AgentToolResult::text("")) })),
        render_call: None,
        render_result: None,
    };
    wrap_tool_definition(def, None)
}

fn make_output_tool(name: &str, output_schema: Value) -> AgentTool {
    let def = ToolDefinition {
        name: name.to_string(),
        label: name.to_string(),
        description: format!("{name} tool"),
        prompt_snippet: None,
        prompt_guidelines: None,
        parameters: Schema::from(json!({ "type": "object", "properties": {} })),
        constrained_sampling: None,
        render_shell: None,
        prepare_arguments: None,
        output_schema: Some(Schema::from(output_schema)),
        exposure: None,
        namespace: None,
        annotations: None,
        default_active: None,
        prepare_loadout: None,
        execution_mode: None,
        execute: Arc::new(|_, _, _, _, _| Box::pin(async { Ok(pi_agent_core::AgentToolResult::text("")) })),
        render_call: None,
        render_result: None,
    };
    wrap_tool_definition(def, None)
}

#[test]
fn tokenize_splits_camel_and_snake_case_drops_stop_words_and_folds_plurals() {
    assert_eq!(
        tokenize("listIssues for the GitHub_repo"),
        vec!["list", "issue", "git", "hub", "repo"]
    );
    assert_eq!(
        tokenize("searches queries HTTPServer"),
        vec!["search", "query", "http", "server"]
    );
}

#[test]
fn bm25_ranker_ranks_by_relevance_and_respects_limit() {
    let tools = vec![
        make_tool(
            "mcp__github__list_issues",
            "List issues in a repository.",
            json!({ "state": { "type": "string", "description": "open or closed" } }),
        ),
        make_tool("mcp__github__create_pull_request", "Open a pull request.", json!({})),
        make_tool("mcp__linear__search_issues", "Search Linear issues by text.", json!({})),
        make_tool("mcp__docs__search", "Search the documentation.", json!({})),
    ];
    let documents: Vec<_> = tools
        .iter()
        .map(|t| {
            create_tool_search_document(
                ToolSearchDocumentInput {
                    name: &t.name,
                    description: &t.description,
                    parameters: &t.parameters,
                },
                None,
            )
        })
        .collect();

    let ranker = Bm25Ranker::new(None);
    let issue_names: Vec<_> = ranker
        .rank("issue", &documents, 8)
        .into_iter()
        .map(|m| m.name)
        .collect();
    assert_eq!(
        issue_names,
        vec!["mcp__linear__search_issues", "mcp__github__list_issues"]
    );

    let pr_matches = ranker.rank("pull requests", &documents, 8);
    assert_eq!(pr_matches[0].name, "mcp__github__create_pull_request");
    assert_eq!(ranker.rank("search", &documents, 1).len(), 1);

    let closed_names: Vec<_> = ranker
        .rank("closed", &documents, 8)
        .into_iter()
        .map(|m| m.name)
        .collect();
    assert_eq!(closed_names, vec!["mcp__github__list_issues"]);
}

#[test]
fn bm25_ranker_returns_nothing_for_unknown_or_empty_queries() {
    let tools = vec![make_tool(
        "mcp__github__list_issues",
        "List issues in a repository.",
        json!({}),
    )];
    let documents: Vec<_> = tools
        .iter()
        .map(|t| {
            create_tool_search_document(
                ToolSearchDocumentInput {
                    name: &t.name,
                    description: &t.description,
                    parameters: &t.parameters,
                },
                None,
            )
        })
        .collect();

    let ranker = Bm25Ranker::new(None);
    assert!(ranker.rank("kubernetes", &documents, 8).is_empty());
    assert!(ranker.rank("the", &documents, 8).is_empty());
    assert!(ranker.rank("tickets", &documents, 8).is_empty());
}

#[test]
fn bm25_ranker_includes_namespace_in_search_text() {
    let entry = make_tool("mcp__x__run", "Run it.", json!({}));
    let ns = ToolNamespace {
        name: "mcp__x".to_string(),
        description: Some("Kubernetes cluster tools".to_string()),
        instructions: None,
    };
    let doc = create_tool_search_document(
        ToolSearchDocumentInput {
            name: &entry.name,
            description: &entry.description,
            parameters: &entry.parameters,
        },
        Some(&ns),
    );
    let matches = Bm25Ranker::new(None).rank("kubernetes", &[doc], 8);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].name, "mcp__x__run");
    assert!(matches[0].score > 0.0);
}

#[test]
fn schema_references_in_search_text() {
    let search_text = |parameters: Value| -> String {
        let schema = Schema::from(parameters);
        create_tool_search_document(
            ToolSearchDocumentInput {
                name: "mcp__ledger__inspect",
                description: "Inspect records",
                parameters: &schema,
            },
            None,
        )
        .text
    };

    assert_eq!(
        search_text(json!({
            "type": "object",
            "properties": { "options": { "$ref": "#/$defs/options" } },
            "$defs": {
                "options": {
                    "type": "object",
                    "properties": { "mode": { "type": "string", "description": "reconciliation" } }
                }
            }
        })),
        "mcp__ledger__inspect mcp  ledger  inspect Inspect records options mode reconciliation"
    );

    assert_eq!(
        search_text(json!({
            "type": "object",
            "properties": { "range": { "$ref": "#/$defs/date~1range" } },
            "$defs": { "date/range": { "description": "fiscal period" } }
        })),
        "mcp__ledger__inspect mcp  ledger  inspect Inspect records range fiscal period"
    );

    assert_eq!(
        search_text(json!({
            "type": "object",
            "properties": {
                "root": { "$ref": "#/$defs/node" },
                "other": { "$ref": "#/$defs/node" },
                "self": { "$ref": "#" }
            },
            "$defs": {
                "node": {
                    "description": "tree node",
                    "properties": { "children": { "type": "array", "items": { "$ref": "#/$defs/node" } } }
                }
            }
        })),
        "mcp__ledger__inspect mcp  ledger  inspect Inspect records root tree node children other self"
    );

    assert_eq!(
        search_text(json!({
            "type": "object",
            "properties": {
                "remote": { "$ref": "https://example.com/schema.json#/$defs/x" },
                "missing": { "$ref": "#/$defs/missing" },
                "malformed": { "$ref": "#/$defs/%E0%A4%A" }
            },
            "$defs": { "unused": { "description": "reconciliation" } }
        })),
        "mcp__ledger__inspect mcp  ledger  inspect Inspect records remote missing malformed"
    );
}

#[test]
fn codemode_description_catalog() {
    let plain = make_tool("read_notes", "Read notes.", json!({}));
    let github: Vec<AgentTool> = ["a", "b", "c"]
        .into_iter()
        .map(|s| make_tool(&format!("mcp__github__{s}"), &format!("GitHub {s}."), json!({})))
        .collect();
    let docs = vec![
        make_tool("mcp__docs__search", "Search docs.", json!({})),
        make_tool("mcp__docs__long", &"Long ".repeat(200), json!({})),
    ];

    let mut all = vec![plain.clone()];
    all.extend(github.clone());
    all.extend(docs.clone());

    let mut namespaces = IndexMap::new();
    for entry in &github {
        namespaces.insert(
            entry.name.clone(),
            ToolNamespace {
                name: "mcp__github".to_string(),
                description: Some("GitHub server".to_string()),
                instructions: None,
            },
        );
    }
    for entry in &docs {
        namespaces.insert(
            entry.name.clone(),
            ToolNamespace {
                name: "mcp__docs".to_string(),
                description: None,
                instructions: None,
            },
        );
    }

    // Lists everything without a budget.
    let description = create_codemode_description(
        &all,
        CodemodeDescriptionOptions {
            namespaces: namespaces.clone(),
            ..Default::default()
        },
    );
    assert!(description.contains("Nested tools:"));
    assert!(description.contains("## mcp__github\nGitHub server"));
    assert!(description.contains("## mcp__docs\n\n### `mcp__docs"));
    assert!(description.contains("find unlisted tools, such as MCP tools"));

    // Fills the budget round-robin, cheapest first, and says what is missing.
    let budgeted = create_codemode_description(
        &all,
        CodemodeDescriptionOptions {
            namespaces: namespaces.clone(),
            inline_budget: Some(170),
            ..Default::default()
        },
    );
    assert!(budgeted.contains("### `read_notes`"));
    assert!(budgeted.contains("## mcp__docs (some tools not listed)"));
    assert!(budgeted.contains("### `mcp__docs__search`"));
    assert!(!budgeted.contains("### `mcp__docs__long`"));
    assert!(budgeted.contains("## mcp__github (some tools not listed)"));
    assert!(budgeted.contains("find unlisted tools, such as MCP tools"));
    assert_eq!(
        create_codemode_description(
            &all,
            CodemodeDescriptionOptions {
                namespaces: namespaces.clone(),
                inline_budget: Some(170),
                ..Default::default()
            },
        ),
        budgeted
    );

    // Leaves deferred tools and their namespaces out entirely.
    let deferred: IndexSet<String> = github.iter().map(|e| e.name.clone()).collect();
    let without_deferred = create_codemode_description(
        &all,
        CodemodeDescriptionOptions {
            namespaces: namespaces.clone(),
            deferred,
            ..Default::default()
        },
    );
    assert!(!without_deferred.contains("mcp__github"));
    let mut only_plain_docs = vec![plain];
    only_plain_docs.extend(docs);
    assert_eq!(
        without_deferred,
        create_codemode_description(
            &only_plain_docs,
            CodemodeDescriptionOptions {
                namespaces: namespaces.clone(),
                ..Default::default()
            },
        )
    );

    // Leaves namespace instructions out.
    let instr_ns: IndexMap<String, ToolNamespace> = github
        .iter()
        .map(|entry| {
            (
                entry.name.clone(),
                ToolNamespace {
                    name: "mcp__github".to_string(),
                    description: None,
                    instructions: Some("Long usage guide.".to_string()),
                },
            )
        })
        .collect();
    let with_instr = create_codemode_description(
        &github,
        CodemodeDescriptionOptions {
            namespaces: instr_ns,
            ..Default::default()
        },
    );
    assert!(with_instr.contains("## mcp__github\n\n### `mcp__github__a`"));
    assert!(!with_instr.contains("Long usage guide."));

    // Lists only namespaces with a zero budget.
    let zero_budget = create_codemode_description(
        &all,
        CodemodeDescriptionOptions {
            namespaces,
            inline_budget: Some(0),
            ..Default::default()
        },
    );
    assert!(zero_budget.contains("## mcp__docs (tools not listed)"));
    assert!(!zero_budget.contains("codemode tool declaration:"));
}

struct TestLoadout {
    tools: Vec<AgentTool>,
}

impl ToolLoadoutBackend for TestLoadout {
    fn declared(&self) -> &[AgentTool] {
        &self.tools
    }
    fn callable(&self) -> &[AgentTool] {
        &self.tools
    }
    fn registered(&self) -> &[AgentTool] {
        &self.tools
    }
    fn get_exposure(&self, _name: &str) -> ToolExposure {
        ToolExposure::Direct
    }
    fn get_namespace(&self, _name: &str) -> Option<ToolNamespace> {
        None
    }
}

#[test]
fn codemode_output_summaries_follow_local_refs() {
    let report = make_output_tool(
        "report",
        json!({
            "$ref": "#/$defs/Report",
            "$defs": {
                "Report": {
                    "type": "object",
                    "properties": {
                        "kind": { "$ref": "#/$defs/Kind" },
                        "summary": { "$ref": "#/$defs/Summary" }
                    },
                    "required": ["kind", "summary"]
                },
                "Kind": { "const": "ledger" },
                "Summary": {
                    "type": "object",
                    "properties": { "total": { "type": "number" } },
                    "required": ["total"]
                }
            }
        }),
    );
    let result = make_output_tool(
        "result",
        json!({
            "anyOf": [
                { "$ref": "#/$defs/Ok" },
                { "$ref": "#/$defs/Failed" }
            ],
            "$defs": {
                "Ok": { "type": "object", "properties": { "value": { "type": "string" } }, "required": ["value"] },
                "Failed": { "type": "object", "properties": { "error": { "type": "string" } }, "required": ["error"] }
            }
        }),
    );

    let def = create_codemode_tool_definition(CodemodeToolOptions {
        models: false,
        ..Default::default()
    });
    let loadout = TestLoadout {
        tools: vec![report, result],
    };
    let changes = def.prepare_loadout.expect("prepare_loadout")(&loadout)
        .expect("prepare ok")
        .expect("loadout changes");
    let descriptions = changes.descriptions.expect("descriptions");

    assert_eq!(
        descriptions.get("report").map(String::as_str),
        Some("report tool\n\nCodemode: `tools.report(args)` resolves to `{ kind: \"ledger\", summary: { total } }`.")
    );
    assert_eq!(
        descriptions.get("result").map(String::as_str),
        Some("result tool\n\nCodemode: `tools.result(args)` resolves to `{ value } | { error }`.")
    );
}
