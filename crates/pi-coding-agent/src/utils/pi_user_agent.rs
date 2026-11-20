//! Port of packages/coding-agent/src/utils/pi-user-agent.ts

use pi_ai::utils::identity::{ClientIdentity, UserAgentMode, format_user_agent};

/// pi's `User-Agent` for its own network requests; the same detailed form pi-ai sends to providers.
pub fn get_pi_user_agent(version: &str) -> String {
    format_user_agent(Some(&ClientIdentity {
        user_agent: Some(UserAgentMode::DETAILED),
        attribution: None,
        version: Some(version.to_owned()),
    }))
}
