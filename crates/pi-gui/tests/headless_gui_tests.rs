use pi_gui::{
    AuthProviderViewModel, BridgeEvent, ContentBlockView, ContextUsageView, DemoScene, DiffFileViewModel,
    DiffHunkViewModel, DiffLineKind, DiffLineViewModel, DiffSummary, ExtensionDialogView, FileTreeEntryView,
    InspectorTab, LaunchConfig, MessageRoleView, MessageViewModel, ModelUsageSummary, ModelViewModel, NavRailTab,
    OpenFileView, OverlayKind, PiGuiAction, PiGuiState, PluginKind, PluginOrigin, PluginViewModel, TerminalOutputLine,
    ThemePreference, ThreadGroup, ThreadSummary, ToastTone, ToolCallStatus, ToolCallViewModel, app_view, on_key,
    poll_state,
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
    for _ in 0..8 {
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
            title: "GUI Step 4B-3".to_string(),
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
        branch: Some("swarm/mcp-w4".to_string()),
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

            assert_positive_rect(&rt, "main-pane");
            assert_positive_rect(&rt, "transcript-scroll-host");
            assert_positive_rect(&rt, "composer-card");
            if rt.state.sidebar_pane.is_open() {
                assert_positive_rect(&rt, "sidebar");
            }
            if rt.state.inspector_pane.is_open() {
                assert_positive_rect(&rt, "right-panel");
            }
            match &rt.state.overlay {
                Some(OverlayKind::CommandPalette) => {
                    assert_positive_rect(&rt, "overlay-scrim");
                    assert_positive_rect(&rt, "overlay-command-palette");
                }
                Some(OverlayKind::Settings) => {
                    assert_positive_rect(&rt, "overlay-scrim");
                    assert_positive_rect(&rt, "overlay-settings");
                    assert_positive_rect(&rt, "settings-scroll");
                }
                Some(_) => assert_positive_rect(&rt, "overlay-scrim"),
                None => {}
            }
        }
    }

    // Verify all primary split chrome panes simultaneously open with populated content.
    let mut full_state = PiGuiState::new(LaunchConfig {
        cwd: "/workspace/pi-rs".to_string(),
        ..LaunchConfig::default()
    });
    populate_rich_state(&mut full_state);
    let rt = boot_runtime(full_state, (1320.0, 840.0), true);
    assert_positive_rect(&rt, "sidebar");
    assert_positive_rect(&rt, "transcript-scroll-host");
    assert_positive_rect(&rt, "composer-card");
    assert_positive_rect(&rt, "right-panel");
}

