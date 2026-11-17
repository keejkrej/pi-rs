# Building & Testing

## Toolchain

Workspace metadata in `Cargo.toml` specifies:

- Rust **1.98+** (`rust-version = "1.98"`, `edition = "2024"`)
- Formatter rules in `rustfmt.toml` (`edition = "2024"`, `max_width = 120`)

## OS Build Prerequisites

`pi-gui` links Rune (`wgpu`, `winit`, `accesskit`), so desktop builds require platform graphics/windowing headers:

- **macOS**: Xcode Command Line Tools (`xcode-select --install`).
- **Linux (Debian/Ubuntu)**:
  ```sh
  sudo apt-get install libxkbcommon-dev libwayland-dev
  # Needed for windowed pi-gui runs or software Vulkan rendering:
  sudo apt-get install libxkbcommon-x11-0 mesa-vulkan-drivers
  ```
  (Fedora: `libxkbcommon-devel wayland-devel`; Arch: `libxkbcommon wayland`.) Headless `--snap` runs do not require a display server.
- **Windows**:
  - MSVC (`x86_64-pc-windows-msvc`, default): Visual Studio C++ build tools.
  - GNU (`x86_64-pc-windows-gnu`): MSYS2 `mingw-w64-ucrt-x86_64-binutils` on `PATH` so `dlltool.exe` resolves `raw-dylib` imports cleanly.

If `pi-gui` system libraries are unavailable in a headless server environment, core CLI crates can still be checked or built individually with `-p pi-coding-agent`, `-p pi-ai`, etc.

## Build & Check Commands

```sh
# Full workspace check (libraries, binaries, examples, tests)
cargo check --workspace --all-targets

# Build all binaries (pi, pi-gui, pi-ai, pi-evals)
cargo build --workspace --bins
cargo build --release --bins

# Format check
cargo fmt --check

# Clippy
cargo clippy --workspace --all-targets
```

Run a specific native extension example:

```sh
cargo run -p pi-coding-agent --example extensions__commands
```

## Testing

All workspace unit and integration tests run offline without real LLM API keys or paid tokens:

- **Faux providers**: `pi_ai::providers::faux` provides deterministic streaming responses, tool calls, and thinking blocks.
- **Test seams**: `pi_js::seam` (`Guard`/`Slot`) enables thread-local overrides for HTTP transports, clipboards, browser launchers, and GUI subprocess dispatch.
- **Fixture binaries**: Integration tests invoke `fixture-stdio-server`, `fixture-stubborn-server` (`pi-mcp`), `fixture-stale-socket-server` (`pi-client` / `pi-server`), `fixture-fake-external-editor`, and `fixture-faux-session-worker` (`pi-coding-agent`).
- **Headless GUI & bridge tests**: `cargo test -p pi-gui` runs `headless_gui_tests` and `bridge_integration_test` off-screen via `rune::Runtime`.

Common test commands:

```sh
# Full workspace suite
cargo test --workspace

# Per-crate runs
cargo test -p pi-js
cargo test -p pi-ai
cargo test -p pi-agent-core
cargo test -p pi-coding-agent
cargo test -p pi-gui
cargo test -p pi-evals
```

`pi-evals` includes an offline documentation audit test suite (`cargo test -p pi-evals`) that validates `crates/pi-coding-agent/package/docs/` and checks that paths returned by `get_docs_path()` / `get_readme_path()` stay consistent.

Live end-to-end tests that require external credentials remain `#[ignore]`d by default.
