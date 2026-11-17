//! Port of packages/coding-agent/test/changelog.test.ts

use pi_coding_agent::utils::changelog::{
    ChangelogEntry, normalize_changelog_links, normalize_changelog_links_for_entry,
};

mod normalize_changelog_links_tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn rewrites_package_relative_changelog_links_to_tag_pinned_github_source_links() {
        let entry = ChangelogEntry {
            major: 0,
            minor: 79,
            patch: 0,
            content: String::new(),
        };
        let markdown = [
            "[Project Trust](README.md#project-trust)",
            "[Extensions](docs/extensions.md#project_trust)",
            "[Examples](examples/extensions/)",
            "[Root README](../../README.md#supply-chain-hardening)",
        ]
        .join("\n");

        assert_eq!(
            normalize_changelog_links_for_entry(&markdown, &entry),
            [
                "[Project Trust](https://github.com/earendil-works/pi/blob/v0.79.0/packages/coding-agent/README.md#project-trust)",
                "[Extensions](https://github.com/earendil-works/pi/blob/v0.79.0/packages/coding-agent/docs/extensions.md#project_trust)",
                "[Examples](https://github.com/earendil-works/pi/tree/v0.79.0/packages/coding-agent/examples/extensions/)",
                "[Root README](https://github.com/earendil-works/pi/blob/v0.79.0/README.md#supply-chain-hardening)",
            ]
            .join("\n"),
        );
    }

    #[test]
    fn canonicalizes_old_repository_urls_without_changing_external_links() {
        let markdown = [
            "[#5167](https://github.com/earendil-works/pi-mono/pull/5167)",
            "[#4163](https://github.com/badlogic/pi-mono/issues/4163)",
            "[Agent README](https://github.com/badlogic/pi-mono/blob/main/packages/agent/README.md)",
            "[External](https://example.com/docs)",
            "[Local anchor](#settings)",
        ]
        .join("\n");

        assert_eq!(
            normalize_changelog_links(&markdown, "0.79.0"),
            [
                "[#5167](https://github.com/earendil-works/pi/pull/5167)",
                "[#4163](https://github.com/earendil-works/pi/issues/4163)",
                "[Agent README](https://github.com/earendil-works/pi/blob/v0.79.0/packages/agent/README.md)",
                "[External](https://example.com/docs)",
                "[Local anchor](#settings)",
            ]
            .join("\n"),
        );
    }
}
