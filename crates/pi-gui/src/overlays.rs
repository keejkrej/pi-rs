//! Waku modal overlays (`Cmd+K` Command Palette, `Cmd+P` Model Picker, Extension Dialogs) and Toasts.

use rune::{Cx, El, ScrollSpec, col, el, icon, row, text, text_input};
use rune_kit::{Badge, BadgeVariant, Button, ButtonVariant, ControlSize, Keybind, Kit, Toast, ToastRegion, glyphs};

use crate::models::{ExtensionDialogView, ModelViewModel, ToastTone};
use crate::settings;
use crate::state::{InspectorTab, OverlayKind, PiGuiAction, PiGuiState};
use crate::theme::{ThemePreference, WakuPalette};

/// Renders active modal overlays (scrim + centered dialog) and corner toast notifications.
pub fn view(s: &PiGuiState, k: &Kit, pal: &WakuPalette, cx: &mut Cx<PiGuiState>) -> Vec<El> {
    let mut layers = Vec::new();

    if let Some(overlay) = &s.overlay {
        let dialog_card = match overlay {
            OverlayKind::Settings => settings::view(s, k, pal, cx),
            OverlayKind::CommandPalette => render_command_palette(s, k, pal),
            OverlayKind::ModelPicker => render_model_picker(s, k, pal),
            OverlayKind::ExtensionDialog(dlg) => render_extension_dialog(s, k, pal, dlg),
            OverlayKind::Confirm {
                title,
                message,
                action_id,
            } => render_confirm_dialog(k, pal, title, message, action_id),
            OverlayKind::SessionPicker => render_session_picker(s, k, pal),
            OverlayKind::BranchPicker => render_branch_picker(s, k, pal),
            OverlayKind::FilePicker => render_file_picker(s, k, pal),
            OverlayKind::Auth => render_auth_overlay(s, k, pal),
            OverlayKind::ImagePreview { path, title } => render_image_preview(k, pal, path, title),
        };

        let modal_panel = dialog_card
            .modal(true, |st: &mut PiGuiState| st.dispatch(PiGuiAction::CloseOverlay, None))
            .modal_click_outside(true);

        layers.push(
            el().key("overlay-scrim")
                .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
                .fill(k.c.scrim)
                .a11y_hidden(),
        );
        layers.push(
            el().key("overlay-backdrop")
                .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
                .padding(24.0)
                .center()
                .child(modal_panel),
        );
    }

    if !s.toasts.is_empty() {
        let mut region = ToastRegion::new();
        for toast in &s.toasts {
            let tid = toast.id;
            let glyph = match toast.tone {
                ToastTone::Error => &glyphs::CIRCLE_X,
                ToastTone::Success => &glyphs::CHECK,
                ToastTone::Warning | ToastTone::Info => &glyphs::SPARKLE,
            };
            let t_el = Toast::new(format!("toast-{}", toast.id), &toast.message)
                .icon(k, glyph)
                .on_close(move |st: &mut PiGuiState| {
                    st.dispatch(PiGuiAction::DismissToast(tid), None);
                })
                .build(k);
            region = region.toast(t_el);
        }
        layers.push(region.build(k));
    }

    layers
}

struct PaletteCommand {
    id: &'static str,
    title: &'static str,
    category: &'static str,
    shortcut: &'static [&'static str],
    action: PiGuiAction,
}

