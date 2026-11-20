//! Port of packages/coding-agent/test/pi-user-agent.test.ts

use fancy_regex::Regex;
use pi_coding_agent::utils::pi_user_agent::get_pi_user_agent;

mod get_pi_user_agent_tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn formats_the_user_agent_expected_by_pi_dev() {
        let user_agent = get_pi_user_agent("1.2.3");
        let expected = format!("pi/1.2.3 ({}; rust; {})", pi_js::env::platform(), pi_js::env::arch());
        assert_eq!(user_agent, expected);
        let pattern = Regex::new(r"^pi/[^\s()]+ \([^;()]+;\s*[^;()]+;\s*[^()]+\)$").expect("valid regex");
        assert!(pattern.is_match(&user_agent).unwrap());
    }
}
