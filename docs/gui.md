# Desktop GUI (`pi-gui`)

`crates/pi-gui` implements the native GPU desktop client for `pi-rs` using [Rune](https://github.com/anomalyco/rune) (`rune`, `rune-kit`, `rune-motion`, `rune-highlights`, `rune-mermaid`, and `rune-theme-opencode`) and a Waku-inspired graphite/light palette.

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
| `--thinking <level>` | Initial thinking level (`off`, `minimal`, `low`, `medium`, `high`, `xhigh`) |
| `--theme <mode>` | Theme override (`dark`, `light`, `system`) |
| `--demo` | Launch with offline deterministic showcase data |
| `--scene <id>` | Apply a named deterministic scene (see below) |
| `--snap <path>` | Render a headless frame to PNG via Rune and exit |
| `--reduced-motion` | Disable spring motion and transitions |
| `-V, --version` | Print `pi-gui` version |
| `-h, --help` | Print CLI help |

## Module Layout

| Module | Responsibility |
|---|---|
| `src/main.rs` | `pi-gui` binary entry point (`rune::set_app_id("dev.pi.gui")`, `SCENE_ENTRIES`, `rune::run`) |
| `src/chrome.rs` | Top-level `AppSpec`, window chrome, split-pane composition, and keyboard routing |
| `src/state.rs` | `PiGuiState`, `LaunchConfig`, `DemoScene`, `SCENE_ENTRIES`, `OverlayKind`, and `PiGuiAction` reducer |
| `src/theme.rs` | `WakuPalette`, dark/light tokens, layout metrics, and `rune_kit::Kit` configuration |
| `src/models.rs` | View models (`MessageViewModel`, `ThreadSummary`, `DiffFileViewModel`, `PluginViewModel`, demo bundle) |
| `src/sidebar.rs` | Navigation rail (`Sessions`, `Extensions`, `Settings`), grouped thread list, and workspace status |
| `src/transcript.rs` | Virtualized conversation transcript with markdown, code highlighting, mermaid diagrams, and tool blocks |
| `src/composer.rs` | Prompt input, model/thinking selectors, attachment chips, and stream abort/send controls |
| `src/right_panel.rs` | Collapsible right inspector tabs (`Diff`, `Files`, `Terminal`, `Plugins`) |
| `src/diff_view.rs` | Changed file list, unified/split hunks, and stage/revert actions |
| `src/file_view.rs` | Workspace file tree and highlighted file preview |
| `src/terminal_view.rs` | Integrated terminal runner and output view |
| `src/overlays.rs` | Command palette, model picker, auth dialog, extension dialogs, image preview, and confirmations |
| `src/settings.rs` | Settings modal (theme, auth providers, plugins, shortcuts) |
| `src/bridge.rs` | `GuiBridge` command/event channel and background Tokio worker driving `AgentSessionRuntime` |
| `src/bridge/ui.rs` | `GuiExtensionUiContext`, `DialogBroker`, and `GuiAuthInteraction` bridging extensions and OAuth |
| `src/bridge/workspace.rs` | Background git diff parsing, workspace file scanning, file reads, and shell command execution |

## Deterministic Scenes & Headless Verification

`pi-gui` exposes deterministic scenes (`DemoScene::ALL` plus Wave 4 aliases in `SCENE_ENTRIES`) for visual inspection and headless testing without a window or live API keys:

- Wave 4 `--scene` aliases: `dark-session`, `light-session`, `diff-inspector`, `command-palette`, `settings-plugins`
- Core scene IDs: `default`, `empty`, `streaming`, `diff-review`, `overlays`, `settings`, `plugins`, `light-theme`

Examples:

```sh
# Render headless PNG snapshots
cargo run -p pi-gui -- --scene dark-session --snap /tmp/dark-session.png
cargo run -p pi-gui -- --scene diff-inspector --snap /tmp/diff-inspector.png

# Run headless UI + live AgentSession/QuickJS bridge tests
cargo test -p pi-gui
```

`crates/pi-gui/tests/headless_gui_tests.rs` drives `rune::Runtime` headlessly across layout, scroll viewport bounds, keyboard shortcuts, and overlays. `crates/pi-gui/tests/bridge_integration_test.rs` exercises `GuiBridge` end-to-end against a temporary workspace, a real `.ts` QuickJS extension, git diff parsing, and `pi-ai`'s faux provider.

## Updating the Rune Dependency Pin

Rune is pre-release and does not inherit `[patch.crates-io]` from a dependency's workspace manifest. In root `Cargo.toml`:

1. Keep `git = "https://github.com/anomalyco/rune.git"` and the exact `rev = "<sha>"` identical across all `rune*` entries in `[workspace.dependencies]` (`rune`, `rune-kit`, `rune-motion`, `rune-highlights`, `rune-mermaid`, `rune-theme-opencode`) and all five entries in `[patch.crates-io]` (`wgpu-hal`, `winit`, `parley_engine`, `accesskit_windows`, `accesskit_macos`).
2. When bumping Rune, inspect Rune's root `Cargo.toml` at the target `dev` commit to verify whether any patched crate list changed, update all matching `rev` pins together, and regenerate `Cargo.lock` with `cargo check -p pi-gui --all-targets`.
3. Never commit local `path = ...` overrides for Rune or its vendored crates.
