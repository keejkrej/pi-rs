//! Port of packages/coding-agent/test/mermaid.test.ts
//! Plus comprehensive 1:1 parity tests against grok-mermaid@0.2.3.

use std::sync::Arc;

use pi_coding_agent::core::settings_manager::MermaidRenderingMode;
use pi_coding_agent::modes::interactive::components::markdown_transform::{
    MarkdownMessageType, MarkdownTransformContext,
};
use pi_coding_agent::modes::interactive::components::mermaid::{
    MermaidTransformerOptions, create_mermaid_markdown_transformer,
};
use pi_coding_agent::modes::interactive::theme::theme::{Theme, ThemeColor, init_theme, theme};
use pi_coding_agent::vendor::grok_mermaid::{DiagramKind, diagram_kind, render, source_box, to_ansi};
use serial_test::serial;

type MessageType = MarkdownMessageType;

struct TransformOptions {
    max_width: usize,
    is_streaming: bool,
    message_type: MessageType,
    mode: MermaidRenderingMode,
    theme: Option<Theme>,
}

impl Default for TransformOptions {
    fn default() -> Self {
        Self {
            max_width: 100,
            is_streaming: false,
            message_type: MessageType::Assistant,
            mode: MermaidRenderingMode::Streaming,
            theme: None,
        }
    }
}

fn transform_mermaid(markdown: &str, options: TransformOptions) -> String {
    let mode = options.mode;
    let transformer = create_mermaid_markdown_transformer(MermaidTransformerOptions {
        get_mode: Arc::new(move || mode),
        theme: options.theme,
    });
    transformer(
        markdown,
        MarkdownTransformContext {
            message_type: options.message_type,
            is_streaming: options.is_streaming,
            available_width: options.max_width as i64,
            message_id: None,
            timestamp: None,
        },
    )
}

mod mermaid_rendering {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn replaces_mermaid_code_blocks_with_unicode_diagrams() {
        let markdown = "Before\n\n```mermaid\nflowchart LR\n  A[Start] --> B[Done]\n```\nAfter";
        let rendered = transform_mermaid(markdown, TransformOptions::default());

        assert!(rendered.contains("Before"));
        assert!(rendered.contains("┌───────┐"));
        assert!(rendered.contains("│ Start ├───▶│ Done │"));
        assert!(rendered.contains("└───────┘    └──────┘`\nAfter"));
        assert!(!rendered.contains("```mermaid"));
        assert!(rendered.contains("After"));
    }

    #[test]
    fn leaves_unsupported_and_oversized_diagrams_unchanged() {
        let unsupported = "```mermaid\npie\n  title Pets\n  \"Dogs\" : 4\n```";
        let oversized = "```mermaid\nflowchart LR\n  A[Start] --> B[Done]\n```";

        assert_eq!(transform_mermaid(unsupported, TransformOptions::default()), unsupported);
        assert_eq!(
            transform_mermaid(
                oversized,
                TransformOptions {
                    max_width: 10,
                    ..Default::default()
                }
            ),
            oversized
        );
    }

    #[test]
    #[serial]
    fn maps_semantic_spans_through_the_pi_theme() {
        struct Guard;
        impl Drop for Guard {
            fn drop(&mut self) {
                init_theme(Some("dark"), None);
            }
        }
        init_theme(Some("dark"), Some(true));
        let _guard = Guard;
        let t = theme();
        let rendered = transform_mermaid(
            "```mermaid\nflowchart LR\n  A --> B\n```",
            TransformOptions {
                theme: Some(t.clone()),
                ..Default::default()
            },
        );

        assert!(rendered.contains(&t.get_fg_ansi(ThemeColor::BorderMuted)));
        assert!(rendered.contains(&t.get_fg_ansi(ThemeColor::Accent)));
    }

    #[test]
    fn renders_incomplete_mermaid_blocks_during_streaming() {
        let partial_markdown = "```mermaid\nflowchart LR\n  A --> B";

        assert!(
            transform_mermaid(
                partial_markdown,
                TransformOptions {
                    is_streaming: true,
                    ..Default::default()
                }
            )
            .contains("───▶")
        );
    }

