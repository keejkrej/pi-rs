//! Port of packages/coding-agent/src/modes/interactive/theme/theme-tokens.ts

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeTokenSlot {
    Foreground,
    Background,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeTokenDescriptor {
    pub name: &'static str,
    pub slot: ThemeTokenSlot,
    pub description: &'static str,
    pub fallback: Option<&'static str>,
}

const fn define_theme_tokens<const N: usize>(tokens: [ThemeTokenDescriptor; N]) -> [ThemeTokenDescriptor; N] {
    let mut i = 0;
    while i < N {
        if let Some(fb_name) = tokens[i].fallback {
            let mut j = 0;
            let mut found = false;
            while j < N {
                if const_str_eq(tokens[j].name, fb_name) {
                    assert!(
                        matches!(
                            (tokens[i].slot, tokens[j].slot),
                            (ThemeTokenSlot::Foreground, ThemeTokenSlot::Foreground)
                                | (ThemeTokenSlot::Background, ThemeTokenSlot::Background)
                        ),
                        "Theme token fallback is in a different color slot"
                    );
                    found = true;
                    break;
                }
                j += 1;
            }
            assert!(found, "Theme token has unknown fallback");
        }
        i += 1;
    }
    tokens
}

const fn const_str_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Theme token metadata in the same order used by theme files and the published JSON Schema.
pub const THEME_TOKENS: &[ThemeTokenDescriptor] = &define_theme_tokens([
    ThemeTokenDescriptor {
        name: "accent",
        slot: ThemeTokenSlot::Foreground,
        description: "Primary accent color (logo, selected items, cursor)",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "border",
        slot: ThemeTokenSlot::Foreground,
        description: "Normal borders",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "borderAccent",
        slot: ThemeTokenSlot::Foreground,
        description: "Highlighted borders",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "borderMuted",
        slot: ThemeTokenSlot::Foreground,
        description: "Subtle borders",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "success",
        slot: ThemeTokenSlot::Foreground,
        description: "Success states",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "error",
        slot: ThemeTokenSlot::Foreground,
        description: "Error states",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "warning",
        slot: ThemeTokenSlot::Foreground,
        description: "Warning states",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "muted",
        slot: ThemeTokenSlot::Foreground,
        description: "Secondary/dimmed text",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "dim",
        slot: ThemeTokenSlot::Foreground,
        description: "Very dimmed text (more subtle than muted)",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "text",
        slot: ThemeTokenSlot::Foreground,
        description: "Default text color (usually empty string)",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "thinkingText",
        slot: ThemeTokenSlot::Foreground,
        description: "Thinking block text color",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "scrollbarTrack",
        slot: ThemeTokenSlot::Foreground,
        description: "Fullscreen scrollbar track foreground (falls back to muted when omitted)",
        fallback: Some("muted"),
    },
    ThemeTokenDescriptor {
        name: "scrollbarThumb",
        slot: ThemeTokenSlot::Foreground,
        description: "Fullscreen scrollbar thumb foreground (falls back to text when omitted)",
        fallback: Some("text"),
    },
    ThemeTokenDescriptor {
        name: "selectedBg",
        slot: ThemeTokenSlot::Background,
        description: "Selected item background",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "searchMatchBg",
        slot: ThemeTokenSlot::Background,
        description: "Transcript search match background and current-match text (falls back to selectedBg when omitted)",
        fallback: Some("selectedBg"),
    },
    ThemeTokenDescriptor {
        name: "searchMatchText",
        slot: ThemeTokenSlot::Foreground,
        description: "Transcript search match text and current-match background (falls back to text when omitted)",
        fallback: Some("text"),
    },
    ThemeTokenDescriptor {
        name: "userMessageBg",
        slot: ThemeTokenSlot::Background,
        description: "User message background",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "userMessageText",
        slot: ThemeTokenSlot::Foreground,
        description: "User message text color",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "customMessageBg",
        slot: ThemeTokenSlot::Background,
        description: "Custom message background (hook-injected messages)",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "customMessageText",
        slot: ThemeTokenSlot::Foreground,
        description: "Custom message text color",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "customMessageLabel",
        slot: ThemeTokenSlot::Foreground,
        description: "Custom message type label color",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "toolPendingBg",
        slot: ThemeTokenSlot::Background,
        description: "Tool execution box (pending state)",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "toolSuccessBg",
        slot: ThemeTokenSlot::Background,
        description: "Tool execution box (success state)",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "toolErrorBg",
        slot: ThemeTokenSlot::Background,
        description: "Tool execution box (error state)",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "toolTitle",
        slot: ThemeTokenSlot::Foreground,
        description: "Tool execution box title color",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "toolOutput",
        slot: ThemeTokenSlot::Foreground,
        description: "Tool execution box output text color",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "mdHeading",
        slot: ThemeTokenSlot::Foreground,
        description: "Markdown heading text",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "mdLink",
        slot: ThemeTokenSlot::Foreground,
        description: "Markdown link text",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "mdLinkUrl",
        slot: ThemeTokenSlot::Foreground,
        description: "Markdown link URL",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "mdCode",
        slot: ThemeTokenSlot::Foreground,
        description: "Markdown inline code",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "mdCodeBlock",
        slot: ThemeTokenSlot::Foreground,
        description: "Markdown code block content",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "mdCodeBlockBorder",
        slot: ThemeTokenSlot::Foreground,
        description: "Markdown code block fences",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "mdQuote",
        slot: ThemeTokenSlot::Foreground,
        description: "Markdown blockquote text",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "mdQuoteBorder",
        slot: ThemeTokenSlot::Foreground,
        description: "Markdown blockquote border",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "mdHr",
        slot: ThemeTokenSlot::Foreground,
        description: "Markdown horizontal rule",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "mdListBullet",
        slot: ThemeTokenSlot::Foreground,
        description: "Markdown list bullets/numbers",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "toolDiffAdded",
        slot: ThemeTokenSlot::Foreground,
        description: "Added lines in tool diffs",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "toolDiffRemoved",
        slot: ThemeTokenSlot::Foreground,
        description: "Removed lines in tool diffs",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "toolDiffContext",
        slot: ThemeTokenSlot::Foreground,
        description: "Context lines in tool diffs",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "syntaxComment",
        slot: ThemeTokenSlot::Foreground,
        description: "Syntax highlighting: comments",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "syntaxKeyword",
        slot: ThemeTokenSlot::Foreground,
        description: "Syntax highlighting: keywords",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "syntaxFunction",
        slot: ThemeTokenSlot::Foreground,
        description: "Syntax highlighting: function names",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "syntaxVariable",
        slot: ThemeTokenSlot::Foreground,
        description: "Syntax highlighting: variable names",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "syntaxString",
        slot: ThemeTokenSlot::Foreground,
        description: "Syntax highlighting: string literals",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "syntaxNumber",
        slot: ThemeTokenSlot::Foreground,
        description: "Syntax highlighting: number literals",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "syntaxType",
        slot: ThemeTokenSlot::Foreground,
        description: "Syntax highlighting: type names",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "syntaxOperator",
        slot: ThemeTokenSlot::Foreground,
        description: "Syntax highlighting: operators",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "syntaxPunctuation",
        slot: ThemeTokenSlot::Foreground,
        description: "Syntax highlighting: punctuation",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "thinkingOff",
        slot: ThemeTokenSlot::Foreground,
        description: "Thinking level border: off",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "thinkingMinimal",
        slot: ThemeTokenSlot::Foreground,
        description: "Thinking level border: minimal",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "thinkingLow",
        slot: ThemeTokenSlot::Foreground,
        description: "Thinking level border: low",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "thinkingMedium",
        slot: ThemeTokenSlot::Foreground,
        description: "Thinking level border: medium",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "thinkingHigh",
        slot: ThemeTokenSlot::Foreground,
        description: "Thinking level border: high",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "thinkingXhigh",
        slot: ThemeTokenSlot::Foreground,
        description: "Thinking level border: xhigh",
        fallback: None,
    },
    ThemeTokenDescriptor {
        name: "thinkingMax",
        slot: ThemeTokenSlot::Foreground,
        description: "Thinking level border: max (falls back to thinkingXhigh when omitted)",
        fallback: Some("thinkingXhigh"),
    },
    ThemeTokenDescriptor {
        name: "bashMode",
        slot: ThemeTokenSlot::Foreground,
        description: "Editor border color in bash mode",
        fallback: None,
    },
]);

pub fn theme_token_descriptor(name: &str) -> Option<&'static ThemeTokenDescriptor> {
    THEME_TOKENS.iter().find(|d| d.name == name)
}

