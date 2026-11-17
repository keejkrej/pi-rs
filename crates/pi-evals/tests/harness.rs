//! Port of packages/evals/test/harness.test.ts

use pi_coding_agent::config::{get_docs_path, get_examples_path, get_readme_path};
use pi_coding_agent::core::system_prompt::{BuildSystemPromptOptions, build_system_prompt};
use pi_evals::harness::{
    DOCUMENTATION_EVAL_TOOLS, ModelSelectionEnvironment, PiCodingAgentHarnessOptions, PiCodingAgentModelSelection,
    apply_isolated_environment, create_pi_documentation_eval_harness, exclude_pi_documentation,
    resolve_documentation_variant, resolve_model_selection, verify_system_prompt,
};
use pi_evals::plan::DocumentationVariant;
use pi_js::env::testing::EnvGuard;

mod resolve_model_selection_tests {
    use super::*;

    #[test]
    fn prefers_an_explicit_harness_model() {
        let explicit = PiCodingAgentModelSelection {
            provider: "anthropic".to_string(),
            id: "claude-opus-4-6".to_string(),
        };
        let env = ModelSelectionEnvironment {
            pi_provider: Some("openai-codex".to_string()),
            pi_model: Some("gpt-5.6-sol".to_string()),
        };
        assert_eq!(
            resolve_model_selection(Some(&explicit), Some(&env)).unwrap(),
            PiCodingAgentModelSelection {
                provider: "anthropic".to_string(),
                id: "claude-opus-4-6".to_string(),
            }
        );
    }

    #[test]
    fn uses_trimmed_environment_defaults() {
        let env = ModelSelectionEnvironment {
            pi_provider: Some(" openai-codex ".to_string()),
            pi_model: Some(" gpt-5.6-sol ".to_string()),
        };
        assert_eq!(
            resolve_model_selection(None, Some(&env)).unwrap(),
            PiCodingAgentModelSelection {
                provider: "openai-codex".to_string(),
                id: "gpt-5.6-sol".to_string(),
            }
        );
    }

    #[test]
    fn rejects_incomplete_model_selection() {
        let cases = [
            ModelSelectionEnvironment::default(),
            ModelSelectionEnvironment {
                pi_provider: Some("openai-codex".to_string()),
                pi_model: None,
            },
            ModelSelectionEnvironment {
                pi_provider: None,
                pi_model: Some("gpt-5.6-sol".to_string()),
            },
        ];
        for env in cases {
            let err = resolve_model_selection(None, Some(&env)).unwrap_err();
            assert!(err.to_string().contains("Select a harness model explicitly"), "{err}");
        }
    }
}

mod isolate_process_environment_tests {
    use super::*;

    #[test]
    fn removes_runner_metadata_and_restores_the_process_environment() {
        let _variant_guard = EnvGuard::set("PI_EVAL_VARIANT", Some("with_docs"));
        let _artifact_guard = EnvGuard::set("PI_EVAL_ARTIFACT_DIR", Some("/tmp/artifacts"));
        let old_home = pi_js::env::var("HOME");

        let restore = apply_isolated_environment("/tmp/eval-home", "/tmp/eval-agent");
        assert_eq!(pi_js::env::home_dir(), "/tmp/eval-home");
        assert_eq!(
            pi_js::env::var("PI_CODING_AGENT_DIR").as_deref(),
            Some("/tmp/eval-agent")
        );
        assert_eq!(pi_js::env::var("PI_EVAL_VARIANT"), None);
        assert_eq!(pi_js::env::var("PI_EVAL_ARTIFACT_DIR"), None);

        restore.restore();
        assert_eq!(pi_js::env::var("HOME"), old_home);
        assert_eq!(pi_js::env::var("PI_EVAL_VARIANT").as_deref(), Some("with_docs"));
    }
}

mod documentation_variant_tests {
    use super::*;

    #[test]
    fn accepts_valid_documentation_variants() {
        assert_eq!(
            resolve_documentation_variant(Some("without_docs")).unwrap(),
            DocumentationVariant::WithoutDocs
        );
        assert_eq!(
            resolve_documentation_variant(Some("with_docs")).unwrap(),
            DocumentationVariant::WithDocs
        );
    }

    #[test]
    fn rejects_invalid_variants() {
        let _clear_env = EnvGuard::set("PI_EVAL_VARIANT", None);
        for value in [None, Some(""), Some("other")] {
            let err = resolve_documentation_variant(value).unwrap_err();
            assert!(err.to_string().contains("PI_EVAL_VARIANT"), "{err}");
        }
    }

    #[test]
    fn strips_only_the_documentation_routing_section_from_the_default_pi_prompt() {
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            cwd: "/workspace".to_string(),
            selected_tools: Some(DOCUMENTATION_EVAL_TOOLS.iter().map(|s| (*s).to_string()).collect()),
            ..Default::default()
        })
        .unwrap();
        assert!(prompt.contains("\n<docs>\nPi documentation (read only"), "{prompt}");
        assert!(prompt.contains("\n<rules>\n"), "{prompt}");
        assert!(prompt.contains("\n<cwd>\n/workspace\n</cwd>"), "{prompt}");
        assert!(prompt.contains("docs/models.md"), "{prompt}");

        let stripped = exclude_pi_documentation(&prompt).unwrap();
        assert!(stripped.contains("\n<rules>\n"), "{stripped}");
        assert!(stripped.contains("\n<cwd>\n/workspace\n</cwd>"), "{stripped}");
        assert!(!stripped.contains("<docs>"), "{stripped}");
        assert!(!stripped.contains("Pi documentation"), "{stripped}");
        assert!(!stripped.contains("docs/models.md"), "{stripped}");
        assert!(!stripped.contains(&get_readme_path()), "{stripped}");
        assert!(!stripped.contains(&get_docs_path()), "{stripped}");
        assert!(!stripped.contains(&get_examples_path()), "{stripped}");
    }

    #[test]
    fn verifies_the_prompt_that_was_sent() {
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            cwd: "/workspace".to_string(),
            selected_tools: Some(DOCUMENTATION_EVAL_TOOLS.iter().map(|s| (*s).to_string()).collect()),
            ..Default::default()
        })
        .unwrap();
        let stripped = exclude_pi_documentation(&prompt).unwrap();
        assert_eq!(
            verify_system_prompt(&stripped, Some("without_docs"), Some(false)).unwrap(),
            stripped
        );
        let err = verify_system_prompt(&prompt, Some("without_docs"), Some(false)).unwrap_err();
        assert!(err.to_string().contains("does not match"), "{err}");
    }

    #[test]
    fn fails_closed_when_prompt_markers_are_missing() {
        let err1 = exclude_pi_documentation("Instructions").unwrap_err();
        assert!(err1.to_string().contains("no Pi documentation section"), "{err1}");

        let err2 = exclude_pi_documentation("\n<docs>\nPi documentation\n</docs>").unwrap_err();
        assert!(err2.to_string().contains("no working-directory section"), "{err2}");
    }

    #[test]
    fn rejects_documentation_harnesses_outside_the_container_sandbox() {
        let _container = EnvGuard::set("PI_EVAL_CONTAINER", None);
        let _uid = EnvGuard::set("PI_EVAL_SANDBOX_UID", None);
        let _gid = EnvGuard::set("PI_EVAL_SANDBOX_GID", None);
        let err = create_pi_documentation_eval_harness(PiCodingAgentHarnessOptions::default())
            .err()
            .expect("expected sandbox error outside container");
        assert!(err.to_string().contains("isolated container sandbox"), "{err}");
    }
}