#[test]
fn inspector_tabs_render_and_handle_interactions() {
    let mut state = PiGuiState::new(LaunchConfig {
        cwd: "/workspace/pi-rs".to_string(),
        ..LaunchConfig::default()
    });
    populate_rich_state(&mut state);
    let mut rt = boot_runtime(state, (1280.0, 820.0), true);

    for tab in InspectorTab::ALL {
        click_key(&mut rt, &format!("inspector-tab-{}", tab.label()));
        assert_eq!(rt.state.inspector_tab, tab);
        assert_positive_rect(&rt, "right-panel");
        match tab {
            InspectorTab::Diff => {
                assert_positive_rect(&rt, "diff-view");
                assert_positive_rect(&rt, "diff-file-list");
                assert_positive_rect(&rt, "diff-scroll");
                assert_positive_rect(&rt, "diff-file-crates/pi-gui/src/right_panel.rs");
            }
            InspectorTab::Files => {
                assert_positive_rect(&rt, "file-view");
                assert_positive_rect(&rt, "file-tree-pane");
            }
            InspectorTab::File => {
                assert_positive_rect(&rt, "file-view");
                assert_positive_rect(&rt, "open-file-view");
                assert_positive_rect(&rt, "open-file-lines");
            }
            InspectorTab::Terminal => {
                assert_positive_rect(&rt, "terminal-view");
                assert_positive_rect(&rt, "terminal-output");
                assert_positive_rect(&rt, "terminal-input");
            }
            InspectorTab::Tools => {
                assert_positive_rect(&rt, "inspector-tools");
                assert_positive_rect(&rt, "tool-src-ext-quickjs");
            }
            InspectorTab::Plugins => {
                assert_positive_rect(&rt, "inspector-plugins");
                assert_positive_rect(&rt, "plugin-ext-quickjs");
            }
        }
    }

    // Verify plugin switch toggle in right panel Plugins tab.
    assert!(rt.state.plugins[0].enabled);
    click_key(&mut rt, "plugin-toggle-ext-quickjs");
    assert!(!rt.state.plugins[0].enabled);

    // Verify terminal preset chip and clear action.
    click_key(&mut rt, "inspector-tab-Terminal");
    assert!(!rt.state.terminal.lines.is_empty());
    click_key(&mut rt, "terminal-preset-git-status");
    assert!(rt.state.terminal.running);
    click_key(&mut rt, "terminal-clear-btn");
    assert!(rt.state.terminal.lines.is_empty());

    // Verify diff revert button opens Confirm modal and Confirm dispatches git checkout.
    click_key(&mut rt, &format!("inspector-tab-{}", InspectorTab::Diff.label()));
    click_key(&mut rt, "diff-list-item-README.md");
    assert_eq!(rt.state.diff.selected_path.as_deref(), Some("README.md"));
    click_key(&mut rt, "diff-revert-README.md");
    assert!(matches!(rt.state.overlay, Some(OverlayKind::Confirm { .. })));
    assert_positive_rect(&rt, "overlay-confirm");
    click_key(&mut rt, "confirm-accept-btn");
    assert!(rt.state.overlay.is_none());

    // Verify inspector collapse button toggles inspector closed cleanly.
    click_key(&mut rt, "inspector-collapse-btn");
    assert!(!rt.state.inspector_pane.is_open());

    // Verify fallback paths when tree is empty and diff is empty.
    let mut empty_inspector = PiGuiState::new(LaunchConfig::default());
    empty_inspector.inspector_pane.set_open(true);
    empty_inspector.files = vec![FileTreeEntryView {
        path: "notes.txt".to_string(),
        name: "notes.txt".to_string(),
        is_dir: false,
        depth: 0,
        expanded: false,
        modified: false,
    }];
    let mut empty_rt = boot_runtime(empty_inspector, (1280.0, 820.0), false);
    assert_positive_rect(&empty_rt, "diff-empty");
    click_key(&mut empty_rt, "inspector-tab-Files");
    assert_positive_rect(&empty_rt, "file-tree-fallback");
    assert_positive_rect(&empty_rt, "file-entry-notes.txt");
}

