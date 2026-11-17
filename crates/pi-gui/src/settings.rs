//! Settings modal view for appearance, auth providers, plugins, and runtime options.

use rune::{Cx, El, col, el, row, text};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit};

use crate::state::{PiGuiAction, PiGuiState};
use crate::theme::{ThemePreference, WakuPalette};

/// Renders the Settings modal body.
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, _cx: &mut Cx<PiGuiState>) -> El {
    let mut theme_row = row().key("settings-theme-row").gap(6.0);
    for pref in ThemePreference::ALL {
        let active = s.theme_preference == pref;
        theme_row = theme_row.child(
            el().key(format!("theme-pref-{}", pref.label()))
                .px(10.0)
                .py(5.0)
                .radius(k.radius.sm)
                .fill(if active { pal.accent } else { pal.inset })
                .on_press(move |st: &mut PiGuiState| {
                    st.dispatch(PiGuiAction::SetThemePreference(pref), None);
                })
                .child(text(pref.label()).font(k.ty.small_medium).color(if active {
                    pal.on_inverse
                } else {
                    pal.text
                })),
        );
    }

    col()
        .key("overlay-settings")
        .w(560.0)
        .padding(20.0)
        .gap(14.0)
        .radius(k.radius.lg)
        .fill(pal.raised)
        .border(1.0, pal.border_strong)
        .child(
            row()
                .w_full()
                .justify_between()
                .center_y()
                .child(text("Settings").font(k.ty.body_medium).color(pal.text))
                .child(
                    Button::new("Close")
                        .variant(ButtonVariant::Ghost)
                        .control_size(ControlSize::Small)
                        .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::CloseOverlay, None))
                        .build(k),
                ),
        )
        .child(
            row()
                .w_full()
                .justify_between()
                .center_y()
                .child(text("Appearance").font(k.ty.ui).color(pal.text_secondary))
                .child(theme_row),
        )
        .child(
            row()
                .w_full()
                .justify_between()
                .center_y()
                .child(text("Version").font(k.ty.ui).color(pal.text_secondary))
                .child(
                    text(*pi_coding_agent::config::VERSION)
                        .font(k.ty.small)
                        .color(pal.text_tertiary),
                ),
        )
        .child(
            row()
                .w_full()
                .justify_between()
                .center_y()
                .child(text("Providers").font(k.ty.ui).color(pal.text_secondary))
                .child(
                    text(format!("{} configured", s.auth_providers.len()))
                        .font(k.ty.small)
                        .color(pal.text_tertiary),
                ),
        )
}