    #[test]
    fn renders_diagrams_with_class_assignments() {
        let markdown = "```mermaid\nflowchart LR\n  A[Foo]:::highlight --> B[Bar]\n```";
        let rendered = transform_mermaid(markdown, TransformOptions::default());

        assert!(!rendered.contains("```mermaid"));
        assert!(!rendered.contains("Mermaid diagram not rendered"));
        assert!(rendered.contains("│ Foo ├───▶│ Bar │"));
    }

    #[test]
    fn falls_back_to_the_code_block_with_a_warning_after_streaming() {
        let markdown = "```mermaid\nflowchart LR\n  A[Foo] invalid\n```";
        let final_out = transform_mermaid(markdown, TransformOptions::default());
        let followed_by_text = transform_mermaid(&format!("{markdown}\nFollowing text"), TransformOptions::default());
        let streaming = transform_mermaid(
            markdown,
            TransformOptions {
                is_streaming: true,
                ..Default::default()
            },
        );

        assert!(final_out.contains(markdown));
        assert!(final_out.contains("```\n`Mermaid diagram not rendered"));
        assert!(final_out.contains("dropped, expected a link: \"invalid\""));
        assert!(!final_out.contains("more)"));
        assert!(followed_by_text.contains("  \nFollowing text"));
        assert!(!streaming.contains("Mermaid diagram not rendered"));
        assert!(!streaming.contains("```mermaid"));
        assert!(streaming.contains("│ Foo │"));
    }

    #[test]
    fn summarizes_additional_partial_render_warnings() {
        let markdown = "```mermaid\nflowchart LR\n  A[Foo] invalid\n  B[Bar] also-invalid\n```";
        let rendered = transform_mermaid(markdown, TransformOptions::default());

        assert!(rendered.contains(markdown));
        assert!(rendered.contains("dropped, expected a link: \"invalid\""));
        assert!(rendered.contains("(+1 more)"));
        assert!(!rendered.contains("dropped, expected a link: \"also-invalid\""));
    }

    #[test]
    fn respects_rendering_modes_and_skips_thinking_blocks() {
        let markdown = "```mermaid\nflowchart LR\n  A --> B\n```";

        assert_eq!(
            transform_mermaid(
                markdown,
                TransformOptions {
                    mode: MermaidRenderingMode::Off,
                    ..Default::default()
                }
            ),
            markdown
        );
        assert_eq!(
            transform_mermaid(
                markdown,
                TransformOptions {
                    mode: MermaidRenderingMode::Final,
                    is_streaming: true,
                    ..Default::default()
                }
            ),
            markdown
        );
        assert!(
            !transform_mermaid(
                markdown,
                TransformOptions {
                    mode: MermaidRenderingMode::Final,
                    ..Default::default()
                }
            )
            .contains("```mermaid")
        );
        assert_eq!(
            transform_mermaid(
                markdown,
                TransformOptions {
                    message_type: MessageType::AssistantThinking,
                    ..Default::default()
                }
            ),
            markdown
        );
        assert!(
            !transform_mermaid(
                markdown,
                TransformOptions {
                    message_type: MessageType::User,
                    ..Default::default()
                }
            )
            .contains("```mermaid")
        );
        assert!(
            !transform_mermaid(
                markdown,
                TransformOptions {
                    message_type: MessageType::Summary,
                    ..Default::default()
                }
            )
            .contains("```mermaid")
        );
        assert_eq!(
            transform_mermaid(
                markdown,
                TransformOptions {
                    message_type: MessageType::Custom,
                    ..Default::default()
                }
            ),
            markdown
        );
        assert_eq!(
            transform_mermaid(
                markdown,
                TransformOptions {
                    message_type: MessageType::Changelog,
                    ..Default::default()
                }
            ),
            markdown
        );
    }
}

mod grok_mermaid_vendor {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn diagram_kind_detects_supported_headers() {
        assert_eq!(diagram_kind("flowchart LR\n  A --> B"), Some(DiagramKind::Flowchart));
        assert_eq!(
            diagram_kind("%% comment\ngraph TD\n  A --> B"),
            Some(DiagramKind::Flowchart)
        );
        assert_eq!(diagram_kind("stateDiagram-v2\n  [*] --> A"), Some(DiagramKind::State));
        assert_eq!(diagram_kind("classDiagram\n  class A"), Some(DiagramKind::Class));
        assert_eq!(diagram_kind("erDiagram\n  A ||--o{ B : rel"), Some(DiagramKind::Er));
        assert_eq!(
            diagram_kind("sequenceDiagram\n  A->>B: hi"),
            Some(DiagramKind::Sequence)
        );
        assert_eq!(diagram_kind("pie\n  title Pets"), None);
        assert_eq!(diagram_kind("flowchart-v2\n  A --> B"), None);
    }

