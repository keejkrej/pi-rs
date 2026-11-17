//! Port of packages/coding-agent/test/collapsible-message-components.test.ts

use pi_coding_agent::modes::interactive::components::branch_summary_message::{
    BranchSummaryMessage, BranchSummaryMessageComponent,
};
use pi_coding_agent::modes::interactive::components::compaction_summary_message::{
    CompactionSummaryMessage, CompactionSummaryMessageComponent,
};
use pi_coding_agent::modes::interactive::components::skill_invocation_message::{
    ParsedSkillBlock, SkillInvocationMessageComponent,
};
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_tui::{Component, TuiMouseButton, TuiMouseEvent, TuiMouseEventType, strip_terminal_sequences};
use serial_test::serial;

const WIDTH: usize = 80;

fn render_text(component: &dyn Component) -> String {
    strip_terminal_sequences(&component.render(WIDTH).join("\n"))
}

fn click_row(component: &dyn Component, marker: &str) {
    let lines = component.render(WIDTH);
    let row = lines
        .iter()
        .position(|line| strip_terminal_sequences(line).contains(marker))
        .expect("marker row exists");
    let event = TuiMouseEvent {
        r#type: TuiMouseEventType::Click,
        button: TuiMouseButton::Left,
        x: 2,
        y: row as i64,
        screen_x: 2,
        screen_y: row as i64,
        width: WIDTH as i64,
        height: lines.len() as i64,
        shift: false,
        alt: false,
        ctrl: false,
        click_count: Some(1),
        wheel_delta: None,
    };
    let result = component.handle_mouse(&event);
    assert_eq!(result.and_then(|r| r.handled), Some(true));
}

#[test]
#[serial]
fn toggles_a_compaction_summary_when_clicked() {
    init_theme(Some("dark"), None);
    let component = CompactionSummaryMessageComponent::new(
        CompactionSummaryMessage {
            role: "compactionSummary".to_string(),
            summary: "compaction details".to_string(),
            tokens_before: 1234,
            timestamp: pi_js::time::now_ms(),
        },
        None,
    );

    assert!(!render_text(&component).contains("compaction details"));
    click_row(&component, "[compaction]");
    assert!(render_text(&component).contains("compaction details"));
    click_row(&component, "[compaction]");
    assert!(!render_text(&component).contains("compaction details"));
}

#[test]
#[serial]
fn toggles_a_branch_summary_when_clicked() {
    init_theme(Some("dark"), None);
    let component = BranchSummaryMessageComponent::new(
        BranchSummaryMessage {
            role: "branchSummary".to_string(),
            summary: "branch details".to_string(),
            from_id: Some("entry-1".to_string()),
            timestamp: pi_js::time::now_ms(),
        },
        None,
    );

    assert!(!render_text(&component).contains("branch details"));
    click_row(&component, "[branch]");
    assert!(render_text(&component).contains("branch details"));
    click_row(&component, "[branch]");
    assert!(!render_text(&component).contains("branch details"));
}

#[test]
#[serial]
fn toggles_a_skill_invocation_when_clicked() {
    init_theme(Some("dark"), None);
    let component = SkillInvocationMessageComponent::new(
        ParsedSkillBlock {
            name: "example-skill".to_string(),
            location: "/tmp/example-skill.md".to_string(),
            content: "skill details".to_string(),
            user_message: None,
        },
        None,
    );

    assert!(!render_text(&component).contains("skill details"));
    click_row(&component, "[skill]");
    assert!(render_text(&component).contains("skill details"));
    click_row(&component, "[skill]");
    assert!(!render_text(&component).contains("skill details"));
}
