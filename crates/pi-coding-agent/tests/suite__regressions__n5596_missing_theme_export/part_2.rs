//! Port of packages/coding-agent/test/suite/regressions/5596-missing-theme-export.test.ts (unit coding-agent-interactive-theme)

// #5596

use pi_coding_agent::modes::interactive::theme::theme::{
    ThemeExportColors, get_resolved_theme_colors, get_theme_export_colors, init_theme, is_light_theme, try_theme,
};
use serial_test::serial;

struct ThemeResetGuard;

impl Drop for ThemeResetGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
    }
}

// PORT: Full `session.exportToHtml(outputPath)` flow lives in part 1 (`coding-agent-agent-session-lifecycle`)
// once Stage 3A `core::agent_session` and `core::export_html` merge onto main. This unit covers the
// `coding-agent-interactive-theme` fallback guarantee when `init_theme` is given a missing theme name.
mod regression_5596_missing_configured_theme_export {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    #[serial]
    fn falls_back_to_the_active_theme_when_the_configured_theme_is_missing() {
        let _guard = ThemeResetGuard;

        init_theme(Some("missing-theme"), None);

        assert!(try_theme().is_ok());
        assert!(!get_resolved_theme_colors(None).is_empty());
        assert_eq!(
            get_resolved_theme_colors(Some("missing-theme")),
            get_resolved_theme_colors(None)
        );
        let _ = is_light_theme(Some("missing-theme"));
        assert_eq!(
            get_theme_export_colors(Some("missing-theme")),
            ThemeExportColors::default()
        );
    }
}