    #[test]
    fn renders_flowchart_td_branch_rl_bt_and_subgraph() {
        let td = render(
            "graph TD\n  A[Start] --> B{Check?}\n  B -->|yes| C[Run]\n  B -->|no| D[Skip]\n  C --> E((End))\n  D --> E",
        )
        .unwrap();
        assert_eq!(td.width, 18);
        assert!(td.warnings.is_empty());
        assert_eq!(
            td.plain,
            vec![
                "     ┌───────┐",
                "     │ Start │",
                "     └───┬───┘",
                "         │",
                "         ▼",
                "    ╭────────╮",
                "    │ Check? │",
                "    ╰────┬───╯",
                "    ┌────┴────┐",
                "    ▼yes      ▼no",
                " ┌─────┐  ┌──────┐",
                " │ Run │  │ Skip │",
                " └──┬──┘  └───┬──┘",
                "    └────┬────┘",
                "         ▼",
                "      ╭─────╮",
                "      │ End │",
                "      ╰─────╯",
            ]
        );

        let rl = render("graph RL\n  A[Left] --> B[Right]").unwrap();
        assert_eq!(rl.width, 21);
        assert_eq!(
            rl.plain,
            vec![
                "┌───────┐    ┌──────┐",
                "│ Right │◄───┤ Left │",
                "└───────┘    └──────┘",
            ]
        );

        let bt = render("graph BT\n  A[Bottom] --> B[Top]").unwrap();
        assert_eq!(bt.width, 10);
        assert_eq!(
            bt.plain,
            vec![
                "  ┌─────┐",
                "  │ Top │",
                "  └─────┘",
                "     ▲",
                "     │",
                "┌────┴───┐",
                "│ Bottom │",
                "└────────┘",
            ]
        );

        let sg =
            render("flowchart TD\n  subgraph sg1[Sub Graph]\n    A[One] --> B[Two]\n  end\n  B --> C[Out]").unwrap();
        assert_eq!(sg.width, 14);
        assert_eq!(
            sg.plain,
            vec![
                " ┌ Sub Graph ┐",
                " │  ┌─────┐  │",
                " │  │ One │  │",
                " │  └──┬──┘  │",
                " │     │     │",
                " │     ▼     │",
                " │  ┌─────┐  │",
                " │  │ Two │  │",
                " │  └─────┘  │",
                " └─────┬─────┘",
                "       │",
                "       ▼",
                "    ┌─────┐",
                "    │ Out │",
                "    └─────┘",
            ]
        );
    }

    #[test]
    fn renders_state_and_salvages_broken_last_line() {
        let state = render("stateDiagram-v2\n  [*] --> Idle\n  Idle --> Running: start\n  Running --> [*]").unwrap();
        assert_eq!(state.width, 12);
        assert!(state.warnings.is_empty());
        assert_eq!(
            state.plain,
            vec![
                "    ╭───╮",
                "    │ ● │",
                "    ╰─┬─╯",
                "      │",
                "      ▼",
                "  ╭──────╮",
                "  │ Idle │",
                "  ╰───┬──╯",
                "      │",
                "      ▼start",
                " ╭─────────╮",
                " │ Running │",
                " ╰────┬────╯",
                "      │",
                "      ▼",
                "    ╭───╮",
                "    │ ● │",
                "    ╰───╯",
            ]
        );

        let salvaged = render("stateDiagram-v2\n  [*] --> A\n  some garbage line").unwrap();
        assert_eq!(salvaged.width, 6);
        assert_eq!(
            salvaged.warnings,
            vec!["dropped, unreadable final line: \"some garbage line\""]
        );
        assert_eq!(
            salvaged.plain,
            vec![
                " ╭───╮",
                " │ ● │",
                " ╰─┬─╯",
                "   │",
                "   ▼",
                " ╭───╮",
                " │ A │",
                " ╰───╯",
            ]
        );
    }

