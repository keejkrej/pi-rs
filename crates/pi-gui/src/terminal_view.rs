//! Embedded command runner and terminal output view in the inspector panel.

use rune::{Cx, El, ScrollSpec, col, el, row, text, text_input};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit};

use crate::state::{PiGuiAction, PiGuiState};
use crate::theme::WakuPalette;

/// Renders the workspace terminal output log and command bar.
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, _cx: &mut Cx<PiGuiState>) -> El {
    let mut output = col()
        .key("terminal-output")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .padding(10.0)
        .gap(2.0)
        .radius(k.radius.sm)
        .fill(pal.terminal)
        .scroll(ScrollSpec { stick_bottom: true });

    if s.terminal.lines.is_empty() {
        output = output.child(
            text("Run a command below to inspect output in this workspace.")
                .font(k.ty.mono)
                .color(pal.text_tertiary),
        );
    } else {
        for line in &s.terminal.lines {
            output = output.child(text(&line.text).font(k.ty.mono).color(if line.is_stderr {
                pal.danger
            } else {
                pal.text
            }));
        }
    }

    let prompt = row()
        .key("terminal-command-bar")
        .w_full()
        .gap(8.0)
        .center_y()
        .child(
            el().grow(1.0)
                .px(10.0)
                .py(6.0)
                .radius(k.radius.sm)
                .fill(pal.inset)
                .child(
                    text_input(&s.terminal_edit, k.ty.mono)
                        .key("terminal-input")
                        .w_full()
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
            Button::new("Clear")
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small)
                .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::ClearTerminal, None))
                .build(k),
        );

    col()
        .key("terminal-view")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .padding(12.0)
        .gap(8.0)
        .child(
            row()
                .w_full()
                .justify_between()
                .child(text(&s.terminal.cwd).font(k.ty.small).color(pal.text_secondary))
                .child(
                    text(if s.terminal.running {
                        "Running".to_string()
                    } else if let Some(code) = s.terminal.last_exit_code {
                        format!("Exit {code}")
                    } else {
                        "Idle".to_string()
                    })
                    .font(k.ty.small)
                    .color(if s.terminal.running {
                        pal.accent
                    } else {
                        pal.text_tertiary
                    }),
                ),
        )
        .child(output)
        .child(prompt)
}
