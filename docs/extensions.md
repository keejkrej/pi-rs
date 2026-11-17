# Extensions & Plugins

`pi-rs` supports three extension surfaces inside `crates/pi-coding-agent`:

1. **QuickJS WASM extensions** (`.ts`, `.js`, `.mjs`, `.cjs`) executed inside `pi-codemode` (`quickjs-wasi` on Wasmtime)
2. **Native Rust extensions** implementing `InlineExtension` / `ExtensionApi`
3. **Built-in extensions** compiled directly into `pi-coding-agent`

## QuickJS WASM Extension Host

Implemented under `crates/pi-coding-agent/src/core/extensions/`:

- **`jiti_loader.rs`**: Pure-Rust lexer and erasable-TypeScript stripper (type annotations, interfaces, `type` aliases, `as`/`satisfies`, `import type`, parameter modifiers) plus CommonJS-to-ESM wrapping so existing `.ts` and `.js` Pi extensions load without Node.js or `jiti`.
- **`virtual_modules.rs`**: Resolves virtual module imports inside QuickJS and bridges calls back to Rust via `VmHostBridge` promises:
  - Pi packages: `@earendil-works/pi-coding-agent`, `@mariozechner/pi-coding-agent`, `@earendil-works/pi-ai` (`/compat`, `/models`, `/oauth`, `/providers/all`), `@earendil-works/pi-tui`, `@earendil-works/pi-agent-core`
  - Schema validation: `typebox`, `typebox/value`, `typebox/compile` (`@sinclair/typebox*`) backed by `pi_js::vendor::typebox`
  - Node built-ins: `node:path`, `node:os`, `node:fs`, `node:fs/promises`, `node:events`, `node:url`, `node:crypto`, `node:module`, `node:child_process` (and bare aliases without `node:`)
- **`loader.rs`**: Discovers extensions from `~/.pi/agent/extensions`, `<cwd>/.pi/extensions`, installed Pi packages, and explicit `--extension` / `-e` flags; initializes each module inside a `QuickJS` VM; wires `ExtensionActions`, `ExtensionContextActions`, tool execution, commands, event handlers, and `pi.events` re-entrant turns.

Non-erasable TypeScript syntax (such as runtime `enum` or `namespace`) is rejected; extensions using standard erasable TS syntax run unmodified.

## Native Rust Extensions

Rust callers can embed or launch `pi` with native Rust extensions using `pi_coding_agent::cli::run_with`:

```rust
use std::process::ExitCode;

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![/* InlineExtension factories */])
}
```

`ExtensionApi` (`crates/pi-coding-agent/src/core/extensions/types.rs`) exposes tool registration (`register_tool`), custom commands (`register_command`), CLI flags (`register_flag`), event hooks (`on(...)`), shortcuts, message renderers, and UI context prompts (`ExtensionUiContext`).

See `crates/pi-coding-agent/examples/extensions__*.rs` for runnable native examples (custom providers, commands, compaction, dynamic tools, plan mode, doom overlay, sandbox, subagent, and bash hooks).

## Built-in Extensions

`crates/pi-coding-agent/src/extensions/` ships native ports of the four built-in extensions (selectable via `--extension builtin:<name>` or loaded automatically unless `--no-extensions` is set):

- **`llama`** (`src/extensions/llama.rs`) — local `llama.cpp` model lifecycle and provider wiring
- **`codemode`** (`src/extensions/codemode.rs`) — sandboxed QuickJS WASM code-execution tool powered by `pi-codemode`
- **`tool_search`** (`src/extensions/tool_search.rs`) — semantic/keyword tool discovery across large tool sets
- **`mcp`** (`src/extensions/mcp.rs`) — MCP server integration and tool/resource bridging via `pi-mcp`
