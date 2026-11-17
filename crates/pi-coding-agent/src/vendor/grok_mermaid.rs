//! Port of vendor grok-mermaid@0.2.3 (PORTING.md §2.6).
//! Full 1:1 Rust port of `grok-mermaid/src/*`: width tables, labels, canvas,
//! parsers (flowchart, state, class, ER, sequence), layout, routing, ANSI, and sourceBox.

use std::sync::LazyLock;

use indexmap::IndexMap;

/// Semantic class of a run of cells. The renderer never knows about colour;
/// consumers map these to their own theme (see `to_ansi` for the common case).
///
/// - `Border`     box outlines, subgraph frames, compartment rules
/// - `Text`       node / participant / compartment labels
/// - `Edge`       connector lines and arrowheads
/// - `EdgeLabel`  text sitting on an edge
/// - `Title`      the `mermaid: <kind>` header of a source box
/// - `None`       blank filler
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cls {
    Border,
    Text,
    Edge,
    EdgeLabel,
    Title,
    None,
}

/// A run of adjacent cells sharing one semantic class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub cls: Cls,
}

/// A rendered diagram. `plain[i]` and `styled[i]` describe the same row:
/// `plain` is right-trimmed for display width and copy/paste, `styled` keeps
/// the run structure needed to colour it.
///
/// `width` is the display columns the widest row needs — the number to compare
/// against the space you have. It cannot be recovered from `plain`, whose rows
/// are strings of code points, not columns.
///
/// `warnings` lists source the flowchart grammar could not read and dropped.
/// Non-empty means the art is real but incomplete — some of what was written is
/// not in it. Only flowcharts warn; the other grammars refuse the whole diagram
/// instead, and `render` returns `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MermaidArt {
    pub plain: Vec<String>,
    pub styled: Vec<Vec<Span>>,
    pub width: usize,
    pub warnings: Vec<String>,
}

/// A diagram type this renderer draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagramKind {
    Flowchart,
    Sequence,
    Class,
    Er,
    State,
}

/// SGR parameter per semantic class, e.g. `"2"` for dim, `"36"` for cyan,
/// `"38;5;244"` for a 256-colour index. A class left out is printed unstyled.
pub type AnsiTheme = IndexMap<Cls, String>;

/// Dim frame, plain labels, cyan connectors. Readable on light and dark.
pub static DEFAULT_THEME: LazyLock<AnsiTheme> = LazyLock::new(default_theme);

/// Dim frame, plain labels, cyan connectors. Readable on light and dark.
pub fn default_theme() -> AnsiTheme {
    let mut m = IndexMap::new();
    m.insert(Cls::Border, "2".to_owned());
    m.insert(Cls::Edge, "36".to_owned());
    m.insert(Cls::EdgeLabel, "2;36".to_owned());
    m.insert(Cls::Title, "1".to_owned());
    m
}

