//! Port of `packages/coding-agent/test/cache-stats.test.ts`.

use pi_agent_core::AgentMessage;
use pi_ai::{AssistantMessage, Message, StopReason, Usage, UsageCost};
use pi_coding_agent::core::cache_stats::{
    ModelPriceSource, collect_cache_misses, compute_cache_waste, detect_cache_miss,
};
use pi_coding_agent::core::session_manager::{CompactionEntry, SessionEntry, SessionMessageEntry, UsageEntry};

struct FixedPriceSource;

impl ModelPriceSource for FixedPriceSource {
    fn get_cache_read_cost(&self, _provider: &str, _model_id: &str) -> Option<f64> {
        Some(0.3)
    }
}

fn make_assistant(
    input: i64,
    cache_read: i64,
    cache_write: i64,
    cost_cache_read: f64,
    cost_cache_write: f64,
    model: &str,
    timestamp: i64,
) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: "anthropic-messages".to_owned(),
        provider: "test".to_owned(),
        model: model.to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            input,
            output: 10,
            cache_read,
            cache_write,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 0,
            cost: UsageCost {
                input: 0.0,
                output: 0.0,
                cache_read: cost_cache_read,
                cache_write: cost_cache_write,
                total: 0.0,
                total_from_provider: None,
            },
        },
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp,
        duration_ms: None,
    }
}

fn msg_entry(message: AssistantMessage) -> SessionEntry {
    SessionEntry::Message(SessionMessageEntry {
        id: "x".to_owned(),
        parent_id: None,
        timestamp: String::new(),
        message: AgentMessage::Message(Message::Assistant(message)),
    })
}

fn usage_entry(kind: &str, timestamp_ms: i64) -> SessionEntry {
    SessionEntry::Usage(UsageEntry {
        id: format!("usage-{kind}"),
        parent_id: None,
        timestamp: pi_js::time::iso_string(timestamp_ms),
        kind: kind.to_owned(),
        provider: "test".to_owned(),
        model: "test-model".to_owned(),
        usage: Usage {
            input: 0,
            output: 0,
            cache_read: 100_000,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 100_000,
            cost: Default::default(),
        },
        note: None,
    })
}

fn turn1() -> AssistantMessage {
    make_assistant(0, 0, 100_000, 0.0, 0.375, "test-model", 0)
}

fn turn2() -> AssistantMessage {
    make_assistant(0, 100_000, 5_000, 0.03, 0.019, "test-model", 60_000)
}

#[test]
fn compute_cache_waste_cases() {
    let models = FixedPriceSource;

    let turn3 = make_assistant(0, 0, 110_000, 0.0, 0.4125, "test-model", 120_000);
    let totals = compute_cache_waste(&[msg_entry(turn1()), msg_entry(turn2()), msg_entry(turn3)], &models);
    assert_eq!(totals.missed_tokens, 105_000);
    assert!((totals.missed_cost - 0.36225).abs() < 1e-5);

    let healthy = compute_cache_waste(&[msg_entry(turn1()), msg_entry(turn2())], &models);
    assert_eq!(healthy.missed_tokens, 0);
    assert_eq!(healthy.missed_cost, 0.0);

    let reset = SessionEntry::Compaction(CompactionEntry {
        id: "c".to_owned(),
        parent_id: None,
        timestamp: String::new(),
        summary: String::new(),
        first_kept_entry_id: String::new(),
        tokens_before: 0,
        system_message: None,
        usage: None,
        details: None,
        from_hook: None,
        first_kept_entry_index: None,
    });
    let after_reset = make_assistant(0, 0, 20_000, 0.0, 0.075, "test-model", 0);
    let reset_totals = compute_cache_waste(&[msg_entry(turn1()), reset, msg_entry(after_reset)], &models);
    assert_eq!(reset_totals.missed_tokens, 0);

    let other_model = make_assistant(0, 0, 100_000, 0.0, 0.375, "other-model", 0);
    let switch_totals = compute_cache_waste(&[msg_entry(turn1()), msg_entry(other_model)], &models);
    assert_eq!(switch_totals.missed_tokens, 100_000);
    assert_eq!(switch_totals.miss_count, 1);

    let a = make_assistant(100_000, 0, 0, 0.0, 0.0, "test-model", 0);
    let b = make_assistant(110_000, 0, 0, 0.0, 0.0, "test-model", 0);
    let no_cache_totals = compute_cache_waste(&[msg_entry(a), msg_entry(b)], &models);
    assert_eq!(no_cache_totals.missed_tokens, 0);
}

#[test]
fn collect_cache_misses_maps_counted_misses() {
    let models = FixedPriceSource;
    let miss_turn = make_assistant(0, 0, 110_000, 0.0, 0.4125, "test-model", 120_000);
    let misses = collect_cache_misses(&[msg_entry(turn1()), msg_entry(turn2()), msg_entry(miss_turn)], &models);
    assert_eq!(misses.len(), 1);
    assert_eq!(misses.get(&2).unwrap().missed_tokens, 105_000);
}

#[test]
fn detect_cache_miss_scenarios() {
    let models = FixedPriceSource;

    let miss_msg = make_assistant(0, 0, 110_000, 0.0, 0.4125, "test-model", 600_000);
    let miss = detect_cache_miss(&[msg_entry(turn1()), msg_entry(turn2())], &miss_msg, &models).unwrap();
    assert_eq!(miss.missed_tokens, 105_000);
    assert!((miss.missed_cost - 0.36225).abs() < 1e-5);
    assert_eq!(miss.idle_ms, 540_000);
    assert!(!miss.model_changed);

    let other_model = make_assistant(0, 0, 110_000, 0.0, 0.4125, "other-model", 120_000);
    let switch_miss = detect_cache_miss(&[msg_entry(turn1()), msg_entry(turn2())], &other_model, &models).unwrap();
    assert_eq!(switch_miss.missed_tokens, 105_000);
    assert!(switch_miss.model_changed);

    let after_cache_warm = detect_cache_miss(
        &[msg_entry(turn1()), usage_entry("cache_warm", 500_000)],
        &miss_msg,
        &models,
    );
    let after_other_usage = detect_cache_miss(
        &[msg_entry(turn1()), usage_entry("custom_operation", 500_000)],
        &miss_msg,
        &models,
    );
    assert_eq!(after_cache_warm.unwrap().idle_ms, 100_000);
    assert_eq!(after_other_usage.unwrap().idle_ms, 600_000);

    let healthy = make_assistant(0, 105_000, 2_000, 0.0315, 0.0075, "test-model", 120_000);
    assert!(detect_cache_miss(&[msg_entry(turn1()), msg_entry(turn2())], &healthy, &models).is_none());

    assert!(detect_cache_miss(&[], &turn1(), &models).is_none());
}
