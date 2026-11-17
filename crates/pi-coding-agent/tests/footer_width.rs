//! Port of packages/coding-agent/test/footer-width.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::{Model, Usage, UsageCost};
use pi_coding_agent::modes::interactive::components::footer::{
    FooterComponent, FooterContextUsage, FooterRoutedModel, FooterSession, FooterUsageRecord,
    ReadonlyFooterDataProvider, format_cwd_for_footer,
};
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_tui::{strip_terminal_sequences, visible_width};
use serde_json::json;
use serial_test::serial;

fn make_usage(input: i64, output: i64, total_cost: f64) -> Usage {
    Usage {
        input,
        output,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: input + output,
        cost: UsageCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: total_cost,
        },
    }
}

fn make_model(id: &str, provider: &str, reasoning: bool) -> Model {
    serde_json::from_value(json!({
        "id": id,
        "name": id,
        "api": "openai-responses",
        "provider": provider,
        "baseUrl": "https://api.example.com/v1",
        "reasoning": reasoning,
        "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 200_000,
        "maxTokens": 16_384,
    }))
    .expect("test model parses")
}

struct FakeSession {
    session_name: String,
    model: Model,
    thinking_level: String,
    using_subscription: bool,
    routed_model: Option<FooterRoutedModel>,
    records: Mutex<Vec<FooterUsageRecord>>,
}

impl FakeSession {
    fn new(
        session_name: &str,
        model_id: &str,
        provider: &str,
        reasoning: bool,
        thinking_level: &str,
        using_subscription: bool,
        routed_model: Option<FooterRoutedModel>,
        records: Vec<FooterUsageRecord>,
    ) -> Arc<Self> {
        Arc::new(Self {
            session_name: session_name.to_string(),
            model: make_model(model_id, provider, reasoning),
            thinking_level: thinking_level.to_string(),
            using_subscription,
            routed_model,
            records: Mutex::new(records),
        })
    }

    fn push_record(&self, record: FooterUsageRecord) {
        self.records.lock().unwrap().push(record);
    }
}

impl FooterSession for FakeSession {
    fn session_ptr_key(&self) -> usize {
        self as *const Self as usize
    }
    fn session_id(&self) -> String {
        "test-session".to_string()
    }
    fn leaf_id(&self) -> Option<String> {
        None
    }
    fn entry_count(&self) -> usize {
        self.records.lock().unwrap().len()
    }
    fn cwd(&self) -> String {
        "/tmp/project".to_string()
    }
    fn session_name(&self) -> Option<String> {
        (!self.session_name.is_empty()).then(|| self.session_name.clone())
    }
    fn model(&self) -> Option<Model> {
        Some(self.model.clone())
    }
    fn routed_model(&self) -> Option<FooterRoutedModel> {
        self.routed_model.clone()
    }
    fn thinking_level(&self) -> String {
        self.thinking_level.clone()
    }
    fn get_context_usage(&self) -> Option<FooterContextUsage> {
        Some(FooterContextUsage {
            context_window: 200_000,
            percent: Some(12),
            percent_f64: Some(12.3),
        })
    }
    fn is_using_subscription(&self, _provider: &str) -> bool {
        self.using_subscription
    }
    fn usage_records(&self) -> Vec<FooterUsageRecord> {
        self.records.lock().unwrap().clone()
    }
}

struct FakeFooterData {
    provider_count: usize,
}

impl ReadonlyFooterDataProvider for FakeFooterData {
    fn get_git_branch(&self) -> Option<String> {
        Some("main".to_string())
    }
    fn get_extension_statuses(&self) -> IndexMap<String, String> {
        IndexMap::new()
    }
    fn get_available_provider_count(&self) -> usize {
        self.provider_count
    }
}

#[test]
fn does_not_abbreviate_sibling_paths_that_share_the_home_prefix() {
    assert_eq!(format_cwd_for_footer("/home/user2", Some("/home/user")), "/home/user2");
}

#[test]
fn abbreviates_the_home_directory_and_descendants() {
    assert_eq!(format_cwd_for_footer("/home/user", Some("/home/user")), "~");
    assert_eq!(
        format_cwd_for_footer("/home/user/project", Some("/home/user")),
        "~/project"
    );
}

#[test]
#[serial]
fn keeps_all_lines_within_width_for_wide_session_names() {
    init_theme(None, Some(false));
    let width = 93usize;
    let session = FakeSession::new(
        &"한글".repeat(30),
        "test-model",
        "test",
        false,
        "off",
        false,
        None,
        Vec::new(),
    );
    let footer = FooterComponent::new(session, Arc::new(FakeFooterData { provider_count: 1 }));

    for line in footer.render(width) {
        assert!(visible_width(&line) <= width as i64, "width exceeded for line: {line}");
    }
}