/// Render art to ANSI-coloured lines.
///
/// A convenience over mapping `art.styled` yourself — reach for that directly
/// when your TUI has its own styling model.
pub fn to_ansi(art: &MermaidArt, theme: Option<&AnsiTheme>) -> Vec<String> {
    let t = theme.unwrap_or(&*DEFAULT_THEME);
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

/// The kind of diagram `src` declares, or `None` if its header names no type
/// this renderer draws.
///
/// Reads the header only — it says nothing about whether the body parses. Pair
/// it with `render` to tell a source this renderer will never draw from one that
/// is merely malformed.
pub fn diagram_kind(src: &str) -> Option<DiagramKind> {
    parse::diagram_kind(src)
}

/// Frame `src` in a titled box, hard-wrapping its lines to `max_width` columns.
///
/// The result can still exceed `max_width`: the body wraps to
/// `max(8, max_width - 4)` and the ` mermaid: <kind> ` title is never truncated,
/// so a long first token sets a floor. Check `width` if it matters.
pub fn source_box(src: &str, max_width: Option<usize>) -> MermaidArt {
    source_box::source_box(src, max_width)
}

/// Render a Mermaid source block as Unicode box-drawing art.
///
/// Supported: `graph`/`flowchart` (including `subgraph`), `stateDiagram`,
/// `classDiagram`, `erDiagram` and `sequenceDiagram`.
pub fn render(src: &str) -> Option<MermaidArt> {
    let src = labels::strip_controls(src);
    if pi_js::str16::trim(&src).is_empty() {
        return None;
    }
    let drawn = attempt(&src)?;
    let (plain, styled, width) = drawn.canvas.to_lines();
    Some(MermaidArt {
        plain,
        styled,
        width,
        warnings: drawn.warnings,
    })
}

struct Drawn {
    canvas: canvas::Canvas,
    warnings: Vec<String>,
}

/// Draw `src`, retrying once without its last line if the grammar rejects it.
fn attempt(src: &str) -> Option<Drawn> {
    if let Some(drawn) = draw(src) {
        return Some(drawn);
    }

    let body = src.trim_end_matches(pi_js::str16::is_js_whitespace);
    let cut = body.rfind('\n')?;
    let mut salvaged = draw(&body[..cut])?;

    let dropped = pi_js::str16::trim(&body[cut + 1..]);
    salvaged
        .warnings
        .push(format!("dropped, unreadable final line: \"{dropped}\""));
    Some(salvaged)
}

/// Dispatch on the declared diagram type; `None` means nothing was drawn.
fn draw(src: &str) -> Option<Drawn> {
    let plain = |canvas: Option<canvas::Canvas>| {
        canvas.map(|canvas| Drawn {
            canvas,
            warnings: Vec::new(),
        })
    };

    match parse::diagram_kind(src)? {
        DiagramKind::Flowchart => {
            let graph = parse::parse_graph(src)?;
            let canvas = if graph.groups.is_empty() {
                layout::layout_flowchart(&graph)
            } else {
                layout::layout_grouped(&graph)
            }?;
            Some(Drawn {
                canvas,
                warnings: graph.warnings,
            })
        }
        DiagramKind::State => {
            let state = parse::parse_state(src)?;
            plain(layout::layout_flowchart(&state))
        }
        DiagramKind::Class => {
            let cls = parse::parse_class(src)?;
            plain(layout::layout_class(&cls.graph, &cls.infos))
        }
        DiagramKind::Er => {
            let er = parse::parse_er(src)?;
            plain(layout::layout_class(&er.graph, &er.infos))
        }
        DiagramKind::Sequence => {
            let seq = parse::parse_sequence(src)?;
            plain(layout_seq::layout_sequence(&seq))
        }
    }
}

mod width_data {
    pub(super) const WIDTHS: &[(u32, u32, u8)] = &[
        (0x0, 0xac, 1),
        (0xad, 0xad, 0),
        (0xae, 0x2ff, 1),
        (0x300, 0x36f, 0),
        (0x370, 0x482, 1),
        (0x483, 0x489, 0),
        (0x48a, 0x590, 1),
        (0x591, 0x5bd, 0),
        (0x5be, 0x5be, 1),
        (0x5bf, 0x5bf, 0),
        (0x5c0, 0x5c0, 1),
        (0x5c1, 0x5c2, 0),
        (0x5c3, 0x5c3, 1),
        (0x5c4, 0x5c5, 0),
        (0x5c6, 0x5c6, 1),
        (0x5c7, 0x5c7, 0),
        (0x5c8, 0x604, 1),
        (0x605, 0x605, 0),
        (0x606, 0x60f, 1),
        (0x610, 0x61a, 0),
        (0x61b, 0x61b, 1),
        (0x61c, 0x61c, 0),
        (0x61d, 0x64a, 1),
        (0x64b, 0x65f, 0),
        (0x660, 0x66f, 1),
        (0x670, 0x670, 0),
        (0x671, 0x6d5, 1),
        (0x6d6, 0x6dc, 0),
        (0x6dd, 0x6de, 1),
        (0x6df, 0x6e4, 0),
        (0x6e5, 0x6e6, 1),
        (0x6e7, 0x6e8, 0),
        (0x6e9, 0x6e9, 1),
        (0x6ea, 0x6ed, 0),
        (0x6ee, 0x70e, 1),
        (0x70f, 0x70f, 0),
        (0x710, 0x710, 1),
        (0x711, 0x711, 0),
        (0x712, 0x72f, 1),
        (0x730, 0x74a, 0),
        (0x74b, 0x7a5, 1),
        (0x7a6, 0x7b0, 0),
        (0x7b1, 0x7ea, 1),
        (0x7eb, 0x7f3, 0),
        (0x7f4, 0x7fc, 1),
        (0x7fd, 0x7fd, 0),
        (0x7fe, 0x815, 1),
        (0x816, 0x819, 0),
        (0x81a, 0x81a, 1),
        (0x81b, 0x823, 0),
        (0x824, 0x824, 1),
        (0x825, 0x827, 0),
        (0x828, 0x828, 1),
        (0x829, 0x82d, 0),
        (0x82e, 0x858, 1),
        (0x859, 0x85b, 0),
        (0x85c, 0x88f, 1),
        (0x890, 0x891, 0),
        (0x892, 0x897, 1),
        (0x898, 0x89f, 0),
        (0x8a0, 0x8c9, 1),
        (0x8ca, 0x902, 0),
        (0x903, 0x939, 1),
        (0x93a, 0x93a, 0),
        (0x93b, 0x93b, 1),
        (0x93c, 0x93c, 0),
        (0x93d, 0x940, 1),
        (0x941, 0x948, 0),
        (0x949, 0x94c, 1),
        (0x94d, 0x94d, 0),
        (0x94e, 0x950, 1),
        (0x951, 0x957, 0),
        (0x958, 0x961, 1),
        (0x962, 0x963, 0),
        (0x964, 0x980, 1),
        (0x981, 0x981, 0),
        (0x982, 0x9bb, 1),
        (0x9bc, 0x9bc, 0),
        (0x9bd, 0x9bd, 1),
        (0x9be, 0x9be, 0),
        (0x9bf, 0x9c0, 1),
        (0x9c1, 0x9c4, 0),
        (0x9c5, 0x9cc, 1),
        (0x9cd, 0x9cd, 0),
        (0x9ce, 0x9d6, 1),
        (0x9d7, 0x9d7, 0),
        (0x9d8, 0x9e1, 1),
        (0x9e2, 0x9e3, 0),
        (0x9e4, 0x9fd, 1),
        (0x9fe, 0x9fe, 0),
        (0x9ff, 0xa00, 1),
        (0xa01, 0xa02, 0),
        (0xa03, 0xa3b, 1),
        (0xa3c, 0xa3c, 0),
        (0xa3d, 0xa40, 1),
        (0xa41, 0xa42, 0),
        (0xa43, 0xa46, 1),
        (0xa47, 0xa48, 0),
        (0xa49, 0xa4a, 1),
        (0xa4b, 0xa4d, 0),
        (0xa4e, 0xa50, 1),
        (0xa51, 0xa51, 0),
        (0xa52, 0xa6f, 1),
        (0xa70, 0xa71, 0),
        (0xa72, 0xa74, 1),
        (0xa75, 0xa75, 0),
        (0xa76, 0xa80, 1),
        (0xa81, 0xa82, 0),
        (0xa83, 0xabb, 1),
        (0xabc, 0xabc, 0),
        (0xabd, 0xac0, 1),
        (0xac1, 0xac5, 0),
        (0xac6, 0xac6, 1),
        (0xac7, 0xac8, 0),
        (0xac9, 0xacc, 1),
        (0xacd, 0xacd, 0),
        (0xace, 0xae1, 1),
        (0xae2, 0xae3, 0),
        (0xae4, 0xaf9, 1),
        (0xafa, 0xaff, 0),
        (0xb00, 0xb00, 1),
        (0xb01, 0xb01, 0),
        (0xb02, 0xb3b, 1),
        (0xb3c, 0xb3c, 0),
        (0xb3d, 0xb3d, 1),
        (0xb3e, 0xb3f, 0),
        (0xb40, 0xb40, 1),
        (0xb41, 0xb44, 0),
        (0xb45, 0xb4c, 1),
        (0xb4d, 0xb4d, 0),
        (0xb4e, 0xb54, 1),
        (0xb55, 0xb57, 0),
        (0xb58, 0xb61, 1),
        (0xb62, 0xb63, 0),
        (0xb64, 0xb81, 1),
        (0xb82, 0xb82, 0),
        (0xb83, 0xbbd, 1),
        (0xbbe, 0xbbe, 0),
        (0xbbf, 0xbbf, 1),
        (0xbc0, 0xbc0, 0),
        (0xbc1, 0xbcc, 1),
        (0xbcd, 0xbcd, 0),
        (0xbce, 0xbd6, 1),
        (0xbd7, 0xbd7, 0),
        (0xbd8, 0xbff, 1),
        (0xc00, 0xc00, 0),
        (0xc01, 0xc03, 1),
        (0xc04, 0xc04, 0),
        (0xc05, 0xc3b, 1),
        (0xc3c, 0xc3c, 0),
        (0xc3d, 0xc3d, 1),
        (0xc3e, 0xc40, 0),
        (0xc41, 0xc45, 1),
        (0xc46, 0xc48, 0),
        (0xc49, 0xc49, 1),
        (0xc4a, 0xc4d, 0),
        (0xc4e, 0xc54, 1),
        (0xc55, 0xc56, 0),
        (0xc57, 0xc61, 1),
        (0xc62, 0xc63, 0),
        (0xc64, 0xc80, 1),
        (0xc81, 0xc81, 0),
        (0xc82, 0xcbb, 1),
        (0xcbc, 0xcbc, 0),
        (0xcbd, 0xcbe, 1),
        (0xcbf, 0xcc0, 0),
        (0xcc1, 0xcc1, 1),
        (0xcc2, 0xcc2, 0),
        (0xcc3, 0xcc5, 1),
        (0xcc6, 0xcc8, 0),
        (0xcc9, 0xcc9, 1),
        (0xcca, 0xccd, 0),
        (0xcce, 0xcd4, 1),
        (0xcd5, 0xcd6, 0),
        (0xcd7, 0xce1, 1),
        (0xce2, 0xce3, 0),
        (0xce4, 0xcff, 1),
        (0xd00, 0xd01, 0),
        (0xd02, 0xd3a, 1),
        (0xd3b, 0xd3c, 0),
        (0xd3d, 0xd3d, 1),
        (0xd3e, 0xd3e, 0),
        (0xd3f, 0xd40, 1),
        (0xd41, 0xd44, 0),
        (0xd45, 0xd4c, 1),
        (0xd4d, 0xd4e, 0),
        (0xd4f, 0xd56, 1),
        (0xd57, 0xd57, 0),
        (0xd58, 0xd61, 1),
        (0xd62, 0xd63, 0),
        (0xd64, 0xd80, 1),
        (0xd81, 0xd81, 0),
        (0xd82, 0xdc9, 1),
        (0xdca, 0xdca, 0),
        (0xdcb, 0xdce, 1),
        (0xdcf, 0xdcf, 0),
        (0xdd0, 0xdd1, 1),
        (0xdd2, 0xdd4, 0),
        (0xdd5, 0xdd5, 1),
        (0xdd6, 0xdd6, 0),
        (0xdd7, 0xdde, 1),
        (0xddf, 0xddf, 0),
        (0xde0, 0xe30, 1),
        (0xe31, 0xe31, 0),
        (0xe32, 0xe33, 1),
        (0xe34, 0xe3a, 0),
        (0xe3b, 0xe46, 1),
        (0xe47, 0xe4e, 0),
        (0xe4f, 0xeb0, 1),
        (0xeb1, 0xeb1, 0),
        (0xeb2, 0xeb3, 1),
        (0xeb4, 0xebc, 0),
        (0xebd, 0xec7, 1),
        (0xec8, 0xece, 0),
        (0xecf, 0xf17, 1),
        (0xf18, 0xf19, 0),
        (0xf1a, 0xf34, 1),
        (0xf35, 0xf35, 0),
        (0xf36, 0xf36, 1),
        (0xf37, 0xf37, 0),
        (0xf38, 0xf38, 1),
        (0xf39, 0xf39, 0),
        (0xf3a, 0xf70, 1),
        (0xf71, 0xf7e, 0),
        (0xf7f, 0xf7f, 1),
        (0xf80, 0xf84, 0),
        (0xf85, 0xf85, 1),
        (0xf86, 0xf87, 0),
        (0xf88, 0xf8c, 1),
        (0xf8d, 0xf97, 0),
        (0xf98, 0xf98, 1),
        (0xf99, 0xfbc, 0),
        (0xfbd, 0xfc5, 1),
        (0xfc6, 0xfc6, 0),
        (0xfc7, 0x102c, 1),
        (0x102d, 0x1030, 0),
        (0x1031, 0x1031, 1),
        (0x1032, 0x1037, 0),
        (0x1038, 0x1038, 1),
        (0x1039, 0x103a, 0),
        (0x103b, 0x103c, 1),
        (0x103d, 0x103e, 0),
        (0x103f, 0x1057, 1),
        (0x1058, 0x1059, 0),
        (0x105a, 0x105d, 1),
        (0x105e, 0x1060, 0),
        (0x1061, 0x1070, 1),
        (0x1071, 0x1074, 0),
        (0x1075, 0x1081, 1),
        (0x1082, 0x1082, 0),
        (0x1083, 0x1084, 1),
        (0x1085, 0x1086, 0),
        (0x1087, 0x108c, 1),
        (0x108d, 0x108d, 0),
        (0x108e, 0x109c, 1),
        (0x109d, 0x109d, 0),
        (0x109e, 0x10ff, 1),
        (0x1100, 0x115f, 2),
        (0x1160, 0x11ff, 0),
        (0x1200, 0x135c, 1),
        (0x135d, 0x135f, 0),
        (0x1360, 0x1711, 1),
        (0x1712, 0x1714, 0),
        (0x1715, 0x1731, 1),
        (0x1732, 0x1733, 0),
        (0x1734, 0x1751, 1),
        (0x1752, 0x1753, 0),
        (0x1754, 0x1771, 1),
        (0x1772, 0x1773, 0),
        (0x1774, 0x17a3, 1),
        (0x17a4, 0x17a4, 2),
        (0x17a5, 0x17b3, 1),
        (0x17b4, 0x17b5, 0),
        (0x17b6, 0x17b6, 1),
        (0x17b7, 0x17bd, 0),
        (0x17be, 0x17c5, 1),
        (0x17c6, 0x17c6, 0),
        (0x17c7, 0x17c8, 1),
        (0x17c9, 0x17d3, 0),
        (0x17d4, 0x17d7, 1),
        (0x17d8, 0x17d8, 3),
        (0x17d9, 0x17dc, 1),
        (0x17dd, 0x17dd, 0),
        (0x17de, 0x180a, 1),
        (0x180b, 0x180f, 0),
        (0x1810, 0x1884, 1),
        (0x1885, 0x1886, 0),
        (0x1887, 0x18a8, 1),
        (0x18a9, 0x18a9, 0),
        (0x18aa, 0x191f, 1),
        (0x1920, 0x1922, 0),
        (0x1923, 0x1926, 1),
        (0x1927, 0x1928, 0),
        (0x1929, 0x1931, 1),
        (0x1932, 0x1932, 0),
        (0x1933, 0x1938, 1),
        (0x1939, 0x193b, 0),
        (0x193c, 0x1a16, 1),
        (0x1a17, 0x1a18, 0),
        (0x1a19, 0x1a1a, 1),
        (0x1a1b, 0x1a1b, 0),
        (0x1a1c, 0x1a55, 1),
        (0x1a56, 0x1a56, 0),
        (0x1a57, 0x1a57, 1),
        (0x1a58, 0x1a5e, 0),
        (0x1a5f, 0x1a5f, 1),
        (0x1a60, 0x1a60, 0),
        (0x1a61, 0x1a61, 1),
        (0x1a62, 0x1a62, 0),
        (0x1a63, 0x1a64, 1),
        (0x1a65, 0x1a6c, 0),
        (0x1a6d, 0x1a72, 1),
        (0x1a73, 0x1a7c, 0),
        (0x1a7d, 0x1a7e, 1),
        (0x1a7f, 0x1a7f, 0),
        (0x1a80, 0x1aaf, 1),
        (0x1ab0, 0x1ace, 0),
        (0x1acf, 0x1aff, 1),
        (0x1b00, 0x1b03, 0),
        (0x1b04, 0x1b33, 1),
        (0x1b34, 0x1b3d, 0),
        (0x1b3e, 0x1b41, 1),
        (0x1b42, 0x1b43, 0),
        (0x1b44, 0x1b6a, 1),
        (0x1b6b, 0x1b73, 0),
        (0x1b74, 0x1b7f, 1),
        (0x1b80, 0x1b81, 0),
        (0x1b82, 0x1ba1, 1),
        (0x1ba2, 0x1ba5, 0),
        (0x1ba6, 0x1ba7, 1),
        (0x1ba8, 0x1ba9, 0),
        (0x1baa, 0x1baa, 1),
        (0x1bab, 0x1bad, 0),
        (0x1bae, 0x1be5, 1),
        (0x1be6, 0x1be6, 0),
        (0x1be7, 0x1be7, 1),
        (0x1be8, 0x1be9, 0),
        (0x1bea, 0x1bec, 1),
        (0x1bed, 0x1bed, 0),
        (0x1bee, 0x1bee, 1),
        (0x1bef, 0x1bf1, 0),
        (0x1bf2, 0x1c2b, 1),
        (0x1c2c, 0x1c33, 0),
        (0x1c34, 0x1c35, 1),
        (0x1c36, 0x1c37, 0),
        (0x1c38, 0x1ccf, 1),
        (0x1cd0, 0x1cd2, 0),
        (0x1cd3, 0x1cd3, 1),
        (0x1cd4, 0x1ce0, 0),
        (0x1ce1, 0x1ce1, 1),
        (0x1ce2, 0x1ce8, 0),
        (0x1ce9, 0x1cec, 1),
        (0x1ced, 0x1ced, 0),
        (0x1cee, 0x1cf3, 1),
        (0x1cf4, 0x1cf4, 0),
        (0x1cf5, 0x1cf7, 1),
        (0x1cf8, 0x1cf9, 0),
        (0x1cfa, 0x1dbf, 1),
        (0x1dc0, 0x1dff, 0),
        (0x1e00, 0x200a, 1),
        (0x200b, 0x200f, 0),
        (0x2010, 0x2029, 1),
        (0x202a, 0x202e, 0),
        (0x202f, 0x205f, 1),
        (0x2060, 0x206f, 0),
        (0x2070, 0x20cf, 1),
        (0x20d0, 0x20f0, 0),
        (0x20f1, 0x2319, 1),
        (0x231a, 0x231b, 2),
        (0x231c, 0x2328, 1),
        (0x2329, 0x232a, 2),
        (0x232b, 0x23e8, 1),
        (0x23e9, 0x23ec, 2),
        (0x23ed, 0x23ef, 1),
        (0x23f0, 0x23f0, 2),
        (0x23f1, 0x23f2, 1),
        (0x23f3, 0x23f3, 2),
        (0x23f4, 0x25fc, 1),
        (0x25fd, 0x25fe, 2),
        (0x25ff, 0x2613, 1),
        (0x2614, 0x2615, 2),
        (0x2616, 0x2647, 1),
        (0x2648, 0x2653, 2),
        (0x2654, 0x267e, 1),
        (0x267f, 0x267f, 2),
        (0x2680, 0x2692, 1),
        (0x2693, 0x2693, 2),
        (0x2694, 0x26a0, 1),
        (0x26a1, 0x26a1, 2),
        (0x26a2, 0x26a9, 1),
        (0x26aa, 0x26ab, 2),
        (0x26ac, 0x26bc, 1),
        (0x26bd, 0x26be, 2),
        (0x26bf, 0x26c3, 1),
        (0x26c4, 0x26c5, 2),
        (0x26c6, 0x26cd, 1),
        (0x26ce, 0x26ce, 2),
        (0x26cf, 0x26d3, 1),
        (0x26d4, 0x26d4, 2),
        (0x26d5, 0x26e9, 1),
        (0x26ea, 0x26ea, 2),
        (0x26eb, 0x26f1, 1),
        (0x26f2, 0x26f3, 2),
        (0x26f4, 0x26f4, 1),
        (0x26f5, 0x26f5, 2),
        (0x26f6, 0x26f9, 1),
        (0x26fa, 0x26fa, 2),
        (0x26fb, 0x26fc, 1),
        (0x26fd, 0x26fd, 2),
        (0x26fe, 0x2704, 1),
        (0x2705, 0x2705, 2),
        (0x2706, 0x2709, 1),
        (0x270a, 0x270b, 2),
        (0x270c, 0x2727, 1),
        (0x2728, 0x2728, 2),
        (0x2729, 0x274b, 1),
        (0x274c, 0x274c, 2),
        (0x274d, 0x274d, 1),
        (0x274e, 0x274e, 2),
        (0x274f, 0x2752, 1),
        (0x2753, 0x2755, 2),
        (0x2756, 0x2756, 1),
        (0x2757, 0x2757, 2),
        (0x2758, 0x2794, 1),
        (0x2795, 0x2797, 2),
        (0x2798, 0x27af, 1),
        (0x27b0, 0x27b0, 2),
        (0x27b1, 0x27be, 1),
        (0x27bf, 0x27bf, 2),
        (0x27c0, 0x2b1a, 1),
        (0x2b1b, 0x2b1c, 2),
        (0x2b1d, 0x2b4f, 1),
        (0x2b50, 0x2b50, 2),
        (0x2b51, 0x2b54, 1),
        (0x2b55, 0x2b55, 2),
        (0x2b56, 0x2cee, 1),
        (0x2cef, 0x2cf1, 0),
        (0x2cf2, 0x2ddf, 1),
        (0x2de0, 0x2dff, 0),
        (0x2e00, 0x2e7f, 1),
        (0x2e80, 0x2e99, 2),
        (0x2e9a, 0x2e9a, 1),
        (0x2e9b, 0x2ef3, 2),
        (0x2ef4, 0x2eff, 1),
        (0x2f00, 0x2fd5, 2),
        (0x2fd6, 0x2fef, 1),
        (0x2ff0, 0x3029, 2),
        (0x302a, 0x302f, 0),
        (0x3030, 0x303e, 2),
        (0x303f, 0x3040, 1),
        (0x3041, 0x3096, 2),
        (0x3097, 0x3098, 1),
        (0x3099, 0x309a, 0),
        (0x309b, 0x30ff, 2),
        (0x3100, 0x3104, 1),
        (0x3105, 0x312f, 2),
        (0x3130, 0x3130, 1),
        (0x3131, 0x3163, 2),
        (0x3164, 0x3164, 0),
        (0x3165, 0x318e, 2),
        (0x318f, 0x318f, 1),
        (0x3190, 0x31e3, 2),
        (0x31e4, 0x31ee, 1),
        (0x31ef, 0x321e, 2),
        (0x321f, 0x321f, 1),
        (0x3220, 0x3247, 2),
        (0x3248, 0x324f, 1),
        (0x3250, 0x4dbf, 2),
        (0x4dc0, 0x4dff, 1),
        (0x4e00, 0xa48c, 2),
        (0xa48d, 0xa48f, 1),
        (0xa490, 0xa4c6, 2),
        (0xa4c7, 0xa66e, 1),
        (0xa66f, 0xa672, 0),
        (0xa673, 0xa673, 1),
        (0xa674, 0xa67d, 0),
        (0xa67e, 0xa69d, 1),
        (0xa69e, 0xa69f, 0),
        (0xa6a0, 0xa6ef, 1),
        (0xa6f0, 0xa6f1, 0),
        (0xa6f2, 0xa801, 1),
        (0xa802, 0xa802, 0),
        (0xa803, 0xa805, 1),
        (0xa806, 0xa806, 0),
        (0xa807, 0xa80a, 1),
        (0xa80b, 0xa80b, 0),
        (0xa80c, 0xa824, 1),
        (0xa825, 0xa826, 0),
        (0xa827, 0xa82b, 1),
        (0xa82c, 0xa82c, 0),
        (0xa82d, 0xa8c3, 1),
        (0xa8c4, 0xa8c5, 0),
        (0xa8c6, 0xa8df, 1),
        (0xa8e0, 0xa8f1, 0),
        (0xa8f2, 0xa8f9, 1),
        (0xa8fa, 0xa8fa, 0),
        (0xa8fb, 0xa8fe, 1),
        (0xa8ff, 0xa8ff, 0),
        (0xa900, 0xa925, 1),
        (0xa926, 0xa92d, 0),
        (0xa92e, 0xa946, 1),
        (0xa947, 0xa951, 0),
        (0xa952, 0xa95f, 1),
        (0xa960, 0xa97c, 2),
        (0xa97d, 0xa97f, 1),
        (0xa980, 0xa982, 0),
        (0xa983, 0xa9b2, 1),
        (0xa9b3, 0xa9b3, 0),
        (0xa9b4, 0xa9b5, 1),
        (0xa9b6, 0xa9b9, 0),
        (0xa9ba, 0xa9bb, 1),
        (0xa9bc, 0xa9bd, 0),
        (0xa9be, 0xa9e4, 1),
        (0xa9e5, 0xa9e5, 0),
        (0xa9e6, 0xaa28, 1),
        (0xaa29, 0xaa2e, 0),
        (0xaa2f, 0xaa30, 1),
        (0xaa31, 0xaa32, 0),
        (0xaa33, 0xaa34, 1),
        (0xaa35, 0xaa36, 0),
        (0xaa37, 0xaa42, 1),
        (0xaa43, 0xaa43, 0),
        (0xaa44, 0xaa4b, 1),
        (0xaa4c, 0xaa4c, 0),
        (0xaa4d, 0xaa7b, 1),
        (0xaa7c, 0xaa7c, 0),
        (0xaa7d, 0xaaaf, 1),
        (0xaab0, 0xaab0, 0),
        (0xaab1, 0xaab1, 1),
        (0xaab2, 0xaab4, 0),
        (0xaab5, 0xaab6, 1),
        (0xaab7, 0xaab8, 0),
        (0xaab9, 0xaabd, 1),
        (0xaabe, 0xaabf, 0),
        (0xaac0, 0xaac0, 1),
        (0xaac1, 0xaac1, 0),
        (0xaac2, 0xaaeb, 1),
        (0xaaec, 0xaaed, 0),
        (0xaaee, 0xaaf5, 1),
        (0xaaf6, 0xaaf6, 0),
        (0xaaf7, 0xabe4, 1),
        (0xabe5, 0xabe5, 0),
        (0xabe6, 0xabe7, 1),
        (0xabe8, 0xabe8, 0),
        (0xabe9, 0xabec, 1),
        (0xabed, 0xabed, 0),
        (0xabee, 0xabff, 1),
        (0xac00, 0xd7a3, 2),
        (0xd7a4, 0xd7af, 1),
        (0xd7b0, 0xd7c6, 0),
        (0xd7c7, 0xd7ca, 1),
        (0xd7cb, 0xd7fb, 0),
        (0xd7fc, 0xf8ff, 1),
        (0xf900, 0xfaff, 2),
        (0xfb00, 0xfb1d, 1),
        (0xfb1e, 0xfb1e, 0),
        (0xfb1f, 0xfdff, 1),
        (0xfe00, 0xfe0f, 0),
        (0xfe10, 0xfe19, 2),
        (0xfe1a, 0xfe1f, 1),
        (0xfe20, 0xfe2f, 0),
        (0xfe30, 0xfe52, 2),
        (0xfe53, 0xfe53, 1),
        (0xfe54, 0xfe66, 2),
        (0xfe67, 0xfe67, 1),
        (0xfe68, 0xfe6b, 2),
        (0xfe6c, 0xfefe, 1),
        (0xfeff, 0xfeff, 0),
        (0xff00, 0xff00, 1),
        (0xff01, 0xff60, 2),
        (0xff61, 0xff9d, 1),
        (0xff9e, 0xffa0, 0),
        (0xffa1, 0xffdf, 1),
        (0xffe0, 0xffe6, 2),
        (0xffe7, 0xffef, 1),
        (0xfff0, 0xfff8, 0),
        (0xfff9, 0x101fc, 1),
        (0x101fd, 0x101fd, 0),
        (0x101fe, 0x102df, 1),
        (0x102e0, 0x102e0, 0),
        (0x102e1, 0x10375, 1),
        (0x10376, 0x1037a, 0),
        (0x1037b, 0x10a00, 1),
        (0x10a01, 0x10a03, 0),
        (0x10a04, 0x10a04, 1),
        (0x10a05, 0x10a06, 0),
        (0x10a07, 0x10a0b, 1),
        (0x10a0c, 0x10a0f, 0),
        (0x10a10, 0x10a37, 1),
        (0x10a38, 0x10a3a, 0),
        (0x10a3b, 0x10a3e, 1),
        (0x10a3f, 0x10a3f, 0),
        (0x10a40, 0x10ae4, 1),
        (0x10ae5, 0x10ae6, 0),
        (0x10ae7, 0x10d23, 1),
        (0x10d24, 0x10d27, 0),
        (0x10d28, 0x10eaa, 1),
        (0x10eab, 0x10eac, 0),
        (0x10ead, 0x10efc, 1),
        (0x10efd, 0x10eff, 0),
        (0x10f00, 0x10f45, 1),
        (0x10f46, 0x10f50, 0),
        (0x10f51, 0x10f81, 1),
        (0x10f82, 0x10f85, 0),
        (0x10f86, 0x11000, 1),
        (0x11001, 0x11001, 0),
        (0x11002, 0x11037, 1),
        (0x11038, 0x11046, 0),
        (0x11047, 0x1106f, 1),
        (0x11070, 0x11070, 0),
        (0x11071, 0x11072, 1),
        (0x11073, 0x11074, 0),
        (0x11075, 0x1107e, 1),
        (0x1107f, 0x11081, 0),
        (0x11082, 0x110b2, 1),
        (0x110b3, 0x110b6, 0),
        (0x110b7, 0x110b8, 1),
        (0x110b9, 0x110ba, 0),
        (0x110bb, 0x110c1, 1),
        (0x110c2, 0x110c2, 0),
        (0x110c3, 0x110ff, 1),
        (0x11100, 0x11102, 0),
        (0x11103, 0x11126, 1),
        (0x11127, 0x1112b, 0),
        (0x1112c, 0x1112c, 1),
        (0x1112d, 0x11134, 0),
        (0x11135, 0x11172, 1),
        (0x11173, 0x11173, 0),
        (0x11174, 0x1117f, 1),
        (0x11180, 0x11181, 0),
        (0x11182, 0x111b5, 1),
        (0x111b6, 0x111be, 0),
        (0x111bf, 0x111c1, 1),
        (0x111c2, 0x111c3, 0),
        (0x111c4, 0x111c8, 1),
        (0x111c9, 0x111cc, 0),
        (0x111cd, 0x111ce, 1),
        (0x111cf, 0x111cf, 0),
        (0x111d0, 0x1122e, 1),
        (0x1122f, 0x11231, 0),
        (0x11232, 0x11233, 1),
        (0x11234, 0x11234, 0),
        (0x11235, 0x11235, 1),
        (0x11236, 0x11237, 0),
        (0x11238, 0x1123d, 1),
        (0x1123e, 0x1123e, 0),
        (0x1123f, 0x11240, 1),
        (0x11241, 0x11241, 0),
        (0x11242, 0x112de, 1),
        (0x112df, 0x112df, 0),
        (0x112e0, 0x112e2, 1),
        (0x112e3, 0x112ea, 0),
        (0x112eb, 0x112ff, 1),
        (0x11300, 0x11301, 0),
        (0x11302, 0x1133a, 1),
        (0x1133b, 0x1133c, 0),
        (0x1133d, 0x1133d, 1),
        (0x1133e, 0x1133e, 0),
        (0x1133f, 0x1133f, 1),
        (0x11340, 0x11340, 0),
        (0x11341, 0x11356, 1),
        (0x11357, 0x11357, 0),
        (0x11358, 0x11365, 1),
        (0x11366, 0x1136c, 0),
        (0x1136d, 0x1136f, 1),
        (0x11370, 0x11374, 0),
        (0x11375, 0x11437, 1),
        (0x11438, 0x1143f, 0),
        (0x11440, 0x11441, 1),
        (0x11442, 0x11444, 0),
        (0x11445, 0x11445, 1),
        (0x11446, 0x11446, 0),
        (0x11447, 0x1145d, 1),
        (0x1145e, 0x1145e, 0),
        (0x1145f, 0x114af, 1),
        (0x114b0, 0x114b0, 0),
        (0x114b1, 0x114b2, 1),
        (0x114b3, 0x114b8, 0),
        (0x114b9, 0x114b9, 1),
        (0x114ba, 0x114ba, 0),
        (0x114bb, 0x114bc, 1),
        (0x114bd, 0x114bd, 0),
        (0x114be, 0x114be, 1),
        (0x114bf, 0x114c0, 0),
        (0x114c1, 0x114c1, 1),
        (0x114c2, 0x114c3, 0),
        (0x114c4, 0x115ae, 1),
        (0x115af, 0x115af, 0),
        (0x115b0, 0x115b1, 1),
        (0x115b2, 0x115b5, 0),
        (0x115b6, 0x115bb, 1),
        (0x115bc, 0x115bd, 0),
        (0x115be, 0x115be, 1),
        (0x115bf, 0x115c0, 0),
        (0x115c1, 0x115db, 1),
        (0x115dc, 0x115dd, 0),
        (0x115de, 0x11632, 1),
        (0x11633, 0x1163a, 0),
        (0x1163b, 0x1163c, 1),
        (0x1163d, 0x1163d, 0),
        (0x1163e, 0x1163e, 1),
        (0x1163f, 0x11640, 0),
        (0x11641, 0x116aa, 1),
        (0x116ab, 0x116ab, 0),
        (0x116ac, 0x116ac, 1),
        (0x116ad, 0x116ad, 0),
        (0x116ae, 0x116af, 1),
        (0x116b0, 0x116b5, 0),
        (0x116b6, 0x116b6, 1),
        (0x116b7, 0x116b7, 0),
        (0x116b8, 0x1171c, 1),
        (0x1171d, 0x1171f, 0),
        (0x11720, 0x11721, 1),
        (0x11722, 0x11725, 0),
        (0x11726, 0x11726, 1),
        (0x11727, 0x1172b, 0),
        (0x1172c, 0x1182e, 1),
        (0x1182f, 0x11837, 0),
        (0x11838, 0x11838, 1),
        (0x11839, 0x1183a, 0),
        (0x1183b, 0x1192f, 1),
        (0x11930, 0x11930, 0),
        (0x11931, 0x1193a, 1),
        (0x1193b, 0x1193c, 0),
        (0x1193d, 0x1193d, 1),
        (0x1193e, 0x1193f, 0),
        (0x11940, 0x11940, 1),
        (0x11941, 0x11941, 0),
        (0x11942, 0x11942, 1),
        (0x11943, 0x11943, 0),
        (0x11944, 0x119d3, 1),
        (0x119d4, 0x119d7, 0),
        (0x119d8, 0x119d9, 1),
        (0x119da, 0x119db, 0),
        (0x119dc, 0x119df, 1),
        (0x119e0, 0x119e0, 0),
        (0x119e1, 0x11a00, 1),
        (0x11a01, 0x11a0a, 0),
        (0x11a0b, 0x11a32, 1),
        (0x11a33, 0x11a38, 0),
        (0x11a39, 0x11a39, 1),
        (0x11a3a, 0x11a3e, 0),
        (0x11a3f, 0x11a46, 1),
        (0x11a47, 0x11a47, 0),
        (0x11a48, 0x11a50, 1),
        (0x11a51, 0x11a56, 0),
        (0x11a57, 0x11a58, 1),
        (0x11a59, 0x11a5b, 0),
        (0x11a5c, 0x11a83, 1),
        (0x11a84, 0x11a96, 0),
        (0x11a97, 0x11a97, 1),
        (0x11a98, 0x11a99, 0),
        (0x11a9a, 0x11c2f, 1),
        (0x11c30, 0x11c36, 0),
        (0x11c37, 0x11c37, 1),
        (0x11c38, 0x11c3d, 0),
        (0x11c3e, 0x11c3e, 1),
        (0x11c3f, 0x11c3f, 0),
        (0x11c40, 0x11c91, 1),
        (0x11c92, 0x11ca7, 0),
        (0x11ca8, 0x11ca9, 1),
        (0x11caa, 0x11cb0, 0),
        (0x11cb1, 0x11cb1, 1),
        (0x11cb2, 0x11cb3, 0),
        (0x11cb4, 0x11cb4, 1),
        (0x11cb5, 0x11cb6, 0),
        (0x11cb7, 0x11d30, 1),
        (0x11d31, 0x11d36, 0),
        (0x11d37, 0x11d39, 1),
        (0x11d3a, 0x11d3a, 0),
        (0x11d3b, 0x11d3b, 1),
        (0x11d3c, 0x11d3d, 0),
        (0x11d3e, 0x11d3e, 1),
        (0x11d3f, 0x11d47, 0),
        (0x11d48, 0x11d8f, 1),
        (0x11d90, 0x11d91, 0),
        (0x11d92, 0x11d94, 1),
        (0x11d95, 0x11d95, 0),
        (0x11d96, 0x11d96, 1),
        (0x11d97, 0x11d97, 0),
        (0x11d98, 0x11ef2, 1),
        (0x11ef3, 0x11ef4, 0),
        (0x11ef5, 0x11eff, 1),
        (0x11f00, 0x11f02, 0),
        (0x11f03, 0x11f35, 1),
        (0x11f36, 0x11f3a, 0),
        (0x11f3b, 0x11f3f, 1),
        (0x11f40, 0x11f40, 0),
        (0x11f41, 0x11f41, 1),
        (0x11f42, 0x11f42, 0),
        (0x11f43, 0x1343f, 1),
        (0x13440, 0x13440, 0),
        (0x13441, 0x13446, 1),
        (0x13447, 0x13455, 0),
        (0x13456, 0x16aef, 1),
        (0x16af0, 0x16af4, 0),
        (0x16af5, 0x16b2f, 1),
        (0x16b30, 0x16b36, 0),
        (0x16b37, 0x16f4e, 1),
        (0x16f4f, 0x16f4f, 0),
        (0x16f50, 0x16f8e, 1),
        (0x16f8f, 0x16f92, 0),
        (0x16f93, 0x16fdf, 1),
        (0x16fe0, 0x16fe3, 2),
        (0x16fe4, 0x16fe4, 0),
        (0x16fe5, 0x16fef, 1),
        (0x16ff0, 0x16ff1, 2),
        (0x16ff2, 0x16fff, 1),
        (0x17000, 0x187f7, 2),
        (0x187f8, 0x187ff, 1),
        (0x18800, 0x18cd5, 2),
        (0x18cd6, 0x18cff, 1),
        (0x18d00, 0x18d08, 2),
        (0x18d09, 0x1afef, 1),
        (0x1aff0, 0x1aff3, 2),
        (0x1aff4, 0x1aff4, 1),
        (0x1aff5, 0x1affb, 2),
        (0x1affc, 0x1affc, 1),
        (0x1affd, 0x1affe, 2),
        (0x1afff, 0x1afff, 1),
        (0x1b000, 0x1b122, 2),
        (0x1b123, 0x1b131, 1),
        (0x1b132, 0x1b132, 2),
        (0x1b133, 0x1b14f, 1),
        (0x1b150, 0x1b152, 2),
        (0x1b153, 0x1b154, 1),
        (0x1b155, 0x1b155, 2),
        (0x1b156, 0x1b163, 1),
        (0x1b164, 0x1b167, 2),
        (0x1b168, 0x1b16f, 1),
        (0x1b170, 0x1b2fb, 2),
        (0x1b2fc, 0x1bc9c, 1),
        (0x1bc9d, 0x1bc9e, 0),
        (0x1bc9f, 0x1bc9f, 1),
        (0x1bca0, 0x1bca3, 0),
        (0x1bca4, 0x1ceff, 1),
        (0x1cf00, 0x1cf2d, 0),
        (0x1cf2e, 0x1cf2f, 1),
        (0x1cf30, 0x1cf46, 0),
        (0x1cf47, 0x1d164, 1),
        (0x1d165, 0x1d165, 0),
        (0x1d166, 0x1d166, 1),
        (0x1d167, 0x1d169, 0),
        (0x1d16a, 0x1d16d, 1),
        (0x1d16e, 0x1d182, 0),
        (0x1d183, 0x1d184, 1),
        (0x1d185, 0x1d18b, 0),
        (0x1d18c, 0x1d1a9, 1),
        (0x1d1aa, 0x1d1ad, 0),
        (0x1d1ae, 0x1d241, 1),
        (0x1d242, 0x1d244, 0),
        (0x1d245, 0x1d9ff, 1),
        (0x1da00, 0x1da36, 0),
        (0x1da37, 0x1da3a, 1),
        (0x1da3b, 0x1da6c, 0),
        (0x1da6d, 0x1da74, 1),
        (0x1da75, 0x1da75, 0),
        (0x1da76, 0x1da83, 1),
        (0x1da84, 0x1da84, 0),
        (0x1da85, 0x1da9a, 1),
        (0x1da9b, 0x1da9f, 0),
        (0x1daa0, 0x1daa0, 1),
        (0x1daa1, 0x1daaf, 0),
        (0x1dab0, 0x1dfff, 1),
        (0x1e000, 0x1e006, 0),
        (0x1e007, 0x1e007, 1),
        (0x1e008, 0x1e018, 0),
        (0x1e019, 0x1e01a, 1),
        (0x1e01b, 0x1e021, 0),
        (0x1e022, 0x1e022, 1),
        (0x1e023, 0x1e024, 0),
        (0x1e025, 0x1e025, 1),
        (0x1e026, 0x1e02a, 0),
        (0x1e02b, 0x1e08e, 1),
        (0x1e08f, 0x1e08f, 0),
        (0x1e090, 0x1e12f, 1),
        (0x1e130, 0x1e136, 0),
        (0x1e137, 0x1e2ad, 1),
        (0x1e2ae, 0x1e2ae, 0),
        (0x1e2af, 0x1e2eb, 1),
        (0x1e2ec, 0x1e2ef, 0),
        (0x1e2f0, 0x1e4eb, 1),
        (0x1e4ec, 0x1e4ef, 0),
        (0x1e4f0, 0x1e8cf, 1),
        (0x1e8d0, 0x1e8d6, 0),
        (0x1e8d7, 0x1e943, 1),
        (0x1e944, 0x1e94a, 0),
        (0x1e94b, 0x1f003, 1),
        (0x1f004, 0x1f004, 2),
        (0x1f005, 0x1f0ce, 1),
        (0x1f0cf, 0x1f0cf, 2),
        (0x1f0d0, 0x1f18d, 1),
        (0x1f18e, 0x1f18e, 2),
        (0x1f18f, 0x1f190, 1),
        (0x1f191, 0x1f19a, 2),
        (0x1f19b, 0x1f1ff, 1),
        (0x1f200, 0x1f202, 2),
        (0x1f203, 0x1f20f, 1),
        (0x1f210, 0x1f23b, 2),
        (0x1f23c, 0x1f23f, 1),
        (0x1f240, 0x1f248, 2),
        (0x1f249, 0x1f24f, 1),
        (0x1f250, 0x1f251, 2),
        (0x1f252, 0x1f25f, 1),
        (0x1f260, 0x1f265, 2),
        (0x1f266, 0x1f2ff, 1),
        (0x1f300, 0x1f320, 2),
        (0x1f321, 0x1f32c, 1),
        (0x1f32d, 0x1f335, 2),
        (0x1f336, 0x1f336, 1),
        (0x1f337, 0x1f37c, 2),
        (0x1f37d, 0x1f37d, 1),
        (0x1f37e, 0x1f393, 2),
        (0x1f394, 0x1f39f, 1),
        (0x1f3a0, 0x1f3ca, 2),
        (0x1f3cb, 0x1f3ce, 1),
        (0x1f3cf, 0x1f3d3, 2),
        (0x1f3d4, 0x1f3df, 1),
        (0x1f3e0, 0x1f3f0, 2),
        (0x1f3f1, 0x1f3f3, 1),
        (0x1f3f4, 0x1f3f4, 2),
        (0x1f3f5, 0x1f3f7, 1),
        (0x1f3f8, 0x1f43e, 2),
        (0x1f43f, 0x1f43f, 1),
        (0x1f440, 0x1f440, 2),
        (0x1f441, 0x1f441, 1),
        (0x1f442, 0x1f4fc, 2),
        (0x1f4fd, 0x1f4fe, 1),
        (0x1f4ff, 0x1f53d, 2),
        (0x1f53e, 0x1f54a, 1),
        (0x1f54b, 0x1f54e, 2),
        (0x1f54f, 0x1f54f, 1),
        (0x1f550, 0x1f567, 2),
        (0x1f568, 0x1f579, 1),
        (0x1f57a, 0x1f57a, 2),
        (0x1f57b, 0x1f594, 1),
        (0x1f595, 0x1f596, 2),
        (0x1f597, 0x1f5a3, 1),
        (0x1f5a4, 0x1f5a4, 2),
        (0x1f5a5, 0x1f5fa, 1),
        (0x1f5fb, 0x1f64f, 2),
        (0x1f650, 0x1f67f, 1),
        (0x1f680, 0x1f6c5, 2),
        (0x1f6c6, 0x1f6cb, 1),
        (0x1f6cc, 0x1f6cc, 2),
        (0x1f6cd, 0x1f6cf, 1),
        (0x1f6d0, 0x1f6d2, 2),
        (0x1f6d3, 0x1f6d4, 1),
        (0x1f6d5, 0x1f6d7, 2),
        (0x1f6d8, 0x1f6db, 1),
        (0x1f6dc, 0x1f6df, 2),
        (0x1f6e0, 0x1f6ea, 1),
        (0x1f6eb, 0x1f6ec, 2),
        (0x1f6ed, 0x1f6f3, 1),
        (0x1f6f4, 0x1f6fc, 2),
        (0x1f6fd, 0x1f7df, 1),
        (0x1f7e0, 0x1f7eb, 2),
        (0x1f7ec, 0x1f7ef, 1),
        (0x1f7f0, 0x1f7f0, 2),
        (0x1f7f1, 0x1f90b, 1),
        (0x1f90c, 0x1f93a, 2),
        (0x1f93b, 0x1f93b, 1),
        (0x1f93c, 0x1f945, 2),
        (0x1f946, 0x1f946, 1),
        (0x1f947, 0x1f9ff, 2),
        (0x1fa00, 0x1fa6f, 1),
        (0x1fa70, 0x1fa7c, 2),
        (0x1fa7d, 0x1fa7f, 1),
        (0x1fa80, 0x1fa88, 2),
        (0x1fa89, 0x1fa8f, 1),
        (0x1fa90, 0x1fabd, 2),
        (0x1fabe, 0x1fabe, 1),
        (0x1fabf, 0x1fac5, 2),
        (0x1fac6, 0x1facd, 1),
        (0x1face, 0x1fadb, 2),
        (0x1fadc, 0x1fadf, 1),
        (0x1fae0, 0x1fae8, 2),
        (0x1fae9, 0x1faef, 1),
        (0x1faf0, 0x1faf8, 2),
        (0x1faf9, 0x1ffff, 1),
        (0x20000, 0x2fffd, 2),
        (0x2fffe, 0x2ffff, 1),
        (0x30000, 0x3fffd, 2),
        (0x3fffe, 0xdffff, 1),
        (0xe0000, 0xe0fff, 0),
        (0xe1000, 0x10ffff, 1),
    ];
}

mod width {
    use super::width_data::WIDTHS;

    const VS16: u32 = 0xfe0f;

    fn is_regional_indicator(cp: u32) -> bool {
        (0x1f1e6..=0x1f1ff).contains(&cp)
    }

    /// Width of one code point; the table covers the whole code point space.
    fn code_point_width(cp: u32) -> usize {
        let mut lo: usize = 0;
        let mut hi: isize = WIDTHS.len() as isize - 1;
        while lo as isize <= hi {
            let mid = (lo + hi as usize) >> 1;
            let (start, end, w) = WIDTHS[mid];
            if cp < start {
                hi = mid as isize - 1;
            } else if cp > end {
                lo = mid + 1;
            } else {
                return w as usize;
            }
        }
        1
    }

    /// Columns occupied by one grapheme cluster.
    pub(super) fn cluster_width(cluster: &str) -> usize {
        let mut w = 0usize;
        let mut vs16 = false;
        let mut regional = 0usize;
        for ch in cluster.chars() {
            let cp = ch as u32;
            if cp == VS16 {
                vs16 = true;
            }
            if is_regional_indicator(cp) {
                regional += 1;
            }
            let cw = code_point_width(cp);
            if cw > w {
                w = cw;
            }
        }
        if vs16 || regional >= 2 { 2 } else { w }
    }

    /// Iterate clusters paired with their display width.
    pub(super) fn measured(s: &str) -> Vec<(&str, usize)> {
        pi_js::intl::graphemes(s)
            .into_iter()
            .map(|seg| (seg.segment, cluster_width(seg.segment)))
            .collect()
    }

    /// Display columns of a string.
    pub(super) fn string_width(s: &str) -> usize {
        pi_js::intl::graphemes(s)
            .into_iter()
            .map(|seg| cluster_width(seg.segment))
            .sum()
    }
}

mod labels {
    use super::width::{measured, string_width};

    /// Node labels wrap to at most this many display columns per line ...
    pub(super) const WRAP_WIDTH: usize = 24;
    /// ... and at most this many lines; overflow is truncated with an ellipsis.
    pub(super) const MAX_LINES: usize = 4;
    /// Edge labels are truncated to this many columns.
    pub(super) const MAX_LABEL: usize = 28;

    const LABEL_BREAK_CHARS: [char; 4] = ['_', '-', '.', '/'];

    pub(super) fn ascii_lower(s: &str) -> String {
        s.to_ascii_lowercase()
    }

    pub(super) fn ascii_upper(s: &str) -> String {
        s.to_ascii_uppercase()
    }

    pub(super) fn strip_controls(src: &str) -> String {
        src.chars()
            .filter(|&c| {
                let cp = c as u32;
                !(cp <= 0x08 || cp == 0x0b || cp == 0x0c || (0x0e..=0x1f).contains(&cp) || (0x7f..=0x9f).contains(&cp))
            })
            .collect()
    }

    /// Split source into lines matching JS `srcLines`: on `\n`, with trailing `\r`
    /// stripped, and without a trailing empty element when input ends in `\n`.
    pub(super) fn src_lines(src: &str) -> Vec<&str> {
        let mut out: Vec<&str> = src.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
        if out.last().is_some_and(|l| l.is_empty()) {
            out.pop();
        }
        out
    }

    pub(super) fn is_alphanumeric(c: char) -> bool {
        c.is_alphanumeric()
    }

    pub(super) fn is_id_char(c: char) -> bool {
        is_alphanumeric(c) || c == '_'
    }

    const ENTITY_LOOKAHEAD: usize = 10;

    fn decode_entity_body(body: &str) -> Option<String> {
        match body {
            "lt" => return Some("<".to_owned()),
            "gt" => return Some(">".to_owned()),
            "amp" => return Some("&".to_owned()),
            "quot" => return Some("\"".to_owned()),
            "apos" => return Some("'".to_owned()),
            _ => {}
        }
        let num = body.strip_prefix('#')?;
        let (hex, digits) = if let Some(rest) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
            (true, rest)
        } else {
            (false, num)
        };
        if digits.is_empty() {
            return None;
        }
        let valid = if hex {
            digits.chars().all(|c| c.is_ascii_hexdigit())
        } else {
            digits.chars().all(|c| c.is_ascii_digit())
        };
        if !valid {
            return None;
        }
        let code = u32::from_str_radix(digits, if hex { 16 } else { 10 }).ok()?;
        if code > 0x10ffff || (0xd800..=0xdfff).contains(&code) {
            return None;
        }
        if code < 0x20 || (0x7f..=0x9f).contains(&code) {
            return None;
        }
        char::from_u32(code).map(|c| c.to_string())
    }

    pub(super) fn decode_html_entities(s: &str) -> String {
        if !s.contains('&') {
            return s.to_owned();
        }
        let chars: Vec<char> = s.chars().collect();
        let mut out = String::new();
        let mut i = 0usize;
        while i < chars.len() {
            if chars[i] != '&' {
                out.push(chars[i]);
                i += 1;
                continue;
            }
            let hi = (i + 1 + ENTITY_LOOKAHEAD).min(chars.len());
            let mut semi = None;
            for (j, &ch) in chars.iter().enumerate().take(hi).skip(i + 1) {
                if ch == ';' {
                    semi = Some(j);
                    break;
                }
            }
            let decoded = semi.and_then(|s_idx| {
                let body: String = chars[i + 1..s_idx].iter().collect();
                decode_entity_body(&body)
            });
            match (semi, decoded) {
                (Some(s_idx), Some(text)) => {
                    out.push_str(&text);
                    i = s_idx + 1;
                }
                _ => {
                    out.push('&');
                    i += 1;
                }
            }
        }
        out
    }

    pub(super) fn strip_markdown(s: &str) -> String {
        let no_code: String = s.chars().filter(|&c| c != '`').collect();
        let no_strong = no_code.replace("**", "").replace("__", "");
        let chars: Vec<char> = no_strong.chars().collect();
        let mut out = String::new();
        for (i, &c) in chars.iter().enumerate() {
            let in_word =
                i > 0 && is_alphanumeric(chars[i - 1]) && chars.get(i + 1).is_some_and(|&nc| is_alphanumeric(nc));
            if (c == '*' || c == '_') && !in_word {
                continue;
            }
            out.push(c);
        }
        pi_js::str16::trim(&out).to_owned()
    }

    fn is_html_format_tag(lower: &str) -> bool {
        matches!(
            lower,
            "b" | "strong"
                | "i"
                | "em"
                | "u"
                | "s"
                | "strike"
                | "del"
                | "ins"
                | "mark"
                | "small"
                | "big"
                | "sub"
                | "sup"
                | "code"
                | "kbd"
                | "samp"
                | "var"
                | "tt"
                | "span"
                | "font"
                | "q"
                | "abbr"
                | "cite"
                | "pre"
        )
    }

    fn html_tag_at(chars: &[char], start: usize) -> Option<(String, usize)> {
        let mut i = start + 1;
        if chars.get(i) == Some(&'/') {
            i += 1;
        }
        let name_start = i;
        while i < chars.len() && chars[i].is_ascii_alphanumeric() {
            i += 1;
        }
        if i == name_start {
            return None;
        }
        let name: String = chars[name_start..i].iter().collect();
        while i < chars.len() && chars[i] != '>' {
            if chars[i] == '<' {
                return None;
            }
            i += 1;
        }
        if chars.get(i) == Some(&'>') {
            Some((name, i + 1))
        } else {
            None
        }
    }

    pub(super) fn strip_html_tags(s: &str) -> String {
        let chars: Vec<char> = s.chars().collect();
        let mut out = String::new();
        let mut i = 0usize;
        while i < chars.len() {
            if chars[i] == '<'
                && let Some((name, end)) = html_tag_at(&chars, i)
            {
                let lower = name.to_ascii_lowercase();
                if lower == "br" {
                    out.push(' ');
                    i = end;
                    continue;
                }
                if is_html_format_tag(&lower) {
                    i = end;
                    continue;
                }
            }
            out.push(chars[i]);
            i += 1;
        }
        out
    }

    fn unwrap<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
        if s.len() >= open.len() + close.len() && s.starts_with(open) && s.ends_with(close) {
            Some(&s[open.len()..s.len() - close.len()])
        } else {
            None
        }
    }

    pub(super) fn clean_label(raw: &str) -> String {
        let trimmed = pi_js::str16::trim(&strip_html_tags(pi_js::str16::trim(raw))).to_owned();
        let unquoted = unwrap(&trimmed, "\"", "\"")
            .or_else(|| unwrap(&trimmed, "'", "'"))
            .unwrap_or(&trimmed);
        let unquoted = pi_js::str16::trim(unquoted);
        if let Some(md) = unwrap(unquoted, "`", "`") {
            decode_html_entities(&strip_markdown(pi_js::str16::trim(md)))
        } else {
            decode_html_entities(unquoted)
        }
    }

    fn last_break(s: &str) -> Option<usize> {
        s.rfind(LABEL_BREAK_CHARS)
    }

    pub(super) fn wrap_label(label: &str, width: usize, max_lines: usize) -> Vec<String> {
        let width = width.max(1);
        let mut lines: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut cur_w = 0usize;

        for word in label.split(pi_js::str16::is_js_whitespace).filter(|w| !w.is_empty()) {
            let ww = string_width(word);
            if ww > width {
                if !cur.is_empty() {
                    lines.push(std::mem::take(&mut cur));
                }
                let mut chunk = String::new();
                let mut chunk_w = 0usize;
                for (ch, cw) in measured(word) {
                    if chunk_w + cw > width && !chunk.is_empty() {
                        if let Some(p) = last_break(&chunk) {
                            let carry = chunk[p + 1..].to_owned();
                            lines.push(chunk[..=p].to_owned());
                            chunk_w = string_width(&carry);
                            chunk = carry;
                        } else {
                            lines.push(std::mem::take(&mut chunk));
                            chunk_w = 0;
                        }
                    }
                    chunk.push_str(ch);
                    chunk_w += cw;
                }
                cur = chunk;
                cur_w = chunk_w;
            } else if cur.is_empty() {
                cur = word.to_owned();
                cur_w = ww;
            } else if cur_w + 1 + ww <= width {
                cur.push(' ');
                cur.push_str(word);
                cur_w += 1 + ww;
            } else {
                lines.push(cur);
                cur = word.to_owned();
                cur_w = ww;
            }
        }
        if !cur.is_empty() {
            lines.push(cur);
        }
        if lines.is_empty() {
            lines.push(String::new());
        }

        if lines.len() > max_lines && max_lines > 0 {
            lines.truncate(max_lines);
            let target = 1.max(width.saturating_sub(1));
            let mut s = String::new();
            let mut sw = 0usize;
            if let Some(last) = lines.last() {
                for (ch, cw) in measured(last) {
                    if sw + cw > target {
                        break;
                    }
                    s.push_str(ch);
                    sw += cw;
                }
            }
            s.push('…');
            if let Some(last) = lines.last_mut() {
                *last = s;
            }
        }
        lines
    }

    pub(super) fn fit_label(label: &str, inner: usize) -> String {
        if string_width(label) <= inner {
            return label.to_owned();
        }
        let mut out = String::new();
        let mut used = 0usize;
        for (c, cw) in measured(label) {
            if used + cw + 1 > inner {
                break;
            }
            out.push_str(c);
            used += cw;
        }
        out.push('…');
        out
    }
}

