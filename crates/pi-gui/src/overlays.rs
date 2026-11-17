//! Modal overlays, command palette, model picker, extension dialogs, and toast notifications.

use rune::{Cx, El, col, el, row, text, text_input};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit};

use crate::settings;
use crate::state::{OverlayKind, PiGuiAction, PiGuiState, ToastTone};
use crate::theme::WakuPalette;

/// Renders active modal overlays and toast notifications to mount inside `rune_kit::root`.
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, cx: &mut Cx<PiGuiState>) -> Vec<El> {
    let mut layers = Vec::new();

    if let Some(overlay) = &s.overlay {
        let dialog_card = match overlay {
            OverlayKind::Settings => settings::view(s, k, pal, cx),
            OverlayKind::CommandPalette => render_command_palette(s, k, pal),
            OverlayKind::ModelPicker => render_model_picker(s, k, pal),
            OverlayKind::ExtensionDialog(dlg) => col()
                .key("overlay-extension-dialog")
                .w(460.0)
                .padding(18.0)
                .gap(10.0)
                .radius(k.radius.lg)
                .fill(pal.raised)
                .border(1.0, pal.border_strong)
                .child(text(&dlg.title).font(k.ty.body_medium).color(pal.text))
                .child(
                    text(dlg.message.as_deref().unwrap_or(""))
                        .font(k.ty.ui)
                        .color(pal.text_secondary),
                ),
            OverlayKind::Confirm { title, message, .. } => col()
                .key("overlay-confirm")
                .w(420.0)
                .padding(18.0)
                .gap(10.0)
                .radius(k.radius.lg)
                .fill(pal.raised)
                .border(1.0, pal.border_strong)
                .child(text(title).font(k.ty.body_medium).color(pal.text))
                .child(text(message).font(k.ty.ui).color(pal.text_secondary)),
            OverlayKind::SessionPicker
            | OverlayKind::BranchPicker
            | OverlayKind::FilePicker
            | OverlayKind::Auth
            | OverlayKind::ImagePreview { .. } => col()
                .key("overlay-generic")
                .w(460.0)
                .padding(18.0)
                .gap(10.0)
                .radius(k.radius.lg)
                .fill(pal.raised)
                .border(1.0, pal.border_strong)
                .child(text("Selector").font(k.ty.body_medium).color(pal.text)),
        };

        let scrim = el()
            .key("overlay-backdrop")
            .w_full()
            .h_full()
            .center()
            .fill(k.c.scrim)
            .modal(true, |st: &mut PiGuiState| {
                st.dispatch(PiGuiAction::CloseOverlay, None);
            })
            .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::CloseOverlay, None))
            .child(dialog_card.on_press(|_: &mut PiGuiState| {}));
        layers.push(scrim);
    }

    if !s.toasts.is_empty() {
        let mut toast_col = col().key("toast-stack").gap(6.0).padding(16.0);
        for toast in &s.toasts {
            let color = match toast.tone {
                ToastTone::Error => pal.danger,
                ToastTone::Warning => pal.warning,
                ToastTone::Success => pal.success,
                ToastTone::Info => pal.text_secondary,
            };
            let tid = toast.id;
            toast_col = toast_col.child(
                row()
                    .key(format!("toast-{}", toast.id))
                    .px(12.0)
                    .py(8.0)
                    .gap(8.0)
                    .radius(k.radius.md)
                    .fill(pal.raised)
                    .border(1.0, color)
                    .child(text(&toast.message).font(k.ty.ui).color(pal.text))
                    .child(
                        Button::new("Dismiss")
                            .variant(ButtonVariant::Ghost)
                            .control_size(ControlSize::Small)
                            .on_press(move |st: &mut PiGuiState| {
                                st.dispatch(PiGuiAction::DismissToast(tid), None);
                            })
                            .build(k),
                    ),
            );
        }
        layers.push(toast_col);
    }

    layers
}

fn render_command_palette(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    col()
        .key("overlay-command-palette")
        .w(520.0)
        .padding(14.0)
        .gap(10.0)
        .radius(k.radius.lg)
        .fill(pal.raised)
        .border(1.0, pal.border_strong)
        .child(text("Command Palette").font(k.ty.ui_medium).color(pal.text))
        .child(
            el().w_full()
                .px(10.0)
                .py(6.0)
                .radius(k.radius.sm)
                .fill(pal.inset)
                .child(
                    text_input(&s.palette_edit, k.ty.ui)
                        .key("palette-search-input")
                        .w_full()
                        .on_edit(|st: &mut PiGuiState| &mut st.palette_edit),
                ),
        )
}

fn render_model_picker(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let mut card = col()
        .key("overlay-model-picker")
        .w(480.0)
        .padding(16.0)
        .gap(8.0)
        .radius(k.radius.lg)
        .fill(pal.raised)
        .border(1.0, pal.border_strong)
        .child(text("Models").font(k.ty.ui_medium).color(pal.text));

    if s.models.is_empty() {
        card = card.child(
            text("No models discovered yet")
                .font(k.ty.small)
                .color(pal.text_tertiary),
        );
    } else {
        for model in &s.models {
            let provider = model.provider.clone();
            let model_id = model.id.clone();
            card = card.child(
                row()
                    .key(format!("model-{}-{}", model.provider, model.id))
                    .w_full()
                    .justify_between()
                    .px(10.0)
                    .py(6.0)
                    .radius(k.radius.sm)
                    .hover(pal.overlay)
                    .on_press(move |st: &mut PiGuiState| {
                        st.dispatch(
                            PiGuiAction::SelectModel {
                                provider: provider.clone(),
                                model_id: model_id.clone(),
                            },
                            None,
                        );
                    })
                    .child(text(&model.name).font(k.ty.ui).color(pal.text))
                    .child(text(&model.provider).font(k.ty.small).color(pal.text_tertiary)),
            );
        }
    }
    card
}
