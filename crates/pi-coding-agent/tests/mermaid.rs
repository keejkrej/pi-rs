//! Port of packages/coding-agent/test/mermaid.test.ts
//! Plus comprehensive 1:1 parity tests against grok-mermaid@0.2.3.

use pi_coding_agent::core::settings_manager::MermaidRenderingMode;
use pi_coding_agent::vendor::grok_mermaid::{
    Cls, DiagramKind, MermaidArt, Span, diagram_kind, render, source_box, to_ansi,
};
use pi_tui::{Marked, Token};

#[derive(Clone, Copy, PartialEq, Eq)]
enum MessageType {
    Assistant,
    AssistantThinking,
}

trait TestTheme {
    fn fg(&self, color: &str, text: &str) -> String;
    fn bold(&self, text: &str) -> String;
}

struct TagTheme;

impl TestTheme for TagTheme {
    fn fg(&self, color: &str, text: &str) -> String {
        format!("<{color}>{text}</{color}>")
    }

    fn bold(&self, text: &str) -> String {
        format!("<bold>{text}</bold>")
    }
}

struct TransformOptions<'a> {
    max_width: usize,
    is_streaming: bool,
    message_type: MessageType,
    mode: MermaidRenderingMode,
    theme: Option<&'a dyn TestTheme>,
}

impl Default for TransformOptions<'_> {
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

fn code_span(line: &str) -> String {
    let content = if line.is_empty() { "\u{00a0}" } else { line };
    let mut longest = 0usize;
    let mut run = 0usize;
    for ch in content.chars() {
        if ch == '`' {
            run += 1;
            if run > longest {
                longest = run;
            }
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat(longest + 1);
    let padding = if content.starts_with('`') || content.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{padding}{content}{padding}{fence}")
}

fn style_span(span: &Span, theme: &dyn TestTheme) -> String {
    match span.cls {
        Cls::Border => theme.fg("borderMuted", &span.text),
        Cls::Text => theme.fg("text", &span.text),
        Cls::Edge => theme.fg("accent", &span.text),
        Cls::EdgeLabel => theme.fg("muted", &span.text),
        Cls::Title => theme.fg("accent", &theme.bold(&span.text)),
        Cls::None => span.text.clone(),
    }
}

fn themed_lines(art: &MermaidArt, theme: &dyn TestTheme) -> Vec<String> {
    art.styled
        .iter()
        .map(|row| row.iter().map(|span| style_span(span, theme)).collect::<String>())
        .collect()
}

// WIRE(@tui-w3): delegate to `pi_coding_agent::modes::interactive::components::mermaid::create_mermaid_markdown_transformer` once @tui-w3 is merged.
fn transform_mermaid(markdown: &str, options: TransformOptions<'_>) -> String {
    if options.mode == MermaidRenderingMode::Off
        || options.message_type == MessageType::AssistantThinking
        || (options.is_streaming && options.mode != MermaidRenderingMode::Streaming)
    {
        return markdown.to_owned();
    }

    let parser = Marked::new();
    let tokens = parser.lexer(markdown, None);
    let mut out = String::new();
    for token in tokens.tokens {
        match token {
            Token::Code(code)
                if code
                    .lang
                    .as_deref()
                    .and_then(|l| l.split_whitespace().next())
                    .is_some_and(|w| w.eq_ignore_ascii_case("mermaid")) =>
            {
                let Some(art) = render(&code.text) else {
                    out.push_str(&code.raw);
                    continue;
                };
                if art.width > options.max_width {
                    out.push_str(&code.raw);
                    continue;
                }
                if !options.is_streaming && !art.warnings.is_empty() {
                    let suffix = if art.warnings.len() > 1 {
                        format!(" (+{} more)", art.warnings.len() - 1)
                    } else {
                        String::new()
                    };
                    let warning = format!("Mermaid diagram not rendered: {}{suffix}", art.warnings[0]);
                    let styled_warning = match options.theme {
                        Some(theme) => theme.fg("warning", &warning),
                        None => warning,
                    };
                    out.push_str(&format!("{}\n{}  \n", code.raw, code_span(&styled_warning)));
                    continue;
                }
                let lines = match options.theme {
                    Some(theme) => themed_lines(&art, theme),
                    None => art.plain.clone(),
                };
                let joined = lines.iter().map(|l| code_span(l)).collect::<Vec<_>>().join("  \n");
                out.push_str(&format!("{joined}\n"));
            }
            other => {
                let raw = match other {
                    Token::Blockquote(t) => t.raw,
                    Token::Br(t) => t.raw,
                    Token::Checkbox(t) => t.raw,
                    Token::Code(t) => t.raw,
                    Token::Codespan(t) => t.raw,
                    Token::Def(t) => t.raw,
                    Token::Del(t) => t.raw,
                    Token::Em(t) => t.raw,
                    Token::Escape(t) => t.raw,
                    Token::Heading(t) => t.raw,
                    Token::Hr(t) => t.raw,
                    Token::Html(t) => t.raw,
                    Token::Image(t) => t.raw,
                    Token::Link(t) => t.raw,
                    Token::List(t) => t.raw,
                    Token::ListItem(t) => t.raw,
                    Token::Paragraph(t) => t.raw,
                    Token::Space(t) => t.raw,
                    Token::Strong(t) => t.raw,
                    Token::Table(t) => t.raw,
                    Token::Tag(t) => t.raw,
                    Token::Text(t) => t.raw,
                    Token::Generic(t) => t.raw,
                };
                out.push_str(&raw);
            }
        }
    }
    out
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
    fn maps_semantic_spans_through_the_pi_theme() {
        let theme = TagTheme;
        let rendered = transform_mermaid(
            "```mermaid\nflowchart LR\n  A --> B\n```",
            TransformOptions {
                theme: Some(&theme),
                ..Default::default()
            },
        );

        assert!(rendered.contains("<borderMuted>"));
        assert!(rendered.contains("<accent>"));
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