mod canvas {
    use super::width::measured;
    use super::{Cls, Span};

    pub(super) const CONT: &str = "\0";

    pub(super) const U: u8 = 1;
    pub(super) const D: u8 = 2;
    pub(super) const L: u8 = 4;
    pub(super) const R: u8 = 8;

    pub(super) const STY_DOT: u8 = 1;
    pub(super) const STY_THICK: u8 = 2;
    pub(super) const STY_SOLID: u8 = 4;

    pub(super) struct Canvas {
        pub(super) w: usize,
        pub(super) h: usize,
        pub(super) ch: Vec<String>,
        pub(super) cls: Vec<Cls>,
        pub(super) mask: Vec<u8>,
        pub(super) style: Vec<u8>,
        pub(super) occupied: Vec<u8>,
        pub(super) cur_style: u8,
    }

    impl Canvas {
        pub(super) fn new(w: usize, h: usize) -> Self {
            let n = w * h;
            Self {
                w,
                h,
                ch: vec![" ".to_owned(); n],
                cls: vec![Cls::None; n],
                mask: vec![0; n],
                style: vec![0; n],
                occupied: vec![0; n],
                cur_style: STY_SOLID,
            }
        }

        pub(super) fn idx(&self, x: usize, y: usize) -> usize {
            y * self.w + x
        }