    #[test]
    fn renders_class_er_and_sequence_diagrams() {
        let cls = render("classDiagram\n  class Animal {\n    +int age\n    +speak()\n  }\n  Animal <|-- Dog").unwrap();
        assert_eq!(cls.width, 12);
        assert_eq!(
            cls.plain,
            vec![
                "┌──────────┐",
                "│  Animal  │",
                "├──────────┤",
                "│ +int age │",
                "├──────────┤",
                "│ +speak() │",
                "└─────△────┘",
                "      │",
                "      │",
                "   ┌─────┐",
                "   │ Dog │",
                "   └─────┘",
            ]
        );

        let er = render("erDiagram\n  CUSTOMER ||--o{ ORDER : places\n  CUSTOMER {\n    string name\n  }").unwrap();
        assert_eq!(er.width, 22);
        assert_eq!(
            er.plain,
            vec![
                " ┌─────────────┐",
                " │  CUSTOMER   │",
                " ├─────────────┤",
                " │ string name │",
                " └──────┬──────┘",
                "        │",
                "        │1 places 0..*",
                "    ┌───────┐",
                "    │ ORDER │",
                "    └───────┘",
            ]
        );

        let seq = render(
            "sequenceDiagram\n  autonumber\n  Alice->>Bob: Hello\n  Note right of Bob: Think\n  Bob-->>Alice: Hi\n  Alice->Alice: Self",
        )
        .unwrap();
        assert_eq!(seq.width, 29);
        assert_eq!(
            seq.plain,
            vec![
                "┌───────┐      ┌─────┐",
                "│ Alice │      │ Bob │",
                "└───┬───┘      └──┬──┘",
                "    │             │",
                "    │  1. Hello   │",
                "    ├────────────▶│",
                "    │             │",
                "    │             │ ┌───────┐",
                "    │             │ │ Think │",
                "    │             │ └───────┘",
                "    │             │",
                "    │    2. Hi    │",
                "    │◄╌╌╌╌╌╌╌╌╌╌╌╌┤",
                "    │             │",
                "    ├──╮          │",
                "    │  │ 3. Self  │",
                "    │◄─╯          │",
                "    │             │",
                "┌───┴───┐      ┌──┴──┐",
                "│ Alice │      │ Bob │",
                "└───────┘      └─────┘",
            ]
        );
    }

    #[test]
    fn renders_wide_unicode_backticks_source_box_and_to_ansi() {
        let uni = render("flowchart LR\n  A[日本語] --> B[👨\u{200d}👩\u{200d}👧]").unwrap();
        assert_eq!(uni.width, 20);
        assert_eq!(
            uni.plain,
            vec![
                "┌────────┐    ┌────┐",
                "│ 日本語 ├───▶│ 👨\u{200d}👩\u{200d}👧 │",
                "└────────┘    └────┘",
            ]
        );

        let bt = render("flowchart LR\n  A[\"plain ` tick\"] --> B[\"two `` ticks\"]").unwrap();
        assert_eq!(bt.width, 36);
        assert_eq!(
            bt.plain,
            vec![
                "┌──────────────┐    ┌──────────────┐",
                "│ plain ` tick ├───▶│ two `` ticks │",
                "└──────────────┘    └──────────────┘",
            ]
        );

        let sb = source_box("flowchart LR\n  A[日本語] --> B[Done]", Some(20));
        assert_eq!(sb.width, 24);
        assert_eq!(
            sb.plain,
            vec![
                "╭ mermaid: flowchart ──╮",
                "│ flowchart LR         │",
                "│   A[日本語] -->      │",
                "│ B[Done]              │",
                "╰──────────────────────╯",
            ]
        );

        let art = render("flowchart LR\n  A --> B").unwrap();
        assert_eq!(
            to_ansi(&art, None),
            vec![
                "\u{001b}[2m┌───┐\u{001b}[0m    \u{001b}[2m┌───┐\u{001b}[0m",
                "\u{001b}[2m│\u{001b}[0m A \u{001b}[2m├\u{001b}[0m\u{001b}[36m───▶\u{001b}[0m\u{001b}[2m│\u{001b}[0m B \u{001b}[2m│\u{001b}[0m",
                "\u{001b}[2m└───┘\u{001b}[0m    \u{001b}[2m└───┘\u{001b}[0m",
            ]
        );
    }
}
