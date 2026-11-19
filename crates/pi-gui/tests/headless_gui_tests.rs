use pi_gui::{
    AuthProviderViewModel, BridgeEvent, ContentBlockView, ContextUsageView, DemoScene, DiffFileViewModel,
    DiffHunkViewModel, DiffLineKind, DiffLineViewModel, DiffSummary, ExtensionDialogView, FileTreeEntryView,
    InspectorTab, LaunchConfig, MessageRoleView, MessageViewModel, ModelUsageSummary, ModelViewModel, OpenFileView,
    OverlayKind, PiGuiAction, PiGuiState, PluginKind, PluginOrigin, PluginViewModel, PopoverKind, SettingsPage,
    TerminalOutputLine, ThemePreference, ThreadGroup, ThreadSummary, ToastTone, ToolCallStatus, ToolCallViewModel,
    app_view, on_key, poll_state,
};
use rune::{Env, Key, KeyEvent, Mods, Runtime};

fn boot_runtime(mut state: PiGuiState, size: (f32, f32), dark: bool) -> Runtime<PiGuiState> {
    state.reduced_motion = true;
    let mut rt = Runtime::new(state, app_view, size, Env::new(dark, 2.0));
    rt.on_key = Some(on_key);
    rt.poll = Some(poll_state);
    settle_frames(&mut rt);
    rt
}

fn settle_frames(rt: &mut Runtime<PiGuiState>) {
    for _ in 0..6 {
        rt.frame(0.016);
    }
}

fn assert_positive_rect(rt: &Runtime<PiGuiState>, key: &str) {
    let rect = rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(key))
        .map(|e| e.rect)
        .unwrap_or_else(|| panic!("expected rendered layout rect for key '{key}'"));
    assert!(
        rect[2] > 0.0 && rect[3] > 0.0,
        "expected non-zero layout bounds for '{key}', got {rect:?}"
    );
}

fn click_key(rt: &mut Runtime<PiGuiState>, key: &str) {
    let (x, y) = rt
        .find(key)
        .unwrap_or_else(|| panic!("expected clickable element with key '{key}'"));
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    settle_frames(rt);
}

fn send_key(rt: &mut Runtime<PiGuiState>, key: Key, mods: Mods) {
    rt.key(KeyEvent { key, mods });
    settle_frames(rt);
}