        pub(super) fn set(&mut self, x: usize, y: usize, c: &str, cls: Cls) {
            if x >= self.w || y >= self.h {
                return;
            }
            let i = self.idx(x, y);
            self.ch[i] = c.to_owned();
            self.cls[i] = cls;
        }

        pub(super) fn add_bits(&mut self, x: usize, y: usize, bits: u8, cls: Cls) {
            if x >= self.w || y >= self.h {
                return;
            }
            let i = self.idx(x, y);
            if self.occupied[i] != 0 {
                return;
            }
            self.mask[i] |= bits;
            self.style[i] |= self.cur_style;
            if self.cls[i] != Cls::Border {
                self.cls[i] = cls;
            }
        }

        pub(super) fn blit(&mut self, sub: &Canvas, ox: usize, oy: usize) {
            for sy in 0..sub.h {
                for sx in 0..sub.w {
                    let x = ox + sx;
                    let y = oy + sy;
                    if x >= self.w || y >= self.h {
                        continue;
                    }
                    let si = sub.idx(sx, sy);
                    let di = self.idx(x, y);
                    self.ch[di] = sub.ch[si].clone();
                    self.cls[di] = sub.cls[si];
                    self.style[di] = sub.style[si];
                    self.occupied[di] = 1;
                }
            }
        }

        pub(super) fn junction(&mut self, x: usize, y: usize, bits: u8) {
            if x >= self.w || y >= self.h {
                return;
            }
            let i = self.idx(x, y);
            self.mask[i] |= bits;
            if self.cls[i] != Cls::Border {
                self.cls[i] = Cls::Edge;
            }
        }

        pub(super) fn seg_v(&mut self, x: usize, y0: usize, y1: usize) {
            let a = y0.min(y1);
            let b = y0.max(y1);
            for y in a..=b {
                let mut bits = 0u8;
                if y > a {
                    bits |= U;
                }
                if y < b {
                    bits |= D;
                }
                self.add_bits(x, y, bits, Cls::Edge);
            }
        }

        pub(super) fn seg_h(&mut self, y: usize, x0: usize, x1: usize) {
            let a = x0.min(x1);
            let b = x0.max(x1);
            for x in a..=b {
                let mut bits = 0u8;
                if x > a {
                    bits |= L;
                }
                if x < b {
                    bits |= R;
                }
                self.add_bits(x, y, bits, Cls::Edge);
            }
        }

        pub(super) fn finalize_mask(&mut self) {
            for i in 0..self.ch.len() {
                if self.mask[i] != 0 && self.ch[i] == " " {
                    let c = mask_char(self.mask[i]);
                    self.ch[i] = if self.style[i] == STY_DOT {
                        dotted_char(c).to_owned()
                    } else if self.style[i] == STY_THICK {
                        thick_char(c).to_owned()
                    } else {
                        c.to_owned()
                    };
                }
            }
        }

        pub(super) fn flip_vertical(&mut self) {
            for y in 0..self.h / 2 {
                let y2 = self.h - 1 - y;
                for x in 0..self.w {
                    let i = self.idx(x, y);
                    let j = self.idx(x, y2);
                    self.ch.swap(i, j);
                    self.cls.swap(i, j);
                }
            }
            for cell in &mut self.ch {
                let flipped = flip_glyph_v(cell);
                if flipped != cell.as_str() {
                    *cell = flipped.to_owned();
                }
            }
        }

        pub(super) fn flip_horizontal(&mut self) {
            for y in 0..self.h {
                for x in 0..self.w / 2 {
                    let x2 = self.w - 1 - x;
                    let i = self.idx(x, y);
                    let j = self.idx(x2, y);
                    self.ch.swap(i, j);
                    self.cls.swap(i, j);
                }
            }
            for cell in &mut self.ch {
                let flipped = flip_glyph_h(cell);
                if flipped != cell.as_str() {
                    *cell = flipped.to_owned();
                }
            }
            for y in 0..self.h {
                let mut x = 0usize;
                while x < self.w {
                    let cls = self.cls[self.idx(x, y)];
                    if cls == Cls::Text || cls == Cls::EdgeLabel {
                        let start = self.idx(x, y);
                        while x < self.w && self.cls[self.idx(x, y)] == cls {
                            x += 1;
                        }
                        let end = self.idx(x, y);
                        self.ch[start..end].reverse();
                    } else {
                        x += 1;
                    }
                }
            }
        }

        pub(super) fn to_lines(&self) -> (Vec<String>, Vec<Vec<Span>>, usize) {
            let mut plain = Vec::with_capacity(self.h);
            let mut styled = Vec::with_capacity(self.h);
            let mut width = 0usize;
            for y in 0..self.h {
                let mut last = 0usize;
                for x in (0..self.w).rev() {
                    if self.ch[self.idx(x, y)] != " " {
                        last = x + 1;
                        break;
                    }
                }
                width = width.max(last);
                let mut spans = Vec::new();
                let mut plain_row = String::new();
                let mut run = String::new();
                let mut run_cls = Cls::None;
                for x in 0..last {
                    let i = self.idx(x, y);
                    let c = &self.ch[i];
                    if c == CONT {
                        continue;
                    }
                    let cls = self.cls[i];
                    plain_row.push_str(c);
                    if cls != run_cls && !run.is_empty() {
                        spans.push(Span {
                            text: std::mem::take(&mut run),
                            cls: run_cls,
                        });
                    }
                    run_cls = cls;
                    run.push_str(c);
                }
                if !run.is_empty() {
                    spans.push(Span {
                        text: run,
                        cls: run_cls,
                    });
                }
                styled.push(spans);
                plain.push(plain_row.trim_end_matches(' ').to_owned());
            }
            let mut first = 0usize;
            while first < plain.len() && plain[first].is_empty() {
                first += 1;
            }
            let mut end = plain.len();
            while end > first && plain[end - 1].is_empty() {
                end -= 1;
            }
            (plain[first..end].to_vec(), styled[first..end].to_vec(), width)
        }
    }

    pub(super) fn draw_text(canvas: &mut Canvas, text: &str, x: usize, y: usize, cls: Cls) {
        let mut cur = x;
        for (cluster, cw) in measured(text) {
            if cw == 0 {
                continue;
            }
            canvas.set(cur, y, cluster, cls);
            for k in 1..cw {
                canvas.set(cur + k, y, CONT, cls);
            }
            cur += cw;
        }
    }

    pub(super) fn draw_text_over_edges(canvas: &mut Canvas, text: &str, x: usize, y: usize, cls: Cls) {
        let mut cur = x;
        for (cluster, cw) in measured(text) {
            if cw == 0 {
                continue;
            }
            for k in 0..cw {
                if cur + k < canvas.w && y < canvas.h {
                    let idx = canvas.idx(cur + k, y);
                    canvas.mask[idx] = 0;
                }
                canvas.set(cur + k, y, if k == 0 { cluster } else { CONT }, cls);
            }
            cur += cw;
        }
    }

    pub(super) fn mask_char(mask: u8) -> &'static str {
        const UD: u8 = U | D;
        const LR: u8 = L | R;
        const DR: u8 = D | R;
        const DL: u8 = D | L;
        const UR: u8 = U | R;
        const UL: u8 = U | L;
        const UDR: u8 = U | D | R;
        const UDL: u8 = U | D | L;
        const DLR: u8 = D | L | R;
        const ULR: u8 = U | L | R;
        match mask {
            0 => " ",
            U | D | UD => "│",
            L | R | LR => "─",
            DR => "┌",
            DL => "┐",
            UR => "└",
            UL => "┘",
            UDR => "├",
            UDL => "┤",
            DLR => "┬",
            ULR => "┴",
            _ => "┼",
        }
    }

    pub(super) fn dotted_char(c: &str) -> &str {
        match c {
            "─" => "╌",
            "│" => "╎",
            _ => c,
        }
    }

    pub(super) fn thick_char(c: &str) -> &str {
        match c {
            "─" => "━",
            "│" => "┃",
            "┌" => "┏",
            "┐" => "┓",
            "└" => "┗",
            "┘" => "┛",
            "├" => "┣",
            "┤" => "┫",
            "┬" => "┳",
            "┴" => "┻",
            "┼" => "╋",
            _ => c,
        }
    }

    pub(super) fn flip_glyph_v(c: &str) -> &str {
        match c {
            "┌" => "└",
            "└" => "┌",
            "┐" => "┘",
            "┘" => "┐",
            "┏" => "┗",
            "┗" => "┏",
            "┓" => "┛",
            "┛" => "┓",
            "╭" => "╰",
            "╰" => "╭",
            "╮" => "╯",
            "╯" => "╮",
            "┬" => "┴",
            "┴" => "┬",
            "┳" => "┻",
            "┻" => "┳",
            "▼" => "▲",
            "▲" => "▼",
            "▽" => "△",
            "△" => "▽",
            _ => c,
        }
    }

    pub(super) fn flip_glyph_h(c: &str) -> &str {
        match c {
            "┌" => "┐",
            "┐" => "┌",
            "└" => "┘",
            "┘" => "└",
            "┏" => "┓",
            "┓" => "┏",
            "┗" => "┛",
            "┛" => "┗",
            "╭" => "╮",
            "╮" => "╭",
            "╰" => "╯",
            "╯" => "╰",
            "├" => "┤",
            "┤" => "├",
            "┣" => "┫",
            "┫" => "┣",
            "▶" => "◄",
            "◄" => "▶",
            "▷" => "◁",
            "◁" => "▷",
            _ => c,
        }
    }
}

mod source_box {
    use super::labels::{src_lines, strip_controls};
    use super::width::{measured, string_width};
    use super::{Cls, MermaidArt, Span};

    pub(super) fn source_box(src: &str, max_width: Option<usize>) -> MermaidArt {
        let src = strip_controls(src);
        let header = src
            .split(pi_js::str16::is_js_whitespace)
            .find(|w| !w.is_empty())
            .unwrap_or("diagram");
        let title = format!(" mermaid: {header} ");
        let limit = max_width.map(|w| 8.max(w.saturating_sub(4)));

        let mut body = Vec::new();
        let mut started = false;
        for l in src_lines(&src) {
            let trimmed = l.trim_end_matches(pi_js::str16::is_js_whitespace);
            if !started && trimmed.is_empty() {
                continue;
            }
            started = true;
            body.extend(chunk_line(trimmed, limit));
        }

        let content_w = body
            .iter()
            .map(|l| string_width(l))
            .fold(string_width(&title), usize::max);
        let inner = content_w + 2;

        let mut plain = Vec::new();
        let mut styled = Vec::new();

        let rule = "─".repeat(inner.saturating_sub(string_width(&title)));
        plain.push(format!("╭{title}{rule}╮"));
        styled.push(vec![
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
        ]);

        for line in body {
            let pad = " ".repeat(content_w.saturating_sub(string_width(&line)));
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

    fn chunk_line(line: &str, limit: Option<usize>) -> Vec<String> {
        let Some(limit) = limit else {
            return vec![line.to_owned()];
        };
        if string_width(line) <= limit {
            return vec![line.to_owned()];
        }
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut cur_w = 0usize;
        for (c, cw) in measured(line) {
            if cur_w + cw > limit && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
                cur_w = 0;
            }
            cur.push_str(c);
            cur_w += cw;
        }
        if !cur.is_empty() {
            out.push(cur);
        }
        out
    }
}

mod graph {
    use indexmap::IndexMap;

    use super::labels::ascii_upper;