fn palette_commands(s: &PiGuiState) -> Vec<PaletteCommand> {
    let next_theme = match s.theme_preference {
        ThemePreference::Dark => ThemePreference::Light,
        ThemePreference::Light => ThemePreference::System,
        ThemePreference::System => ThemePreference::Dark,
    };
    vec![
        PaletteCommand {
            id: "new-thread",
            title: "New Thread",
            category: "Session",
            shortcut: &["⌘", "N"],
            action: PiGuiAction::NewThread,
        },
        PaletteCommand {
            id: "compact-session",
            title: "Compact Session Context",
            category: "Session",
            shortcut: &[],
            action: PiGuiAction::CompactSession,
        },
        PaletteCommand {
            id: "toggle-theme",
            title: match next_theme {
                ThemePreference::Light => "Switch Theme to Waku Light",
                ThemePreference::Dark => "Switch Theme to Waku Dark",
                ThemePreference::System => "Switch Theme to System",
            },
            category: "Appearance",
            shortcut: &["⌘", "T"],
            action: PiGuiAction::SetThemePreference(next_theme),
        },
        PaletteCommand {
            id: "toggle-sidebar",
            title: "Toggle Sidebar",
            category: "Layout",
            shortcut: &["⌘", "B"],
            action: PiGuiAction::ToggleSidebar,
        },
        PaletteCommand {
            id: "toggle-inspector",
            title: "Toggle Right Inspector",
            category: "Layout",
            shortcut: &["⌘", "⇧", "I"],
            action: PiGuiAction::ToggleInspector,
        },
        PaletteCommand {
            id: "tab-diff",
            title: "Inspect Git Changes (Diff)",
            category: "Inspector",
            shortcut: &[],
            action: PiGuiAction::SelectInspectorTab(InspectorTab::Diff),
        },
        PaletteCommand {
            id: "tab-files",
            title: "Browse Workspace Files",
            category: "Inspector",
            shortcut: &[],
            action: PiGuiAction::SelectInspectorTab(InspectorTab::Files),
        },
        PaletteCommand {
            id: "tab-terminal",
            title: "Open Workspace Terminal",
            category: "Inspector",
            shortcut: &[],
            action: PiGuiAction::SelectInspectorTab(InspectorTab::Terminal),
        },
        PaletteCommand {
            id: "tab-plugins",
            title: "View QuickJS & Native Plugins",
            category: "Plugins",
            shortcut: &[],
            action: PiGuiAction::SelectInspectorTab(InspectorTab::Plugins),
        },
        PaletteCommand {
            id: "reload-plugins",
            title: "Reload QuickJS Plugins & Workspace",
            category: "Plugins",
            shortcut: &[],
            action: PiGuiAction::ReloadPlugins,
        },
        PaletteCommand {
            id: "model-picker",
            title: "Change Active Model",
            category: "Model",
            shortcut: &["⌘", "P"],
            action: PiGuiAction::OpenOverlay(OverlayKind::ModelPicker),
        },
        PaletteCommand {
            id: "open-settings",
            title: "Open Settings",
            category: "Settings",
            shortcut: &["⌘", ","],
            action: PiGuiAction::OpenOverlay(OverlayKind::Settings),
        },
    ]
}