fn populate_rich_state(state: &mut PiGuiState) {
    state.sidebar_pane.set_open(true);
    state.inspector_pane.set_open(true);
    state.open_right_tabs = vec![
        InspectorTab::Changes,
        InspectorTab::Files,
        InspectorTab::Terminal,
        InspectorTab::Tree,
    ];

    let active_model = ModelViewModel {
        provider: "anthropic".to_string(),
        id: "claude-sonnet-4".to_string(),
        name: "Claude Sonnet 4".to_string(),
        reasoning: true,
        context_window: 200_000,
    };
    let models = vec![
        active_model.clone(),
        ModelViewModel {
            provider: "openai".to_string(),
            id: "gpt-4.1".to_string(),
            name: "GPT-4.1".to_string(),
            reasoning: false,
            context_window: 128_000,
        },
    ];
    let plugins = vec![
        PluginViewModel {
            id: "ext-quickjs".to_string(),
            name: "workspace-tools.ts".to_string(),
            description: "QuickJS extension exposing search & check tools".to_string(),
            kind: PluginKind::QuickJs,
            origin: PluginOrigin::Project,
            enabled: true,
            tool_count: 2,
            command_count: 1,
            error: None,
        },
        PluginViewModel {
            id: "ext-broken".to_string(),
            name: "broken-plugin.ts".to_string(),
            description: "QuickJS plugin failing validation".to_string(),
            kind: PluginKind::QuickJs,
            origin: PluginOrigin::Global,
            enabled: false,
            tool_count: 0,
            command_count: 0,
            error: Some("TypeError: expected export default function".to_string()),
        },
        PluginViewModel {
            id: "mcp-docs".to_string(),
            name: "docs-mcp".to_string(),
            description: "Documentation search MCP server".to_string(),
            kind: PluginKind::Mcp,
            origin: PluginOrigin::Global,
            enabled: true,
            tool_count: 3,
            command_count: 0,
            error: None,
        },
    ];
    let auth_providers = vec![
        AuthProviderViewModel {
            id: "anthropic".to_string(),
            name: "Anthropic".to_string(),
            authenticated: true,
            source_label: Some("OAuth".to_string()),
            supports_oauth: true,
        },
        AuthProviderViewModel {
            id: "openai".to_string(),
            name: "OpenAI".to_string(),
            authenticated: false,
            source_label: None,
            supports_oauth: false,
        },
    ];
    let messages = vec![
        MessageViewModel {
            id: "msg-1".to_string(),
            role: MessageRoleView::User,
            timestamp_ms: 1000,
            model_label: None,
            blocks: vec![ContentBlockView::Text("Inspect diff and run checks".to_string())],
            is_error: false,
            error_message: None,
        },
        MessageViewModel {
            id: "msg-2".to_string(),
            role: MessageRoleView::Assistant,
            timestamp_ms: 2000,
            model_label: Some("Claude Sonnet 4".to_string()),
            blocks: vec![
                ContentBlockView::Thinking("Reviewing modified Rust files".to_string()),
                ContentBlockView::ToolCall(ToolCallViewModel {
                    id: "tool-1".to_string(),
                    name: "read".to_string(),
                    arguments_preview: "crates/pi-gui/src/lib.rs".to_string(),
                    output_preview: Some("pub mod right_panel;".to_string()),
                    status: ToolCallStatus::Success,
                    duration_ms: Some(12),
                    expanded: true,
                }),
                ContentBlockView::Text("All inspector views are ready.".to_string()),
            ],
            is_error: false,
            error_message: None,
        },
    ];

    state.apply_bridge_event(BridgeEvent::SessionLoaded {
        session_id: "sess-1".to_string(),
        messages,
        models,
        active_model: Some(active_model),
        plugins,
        auth_providers,
        context_usage: ContextUsageView {
            used_tokens: 12_400,
            window_tokens: 200_000,
            percent: 6.2,
            thinking_level: "medium".to_string(),
            session_usage: ModelUsageSummary {
                input_tokens: 9_000,
                output_tokens: 3_400,
                cache_read_tokens: 1_200,
                total_cost: 0.04,
            },
        },
    });
    state.apply_bridge_event(BridgeEvent::ThreadsUpdated(vec![
        ThreadSummary {
            id: "sess-1".to_string(),
            title: "GUI Waku Overhaul".to_string(),
            cwd: "/workspace/pi-rs".to_string(),
            model: Some("Claude Sonnet 4".to_string()),
            updated_at_ms: 2000,
            message_count: 2,
            is_streaming: false,
            has_unread: false,
            group: ThreadGroup::Today,
        },
        ThreadSummary {
            id: "sess-0".to_string(),
            title: "Earlier exploration".to_string(),
            cwd: "/workspace/pi-rs".to_string(),
            model: None,
            updated_at_ms: 1000,
            message_count: 4,
            is_streaming: false,
            has_unread: false,
            group: ThreadGroup::Yesterday,
        },
    ]));
    state.apply_bridge_event(BridgeEvent::DiffUpdated(DiffSummary {
        branch: Some("swarm/waku-ui".to_string()),
        files: vec![
            DiffFileViewModel {
                path: "crates/pi-gui/src/right_panel.rs".to_string(),
                status: "M".to_string(),
                additions: 12,
                deletions: 3,
                collapsed: false,
                hunks: vec![DiffHunkViewModel {
                    header: "@@ -10,4 +10,6 @@".to_string(),
                    lines: vec![
                        DiffLineViewModel {
                            kind: DiffLineKind::Context,
                            old_line: Some(10),
                            new_line: Some(10),
                            text: "use rune::{El, row};".to_string(),
                        },
                        DiffLineViewModel {
                            kind: DiffLineKind::Removed,
                            old_line: Some(11),
                            new_line: None,
                            text: "// old placeholder".to_string(),
                        },
                        DiffLineViewModel {
                            kind: DiffLineKind::Added,
                            old_line: None,
                            new_line: Some(11),
                            text: "pub fn view(s: &PiGuiState) -> El {".to_string(),
                        },
                    ],
                }],
            },
            DiffFileViewModel {
                path: "README.md".to_string(),
                status: "A".to_string(),
                additions: 2,
                deletions: 0,
                collapsed: false,
                hunks: Vec::new(),
            },
        ],
        selected_path: Some("crates/pi-gui/src/right_panel.rs".to_string()),
        total_additions: 14,
        total_deletions: 3,
    }));
    state.apply_bridge_event(BridgeEvent::FilesUpdated(vec![
        FileTreeEntryView {
            path: "crates".to_string(),
            name: "crates".to_string(),
            is_dir: true,
            depth: 0,
            expanded: true,
            modified: false,
        },
        FileTreeEntryView {
            path: "crates/pi-gui/src/right_panel.rs".to_string(),
            name: "right_panel.rs".to_string(),
            is_dir: false,
            depth: 1,
            expanded: false,
            modified: true,
        },
        FileTreeEntryView {
            path: "Cargo.toml".to_string(),
            name: "Cargo.toml".to_string(),
            is_dir: false,
            depth: 0,
            expanded: false,
            modified: false,
        },
    ]));
    state.apply_bridge_event(BridgeEvent::FileOpened(OpenFileView {
        path: "crates/pi-gui/src/right_panel.rs".to_string(),
        language: Some("rust".to_string()),
        content: "pub fn render_tab() -> &'static str {\n    \"ok\"\n}\n".to_string(),
        line_count: 3,
        dirty: true,
    }));
    state.apply_bridge_event(BridgeEvent::TerminalOutput(TerminalOutputLine {
        text: "\x1b[32mok\x1b[0m\t\x1b[1;31mwarning\x1b[0m\t🚀".to_string(),
        is_stderr: false,
    }));
    state.apply_bridge_event(BridgeEvent::TerminalOutput(TerminalOutputLine {
        text: "stderr diagnostic line".to_string(),
        is_stderr: true,
    }));
}