    pub(super) const MAX_NODES: usize = 128;
    pub(super) const MAX_EDGES: usize = 512;
    pub(super) const MAX_GROUPS: usize = 24;
    pub(super) const MAX_GROUP_DEPTH: usize = 6;
    pub(super) const MAX_MEMBERS: usize = 8;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum Shape {
        Rect,
        Round,
        Diamond,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum Head {
        None,
        Arrow,
        Circle,
        Cross,
        Triangle,
        DiamondFill,
        DiamondOpen,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum LineKind {
        Solid,
        Dotted,
        Thick,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum Dir {
        Down,
        Up,
        Right,
        Left,
    }

    #[derive(Clone, Debug)]
    pub(super) struct Node {
        pub(super) label: String,
        pub(super) shape: Shape,
    }

    #[derive(Clone, Debug)]
    pub(super) struct Edge {
        pub(super) from: usize,
        pub(super) to: usize,
        pub(super) label: Option<String>,
        pub(super) head_to: Head,
        pub(super) head_from: Head,
        pub(super) line: LineKind,
    }

    #[derive(Clone, Debug)]
    pub(super) struct Group {
        pub(super) id: String,
        pub(super) label: String,
        pub(super) parent: Option<usize>,
    }

    #[derive(Clone, Debug, Default)]
    pub(super) struct ClassInfo {
        pub(super) annotation: Option<String>,
        pub(super) attrs: Vec<String>,
        pub(super) methods: Vec<String>,
    }

    pub(super) fn parse_dir(token: &str) -> Dir {
        match ascii_upper(token).as_str() {
            "LR" => Dir::Right,
            "RL" => Dir::Left,
            "BT" => Dir::Up,
            _ => Dir::Down,
        }
    }

    pub(super) struct Graph {
        pub(super) nodes: Vec<Node>,
        pub(super) edges: Vec<Edge>,
        pub(super) index: IndexMap<String, usize>,
        pub(super) groups: Vec<Group>,
        pub(super) node_group: Vec<Option<usize>>,
        pub(super) cur_group: Option<usize>,
        pub(super) over_cap: bool,
        pub(super) warnings: Vec<String>,
        pub(super) dir: Dir,
    }

    impl Graph {
        pub(super) fn new(dir: Dir) -> Self {
            Self {
                nodes: Vec::new(),
                edges: Vec::new(),
                index: IndexMap::new(),
                groups: Vec::new(),
                node_group: Vec::new(),
                cur_group: None,
                over_cap: false,
                warnings: Vec::new(),
                dir,
            }
        }

        pub(super) fn node_index(&mut self, id: &str, label: Option<String>, shape: Shape) -> Option<usize> {
            if let Some(&existing) = self.index.get(id) {
                if let Some(lbl) = label {
                    self.nodes[existing].label = lbl;
                    self.nodes[existing].shape = shape;
                }
                return Some(existing);
            }
            if self.nodes.len() >= MAX_NODES {
                self.over_cap = true;
                return None;
            }
            let idx = self.nodes.len();
            self.index.insert(id.to_owned(), idx);
            self.nodes.push(Node {
                label: label.unwrap_or_else(|| id.to_owned()),
                shape,
            });
            self.node_group.push(self.cur_group);
            Some(idx)
        }

        pub(super) fn node_label(&mut self, id: &str, label: String) -> Option<usize> {
            if let Some(&existing) = self.index.get(id) {
                self.nodes[existing].label = label;
                return Some(existing);
            }
            self.node_index(id, Some(label), Shape::Round)
        }

        pub(super) fn push_edge(&mut self, edge: Edge) -> bool {
            if self.edges.len() >= MAX_EDGES {
                self.over_cap = true;
                return false;
            }
            self.edges.push(edge);
            true
        }
    }
}

mod parse {
    use indexmap::IndexMap;

    use super::DiagramKind;
    use super::graph::{
        ClassInfo, Dir, Edge, Graph, Group, Head, LineKind, MAX_EDGES, MAX_GROUP_DEPTH, MAX_GROUPS, MAX_MEMBERS,
        MAX_NODES, Shape, parse_dir,
    };
    use super::labels::{ascii_lower, clean_label, decode_html_entities, is_id_char, src_lines};

    fn flush_statement(cur: &mut String, out: &mut Vec<String>) {
        let trimmed = pi_js::str16::trim(cur);
        if !trimmed.is_empty() {
            out.push(trimmed.to_owned());
        }
        cur.clear();
    }

    fn split_statements(line: &str, out: &mut Vec<String>) {
        let chars: Vec<char> = line.chars().collect();
        let mut cur = String::new();
        let mut in_quotes = false;
        let mut i = 0usize;
        while i < chars.len() {
            let c = chars[i];
            if in_quotes {
                if c == '"' {
                    in_quotes = false;
                }
                cur.push(c);
            } else if c == '"' {
                in_quotes = true;
                cur.push(c);
            } else if c == '%' && chars.get(i + 1) == Some(&'%') {
                break;
            } else if c == ';' {
                flush_statement(&mut cur, out);
            } else {
                cur.push(c);
            }
            i += 1;
        }
        flush_statement(&mut cur, out);
    }

    fn statements_of(src: &str) -> Vec<String> {
        let mut out = Vec::new();
        for line in src_lines(src) {
            split_statements(line, &mut out);
        }
        out
    }

    fn first_word(s: &str) -> &str {
        s.split(pi_js::str16::is_js_whitespace)
            .find(|w| !w.is_empty())
            .unwrap_or("")
    }

    fn words(s: &str) -> Vec<&str> {
        s.split(pi_js::str16::is_js_whitespace)
            .filter(|w| !w.is_empty())
            .collect()
    }

    fn split_once<'a>(s: &'a str, sep: &str) -> Option<(&'a str, &'a str)> {
        s.split_once(sep)
    }

    fn non_empty(s: String) -> Option<String> {
        if s.is_empty() { None } else { Some(s) }
    }

    fn header_kind(statements: &[String]) -> Option<String> {
        let header = statements.first()?;
        let kind = first_word(header);
        if kind.is_empty() { None } else { Some(ascii_lower(kind)) }
    }

    pub(super) fn diagram_kind(src: &str) -> Option<DiagramKind> {
        let stmts = statements_of(src);
        let kind = header_kind(&stmts)?;
        if kind == "graph" || kind == "flowchart" {
            return Some(DiagramKind::Flowchart);
        }
        if kind.starts_with("statediagram") {
            return Some(DiagramKind::State);
        }
        if kind.starts_with("classdiagram") {
            return Some(DiagramKind::Class);
        }
        if kind == "erdiagram" {
            return Some(DiagramKind::Er);
        }
        if kind == "sequencediagram" {
            return Some(DiagramKind::Sequence);
        }
        None
    }

    // ----------------------------------------------------------------- flowchart

    pub(super) fn parse_graph(src: &str) -> Option<Graph> {
        let statements = statements_of(src);
        let kind = header_kind(&statements)?;
        if kind != "graph" && kind != "flowchart" {
            return None;
        }

        let dir_tok = words(&statements[0]).get(1).copied().unwrap_or("TB");
        let mut graph = Graph::new(parse_dir(dir_tok));
        let mut stack: Vec<usize> = Vec::new();

        for st in &statements[1..] {
            match ascii_lower(first_word(st)).as_str() {
                "subgraph" => {
                    if graph.groups.len() >= MAX_GROUPS || stack.len() >= MAX_GROUP_DEPTH {
                        return None;
                    }
                    let rest = pi_js::str16::trim(&st["subgraph".len()..]);
                    let (id, label) = parse_subgraph_decl(rest);
                    graph.groups.push(Group {
                        id,
                        label,
                        parent: stack.last().copied(),
                    });
                    stack.push(graph.groups.len() - 1);
                    graph.cur_group = stack.last().copied();
                    continue;
                }
                "end" => {
                    stack.pop();
                    graph.cur_group = stack.last().copied();
                    continue;
                }
                "classdef" | "class" | "style" | "linkstyle" | "click" | "direction" => {
                    continue;
                }
                _ => {}
            }
            parse_statement(st, &mut graph);
            if graph.over_cap {
                return None;
            }
        }

        if graph.nodes.is_empty() { None } else { Some(graph) }
    }

    fn parse_subgraph_decl(rest: &str) -> (String, String) {
        if let Some(after_open) = rest.strip_prefix('"')
            && let Some(close) = after_open.find('"')
        {
            let label = &after_open[..close];
            return (label.to_owned(), decode_html_entities(label));
        }
        if let Some(open) = rest.find('[') {
            let id = pi_js::str16::trim(&rest[..open]);
            let label = clean_label(pi_js::str16::trim(rest[open + 1..].trim_end_matches(']')));
            if !id.is_empty() && !label.is_empty() {
                return (id.to_owned(), label);
            }
        }
        (rest.to_owned(), rest.to_owned())
    }

    fn parse_statement(st: &str, graph: &mut Graph) {
        let chars: Vec<char> = st.chars().collect();
        let mut i = 0usize;

        let Some((mut prev, next_i)) = parse_node_group(&chars, i, graph) else {
            graph
                .warnings
                .push(format!("dropped, does not start with a node: \"{st}\""));
            return;
        };
        i = next_i;

        loop {
            i = skip_spaces(&chars, i);
            if i >= chars.len() {
                break;
            }
            let Some(link) = parse_link(&chars, i) else {
                let tail: String = chars[i..].iter().collect();
                graph.warnings.push(format!("dropped, expected a link: \"{tail}\""));
                break;
            };
            i = skip_spaces(&chars, link.next);
            let Some((target, target_next)) = parse_node_group(&chars, i, graph) else {
                graph.warnings.push(format!("dropped, link has no target: \"{st}\""));
                break;
            };
            i = target_next;
            for &f in &prev {
                for &t in &target {
                    let reversed = link.left == Head::Arrow && link.right != Head::Arrow;
                    let pushed = graph.push_edge(Edge {
                        from: if reversed { t } else { f },
                        to: if reversed { f } else { t },
                        label: link.label.clone(),
                        head_to: if reversed { Head::Arrow } else { link.right },
                        head_from: if reversed { link.right } else { link.left },
                        line: link.line,
                    });
                    if !pushed {
                        return;
                    }
                }
            }
            prev = target;
        }
    }

    fn parse_node_group(chars: &[char], start: usize, graph: &mut Graph) -> Option<(Vec<usize>, usize)> {
        let (first_idx, mut i) = parse_node(chars, start, graph)?;
        let mut group = vec![first_idx];
        loop {
            let j = skip_spaces(chars, i);
            if chars.get(j) != Some(&'&') {
                break;
            }
            let (next_idx, next_i) = parse_node(chars, j + 1, graph)?;
            group.push(next_idx);
            i = next_i;
        }
        Some((group, i))
    }

    fn skip_spaces(chars: &[char], mut i: usize) -> usize {
        while i < chars.len() && (chars[i] == ' ' || chars[i] == '\t') {
            i += 1;
        }
        i
    }

    fn parse_node(chars: &[char], start: usize, graph: &mut Graph) -> Option<(usize, usize)> {
        let mut i = skip_spaces(chars, start);
        let id_start = i;
        while i < chars.len() && is_id_char(chars[i]) {
            i += 1;
        }
        if i == id_start {
            return None;
        }
        let id: String = chars[id_start..i].iter().collect();

        let shaped = read_shape_at(chars, i);
        if let Some(unclosed) = shaped.unclosed {
            graph
                .warnings
                .push(format!("node \"{id}\": label is missing its closing `{unclosed}`"));
        }
        let index = graph.node_index(&id, shaped.label, shaped.shape)?;

        let mut next = shaped.after;
        if chars.get(next) == Some(&':') && chars.get(next + 1) == Some(&':') && chars.get(next + 2) == Some(&':') {
            let mut k = next + 3;
            while k < chars.len() && (is_id_char(chars[k]) || chars[k] == '-') {
                k += 1;
            }
            while k > next + 3 && chars[k - 1] == '-' {
                k -= 1;
            }
            if k > next + 3 {
                next = k;
            }
        }
        Some((index, next))
    }

    struct Shaped {
        shape: Shape,
        label: Option<String>,
        after: usize,
        unclosed: Option<&'static str>,
    }

    fn read_shape_at(chars: &[char], i: usize) -> Shaped {
        let c = chars.get(i).copied();
        let n = chars.get(i + 1).copied();
        match c {
            Some('[') => match n {
                Some('[') => read_shape(chars, i + 2, "]]", Shape::Rect),
                Some('(') => read_shape(chars, i + 2, ")]", Shape::Round),
                _ => read_shape(chars, i + 1, "]", Shape::Rect),
            },
            Some('(') => match n {
                Some('(') => read_shape(chars, i + 2, "))", Shape::Round),
                Some('[') => read_shape(chars, i + 2, "])", Shape::Round),
                _ => read_shape(chars, i + 1, ")", Shape::Round),
            },
            Some('{') => match n {
                Some('{') => read_shape(chars, i + 2, "}}", Shape::Diamond),
                _ => read_shape(chars, i + 1, "}", Shape::Diamond),
            },
            Some('>') => read_shape(chars, i + 1, "]", Shape::Rect),
            _ => Shaped {
                shape: Shape::Rect,
                label: None,
                after: i,
                unclosed: None,
            },
        }
    }

    fn matches_closer(chars: &[char], i: usize, closer: &str) -> bool {
        let mut iter = closer.chars();
        for offset in 0..closer.len() {
            match (chars.get(i + offset), iter.next()) {
                (Some(&a), Some(b)) if a == b => {}
                _ => return false,
            }
        }
        true
    }

    fn read_shape(chars: &[char], start: usize, closer: &'static str, shape: Shape) -> Shaped {
        let mut j = start;
        while j < chars.len() && (chars[j] == ' ' || chars[j] == '\t') {
            j += 1;
        }
        let quoted = chars.get(j) == Some(&'"');

        let mut i = start;
        let mut text = String::new();
        let mut in_quotes = false;
        while i < chars.len() {
            let c = chars[i];
            if quoted && c == '"' {
                in_quotes = !in_quotes;
                text.push(c);
                i += 1;
                continue;
            }
            if !in_quotes && matches_closer(chars, i, closer) {
                return Shaped {
                    shape,
                    label: Some(clean_label(&text)),
                    after: i + closer.len(),
                    unclosed: None,
                };
            }
            text.push(c);
            i += 1;
        }
        Shaped {
            shape,
            label: Some(clean_label(&text)),
            after: chars.len(),
            unclosed: Some(closer),
        }
    }

    fn is_link_char(c: char) -> bool {
        matches!(c, '-' | '.' | '=' | '<' | '>')
    }

    struct Link {
        left: Head,
        right: Head,
        line: LineKind,
        label: Option<String>,
        next: usize,
    }

    fn parse_link(chars: &[char], start: usize) -> Option<Link> {
        let mut i = skip_spaces(chars, start);
        let mut left = Head::None;
        if matches!(chars.get(i), Some('o' | 'x')) && matches!(chars.get(i + 1), Some('-' | '.' | '=')) {
            left = if chars[i] == 'o' { Head::Circle } else { Head::Cross };
            i += 1;
        }

        let op_start = i;
        while i < chars.len() && is_link_char(chars[i]) {
            i += 1;
        }
        if i == op_start {
            return None;
        }
        let op1: String = chars[op_start..i].iter().collect();
        if left == Head::None && op1.starts_with('<') {
            left = Head::Arrow;
        }

        let mut line = line_kind(&op1);
        let mut right = if op1.contains('>') { Head::Arrow } else { Head::None };
        if right == Head::None
            && let Some((head, next)) = trailing_head(chars, i)
        {
            right = head;
            i = next;
        }

        if chars.get(i) == Some(&'|') {
            i += 1;
            let l_start = i;
            while i < chars.len() && chars[i] != '|' {
                i += 1;
            }
            let raw: String = chars[l_start..i].iter().collect();
            let label = clean_label(&raw);
            if chars.get(i) == Some(&'|') {
                i += 1;
            }
            return Some(Link {
                left,
                right,
                line,
                label: non_empty(label),
                next: i,
            });
        }

        if right == Head::None {
            let text_start = skip_spaces(chars, i);
            let mut j = text_start;
            while j < chars.len() && !is_link_char(chars[j]) {
                j += 1;
            }
            if j < chars.len() && j > text_start && chars[j] != '<' {
                let text: String = chars[text_start..j].iter().collect();
                let op2_start = j;
                while j < chars.len() && is_link_char(chars[j]) {
                    j += 1;
                }
                let op2: String = chars[op2_start..j].iter().collect();
                if op2.contains('>') {
                    right = Head::Arrow;
                } else if let Some((head, next)) = trailing_head(chars, j) {
                    right = head;
                    j = next;
                }
                if line == LineKind::Solid {
                    line = line_kind(&op2);
                }
                return Some(Link {
                    left,
                    right,
                    line,
                    label: non_empty(clean_label(&text)),
                    next: j,
                });
            }
        }

        Some(Link {
            left,
            right,
            line,
            label: None,
            next: i,
        })
    }

    fn line_kind(op: &str) -> LineKind {
        if op.contains('=') {
            LineKind::Thick
        } else if op.contains('.') {
            LineKind::Dotted
        } else {
            LineKind::Solid
        }
    }

    fn trailing_head(chars: &[char], i: usize) -> Option<(Head, usize)> {
        let head = match chars.get(i).copied() {
            Some('o') => Head::Circle,
            Some('x') => Head::Cross,
            _ => return None,
        };
        let boundary = matches!(chars.get(i + 1).copied(), None | Some(' ' | '\t' | '|' | '&' | ';'));
        if boundary { Some((head, i + 1)) } else { None }
    }

    // --------------------------------------------------------------------- state

    pub(super) fn parse_state(src: &str) -> Option<Graph> {
        let statements = statements_of(src);
        let kind = header_kind(&statements)?;
        if !kind.starts_with("statediagram") {
            return None;
        }

        let mut graph = Graph::new(Dir::Down);
        let mut in_note = false;

        for raw_st in &statements[1..] {
            if in_note {
                if ascii_lower(raw_st) == "end note" {
                    in_note = false;
                }
                continue;
            }
            let st = drop_style_tags(raw_st);
            let first = ascii_lower(first_word(&st));
            if first == "direction" {
                graph.dir = parse_dir(words(&st).get(1).copied().unwrap_or(""));
            } else if first == "note" {
                if !st.contains(':') {
                    in_note = true;
                }
            } else if first == "state" {
                parse_state_decl(&st, &mut graph)?;
            } else if matches!(first.as_str(), "classdef" | "class" | "hide" | "scale" | "}" | "--") {
                // Styling and composite-state punctuation carry no layout meaning.
            } else if st.contains("-->") {
                parse_transition(&st, &mut graph)?;
            } else {
                parse_state_desc(&st, &mut graph)?;
            }
            if graph.over_cap {
                return None;
            }
        }

        if graph.nodes.is_empty() { None } else { Some(graph) }
    }

    fn parse_state_decl(st: &str, graph: &mut Graph) -> Option<()> {
        let rest = pi_js::str16::trim(
            pi_js::str16::trim(&st["state".len()..])
                .strip_suffix('{')
                .unwrap_or(pi_js::str16::trim(&st["state".len()..])),
        );
        if rest.is_empty() {
            return Some(());
        }

        if let Some(after_q) = rest.strip_prefix('"') {
            let close = after_q.find('"')?;
            let label = &after_q[..close];
            let after = pi_js::str16::trim(&after_q[close + 1..]);
            let id = if let Some(rest_id) = after.strip_prefix("as") {
                pi_js::str16::trim(rest_id)
            } else {
                label
            };
            graph.node_label(id, decode_html_entities(label))?;
            return Some(());
        }

        let mut shape = Shape::Round;
        let mut id = rest;
        let mut stereotyped = false;
        if let Some(pos) = rest.find("<<") {
            let stereo = pi_js::str16::trim(rest[pos + 2..].strip_suffix(">>").unwrap_or(&rest[pos + 2..]));
            if stereo == "choice" {
                shape = Shape::Diamond;
            }
            id = pi_js::str16::trim(&rest[..pos]);
            stereotyped = true;
        }
        if id.is_empty() || id.chars().any(pi_js::str16::is_js_whitespace) {
            return None;
        }
        graph.node_index(id, if stereotyped { Some(id.to_owned()) } else { None }, shape)?;
        Some(())
    }

    fn parse_transition(st: &str, graph: &mut Graph) -> Option<()> {
        let mut rest = st;
        let mut prev: Option<usize> = None;

        while let Some((lhs, rhs)) = split_once(rest, "-->") {
            let from_id = pi_js::str16::trim(pi_js::str16::trim_end(lhs).trim_end_matches('-'));
            let from = if let Some(p) = prev {
                if !from_id.is_empty() {
                    return None;
                }
                p
            } else {
                if from_id.is_empty() {
                    return None;
                }
                state_endpoint(graph, from_id, true)?
            };

            let (to_part_raw, tail) = match rhs.find("-->") {
                Some(idx) => (&rhs[..idx], &rhs[idx..]),
                None => (rhs, ""),
            };

            let (to_part, label) = match split_once(to_part_raw, ":") {
                Some((tp, lbl)) => (tp, non_empty(decode_html_entities(pi_js::str16::trim(lbl)))),
                None => (to_part_raw, None),
            };

            let to_id = pi_js::str16::trim(
                pi_js::str16::trim_end(pi_js::str16::trim_start(to_part).trim_start_matches('>')).trim_end_matches('-'),
            );
            if to_id.is_empty() {
                return None;
            }
            let to = state_endpoint(graph, to_id, false)?;

            if !graph.push_edge(Edge {
                from,
                to,
                label,
                head_to: Head::Arrow,
                head_from: Head::None,
                line: LineKind::Solid,
            }) {
                return Some(());
            }
            prev = Some(to);
            rest = tail;
        }
        Some(())
    }

    fn drop_style_tags(st: &str) -> String {
        let chars: Vec<char> = st.chars().collect();
        let mut out = String::new();
        let mut i = 0usize;
        while i < chars.len() {
            if chars[i] == ':' && chars.get(i + 1) == Some(&':') && chars.get(i + 2) == Some(&':') {
                let mut k = i + 3;
                while k < chars.len() && (is_id_char(chars[k]) || chars[k] == '-') {
                    k += 1;
                }
                while k > i + 3 && chars[k - 1] == '-' {
                    k -= 1;
                }
                if k > i + 3 {
                    i = k;
                    continue;
                }
            }
            out.push(chars[i]);
            i += 1;
        }
        out
    }

    fn state_endpoint(graph: &mut Graph, id: &str, is_source: bool) -> Option<usize> {
        if id == "[*]" {
            graph.node_index(
                if is_source { "[*]start" } else { "[*]end" },
                Some("●".to_owned()),
                Shape::Round,
            )
        } else {
            graph.node_index(id, None, Shape::Round)
        }
    }

    fn parse_state_desc(st: &str, graph: &mut Graph) -> Option<()> {
        if let Some((lhs, rhs)) = split_once(st, ":") {
            let id = pi_js::str16::trim(lhs);
            let desc = pi_js::str16::trim(rhs);
            if id.is_empty() || id.chars().any(pi_js::str16::is_js_whitespace) || desc.is_empty() {
                return None;
            }
            graph.node_label(id, decode_html_entities(desc))?;
            return Some(());
        }
        if st.chars().any(pi_js::str16::is_js_whitespace) {
            return None;
        }
        graph.node_index(st, None, Shape::Round)?;
        Some(())
    }

    // --------------------------------------------------------------------- class

    const CLASS_OPS: &[(&str, Head, Head, LineKind)] = &[
        ("<|--", Head::Triangle, Head::None, LineKind::Solid),
        ("--|>", Head::None, Head::Triangle, LineKind::Solid),
        ("<|..", Head::Triangle, Head::None, LineKind::Dotted),
        ("..|>", Head::None, Head::Triangle, LineKind::Dotted),
        ("*--", Head::DiamondFill, Head::None, LineKind::Solid),
        ("--*", Head::None, Head::DiamondFill, LineKind::Solid),
        ("o--", Head::DiamondOpen, Head::None, LineKind::Solid),
        ("--o", Head::None, Head::DiamondOpen, LineKind::Solid),
        ("<--", Head::Arrow, Head::None, LineKind::Solid),
        ("-->", Head::None, Head::Arrow, LineKind::Solid),
        ("<..", Head::Arrow, Head::None, LineKind::Dotted),
        ("..>", Head::None, Head::Arrow, LineKind::Dotted),
        ("--", Head::None, Head::None, LineKind::Solid),
        ("..", Head::None, Head::None, LineKind::Dotted),
    ];

    const MAX_CLASS_OP: usize = 4;

    pub(super) struct ParsedClass {
        pub(super) graph: Graph,
        pub(super) infos: Vec<ClassInfo>,
    }

    fn sync_infos(graph: &Graph, infos: &mut Vec<ClassInfo>) {
        while infos.len() < graph.nodes.len() {
            infos.push(ClassInfo::default());
        }
    }

    fn declare_class(graph: &mut Graph, infos: &mut Vec<ClassInfo>, name: &str) -> Option<usize> {
        let idx = graph.node_index(name, None, Shape::Rect);
        sync_infos(graph, infos);
        idx
    }

    pub(super) fn parse_class(src: &str) -> Option<ParsedClass> {
        let statements = statements_of(src);
        let kind = header_kind(&statements)?;
        if !kind.starts_with("classdiagram") {
            return None;
        }

        let mut graph = Graph::new(Dir::Down);
        let mut infos: Vec<ClassInfo> = Vec::new();
        let mut cur_class: Option<usize> = None;

        for raw_st in &statements[1..] {
            if let Some(cc) = cur_class {
                if raw_st == "}" {
                    cur_class = None;
                } else {
                    push_member(&mut infos[cc], raw_st);
                }
                continue;
            }
            let st = drop_style_tags(raw_st);

            let first = ascii_lower(first_word(&st));
            if first == "direction" {
                graph.dir = parse_dir(words(&st).get(1).copied().unwrap_or(""));
                continue;
            }
            if matches!(
                first.as_str(),
                "note" | "callback" | "click" | "link" | "style" | "cssclass" | "classdef" | "namespace" | "}"
            ) {
                continue;
            }
            if first == "class" {
                let rest = pi_js::str16::trim(&st["class".len()..]);
                let open = rest.ends_with('{');
                let name = if open {
                    pi_js::str16::trim(&rest[..rest.len() - 1])
                } else {
                    rest
                };
                if name.is_empty() || name.chars().any(pi_js::str16::is_js_whitespace) {
                    return None;
                }
                let idx = declare_class(&mut graph, &mut infos, name)?;
                if open {
                    cur_class = Some(idx);
                }
                continue;
            }

            if let Some(after_open) = st.strip_prefix("<<") {
                let (ann, rest_name) = split_once(after_open, ">>")?;
                let name = pi_js::str16::trim(rest_name);
                if name.is_empty() || name.chars().any(pi_js::str16::is_js_whitespace) {
                    return None;
                }
                let idx = declare_class(&mut graph, &mut infos, name)?;
                infos[idx].annotation = Some(pi_js::str16::trim(ann).to_owned());
                continue;
            }

            if let Some(rel) = parse_class_relation(&st) {
                let f = declare_class(&mut graph, &mut infos, &rel.from)?;
                let t = declare_class(&mut graph, &mut infos, &rel.to)?;
                if graph.edges.len() >= MAX_EDGES {
                    return None;
                }
                graph.edges.push(Edge {
                    from: f,
                    to: t,
                    label: rel.label,
                    head_to: rel.head_to,
                    head_from: rel.head_from,
                    line: rel.line,
                });
                continue;
            }

            if let Some((lhs, rhs)) = split_once(&st, ":") {
                let id = pi_js::str16::trim(lhs);
                let text = pi_js::str16::trim(rhs);
                if id.is_empty() || id.chars().any(pi_js::str16::is_js_whitespace) || text.is_empty() {
                    return None;
                }
                let idx = declare_class(&mut graph, &mut infos, id)?;
                push_member(&mut infos[idx], text);
                continue;
            }
            return None;
        }

        if graph.nodes.is_empty() {
            return None;
        }
        sync_infos(&graph, &mut infos);
        Some(ParsedClass { graph, infos })
    }

    pub(super) fn push_member(info: &mut ClassInfo, raw: &str) {
        if let Some(after_open) = raw.strip_prefix("<<") {
            if let Some((ann, _)) = split_once(after_open, ">>") {
                info.annotation = Some(pi_js::str16::trim(ann).to_owned());
            }
            return;
        }
        let member = decode_html_entities(&display_generics(pi_js::str16::trim(raw)));
        let list = if member.contains('(') {
            &mut info.methods
        } else {
            &mut info.attrs
        };
        if list.len() < MAX_MEMBERS {
            list.push(member);
        } else if list.len() == MAX_MEMBERS {
            list.push("…".to_owned());
        }
    }

    struct ClassRelation {
        from: String,
        to: String,
        head_from: Head,
        head_to: Head,
        line: LineKind,
        label: Option<String>,
    }

    fn parse_class_relation(st: &str) -> Option<ClassRelation> {
        let chars: Vec<char> = st.chars().collect();
        let mut found: Option<(usize, &str, Head, Head, LineKind)> = None;

        'outer: for pos in 0..chars.len() {
            let end = (pos + MAX_CLASS_OP).min(chars.len());
            let tail: String = chars[pos..end].iter().collect();
            for &(op, head_from, head_to, line) in CLASS_OPS {
                if !tail.starts_with(op) {
                    continue;
                }
                if op.starts_with('o') && pos > 0 && is_id_char(chars[pos - 1]) {
                    continue;
                }
                let op_len = op.chars().count();
                if op.ends_with('o') && chars.get(pos + op_len).is_some_and(|&c| is_id_char(c)) {
                    continue;
                }
                found = Some((pos, op, head_from, head_to, line));
                break 'outer;
            }
        }
        let (pos, op, head_from, head_to, line) = found?;
        let op_len = op.chars().count();

        let lhs_raw: String = chars[..pos].iter().collect();
        let rhs_raw: String = chars[pos + op_len..].iter().collect();

        let (lhs, card_from) = strip_cardinality_suffix(pi_js::str16::trim(&lhs_raw));
        let (rhs, card_to) = strip_cardinality_prefix(pi_js::str16::trim(&rhs_raw));

        let (to_raw, rel_label) = match split_once(rhs, ":") {
            Some((t, lbl)) => (t, non_empty(decode_html_entities(pi_js::str16::trim(lbl)))),
            None => (rhs, None),
        };
        let to_id = pi_js::str16::trim(to_raw);

        if lhs.is_empty()
            || to_id.is_empty()
            || lhs.chars().any(pi_js::str16::is_js_whitespace)
            || to_id.chars().any(pi_js::str16::is_js_whitespace)
        {
            return None;
        }

        let mut parts = Vec::new();
        if !card_from.is_empty() {
            parts.push(card_from);
        }
        if let Some(rl) = rel_label {
            parts.push(rl);
        }
        if !card_to.is_empty() {
            parts.push(card_to);
        }
        let label = non_empty(parts.join(" "));
        Some(ClassRelation {
            from: lhs.to_owned(),
            to: to_id.to_owned(),
            head_from,
            head_to,
            line,
            label,
        })
    }

    fn strip_cardinality_suffix(s: &str) -> (&str, String) {
        let t = pi_js::str16::trim_end(s);
        if let Some(rest) = t.strip_suffix('"')
            && let Some(q) = rest.rfind('"')
        {
            return (pi_js::str16::trim_end(&rest[..q]), rest[q + 1..].to_owned());
        }
        (t, String::new())
    }

    fn strip_cardinality_prefix(s: &str) -> (&str, String) {
        let t = pi_js::str16::trim_start(s);
        if let Some(rest) = t.strip_prefix('"')
            && let Some(q) = rest.find('"')
        {
            return (pi_js::str16::trim_start(&rest[q + 1..]), rest[..q].to_owned());
        }
        (t, String::new())
    }

    pub(super) fn display_generics(s: &str) -> String {
        let mut out = String::new();
        let mut open = false;
        for c in s.chars() {
            if c == '~' {
                out.push(if open { '>' } else { '<' });
                open = !open;
            } else {
                out.push(c);
            }
        }
        out
    }

    // ------------------------------------------------------------------------ ER

    pub(super) fn parse_er(src: &str) -> Option<ParsedClass> {
        let statements = statements_of(src);
        if header_kind(&statements).as_deref() != Some("erdiagram") {
            return None;
        }

        let mut graph = Graph::new(Dir::Down);
        let mut infos: Vec<ClassInfo> = Vec::new();
        let mut cur_entity: Option<usize> = None;

        for st in &statements[1..] {
            if let Some(ce) = cur_entity {
                if st == "}" {
                    cur_entity = None;
                } else {
                    push_er_attribute(&mut infos[ce], st);
                }
                continue;
            }

            if let Some((rel, label)) = split_er_relationship(st) {
                let tokens = words(rel);
                if tokens.len() != 3 {
                    return None;
                }
                let op = parse_er_op(tokens[1])?;
                let f = er_entity(&mut graph, &mut infos, tokens[0])?;
                let t = er_entity(&mut graph, &mut infos, tokens[2])?;
                if graph.edges.len() >= MAX_EDGES {
                    return None;
                }
                let rel_label = label.map(clean_label).unwrap_or_default();
                let mut parts = Vec::new();
                if !op.card_l.is_empty() {
                    parts.push(op.card_l);
                }
                if !rel_label.is_empty() {
                    parts.push(&rel_label);
                }
                if !op.card_r.is_empty() {
                    parts.push(op.card_r);
                }
                graph.edges.push(Edge {
                    from: f,
                    to: t,
                    label: non_empty(parts.join(" ")),
                    head_to: Head::None,
                    head_from: Head::None,
                    line: op.line,
                });
                continue;
            }

            let open = st.ends_with('{');
            let decl = if open {
                pi_js::str16::trim(&st[..st.len() - 1])
            } else {
                st.as_str()
            };
            if decl.is_empty() || words(decl).len() != 1 {
                return None;
            }
            let idx = er_entity(&mut graph, &mut infos, decl)?;
            if open {
                cur_entity = Some(idx);
            }
        }

        if graph.nodes.is_empty() {
            return None;
        }
        sync_infos(&graph, &mut infos);
        Some(ParsedClass { graph, infos })
    }

    fn er_entity(graph: &mut Graph, infos: &mut Vec<ClassInfo>, token: &str) -> Option<usize> {
        let idx = if let Some(open) = token.find('[') {
            let id = &token[..open];
            let label = clean_label(token[open + 1..].trim_end_matches(']'));
            if id.is_empty() || label.is_empty() {
                return None;
            }
            graph.node_label(id, label)?
        } else {
            graph.node_index(token, None, Shape::Rect)?
        };
        sync_infos(graph, infos);
        Some(idx)
    }

    fn split_er_relationship(st: &str) -> Option<(&str, Option<&str>)> {
        let (rel, label) = match split_once(st, ":") {
            Some((r, l)) => (r, Some(pi_js::str16::trim(l))),
            None => (st, None),
        };
        if words(rel).into_iter().any(|t| parse_er_op(t).is_some()) {
            Some((rel, label))
        } else {
            None
        }
    }

    struct ErOp {
        card_l: &'static str,
        card_r: &'static str,
        line: LineKind,
    }

    fn parse_er_op(tok: &str) -> Option<ErOp> {
        if tok.len() != 6 || !tok.is_ascii() {
            return None;
        }
        let line = match &tok[2..4] {
            "--" => LineKind::Solid,
            ".." => LineKind::Dotted,
            _ => return None,
        };
        let card_l = er_card(&tok[0..2])?;
        let card_r = er_card(&tok[4..6])?;
        Some(ErOp { card_l, card_r, line })
    }

    fn er_card(tok: &str) -> Option<&'static str> {
        match tok {
            "|o" | "o|" => Some("0..1"),
            "||" => Some("1"),
            "}o" | "o{" => Some("0..*"),
            "}|" | "|{" => Some("1..*"),
            _ => None,
        }
    }