fn render_command_palette(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let query = s.palette_edit.text.trim().to_ascii_lowercase();
    let commands: Vec<PaletteCommand> = palette_commands(s)
        .into_iter()
        .filter(|cmd| {
            query.is_empty()
                || cmd.title.to_ascii_lowercase().contains(&query)
                || cmd.category.to_ascii_lowercase().contains(&query)
        })
        .collect();

    let first_action = commands.first().map(|c| c.action.clone());

    let search_bar = row()
        .w_full()
        .px(10.0)
        .py(8.0)
        .gap(8.0)
        .center_y()
        .radius(k.radius.sm)
        .fill(pal.inset)
        .border(1.0, pal.border_strong)
        .child(icon(&glyphs::SEARCH, 15.0).color(pal.text_tertiary))
        .child(
            text_input(&s.palette_edit, k.ty.ui)
                .single_line()
                .key("palette-search-input")
                .label("Command palette search")
                .grow(1.0)
                .min_w(0.0)
                .color(pal.text)
                .placeholder("Type a command or search...", pal.text_tertiary)
                .on_edit(|st: &mut PiGuiState| &mut st.palette_edit)
                .on_submit(move |st: &mut PiGuiState, tx| {
                    if let Some(act) = &first_action {
                        let closes_palette = !matches!(act, PiGuiAction::OpenOverlay(_));
                        st.dispatch(act.clone(), Some(tx));
                        if closes_palette {
                            st.dispatch(PiGuiAction::CloseOverlay, Some(tx));
                        }
                        st.palette_edit.clear();
                    }
                }),
        );

    let mut list = col()
        .key("palette-command-list")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .gap(4.0)
        .scroll(ScrollSpec { stick_bottom: false });

    if commands.is_empty() {
        list = list.child(
            el().w_full()
                .py(24.0)
                .center()
                .child(text("No matching commands").font(k.ty.small).color(pal.text_tertiary)),
        );
    } else {
        for (idx, cmd) in commands.into_iter().enumerate() {
            let act = cmd.action;
            let keep_open = matches!(act, PiGuiAction::OpenOverlay(_));
            let mut item_row = row()
                .key(format!("palette-cmd-{}", cmd.id))
                .role("option")
                .focusable(true)
                .w_full()
                .px(10.0)
                .py(7.0)
                .gap(8.0)
                .justify_between()
                .center_y()
                .radius(k.radius.sm)
                .fill(if idx == 0 {
                    pal.sidebar_item_background
                } else {
                    rune::Color::TRANSPARENT
                })
                .hover(pal.overlay)
                .on_press_tx(move |st: &mut PiGuiState, tx| {
                    st.dispatch(act.clone(), Some(tx));
                    if !keep_open {
                        st.dispatch(PiGuiAction::CloseOverlay, Some(tx));
                    }
                    st.palette_edit.clear();
                })
                .child(
                    row()
                        .grow(1.0)
                        .min_w(0.0)
                        .gap(8.0)
                        .center_y()
                        .child(Badge::new(cmd.category).variant(BadgeVariant::Subtle).build(k))
                        .child(text(cmd.title).font(k.ty.ui).color(pal.text).truncate()),
                );
            if !cmd.shortcut.is_empty() {
                item_row = item_row.child(Keybind::new(cmd.shortcut).build(k));
            }
            list = list.child(item_row);
        }
    }

    col()
        .key("overlay-command-palette")
        .w(540.0)
        .h(400.0)
        .min_h(0.0)
        .padding(14.0)
        .gap(10.0)
        .radius(k.radius.lg)
        .fill(pal.raised)
        .border(1.0, pal.border_strong)
        .child(
            row()
                .w_full()
                .justify_between()
                .center_y()
                .child(text("Command Palette").font(k.ty.ui_medium).color(pal.text))
                .child(Keybind::new(&["Esc"]).build(k)),
        )
        .child(search_bar)
        .child(list)
}

fn format_ctx_window(tokens: i64) -> String {
    if tokens >= 1_000_000 {
        format!("{}M ctx", tokens / 1_000_000)
    } else if tokens >= 1_000 {
        format!("{}K ctx", tokens / 1_000)
    } else {
        format!("{tokens} ctx")
    }
}