#[test]
fn demo_scenes_render_without_panic_and_non_zero_bounds() {
    for dark in [true, false] {
        for scene in DemoScene::ALL {
            let cfg = LaunchConfig {
                cwd: "/workspace/pi-rs".to_string(),
                demo: true,
                scene: Some(scene.id().to_string()),
                reduced_motion: true,
                ..LaunchConfig::default()
            };
            let rt = boot_runtime(PiGuiState::new(cfg), (1280.0, 820.0), dark);

            if rt.state.settings_open.is_some() {
                assert_positive_rect(&rt, "settings");
                assert_positive_rect(&rt, "settings.sidebar");
                assert_positive_rect(&rt, "settings.content");
                assert_positive_rect(&rt, "settings.content.inner");
            } else {
                assert_positive_rect(&rt, "workspace");
                assert_positive_rect(&rt, "workspace.main");
                assert_positive_rect(&rt, "workspace.main.header");
                assert_positive_rect(&rt, "workspace.main.composer");
                assert_positive_rect(&rt, "workspace.main.footer");
                if rt.state.messages.is_empty() && rt.state.active_stream.is_none() {
                    assert_positive_rect(&rt, "workspace.main.empty_state");
                } else {
                    assert_positive_rect(&rt, "workspace.main.transcript");
                }
                if rt.state.sidebar_pane.is_open() {
                    assert_positive_rect(&rt, "workspace.sidebar");
                    assert_positive_rect(&rt, "workspace.sidebar.settings");
                } else {
                    assert_positive_rect(&rt, "workspace.main.header.sidebar_toggle");
                }
                if rt.state.inspector_pane.is_open() {
                    assert_positive_rect(&rt, "right_panel");
                }
            }
        }
    }

    // Verify all primary Waku 3 panes simultaneously open with populated content.
    let mut full_state = PiGuiState::new(LaunchConfig {
        cwd: "/workspace/pi-rs".to_string(),
        ..LaunchConfig::default()
    });
    populate_rich_state(&mut full_state);
    let rt = boot_runtime(full_state, (1320.0, 840.0), true);
    assert_positive_rect(&rt, "workspace.sidebar");
    assert_positive_rect(&rt, "workspace.main.transcript");
    assert_positive_rect(&rt, "workspace.main.composer");
    assert_positive_rect(&rt, "right_panel");
}

