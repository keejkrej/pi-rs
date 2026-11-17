//! Port of packages/coding-agent/src/utils/pi-user-agent.ts

pub fn get_pi_user_agent(version: &str) -> String {
    let runtime = format!("rust/{}", env!("CARGO_PKG_VERSION"));
    format!(
        "pi/{version} ({}; {runtime}; {})",
        pi_js::env::platform(),
        pi_js::env::arch()
    )
}