fn render_model_picker(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let query = s.palette_edit.text.trim().to_ascii_lowercase();
    let models: Vec<&ModelViewModel> = s
        .models
        .iter()
        .filter(|m| {
            query.is_empty()
                || m.name.to_ascii_lowercase().contains(&query)
                || m.id.to_ascii_lowercase().contains(&query)
                || m.provider.to_ascii_lowercase().contains(&query)
        })
        .collect();

    let search = row()
        .w_full()
        .px(10.0)
        .py(7.0)
        .gap(8.0)
        .center_y()
        .radius(k.radius.sm)
        .fill(pal.inset)
        .border(1.0, pal.border_strong)
        .child(icon(&glyphs::SEARCH, 15.0).color(pal.text_tertiary))
        .child(
            text_input(&s.palette_edit, k.ty.ui)
                .single_line()
                .key("model-search-input")
                .label("Filter models")
                .grow(1.0)
                .min_w(0.0)
                .color(pal.text)
                .placeholder("Filter models by provider or name...", pal.text_tertiary)
                .on_edit(|st: &mut PiGuiState| &mut st.palette_edit),
        );

    let mut list = col()
        .key("model-picker-list")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .gap(4.0)
        .scroll(ScrollSpec { stick_bottom: false });

    if models.is_empty() {
        list = list.child(
            el().w_full().py(24.0).center().child(
                text("No matching models found")
                    .font(k.ty.small)
                    .color(pal.text_tertiary),
            ),
        );
    } else {
        for model in models {
            let active = s
                .active_model
                .as_ref()
                .is_some_and(|a| a.provider == model.provider && a.id == model.id);
            let provider = model.provider.clone();
            let model_id = model.id.clone();

            let mut right_meta = row()
                .gap(6.0)
                .center_y()
                .child(Badge::new(&model.provider).variant(BadgeVariant::Neutral).build(k))
                .child(
                    Badge::new(format_ctx_window(model.context_window))
                        .variant(BadgeVariant::Subtle)
                        .build(k),
                );
            if model.reasoning {
                right_meta = right_meta.child(Badge::new("Reasoning").variant(BadgeVariant::Accent).build(k));
            }
            if active {
                right_meta = right_meta.child(icon(&glyphs::CHECK, 14.0).color(pal.success));
            }

            list = list.child(
                row()
                    .key(format!("model-{}-{}", model.provider, model.id))
                    .role("option")
                    .focusable(true)
                    .w_full()
                    .justify_between()
                    .center_y()
                    .px(10.0)
                    .py(7.0)
                    .gap(8.0)
                    .radius(k.radius.sm)
                    .fill(if active {
                        pal.sidebar_item_background
                    } else {
                        rune::Color::TRANSPARENT
                    })
                    .hover(pal.overlay)
                    .on_press(move |st: &mut PiGuiState| {
                        st.dispatch(
                            PiGuiAction::SelectModel {
                                provider: provider.clone(),
                                model_id: model_id.clone(),
                            },
                            None,
                        );
                        st.palette_edit.clear();
                    })
                    .child(
                        col()
                            .grow(1.0)
                            .min_w(0.0)
                            .gap(1.0)
                            .child(text(&model.name).font(k.ty.ui_medium).color(pal.text).truncate())
                            .child(text(&model.id).font(k.ty.small).color(pal.text_tertiary).truncate()),
                    )
                    .child(right_meta),
            );
        }
    }

    col()
        .key("overlay-model-picker")
        .w(540.0)
        .h(420.0)
        .min_h(0.0)
        .padding(16.0)
        .gap(10.0)
        .radius(k.radius.lg)
        .fill(pal.raised)
        .border(1.0, pal.border_strong)
        .child(
            row()
                .w_full()
                .justify_between()
                .center_y()
                .child(text("Select Model").font(k.ty.ui_medium).color(pal.text))
                .child(
                    Button::new("Close")
                        .variant(ButtonVariant::Ghost)
                        .control_size(ControlSize::Small)
                        .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::CloseOverlay, None))
                        .build(k),
                ),
        )
        .child(search)
        .child(list)
}

fn render_extension_dialog(s: &PiGuiState, k: &Kit, pal: &WakuPalette, dlg: &ExtensionDialogView) -> El {
    let mut card = col()
        .key("overlay-extension-dialog")
        .w(480.0)
        .max_h(440.0)
        .min_h(0.0)
        .padding(18.0)
        .gap(12.0)
        .radius(k.radius.lg)
        .fill(pal.raised)
        .border(1.0, pal.border_strong)
        .child(
            row()
                .w_full()
                .justify_between()
                .center_y()
                .child(text(&dlg.title).font(k.ty.body_medium).color(pal.text))
                .child(Badge::new("Extension").variant(BadgeVariant::Accent).build(k)),
        );

    if let Some(msg) = &dlg.message {
        card = card.child(text(msg).font(k.ty.ui).color(pal.text_secondary).wrap());
    }

    if !dlg.options.is_empty() {
        let mut opts = col()
            .grow(1.0)
            .min_h(0.0)
            .w_full()
            .gap(4.0)
            .scroll(ScrollSpec { stick_bottom: false });
        for (idx, opt) in dlg.options.iter().enumerate() {
            let req_id = dlg.request_id.clone();
            let val = opt.clone();
            opts = opts.child(
                row()
                    .key(format!("ext-dlg-opt-{idx}"))
                    .w_full()
                    .px(10.0)
                    .py(7.0)
                    .radius(k.radius.sm)
                    .fill(pal.inset)
                    .border(1.0, pal.border)
                    .hover(pal.overlay)
                    .on_press(move |st: &mut PiGuiState| {
                        st.dispatch(
                            PiGuiAction::RespondExtensionDialog {
                                request_id: req_id.clone(),
                                value: Some(val.clone()),
                            },
                            None,
                        );
                    })
                    .child(text(opt).font(k.ty.ui).color(pal.text)),
            );
        }
        card = card.child(opts);
    }

    if dlg.allow_input {
        let req_id_submit = dlg.request_id.clone();
        card = card.child(
            el().w_full()
                .px(10.0)
                .py(7.0)
                .radius(k.radius.sm)
                .fill(pal.inset)
                .border(1.0, pal.border_strong)
                .child(
                    text_input(&s.palette_edit, k.ty.ui)
                        .single_line()
                        .key("ext-dlg-input")
                        .label("Extension dialog response")
                        .w_full()
                        .color(pal.text)
                        .placeholder("Enter response...", pal.text_tertiary)
                        .on_edit(|st: &mut PiGuiState| &mut st.palette_edit)
                        .on_submit(move |st: &mut PiGuiState, tx| {
                            let val = st.palette_edit.text.clone();
                            st.palette_edit.clear();
                            st.dispatch(
                                PiGuiAction::RespondExtensionDialog {
                                    request_id: req_id_submit.clone(),
                                    value: Some(val),
                                },
                                Some(tx),
                            );
                        }),
                ),
        );
    }

    let req_cancel = dlg.request_id.clone();
    let req_ok = dlg.request_id.clone();
    let mut footer = row().w_full().gap(8.0).justify_between().center_y().child(
        Button::new("Cancel")
            .variant(ButtonVariant::Ghost)
            .control_size(ControlSize::Regular)
            .on_press(move |st: &mut PiGuiState| {
                st.palette_edit.clear();
                st.dispatch(
                    PiGuiAction::RespondExtensionDialog {
                        request_id: req_cancel.clone(),
                        value: None,
                    },
                    None,
                );
            })
            .build(k)
            .key("ext-dlg-cancel-btn"),
    );
    if dlg.allow_input {
        footer = footer.child(
            Button::new("Confirm")
                .variant(ButtonVariant::Submit)
                .control_size(ControlSize::Regular)
                .on_press(move |st: &mut PiGuiState| {
                    let val = st.palette_edit.text.clone();
                    st.palette_edit.clear();
                    st.dispatch(
                        PiGuiAction::RespondExtensionDialog {
                            request_id: req_ok.clone(),
                            value: Some(val),
                        },
                        None,
                    );
                })
                .build(k)
                .key("ext-dlg-confirm-btn"),
        );
    }

    card.child(footer)
}

