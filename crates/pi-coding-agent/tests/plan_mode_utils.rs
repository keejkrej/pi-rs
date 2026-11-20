//! Port of packages/coding-agent/test/plan-mode-utils.test.ts

#[allow(dead_code)]
#[path = "../examples/extensions__plan_mode/utils.rs"]
mod utils;

use pretty_assertions::assert_eq;
use utils::{
    TodoItem, all_steps_complete, clean_step_text, extract_done_steps, extract_todo_items, is_safe_command,
    mark_completed_steps,
};

#[test]
fn is_safe_command_allows_safe_and_blocks_destructive_commands() {
    for cmd in [
        "ls -la",
        "cat file.txt",
        "head -n 10 file.txt",
        "tail -f log.txt",
        "grep pattern file",
        "find . -name '*.ts'",
        "git status",
        "git log --oneline",
        "git diff",
        "git branch",
        "npm list",
        "npm outdated",
        "yarn info react",
        "pwd",
        "echo hello",
        "wc -l file.txt",
        "du -sh .",
        "df -h",
        "  ls -la",
    ] {
        assert!(is_safe_command(cmd), "expected safe: {cmd}");
    }

    for cmd in [
        "rm file.txt",
        "rm -rf dir",
        "mv old new",
        "cp src dst",
        "mkdir newdir",
        "touch newfile",
        "git add .",
        "git commit -m 'msg'",
        "git push",
        "git checkout main",
        "git reset --hard",
        "npm install lodash",
        "yarn add react",
        "pip install requests",
        "brew install node",
        "echo hello > file.txt",
        "cat foo >> bar",
        ">file.txt",
        "sudo rm -rf /",
        "kill -9 1234",
        "reboot",
        "vim file.txt",
        "nano file.txt",
        "code .",
        "unknown-command",
        "my-script.sh",
        "  rm file",
    ] {
        assert!(!is_safe_command(cmd), "expected unsafe: {cmd}");
    }
}

#[test]
fn clean_step_text_strips_markdown_action_verbs_and_preserves_length() {
    assert_eq!(clean_step_text("**bold text**"), "Bold text");
    assert_eq!(clean_step_text("*italic text*"), "Italic text");
    assert_eq!(clean_step_text("run `npm install`"), "Npm install");
    assert_eq!(clean_step_text("check the `config.json` file"), "Config.json file");
    assert_eq!(clean_step_text("Create the new file"), "New file");
    assert_eq!(clean_step_text("Run the tests"), "Tests");
    assert_eq!(clean_step_text("Check the status"), "Status");
    assert_eq!(clean_step_text("update config"), "Config");
    let long_text = "This is a very long step description that exceeds the maximum allowed length for display";
    assert_eq!(clean_step_text(long_text), long_text);
    assert_eq!(clean_step_text("multiple   spaces   here"), "Multiple spaces here");
}

#[test]
fn extract_todo_items_handles_last_plan_section_and_markdown() {
    let msg = "Here's what we'll do:\n\nPlan:\n1. First step here\n2. Second step here\n3. Third step here";
    let items = extract_todo_items(msg);
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].step, 1);
    assert_eq!(items[0].text, "First step here");
    assert!(!items[0].completed);

    assert_eq!(extract_todo_items("**Plan:**\n1. Do something").len(), 1);
    assert_eq!(extract_todo_items("Plan:\n1) First item\n2) Second item").len(), 2);
    assert_eq!(
        extract_todo_items("Here are some steps:\n1. First step\n2. Second step").len(),
        0
    );

    let filtered = extract_todo_items("Plan:\n1. OK\n2. This is a proper step");
    assert_eq!(filtered.len(), 1);
    assert!(filtered[0].text.contains("proper"));

    let code_steps = extract_todo_items("Plan:\n1. `npm install`\n2. Run the build process");
    assert_eq!(code_steps.len(), 2);
    assert_eq!(code_steps[0].text, "Npm install");

    let multi_section = "Some reasoning...\nPlan:\n1. Draft one\n2. Draft two\nNow the answer.\nPlan:\n1. Print 1\n2. Print 2\n3. Print 3";
    let last_only: Vec<String> = extract_todo_items(multi_section).into_iter().map(|i| i.text).collect();
    assert_eq!(last_only, vec!["Print 1", "Print 2", "Print 3"]);

    let md = extract_todo_items("Plan:\n1. **Explore** the repo structure\n2. `index.ts` — fix getTextContent");
    assert_eq!(md.len(), 2);
    assert_eq!(md[0].text, "Explore the repo structure");
    assert_eq!(md[1].text, "Index.ts — fix getTextContent");
}

#[test]
fn extract_done_steps_and_mark_completed_steps_work() {
    assert_eq!(extract_done_steps("I've completed the first step [DONE:1]"), vec![1]);
    assert_eq!(
        extract_done_steps("Did steps [DONE:1] and [DONE:2] and [DONE:3]"),
        vec![1, 2, 3]
    );
    assert_eq!(extract_done_steps("[done:1] [DONE:2] [Done:3]"), vec![1, 2, 3]);
    assert_eq!(
        extract_done_steps("[DONE:1] [DONE: 2] [DONE3] [Done:4] Done: 5"),
        vec![1, 2, 3, 4, 5]
    );
    assert_eq!(
        extract_done_steps("Step 6 done, Step 7 is complete, Step 8 finished"),
        vec![6, 7, 8]
    );
    assert!(extract_done_steps("Step 9 is not done, Step 10 incomplete").is_empty());
    assert_eq!(extract_done_steps("[DONE:1] [DONE: 1]"), vec![1]);
    assert_eq!(extract_done_steps("[DONE:abc] [DONE:] [DONE:1]"), vec![1]);

    let mut items = vec![
        TodoItem {
            step: 1,
            text: "First".to_owned(),
            completed: false,
        },
        TodoItem {
            step: 2,
            text: "Second".to_owned(),
            completed: false,
        },
        TodoItem {
            step: 3,
            text: "Third".to_owned(),
            completed: false,
        },
    ];
    assert_eq!(mark_completed_steps("[DONE:1] [DONE:3]", &mut items), 2);
    assert!(items[0].completed);
    assert!(!items[1].completed);
    assert!(items[2].completed);

    let mut single = vec![TodoItem {
        step: 1,
        text: "First".to_owned(),
        completed: false,
    }];
    assert_eq!(mark_completed_steps("[DONE:1]", &mut single), 1);
    assert_eq!(mark_completed_steps("no markers", &mut single), 0);

    let mut ignore = vec![TodoItem {
        step: 1,
        text: "First".to_owned(),
        completed: false,
    }];
    assert_eq!(mark_completed_steps("[DONE:99]", &mut ignore), 0);
    assert!(!ignore[0].completed);

    assert!(all_steps_complete("All steps complete, wrapping up"));
    assert!(all_steps_complete("Every step is finished"));
    assert!(all_steps_complete("The plan is done"));
    assert!(!all_steps_complete("Step 1 complete, moving on"));
}
