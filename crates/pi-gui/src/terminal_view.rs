//! Integrated workspace terminal panel with ANSI log, preset chips, and Run/Kill/Clear bar.

use rune::{Color, Cx, El, ScrollSpec, Span, col, el, rich, row, text, text_input};
use rune_kit::{Badge, BadgeVariant, Button, ButtonVariant, ControlSize, Kit};

use crate::state::{PiGuiAction, PiGuiState};
use crate::theme::WakuPalette;

const PRESET_COMMANDS: [&str; 4] = ["git status", "git diff --stat", "cargo test", "ls -la"];

/// Renders the workspace terminal header, presets, scrollable ANSI log, and command input bar.
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, _cx: &mut Cx<PiGuiState>) -> El {
    let status_bar = row()
        .key("terminal-header")
        .w_full()
        .px(12.0)
        .py(8.0)
        .gap(8.0)
        .justify_between()
        .center_y()
        .border_sides([0.0, 0.0, 1.0, 0.0], pal.border)
        .child(
            row()
                .grow(1.0)
                .min_w(0.0)
                .gap(6.0)
                .center_y()
                .child(el().size(8.0, 8.0).radius(4.0).fill(if s.terminal.running {
                    pal.accent
                } else if s.terminal.last_exit_code == Some(0) {
                    pal.success
                } else if s.terminal.last_exit_code.is_some() {
                    pal.danger
                } else {
                    pal.text_ghost
                }))
                .child(
                    text(&s.terminal.cwd)
                        .font(k.ty.small)
                        .color(pal.text_secondary)
                        .truncate(),
                ),
        )
        .child(
            Badge::new(if s.terminal.running {
                "Running".to_string()
            } else if let Some(code) = s.terminal.last_exit_code {
                format!("Exit {code}")
            } else {
                "Idle".to_string()
            })
            .variant(if s.terminal.running {
                BadgeVariant::Accent
            } else {
                BadgeVariant::Subtle
            })
            .build(k),
        );

    let mut presets = row().key("terminal-presets").w_full().gap(6.0).flex_wrap();
    for preset in PRESET_COMMANDS {
        presets = presets.child(
            el().key(format!("terminal-preset-{}", preset.replace(' ', "-")))
                .px(8.0)
                .py(3.0)
                .radius(k.radius.sm)
                .fill(pal.raised)
                .border(1.0, pal.border)
                .hover(pal.overlay_strong)
                .on_press(move |st: &mut PiGuiState| {
                    st.dispatch(PiGuiAction::RunTerminalCommand(preset.to_string()), None);
                })
                .child(text(preset).font(k.ty.small).color(pal.text_secondary)),
        );
    }

    let mut output = col()
        .key("terminal-output")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .padding(10.0)
        .gap(2.0)
        .radius(k.radius.sm)
        .fill(pal.terminal)
        .border(1.0, pal.border)
        .scroll(ScrollSpec { stick_bottom: true });

    if s.terminal.lines.is_empty() {
        output = output.child(
            text("Run a command below or select a preset to inspect workspace output.")
                .font(k.ty.mono)
                .color(pal.text_tertiary),
        );
    } else {
        for (idx, line) in s.terminal.lines.iter().enumerate() {
            let base_color = if line.is_stderr { pal.danger } else { pal.text };
            let (plain, spans) = parse_ansi_line(&line.text, pal, base_color);
            let line_el = if spans.is_empty() {
                text(plain).font(k.ty.mono).color(base_color)
            } else {
                rich(plain, spans).font(k.ty.mono).color(base_color)
            };
            output = output.child(el().key(format!("term-line-{idx}")).w_full().child(line_el));
        }
    }

    let prompt = row()
        .key("terminal-command-bar")
        .w_full()
        .gap(6.0)
        .center_y()
        .child(
            row()
                .grow(1.0)
                .min_w(0.0)
                .px(10.0)
                .py(6.0)
                .gap(6.0)
                .center_y()
                .radius(k.radius.sm)
                .fill(pal.inset)
                .border(1.0, pal.border)
                .child(text("$").font(k.ty.mono).color(pal.accent))
                .child(
                    text_input(&s.terminal_edit, k.ty.mono)
                        .single_line()
                        .key("terminal-input")
                        .label("Terminal command")
                        .grow(1.0)
                        .min_w(0.0)
                        .color(pal.text)
                        .placeholder("Run shell command...", pal.text_tertiary)
                        .on_edit(|st: &mut PiGuiState| &mut st.terminal_edit)
                        .on_submit(|st: &mut PiGuiState, tx| {
                            let cmd = st.terminal_edit.text.trim().to_string();
                            if !cmd.is_empty() {
                                st.terminal_edit.clear();
                                st.dispatch(PiGuiAction::RunTerminalCommand(cmd), Some(tx));
                            }
                        }),
                ),
        )
        .child(
            Button::new("Run")
                .variant(ButtonVariant::Submit)
                .control_size(ControlSize::Small)
                .on_press_tx(|st: &mut PiGuiState, tx| {
                    let cmd = st.terminal_edit.text.trim().to_string();
                    if !cmd.is_empty() {
                        st.terminal_edit.clear();
                        st.dispatch(PiGuiAction::RunTerminalCommand(cmd), Some(tx));
                    }
                })
                .build(k)
                .key("terminal-run-btn"),
        )
        .child(
            Button::new("Kill")
                .variant(ButtonVariant::Danger)
                .control_size(ControlSize::Small)
                .disabled(!s.terminal.running)
                .on_press_tx(|st: &mut PiGuiState, tx| {
                    st.terminal.running = false;
                    st.dispatch(PiGuiAction::AbortRun, Some(tx));
                })
                .build(k)
                .key("terminal-kill-btn"),
        )
        .child(
            Button::new("Clear")
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small)
                .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::ClearTerminal, None))
                .build(k)
                .key("terminal-clear-btn"),
        );

    col()
        .key("right_panel.terminal.surface")
        .role("region")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .h_full()
        .gap(4.0)
        .fill(pal.terminal)
        .child(status_bar)
        .child(
            col()
                .grow(1.0)
                .min_h(0.0)
                .w_full()
                .padding(12.0)
                .gap(8.0)
                .child(presets)
                .child(output)
                .child(prompt),
        )
}

