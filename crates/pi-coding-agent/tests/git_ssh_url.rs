//! Port of packages/coding-agent/test/git-ssh-url.test.ts

use pi_coding_agent::utils::git::parse_git_url;

mod git_url_parsing {
    use super::*;
    use pretty_assertions::assert_eq;

    mod protocol_urls_accepted_without_git_prefix {
        use super::*;
        use pretty_assertions::assert_eq;

        #[test]
        fn should_parse_https_url() {
            let result = parse_git_url("https://github.com/user/repo").unwrap();
            assert_eq!(result.host, "github.com");
            assert_eq!(result.path, "user/repo");
            assert_eq!(result.repo, "https://github.com/user/repo");
        }

        #[test]
        fn should_parse_ssh_url() {
            let result = parse_git_url("ssh://git@github.com/user/repo").unwrap();
            assert_eq!(result.host, "github.com");
            assert_eq!(result.path, "user/repo");
            assert_eq!(result.repo, "ssh://git@github.com/user/repo");
        }

        #[test]
        fn should_parse_protocol_url_with_ref() {
            let result = parse_git_url("https://github.com/user/repo@v1.0.0").unwrap();
            assert_eq!(result.host, "github.com");
            assert_eq!(result.path, "user/repo");
            assert_eq!(result.ref_name.as_deref(), Some("v1.0.0"));
            assert_eq!(result.repo, "https://github.com/user/repo");
        }
    }

    mod shorthand_urls_accepted_only_with_git_prefix {
        use super::*;
        use pretty_assertions::assert_eq;

        #[test]
        fn should_parse_git_at_host_path_with_git_prefix() {
            let result = parse_git_url("git:git@github.com:user/repo").unwrap();
            assert_eq!(result.host, "github.com");
            assert_eq!(result.path, "user/repo");
            assert_eq!(result.repo, "git@github.com:user/repo");
        }

        #[test]
        fn should_parse_host_path_shorthand_with_git_prefix() {
            let result = parse_git_url("git:github.com/user/repo").unwrap();
            assert_eq!(result.host, "github.com");
            assert_eq!(result.path, "user/repo");
            assert_eq!(result.repo, "https://github.com/user/repo");
        }

        #[test]
        fn should_parse_shorthand_with_ref_and_git_prefix() {
            let result = parse_git_url("git:git@github.com:user/repo@v1.0.0").unwrap();
            assert_eq!(result.host, "github.com");
            assert_eq!(result.path, "user/repo");
            assert_eq!(result.ref_name.as_deref(), Some("v1.0.0"));
            assert_eq!(result.repo, "git@github.com:user/repo");
        }
    }

    #[test]
    fn should_reject_unsafe_git_install_path_inputs() {
        for source in [
            "git:git@evil.example:../../victim/repo",
            "https://evil.example/..%2F..%2Fvictim/repo",
            "https://evil.example/..%2F..%2Fvictim/repo%",
            "git:git@evil.example:/absolute/repo",
            "git:git@evil.example:user\\repo/name",
            "git:git@evil.example:user/repo\0name",
        ] {
            assert_eq!(parse_git_url(source), None, "expected None for {source}");
        }
    }

    mod unsupported_without_git_prefix {
        use super::*;
        use pretty_assertions::assert_eq;

        #[test]
        fn should_reject_git_at_host_path_without_git_prefix() {
            assert_eq!(parse_git_url("git@github.com:user/repo"), None);
        }

        #[test]
        fn should_reject_host_path_shorthand_without_git_prefix() {
            assert_eq!(parse_git_url("github.com/user/repo"), None);
        }

        #[test]
        fn should_reject_user_repo_shorthand() {
            assert_eq!(parse_git_url("user/repo"), None);
        }
    }
}
