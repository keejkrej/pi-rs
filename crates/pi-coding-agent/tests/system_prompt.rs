//! Port of `packages/coding-agent/test/system-prompt.test.ts`.

use indexmap::IndexMap;
use pi_coding_agent::core::skills::Skill;
use pi_coding_agent::core::source_info::{SyntheticSourceOptions, create_synthetic_source_info};
use pi_coding_agent::core::system_prompt::{BuildSystemPromptOptions, ContextFile, build_system_prompt};

fn test_skill() -> Skill {
    Skill {
        name: "test-skill".to_owned(),
        description: "A test skill.".to_owned(),
        file_path: "/skills/test-skill/SKILL.md".to_owned(),
        base_dir: "/skills/test-skill".to_owned(),
        source_info: create_synthetic_source_info(
            "/skills/test-skill/SKILL.md",
            SyntheticSourceOptions {
                source: "test".to_owned(),
                scope: None,
                origin: None,
                base_dir: None,
                changelog_path: None,
            },
        ),
        disable_model_invocation: false,
    }
}

#[test]
fn empty_tools() {
    let prompt = build_system_prompt(&BuildSystemPromptOptions {
        selected_tools: Some(Vec::new()),
        context_files: Some(Vec::new()),
        skills: Some(Vec::new()),
        cwd: pi_js::env::cwd(),
        ..Default::default()
    })
    .unwrap();

    assert!(prompt.contains("<tools>\n(none)\n"));
    assert!(prompt.contains("Show file paths clearly"));
}