fn sgr_color(code: u8, pal: &WakuPalette) -> Option<Color> {
    match code {
        31 | 91 => Some(pal.danger),
        32 | 92 => Some(pal.success),
        33 | 93 => Some(pal.warning),
        34 | 94 | 36 | 96 => Some(pal.gauge),
        35 | 95 => Some(pal.accent),
        90 | 37 => Some(pal.text_secondary),
        97 => Some(pal.text),
        _ => None,
    }
}

fn parse_ansi_line(raw: &str, pal: &WakuPalette, default_color: Color) -> (String, Vec<Span>) {
    let mut plain = String::with_capacity(raw.len());
    let mut spans = Vec::new();
    let mut current_color: Option<Color> = None;
    let mut current_bold = false;
    let mut seg_start = 0usize;
    let mut chars = raw.char_indices().peekable();

    while let Some((_, ch)) = chars.next() {
        if ch == '\x1b' && chars.peek().is_some_and(|(_, next)| *next == '[') {
            chars.next();
            let mut params = String::new();
            let mut terminated = false;
            for (_, seq_ch) in chars.by_ref() {
                if seq_ch == 'm' {
                    terminated = true;
                    break;
                }
                if seq_ch.is_ascii_alphabetic() {
                    break;
                }
                params.push(seq_ch);
            }
            if terminated {
                if plain.len() > seg_start && (current_color.is_some() || current_bold) {
                    let mut sp = Span::new(seg_start, plain.len());
                    sp.color = Some(current_color.unwrap_or(default_color));
                    if current_bold {
                        sp.weight = Some(600.0);
                    }
                    spans.push(sp);
                }
                if params.is_empty() {
                    current_color = None;
                    current_bold = false;
                } else {
                    for part in params.split(';') {
                        if let Ok(code) = part.parse::<u8>() {
                            match code {
                                0 | 39 => {
                                    current_color = None;
                                    current_bold = false;
                                }
                                1 => current_bold = true,
                                22 => current_bold = false,
                                c => {
                                    if let Some(mapped) = sgr_color(c, pal) {
                                        current_color = Some(mapped);
                                    }
                                }
                            }
                        }
                    }
                }
                seg_start = plain.len();
            }
            continue;
        }
        plain.push(ch);
    }

    if plain.len() > seg_start && (current_color.is_some() || current_bold) {
        let mut sp = Span::new(seg_start, plain.len());
        sp.color = Some(current_color.unwrap_or(default_color));
        if current_bold {
            sp.weight = Some(600.0);
        }
        spans.push(sp);
    }

    (plain, spans)
}