#[test]
fn right_panel_tabs_render_and_handle_interactions() {
    let mut state = PiGuiState::new(LaunchConfig {
        cwd: "/workspace/pi-rs".to_string(),
        ..LaunchConfig::default()
    });
    populate_rich_state(&mut state);
    let mut rt = boot_runtime(state, (1280.0, 820.0), true);

    for tab in InspectorTab::ALL {
        click_key(&mut rt, &format!("right_panel.tab.{}", tab.id()));
        assert_eq!(rt.state.inspector_tab, tab);
        assert_positive_rect(&rt, "right_panel");
        match tab {
            InspectorTab::Changes => {
                assert_positive_rect(&rt, "diff-view");
                assert_positive_rect(&rt, "right_panel.changes.commit_btn");
                assert_positive_rect(&rt, "diff-file-crates/pi-gui/src/right_panel.rs");
            }
            InspectorTab::Files => {
                assert_positive_rect(&rt, "file-view");
                assert_positive_rect(&rt, "right_panel.files.tree");
            }
            InspectorTab::Terminal => {
                assert_positive_rect(&rt, "right_panel.terminal.surface");
                assert_positive_rect(&rt, "terminal-output");
                assert_positive_rect(&rt, "terminal-input");
            }
            InspectorTab::Tree => {
                assert_positive_rect(&rt, "right_panel.tree.body");
            }
        }
    }

    // Verify terminal preset chip and clear action.
    click_key(&mut rt, "right_panel.tab.terminal");
    assert!(!rt.state.terminal.lines.is_empty());
    click_key(&mut rt, "terminal-preset-git-status");
    assert!(rt.state.terminal.running);
    click_key(&mut rt, "terminal-clear-btn");
    assert!(rt.state.terminal.lines.is_empty());

    // Verify Changes Commit button opens CommitDialog modal.
    click_key(&mut rt, "right_panel.tab.changes");
    click_key(&mut rt, "right_panel.changes.commit_btn");
    assert!(matches!(rt.state.overlay, Some(OverlayKind::CommitDialog)));
    assert_positive_rect(&rt, "commit_dialog.card");
    click_key(&mut rt, "commit_dialog.include_unstaged");
    assert!(!rt.state.commit_include_unstaged);
    send_key(&mut rt, Key::Escape, Mods::default());
    assert!(rt.state.overlay.is_none());
}