pub const REQUIRED_THEME_COLOR_TOKENS: &[&str] = &[
    "accent",
    "border",
    "borderAccent",
    "borderMuted",
    "success",
    "error",
    "warning",
    "muted",
    "dim",
    "text",
    "thinkingText",
    "selectedBg",
    "userMessageBg",
    "userMessageText",
    "customMessageBg",
    "customMessageText",
    "customMessageLabel",
    "toolPendingBg",
    "toolSuccessBg",
    "toolErrorBg",
    "toolTitle",
    "toolOutput",
    "mdHeading",
    "mdLink",
    "mdLinkUrl",
    "mdCode",
    "mdCodeBlock",
    "mdCodeBlockBorder",
    "mdQuote",
    "mdQuoteBorder",
    "mdHr",
    "mdListBullet",
    "toolDiffAdded",
    "toolDiffRemoved",
    "toolDiffContext",
    "syntaxComment",
    "syntaxKeyword",
    "syntaxFunction",
    "syntaxVariable",
    "syntaxString",
    "syntaxNumber",
    "syntaxType",
    "syntaxOperator",
    "syntaxPunctuation",
    "thinkingOff",
    "thinkingMinimal",
    "thinkingLow",
    "thinkingMedium",
    "thinkingHigh",
    "thinkingXhigh",
    "bashMode",
];

pub const OPTIONAL_THEME_COLOR_TOKENS: &[&str] = &[
    "scrollbarTrack",
    "scrollbarThumb",
    "searchMatchBg",
    "searchMatchText",
    "thinkingMax",
];
