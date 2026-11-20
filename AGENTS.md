# AGENTS.md

`pi-rs` is a pure-Rust implementation of the [Pi](https://github.com/earendil-works/pi) v1.5.2 coding agent harness across 15 parity crates plus `pi-gui` (a native GPU desktop client built with [Rune](https://github.com/anomalyco/rune)) and QuickJS WASM extension hosting (`pi-codemode`).

This file states repository layout, invariants, commands, and git rules. Detailed guides live under [`docs/README.md`](docs/README.md).

## Layout

```text
Cargo.toml                          Workspace manifest, shared deps, Rune git pin + [patch.crates-io]
Cargo.lock                          Workspace lockfile
rustfmt.toml                        edition = "2024", max_width = 120
AGENTS.md                           Repository guide (this file)
CLAUDE.md                           Symlink to AGENTS.md
README.md                           Project overview and quick start
LICENSE                             MIT license
docs/
  README.md                         Documentation index
  architecture.md                   Crate dependency graph, runtime model, storage and wire formats
  gui.md                            pi-gui Rune desktop client, scenes, and --snap verification
  extensions.md                     QuickJS WASM plugin loader, virtual modules, and native extensions
  building-and-testing.md           Toolchain, OS build deps, test strategy, and evals
  porting/                          Historical v1.0.0 TS-to-Rust porting archive (PORTING.md, port-map.json, HANDOFF.md)
crates/
  pi-js/                            Rust foundation providing Node/JS semantics (JSON, UTF-16, Intl, fetch, fs, path, time, env, TypeBox, jsdiff)
  pi-schema/                        Eval-free JSON Schema (Draft 2020-12 / Draft-07) validator, coercer, and type builders
  pi-telemetry/                     Telemetry contracts, reference in-memory adapter, and schemas
  pi-chord/                         Chord object runtime (contracts, store, documents, links, HTTP/WS)
  pi-tui/                           Differential terminal UI library, markdown renderer, image protocols, native modifiers FFI
  pi-mcp/                           MCP client, stdio/HTTP transports, OAuth/token management, and fixture servers
  pi-codemode/                      Wasmtime host for embedded quickjs-wasi (sandboxed JS/TS execution)
  pi-ai/                            Multi-provider LLM streaming API, OAuth, model catalog, faux provider (`pi-ai` binary)
  pi-protocol/                      Chord CBOR frame codecs, envelope types, and protocol helpers
  pi-agent-core/                    Agent loop, tool execution runtime, steering/follow-up queues, and state management
  pi-client/                        Chord client transports (Unix socket, WebSocket, reconnection, handshake)
  pi-server/                        Chord server runtime and Unix/WebSocket listener transports
  pi-durable/                       Durable session, task, and document storage (SQLite + Chord)
  pi-coding-agent/                  CLI (`pi` binary), session manager, built-in tools, interactive/print/JSON/RPC modes, extension host
    package/                        Runtime-loaded package docs, examples, README, CHANGELOG, and package.json
  pi-gui/                           Rune GPU desktop application (`pi-gui` binary), Waku theme, diff/file/terminal inspector
  pi-evals/                         Documentation audit and Docker evaluation harness (`pi-evals` binary)
```

## Invariants

- **On-disk, wire, and CLI parity**: Keep `~/.pi/agent` layout (`settings.json`, `auth.json`, `models.json`, session `.jsonl`, lockfiles), RPC JSONL events, Chord CBOR frames, and CLI output byte-compatible with upstream Pi v1.5.2.
- **`pi-js` runtime discipline** (enforced across ported crates; see [`docs/architecture.md`](docs/architecture.md)):
  - Use `indexmap::{IndexMap, IndexSet}` instead of `std::collections::{HashMap, HashSet}` to preserve JS insertion order.
  - Use `pi_js::json` instead of `serde_json::to_string*` / `to_vec*` / `to_writer*` outside `crates/pi-js`.
  - Use `pi_js::console` instead of `println!` / `eprintln!` / `print!` / `eprint!` outside fixture binaries.
  - Use `pi_js::env` instead of `std::env::set_var` / `std::env::remove_var`.
  - Use `tokio::time` or `pi_js::time` instead of `std::thread::sleep` / `std::time::Instant` in timing logic.
  - Create Tokio `current_thread` runtimes only in binary `run()` entrypoints; library code must not call `block_on`.
  - Never call `.unwrap()` on untrusted user, file, or network data. Workspace lint sets `unsafe_op_in_unsafe_fn = "deny"`.
- **Scaffold and port markers**: Preserve `// @generated-mods begin ... end` blocks in module roots where present, and annotate intentional Rust deviations from TS with `// PORT: <reason>`.
- **Runtime package assets**: Do not move, rename, or delete `crates/pi-coding-agent/package/` (`docs/`, `examples/`, `README.md`, `CHANGELOG.md`, `package.json`). `pi_coding_agent::config::{get_docs_path, get_readme_path, get_changelog_path, get_examples_path}` resolve those paths at runtime, and `pi-evals` audits them.
- **Rune dependencies**: Root `Cargo.toml` pins `https://github.com/anomalyco/rune.git` at one exact `dev` commit across all `rune*` `[workspace.dependencies]` and all 5 `[patch.crates-io]` crates (`wgpu-hal`, `winit`, `parley_engine`, `accesskit_windows`, `accesskit_macos`). Never commit machine-local paths (`/Users/`, `/Volumes/`, `C:/Users/`).
- **UI thread boundary in `pi-gui`**: Keep all `pi-coding-agent` session work, file scanning, git diffs, and shell commands in the background `GuiBridge` worker (`crates/pi-gui/src/bridge/`), never on the Rune render/UI thread.

## Commands

Requires Rust 1.98+ (`edition = "2024"`). See [`docs/building-and-testing.md`](docs/building-and-testing.md) for OS-specific build dependencies (Linux `libxkbcommon-dev` / `libwayland-dev`, Windows MSVC vs GNU `dlltool`).

```sh
# Type-check all workspace crates, binaries, examples, and tests
cargo check --workspace --all-targets

# Build the four workspace binaries (pi, pi-gui, pi-ai, pi-evals)
cargo build --workspace --bins

# Run offline tests for a crate or the full workspace (uses faux providers + pi_js::seam)
cargo test -p pi-gui
cargo test --workspace

# Check formatting (rustfmt.toml: edition = "2024", max_width = 120)
cargo fmt --check

# Render a deterministic pi-gui scene headlessly to PNG via Rune
cargo run -p pi-gui -- --scene dark-session --snap /tmp/pi-gui-dark.png
```

## Rules

- Keep prose concise and technical; do not use emojis in commits, PRs, docs, or code.
- Tests must stay offline and deterministic: use `pi_ai::providers::faux` and `pi_js::seam` instead of live API calls or paid tokens.
- Stage explicit paths (`git add <path>...`); never run `git add -A`, `git add .`, `git reset --hard`, `git checkout .`, `git clean -fd`, `git stash`, or `git commit --no-verify`.
- Do not commit, force-push, or open/merge PRs unless requested by the user.