#[test]
fn waku_popovers_command_palette_and_settings_interactions() {
    let mut state = PiGuiState::new(LaunchConfig {
        cwd: "/workspace/pi-rs".to_string(),
        ..LaunchConfig::default()
    });
    populate_rich_state(&mut state);
    state.inspector_pane.set_open(false);
    let mut rt = boot_runtime(state, (1280.0, 820.0), true);

    // Toggle sidebar via Waku header icon button.
    let initial_sidebar = rt.state.sidebar_pane.is_open();
    click_key(&mut rt, "workspace.sidebar.toggle");
    assert_ne!(rt.state.sidebar_pane.is_open(), initial_sidebar);
    click_key(&mut rt, "workspace.main.header.sidebar_toggle");
    assert_eq!(rt.state.sidebar_pane.is_open(), initial_sidebar);

    // Open CommandPalette via Cmd+K shortcut, verify card, filter, and escape.
    send_key(
        &mut rt,
        Key::Command('k'),
        Mods {
            cmd: true,
            ..Mods::default()
        },
    );
    assert!(matches!(rt.state.overlay, Some(OverlayKind::CommandPalette)));
    assert_positive_rect(&rt, "command_palette.card");
    assert_positive_rect(&rt, "command_palette.search");
    assert_positive_rect(&rt, "command_palette.results");
    send_key(&mut rt, Key::Escape, Mods::default());
    assert!(rt.state.overlay.is_none());

    // Open full-window 2-pane Settings via bottom-left footer gear button.
    click_key(&mut rt, "workspace.sidebar.settings");
    assert_eq!(rt.state.settings_open, Some(SettingsPage::General));
    assert_positive_rect(&rt, "settings");
    assert_positive_rect(&rt, "settings.sidebar");
    assert_positive_rect(&rt, "settings.content");

    click_key(&mut rt, "settings.sidebar.nav.appearance");
    assert_eq!(rt.state.settings_open, Some(SettingsPage::Appearance));
    click_key(&mut rt, "settings-theme-light");
    assert_eq!(rt.state.theme_preference, ThemePreference::Light);
    click_key(&mut rt, "settings-theme-dark");
    assert_eq!(rt.state.theme_preference, ThemePreference::Dark);

    click_key(&mut rt, "settings.sidebar.nav.skills");
    assert_eq!(rt.state.settings_open, Some(SettingsPage::Skills));
    let before = rt.state.plugins[0].enabled;
    click_key(&mut rt, "settings-skill-ext-quickjs");
    rt.state.dispatch(
        PiGuiAction::TogglePlugin {
            id: "ext-quickjs".to_string(),
            enabled: !before,
        },
        None,
    );
    assert_ne!(rt.state.plugins[0].enabled, before);

    click_key(&mut rt, "settings.sidebar.back");
    assert!(rt.state.settings_open.is_none());

    // Verify all 7 anchored Waku popovers open cleanly and Escape dismisses them.
    for pop in [
        PopoverKind::SidebarOptions,
        PopoverKind::ModelPicker,
        PopoverKind::ThinkingPicker,
        PopoverKind::ProjectPicker,
        PopoverKind::WorkspacePicker,
        PopoverKind::BranchPicker,
        PopoverKind::ContextUsage,
    ] {
        rt.state.dispatch(PiGuiAction::OpenPopover(pop), None);
        settle_frames(&mut rt);
        assert_eq!(rt.state.popover, Some(pop));
        send_key(&mut rt, Key::Escape, Mods::default());
        assert!(rt.state.popover.is_none(), "Escape should close {pop:?}");
    }

    // Verify toast renders at top-center and dismiss works.
    rt.state.push_toast("Extension updated", ToastTone::Success);
    rt.invalidate();
    settle_frames(&mut rt);
    assert_positive_rect(&rt, "workspace.toast");
    click_key(&mut rt, "workspace.toast.dismiss");
    assert!(rt.state.toasts.is_empty());

    // Verify ExtensionDialog opens and Escape dismisses.
    rt.state.dispatch(
        PiGuiAction::OpenOverlay(OverlayKind::ExtensionDialog(ExtensionDialogView {
            request_id: "dlg-1".to_string(),
            title: "Choose environment".to_string(),
            message: Some("Pick target".to_string()),
            options: vec!["staging".to_string(), "prod".to_string()],
            allow_input: false,
        })),
        None,
    );
    rt.invalidate();
    settle_frames(&mut rt);
    assert_positive_rect(&rt, "overlay-extension-dialog");
    send_key(&mut rt, Key::Escape, Mods::default());
    assert!(rt.state.overlay.is_none());
}
