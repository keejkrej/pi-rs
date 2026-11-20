# Desktop GUI (`pi-gui`)

`crates/pi-gui` implements the native GPU desktop client for `pi-rs` using [Rune](https://github.com/anomalyco/rune) (`rune`, `rune-kit`, `rune-motion`, `rune-highlights`, `rune-mermaid`, and `rune-theme-opencode`), matching Waku's 3-pane graphite/light layout and palette tokens.

## Launching

Launch `pi-gui` either directly or through the `pi` CLI:

```sh
pi gui [options]
pi --gui [options]
pi-gui [options]
```

Supported `pi-gui` flags (`crates/pi-gui/src/main.rs`):

| Flag | Description |
|---|---|
| `--cwd <dir>` | Set workspace working directory |
| `--session <id\|path>` | Resume a session by ID or `.jsonl` file path |
| `--session-dir <dir>` | Override session storage directory |
| `--provider <id>` | Initial provider ID |
| `--model <id>` | Initial model ID |
| `--thinking <level>` | Initial thinking level (`off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`) |
| `--theme <mode>` | Theme override (`dark`, `light`, `system`) |
| `--demo` | Launch with offline deterministic showcase data |
| `--scene <id>` | Apply a named deterministic scene (see below) |
| `--size <WxH>` | Override headless viewport dimensions (`1280x820`, `860x640`, `1600x960`) |
| `--scale <N>` | Override device pixel ratio (`1`, `2`) |
| `--snap <path>` | Render a headless frame to PNG via Rune and exit |
| `--reduced-motion` | Disable spring motion and transitions |
| `-V, --version` | Print `pi-gui` version |
| `-h, --help` | Print CLI help |

## Module Layout

| Module | Responsibility |
|---|---|
| `src/main.rs` | `pi-gui` binary entry point (`chrome::run`) |
| `src/chrome.rs` | Top-level `AppSpec`, 3-pane workspace composition (`sidebar` + `main` + `right_panel`), borderless 48px header, floating toast layer, and keyboard routing |
| `src/state.rs` | `PiGuiState`, `LaunchConfig`, `DemoScene`, `SCENE_ENTRIES`, `SettingsPage`, `PopoverKind`, `OverlayKind`, and `PiGuiAction` reducer |
| `src/theme.rs` | `WakuPalette`, dark/light tokens, 48px/252px/460px/720px layout metrics, and `rune_kit::Kit` configuration |
| `src/models.rs` | View models (`MessageViewModel`, `ThreadSummary`, `DiffFileViewModel`, `PluginViewModel`, demo bundle) |
| `src/sidebar.rs` | 252px left sidebar (48px traffic-light-cleared header, 32px New Task & Search rows, grouped 51px session cards, 40px footer with 26x26 Open Settings) |
| `src/transcript.rs` | Centered 720px transcript (empty state sparkle, 540px right-aligned user bubble, 24px collapsible `"Worked for …"` divider, chromeless assistant markdown, clamped tool cards) |
| `src/composer.rs` | 720px 13px-radius composer card, 692px docked queue tray, `@`/`/` autocomplete, clipboard/bash badges, and 28px workspace footer (`Project`, `Workspace`, `Branch`, `Context Window Usage`) |
| `src/right_panel.rs` | Collapsible 460px right panel (`48px` header, closable `28px` tabs `Changes`, `Files`, `Terminal`, `Tree`, `+` add surface) |
| `src/diff_view.rs` | Changed file list, unified line-numbered hunks, and `Commit` dialog trigger |
| `src/file_view.rs` | Side-by-side 184px workspace file tree and highlighted file preview |
| `src/terminal_view.rs` | Integrated terminal runner (`#151515` dark / `#FFFFFF` light surface), presets, and ANSI log |
| `src/overlays.rs` | Anchored popovers (`SidebarOptions`, 460x390 `ModelPicker`, `ThinkingPicker`, `ProjectPicker`, `WorkspacePicker`, `BranchPicker`, `ContextUsage`) and modals (`CommandPalette`, `CommitDialog`, extension/confirm dialogs) |
| `src/settings.rs` | Full-window 2-pane Settings (`General`, `Appearance`, `Providers`, `Skills`, `Usage`) |
| `src/replication.rs` | Mechanical `ui-replication` extractor emitting `actual-layout/`, `actual-tokens.yaml`, `actual-components.yaml`, and `actual-states.yaml` from live Rune runtime elements |
| `src/bridge.rs` | `GuiBridge` command/event channel and background Tokio worker driving `AgentSessionRuntime` |
| `src/bridge/ui.rs` | `GuiExtensionUiContext`, `DialogBroker`, and `GuiAuthInteraction` bridging extensions and OAuth |
| `src/bridge/workspace.rs` | Background git diff parsing, workspace file scanning, file reads, and shell command execution |

## Deterministic Scenes & UI Replication Verification

`pi-gui` exposes 31 canonical scenes in `DemoScene::ALL` plus Wave 4 compatibility aliases in `SCENE_ENTRIES`:

- Workspace scenes: `workspace-empty`, `workspace-populated`, `workspace-light`, `workspace-sidebar-collapsed`, `workspace-right-panel-open`, `workspace-busy-queued`, `workspace-toast`, `workspace-overflow`
- Popover scenes: `popover-sidebar-options`, `popover-model-picker`, `popover-model-picker-favorites`, `popover-model-picker-search-cleared`, `popover-thinking-picker`, `popover-project-picker`, `popover-workspace-picker`, `popover-branch-picker`, `popover-branch-picker-create`, `popover-context-usage`
- Right panel scenes: `right-panel-changes`, `right-panel-files`, `right-panel-terminal`, `right-panel-tree`
- Modals & Settings scenes: `command-palette`, `command-palette-filtered`, `command-palette-empty`, `dialog-commit`, `settings-general`, `settings-appearance`, `settings-providers`, `settings-skills`, `settings-usage`
- Legacy aliases: `default`, `empty`, `streaming`, `diff-review`, `overlays`, `settings`, `plugins`, `light-theme`, `dark-session`, `light-session`, `diff-inspector`, `settings-plugins`

Examples:

```sh
# Render headless Retina (2x) PNG snapshots
cargo run -p pi-gui -- --scene workspace-populated --size 1280x820 --scale 2 --snap /tmp/populated.png
cargo run -p pi-gui -- --scene popover-model-picker --size 1280x820 --scale 2 --snap /tmp/model-picker.png

# Run headless UI, replication extraction, and live AgentSession/QuickJS bridge tests
# (use "$(pwd)/.agents/ui-replication" from the workspace root or omit PI_UI_REPLICATION_DIR, as cargo test runs in crates/pi-gui)
PI_UI_REPLICATION_DIR="$(pwd)/.agents/ui-replication" cargo test -p pi-gui
```

`crates/pi-gui/tests/headless_gui_tests.rs` exercises 3-pane layout, sidebar toggling, right panel tabs, popovers, command palette, full-window Settings, and dialogs. `crates/pi-gui/tests/waku_ui_replication_test.rs` extracts live measured geometry, resolved tokens, components, and state transitions across all 35 `(surface, state, viewport)` targets. `crates/pi-gui/tests/bridge_integration_test.rs` exercises `GuiBridge` end-to-end against a temporary workspace, a real `.ts` QuickJS extension, git diff parsing, and `pi-ai`'s faux provider.

## Updating the Rune Dependency Pin

Rune is pre-release and does not inherit `[patch.crates-io]` from a dependency's workspace manifest. In root `Cargo.toml`:

1. Keep `git = "https://github.com/anomalyco/rune.git"` and the exact `rev = "<sha>"` identical across all `rune*` entries in `[workspace.dependencies]` (`rune`, `rune-kit`, `rune-motion`, `rune-highlights`, `rune-mermaid`, `rune-theme-opencode`) and all five entries in `[patch.crates-io]` (`wgpu-hal`, `winit`, `parley_engine`, `accesskit_windows`, `accesskit_macos`).
2. When bumping Rune, inspect Rune's root `Cargo.toml` at the target `dev` commit to verify whether any patched crate list changed, update all matching `rev` pins together, and regenerate `Cargo.lock` with `cargo check -p pi-gui --all-targets`.
3. Never commit local `path = ...` overrides for Rune or its vendored crates.
