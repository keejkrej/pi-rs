//! Port of packages/coding-agent/src/modes/interactive/theme/system-theme.ts

use std::sync::LazyLock;

use indexmap::IndexMap;
use pi_tui::{
    Color, OkhslChannels, RgbColor, color_to_okhsl, color_to_oklch, color_to_rgb, okhsl_color,
    oklab_to_okhsl_lightness, oklch_color, rgb_color,
};

use super::theme::{ThemeAppearance, ThemeBg, ThemeColor, ThemeToken};
use super::theme_schema::ThemeColorValue;

pub const SYSTEM_THEME_NAME: &str = "system";

#[derive(Clone, Copy)]
struct Family {
    hue: f64,
    saturation_min: f64,
    saturation_max: f64,
    slot: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FamilyName {
    Neutral,
    Blue,
    Green,
    Red,
    Yellow,
    Orange,
    Violet,
    Calamine,
    ThinkingSlate,
    ThinkingBlue,
    ThinkingPeriwinkle,
    ThinkingViolet,
    ThinkingMagenta,
    ThinkingRed,
}

fn family_spec(name: FamilyName) -> Family {
    match name {
        FamilyName::Neutral => Family {
            hue: 231.49,
            saturation_min: 0.02,
            saturation_max: 0.08,
            slot: 8,
        },
        FamilyName::Blue => Family {
            hue: 231.49,
            saturation_min: 0.1,
            saturation_max: 0.68,
            slot: 4,
        },
        FamilyName::Green => Family {
            hue: 158.68,
            saturation_min: 0.1,
            saturation_max: 0.76,
            slot: 2,
        },
        FamilyName::Red => Family {
            hue: 20.0,
            saturation_min: 0.1,
            saturation_max: 0.92,
            slot: 1,
        },
        FamilyName::Yellow => Family {
            hue: 82.36,
            saturation_min: 0.5,
            saturation_max: 1.0,
            slot: 3,
        },
        FamilyName::Orange => Family {
            hue: 52.0,
            saturation_min: 0.12,
            saturation_max: 0.85,
            slot: 3,
        },
        FamilyName::Violet => Family {
            hue: 295.0,
            saturation_min: 0.2,
            saturation_max: 0.6,
            slot: 5,
        },
        FamilyName::Calamine => Family {
            hue: 202.43,
            saturation_min: 0.1,
            saturation_max: 0.74,
            slot: 6,
        },
        FamilyName::ThinkingSlate => Family {
            hue: 231.49,
            saturation_min: 0.08,
            saturation_max: 0.2,
            slot: 4,
        },
        FamilyName::ThinkingBlue => Family {
            hue: 231.49,
            saturation_min: 0.2,
            saturation_max: 0.45,
            slot: 4,
        },
        FamilyName::ThinkingPeriwinkle => Family {
            hue: 263.25,
            saturation_min: 0.3,
            saturation_max: 0.6,
            slot: 6,
        },
        FamilyName::ThinkingViolet => Family {
            hue: 295.0,
            saturation_min: 0.4,
            saturation_max: 0.75,
            slot: 5,
        },
        FamilyName::ThinkingMagenta => Family {
            hue: 337.5,
            saturation_min: 0.5,
            saturation_max: 0.85,
            slot: 13,
        },
        FamilyName::ThinkingRed => Family {
            hue: 20.0,
            saturation_min: 0.95,
            saturation_max: 1.0,
            slot: 1,
        },
    }
}

fn token_family(token: ThemeToken) -> FamilyName {
    use ThemeBg::*;
    use ThemeColor::*;
    match token {
        ThemeToken::Bg(SelectedBg | UserMessageBg) => FamilyName::Blue,
        ThemeToken::Bg(SearchMatchBg) => FamilyName::Orange,
        ThemeToken::Bg(CustomMessageBg) => FamilyName::Violet,
        ThemeToken::Bg(ToolPendingBg) => FamilyName::Neutral,
        ThemeToken::Bg(ToolSuccessBg) => FamilyName::Green,
        ThemeToken::Bg(ToolErrorBg) => FamilyName::Red,
        ThemeToken::Fg(
            Text | UserMessageText | CustomMessageText | ToolTitle | SyntaxOperator | SyntaxPunctuation | Muted | Dim
            | ThinkingText | ToolOutput | MdLinkUrl | MdQuote | MdQuoteBorder | MdHr | MdCodeBlockBorder
            | ToolDiffContext | SyntaxComment | ScrollbarTrack | ScrollbarThumb | SearchMatchText | BorderMuted
            | ThinkingOff,
        ) => FamilyName::Neutral,
        ThemeToken::Fg(Accent | BorderAccent | CustomMessageLabel | MdCode | MdListBullet | SyntaxType) => {
            FamilyName::Violet
        }
        ThemeToken::Fg(Border | MdLink | SyntaxKeyword) => FamilyName::Blue,
        ThemeToken::Fg(SyntaxVariable) => FamilyName::Calamine,
        ThemeToken::Fg(Success | MdCodeBlock | ToolDiffAdded | BashMode | SyntaxNumber) => FamilyName::Green,
        ThemeToken::Fg(Error | ToolDiffRemoved) => FamilyName::Red,
        ThemeToken::Fg(Warning | MdHeading | SyntaxFunction) => FamilyName::Yellow,
        ThemeToken::Fg(SyntaxString) => FamilyName::Orange,
        ThemeToken::Fg(ThinkingMinimal) => FamilyName::ThinkingSlate,
        ThemeToken::Fg(ThinkingLow) => FamilyName::ThinkingBlue,
        ThemeToken::Fg(ThinkingMedium) => FamilyName::ThinkingPeriwinkle,
        ThemeToken::Fg(ThinkingHigh) => FamilyName::ThinkingViolet,
        ThemeToken::Fg(ThinkingXhigh) => FamilyName::ThinkingMagenta,
        ThemeToken::Fg(ThinkingMax) => FamilyName::ThinkingRed,
    }
}

fn token_slot_override(token: ThemeToken) -> Option<usize> {
    match token {
        ThemeToken::Fg(ThemeColor::SyntaxString) => Some(2),
        ThemeToken::Fg(ThemeColor::SyntaxNumber) => Some(5),
        ThemeToken::Bg(ThemeBg::SearchMatchBg) => Some(3),
        _ => None,
    }
}

#[derive(Clone, Copy)]
struct Curve {
    coefficients: [f64; 6],
    reachable: [f64; 2],
}

#[derive(Clone, Copy)]
enum Level {
    Panel,
    Track,
    Thinking0,
    Thinking1,
    Thinking2,
    Thinking3,
    Thinking4,
    Thinking5,
    Thinking6,
    Subtle,
    Thumb,
    Readable,
    Emphasis,
    TextOnPanel,
    Text,
}

fn level_curve(level: Level, appearance: ThemeAppearance) -> Curve {
    use Level::*;
    use ThemeAppearance::*;
    match (level, appearance) {
        (Panel, Dark) => Curve {
            coefficients: [0.29131, -0.39746, 2.33185, -0.85524, -1.2076, 0.86276],
            reachable: [0.0, 0.979],
        },
        (Panel, Light) => Curve {
            coefficients: [-3.74073, 27.94549, -78.44258, 112.6798, -79.60015, 22.11277],
            reachable: [0.348, 1.0],
        },
        (Track, Dark) => Curve {
            coefficients: [0.39028, -0.23015, 0.83573, 2.43829, -4.38292, 2.01582],
            reachable: [0.0, 0.946],
        },
        (Track, Light) => Curve {
            coefficients: [-5.24921, 38.37322, -107.28833, 152.10005, -106.17127, 29.18061],
            reachable: [0.368, 1.0],
        },
        (Thinking0, Dark) => Curve {
            coefficients: [0.52988, -0.05809, -0.30924, 4.63567, -6.52933, 2.89108],
            reachable: [0.0, 0.873],
        },
        (Thinking0, Light) => Curve {
            coefficients: [-28.27749, 182.85284, -469.62416, 603.15916, -384.59976, 97.35147],
            reachable: [0.51, 1.0],
        },
        (Thinking1, Dark) => Curve {
            coefficients: [0.55278, -0.03667, -0.45659, 4.95347, -6.90265, 3.0706],
            reachable: [0.0, 0.858],
        },
        (Thinking1, Light) => Curve {
            coefficients: [-37.10484, 235.86282, -596.62344, 754.3633, -474.00763, 118.3551],
            reachable: [0.535, 1.0],
        },
        (Thinking2, Dark) => Curve {
            coefficients: [0.57486, -0.01765, -0.58987, 5.25227, -7.27175, 3.25532],
            reachable: [0.0, 0.842],
        },
        (Thinking2, Light) => Curve {
            coefficients: [-59.89653, 377.05024, -945.07843, 1182.03145, -734.96375, 181.68658],
            reachable: [0.556, 1.0],
        },
        (Thinking3, Dark) => Curve {
            coefficients: [0.59621, -0.00062, -0.71148, 5.53588, -7.6392, 3.44606],
            reachable: [0.0, 0.827],
        },
        (Thinking3, Light) => Curve {
            coefficients: [-72.07122, 445.84082, -1099.57352, 1353.88793, -829.53392, 202.26164],
            reachable: [0.58, 1.0],
        },
        (Thinking4, Dark) => Curve {
            coefficients: [0.61691, 0.01462, -0.82288, 5.80651, -8.00641, 3.64333],
            reachable: [0.0, 0.811],
        },
        (Thinking4, Light) => Curve {
            coefficients: [-110.14338, 674.21488, -1645.75941, 2004.32367, -1215.15899, 293.3183],
            reachable: [0.6, 1.0],
        },
        (Thinking5, Dark) => Curve {
            coefficients: [0.63702, 0.02826, -0.92498, 6.06465, -8.37246, 3.84651],
            reachable: [0.0, 0.795],
        },
        (Thinking5, Light) => Curve {
            coefficients: [-175.47701, 1063.54495, -2570.70594, 3098.80776, -1860.15527, 444.76392],
            reachable: [0.62, 1.0],
        },
        (Thinking6, Dark) => Curve {
            coefficients: [0.65658, 0.04044, -1.01835, 6.30989, -8.73529, 4.05439],
            reachable: [0.0, 0.779],
        },
        (Thinking6, Light) => Curve {
            coefficients: [-183.81712, 1094.70055, -2602.68539, 3088.71276, -1826.91131, 430.75931],
            reachable: [0.643, 1.0],
        },
        (Subtle, Dark) => Curve {
            coefficients: [0.56762, -0.02475, -0.5383, 5.12628, -7.10931, 3.17324],
            reachable: [0.0, 0.848],
        },
        (Subtle, Light) => Curve {
            coefficients: [-232.85459, 1376.54473, -3249.11801, 3827.91186, -2248.29472, 526.55751],
            reachable: [0.657, 1.0],
        },
        (Thumb, Dark) => Curve {
            coefficients: [0.60323, 0.00278, -0.73328, 5.57157, -7.68067, 3.46933],
            reachable: [0.0, 0.823],
        },
        (Thumb, Light) => Curve {
            coefficients: [-82.89897, 511.01355, -1255.98095, 1540.76821, -940.68087, 228.58523],
            reachable: [0.586, 1.0],
        },
        (Readable, Dark) => Curve {
            coefficients: [0.66937, 0.04704, -1.06871, 6.43941, -8.9332, 4.17229],
            reachable: [0.0, 0.77],
        },
        (Readable, Light) => Curve {
            coefficients: [
                -1554.52576,
                8733.56817,
                -19604.93507,
                21977.72696,
                -12300.99599,
                2749.81288,
            ],
            reachable: [0.751, 1.0],
        },
        (Emphasis, Dark) => Curve {
            coefficients: [0.7303, 0.07695, -1.31626, 7.1681, -10.14436, 4.92846],
            reachable: [0.0, 0.712],
        },
        (Emphasis, Light) => Curve {
            coefficients: [
                -4948.31942,
                26870.91986,
                -58334.48399,
                63280.17197,
                -34298.01053,
                7430.30146,
            ],
            reachable: [0.811, 1.0],
        },
        (TextOnPanel, Dark) => Curve {
            coefficients: [0.86713, 0.05232, -0.89428, 4.79014, -5.5432, 1.75023],
            reachable: [0.0, 0.542],
        },
        (TextOnPanel, Light) => Curve {
            coefficients: [
                -8570.89457,
                43954.60805,
                -90084.00702,
                92220.6791,
                -47152.15802,
                9632.27113,
            ],
            reachable: [0.867, 1.0],
        },
        (Text, Dark) => Curve {
            coefficients: [0.89242, 0.02311, -0.44862, 2.34417, -0.06084, -2.63844],
            reachable: [0.0, 0.5],
        },
        (Text, Light) => Curve {
            coefficients: [
                -2004.67048,
                6664.47299,
                -6060.70202,
                -1792.61209,
                5133.82359,
                -1939.85583,
            ],
            reachable: [0.894, 1.0],
        },
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Surface {
    Background,
    Token(ThemeToken),
}

struct Rule {
    token: ThemeToken,
    on: Vec<Surface>,
    level: Level,
}

const PANELS: &[ThemeBg] = &[
    ThemeBg::UserMessageBg,
    ThemeBg::ToolPendingBg,
    ThemeBg::ToolSuccessBg,
    ThemeBg::ToolErrorBg,
    ThemeBg::SelectedBg,
    ThemeBg::SearchMatchBg,
    ThemeBg::CustomMessageBg,
];

const FOREGROUND_TOKENS: &[ThemeColor] = &[ThemeColor::Text, ThemeColor::UserMessageText, ThemeColor::ToolTitle];

const TEXT_MINIMUM_WCAG_CONTRAST: f64 = 4.5;
const FOREGROUND_LEVEL: Level = Level::Emphasis;

fn build_rules() -> Vec<Rule> {
    use Surface::*;
    use ThemeBg::*;
    use ThemeColor::*;
    let tool_panels = vec![
        Token(ThemeToken::Bg(ToolPendingBg)),
        Token(ThemeToken::Bg(ToolSuccessBg)),
        Token(ThemeToken::Bg(ToolErrorBg)),
    ];
    let msg_panels = vec![
        Token(ThemeToken::Bg(UserMessageBg)),
        Token(ThemeToken::Bg(CustomMessageBg)),
    ];

    let mut rules = Vec::new();
    for &p in PANELS {
        rules.push(Rule {
            token: ThemeToken::Bg(p),
            on: vec![Background],
            level: Level::Panel,
        });
    }
    rules.push(Rule {
        token: ThemeToken::Fg(Text),
        on: vec![Background],
        level: Level::Text,
    });
    rules.push(Rule {
        token: ThemeToken::Fg(Text),
        on: vec![Token(ThemeToken::Bg(SelectedBg))],
        level: Level::TextOnPanel,
    });
    rules.push(Rule {
        token: ThemeToken::Fg(UserMessageText),
        on: vec![Token(ThemeToken::Bg(UserMessageBg))],
        level: Level::TextOnPanel,
    });
    rules.push(Rule {
        token: ThemeToken::Fg(ToolTitle),
        on: tool_panels.clone(),
        level: Level::TextOnPanel,
    });

    let mut bg_sel_tools = vec![Background, Token(ThemeToken::Bg(SelectedBg))];
    bg_sel_tools.extend_from_slice(&tool_panels);
    for t in [Accent, Success, Error, Warning] {
        rules.push(Rule {
            token: ThemeToken::Fg(t),
            on: bg_sel_tools.clone(),
            level: Level::Readable,
        });
    }

    let mut bg_sel_custom_tools = vec![
        Background,
        Token(ThemeToken::Bg(SelectedBg)),
        Token(ThemeToken::Bg(CustomMessageBg)),
    ];
    bg_sel_custom_tools.extend_from_slice(&tool_panels);
    rules.push(Rule {
        token: ThemeToken::Fg(Muted),
        on: bg_sel_custom_tools.clone(),
        level: Level::Readable,
    });
    rules.push(Rule {
        token: ThemeToken::Fg(Dim),
        on: bg_sel_custom_tools.clone(),
        level: Level::Subtle,
    });
    rules.push(Rule {
        token: ThemeToken::Fg(ThinkingText),
        on: vec![Background],
        level: Level::Readable,
    });

    let mut custom_and_tools = vec![Token(ThemeToken::Bg(CustomMessageBg))];
    custom_and_tools.extend_from_slice(&tool_panels);
    rules.push(Rule {
        token: ThemeToken::Fg(CustomMessageText),
        on: custom_and_tools,
        level: Level::Readable,
    });

    let mut bg_custom_sel_tools = vec![
        Background,
        Token(ThemeToken::Bg(CustomMessageBg)),
        Token(ThemeToken::Bg(SelectedBg)),
    ];
    bg_custom_sel_tools.extend_from_slice(&tool_panels);
    rules.push(Rule {
        token: ThemeToken::Fg(CustomMessageLabel),
        on: bg_custom_sel_tools,
        level: Level::Readable,
    });

    let mut bg_and_tools = vec![Background];
    bg_and_tools.extend_from_slice(&tool_panels);
    rules.push(Rule {
        token: ThemeToken::Fg(ToolOutput),
        on: bg_and_tools,
        level: Level::Readable,
    });

    let mut bg_and_msgs = vec![Background];
    bg_and_msgs.extend_from_slice(&msg_panels);
    for t in [
        MdHeading,
        MdLink,
        MdLinkUrl,
        MdCode,
        MdQuote,
        MdCodeBlockBorder,
        MdListBullet,
    ] {
        rules.push(Rule {
            token: ThemeToken::Fg(t),
            on: bg_and_msgs.clone(),
            level: Level::Readable,
        });
    }

    let mut bg_msgs_tools = vec![Background];
    bg_msgs_tools.extend_from_slice(&msg_panels);
    bg_msgs_tools.extend_from_slice(&tool_panels);
    rules.push(Rule {
        token: ThemeToken::Fg(MdCodeBlock),
        on: bg_msgs_tools.clone(),
        level: Level::Readable,
    });

    for t in [ToolDiffAdded, ToolDiffRemoved, ToolDiffContext] {
        rules.push(Rule {
            token: ThemeToken::Fg(t),
            on: tool_panels.clone(),
            level: Level::Readable,
        });
    }

    for t in [
        SyntaxComment,
        SyntaxKeyword,
        SyntaxFunction,
        SyntaxVariable,
        SyntaxString,
        SyntaxNumber,
        SyntaxType,
        SyntaxOperator,
        SyntaxPunctuation,
    ] {
        rules.push(Rule {
            token: ThemeToken::Fg(t),
            on: bg_and_msgs.clone(),
            level: Level::Readable,
        });
    }

    rules.push(Rule {
        token: ThemeToken::Fg(SearchMatchText),
        on: vec![Token(ThemeToken::Bg(SearchMatchBg))],
        level: Level::Readable,
    });
    for t in [BashMode, Border, BorderAccent] {
        rules.push(Rule {
            token: ThemeToken::Fg(t),
            on: vec![Background],
            level: Level::Readable,
        });
    }
    rules.push(Rule {
        token: ThemeToken::Fg(BorderMuted),
        on: vec![Background],
        level: Level::Subtle,
    });
    for t in [MdQuoteBorder, MdHr] {
        rules.push(Rule {
            token: ThemeToken::Fg(t),
            on: bg_msgs_tools.clone(),
            level: Level::Readable,
        });
    }
    rules.push(Rule {
        token: ThemeToken::Fg(ScrollbarTrack),
        on: vec![Background],
        level: Level::Track,
    });
    rules.push(Rule {
        token: ThemeToken::Fg(ScrollbarThumb),
        on: vec![Token(ThemeToken::Fg(ScrollbarTrack))],
        level: Level::Thumb,
    });

    let thinking = [
        (ThinkingOff, Level::Thinking0),
        (ThinkingMinimal, Level::Thinking1),
        (ThinkingLow, Level::Thinking2),
        (ThinkingMedium, Level::Thinking3),
        (ThinkingHigh, Level::Thinking4),
        (ThinkingXhigh, Level::Thinking5),
        (ThinkingMax, Level::Thinking6),
    ];
    for (tok, lvl) in thinking {
        rules.push(Rule {
            token: ThemeToken::Fg(tok),
            on: vec![Background],
            level: lvl,
        });
    }
    rules
}

static RULES: LazyLock<Vec<Rule>> = LazyLock::new(build_rules);

static SOLVE_ORDER: LazyLock<Vec<ThemeToken>> = LazyLock::new(|| {
    fn visit(token: ThemeToken, order: &mut Vec<ThemeToken>) {
        if order.contains(&token) {
            return;
        }
        for rule in RULES.iter() {
            if rule.token != token {
                continue;
            }
            for &surface in &rule.on {
                if let Surface::Token(dep) = surface {
                    visit(dep, order);
                }
            }
        }
        order.push(token);
    }
    let mut order = Vec::new();
    for rule in RULES.iter() {
        visit(rule.token, &mut order);
    }
    order
});

#[derive(Clone, Debug, Default)]
pub struct SystemThemeInput {
    pub foreground: Option<RgbColor>,
    pub background: Option<RgbColor>,
    /// ANSI colors 0-15.
    pub palette: Option<Vec<RgbColor>>,
    /// Saturation multiplier from 0 (grayscale) to 1. The first frame renders in grayscale until colors arrive.
    pub saturation: Option<f64>,
    /// Appearance when the terminal did not report its background.
    pub appearance_hint: Option<ThemeAppearance>,
}

#[derive(Clone, Debug)]
pub struct SystemThemeColors {
    /// Hex colors, ANSI palette indices, or `""` for the terminal default.
    pub colors: IndexMap<ThemeToken, ThemeColorValue>,
    /// Foreground tokens rendered faint (SGR 2), for terminals that did not report colors.
    pub dim: Vec<ThemeColor>,
    pub appearance: Option<ThemeAppearance>,
}

fn to_color_rgb(c: RgbColor) -> Color {
    match rgb_color(c.r, c.g, c.b) {
        Ok(v) => Color::Rgb(v),
        Err(_) => Color::Rgb(pi_tui::RgbColorValue {
            r: c.r.clamp(0.0, 255.0),
            g: c.g.clamp(0.0, 255.0),
            b: c.b.clamp(0.0, 255.0),
        }),
    }
}

/// OKLab lightness of an sRGB color, 0-1.
fn oklab_lightness(color: RgbColor) -> f64 {
    color_to_oklch(to_color_rgb(color)).l
}

/// WCAG 2 relative luminance.
pub fn relative_luminance(color: RgbColor) -> f64 {
    let linear = |channel: f64| {
        let value = channel / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}

/// WCAG 2 contrast ratio, 1-21.
pub fn wcag_contrast(first: RgbColor, second: RgbColor) -> f64 {
    let a = relative_luminance(first);
    let b = relative_luminance(second);
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// Whether a terminal is dark or light, from its reported colors.
pub fn terminal_appearance(background: RgbColor, foreground: Option<RgbColor>) -> ThemeAppearance {
    let white = RgbColor {
        r: 255.0,
        g: 255.0,
        b: 255.0,
    };
    let black = RgbColor { r: 0.0, g: 0.0, b: 0.0 };
    let white_contrast = wcag_contrast(white, background);
    let black_contrast = wcag_contrast(black, background);
    if let Some(fg) = foreground {
        let foreground_l = oklab_lightness(fg);
        let background_l = oklab_lightness(background);
        if (foreground_l - background_l).abs() > 0.05 {
            let appearance = if foreground_l > background_l {
                ThemeAppearance::Dark
            } else {
                ThemeAppearance::Light
            };
            let best = if appearance == ThemeAppearance::Dark {
                white_contrast
            } else {
                black_contrast
            };
            if best >= TEXT_MINIMUM_WCAG_CONTRAST {
                return appearance;
            }
        }
    }
    if white_contrast >= black_contrast {
        ThemeAppearance::Dark
    } else {
        ThemeAppearance::Light
    }
}

fn hex_of(c: RgbColor) -> String {
    let ch = |v: f64| format!("{:02x}", (v.round().clamp(0.0, 255.0)) as u8);
    format!("#{}{}{}", ch(c.r), ch(c.g), ch(c.b))
}

fn bell_weight(lightness: f64) -> f64 {
    let gaussian = |x: f64| (-((x - 0.5).powi(2)) / (2.0 * 0.25_f64.powi(2))).exp();
    (gaussian(lightness) - gaussian(0.0)) / (1.0 - gaussian(0.0))
}

fn saturation_curve(family: Family, lightness: f64) -> f64 {
    let floor = if family.saturation_max > 0.0 {
        family.saturation_min / family.saturation_max
    } else {
        1.0
    };
    floor + (1.0 - floor) * bell_weight(lightness)
}

fn level_target(level: Level, appearance: ThemeAppearance, surface_l: f64) -> Option<f64> {
    let curve = level_curve(level, appearance);
    if surface_l < curve.reachable[0] || surface_l > curve.reachable[1] {
        return None;
    }
    let mut sum = 0.0;
    let mut pow = 1.0;
    for &coeff in &curve.coefficients {
        sum += coeff * pow;
        pow *= surface_l;
    }
    Some(sum)
}

#[derive(Clone, Copy)]
struct SourceColor {
    h: f64,
    s: f64,
    l: f64,
    chroma: f64,
}

fn okhsl_of(c: RgbColor) -> OkhslChannels {
    color_to_okhsl(to_color_rgb(c))
}

fn source_of(c: RgbColor) -> SourceColor {
    let ch = okhsl_of(c);
    let chroma = color_to_oklch(to_color_rgb(c)).c;
    SourceColor {
        h: ch.h,
        s: ch.s,
        l: ch.l,
        chroma,
    }
}

fn safe_okhsl_rgb(h: f64, s: f64, l: f64) -> RgbColor {
    let sc = s.clamp(0.0, 1.0);
    let lc = l.clamp(0.0, 1.0);
    match okhsl_color(h, sc, lc) {
        Ok(v) => RgbColor { r: v.r, g: v.g, b: v.b },
        Err(_) => RgbColor { r: 0.0, g: 0.0, b: 0.0 },
    }
}

fn anchored(source: SourceColor, family: Family, lightness: f64, saturation: f64) -> RgbColor {
    let anchor = saturation_curve(family, source.l);
    let falloff = if anchor > 0.0 {
        (saturation_curve(family, lightness) / anchor).min(1.0)
    } else {
        1.0
    };
    let color = safe_okhsl_rgb(source.h, source.s * falloff * saturation, lightness);
    let cap = source.chroma * falloff * saturation;
    let oklch = color_to_oklch(to_color_rgb(color));
    if oklch.c <= cap {
        color
    } else {
        match oklch_color(oklch.l, cap, source.h) {
            Ok(v) => color_to_rgb(Color::Oklch(v)),
            Err(_) => color,
        }
    }
}

fn with_text_contrast(color: RgbColor, surfaces: &[RgbColor], lighter: bool) -> RgbColor {
    let meets = |candidate: RgbColor| {
        surfaces
            .iter()
            .all(|&surface| wcag_contrast(candidate, surface) >= TEXT_MINIMUM_WCAG_CONTRAST)
    };
    if meets(color) {
        return color;
    }
    let ch = okhsl_of(color);
    let at = |lightness: f64| safe_okhsl_rgb(ch.h, ch.s, lightness);
    let extreme = if lighter { 1.0 } else { 0.0 };
    if !meets(at(extreme)) {
        return at(extreme);
    }
    let mut low = ch.l;
    let mut high = extreme;
    for _ in 0..20 {
        let middle = (low + high) / 2.0;
        if meets(at(middle)) {
            high = middle;
        } else {
            low = middle;
        }
    }
    at(high)
}

fn indexed_colors(saturation: f64, appearance: Option<ThemeAppearance>) -> SystemThemeColors {
    let mut colors = IndexMap::new();
    let mut dim = Vec::new();
    for &token in ThemeToken::all() {
        if let ThemeToken::Bg(_) = token {
            colors.insert(token, ThemeColorValue::String(String::new()));
            continue;
        }
        let fam = token_family(token);
        let neutral = fam == FamilyName::Neutral;
        let val = if !neutral && saturation > 0.0 {
            let slot = token_slot_override(token).unwrap_or_else(|| family_spec(fam).slot);
            ThemeColorValue::Indexed(slot as i64)
        } else {
            ThemeColorValue::String(String::new())
        };
        colors.insert(token, val);
        if let ThemeToken::Fg(fg_tok) = token
            && neutral
            && !FOREGROUND_TOKENS.contains(&fg_tok)
        {
            dim.push(fg_tok);
        }
    }
    SystemThemeColors {
        colors,
        dim,
        appearance,
    }
}

/// Generate the system theme's colors from the terminal's reported colors.
pub fn generate_system_theme_colors(input: &SystemThemeInput) -> SystemThemeColors {
    let saturation = input.saturation.unwrap_or(1.0).clamp(0.0, 1.0);
    let Some(background) = input.background else {
        return indexed_colors(saturation, input.appearance_hint);
    };
    let palette: Option<Vec<SourceColor>> = input
        .palette
        .as_ref()
        .filter(|p| p.len() == 16)
        .map(|p| p.iter().copied().map(source_of).collect());

    let appearance = terminal_appearance(background, input.foreground);
    let lighter = appearance == ThemeAppearance::Dark;
    let extreme = if lighter { 1.0 } else { 0.0 };
    let background_l = oklab_lightness(background);

    let paint = |token: ThemeToken, oklab_l: f64| -> RgbColor {
        let lightness = oklab_to_okhsl_lightness(oklab_l);
        let fam_name = token_family(token);
        let family = family_spec(fam_name);
        match &palette {
            None => {
                let s = (family.saturation_min
                    + (family.saturation_max - family.saturation_min) * bell_weight(lightness))
                    * saturation;
                safe_okhsl_rgb(family.hue, s, lightness)
            }
            Some(pal) => {
                let slot = token_slot_override(token).unwrap_or(family.slot);
                anchored(pal[slot], family, lightness, saturation)
            }
        }
    };

    let readable_floor = match appearance {
        ThemeAppearance::Dark => Level::Readable,
        ThemeAppearance::Light => Level::Subtle,
    };

    let target = |level: Level, surface_l: f64, t: f64| -> Option<f64> {
        let reached = level_target(level, appearance, surface_l);
        if reached.is_none() && t == 0.0 {
            return None;
        }
        let distance = reached.unwrap_or(extreme) - surface_l;
        let floor = level_target(readable_floor, appearance, surface_l).unwrap_or(extreme) - surface_l;
        let compressed = if distance.abs() > floor.abs() {
            distance - (distance - floor) * t.min(1.0)
        } else {
            distance
        };
        Some(surface_l + compressed * (1.0 - (t - 1.0).max(0.0)))
    };

    let extreme_text = if lighter {
        RgbColor {
            r: 255.0,
            g: 255.0,
            b: 255.0,
        }
    } else {
        RgbColor { r: 0.0, g: 0.0, b: 0.0 }
    };
    let readable = |color: RgbColor| wcag_contrast(extreme_text, color) >= TEXT_MINIMUM_WCAG_CONTRAST;
    let limit_panel = |token: ThemeToken, l: f64| -> RgbColor {
        let color = paint(token, l);
        if readable(color) {
            return color;
        }
        let mut low = background_l;
        let mut high = l;
        for _ in 0..20 {
            let middle = (low + high) / 2.0;
            if readable(paint(token, middle)) {
                low = middle;
            } else {
                high = middle;
            }
        }
        paint(token, low)
    };

    let solve = |t: f64| -> Option<IndexMap<Surface, RgbColor>> {
        let mut colors: IndexMap<Surface, RgbColor> = IndexMap::new();
        colors.insert(Surface::Background, background);
        for &token in SOLVE_ORDER.iter() {
            let mut targets = Vec::new();
            for rule in RULES.iter() {
                if rule.token != token {
                    continue;
                }
                for &surface in &rule.on {
                    let surf_rgb = colors.get(&surface).copied().unwrap_or(background);
                    let value = target(rule.level, oklab_lightness(surf_rgb), t)?;
                    if !(0.0..=1.0).contains(&value) {
                        return None;
                    }
                    targets.push(value);
                }
            }
            let l = if lighter {
                targets.into_iter().fold(f64::NEG_INFINITY, f64::max)
            } else {
                targets.into_iter().fold(f64::INFINITY, f64::min)
            };
            let rgb = match token {
                ThemeToken::Bg(bg) if PANELS.contains(&bg) => limit_panel(token, l),
                _ => paint(token, l),
            };
            colors.insert(Surface::Token(token), rgb);
        }
        Some(colors)
    };

    let mut relaxation = 0.0;
    let mut colors = solve(0.0);
    if colors.is_none() {
        let mut low = 0.0;
        let mut high = 2.0;
        colors = solve(high);
        for _ in 0..20 {
            let middle = (low + high) / 2.0;
            if let Some(attempt) = solve(middle) {
                high = middle;
                colors = Some(attempt);
            } else {
                low = middle;
            }
        }
        relaxation = high;
    }

    let solved = colors.unwrap_or_default();
    let surfaces_of = |token: ThemeToken| -> Vec<RgbColor> {
        let mut out = Vec::new();
        for rule in RULES.iter() {
            if rule.token != token {
                continue;
            }
            for &surface in &rule.on {
                out.push(solved.get(&surface).copied().unwrap_or(background));
            }
        }
        out
    };

    let mut result: IndexMap<ThemeToken, ThemeColorValue> = IndexMap::new();
    for &token in ThemeToken::all() {
        let hex = solved
            .get(&Surface::Token(token))
            .map(|&c| hex_of(c))
            .unwrap_or_default();
        result.insert(token, ThemeColorValue::String(hex));
    }

    for &fg_tok in FOREGROUND_TOKENS {
        let token = ThemeToken::Fg(fg_tok);
        let surfaces = surfaces_of(token);
        let mut text_rgb = solved.get(&Surface::Token(token)).copied();
        if let Some(foreground) = input.foreground {
            let mut targets = Vec::with_capacity(surfaces.len());
            let mut all_valid = true;
            for &surf in &surfaces {
                match target(FOREGROUND_LEVEL, oklab_lightness(surf), relaxation) {
                    Some(v) if (0.0..=1.0).contains(&v) => targets.push(v),
                    _ => {
                        all_valid = false;
                        break;
                    }
                }
            }
            if all_valid && !targets.is_empty() {
                let needed = if lighter {
                    targets.into_iter().fold(f64::NEG_INFINITY, f64::max)
                } else {
                    targets.into_iter().fold(f64::INFINITY, f64::min)
                };
                let foreground_l = oklab_lightness(foreground);
                if if lighter {
                    foreground_l >= needed
                } else {
                    foreground_l <= needed
                } {
                    result.insert(token, ThemeColorValue::String(String::new()));
                    continue;
                }
                text_rgb = Some(anchored(
                    source_of(foreground),
                    family_spec(FamilyName::Neutral),
                    oklab_to_okhsl_lightness(needed),
                    saturation,
                ));
            }
        }
        if let Some(text) = text_rgb {
            result.insert(
                token,
                ThemeColorValue::String(hex_of(with_text_contrast(text, &surfaces, lighter))),
            );
        }
    }

    SystemThemeColors {
        colors: result,
        dim: Vec::new(),
        appearance: Some(appearance),
    }
}
