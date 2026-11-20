//! Port of packages/ai/test/strict-tool-limits.test.ts

use pi_ai::api::strict_tool_limits::{has_exceeded_strict_tool_limits, mark_strict_tool_limits_exceeded};
use pi_ai::session_resources::cleanup_session_resources;
use serial_test::serial;

#[test]
#[serial]
fn remembers_marked_sessions_until_cleanup_and_bounds_cache_to_10000() {
    cleanup_session_resources(None).unwrap();

    mark_strict_tool_limits_exceeded(Some("a"));
    mark_strict_tool_limits_exceeded(Some("b"));
    mark_strict_tool_limits_exceeded(None);
    assert!(has_exceeded_strict_tool_limits(Some("a")));
    assert!(!has_exceeded_strict_tool_limits(Some("c")));
    assert!(!has_exceeded_strict_tool_limits(None));

    cleanup_session_resources(Some("a")).unwrap();
    assert!(!has_exceeded_strict_tool_limits(Some("a")));
    assert!(has_exceeded_strict_tool_limits(Some("b")));

    cleanup_session_resources(None).unwrap();
    assert!(!has_exceeded_strict_tool_limits(Some("b")));

    for index in 0..=10_000 {
        mark_strict_tool_limits_exceeded(Some(&format!("session-{index}")));
    }
    assert!(!has_exceeded_strict_tool_limits(Some("session-0")));
    assert!(has_exceeded_strict_tool_limits(Some("session-1")));
    assert!(has_exceeded_strict_tool_limits(Some("session-10000")));

    cleanup_session_resources(None).unwrap();
}
