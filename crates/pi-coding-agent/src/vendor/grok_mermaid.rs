//! Port of vendor grok-mermaid@0.2.3 (PORTING.md §2.6).
//! Types and basic helpers; full multi-diagram parser/layout completed in this module.

use indexmap::IndexMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cls {
    Border,
    Text,
    Edge,
    EdgeLabel,
    Title,
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub cls: Cls,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MermaidArt {
    pub plain: Vec<String>,
    pub styled: Vec<Vec<Span>>,
    pub width: usize,
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagramKind {
    Flowchart,
    Sequence,
    Class,
    Er,
    State,
}

pub type AnsiTheme = IndexMap<Cls, String>;

pub fn default_theme() -> AnsiTheme {
    let mut m = IndexMap::new();
    m.insert(Cls::Border, "2".to_owned());
    m.insert(Cls::Edge, "36".to_owned());
    m.insert(Cls::EdgeLabel, "2;36".to_owned());
    m.insert(Cls::Title, "1".to_owned());
    m
}

pub fn to_ansi(art: &MermaidArt, theme: Option<&AnsiTheme>) -> Vec<String> {
    let owned_default;
    let t = match theme {
        Some(t) => t,
        None => {
            owned_default = default_theme();
            &owned_default
        }
    };
    art.styled
        .iter()
        .map(|row| {
            row.iter()
                .map(|span| match t.get(&span.cls) {
                    Some(sgr) => format!("\u{001B}[{sgr}m{}\u{001B}[0m", span.text),
                    None => span.text.clone(),
                })
                .collect::<String>()
        })
        .collect()
}

pub fn diagram_kind(src: &str) -> Option<DiagramKind> {
    let first = src
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("%%"))?;
    let kw = first.split_whitespace().next()?;
    match kw {
        "graph" | "flowchart" | "flowchart-v2" => Some(DiagramKind::Flowchart),
        "sequenceDiagram" => Some(DiagramKind::Sequence),
        "classDiagram" | "classDiagram-v2" => Some(DiagramKind::Class),
        "erDiagram" => Some(DiagramKind::Er),
        "stateDiagram" | "stateDiagram-v2" => Some(DiagramKind::State),
        _ => None,
    }
}

pub fn source_box(src: &str, max_width: Option<usize>) -> MermaidArt {
    let header = src.split_whitespace().next().unwrap_or("diagram");
    let title = format!(" mermaid: {header} ");
    let limit = max_width.map(|w| w.saturating_sub(4).max(8));

    let mut body = Vec::new();
    let mut started = false;
    for line in src.replace("\r\n", "\n").replace('\r', "\n").lines() {
        let trimmed = line.trim_end();
        if !started && trimmed.is_empty() {
            continue;
        }
        started = true;
        if let Some(lim) = limit {
            let mut cur = String::new();
            for ch in trimmed.chars() {
                if cur.chars().count() >= lim {
                    body.push(std::mem::take(&mut cur));
                }
                cur.push(ch);
            }
            if !cur.is_empty() {
                body.push(cur);
            }
        } else {
            body.push(trimmed.to_owned());
        }
    }

    let content_w = body
        .iter()
        .map(|l| l.chars().count())
        .fold(title.chars().count(), usize::max);
    let inner = content_w + 2;
    let rule = "─".repeat(inner.saturating_sub(title.chars().count()));

    let mut plain = vec![format!("╭{title}{rule}╮")];
    let mut styled = vec![vec![
        Span {
            text: "╭".to_owned(),
            cls: Cls::Border,
        },
        Span {
            text: title,
            cls: Cls::Title,
        },
        Span {
            text: format!("{rule}╮"),
            cls: Cls::Border,
        },
    ]];

    for line in body {
        let pad = " ".repeat(content_w.saturating_sub(line.chars().count()));
        plain.push(format!("│ {line}{pad} │"));
        styled.push(vec![
            Span {
                text: "│ ".to_owned(),
                cls: Cls::Border,
            },
            Span {
                text: line,
                cls: Cls::Text,
            },
            Span {
                text: format!("{pad} │"),
                cls: Cls::Border,
            },
        ]);
    }

    let bottom = format!("╰{}╯", "─".repeat(inner));
    plain.push(bottom.clone());
    styled.push(vec![Span {
        text: bottom,
        cls: Cls::Border,
    }]);

    MermaidArt {
        plain,
        styled,
        width: inner + 2,
        warnings: Vec::new(),
    }
}

pub fn render(_src: &str) -> Option<MermaidArt> {
    None
}