    fn push_er_attribute(info: &mut ClassInfo, raw: &str) {
        let mut parts = Vec::new();
        for tok in words(raw) {
            if tok.starts_with('"') {
                break;
            }
            parts.push(tok);
        }
        if parts.is_empty() {
            return;
        }
        let line = decode_html_entities(&parts.join(" "));
        if info.attrs.len() < MAX_MEMBERS {
            info.attrs.push(line);
        } else if info.attrs.len() == MAX_MEMBERS {
            info.attrs.push("…".to_owned());
        }
    }

    // ------------------------------------------------------------------ sequence

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum SeqHead {
        Arrow,
        Cross,
    }

    const SEQ_OPS: &[(&str, bool, SeqHead)] = &[
        ("-->>", true, SeqHead::Arrow),
        ("->>", false, SeqHead::Arrow),
        ("--x", true, SeqHead::Cross),
        ("-x", false, SeqHead::Cross),
        ("--)", true, SeqHead::Arrow),
        ("-)", false, SeqHead::Arrow),
        ("-->", true, SeqHead::Arrow),
        ("->", false, SeqHead::Arrow),
    ];

    const MAX_SEQ_OP: usize = 4;

    #[derive(Clone, Copy, Debug)]
    pub(super) enum NoteAnchor {
        Over { from: usize, to: usize },
        Left { at: usize },
        Right { at: usize },
    }

    #[derive(Clone, Debug)]
    pub(super) enum SeqItem {
        Message {
            from: usize,
            to: usize,
            text: Option<String>,
            dashed: bool,
            head: SeqHead,
        },
        Note {
            anchor: NoteAnchor,
            text: String,
        },
        Divider {
            text: String,
        },
    }

    pub(super) struct Sequence {
        pub(super) labels: Vec<String>,
        pub(super) index: IndexMap<String, usize>,
        pub(super) items: Vec<SeqItem>,
    }

    impl Sequence {
        fn new() -> Self {
            Self {
                labels: Vec::new(),
                index: IndexMap::new(),
                items: Vec::new(),
            }
        }

        fn participant(&mut self, id: &str, label: Option<String>) -> Option<usize> {
            if let Some(&existing) = self.index.get(id) {
                if let Some(lbl) = label {
                    self.labels[existing] = lbl;
                }
                return Some(existing);
            }
            if self.labels.len() >= MAX_NODES {
                return None;
            }
            let idx = self.labels.len();
            self.index.insert(id.to_owned(), idx);
            self.labels.push(label.unwrap_or_else(|| id.to_owned()));
            Some(idx)
        }
    }

    pub(super) fn parse_sequence(src: &str) -> Option<Sequence> {
        let statements = statements_of(src);
        if header_kind(&statements).as_deref() != Some("sequencediagram") {
            return None;
        }

        let mut seq = Sequence::new();
        let mut autonumber = false;
        let mut msg_count = 0usize;
        let mut blocks: Vec<bool> = Vec::new();

        for st in &statements[1..] {
            let first = first_word(st);
            let lower = ascii_lower(first);

            if lower == "participant" || lower == "actor" {
                let rest = pi_js::str16::trim(&st[first.len()..]);
                if rest.is_empty() {
                    return None;
                }
                match split_once(rest, " as ") {
                    Some((id, lbl)) => {
                        seq.participant(pi_js::str16::trim(id), Some(clean_label(lbl)))?;
                    }
                    None => {
                        seq.participant(rest, None)?;
                    }
                }
                continue;
            }
            if lower == "autonumber" {
                autonumber = true;
                continue;
            }
            if matches!(
                lower.as_str(),
                "activate"
                    | "deactivate"
                    | "create"
                    | "destroy"
                    | "title"
                    | "acctitle"
                    | "accdescr"
                    | "links"
                    | "link"
                    | "properties"
            ) {
                continue;
            }
            if lower == "note" {
                let (anchor, text) = parse_note_anchor(pi_js::str16::trim(&st[first.len()..]), &mut seq)?;
                if seq.items.len() >= MAX_EDGES {
                    return None;
                }
                seq.items.push(SeqItem::Note { anchor, text });
                continue;
            }
            if matches!(
                lower.as_str(),
                "loop" | "alt" | "opt" | "par" | "critical" | "break" | "else" | "and" | "option"
            ) {
                if matches!(lower.as_str(), "else" | "and" | "option") {
                    if blocks.last().copied() != Some(true) {
                        continue;
                    }
                } else {
                    blocks.push(true);
                }
                if seq.items.len() >= MAX_EDGES {
                    return None;
                }
                seq.items.push(SeqItem::Divider {
                    text: decode_html_entities(st),
                });
                continue;
            }
            if lower == "rect" || lower == "box" {
                blocks.push(false);
                continue;
            }
            if lower == "end" {
                if blocks.pop() == Some(true) {
                    if seq.items.len() >= MAX_EDGES {
                        return None;
                    }
                    seq.items.push(SeqItem::Divider { text: "end".to_owned() });
                }
                continue;
            }

            let msg = parse_seq_message(st, &mut seq)?;
            let mut text = msg.text;
            if autonumber {
                msg_count += 1;
                text = Some(match text {
                    Some(t) => format!("{msg_count}. {t}"),
                    None => format!("{msg_count}."),
                });
            }
            if seq.items.len() >= MAX_EDGES {
                return None;
            }
            seq.items.push(SeqItem::Message {
                from: msg.from,
                to: msg.to,
                text,
                dashed: msg.dashed,
                head: msg.head,
            });
        }

        if seq.labels.is_empty() { None } else { Some(seq) }
    }

    fn parse_note_anchor(rest: &str, seq: &mut Sequence) -> Option<(NoteAnchor, String)> {
        let lower = ascii_lower(rest);
        enum Kind {
            Over,
            Left,
            Right,
        }
        let (kind, ids_and_text) = if lower.starts_with("over ") {
            (Kind::Over, &rest["over ".len()..])
        } else if lower.starts_with("left of ") {
            (Kind::Left, &rest["left of ".len()..])
        } else if lower.starts_with("right of ") {
            (Kind::Right, &rest["right of ".len()..])
        } else {
            return None;
        };

        let (ids_part, text_part) = split_once(ids_and_text, ":")?;
        let text = decode_html_entities(pi_js::str16::trim(text_part));
        let parts: Vec<&str> = ids_part
            .split(',')
            .map(pi_js::str16::trim)
            .filter(|s| !s.is_empty())
            .collect();
        if parts.is_empty() {
            return None;
        }
        let a = seq.participant(parts[0], None)?;
        match kind {
            Kind::Left => Some((NoteAnchor::Left { at: a }, text)),
            Kind::Right => Some((NoteAnchor::Right { at: a }, text)),
            Kind::Over => {
                let mut b = a;
                if let Some(p1) = parts.get(1) {
                    b = seq.participant(p1, None)?;
                }
                Some((
                    NoteAnchor::Over {
                        from: a.min(b),
                        to: a.max(b),
                    },
                    text,
                ))
            }
        }
    }

    struct SeqMsg {
        from: usize,
        to: usize,
        text: Option<String>,
        dashed: bool,
        head: SeqHead,
    }

    fn parse_seq_message(st: &str, seq: &mut Sequence) -> Option<SeqMsg> {
        let chars: Vec<char> = st.chars().collect();
        let mut found: Option<(usize, &str, bool, SeqHead)> = None;
        'outer: for pos in 0..chars.len() {
            let end = (pos + MAX_SEQ_OP).min(chars.len());
            let tail: String = chars[pos..end].iter().collect();
            for &(op, dashed, head) in SEQ_OPS {
                if tail.starts_with(op) {
                    found = Some((pos, op, dashed, head));
                    break 'outer;
                }
            }
        }
        let (pos, op, dashed, head) = found?;
        let from_id_str: String = chars[..pos].iter().collect();
        let from_id = pi_js::str16::trim(&from_id_str);
        if from_id.is_empty() {
            return None;
        }
        let op_len = op.chars().count();
        let after_op: String = chars[pos + op_len..].iter().collect();
        let rest = pi_js::str16::trim_start(&after_op).trim_start_matches(['+', '-']);

        let (to_raw, text) = match split_once(rest, ":") {
            Some((to_part, txt)) => (to_part, non_empty(decode_html_entities(pi_js::str16::trim(txt)))),
            None => (rest, None),
        };
        let to_id = pi_js::str16::trim(to_raw);
        if to_id.is_empty() {
            return None;
        }

        let from = seq.participant(from_id, None)?;
        let to = seq.participant(to_id, None)?;
        Some(SeqMsg {
            from,
            to,
            text,
            dashed,
            head,
        })
    }
}

mod layout {
    use indexmap::IndexMap;

    use super::Cls;
    use super::canvas::{CONT, Canvas, D, L, R, STY_DOT, STY_SOLID, STY_THICK, U, draw_text, draw_text_over_edges};
    use super::graph::{ClassInfo, Dir, Edge, Graph, Head, LineKind, Node, Shape};
    use super::labels::{MAX_LABEL, MAX_LINES, WRAP_WIDTH, fit_label, wrap_label};
    use super::parse::display_generics;
    use super::width::{measured, string_width};

    pub(super) const PAD: usize = 1;
    const GAP_X: usize = 3;
    const GAP_Y: usize = 2;
    pub(super) const MAX_CANVAS_CELLS: usize = 1 << 21;

    pub(super) fn half(n: usize) -> usize {
        n / 2
    }

    #[derive(Clone, Copy, Debug)]
    pub(super) struct Placed {
        pub(super) x: usize,
        pub(super) y: usize,
        pub(super) w: usize,
        pub(super) h: usize,
        pub(super) cx: usize,
        pub(super) cy: usize,
        pub(super) rank: usize,
    }

    struct NodeSizes {
        box_w: Vec<usize>,
        box_h: Vec<usize>,
        lay_w: Vec<usize>,
        lay_h: Vec<usize>,
        extra_h: Vec<usize>,
        self_label_w: Vec<usize>,
    }

    pub(super) enum NodeExtra {
        Plain,
        Frame { sub: Canvas },
        Compartments { sections: Vec<Vec<String>> },
    }

    struct RoutePlan {
        canvas_w: usize,
        canvas_h: usize,
        band_end: Vec<usize>,
        edge_bus: Vec<usize>,
        lane_base: usize,
        edge_lane: Vec<usize>,
    }

    // ------------------------------------------------------------------ ranking

    fn compute_ranks(graph: &Graph) -> Vec<usize> {
        let n = graph.nodes.len();
        let mut children = vec![Vec::new(); n];
        let mut indeg = vec![0usize; n];
        for e in &graph.edges {
            if e.from != e.to {
                children[e.from].push(e.to);
                indeg[e.to] += 1;
            }
        }

        let mut color = vec![0u8; n];
        let mut dag = vec![Vec::new(); n];
        let mut order = Vec::with_capacity(n);

        let roots: Vec<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
        for start in roots.into_iter().chain(0..n) {
            if color[start] == 0 {
                dfs_dag(start, &children, &mut color, &mut dag, &mut order);
            }
        }

        let mut rank = vec![0usize; n];
        for &u in order.iter().rev() {
            for &v in &dag[u] {
                rank[v] = rank[v].max(rank[u] + 1);
            }
        }
        rank
    }

    fn dfs_dag(
        start: usize,
        children: &[Vec<usize>],
        color: &mut [u8],
        dag: &mut [Vec<usize>],
        order: &mut Vec<usize>,
    ) {
        struct Frame {
            u: usize,
            i: usize,
        }
        let mut stack = vec![Frame { u: start, i: 0 }];
        color[start] = 1;
        while let Some(frame) = stack.last_mut() {
            let u = frame.u;
            if frame.i < children[u].len() {
                let v = children[u][frame.i];
                frame.i += 1;
                if color[v] == 1 {
                    continue;
                }
                dag[u].push(v);
                if color[v] == 0 {
                    color[v] = 1;
                    stack.push(Frame { u: v, i: 0 });
                }
            } else {
                color[u] = 2;
                order.push(u);
                stack.pop();
            }
        }
    }

    fn order_ranks(by_rank: &mut [Vec<usize>], edges: &[Edge], ranks: &[usize]) {
        let n = ranks.len();
        if by_rank.len() < 2 || n < 3 {
            return;
        }

        let mut parents = vec![Vec::new(); n];
        let mut children = vec![Vec::new(); n];
        for e in edges {
            if e.from != e.to && ranks[e.to] > ranks[e.from] {
                parents[e.to].push(e.from);
                children[e.from].push(e.to);
            }
        }

        let mut pos = vec![0usize; n];
        let reindex = |row: &[usize], pos: &mut [usize]| {
            for (i, &node) in row.iter().enumerate() {
                pos[node] = i;
            }
        };
        for row in by_rank.iter() {
            reindex(row, &mut pos);
        }

        let mut best: Vec<Vec<usize>> = by_rank.to_vec();
        let mut best_crossings = count_crossings(edges, ranks, &pos);
        if best_crossings == 0 {
            return;
        }

        for it in 0..8 {
            if it % 2 == 0 {
                for row in by_rank.iter_mut().skip(1) {
                    sort_by_barycenter(row, &parents, &pos);
                    reindex(row, &mut pos);
                }
            } else {
                let len = by_rank.len();
                for r in (0..len.saturating_sub(1)).rev() {
                    sort_by_barycenter(&mut by_rank[r], &children, &pos);
                    reindex(&by_rank[r], &mut pos);
                }
            }
            let crossings = count_crossings(edges, ranks, &pos);
            if crossings < best_crossings {
                best_crossings = crossings;
                best = by_rank.to_vec();
            }
            if best_crossings == 0 {
                break;
            }
        }

        for (dst, src) in by_rank.iter_mut().zip(best.into_iter()) {
            *dst = src;
        }
    }

    fn sort_by_barycenter(row: &mut [usize], neigh: &[Vec<usize>], pos: &[usize]) {
        let mut keyed: Vec<(f64, usize)> = row
            .iter()
            .map(|&v| {
                let key = if neigh[v].is_empty() {
                    pos[v] as f64
                } else {
                    neigh[v].iter().map(|&u| pos[u] as f64).sum::<f64>() / neigh[v].len() as f64
                };
                (key, v)
            })
            .collect();
        keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (dst, (_, v)) in row.iter_mut().zip(keyed.into_iter()) {
            *dst = v;
        }
    }

    fn count_crossings(edges: &[Edge], ranks: &[usize], pos: &[usize]) -> usize {
        let adjacent: Vec<(usize, usize, usize)> = edges
            .iter()
            .filter(|e| e.from != e.to && ranks[e.to] == ranks[e.from] + 1)
            .map(|e| (ranks[e.from], pos[e.from], pos[e.to]))
            .collect();
        let mut crossings = 0usize;
        for (i, &a) in adjacent.iter().enumerate() {
            for &b in adjacent.iter().skip(i + 1) {
                if a.0 == b.0 && ((a.1 < b.1 && a.2 > b.2) || (a.1 > b.1 && a.2 < b.2)) {
                    crossings += 1;
                }
            }
        }
        crossings
    }

