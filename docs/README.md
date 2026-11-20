# Documentation

Start with the guide for your area. Repository rules and day-to-day commands live in [AGENTS.md](../AGENTS.md).

## Engineering Guides

- [Architecture](architecture.md) — workspace crate graph (`pi-js`, `pi-schema`, runtime, and client crates), execution model, `pi-js` runtime contract, vendored libraries, and storage/wire parity
- [Desktop GUI (`pi-gui`)](gui.md) — Rune UI layout, `GuiBridge` worker, deterministic `--scene` / `--snap` verification, and Rune dependency pin maintenance
- [Extensions & Plugins](extensions.md) — QuickJS WASM extension host (`.ts`/`.js`), virtual modules, native Rust `InlineExtension` API, and built-in extensions
- [Building & Testing](building-and-testing.md) — Rust toolchain, OS build dependencies, offline faux-provider testing, and `pi-evals`
- [AGENTS.md](../AGENTS.md) — workspace invariants, coding conventions, and git rules

## User & CLI Documentation

User documentation shipped with the `pi` package lives in [`crates/pi-coding-agent/package/docs/`](../crates/pi-coding-agent/package/docs/index.md) (also resolved at runtime via `pi_coding_agent::config::get_docs_path`):

- **Getting Started & CLI**
  - [Overview](../crates/pi-coding-agent/package/docs/index.md) · [Quickstart](../crates/pi-coding-agent/package/docs/quickstart.md) · [How Pi Works](../crates/pi-coding-agent/package/docs/how-pi-works.md)
  - [CLI Reference](../crates/pi-coding-agent/package/docs/cli.md) · [Interactive Usage](../crates/pi-coding-agent/package/docs/usage.md) · [Slash Commands](../crates/pi-coding-agent/package/docs/slash-commands.md) · [Keybindings](../crates/pi-coding-agent/package/docs/keybindings.md)
  - [Settings](../crates/pi-coding-agent/package/docs/settings.md) · [Configuration](../crates/pi-coding-agent/package/docs/configuration.md) · [Environment Variables](../crates/pi-coding-agent/package/docs/environment-variables.md)
  - [Terminal Setup](../crates/pi-coding-agent/package/docs/terminal-setup.md) · [Shell Aliases](../crates/pi-coding-agent/package/docs/shell-aliases.md) · [tmux](../crates/pi-coding-agent/package/docs/tmux.md) · [Windows](../crates/pi-coding-agent/package/docs/windows.md) · [Termux](../crates/pi-coding-agent/package/docs/termux.md)
- **Providers & Models**
  - [Models](../crates/pi-coding-agent/package/docs/models.md) · [Models Configuration](../crates/pi-coding-agent/package/docs/models-configuration.md) · [Providers](../crates/pi-coding-agent/package/docs/providers.md) · [Custom Providers](../crates/pi-coding-agent/package/docs/custom-provider.md) · [Virtual Models](../crates/pi-coding-agent/package/docs/virtual-models.md) · [Local `llama.cpp` Models](../crates/pi-coding-agent/package/docs/llama-cpp.md) · [RTX Local Models](../crates/pi-coding-agent/package/docs/nvidia-rtx-llama-cpp.md)
- **Sessions, Context & Security**
  - [Sessions](../crates/pi-coding-agent/package/docs/sessions.md) · [Session Format](../crates/pi-coding-agent/package/docs/session-format.md) · [Compaction](../crates/pi-coding-agent/package/docs/compaction.md) · [Message Types](../crates/pi-coding-agent/package/docs/message-types.md)
  - [Security](../crates/pi-coding-agent/package/docs/security.md) · [Containerization](../crates/pi-coding-agent/package/docs/containerization.md)
- **Customization & Extensions**
  - [Extensions](../crates/pi-coding-agent/package/docs/extensions.md) · [Codemode](../crates/pi-coding-agent/package/docs/codemode.md) · [MCP](../crates/pi-coding-agent/package/docs/mcp.md) · [Skills](../crates/pi-coding-agent/package/docs/skills.md) · [Prompt Templates](../crates/pi-coding-agent/package/docs/prompt-templates.md) · [Themes](../crates/pi-coding-agent/package/docs/themes.md) · [Packages](../crates/pi-coding-agent/package/docs/packages.md)
- **Automation & Integration**
  - [JSON Event Stream](../crates/pi-coding-agent/package/docs/json.md) · [RPC Protocol](../crates/pi-coding-agent/package/docs/rpc.md) · [RPC Commands](../crates/pi-coding-agent/package/docs/rpc-commands.md) · [RPC Extension UI](../crates/pi-coding-agent/package/docs/rpc-extension-ui.md)
  - [CLI Integration](../crates/pi-coding-agent/package/docs/cli-integration.md) · [SDK](../crates/pi-coding-agent/package/docs/sdk.md) · [TUI Components](../crates/pi-coding-agent/package/docs/tui.md)

## Porting Archive

Historical design documents from the v1.0.0 TypeScript-to-Rust port:

- [`porting/PORTING.md`](porting/PORTING.md) — original port specification, naming mapping, and parity contract
- [`porting/port-map.json`](porting/port-map.json) — machine-readable mapping from upstream TS files to Rust module paths
- [`porting/HANDOFF.md`](porting/HANDOFF.md) — historical mid-port checkpoint