#[test]
fn prompt_structure_and_sections() {
    let default_prompt = build_system_prompt(&BuildSystemPromptOptions {
        cwd: "/tmp".to_owned(),
        selected_tools: Some(Vec::new()),
        context_files: Some(Vec::new()),
        skills: Some(Vec::new()),
        ..Default::default()
    })
    .unwrap();
    let custom_prompt = build_system_prompt(&BuildSystemPromptOptions {
        custom_prompt: Some("You are Exact.".to_owned()),
        cwd: "/tmp".to_owned(),
        selected_tools: Some(Vec::new()),
        context_files: Some(Vec::new()),
        skills: Some(Vec::new()),
        ..Default::default()
    })
    .unwrap();

    assert!(default_prompt.starts_with("You are an expert coding assistant operating inside pi"));
    assert!(custom_prompt.starts_with("You are Exact.\n\n<cwd>"));

    let forced = build_system_prompt(&BuildSystemPromptOptions {
        force_system_prompt: Some("exact".to_owned()),
        cwd: "/tmp".to_owned(),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(forced, "exact");

    let structured = build_system_prompt(&BuildSystemPromptOptions {
        custom_prompt: Some("You are Exact.".to_owned()),
        append_system_prompt: Some("Additional instructions.".to_owned()),
        context_files: Some(vec![ContextFile {
            path: "/tmp/AGENTS.md".to_owned(),
            content: "Project instructions.".to_owned(),
        }]),
        selected_tools: Some(Vec::new()),
        skills: Some(Vec::new()),
        cwd: "/tmp".to_owned(),
        ..Default::default()
    })
    .unwrap();

    assert!(structured.contains("<addendum>\nAdditional instructions.\n</addendum>"));
    assert!(
        structured.contains("<project_context>\nProject-specific instructions and guidelines:\n\n<project_instructions path=\"/tmp/AGENTS.md\">")
    );
    assert!(structured.contains("<cwd>\n/tmp\n</cwd>"));
}

#[test]
fn default_tools_and_docs() {
    let mut snippets = IndexMap::new();
    snippets.insert("read".to_owned(), "Read file contents".to_owned());
    snippets.insert("bash".to_owned(), "Execute bash commands".to_owned());
    snippets.insert("edit".to_owned(), "Make surgical edits".to_owned());
    snippets.insert("write".to_owned(), "Create or overwrite files".to_owned());

    let prompt = build_system_prompt(&BuildSystemPromptOptions {
        tool_snippets: Some(snippets),
        context_files: Some(Vec::new()),
        skills: Some(Vec::new()),
        cwd: pi_js::env::cwd(),
        ..Default::default()
    })
    .unwrap();

    assert!(prompt.contains("- read:"));
    assert!(prompt.contains("- bash:"));
    assert!(prompt.contains("- edit:"));
    assert!(prompt.contains("- write:"));
    assert!(prompt.contains(
        "- When reading pi docs or examples, resolve docs/... under Additional docs and examples/... under Examples, not the current working directory"
    ));
    assert!(prompt.contains("environment variables (docs/environment-variables.md), MCP servers (docs/mcp.md)"));

    let ps_only = build_system_prompt(&BuildSystemPromptOptions {
        selected_tools: Some(vec!["powershell".to_owned()]),
        cwd: pi_js::env::cwd(),
        ..Default::default()
    })
    .unwrap();
    assert!(ps_only.contains("Use PowerShell for file operations"));

    let bash_ps = build_system_prompt(&BuildSystemPromptOptions {
        selected_tools: Some(vec!["bash".to_owned(), "powershell".to_owned()]),
        cwd: pi_js::env::cwd(),
        ..Default::default()
    })
    .unwrap();
    assert!(bash_ps.contains("Use bash or PowerShell for file operations"));
}

#[test]
fn custom_tool_snippets_and_guidelines() {
    let mut snippets = IndexMap::new();
    snippets.insert("dynamic_tool".to_owned(), "Run dynamic test behavior".to_owned());
    let prompt_with = build_system_prompt(&BuildSystemPromptOptions {
        selected_tools: Some(vec!["read".to_owned(), "dynamic_tool".to_owned()]),
        tool_snippets: Some(snippets),
        cwd: pi_js::env::cwd(),
        ..Default::default()
    })
    .unwrap();
    assert!(prompt_with.contains("- dynamic_tool: Run dynamic test behavior"));

    let prompt_without = build_system_prompt(&BuildSystemPromptOptions {
        selected_tools: Some(vec!["read".to_owned(), "dynamic_tool".to_owned()]),
        cwd: pi_js::env::cwd(),
        ..Default::default()
    })
    .unwrap();
    assert!(!prompt_without.contains("dynamic_tool"));

    let prompt_guidelines = build_system_prompt(&BuildSystemPromptOptions {
        selected_tools: Some(vec!["read".to_owned(), "dynamic_tool".to_owned()]),
        prompt_guidelines: Some(vec![
            "Use dynamic_tool for summaries.".to_owned(),
            "  Use dynamic_tool for summaries.  ".to_owned(),
            "   ".to_owned(),
        ]),
        cwd: pi_js::env::cwd(),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        prompt_guidelines.matches("- Use dynamic_tool for summaries.").count(),
        1
    );
}

#[test]
fn skills_and_hidden_tools() {
    for custom_prompt in [None, Some("Custom system prompt".to_owned())] {
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            custom_prompt,
            selected_tools: Some(vec!["bash".to_owned()]),
            skills: Some(vec![test_skill()]),
            cwd: pi_js::env::cwd(),
            ..Default::default()
        })
        .unwrap();
        assert!(prompt.contains("<skills>"));
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("<name>test-skill</name>"));
        assert!(prompt.contains("Use bash to load a skill's file"));
        assert!(prompt.contains(
            "Resolve relative paths against the skill's directory (parent of SKILL.md). Use absolute paths in tool calls."
        ));
    }

    let no_reader = build_system_prompt(&BuildSystemPromptOptions {
        selected_tools: Some(vec!["write".to_owned()]),
        skills: Some(vec![test_skill()]),
        cwd: pi_js::env::cwd(),
        ..Default::default()
    })
    .unwrap();
    assert!(!no_reader.contains("<available_skills>"));

    // #10343 hidden tools
    let build_hidden = |hidden: &[&str]| {
        let mut snippets = IndexMap::new();
        snippets.insert("read".to_owned(), "Read files".to_owned());
        snippets.insert("bash".to_owned(), "Run commands".to_owned());
        snippets.insert("run".to_owned(), "Run a task".to_owned());
        let mut guidelines = IndexMap::new();
        guidelines.insert("read".to_owned(), vec!["Use read for files.".to_owned()]);
        guidelines.insert("run".to_owned(), vec!["Prefer run.".to_owned()]);
        build_system_prompt(&BuildSystemPromptOptions {
            selected_tools: Some(vec!["read".to_owned(), "bash".to_owned(), "run".to_owned()]),
            hidden_tools: Some(hidden.iter().map(|s| (*s).to_owned()).collect()),
            tool_snippets: Some(snippets),
            tool_guidelines: Some(guidelines),
            skills: Some(vec![test_skill()]),
            cwd: pi_js::env::cwd(),
            ..Default::default()
        })
        .unwrap()
    };

    let both_hidden = build_hidden(&["read", "bash"]);
    assert!(both_hidden.contains("<tools>\n- run: Run a task\n"));
    assert!(!both_hidden.contains("- read: "));
    assert!(!both_hidden.contains("Use read for files."));
    assert!(!both_hidden.contains("Use bash for file operations"));
    assert!(both_hidden.contains("- Prefer run."));
    assert!(both_hidden.contains("\nLoad a skill's file when the task matches its description."));
    assert!(build_hidden(&["read"]).contains("Use bash to load a skill's file"));
    assert!(build_hidden(&[]).contains("Use the read tool to load a skill's file"));
}