fn modal_card(key: &str, width: f32, height: Option<f32>, padding: f32, k: &Kit, pal: &WakuPalette) -> El {
    let mut card = col()
        .key(key)
        .w(width)
        .min_h(0.0)
        .padding(padding)
        .gap(10.0)
        .radius(k.radius.lg)
        .fill(pal.raised)
        .border(1.0, pal.border_strong);
    if let Some(h) = height {
        card = card.h(h);
    }
    card
}

fn modal_scroll(gap: f32) -> El {
    col()
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .gap(gap)
        .scroll(ScrollSpec { stick_bottom: false })
}

fn render_confirm_dialog(k: &Kit, pal: &WakuPalette, title: &str, message: &str, action_id: &str) -> El {
    let act_id = action_id.to_string();
    modal_card("overlay-confirm", 420.0, None, 18.0, k, pal)
        .gap(12.0)
        .child(text(title).font(k.ty.body_medium).color(pal.text))
        .child(text(message).font(k.ty.ui).color(pal.text_secondary).wrap())
        .child(
            row()
                .w_full()
                .gap(8.0)
                .justify_between()
                .child(
                    Button::new("Cancel")
                        .variant(ButtonVariant::Ghost)
                        .control_size(ControlSize::Regular)
                        .on_press(|st: &mut PiGuiState| st.dispatch(PiGuiAction::CloseOverlay, None))
                        .build(k)
                        .key("confirm-cancel-btn"),
                )
                .child(
                    Button::new("Confirm")
                        .variant(ButtonVariant::Danger)
                        .control_size(ControlSize::Regular)
                        .on_press(move |st: &mut PiGuiState| {
                            if let Some(path) = act_id.strip_prefix("revert:") {
                                let escaped = path.replace('\'', "'\\''");
                                st.dispatch(
                                    PiGuiAction::RunTerminalCommand(format!("git checkout -- '{escaped}'")),
                                    None,
                                );
                            }
                            st.dispatch(PiGuiAction::CloseOverlay, None);
                        })
                        .build(k)
                        .key("confirm-accept-btn"),
                ),
        )
}