#[test]
#[serial]
fn keeps_stats_line_within_width_for_wide_model_and_provider_names() {
    init_theme(None, Some(false));
    let width = 60usize;
    let session = FakeSession::new(
        "",
        &"模".repeat(30),
        "공급자",
        true,
        "high",
        false,
        None,
        vec![FooterUsageRecord::AssistantUsage(make_usage(12_345, 6_789, 1.234))],
    );
    let footer = FooterComponent::new(session, Arc::new(FakeFooterData { provider_count: 2 }));

    for line in footer.render(width) {
        assert!(visible_width(&line) <= width as i64, "width exceeded for line: {line}");
    }
}

#[test]
#[serial]
fn shows_the_physical_model_a_virtual_model_routed_to() {
    init_theme(None, Some(false));
    let session = FakeSession::new(
        "",
        "auto",
        "test",
        true,
        "high",
        false,
        Some(FooterRoutedModel {
            model_id: "gpt-5.6-luna".to_string(),
            context_window: 200_000,
            thinking_level: Some("medium".to_string()),
        }),
        Vec::new(),
    );
    let footer = FooterComponent::new(session, Arc::new(FakeFooterData { provider_count: 1 }));
    let stats_line = strip_terminal_sequences(&footer.render(120)[1]);
    assert!(
        stats_line.contains("auto \u{2022} high \u{2192} gpt-5.6-luna \u{2022} medium"),
        "stats_line={stats_line}"
    );
}

#[test]
#[serial]
fn includes_summary_and_tool_result_usage_in_the_total_cost() {
    init_theme(None, Some(false));
    let session = FakeSession::new(
        "",
        "test-model",
        "test",
        false,
        "off",
        false,
        None,
        vec![
            FooterUsageRecord::AssistantUsage(make_usage(100, 10, 0.5)),
            FooterUsageRecord::SummaryUsage(make_usage(20, 5, 0.25)),
            FooterUsageRecord::SummaryUsage(make_usage(5, 2, 0.125)),
            FooterUsageRecord::ToolResultUsage(make_usage(15, 3, 0.375)),
        ],
    );
    let footer = FooterComponent::new(session, Arc::new(FakeFooterData { provider_count: 1 }));
    let stats_line = strip_terminal_sequences(&footer.render(120)[1]);
    assert!(stats_line.contains("$1.250"), "stats_line={stats_line}");
}

#[test]
#[serial]
fn updates_cached_usage_totals_after_an_entry_is_appended() {
    init_theme(None, Some(false));
    let usage = make_usage(10, 1, 0.5);
    let session = FakeSession::new(
        "",
        "test-model",
        "test",
        false,
        "off",
        false,
        None,
        vec![FooterUsageRecord::AssistantUsage(usage.clone())],
    );
    let footer = FooterComponent::new(session.clone(), Arc::new(FakeFooterData { provider_count: 1 }));
    assert!(strip_terminal_sequences(&footer.render(120)[1]).contains("$0.500"));

    session.push_record(FooterUsageRecord::AssistantUsage(usage));
    assert!(strip_terminal_sequences(&footer.render(120)[1]).contains("$1.000"));
}

#[test]
#[serial]
fn marks_kimi_coding_as_subscription_even_with_api_key_auth() {
    init_theme(None, Some(false));
    let session = FakeSession::new(
        "",
        "test-model",
        "kimi-coding",
        false,
        "off",
        false,
        None,
        vec![FooterUsageRecord::AssistantUsage(make_usage(100, 10, 1.234))],
    );
    let footer = FooterComponent::new(session, Arc::new(FakeFooterData { provider_count: 1 }));
    let stats = strip_terminal_sequences(&footer.render(120)[1]);
    assert!(stats.contains("$1.234 (sub)"), "stats={stats}");
}

#[test]
#[serial]
fn marks_explicitly_identified_subscription_auth() {
    init_theme(None, Some(false));
    let session = FakeSession::new("", "test-model", "anthropic", false, "off", true, None, Vec::new());
    let footer = FooterComponent::new(session, Arc::new(FakeFooterData { provider_count: 1 }));
    let stats = strip_terminal_sequences(&footer.render(120)[1]);
    assert!(stats.contains("$0.000 (sub)"), "stats={stats}");
}

#[test]
#[serial]
fn does_not_mark_generic_oauth_sign_in_as_a_subscription() {
    init_theme(None, Some(false));
    let session = FakeSession::new(
        "",
        "test-model",
        "openrouter",
        false,
        "off",
        false,
        None,
        vec![FooterUsageRecord::AssistantUsage(make_usage(100, 10, 1.234))],
    );
    let footer = FooterComponent::new(session, Arc::new(FakeFooterData { provider_count: 1 }));
    let stats = strip_terminal_sequences(&footer.render(120)[1]);
    assert!(stats.contains("$1.234"), "stats={stats}");
    assert!(!stats.contains("(sub)"), "stats={stats}");
}
