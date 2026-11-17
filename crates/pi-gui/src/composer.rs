//! Bottom prompt composer with model/thinking selectors and submit/stop actions.

use rune::{Cx, El, col, el, row, text, text_input};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit};

use crate::state::{OverlayKind, PiGuiAction, PiGuiState};
use crate::theme::{LayoutMetrics, WakuPalette};

/// Renders the composer card docked beneath the transcript viewport.
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, _cx: &mut Cx<PiGuiState>) -> El {
    let model_label = s.active_model.as_ref().map_or("Select model", |m| m.name.as_str());
    let is_streaming = s.active_stream.is_some();

    let input = text_input(&s.composer_edit, k.ty.body)
        .key("composer-input")
        .w_full()
        .min_h(44.0)
        .on_edit(|st: &mut PiGuiState| &mut st.composer_edit)
        .on_submit(|st: &mut PiGuiState, tx| {
            st.dispatch(PiGuiAction::SubmitPrompt, Some(tx));
        });

    let controls = row()
        .key("composer-controls")
        .w_full()
        .justify_between()
        .center_y()
        .child(
            row()
                .gap(8.0)
                .center_y()
                .child(
                    el().key("composer-model-btn")
                        .px(8.0)
                        .py(4.0)
                        .radius(k.radius.sm)
                        .hover(pal.overlay)
                        .on_press(|st: &mut PiGuiState| {
                            st.dispatch(PiGuiAction::OpenOverlay(OverlayKind::ModelPicker), None);
                        })
                        .child(text(model_label).font(k.ty.small_medium).color(pal.text_secondary)),
                )
                .child(
                    el().key("composer-thinking-btn")
                        .px(8.0)
                        .py(4.0)
                        .radius(k.radius.sm)
                        .hover(pal.overlay)
                        .on_press(|st: &mut PiGuiState| {
                            st.dispatch(PiGuiAction::CycleThinkingLevel, None);
                        })
                        .child(
                            text(format!("Thinking: {}", s.context_usage.thinking_level))
                                .font(k.ty.small)
                                .color(pal.text_tertiary),
                        ),
                ),
        )
        .child(if is_streaming {
            Button::new("Stop")
                .variant(ButtonVariant::Danger)
                .control_size(ControlSize::Regular)
                .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::AbortRun, None))
                .build(k)
        } else {
            Button::new("Send")
                .variant(ButtonVariant::Submit)
                .control_size(ControlSize::Regular)
                .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::SubmitPrompt, None))
                .build(k)
        });

    col()
        .key("composer-dock")
        .w_full()
        .align(rune::Align::Center)
        .px(20.0)
        .pb(16.0)
        .pt(8.0)
        .child(
            col()
                .key("composer-card")
                .w_full()
                .max_w(LayoutMetrics::CONTENT_MAX_WIDTH)
                .gap(8.0)
                .padding(12.0)
                .radius(k.radius.lg)
                .fill(pal.composer)
                .border(1.0, pal.border_strong)
                .child(input)
                .child(controls),
        )
}