    fn assign_positions(
        by_rank: &[Vec<usize>],
        size: &[usize],
        sep: usize,
        edges: &[Edge],
        ranks: &[usize],
    ) -> Vec<usize> {
        let n = size.len();
        let mut parents = vec![Vec::new(); n];
        let mut children = vec![Vec::new(); n];
        for e in edges {
            if e.from != e.to && ranks[e.to] > ranks[e.from] {
                parents[e.to].push(e.from);
                children[e.from].push(e.to);
            }
        }

        let mut pos = vec![0.0f64; n];
        for row in by_rank {
            let mut x = 0.0f64;
            for &v in row {
                let h = size[v] as f64 / 2.0;
                x += h;
                pos[v] = x;
                x += h + sep as f64;
            }
        }

        for it in 0..10 {
            if it % 2 == 0 {
                for row in by_rank {
                    relax_rank(row, &parents, &mut pos, size, sep as f64);
                }
            } else {
                for row in by_rank.iter().rev() {
                    relax_rank(row, &children, &mut pos, size, sep as f64);
                }
            }
        }

        let mut min_left = f64::INFINITY;
        for v in 0..n {
            min_left = min_left.min(pos[v] - size[v] as f64 / 2.0);
        }
        if !min_left.is_finite() {
            min_left = 0.0;
        }
        (0..n)
            .map(|v| {
                let rounded = pi_js::num::math_round(pos[v] - min_left);
                if rounded <= 0.0 { 0 } else { rounded as usize }
            })
            .collect()
    }

    fn relax_rank(nodes: &[usize], neigh: &[Vec<usize>], pos: &mut [f64], size: &[usize], sep: f64) {
        let n = nodes.len();
        if n == 0 {
            return;
        }

        let desired: Vec<f64> = nodes
            .iter()
            .map(|&v| {
                if neigh[v].is_empty() {
                    pos[v]
                } else {
                    neigh[v].iter().map(|&u| pos[u]).sum::<f64>() / neigh[v].len() as f64
                }
            })
            .collect();
        let half_of = |i: usize| -> f64 { size[nodes[i]] as f64 / 2.0 };

        let mut left = vec![0.0f64; n];
        for i in 0..n {
            left[i] = if i == 0 {
                desired[i]
            } else {
                desired[i].max(left[i - 1] + half_of(i - 1) + sep + half_of(i))
            };
        }
        let mut right = vec![0.0f64; n];
        for i in (0..n).rev() {
            right[i] = if i == n - 1 {
                desired[i]
            } else {
                desired[i].min(right[i + 1] - half_of(i + 1) - sep - half_of(i))
            };
        }
        for i in 0..n {
            pos[nodes[i]] = (left[i] + right[i]) / 2.0;
        }
        for i in 1..n {
            let min_p = pos[nodes[i - 1]] + half_of(i - 1) + sep + half_of(i);
            if pos[nodes[i]] < min_p {
                pos[nodes[i]] = min_p;
            }
        }
    }

    // ------------------------------------------------------------------- tracks

    type Span5 = (usize, usize, usize, usize, usize);

    fn assign_tracks(spans: &[Span5]) -> (Vec<(usize, usize)>, usize) {
        let mut sorted = spans.to_vec();
        sorted.sort();
        let mut tracks: Vec<Vec<(usize, usize, usize, usize)>> = Vec::new();
        let mut assigned = Vec::new();
        for (s, e, f, t, idx) in sorted {
            let slot = tracks
                .iter()
                .position(|members| {
                    members
                        .iter()
                        .all(|&(s2, e2, f2, t2)| e2 + 2 <= s || e + 2 <= s2 || f2 == f || t2 == t)
                })
                .unwrap_or_else(|| {
                    tracks.push(Vec::new());
                    tracks.len() - 1
                });
            tracks[slot].push((s, e, f, t));
            assigned.push((idx, slot));
        }
        (assigned, tracks.len())
    }

    fn bus_spans(graph: &Graph, ranks: &[usize], centers: &[usize], r: usize, exact: bool) -> Vec<Span5> {
        let mut out = Vec::new();
        for (i, e) in graph.edges.iter().enumerate() {
            let jogs = if exact {
                centers[e.from] != centers[e.to]
            } else {
                centers[e.from].abs_diff(centers[e.to]) > 1
            };
            if e.from != e.to && ranks[e.from] == r && ranks[e.to] == r + 1 && jogs {
                out.push((
                    centers[e.from].min(centers[e.to]),
                    centers[e.from].max(centers[e.to]),
                    e.from,
                    e.to,
                    i,
                ));
            }
        }
        out
    }

    fn lane_spans(graph: &Graph, ranks: &[usize], placed: &[Placed], vertical: bool) -> Vec<Span5> {
        let mut out = Vec::new();
        for (i, e) in graph.edges.iter().enumerate() {
            if e.from == e.to || ranks[e.to] == ranks[e.from] + 1 {
                continue;
            }
            let pf = placed[e.from];
            let pt = placed[e.to];
            let (a, b) = if vertical {
                (pf.cy.min(pt.cy), pf.cy.max(pt.cy))
            } else {
                (pf.cx.min(pt.cx), pf.cx.max(pt.cx))
            };
            out.push((a, b, e.from, e.to, i));
        }
        out
    }

    // ----------------------------------------------------------------- placement

    fn place_td(
        ranks: &[usize],
        max_rank: usize,
        by_rank: &[Vec<usize>],
        sizes: &NodeSizes,
        graph: &Graph,
        placed: &mut [Placed],
    ) -> RoutePlan {
        let centers = assign_positions(by_rank, &sizes.lay_w, GAP_X, &graph.edges, ranks);

        let mut edge_bus = vec![0usize; graph.edges.len()];
        let mut bus_tracks = vec![0usize; max_rank + 1];
        for r in 0..max_rank {
            let spans = bus_spans(graph, ranks, &centers, r, false);
            if spans.is_empty() {
                continue;
            }
            let (assigned, count) = assign_tracks(&spans);
            for (idx, slot) in assigned {
                edge_bus[idx] = slot;
            }
            bus_tracks[r] = count;
        }

        let rank_h: Vec<usize> = by_rank
            .iter()
            .map(|row| {
                row.iter()
                    .map(|&i| sizes.box_h[i] + sizes.extra_h[i])
                    .max()
                    .unwrap_or(3)
            })
            .collect();
        let mut rank_y = vec![0usize; max_rank + 1];
        for r in 1..=max_rank {
            rank_y[r] = rank_y[r - 1] + rank_h[r - 1] + GAP_Y.max(bus_tracks[r - 1] + 1);
        }
        let canvas_h = rank_y[max_rank] + rank_h[max_rank];
        let band_end: Vec<usize> = (0..=max_rank).map(|r| rank_y[r] + rank_h[r]).collect();

        let mut diagram_w = 1usize;
        for (r, row) in by_rank.iter().enumerate() {
            for &idx in row {
                let w = sizes.box_w[idx];
                let h = sizes.box_h[idx];
                let cx = centers[idx];
                let x = cx.saturating_sub(half(w));
                let y = rank_y[r] + half(rank_h[r].saturating_sub(h).saturating_sub(sizes.extra_h[idx]));
                placed[idx] = Placed {
                    x,
                    y,
                    w,
                    h,
                    cx,
                    cy: y + half(h),
                    rank: r,
                };
                diagram_w = diagram_w.max(x + w);
                if sizes.extra_h[idx] > 0 && sizes.self_label_w[idx] > 0 {
                    diagram_w = diagram_w.max(x + w + 2 + sizes.self_label_w[idx]);
                }
            }
        }

        let mut content_w = diagram_w;
        for e in &graph.edges {
            if e.from == e.to {
                continue;
            }
            let Some(ref label) = e.label else {
                continue;
            };
            let lw = string_width(label).min(MAX_LABEL);
            content_w = if ranks[e.to] == ranks[e.from] + 1 {
                content_w.max(placed[e.to].cx + 2 + lw)
            } else {
                content_w.max(diagram_w + lw + 1)
            };
        }

        let mut edge_lane = vec![0usize; graph.edges.len()];
        let lanes = lane_spans(graph, ranks, placed, true);
        let mut canvas_w = content_w;
        let mut lane_base = 0usize;
        if !lanes.is_empty() {
            let (assigned, count) = assign_tracks(&lanes);
            for (idx, slot) in assigned {
                edge_lane[idx] = slot;
            }
            canvas_w = content_w + 1 + count;
            lane_base = content_w + 1;
        }

        RoutePlan {
            canvas_w,
            canvas_h,
            band_end,
            edge_bus,
            lane_base,
            edge_lane,
        }
    }

    fn place_lr(
        ranks: &[usize],
        max_rank: usize,
        by_rank: &[Vec<usize>],
        sizes: &NodeSizes,
        graph: &Graph,
        placed: &mut [Placed],
    ) -> RoutePlan {
        let col_w: Vec<usize> = by_rank
            .iter()
            .map(|row| row.iter().map(|&i| sizes.box_w[i]).max().unwrap_or(0))
            .collect();

        let max_label = graph
            .edges
            .iter()
            .filter(|e| e.from == e.to || ranks[e.to] == ranks[e.from] + 1)
            .filter_map(|e| e.label.as_deref())
            .map(|l| string_width(l).min(MAX_LABEL))
            .max()
            .unwrap_or(0);
        let base_gap = (GAP_X + 1).max(max_label + 3);

        let centers = assign_positions(by_rank, &sizes.lay_h, 1, &graph.edges, ranks);

        let mut edge_bus = vec![0usize; graph.edges.len()];
        let mut bus_tracks = vec![0usize; max_rank + 1];
        for r in 0..max_rank {
            let spans = bus_spans(graph, ranks, &centers, r, true);
            if spans.is_empty() {
                continue;
            }
            let (assigned, count) = assign_tracks(&spans);
            for (idx, slot) in assigned {
                edge_bus[idx] = slot;
            }
            bus_tracks[r] = count;
        }

        let mut rank_x = vec![0usize; max_rank + 1];
        for r in 1..=max_rank {
            rank_x[r] = rank_x[r - 1] + col_w[r - 1] + base_gap.max(bus_tracks[r - 1] + 1);
        }
        let max_self_tail = by_rank[max_rank]
            .iter()
            .filter(|&&i| sizes.extra_h[i] > 0 && sizes.self_label_w[i] > 0)
            .map(|&i| 2 + sizes.self_label_w[i])
            .max()
            .unwrap_or(0);
        let canvas_w = rank_x[max_rank] + col_w[max_rank] + max_self_tail;
        let band_end: Vec<usize> = (0..=max_rank).map(|r| rank_x[r] + col_w[r]).collect();

        let mut diagram_h = 1usize;
        for (r, row) in by_rank.iter().enumerate() {
            let x = rank_x[r];
            for &idx in row {
                let w = sizes.box_w[idx];
                let h = sizes.box_h[idx];
                let cy = centers[idx];
                let y = cy.saturating_sub(half(h + sizes.extra_h[idx]));
                placed[idx] = Placed {
                    x,
                    y,
                    w,
                    h,
                    cx: x + half(w),
                    cy: y + half(h),
                    rank: r,
                };
                diagram_h = diagram_h.max(y + h + sizes.extra_h[idx]);
            }
        }

        let mut edge_lane = vec![0usize; graph.edges.len()];
        let lanes = lane_spans(graph, ranks, placed, false);
        let mut canvas_h = diagram_h;
        let mut lane_base = 0usize;
        if !lanes.is_empty() {
            let (assigned, count) = assign_tracks(&lanes);
            for (idx, slot) in assigned {
                edge_lane[idx] = slot;
            }
            canvas_h = diagram_h + 1 + count;
            lane_base = diagram_h + 1;
        }

        RoutePlan {
            canvas_w,
            canvas_h,
            band_end,
            edge_bus,
            lane_base,
            edge_lane,
        }
    }

    // -------------------------------------------------------------------- canvas

    fn layout_canvas(graph: &Graph, extras: &[NodeExtra]) -> Option<Canvas> {
        let n = graph.nodes.len();
        if n == 0 {
            return None;
        }

        let ranks = compute_ranks(graph);
        let max_rank = ranks.iter().copied().max().unwrap_or(0);

        let mut by_rank = vec![Vec::new(); max_rank + 1];
        for (idx, &r) in ranks.iter().enumerate() {
            by_rank[r].push(idx);
        }
        order_ranks(&mut by_rank, &graph.edges, &ranks);

        let wrapped: Vec<Vec<String>> = graph
            .nodes
            .iter()
            .map(|node| wrap_label(&node.label, WRAP_WIDTH, MAX_LINES))
            .collect();
        let widest = |lines: &[String]| -> usize { lines.iter().map(|l| string_width(l)).max().unwrap_or(1).max(1) };

        let mut box_w: Vec<usize> = extras
            .iter()
            .enumerate()
            .map(|(i, extra)| match extra {
                NodeExtra::Frame { sub } => {
                    (sub.w + 2).max(string_width(&fit_label(&graph.nodes[i].label, WRAP_WIDTH)) + 4)
                }
                NodeExtra::Compartments { sections } => {
                    let flat: Vec<String> = sections.iter().flat_map(|s| s.iter().cloned()).collect();
                    widest(&flat) + 2 * PAD + 2
                }
                NodeExtra::Plain => widest(&wrapped[i]) + 2 * PAD + 2,
            })
            .collect();

        let box_h: Vec<usize> = extras
            .iter()
            .enumerate()
            .map(|(i, extra)| match extra {
                NodeExtra::Frame { sub } => sub.h + 2,
                NodeExtra::Compartments { sections } => {
                    let filled = sections.iter().filter(|s| !s.is_empty()).count();
                    let rows: usize = sections.iter().map(Vec::len).sum();
                    rows + filled.saturating_sub(1) + 2
                }
                NodeExtra::Plain => wrapped[i].len() + 2,
            })
            .collect();

        let mut extra_h = vec![0usize; n];
        let mut self_label_w = vec![0usize; n];
        for e in &graph.edges {
            if e.from != e.to {
                continue;
            }
            extra_h[e.from] = 2;
            if let Some(ref label) = e.label {
                self_label_w[e.from] = self_label_w[e.from].max(string_width(label).min(MAX_LABEL));
            }
        }
        for i in 0..n {
            if extra_h[i] > 0 {
                box_w[i] = box_w[i].max(7);
            }
        }

        let sizes = NodeSizes {
            lay_w: box_w
                .iter()
                .enumerate()
                .map(|(i, &w)| {
                    w + if self_label_w[i] > 0 {
                        2 * (self_label_w[i] + 3)
                    } else {
                        0
                    }
                })
                .collect(),
            lay_h: box_h.iter().enumerate().map(|(i, &h)| h + extra_h[i]).collect(),
            box_w,
            box_h,
            extra_h,
            self_label_w,
        };

        let mut placed = vec![
            Placed {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
                cx: 0,
                cy: 0,
                rank: 0,
            };
            n
        ];

        let vertical = matches!(graph.dir, Dir::Down | Dir::Up);
        let plan = if vertical {
            place_td(&ranks, max_rank, &by_rank, &sizes, graph, &mut placed)
        } else {
            place_lr(&ranks, max_rank, &by_rank, &sizes, graph, &mut placed)
        };

        if plan.canvas_w * plan.canvas_h > MAX_CANVAS_CELLS {
            return None;
        }

        let mut canvas = Canvas::new(plan.canvas_w, plan.canvas_h);
        for (idx, extra) in extras.iter().enumerate() {
            match extra {
                NodeExtra::Frame { sub } => draw_frame(&mut canvas, placed[idx], &graph.nodes[idx].label, sub),
                NodeExtra::Compartments { sections } => draw_class_box(&mut canvas, placed[idx], sections),
                NodeExtra::Plain => draw_box(&mut canvas, placed[idx], &wrapped[idx], graph.nodes[idx].shape),
            }
        }

        for (i, edge) in graph.edges.iter().enumerate() {
            canvas.cur_style = match edge.line {
                LineKind::Dotted => STY_DOT,
                LineKind::Thick => STY_THICK,
                LineKind::Solid => STY_SOLID,
            };
            if edge.from == edge.to {
                route_self(&mut canvas, placed[edge.from], edge);
                continue;
            }
            let from = placed[edge.from];
            let to = placed[edge.to];
            let adjacent = to.rank == from.rank + 1;
            let bus = plan.band_end[from.rank] + plan.edge_bus[i];
            let lane = plan.lane_base + plan.edge_lane[i];
            if vertical {
                if adjacent {
                    route_forward(&mut canvas, from, to, edge, bus);
                } else {
                    route_back(&mut canvas, from, to, edge, lane);
                }
            } else if adjacent {
                route_forward_lr(&mut canvas, from, to, edge, bus);
            } else {
                route_back_lr(&mut canvas, from, to, edge, lane);
            }
        }

        canvas.finalize_mask();
        Some(canvas)
    }

    fn orient(mut canvas: Canvas, graph: &Graph) -> Canvas {
        if graph.dir == Dir::Up {
            canvas.flip_vertical();
        } else if graph.dir == Dir::Left {
            canvas.flip_horizontal();
        }
        canvas
    }

    pub(super) fn layout_flowchart(graph: &Graph) -> Option<Canvas> {
        let extras: Vec<NodeExtra> = (0..graph.nodes.len()).map(|_| NodeExtra::Plain).collect();
        let canvas = layout_canvas(graph, &extras)?;
        Some(orient(canvas, graph))
    }

    pub(super) fn layout_class(graph: &Graph, infos: &[ClassInfo]) -> Option<Canvas> {
        let extras: Vec<NodeExtra> = graph
            .nodes
            .iter()
            .enumerate()
            .map(|(i, node)| {
                let mut title = Vec::new();
                if let Some(ref ann) = infos[i].annotation {
                    title.push(format!("«{ann}»"));
                }
                title.push(display_generics(&node.label));
                NodeExtra::Compartments {
                    sections: vec![title, infos[i].attrs.clone(), infos[i].methods.clone()],
                }
            })
            .collect();
        let canvas = layout_canvas(graph, &extras)?;
        Some(orient(canvas, graph))
    }

    // -------------------------------------------------------------------- groups

    pub(super) fn layout_grouped(graph: &Graph) -> Option<Canvas> {
        let mut proxy: IndexMap<usize, usize> = IndexMap::new();
        for (gi, g) in graph.groups.iter().enumerate() {
            if let Some(&ni) = graph.index.get(&g.id) {
                proxy.insert(ni, gi);
            }
        }

        let group_chain = |g: Option<usize>| -> Vec<usize> {
            let mut chain = Vec::new();
            let mut cur = g;
            while let Some(c) = cur {
                chain.push(c);
                cur = graph.groups[c].parent;
            }
            chain.reverse();
            chain
        };

        #[derive(Clone)]
        enum ItemKey {
            Node(usize),
            Group(usize),
        }

        let endpoint = |n: usize| -> (ItemKey, Vec<usize>) {
            if let Some(&gi) = proxy.get(&n) {
                (ItemKey::Group(gi), group_chain(graph.groups[gi].parent))
            } else {
                (ItemKey::Node(n), group_chain(graph.node_group[n]))
            }
        };

        let mut scope_edges: IndexMap<Option<usize>, Vec<(ItemKey, ItemKey, usize)>> = IndexMap::new();
        let mut referenced = vec![false; graph.groups.len()];
        for (ei, e) in graph.edges.iter().enumerate() {
            let (f_key_base, f_chain) = endpoint(e.from);
            let (t_key_base, t_chain) = endpoint(e.to);
            let mut k = 0usize;
            while k < f_chain.len() && k < t_chain.len() && f_chain[k] == t_chain[k] {
                k += 1;
            }
            let scope = if k == 0 { None } else { Some(f_chain[k - 1]) };
            let f_key = if f_chain.len() > k {
                ItemKey::Group(f_chain[k])
            } else {
                f_key_base
            };
            let t_key = if t_chain.len() > k {
                ItemKey::Group(t_chain[k])
            } else {
                t_key_base
            };
            if let ItemKey::Group(gi) = f_key {
                referenced[gi] = true;
            }
            if let ItemKey::Group(gi) = t_key {
                referenced[gi] = true;
            }
            scope_edges.entry(scope).or_default().push((f_key, t_key, ei));
        }

        let mut direct_nodes: IndexMap<Option<usize>, Vec<usize>> = IndexMap::new();
        for (ni, &g) in graph.node_group.iter().enumerate() {
            if proxy.contains_key(&ni) {
                continue;
            }
            direct_nodes.entry(g).or_default().push(ni);
        }

        let mut keep = vec![false; graph.groups.len()];
        for gi in (0..graph.groups.len()).rev() {
            let has_nodes = direct_nodes.get(&Some(gi)).is_some_and(|v| !v.is_empty());
            let has_children = graph
                .groups
                .iter()
                .enumerate()
                .any(|(c, g)| g.parent == Some(gi) && keep[c]);
            keep[gi] = has_nodes || has_children || referenced[gi];
        }

        #[derive(Clone, PartialEq, Eq, Hash)]
        struct ScopeKey(u8, usize);

        fn build_scope(
            graph: &Graph,
            scope: Option<usize>,
            scope_edges: &IndexMap<Option<usize>, Vec<(ItemKey, ItemKey, usize)>>,
            direct_nodes: &IndexMap<Option<usize>, Vec<usize>>,
            keep: &[bool],
        ) -> Option<Canvas> {
            let mut items: Vec<ItemKey> = direct_nodes
                .get(&scope)
                .map(|v| v.iter().copied().map(ItemKey::Node).collect())
                .unwrap_or_default();
            for (gi, g) in graph.groups.iter().enumerate() {
                if g.parent == scope && keep[gi] {
                    items.push(ItemKey::Group(gi));
                }
            }

            if items.is_empty() {
                return Some(Canvas::new(1, 1));
            }

            let mut index_of: IndexMap<ScopeKey, usize> = IndexMap::new();
            let mut nodes = Vec::new();
            let mut extras = Vec::new();
            for item in items {
                let sk = match item {
                    ItemKey::Node(i) => ScopeKey(0, i),
                    ItemKey::Group(i) => ScopeKey(1, i),
                };
                index_of.insert(sk, nodes.len());
                match item {
                    ItemKey::Node(i) => {
                        nodes.push(Node {
                            label: graph.nodes[i].label.clone(),
                            shape: graph.nodes[i].shape,
                        });
                        extras.push(NodeExtra::Plain);
                    }
                    ItemKey::Group(i) => {
                        let sub = build_scope(graph, Some(i), scope_edges, direct_nodes, keep)?;
                        nodes.push(Node {
                            label: graph.groups[i].label.clone(),
                            shape: Shape::Rect,
                        });
                        extras.push(NodeExtra::Frame { sub });
                    }
                }
            }

            let mut edges = Vec::new();
            if let Some(list) = scope_edges.get(&scope) {
                for (f, t, ei) in list {
                    let fk = match *f {
                        ItemKey::Node(i) => ScopeKey(0, i),
                        ItemKey::Group(i) => ScopeKey(1, i),
                    };
                    let tk = match *t {
                        ItemKey::Node(i) => ScopeKey(0, i),
                        ItemKey::Group(i) => ScopeKey(1, i),
                    };
                    let (Some(&fi), Some(&ti)) = (index_of.get(&fk), index_of.get(&tk)) else {
                        continue;
                    };
                    let e = &graph.edges[*ei];
                    edges.push(Edge {
                        from: fi,
                        to: ti,
                        label: e.label.clone(),
                        head_to: e.head_to,
                        head_from: e.head_from,
                        line: e.line,
                    });
                }
            }

            let mut synth = Graph::new(graph.dir);
            synth.nodes = nodes;
            synth.edges = edges;
            layout_canvas(&synth, &extras)
        }

        let canvas = build_scope(graph, None, &scope_edges, &direct_nodes, &keep)?;
        Some(orient(canvas, graph))
    }

