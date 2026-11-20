# pi-rs

A pure-Rust implementation of the [Pi](https://github.com/earendil-works/pi) v1.5.2 coding agent harness, pairing CLI/wire/session parity with embedded QuickJS WASM extensions and a native GPU desktop client (`pi-gui`).

## Overview

- **Multi-mode harness**: Interactive TUI (`pi`), native GPU desktop GUI (`pi gui` / `pi-gui` built with [Rune](https://github.com/anomalyco/rune)), non-interactive print/JSON modes (`pi -p`, `pi --mode json`), and JSONL RPC (`pi --mode rpc`).
- **Drop-in compatibility**: Preserves upstream Pi's `~/.pi/agent` directory layout (`settings.json`, `auth.json`, `models.json`, session `.jsonl`, lockfiles), RPC JSONL stream, and Chord CBOR protocol.
- **No Node.js required**: Built-in `pi-js` runtime semantics, Rust-native provider clients (`pi-ai`), and an embedded `quickjs-wasi` host (`pi-codemode`) execute `.ts`/`.js`/`.mjs`/`.cjs` Pi extensions alongside native Rust `InlineExtension` plugins and built-in `llama`, `codemode`, `tool-search`, and `mcp` extensions.

## Quick Start

Requires Rust **1.98+** (edition 2024). See [Building and Testing](docs/building-and-testing.md) for Linux and Windows system library prerequisites.

```sh
cargo build --release --bins
```

Binaries are written to `target/release/`:

```sh
# Interactive terminal session
./target/release/pi

# Non-interactive print mode
./target/release/pi -p "Summarize the workspace crates"

# JSONL RPC server over stdin/stdout
./target/release/pi --mode rpc

# Launch the native desktop GUI (via pi subcommand/flag or directly)
./target/release/pi gui
./target/release/pi --gui
./target/release/pi-gui --demo

# Render a deterministic desktop GUI scene headlessly to PNG
./target/release/pi-gui --scene dark-session --snap /tmp/pi-gui.png
```

## Binaries

| Binary | Crate | Purpose |
|---|---|---|
| `pi` | [`crates/pi-coding-agent`](crates/pi-coding-agent) | Coding agent CLI (`text`, `json`, `rpc`, interactive TUI, `pi gui` launcher) |
| `pi-gui` | [`crates/pi-gui`](crates/pi-gui) | Native GPU desktop client (Rune + `rune-kit` + Waku dark/light theme) |
| `pi-ai` | [`crates/pi-ai`](crates/pi-ai) | Standalone OAuth login and credential CLI |
| `pi-evals` | [`crates/pi-evals`](crates/pi-evals) | Documentation audit and Docker evaluation runner |

## Workspace Crates

| Crate | Path | Responsibility |
|---|---|---|
| `pi-js` | [`crates/pi-js`](crates/pi-js) | JS/Node compatibility runtime (JSON, UTF-16, Intl, fetch, fs, path, TypeBox, jsdiff) |
| `pi-schema` | [`crates/pi-schema`](crates/pi-schema) | Eval-free JSON Schema (Draft 2020-12 / Draft-07) validator, coercer, and type builders |
| `pi-telemetry` | [`crates/pi-telemetry`](crates/pi-telemetry) | Telemetry contracts, schemas, and in-memory test adapter |
| `pi-chord` | [`crates/pi-chord`](crates/pi-chord) | Chord object runtime (contracts, store, documents, links, HTTP/WS) |
| `pi-tui` | [`crates/pi-tui`](crates/pi-tui) | Differential terminal UI, markdown/mermaid rendering, image protocols |
| `pi-mcp` | [`crates/pi-mcp`](crates/pi-mcp) | Model Context Protocol client, stdio/HTTP transports, OAuth management |
| `pi-codemode` | [`crates/pi-codemode`](crates/pi-codemode) | Sandboxed `quickjs-wasi` runtime on Wasmtime |
| `pi-ai` | [`crates/pi-ai`](crates/pi-ai) | Multi-provider LLM streaming API, model registry, OAuth, faux provider |
| `pi-protocol` | [`crates/pi-protocol`](crates/pi-protocol) | Chord CBOR protocol envelopes and codecs |
| `pi-agent-core` | [`crates/pi-agent-core`](crates/pi-agent-core) | Core agent loop, tool execution, and steering/follow-up queues |
| `pi-client` | [`crates/pi-client`](crates/pi-client) | Chord Unix socket and WebSocket client transports |
| `pi-server` | [`crates/pi-server`](crates/pi-server) | Chord Unix socket and WebSocket server runtime |
| `pi-durable` | [`crates/pi-durable`](crates/pi-durable) | Durable conversation, task, and document storage (SQLite + Chord) |
| `pi-coding-agent` | [`crates/pi-coding-agent`](crates/pi-coding-agent) | Coding agent session runtime, built-in tools, modes, and extension host |
| `pi-gui` | [`crates/pi-gui`](crates/pi-gui) | Native Rune desktop client with Diff, Files, and Terminal inspector |
| `pi-evals` | [`crates/pi-evals`](crates/pi-evals) | Automated documentation audit and eval harness |

## Documentation

- [Documentation Index](docs/README.md) — entry point for engineering and user guides
- [Architecture](docs/architecture.md) — crate layering, execution model, `pi-js`, and on-disk/wire formats
- [Desktop GUI (`pi-gui`)](docs/gui.md) — layout, scenes, `--snap` headless rendering, and Rune pin maintenance
- [Extensions & Plugins](docs/extensions.md) — QuickJS WASM extension host, virtual modules, and native Rust API
- [Building & Testing](docs/building-and-testing.md) — platform prerequisites, check/test commands, and offline test harness
- [Agent & Contributor Rules](AGENTS.md) — invariants, coding constraints, and repository layout
- [Pi User & CLI Reference](crates/pi-coding-agent/package/docs/index.md) — sessions, models, settings, RPC, SDK, and skills

## Development

```sh
cargo check --workspace --all-targets
cargo test --workspace
cargo fmt --check
```

## License

MIT — see [LICENSE](LICENSE).