#[test]
fn interactive_dispatch_overlays_command_palette_and_settings() {
    let mut state = PiGuiState::new(LaunchConfig {
        cwd: "/workspace/pi-rs".to_string(),
        ..LaunchConfig::default()
    });
    populate_rich_state(&mut state);
    let mut rt = boot_runtime(state, (1280.0, 820.0), true);

    // Dispatch ToggleSidebar and ToggleInspector interactively.
    let initial_sidebar = rt.state.sidebar_pane.is_open();
    rt.state.dispatch(PiGuiAction::ToggleSidebar, None);
    rt.invalidate();
    settle_frames(&mut rt);
    assert_ne!(rt.state.sidebar_pane.is_open(), initial_sidebar);

    // Open CommandPalette via Cmd+K shortcut, filter by typing, and submit.
    send_key(
        &mut rt,
        Key::Command('k'),
        Mods {
            cmd: true,
            ..Mods::default()
        },
    );
    assert!(matches!(rt.state.overlay, Some(OverlayKind::CommandPalette)));
    assert_positive_rect(&rt, "overlay-command-palette");
    assert_positive_rect(&rt, "palette-command-list");

    click_key(&mut rt, "palette-search-input");
    send_key(&mut rt, Key::Text("settings".to_string()), Mods::default());
    assert!(rt.find("palette-cmd-open-settings").is_some());
    send_key(&mut rt, Key::Enter, Mods::default());
    assert!(matches!(rt.state.overlay, Some(OverlayKind::Settings)));
    assert_positive_rect(&rt, "overlay-settings");
    assert_positive_rect(&rt, "settings-scroll");

    // Change theme preference and toggle reduced motion + QuickJS plugin in Settings.
    assert_eq!(rt.state.theme_preference, ThemePreference::System);
    click_key(&mut rt, "theme-pref-Light");
    assert_eq!(rt.state.theme_preference, ThemePreference::Light);
    click_key(&mut rt, "theme-pref-Dark");
    assert_eq!(rt.state.theme_preference, ThemePreference::Dark);

    assert_positive_rect(&rt, "settings-plugin-ext-quickjs");
    assert_positive_rect(&rt, "settings-plugin-mcp-docs");
    let before = rt.state.plugins[0].enabled;
    click_key(&mut rt, "settings-plugin-toggle-ext-quickjs");
    assert_ne!(rt.state.plugins[0].enabled, before);
    click_key(&mut rt, "settings-reload-plugins-btn");
    click_key(&mut rt, "settings-close-btn");
    assert!(rt.state.overlay.is_none());

    // Verify all remaining overlay kinds render cleanly and Escape dismisses them.
    let cases: Vec<(OverlayKind, &str)> = vec![
        (OverlayKind::ModelPicker, "overlay-model-picker"),
        (OverlayKind::SessionPicker, "overlay-session-picker"),
        (OverlayKind::BranchPicker, "overlay-branch-picker"),
        (OverlayKind::FilePicker, "overlay-file-picker"),
        (OverlayKind::Auth, "overlay-auth"),
        (
            OverlayKind::ImagePreview {
                path: "/workspace/snap.png".to_string(),
                title: "Snapshot".to_string(),
            },
            "overlay-image-preview",
        ),
        (
            OverlayKind::ExtensionDialog(ExtensionDialogView {
                request_id: "dlg-1".to_string(),
                title: "Choose environment".to_string(),
                message: Some("Pick a target or type an override".to_string()),
                options: vec!["staging".to_string(), "prod".to_string()],
                allow_input: true,
            }),
            "overlay-extension-dialog",
        ),
    ];

    for (kind, key) in cases {
        rt.state.dispatch(PiGuiAction::OpenOverlay(kind), None);
        rt.invalidate();
        settle_frames(&mut rt);
        assert_positive_rect(&rt, "overlay-scrim");
        assert_positive_rect(&rt, key);
        send_key(&mut rt, Key::Escape, Mods::default());
        assert!(rt.state.overlay.is_none(), "Escape should close {key}");
    }

    // Verify ExtensionDialog confirm submits input and clears palette_edit, plus ToastRegion renders.
    rt.state.push_toast("Extension updated", ToastTone::Success);
    rt.state.dispatch(
        PiGuiAction::OpenOverlay(OverlayKind::ExtensionDialog(ExtensionDialogView {
            request_id: "dlg-2".to_string(),
            title: "Confirm Name".to_string(),
            message: None,
            options: Vec::new(),
            allow_input: true,
        })),
        None,
    );
    rt.state.palette_edit.replace_text("custom-val");
    rt.invalidate();
    settle_frames(&mut rt);
    assert_positive_rect(&rt, "toast-1");
    click_key(&mut rt, "ext-dlg-confirm-btn");
    assert!(rt.state.overlay.is_none());
    assert!(rt.state.palette_edit.text.is_empty());

    // Select nav tab Settings also opens Settings overlay.
    rt.state.dispatch(PiGuiAction::SelectNavTab(NavRailTab::Settings), None);
    assert!(matches!(rt.state.overlay, Some(OverlayKind::Settings)));
}