    // ------------------------------------------------------------------- drawing

    pub(super) fn draw_box(canvas: &mut Canvas, p: Placed, lines: &[String], shape: Shape) {
        let Placed { x, y, w, h, .. } = p;
        let right = x + w - 1;
        let bottom = y + h - 1;

        let rounded = matches!(shape, Shape::Round | Shape::Diamond);
        canvas.set(x, y, if rounded { "╭" } else { "┌" }, Cls::Border);
        canvas.set(right, y, if rounded { "╮" } else { "┐" }, Cls::Border);
        canvas.set(x, bottom, if rounded { "╰" } else { "└" }, Cls::Border);
        canvas.set(right, bottom, if rounded { "╯" } else { "┘" }, Cls::Border);

        for cx in (x + 1)..right {
            canvas.add_bits(cx, y, L | R, Cls::Border);
            canvas.add_bits(cx, bottom, L | R, Cls::Border);
        }
        for cy in (y + 1)..bottom {
            canvas.add_bits(x, cy, U | D, Cls::Border);
            canvas.add_bits(right, cy, U | D, Cls::Border);
        }

        for cy in y..=bottom {
            for cx in x..=right {
                let idx = canvas.idx(cx, cy);
                canvas.occupied[idx] = 1;
            }
        }

        let inner = 1.max(w.saturating_sub(2 * PAD + 2));
        for (li, line) in lines.iter().enumerate() {
            let text = fit_label(line, inner);
            let text_x = x + 1 + PAD + half(inner.saturating_sub(string_width(&text)));
            draw_text(canvas, &text, text_x, y + 1 + li, Cls::Text);
        }
    }

    fn draw_class_box(canvas: &mut Canvas, p: Placed, sections: &[Vec<String>]) {
        draw_box(canvas, p, &[], Shape::Rect);
        let inner = 1.max(p.w.saturating_sub(2 * PAD + 2));
        let mut row = p.y + 1;
        let mut first = true;
        for (si, section) in sections.iter().enumerate() {
            if section.is_empty() {
                continue;
            }
            if !first {
                canvas.set(p.x, row, "├", Cls::Border);
                for x in (p.x + 1)..(p.x + p.w - 1) {
                    canvas.set(x, row, "─", Cls::Border);
                }
                canvas.set(p.x + p.w - 1, row, "┤", Cls::Border);
                row += 1;
            }
            first = false;
            for line in section {
                let text = fit_label(line, inner);
                let tx = if si == 0 {
                    p.x + 1 + PAD + half(inner.saturating_sub(string_width(&text)))
                } else {
                    p.x + 1 + PAD
                };
                draw_text_over_edges(canvas, &text, tx, row, Cls::Text);
                row += 1;
            }
        }
    }

    fn draw_frame(canvas: &mut Canvas, p: Placed, title: &str, sub: &Canvas) {
        draw_box(canvas, p, &[], Shape::Rect);
        let t = fit_label(title, p.w.saturating_sub(4));
        draw_text_over_edges(canvas, &format!(" {t} "), p.x + 1, p.y, Cls::Text);
        canvas.blit(sub, p.x + 1 + half(p.w - 2 - sub.w), p.y + 1 + half(p.h - 2 - sub.h));
    }

    // ------------------------------------------------------------------- routing

    fn head_glyph(head: Head, arrow: &'static str) -> &'static str {
        match head {
            Head::Circle => "o",
            Head::Cross => "×",
            Head::DiamondFill => "◆",
            Head::DiamondOpen => "◇",
            Head::Triangle => match arrow {
                "▼" => "▽",
                "▲" => "△",
                "◄" => "◁",
                "▶" => "▷",
                _ => arrow,
            },
            _ => arrow,
        }
    }

    fn route_forward(canvas: &mut Canvas, from: Placed, to: Placed, edge: &Edge, bus: usize) {
        let tx = to.cx;
        let bx = if from.cx.abs_diff(tx) <= 1 { tx } else { from.cx };
        let by = from.y + from.h - 1;
        let head_row = to.y - 1;

        canvas.junction(bx, by, D);
        canvas.seg_v(bx, by, bus);
        if bx == tx {
            canvas.seg_v(bx, bus, head_row);
        } else {
            canvas.seg_h(bus, bx, tx);
            canvas.seg_v(tx, bus, head_row);
        }

        if edge.head_to == Head::None {
            canvas.add_bits(tx, head_row, U, Cls::Edge);
        } else {
            canvas.set(tx, head_row, head_glyph(edge.head_to, "▼"), Cls::Edge);
        }
        if edge.head_from != Head::None {
            canvas.set(bx, by, head_glyph(edge.head_from, "▲"), Cls::Edge);
        }

        if let Some(ref label) = edge.label {
            place_label(canvas, label, head_row, tx + 1);
        }
    }

    fn route_self(canvas: &mut Canvas, p: Placed, edge: &Edge) {
        let bottom = p.y + p.h - 1;
        let exit_x = p.cx + 1;
        let ret_x = p.x + p.w - 2;
        if ret_x <= exit_x || bottom + 2 >= canvas.h {
            return;
        }

        let (v, h, bl, br) = match edge.line {
            LineKind::Dotted => ("╎", "╌", "╰", "╯"),
            LineKind::Thick => ("┃", "━", "┗", "┛"),
            LineKind::Solid => ("│", "─", "╰", "╯"),
        };

        canvas.junction(exit_x, bottom, D);
        canvas.set(exit_x, bottom + 1, v, Cls::Edge);
        canvas.set(exit_x, bottom + 2, bl, Cls::Edge);
        for x in (exit_x + 1)..ret_x {
            canvas.set(x, bottom + 2, h, Cls::Edge);
        }
        canvas.set(ret_x, bottom + 2, br, Cls::Edge);
        canvas.set(ret_x, bottom + 1, head_glyph(edge.head_to, "▲"), Cls::Edge);
        if let Some(ref label) = edge.label {
            place_label(canvas, label, bottom + 1, p.x + p.w + 1);
        }
    }

    fn route_back(canvas: &mut Canvas, from: Placed, to: Placed, edge: &Edge, lane_x: usize) {
        let sx = from.x + from.w - 1;
        let sy = from.cy;
        let tx = to.x + to.w - 1;
        let tyc = to.cy;

        canvas.junction(sx, sy, R);
        canvas.seg_h(sy, sx, lane_x);
        canvas.seg_v(lane_x, sy, tyc);
        canvas.seg_h(tyc, tx + 1, lane_x);

        if edge.head_to == Head::None {
            canvas.add_bits(tx + 1, tyc, R, Cls::Edge);
        } else {
            canvas.set(tx + 1, tyc, head_glyph(edge.head_to, "◄"), Cls::Edge);
        }
        if edge.head_from != Head::None {
            canvas.set(sx, sy, head_glyph(edge.head_from, "◄"), Cls::Edge);
        }

        if let Some(ref label) = edge.label {
            place_label(
                canvas,
                label,
                tyc.saturating_sub(1),
                lane_x.saturating_sub(string_width(label) + 1),
            );
        }
    }

    fn route_forward_lr(canvas: &mut Canvas, from: Placed, to: Placed, edge: &Edge, bus: usize) {
        let rx = from.x + from.w - 1;
        let ry = from.cy;
        let ly = to.cy;
        let head_col = to.x - 1;

        canvas.junction(rx, ry, R);
        canvas.seg_h(ry, rx, bus);
        if ry == ly {
            canvas.seg_h(ry, bus, head_col);
        } else {
            canvas.seg_v(bus, ry, ly);
            canvas.seg_h(ly, bus, head_col);
        }

        if edge.head_to == Head::None {
            canvas.add_bits(head_col, ly, R, Cls::Edge);
        } else {
            canvas.set(head_col, ly, head_glyph(edge.head_to, "▶"), Cls::Edge);
        }
        if edge.head_from != Head::None {
            canvas.set(rx, ry, head_glyph(edge.head_from, "◄"), Cls::Edge);
        }

        if let Some(ref label) = edge.label {
            place_label(canvas, label, ly.saturating_sub(1), bus + 1);
        }
    }

    fn route_back_lr(canvas: &mut Canvas, from: Placed, to: Placed, edge: &Edge, lane_y: usize) {
        let sx = from.cx;
        let sy = from.y + from.h - 1;
        let tx = to.cx;
        let ty = to.y + to.h - 1;

        canvas.junction(sx, sy, D);
        canvas.seg_v(sx, sy, lane_y);
        canvas.seg_h(lane_y, sx, tx);
        canvas.seg_v(tx, lane_y, ty + 1);

        if edge.head_to == Head::None {
            canvas.add_bits(tx, ty + 1, D, Cls::Edge);
        } else {
            canvas.set(tx, ty + 1, head_glyph(edge.head_to, "▲"), Cls::Edge);
        }
        if edge.head_from != Head::None {
            canvas.set(sx, sy, head_glyph(edge.head_from, "▲"), Cls::Edge);
        }

        if let Some(ref label) = edge.label {
            place_label(canvas, label, lane_y.saturating_sub(1), half(sx + tx));
        }
    }

    fn place_label(canvas: &mut Canvas, label: &str, row: usize, start_x: usize) {
        if row >= canvas.h {
            return;
        }
        let text = fit_label(label, MAX_LABEL);
        let mut x = start_x;
        for (c, cw) in measured(&text) {
            if cw == 0 {
                continue;
            }
            if x + cw > canvas.w {
                break;
            }
            let mut blocked = false;
            for k in 0..cw {
                let i = canvas.idx(x + k, row);
                if canvas.ch[i] != " " || canvas.mask[i] != 0 || canvas.occupied[i] != 0 {
                    blocked = true;
                }
            }
            if blocked {
                break;
            }
            canvas.set(x, row, c, Cls::EdgeLabel);
            for k in 1..cw {
                canvas.set(x + k, row, CONT, Cls::EdgeLabel);
            }
            x += cw;
        }
    }
}

mod layout_seq {
    use super::Cls;
    use super::canvas::{Canvas, D, L, R, U, draw_text_over_edges};
    use super::graph::Shape;
    use super::labels::{WRAP_WIDTH, fit_label};
    use super::layout::{MAX_CANVAS_CELLS, PAD, Placed, draw_box, half};
    use super::parse::{NoteAnchor, SeqHead, SeqItem, Sequence};
    use super::width::string_width;

    const SEQ_GAP: usize = 5;

    fn note_geometry(xs: &[usize], anchor: NoteAnchor, text_w: usize) -> (usize, usize) {
        match anchor {
            NoteAnchor::Over { from, to } => {
                let center = half(xs[from] + xs[to]);
                let w = (xs[to] - xs[from] + 5).max(text_w + 2 * PAD + 2);
                (center.saturating_sub(half(w)), w)
            }
            NoteAnchor::Left { at } => {
                let w = text_w + 2 * PAD + 2;
                (xs[at].saturating_sub(2 + w - 1), w)
            }
            NoteAnchor::Right { at } => {
                let w = text_w + 2 * PAD + 2;
                (xs[at] + 2, w)
            }
        }
    }

    fn item_text_w(text: Option<&str>) -> usize {
        text.map(string_width).unwrap_or(0)
    }

    pub(super) fn layout_sequence(seq: &Sequence) -> Option<Canvas> {
        let n = seq.labels.len();
        let labels: Vec<String> = seq.labels.iter().map(|l| fit_label(l, WRAP_WIDTH)).collect();
        let box_w: Vec<usize> = labels.iter().map(|l| string_width(l).max(1) + 2 * PAD + 2).collect();
        let box_h = 3usize;

        let mut gaps: Vec<usize> = (0..n.saturating_sub(1))
            .map(|i| SEQ_GAP.max(box_w[i].div_ceil(2) + box_w[i + 1].div_ceil(2) + 1))
            .collect();

        let mut reqs: Vec<(usize, usize, usize)> = Vec::new();
        for item in &seq.items {
            match item {
                SeqItem::Message { from, to, text, .. } => {
                    let tw = item_text_w(text.as_deref());
                    if from != to {
                        reqs.push(((*from).min(*to), (*from).max(*to), (tw + 2).max(4)));
                    } else if from + 1 < n {
                        reqs.push((*from, from + 1, 5 + tw + 2));
                    }
                }
                SeqItem::Note { anchor, text } => {
                    let tw = string_width(text);
                    match *anchor {
                        NoteAnchor::Over { from, to } if from < to => {
                            reqs.push((from, to, tw.saturating_sub(1)));
                        }
                        NoteAnchor::Over { from, .. } => {
                            let need = (tw + 4).div_ceil(2) + 2;
                            if from > 0 {
                                reqs.push((from - 1, from, need));
                            }
                            if from + 1 < n {
                                reqs.push((from, from + 1, need));
                            }
                        }
                        NoteAnchor::Left { at } if at > 0 => {
                            reqs.push((at - 1, at, tw + 7));
                        }
                        NoteAnchor::Right { at } if at + 1 < n => {
                            reqs.push((at, at + 1, tw + 7));
                        }
                        _ => {}
                    }
                }
                SeqItem::Divider { .. } => {}
            }
        }
        reqs.sort_by_key(|&(l, r, _)| r - l);
        for (l, r, need) in reqs {
            let cur: usize = gaps[l..r].iter().sum();
            if cur < need {
                gaps[r - 1] += need - cur;
            }
        }

        let mut xs = vec![0usize; n];
        xs[0] = half(box_w[0]);
        for i in 1..n {
            xs[i] = xs[i - 1] + gaps[i - 1];
        }

        let mut canvas_w = xs[n - 1] + box_w[n - 1].div_ceil(2) + 1;
        for item in &seq.items {
            match item {
                SeqItem::Message { from, to, text, .. } if from == to => {
                    canvas_w = canvas_w.max(xs[*from] + 5 + item_text_w(text.as_deref()) + 1);
                }
                SeqItem::Note { anchor, text } => {
                    let (gx, gw) = note_geometry(&xs, *anchor, string_width(text));
                    canvas_w = canvas_w.max(gx + gw + 1);
                }
                SeqItem::Divider { text } => {
                    canvas_w = canvas_w.max(string_width(text) + 4);
                }
                _ => {}
            }
        }

        let mut rows = Vec::with_capacity(seq.items.len());
        let mut y = box_h + 1;
        for item in &seq.items {
            rows.push(y);
            y += row_height(item);
        }
        let bottom_top = y;
        let canvas_h = bottom_top + box_h;

        if canvas_w * canvas_h > MAX_CANVAS_CELLS {
            return None;
        }

        let mut canvas = Canvas::new(canvas_w, canvas_h);

        for i in 0..n {
            let bx = xs[i].saturating_sub(half(box_w[i]));
            for by in [0, bottom_top] {
                draw_box(
                    &mut canvas,
                    make_box(bx, by, box_w[i], box_h),
                    std::slice::from_ref(&labels[i]),
                    Shape::Rect,
                );
            }
        }
        for (k, item) in seq.items.iter().enumerate() {
            if let SeqItem::Note { anchor, text } = item {
                let (gx, gw) = note_geometry(&xs, *anchor, string_width(text));
                draw_box(
                    &mut canvas,
                    make_box(gx, rows[k], gw, 3),
                    std::slice::from_ref(text),
                    Shape::Rect,
                );
            }
        }

        for &x in &xs {
            canvas.junction(x, box_h - 1, D);
            canvas.seg_v(x, box_h, bottom_top - 1);
            canvas.junction(x, bottom_top, U);
        }

        for (k, item) in seq.items.iter().enumerate() {
            let r = rows[k];
            match item {
                SeqItem::Message {
                    from,
                    to,
                    text,
                    dashed,
                    head,
                } => draw_message(&mut canvas, *from, *to, text.as_deref(), *dashed, *head, &xs, r),
                SeqItem::Divider { text } => draw_divider(&mut canvas, text, r, canvas_w),
                SeqItem::Note { .. } => {}
            }
        }

        canvas.finalize_mask();
        Some(canvas)
    }

    fn row_height(item: &SeqItem) -> usize {
        match item {
            SeqItem::Note { .. } => 4,
            SeqItem::Divider { .. } => 2,
            SeqItem::Message { from, to, text, .. } => {
                if from == to {
                    4
                } else if text.is_some() {
                    3
                } else {
                    2
                }
            }
        }
    }

    fn make_box(x: usize, y: usize, w: usize, h: usize) -> Placed {
        Placed {
            x,
            y,
            w,
            h,
            cx: x + half(w),
            cy: y + 1,
            rank: 0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_message(
        canvas: &mut Canvas,
        from: usize,
        to: usize,
        text: Option<&str>,
        dashed: bool,
        head: SeqHead,
        xs: &[usize],
        r: usize,
    ) {
        let line_ch = if dashed { "╌" } else { "─" };

        if from == to {
            let x = xs[from];
            canvas.junction(x, r, R);
            canvas.set(x + 1, r, line_ch, Cls::Edge);
            canvas.set(x + 2, r, line_ch, Cls::Edge);
            canvas.set(x + 3, r, "╮", Cls::Edge);
            canvas.set(x + 3, r + 1, "│", Cls::Edge);
            canvas.set(x + 1, r + 2, if head == SeqHead::Cross { "×" } else { "◄" }, Cls::Edge);
            canvas.set(x + 2, r + 2, line_ch, Cls::Edge);
            canvas.set(x + 3, r + 2, "╯", Cls::Edge);
            if let Some(t) = text {
                draw_text_over_edges(canvas, t, x + 5, r + 1, Cls::Text);
            }
            return;
        }

        let x0 = xs[from];
        let x1 = xs[to];
        let rightward = x1 > x0;
        let arrow_row = if text.is_some() { r + 1 } else { r };
        let lo = x0.min(x1);
        let hi = x0.max(x1);

        canvas.junction(x0, arrow_row, if rightward { R } else { L });
        for x in (lo + 1)..hi {
            canvas.set(x, arrow_row, line_ch, Cls::Edge);
        }
        let head_ch = if head == SeqHead::Cross {
            "×"
        } else if rightward {
            "▶"
        } else {
            "◄"
        };
        canvas.set(if rightward { x1 - 1 } else { x1 + 1 }, arrow_row, head_ch, Cls::Edge);

        if let Some(txt) = text {
            let span = hi - lo - 1;
            let t = fit_label(txt, span.max(1));
            draw_text_over_edges(
                canvas,
                &t,
                lo + 1 + half(span.saturating_sub(string_width(&t))),
                r,
                Cls::Text,
            );
        }
    }

    fn draw_divider(canvas: &mut Canvas, text: &str, r: usize, canvas_w: usize) {
        for x in 0..canvas_w {
            canvas.set(x, r, "─", Cls::Edge);
        }
        let lbl = fit_label(text, canvas_w.saturating_sub(4));
        draw_text_over_edges(canvas, &format!(" {lbl} "), 2, r, Cls::EdgeLabel);
    }
}
