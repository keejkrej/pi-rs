//! Port of packages/coding-agent/src/core/defaults.ts

pub use pi_ai::ModelThinkingLevel as CodingThinkingLevel;

pub const DEFAULT_THINKING_LEVEL: pi_ai::ModelThinkingLevel = pi_ai::ModelThinkingLevel::Medium;

pub const THINKING_LEVEL_OPTIONS: &[pi_ai::ModelThinkingLevel] = &pi_ai::ModelThinkingLevel::ALL;

pub const fn to_agent_thinking_level(level: pi_ai::ModelThinkingLevel) -> pi_agent_core::ThinkingLevel {
    match level {
        pi_ai::ModelThinkingLevel::Off => pi_agent_core::ThinkingLevel::Off,
        pi_ai::ModelThinkingLevel::Minimal => pi_agent_core::ThinkingLevel::Minimal,
        pi_ai::ModelThinkingLevel::Low => pi_agent_core::ThinkingLevel::Low,
        pi_ai::ModelThinkingLevel::Medium => pi_agent_core::ThinkingLevel::Medium,
        pi_ai::ModelThinkingLevel::High => pi_agent_core::ThinkingLevel::High,
        pi_ai::ModelThinkingLevel::XHigh | pi_ai::ModelThinkingLevel::Max => pi_agent_core::ThinkingLevel::Xhigh,
    }
}

pub const fn from_agent_thinking_level(level: pi_agent_core::ThinkingLevel) -> pi_ai::ModelThinkingLevel {
    match level {
        pi_agent_core::ThinkingLevel::Off => pi_ai::ModelThinkingLevel::Off,
        pi_agent_core::ThinkingLevel::Minimal => pi_ai::ModelThinkingLevel::Minimal,
        pi_agent_core::ThinkingLevel::Low => pi_ai::ModelThinkingLevel::Low,
        pi_agent_core::ThinkingLevel::Medium => pi_ai::ModelThinkingLevel::Medium,
        pi_agent_core::ThinkingLevel::High => pi_ai::ModelThinkingLevel::High,
        pi_agent_core::ThinkingLevel::Xhigh => pi_ai::ModelThinkingLevel::XHigh,
    }
}