fn render_session_picker(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let mut list = modal_scroll(4.0);
    for thread in &s.threads {
        let tid = thread.id.clone();
        list = list.child(
            row()
                .key(format!("pick-session-{}", thread.id))
                .w_full()
                .px(10.0)
                .py(7.0)
                .justify_between()
                .radius(k.radius.sm)
                .hover(pal.overlay)
                .on_press(move |st: &mut PiGuiState| {
                    st.dispatch(PiGuiAction::SelectThread(tid.clone()), None);
                    st.dispatch(PiGuiAction::CloseOverlay, None);
                })
                .child(text(&thread.title).font(k.ty.ui).color(pal.text).truncate())
                .child(
                    text(format!("{} msgs", thread.message_count))
                        .font(k.ty.small)
                        .color(pal.text_tertiary),
                ),
        );
    }
    modal_card("overlay-session-picker", 480.0, Some(380.0), 16.0, k, pal)
        .child(text("Switch Session").font(k.ty.ui_medium).color(pal.text))
        .child(list)
}

fn render_branch_picker(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let branch = s.diff.branch.as_deref().unwrap_or("detached");
    modal_card("overlay-branch-picker", 440.0, None, 18.0, k, pal)
        .child(text("Session & Git Branch").font(k.ty.body_medium).color(pal.text))
        .child(
            row()
                .w_full()
                .justify_between()
                .child(text("Current branch").font(k.ty.ui).color(pal.text_secondary))
                .child(Badge::new(branch).variant(BadgeVariant::Accent).build(k)),
        )
}

fn render_file_picker(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let mut list = modal_scroll(4.0);
    for entry in s.files.iter().filter(|f| !f.is_dir) {
        let p = entry.path.clone();
        list = list.child(
            row()
                .key(format!("pick-file-{}", entry.path))
                .w_full()
                .px(10.0)
                .py(6.0)
                .radius(k.radius.sm)
                .hover(pal.overlay)
                .on_press(move |st: &mut PiGuiState| {
                    st.dispatch(PiGuiAction::OpenFile(p.clone()), None);
                    st.dispatch(PiGuiAction::CloseOverlay, None);
                })
                .child(text(&entry.path).font(k.ty.ui).color(pal.text).truncate()),
        );
    }
    modal_card("overlay-file-picker", 500.0, Some(380.0), 16.0, k, pal)
        .child(text("Open Workspace File").font(k.ty.ui_medium).color(pal.text))
        .child(list)
}

fn render_auth_overlay(s: &PiGuiState, k: &Kit, pal: &WakuPalette) -> El {
    let mut list = modal_scroll(6.0);
    for prov in &s.auth_providers {
        let pid_in = prov.id.clone();
        let pid_out = prov.id.clone();
        list = list.child(
            row()
                .key(format!("auth-row-{}", prov.id))
                .w_full()
                .px(10.0)
                .py(8.0)
                .justify_between()
                .center_y()
                .radius(k.radius.sm)
                .fill(pal.inset)
                .border(1.0, pal.border)
                .child(text(&prov.name).font(k.ty.ui_medium).color(pal.text))
                .child(if prov.authenticated {
                    Button::new("Logout")
                        .variant(ButtonVariant::Ghost)
                        .control_size(ControlSize::Small)
                        .on_press(move |st: &mut PiGuiState| {
                            st.dispatch(PiGuiAction::LogoutProvider(pid_out.clone()), None)
                        })
                        .build(k)
                } else {
                    Button::new("Login")
                        .variant(ButtonVariant::Outline)
                        .control_size(ControlSize::Small)
                        .on_press(move |st: &mut PiGuiState| {
                            st.dispatch(PiGuiAction::LoginProvider(pid_in.clone()), None)
                        })
                        .build(k)
                }),
        );
    }
    modal_card("overlay-auth", 480.0, Some(360.0), 16.0, k, pal)
        .child(text("Provider Authentication").font(k.ty.ui_medium).color(pal.text))
        .child(list)
}

fn render_image_preview(k: &Kit, pal: &WakuPalette, path: &str, title: &str) -> El {
    modal_card("overlay-image-preview", 480.0, None, 18.0, k, pal)
        .child(text(title).font(k.ty.body_medium).color(pal.text))
        .child(text(path).font(k.ty.mono).color(pal.text_secondary))
}
