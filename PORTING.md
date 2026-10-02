# PORTING.md: pi v1.0.0 (TypeScript) to Rust

Every porting agent must follow these conventions. In this document:

- `$TS` is `/Users/jack/.t3/worktrees/pi/t3code-ef43beb3`, the TS monorepo at v1.0.0. It is read-only and is the behavioral source of truth.
- `$RS` is `/Users/jack/workspace/pi-rs`, the target.

When something is observable (bytes on disk or on the wire, CLI behavior, terminal output, error messages), TS behavior overrides this document. Where Rust idiom conflicts with this document, this document wins.

## 0. Ground rules

1. Port behavior 1:1: the same functions, branches, defaults, messages and ordering. Do not redesign, merge or "improve" anything, and do not add features. Nothing is dropped except the items listed in §2.6.
2. Observable output must be byte-identical (§13).
3. Every exported TS item gets a `pub` Rust item named by §3. Items TS does not export become private, or `pub(crate)` if another module uses them.
4. Read the whole TS file, plus the types it imports, before you write anything. Look up third-party behavior in `$TS/node_modules` (SDK headers, error formats, defaults). Never guess.
5. Port TSDoc to `///`/`//!` verbatim; reflowing is allowed. Keep the TS inline comments. The only comments you add are `// PORT: <reason>` notes that explain a forced deviation.
6. Banned outside the listed exceptions:

   | Banned | Use instead | Exceptions |
   |---|---|---|
   | `serde_json::to_string*` / `to_vec*` / `to_writer*` | `pi_js::json` | inside `crates/pi-js` |
   | `println!` / `eprintln!` / `print!` / `eprint!` | `pi_js::console` | fixture bins |
   | `HashMap` / `HashSet` | `IndexMap` / `IndexSet` | none |
   | `std::env::set_var` / `remove_var` | `pi_js::env` | none |
   | `std::thread::sleep` and `std::time::Instant` for timing logic | `tokio::time` / `pi_js::time` | none |
   | `block_on` | — | `run()` entry functions only |
   | `unsafe` | — | platform FFI (§9) |
   | `.unwrap()` on data from users, files or the network | `?` or explicit handling | none |

## 1. Crate layout

```
$RS/
  Cargo.toml  Cargo.lock  rustfmt.toml  .gitignore   scaffold-owned
  PORTING.md
  legacy/                 old workspace (Cargo.toml, Cargo.lock, crates/{pi-agent, pi-ai, pi-coding-agent, pi-tui, pi-web-ui}): reference only, excluded from the workspace
  port-map.json           scaffold-owned TS-to-Rust file map with unit ids (§2.10)
  interop/gen/            TS golden generators (§13)
  crates/<crate>/         one crate per TS package, plus pi-js
```

| TS package (`$TS/packages/…`) | Crate dir and package name | lib | Internal deps | Wave |
|---|---|---|---|---|
| (none; Rust-only foundation) | `crates/pi-js` | `pi_js` | none | W0 |
| `telemetry` | `crates/pi-telemetry` | `pi_telemetry` | pi-js | W1 |
| `chord` | `crates/pi-chord` | `pi_chord` | pi-js | W1 |
| `tui` | `crates/pi-tui` | `pi_tui` | pi-js | W1 |
| `mcp` | `crates/pi-mcp` | `pi_mcp` | pi-js | W1 |
| `codemode` | `crates/pi-codemode` | `pi_codemode` | pi-js | W1 |
| `ai` | `crates/pi-ai` | `pi_ai` | pi-js, pi-telemetry | W2 |
| `protocol` | `crates/pi-protocol` | `pi_protocol` | pi-js, pi-chord | W2 |
| `agent` | `crates/pi-agent-core` | `pi_agent_core` | pi-js, pi-ai | W3 |
| `client` | `crates/pi-client` | `pi_client` | pi-js, pi-chord, pi-protocol | W3 |
| `server` | `crates/pi-server` | `pi_server` | pi-js, pi-chord, pi-protocol | W3 |
| `durable` | `crates/pi-durable` | `pi_durable` | pi-js, pi-chord, pi-ai | W3 |
| `coding-agent` | `crates/pi-coding-agent` | `pi_coding_agent` | all of the above | W4 |
| `evals` | `crates/pi-evals` | `pi_evals` | pi-js, pi-ai, pi-coding-agent | W5 |

**Waves.** Agents in the same wave run in parallel. A wave starts only when every crate it depends on passes `cargo check -p <crate> --all-targets` with zero errors. Rust examples and the live CLI diff (§13.3) belong to W5; golden interop tests are written in the wave of the format's owner.

**pi-js.** This is a Rust-only crate. It provides JS runtime semantics (JSON, numbers, UTF-16 strings, AbortSignal, timers, env, Node `path`/`fs`, fetch, Intl, console) plus vendored ports of npm libraries that more than one crate uses. Its API is the cross-agent contract in Appendix A. W0 agents implement it; nobody else edits it.

**Binaries.** Each TS bin entry file becomes a module with `pub fn run() -> std::process::ExitCode`. A 3-line wrapper calls it:

| Binary | Wrapper | Calls |
|---|---|---|
| `pi` | `crates/pi-coding-agent/src/bin/pi.rs` | `pi_coding_agent::cli::run()` |
| `pi-ai` | `crates/pi-ai/src/bin/pi-ai.rs` | `pi_ai::cli::run()` |
| `pi-evals` | `crates/pi-evals/src/bin/pi-evals.rs` | `pi_evals::cli::run()` |

`run()` builds `tokio::runtime::Builder::new_current_thread().enable_all().build()`, calls `pi_js::ensure_crypto_provider()`, runs the async main with `block_on`, and maps `Err(Error::Exit(c))` to `ExitCode::from(c)`. The current-thread runtime mirrors Node's single event loop: tasks interleave only at `.await`. Library code never creates a runtime.

**Phase 0 scaffold.** The orchestrator runs this once, before W0. Agents can rely on everything it produces:

1. `git mv` the old crates to `legacy/`.
2. Write the workspace and crate `Cargo.toml` files (§8) and `rustfmt.toml` (`edition = "2024"`, `max_width = 120`).
3. Create every Rust file that §2 maps. Each one starts as a stub containing only `//! Port of <TS path relative to $TS>` and, in module roots, the generated mod block (§2.3). Two exceptions keep `--all-targets` compiling (§2.10): binary roots (examples, fixture bins) also contain `fn main() {}`, and the three `cli.rs` files contain a `run()` placeholder.
4. Copy all assets, fixtures and model data (§2.9).
5. Write the `src/bin/*` wrappers.
6. Run `cargo generate-lockfile && cargo check --workspace --all-targets`, which must be green. The scaffold may fix feature lists in §8; versions are binding.

## 2. File mapping (deterministic)

### 2.1 Name conversion `snake(x)`

1. Apply heck `to_snake_case` semantics: `-`, `.` and space become `_`; camelCase boundaries get `_`; acronyms stay together (`parseJSONLine` becomes `parse_json_line`); lowercase; repeated `_` collapse.
2. If the result is a Rust keyword (strict, reserved, or 2024 `gen`) or `main`, `lib` or `mod`, append `_`. Examples: `box.ts` becomes `box_.rs`, `main.ts` becomes `main_.rs`.
3. If it starts with a digit, prefix `n`. Example: `00-conversation.ts` becomes `n00_conversation.rs`.

### 2.2 Source files

- `packages/<pkg>/src/<dirs…>/<file>.ts` maps to `crates/<crate>/src/<snake(dirs)…>/<snake(stem)>.rs`. Compound suffixes are part of the stem: `anthropic-messages.lazy.ts` becomes `anthropic_messages_lazy.rs`, and `anthropic.models.ts` becomes `anthropic_models.rs`.
- Module layout follows the 2018 style: `foo.rs` plus `foo/`. Never use `mod.rs`; the one exception is `tests/support/mod.rs` (§2.7).
- `src/index.ts` maps to `src/lib.rs`. `src/<dir>/index.ts` maps to `src/<dir>.rs`.
- If both `x.ts` and the directory `x/` exist (`ai/src/compat.ts`, `chord/src/node.ts`, `coding-agent/src/cli.ts`), the content of `x.ts` goes in `x.rs`, which also carries the generated mod block for `x/`.
- A directory with neither `index.ts` nor a sibling `.ts` gets a scaffold-owned `<dir>.rs` that contains only the mod block.
- Module tree visibility: every module is `pub mod` (mirroring TS deep imports), and every TS `export` is `pub`.
- How TS exports translate:

  | TS | Rust |
  |---|---|
  | `export * from "./x"` | `pub use crate::…::x::*;` |
  | `export { a as b }` | `pub use …::a as b;` |
  | `export type` re-exports | same as the value form |
  | `export default <named item>` | keep the name |
  | `export default <expr>` | `pub fn default_export() -> T` |

### 2.3 Module roots and generated blocks

Module roots (`lib.rs`, `<dir>.rs`) contain one scaffold-owned block, listed alphabetically:

```rust
// @generated-mods begin (scaffold-owned, do not edit)
pub mod foo;
pub mod bar;
// @generated-mods end
```

The owner of the TS file that maps onto the root file (`index.ts` or `x.ts`) writes its content below the block. Nobody edits the block or adds `mod` lines. If you need private structure, use inline modules (`mod helpers { … }`) inside your own file. Never create files outside your mapping.

### 2.4 TS imports to Rust paths

- Relative imports become `crate::<snake path>::Item`.
- Package imports `@earendil-works/<pkg>` become `pi_<x>::Item`; the crate root re-exports what `index.ts` exports.
- Subpath exports resolve through that package's `package.json` `exports` map to a file, and the file maps to a module path: `pi-ai/providers/faux` → `pi_ai::providers::faux`, `pi-server/unix` → `pi_server::transports::unix`, `chord/context` → `pi_chord::context`, `pi-durable/storage/sqlite/node` → `pi_durable::storage::sqlite::node`.
- `/testing` subpaths are always compiled (no cargo features), marked `#[doc(hidden)]`.

### 2.5 Split files

The orchestrator may split a TS file over 2500 lines into line ranges cut at top-level declaration boundaries (for example `interactive-mode.ts`, `agent-session.ts`, `package-manager.ts`).

- Part 1 owns `<stem>.rs`. It holds every type, struct, enum and constant of the whole TS file, the class's struct fields, and the functions/methods in range 1.
- Part k (k ≥ 2) owns `<stem>/part_<k>.rs`. It holds `impl <Type> { … }` blocks and free functions for its range.
- The scaffold declares `mod part_k; pub use part_k::*;` inside `<stem>.rs`'s generated block.
- Struct fields that later parts need are `pub(crate)`.

### 2.6 Dispositions (not ported 1:1)

| TS | Rust disposition |
|---|---|
| `*.d.ts`, `tsconfig*`, `vitest*.config.ts`, `packages/*/scripts/**` (including `generate-models.ts`) | Not ported. Model data JSON is synced from `$TS` by copying (§2.9). |
| `coding-agent/src/bun/**`, `experimental/source-resolver.ts`, `tui/src/native-module-path.ts`, `tui/native/**` (C/ObjC/N-API) | Not ported. The native addon is replaced by direct FFI in `native_platform.rs` / `native_modifiers.rs` (§9). |
| `core/extensions/jiti-loader.ts`, `jiti-static-loader.ts`, `virtual-modules.ts` | Ported as stubs. The JS loading path fails with the §10.6 error. |
| `chord/src/bundler.ts`, `chord/src/node/bundle-loader.ts`, `chord/src/facets/loader.ts` JS-bundle loading, `coding-agent/src/experimental/plugins/package.ts` bundling | Signatures are ported. Bodies return `Error::js("Error", "JavaScript facet bundles are not supported by pi-rs")`. |
| Worker entry files (`codemode/src/runtime/worker.ts`, `extensions/codemode/worker.ts`, `utils/image-resize-worker.ts`) | `runtime/worker.rs` is the body of a dedicated `std::thread` (codemode). Image resize runs in `tokio::task::spawn_blocking` inside `image_resize.rs`. The other worker files are not ported. |
| `*.lazy.ts` | Ported as thin direct-call wrappers with the same exported names (no dynamic import). |
| Module top-level side effects (registration on import) | Idempotent `pub fn ensure_<what>()` guarded by `std::sync::Once`, called by the accessor that needs it. Module state uses `static X: LazyLock<Mutex<T>>`. |
| `photon-node` image ops | The `image` crate. Tests assert dimensions and format, never encoded bytes. |
| `highlight.js` | `syntect` + `two-face`, with scopes mapped to hljs class names so that `renderHighlightedHtml` and the theme mapping port 1:1. Tests asserting exact hljs tokenization are rewritten to assert stripped text and the presence of styling (`// PORT:` note). |
| `esbuild` | Unsupported (see the chord row). |

npm libraries without a byte-compatible Rust crate are hand-ported. Each port goes in `src/vendor/<snake(npm name)>.rs` (or a dir), owned by its assigned agent, who may add files inside that vendor dir:

| npm package (version) | Rust location |
|---|---|
| `typebox` 1.3.27 (Type, Compile, Value.Convert/Check/Errors, en_US messages) | `pi-js/src/vendor/typebox` |
| `diff` 8.0.4 (jsdiff) | `pi-js/src/vendor/jsdiff` |
| `partial-json` 0.1.7 | `pi-ai/src/vendor/partial_json.rs` |
| `marked` 18.0.11 (lexer) | `pi-tui/src/vendor/marked` |
| `get-east-asian-width` 1.6.0 | `pi-tui/src/vendor/east_asian_width.rs` |
| `quickjs-wasi` 3.6.2 (JS glue, env imports, WASI shim, WTF-8) | `pi-codemode/src/vendor/quickjs_wasi` |
| `chalk` 6.0.0 (+ `supports-color`) | `pi-coding-agent/src/vendor/chalk.rs` |
| `proper-lockfile` 4.1.2 | `pi-coding-agent/src/vendor/proper_lockfile.rs` |
| `hosted-git-info` 9.0.3 | `pi-coding-agent/src/vendor/hosted_git_info.rs` |
| `minimatch` 10.2.6 (default options + `nocase`) | `pi-coding-agent/src/vendor/minimatch.rs` |
| `grok-mermaid` 0.2.3 | `pi-coding-agent/src/vendor/grok_mermaid` |
| `cross-spawn` | Not ported: `which` crate plus PATHEXT handling inside `utils/child_process.rs` |

Crate replacements for the remaining npm libraries: `semver` → `node-semver`; `yaml` (parse only) → `serde_yaml_ng` into `serde_json::Value`; `ignore` → `ignore::gitignore`; `undici` → `pi_js::fetch` / `tokio-tungstenite`; `@aws-sdk/client-bedrock-runtime` → `aws-sdk-bedrockruntime`.

### 2.7 Tests

- `packages/<pkg>/test/<dirs…>/<name>.test.ts` maps to `crates/<crate>/tests/<snake(dir1)>__…__<snake(name)>.rs`. Directory segments are joined with `__`. Each file is its own integration-test binary. Examples:
  - `test/suite/regressions/2023-queued-slash-command-followup.test.ts` becomes `tests/suite__regressions__n2023_queued_slash_command_followup.rs`.
  - `test/session-manager/foo.test.ts` becomes `tests/session_manager__foo.rs`.
- Non-test `.ts` under `test/` (harnesses, helpers, `durable/test/examples/*`) maps to `tests/support/<snake path>.rs`. The scaffold generates `tests/support/mod.rs` (the one sanctioned `mod.rs`), which starts with `#![allow(dead_code, unused_imports)]` and holds the full `pub mod` tree. Test files that need helpers write `mod support;`.
- Non-TS files under `test/` (fixtures, data, conformance JSON) are copied byte-for-byte to the same relative path under `tests/`; directory names are not snake-cased. Reference them as `concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/…")`.
- Node fixture scripts `test/fixtures/<name>.mjs` become Rust bins `crates/<crate>/src/bin/fixture-<name>.rs`. Tests launch them with `env!("CARGO_BIN_EXE_fixture-<name>")`. They are owned by the test file's owner. The same applies to `.ts` fixtures that are process entry points: `coding-agent/test/fixtures/faux-session-worker.ts` becomes `src/bin/fixture-faux-session-worker.rs`.
- In `tests/support/mod.rs`, subdirectories are inline modules (`pub mod suite { pub mod harness; }` loads `tests/support/suite/harness.rs`).
- **Shared and split test files.** A test file listed by more than one unit (including `#L` line-range splits such as `tools.test.ts#L1-484`) gets one Rust file per unit, so no two agents write the same file. The first unit (in UNITS order) owns `tests/<name>.rs`. Unit k ≥ 2 owns `tests/<name>/part_<k>.rs`, which the scaffold declares in the root's generated block as `#[path = "<name>/part_<k>.rs"] mod part_<k>;`. A part that needs support modules declares `#[path = "../support/mod.rs"] mod support;` itself. Shared support files keep one path; the first unit owns it (port-map `owner` field on the other entries).
- E2E tests (any case that needs real provider credentials or the public network) are not ported. If a file mixes such cases with others, port only the non-gated cases.

### 2.8 Examples

- `packages/coding-agent/examples/<dirs…>/<name>.ts` maps to `crates/pi-coding-agent/examples/<snake(dirs)>__<snake(name)>.rs`.
- A directory example with `index.ts` (or `main.ts`) maps to `examples/<snake(dirs)>__<snake(dir)>/main.rs`, with its other files as sibling modules declared in `main.rs`'s generated block. Example: `agent/examples/mcp-codemode/{main,tools}.ts` become `crates/pi-agent-core/examples/mcp_codemode/{main,tools}.rs`. Directories without either entry (`plugins/pi-example-plugin/src/*.ts`) map per file.
- `packages/agent/examples/**` maps the same way into `crates/pi-agent-core/examples/`.
- Extension examples are binaries: `fn main() -> ExitCode { pi_coding_agent::cli::run_with(vec![…]) }` (§10.7).
- If an example cannot work in Rust (it depends on npm packages or JS assets, e.g. `doom-overlay`, `with-deps`), write a `main` that prints `"<name>: not available in pi-rs: <reason>"` and exits with code 1.

### 2.9 Assets

All assets are copied by the scaffold.

| Source (`$TS`) | Destination (`$RS`) | Access |
|---|---|---|
| `packages/ai/src/providers/data/*.json` | `crates/pi-ai/src/providers/data/` | `include_str!` in `<provider>_models.rs`, parsed once into a `LazyLock` (order preserved) |
| Non-`.ts` files under `packages/<pkg>/src/**` (themes `dark.json`, `light.json`, `theme-schema.json`; `export-html/template.{html,css,js}`, `vendor/*.min.js`; `assets/clankolas.png`; READMEs) | Same relative path; directory names snake-cased (`export_html/`), file names unchanged | `include_str!` / `include_bytes!` |
| `node_modules/quickjs-wasi/quickjs.wasm` | `crates/pi-codemode/assets/quickjs.wasm` | `include_bytes!` in `wasm.rs`; compiled once per process into a `OnceLock<wasmtime::Module>` |
| `packages/coding-agent/{package.json, README.md, CHANGELOG.md, docs/**, examples/**}` | `crates/pi-coding-agent/package/` | Read from disk at runtime through `config::get_package_dir()` |
| Every non-`.ts`, non-`.mjs` file under `packages/<pkg>/test/` | `crates/<crate>/tests/<same path>` | §2.7 |
| `packages/evals/docker/{Dockerfile, Dockerfile.dockerignore, install-runtime.mjs}` | `crates/pi-evals/docker/` | Rewritten by the evals-runner owner for the Rust binaries |
| `packages/mcp/LICENSES/*` | `crates/pi-mcp/LICENSES/` | License of the ported MCP SDK code |

`get_package_dir()` resolves in this order:

1. `PI_PACKAGE_DIR`.
2. The executable's directory, if it contains `package.json` (release archive = binary + `package/` contents, the TS Bun-binary layout).
3. `concat!(env!("CARGO_MANIFEST_DIR"), "/package")`.

`is_bun_binary` semantics hold: themes, export-html and assets dirs resolve as `<pkg>/theme`, `<pkg>/export-html` and `<pkg>/assets`. Built-in theme, template and wasm content is always taken from the embedded bytes. `VERSION`, `APP_NAME` and `CONFIG_DIR_NAME` come from `<pkg>/package.json` as in TS; `VERSION` falls back to `env!("CARGO_PKG_VERSION")`, which is `1.0.0`.

### 2.10 Scaffold specifics and `port-map.json`

Mappings beyond §2.2-§2.9:

- `packages/evals/evals/*.ts` (eval suites and their fixtures) map to `crates/pi-evals/src/evals/<snake(stem)>.rs`, under a scaffold-owned `src/evals.rs` root. Example: `smoke.eval.ts` becomes `src/evals/smoke_eval.rs`.
- `packages/evals/docker/entrypoint.ts` maps to `crates/pi-evals/src/docker/entrypoint.rs`; `docker.rs` carries its mod block.
- `packages/coding-agent/src/client/index.ts` maps to `src/client.rs` (`pub use pi_client::*;`). No unit lists it, so its port-map entry has `unit: null`.
- `packages/evals` has no `src/index.ts`, so `crates/pi-evals/src/lib.rs` holds only the generated block. The `evals/src/cli.ts` owner adds the §5 re-export `pub use pi_js::{Error, Result};` below the block.

Compile placeholders written by the scaffold:

- Binary roots (`examples/*.rs`, `examples/*/main.rs`, `src/bin/fixture-*.rs`) end with `fn main() {}`. The owner replaces it.
- `cli.rs` in pi-coding-agent, pi-ai and pi-evals contains `pub fn run() -> std::process::ExitCode { unimplemented!(…) }` between `// @scaffold-placeholder begin` and `// @scaffold-placeholder end`. The `cli.ts` owner replaces the whole block with the real `run()`. The `src/bin/*` wrappers call it.

`port-map.json` is a JSON array with three kinds of entries:

| Kind | Fields |
|---|---|
| Source | `{ts, rust, unit}`. `ts` keeps any `#L<a>-L<b>` suffix. `rust` is the owned Rust file or the copied asset path. It is `null` for files that are not ported, and then `disposition` gives the §2.6 reason. |
| Vendor | `{ts: "node_modules/<npm>", rust, unit, vendor: "<npm>@<version>"}` for the §2.6 vendor ports. `unit` is a suggested owner; `"pi-js"` means W0. |
| Test | `{ts_test, rust_test, unit}`, plus `owner` when the file is shared and another unit owns it. `rust_test` ends with `/` for copied fixture directories. |

pi-js modules (Appendix A) have no TS source and no port-map entry; W0 owns every file under `crates/pi-js/src/`.

## 3. Naming and serde

### 3.1 Identifiers

| TS | Rust |
|---|---|
| function / method / variable / property `camelCase` | `snake(x)` without the keyword suffix; keep `get_`/`set_`/`is_` prefixes exactly (`getAgentDir` becomes `get_agent_dir`) |
| class / interface / type alias / enum | same name verbatim (`RPCClient` stays `RPCClient`) |
| `const FOO_BAR` | `FOO_BAR` (`const` if const-evaluable, otherwise `static LazyLock`) |
| field named like a keyword | raw identifier `r#type`, `r#ref`, `r#match`; `self`/`super`/`crate` get a `_` suffix plus `#[serde(rename)]` |
| getter `get x()` / setter `set x(v)` | `fn x(&self)` / `fn set_x(&self, v)` |
| overloads | one fn per overload, suffixed by the distinguishing parameter (`on` is generic, §10) |
| optional parameter / default parameter | `Option<T>` parameter; the default is applied inside |
| inline options-object type of `fooBar(opts?)` | `struct FooBarOptions` (`#[derive(Default, Clone)]`), passed by value |

### 3.2 Types

| TS | Rust |
|---|---|
| `string` | `String` (params `&str`) |
| `number` that is integral and written only by pi (timestamps ms, counts, tokens, indices, ports, exit codes) | `i64` (`usize` only for never-serialized in-memory indices) |
| `number` otherwise (costs, ratios, user-edited JSON) | `f64` |
| `boolean` | `bool` |
| `unknown` / `any` / generic payloads (`TDetails`, `Static<T>`) | `serde_json::Value` |
| `T \| undefined`, `x?: T`, `T \| null` | `Option<T>` |
| `x?: T \| null`, where absent, `null` and value are all distinct | `Option<Option<T>>` with `#[serde(default, skip_serializing_if = "Option::is_none", with = "pi_js::json::double_option")]` |
| `x?: unknown` | `Option<Value>` (`Some(Value::Null)` = `null`) |
| `x?: T[]` | `Option<Vec<T>>` (empty ≠ absent) |
| `Record<string, T>`, `Map<K, V>` | `IndexMap<K, V>` |
| `Set<T>` | `IndexSet<T>` |
| JS `delete` / `Map.delete` / `Set.delete` | `shift_remove` (never `remove` / `swap_remove`: serde_json's `Map::remove` swap-removes) |
| `Uint8Array` / `Buffer` | `Vec<u8>` (`bytes::Bytes` for stream chunks) |
| `Date` | `i64` epoch ms; ISO text via `pi_js::time::iso_string` |
| string literal union | `#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)] enum` with an explicit `#[serde(rename = "…")]` per variant |
| discriminated union `{type: "a", …} \| …` | `#[serde(tag = "type")]` enum, explicit `rename` per variant, `#[serde(rename_all_fields = "camelCase")]` |
| union with no discriminator | `#[serde(untagged)]` enum, variants ordered as TS checks them |
| class with identity / shared mutable state | handle pattern (§4.4) |
| callback `(a) => R` / `(a) => Promise<R>` | `Arc<dyn Fn(A) -> R + Send + Sync>` / `Arc<dyn Fn(A) -> BoxFuture<R> + Send + Sync>` |
| returned unsubscribe `() => void` | `pi_js::Unsubscribe` (dropping it does not unsubscribe) |
| TS-only generics (`Model<TApi>`, `AgentTool<TParams, TDetails>`) | erased to concrete types (`api: String`, `Value`) |

### 3.3 Serde rules (byte compatibility)

- Every serialized struct uses `#[serde(rename_all = "camelCase")]`. If the camelCase of the Rust field does not reproduce the TS key exactly (acronyms, snake_case wire keys, keys with digits), add an explicit `#[serde(rename = "…")]`.
- Field order equals the key insertion order of the TS object literal that produces the object. Look at the construction site, not the interface declaration. If construction sites disagree, build a `serde_json::Map` in each site's order instead of using a struct.
- Every `Option` field gets `skip_serializing_if = "Option::is_none"` (JSON.stringify drops `undefined`). Never use `deny_unknown_fields`.
- Data read from disk, the network or users must deserialize at least as permissively as TS:
  - Missing fields become `None` or `Default`.
  - Unknown fields are ignored.
  - Enums deserialized from such sources get a catch-all variant `Unknown(serde_json::Value)` that re-serializes the raw value unchanged (implement `Deserialize` via `Value`: match `type`, else `Unknown`).
- Files that TS read-modify-writes (settings.json, auth.json, models.json, trust/keybindings/MCP configs, experimental meta.json) are manipulated as `serde_json::Map` at the persistence layer with JS spread semantics. `{...a, ...b}` is: clone `a`, then insert each key of `b`; existing keys keep their position. Typed views are derived from the map.
- Copying entries verbatim (fork, migrate, export) copies the raw `Value` or raw line, never a re-serialized typed struct.

## 4. Async, cancellation, shared state

### 4.1 Functions

- A TS `async function` becomes `async fn`; a sync function stays sync.
- Sync `fs` calls map to `pi_js::fs` (sync); `fs/promises` maps to `pi_js::fs::promises`.
- Every async trait method uses `#[async_trait]`, so all traits stay `dyn`-compatible.

| TS | Rust |
|---|---|
| `Promise.all` | `futures::future::try_join_all` (or `join_all`), preserving order |
| `Promise.allSettled` | `join_all` over `Result`s |
| `Promise.race` | `tokio::select! { biased; … }`, arms in array order |
| `void promise` / unawaited fire-and-forget | `tokio::spawn` |
| `queueMicrotask` / `process.nextTick` / `setImmediate` where ordering matters | `tokio::task::yield_now().await` |
| `setTimeout` / `clearTimeout` | `pi_js::time::set_timeout(ms, f) -> Timeout`, `.clear()` |
| `setInterval` / `clearInterval` | `pi_js::time::set_interval` |
| `await sleep(ms, signal)` | `pi_js::time::sleep(ms, signal).await?` |

Never use dropping a future as a stand-in for abort when TS keeps the operation running. `raceWithAbortSignal` stops waiting but spawns and keeps the operation.

### 4.2 Cancellation

- `AbortController` / `AbortSignal` become `pi_js::abort::{AbortController, AbortSignal}`. This is a `CancellationToken` plus a stored `AbortReason`. Listeners run synchronously, in registration order, inside `abort()`. Registering on an already-aborted signal does not fire.
- `signal?: AbortSignal` becomes `Option<AbortSignal>`. Also: `AbortSignal::any`, `AbortSignal::timeout`, `signal.throw_if_aborted()?`.
- Abort messages are exact: default `This operation was aborted` (name `AbortError`); timeout `The operation was aborted due to timeout` (name `TimeoutError`); a custom reason keeps its own message (e.g. `Request aborted`, `Login cancelled`).

### 4.3 Streams and events

- `EventStream<T, R>` (`pi_ai::utils::event_stream`) is a FIFO with waiters behind a `Mutex<VecDeque>` plus oneshot senders. It implements `futures::Stream<Item = T>`. `push`/`end` are sync, and `result()` is async through a `OnceCell`.
- Provider `stream*` functions return the stream synchronously after `tokio::spawn`ing the producer. Setup failures become `error` events or messages, exactly as TS; they are never an `Err` return.
- Anything TS delivers in a guaranteed order goes through one ordered sink: agent events, serialized listeners, parallel tool results, the session-router per-client chain, socket write tails, the durable mutation log. That sink is a single task fed by `tokio::sync::mpsc::unbounded_channel`, or a sync call under one mutex.
- Listener calls are sync when TS ignores the return value, and `BoxFuture` when TS awaits it.

### 4.4 Shared mutable state: the handle pattern

TS classes that are referenced from several places become:

```rust
#[derive(Clone)]
pub struct Agent { inner: Arc<AgentInner> }
struct AgentInner { state: std::sync::Mutex<AgentState>, /* … */ }
```

- Methods take `&self`. Use `std::sync::Mutex` / `RwLock` with `.lock().unwrap()`.
- Rule: lock, read or mutate, unlock, then await or invoke callbacks. Never hold a guard across `.await` or while calling a callback, because user callbacks re-enter.
- `tokio::sync::Mutex` is used only for TS serialization chains (`this.chain = this.chain.then(…)`).
- Plain data (messages, entries, configs, events) are `Clone` value structs.
- TUI components follow the same pattern (§9.4).

### 4.5 Time, env, process

- `Date.now()` becomes `pi_js::time::now_ms()`; `performance.now()` becomes `pi_js::time::performance_now()`. Both are fakeable.
- `process.env.X` becomes `pi_js::env::var("X")`; writes become `pi_js::env::set_var`, which is an overlay that also flows into spawned children through `pi_js::env::vars()`.
- `os.homedir()` / `tmpdir()` / `process.cwd()` / `process.platform` / `process.arch` become `pi_js::env::{home_dir, tmp_dir, cwd, platform, arch}`.
- Child processes use `tokio::process::Command` with `.env_clear().envs(pi_js::env::vars())`. Detached process groups on unix use `process_group(0)`, and the group is killed with `nix::sys::signal::killpg`; on Windows use `taskkill /T /F`, as TS does.
- Signals use `tokio::signal`.
- `process.exit(c)` below the entry point becomes `return Err(pi_js::Error::Exit(c))`, propagated to `run()`.
- `console.*` and `process.stdout.write` become `pi_js::console`.

## 5. Errors

- **Shared carrier.** All library crates use `pi_js::Error` / `pi_js::Result<T>`, re-exported in each `lib.rs` (`pub use pi_js::{Error, Result};`), so no conversions are needed between crates. It is a thiserror enum modeling JS `throw` (Appendix A): `Js(JsError)`, `Abort(AbortReason)`, `Node(NodeError)` (fs/net/child errors in Node format), `Typed(name, Box<dyn Error>)`, `Exit(i32)`.
- **Display** of every variant equals the JS `error.message`. `String(err)` / `${err}` is `err.to_js_string()`, i.e. `"<name>: <message>"`.
- **Throws:** `throw new Error(m)` becomes `return Err(Error::msg(m))`; `new TypeError(m)` becomes `Error::js("TypeError", m)` (same for RangeError and SyntaxError).
- **TS `Error` subclasses** become a `#[derive(Debug, thiserror::Error)]` struct per class, defined in the file that defines the TS class (thiserror per crate), with the same fields and `#[error("{message}")]`. Throw with `Err(Error::typed("ClassName", MyError { … }))`. `instanceof` becomes `err.downcast_ref::<MyError>()`.
- **Catch:** `try/catch` becomes `match` / `if let Err(e)`; `catch {}` becomes `let _ = …;` or `.ok()`; `e instanceof Error ? e.message : String(e)` becomes `e.to_string()`; `e.code === "ENOENT"` becomes `e.code() == Some("ENOENT")`; `e.name === "AbortError"` becomes `e.is_abort()`.
- **Node fs/net/spawn errors** must keep Node's exact message text (e.g. `ENOENT: no such file or directory, open '/x'`, `EACCES: permission denied, mkdir '/y'`). Use `pi_js::fs`, which builds `NodeError`. Do not surface raw `std::io::Error` text.
- **App layer.** Only `run()` entry functions and `src/bin/*` may use `anyhow`, to report top-level failures.
- **No panics** for any condition TS handles. `unwrap()`/`expect()` are allowed only for true invariants such as static regex compilation or poisoned locks.

## 6. JSON Schema and TypeBox

- TypeBox is hand-ported in `pi_js::vendor::typebox`. The builder (`Type.*`) produces JSON Schema whose keys are byte-identical, in the same order, to TypeBox 1.3.27 output. Verified examples: `Object` is `{"type":"object","required":[…],"properties":{…},…opts}` (`required` omitted when every property is optional); `Literal("a")` is `{"type":"string","const":"a"}`; `Record(String, X)` is `{"type":"object","patternProperties":{"^.*$":X}}`; `Union` is `{"anyOf":[…],…opts}`; `Optional` is not serialized.
- `pi_ai::schema` is `pub use pi_js::vendor::typebox as schema;` in `pi-ai/src/lib.rs`. Every crate writes schemas with it:

```rust
use pi_js::vendor::typebox::{t, Schema};
let params: Schema = t::object([
    ("path", t::string().with("description", "Path to the file")),
    ("limit", t::number().with("minimum", 1).optional()),
]);
```

`Schema` is a `serde_json::Value` object with TypeBox key order, plus non-serialized flags (`optional`, `typebox: true` when built through `t::*`, `false` when parsed from JSON). It serializes as the bare JSON Schema. Builders: `object`, `string`, `number`, `integer`, `boolean`, `null`, `any`, `unknown`, `literal`, `array`, `tuple`, `union`, `intersect`, `record`, `enum_`; `.with(key, value)` appends options in call order; `.optional()`.

**Validation is not delegated to the `jsonschema` crate,** because error text must match:

- `Compile(schema)` becomes `typebox::compile(&schema) -> Validator`, with `.check(&Value) -> bool` and `.errors(&Value) -> Vec<ValueError { instance_path, schema_path, keyword, message }>`. Messages are TypeBox en_US.
- `Value.Convert` / `Check` / `Errors` / `Clean` / `Default` become `typebox::value::*`.
- Patterns (`pattern`, `patternProperties`) are ECMAScript regexes compiled with `regress` via `pi_js::regex::ecma`.
- `pi_ai::utils::validation` (tool-argument coercion and validation) ports `validation.ts` on top of this.

Tool parameters cross tool boundaries as `serde_json::Value`. After validation, a tool deserializes its own typed params privately with `serde_json::from_value::<P>(params)`.

## 7. HTTP and providers

- **The `fetch` seam.** Every HTTP call in every crate goes through `pi_js::fetch::fetch(Request) -> Result<Response>` (Appendix A). This mirrors the WHATWG fetch that TS SDKs use and makes `globalThis.fetch` mocks portable. Responses expose `status`, `status_text`, `headers` (lowercased names), `text()`, `json()`, `bytes()`, `body_stream()` and `sse()`: the spec-compliant EventSource line parser (CR/LF/CRLF, multi-line `data` joined with `\n`, `event`/`id`/`retry`, comments ignored, BOM stripped).
- **The default reqwest client** is built once: HTTP/1.1 only (`http1_only()`, as Node undici), rustls, automatic gzip/br/deflate/zstd decoding, `connect_timeout(10s)` and `read_timeout(300s)` (undici `connectTimeout` / `bodyTimeout`), no total timeout. It uses `.no_proxy()`; proxies are added only as `coding-agent/src/core/http-dispatcher.ts` and `ai/src/utils/node-http-proxy.ts` dictate (env `HTTP(S)_PROXY`/`NO_PROXY` logic ported exactly into a `reqwest::Proxy::custom`). `http-dispatcher.ts` installs it with `pi_js::fetch::set_default_client`.
- **Network errors** surface as `TypeError: fetch failed` with the cause, as undici does.
- **Official SDKs** (`@anthropic-ai/sdk`, `openai`, `@google/genai`) are replaced by hand-written clients in their `api/*.rs` files. Each adapter owner reads `$TS/node_modules/<sdk>` and replicates what the SDK does on the wire and in errors: URL joining and query params; every header the SDK sends, including `user-agent` and `x-stainless-*` (package version from the SDK's `package.json`, `runtime=node`, `runtime-version=v24.21.0`, os/arch mapped as the SDK does); body serialization via `pi_js::json::stringify`; the default timeout; SDK retry logic when pi does not pass `maxRetries: 0`; SDK-specific SSE handling (`[DONE]`, `event: error`, `ping`); and the `APIError` message format (e.g. Anthropic `"{status} {json body}"`) with `status`, `headers` and `request_id` fields, exposed as `Error::typed("APIError", …)`.
- **OpenAI Codex WebSocket** and `experimental/radius-relay.ts` use `tokio-tungstenite` (rustls native roots), connected after `pi_js::ensure_crypto_provider()`. zstd request bodies use `zstd` level 3.
- **Bedrock** uses `aws-sdk-bedrockruntime` `converse_stream` with `aws-config` (`behavior-version-latest`) for the full credential chain, region and profile resolution. The adapter builds the command input as a `serde_json::Value` in exactly the TS `ConverseStreamCommand` input shape; one function converts it to the SDK builder (tool schemas and tool input become `aws_smithy_types::Document`). Stream events are converted back to `Value` in the TS SDK shape before the TS-ported handling. The test seam is a `BedrockTransport` slot (§12.3) that captures the input `Value` and returns scripted event `Value`s.
- **Google Generative AI and Vertex** are hand REST calls (`:streamGenerateContent?alt=sse`). Vertex ADC (`GOOGLE_APPLICATION_CREDENTIALS`, gcloud ADC file, metadata server) uses `gcp_auth::provider()`; the API-key mode is ported as in TS.
- **OAuth** callback servers and other local HTTP servers use `hyper` 1 + `hyper-util` on `127.0.0.1`. Browsers are opened by spawning commands exactly as `open-browser.ts` does.
- **Retry-After** parsing uses `httpdate`. Rate-limit and backoff logic is ported 1:1 with `pi_js::time::sleep` (fake-timer compatible).

## 8. Dependencies

The workspace manifest below is binding; the scaffold writes it. Agents never edit any `Cargo.toml` (§11).

```toml
[workspace]
resolver = "3"
members = ["crates/*"]
exclude = ["legacy"]

[workspace.package]
version = "1.0.0"
edition = "2024"
rust-version = "1.98"
license = "MIT"
authors = ["Mario Zechner"]
publish = false

[workspace.lints.rust]
unsafe_op_in_unsafe_fn = "deny"

[profile.dev]
debug = "line-tables-only"
[profile.dev.package."*"]
opt-level = 1
[profile.dev.package.cranelift-codegen]
opt-level = 3

[workspace.dependencies]
pi-js = { path = "crates/pi-js" }
pi-telemetry = { path = "crates/pi-telemetry" }
pi-chord = { path = "crates/pi-chord" }
pi-tui = { path = "crates/pi-tui" }
pi-mcp = { path = "crates/pi-mcp" }
pi-codemode = { path = "crates/pi-codemode" }
pi-ai = { path = "crates/pi-ai" }
pi-protocol = { path = "crates/pi-protocol" }
pi-agent-core = { path = "crates/pi-agent-core" }
pi-client = { path = "crates/pi-client" }
pi-server = { path = "crates/pi-server" }
pi-durable = { path = "crates/pi-durable" }
pi-coding-agent = { path = "crates/pi-coding-agent" }
# async
tokio = { version = "1.53.1", features = ["full"] }
tokio-util = { version = "0.7.19", features = ["codec", "io", "rt"] }
futures = "0.3.34"
async-trait = "0.1.92"
pin-project-lite = "0.2.17"
bytes = "1.12.1"
# serialization
serde = { version = "1.0.229", features = ["derive", "rc"] }
serde_json = { version = "1.0.151", features = ["preserve_order", "float_roundtrip"] }
indexmap = { version = "2.14.2", features = ["serde"] }
ryu-js = "1.0.3"
serde_yaml_ng = "0.10.0"
# errors
thiserror = "2.0.21"
anyhow = "1.0.104"
# text
regex = "1.13.1"
fancy-regex = "0.19.2"
regress = "0.12.0"
icu_segmenter = "2.3.0"
icu_collator = "2.3.1"
unicode-normalization = "0.1.25"
# http / net
reqwest = { version = "0.13.5", default-features = false, features = ["rustls", "charset", "stream", "gzip", "brotli", "deflate", "zstd"] }
rustls = "0.23.45"
url = "2.5.8"
form_urlencoded = "1.2.2"
percent-encoding = "2.3.2"
hyper = { version = "1.11.1", features = ["http1", "server"] }
hyper-util = { version = "0.1.21", features = ["tokio", "server", "http1"] }
http-body-util = "0.1.5"
tokio-tungstenite = { version = "0.30.0", features = ["rustls-tls-native-roots"] }
httpdate = "1.0.3"
# cloud
aws-config = { version = "1.12.0", features = ["behavior-version-latest"] }
aws-sdk-bedrockruntime = "1.148.0"
aws-smithy-types = "1.8.1"
gcp_auth = "0.12.7"
# crypto / ids / encoding
sha2 = "0.11.0"
base64 = "0.23.1"
hex = "0.4.3"
rand = "0.10.3"
getrandom = "0.4.3"
uuid = { version = "1.26.1", features = ["v4"] }
# time
chrono = { version = "0.4.45", default-features = false, features = ["clock", "std"] }
# compression
zstd = "0.14.0"
flate2 = "1.1.10"
crc32fast = "1.5.2"
# fs / process / packages
tempfile = "3.27.0"
filetime = "0.2.29"
notify = "8.2.0"
which = "8.0.6"
ignore = "0.4.33"
node-semver = "2.2.0"
# terminal / media
crossterm = "0.29.0"
image = { version = "0.25.10", default-features = false, features = ["png", "jpeg", "gif", "webp", "bmp"] }
syntect = { version = "5.3.0", default-features = false, features = ["parsing", "default-syntaxes", "regex-fancy"] }
two-face = { version = "0.5.2", default-features = false, features = ["syntect-fancy"] }
arboard = { version = "3.6.1", default-features = false, features = ["image-data", "wayland-data-control"] }
# storage / wasm
rusqlite = { version = "0.40.2", features = ["bundled"] }
wasmtime = { version = "49.0.1", default-features = false, features = ["cranelift", "runtime", "std", "parallel-compilation"] }
# platform
nix = { version = "0.31.3", features = ["signal", "process", "term", "fs", "user", "hostname", "feature"] }
libc = "0.2.189"
objc2 = "0.6.4"
objc2-app-kit = { version = "0.3.2", features = ["NSEvent"] }
windows-sys = { version = "0.61.2", features = ["Win32_Foundation", "Win32_System_Console", "Win32_System_Threading", "Win32_UI_Input_KeyboardAndMouse", "Win32_System_SystemInformation", "Win32_Storage_FileSystem", "Win32_System_Pipes"] }
x11rb = "0.14.0"
# dev
pretty_assertions = "1.4.1"
serial_test = "4.0.1"
```

One crate per need; these are the only choices. JSON: serde_json via `pi_js::json`. JS numbers: ryu-js. Regex: regress (ECMAScript), fancy-regex (lookaround/backreferences), regex (plain). Intl.Segmenter: icu_segmenter. localeCompare: icu_collator (root). Crypto randomness: getrandom; Math.random: rand. Local time formatting: chrono. File watching: notify. Executable lookup: which. gitignore: ignore. npm semver: node-semver. YAML: serde_yaml_ng. `node:sqlite`: rusqlite bundled. Wasm: wasmtime (no wasmtime-wasi; the WASI shim is ported from the quickjs-wasi glue). Terminal raw mode and size: crossterm (stdin bytes are parsed by the ported `keys.ts` / `stdin-buffer.ts`, never by crossterm events). Clipboard: arboard. Images: image.

Every crate manifest has `[package]` with `*.workspace = true`, `[lib] doctest = false` (TSDoc code blocks are not Rust), `[lints] workspace = true`, and dev-deps `tokio = { workspace = true, features = ["test-util"] }`, `pretty_assertions`, `serial_test`, `tempfile`.

Common deps of every crate except pi-js: pi-js, anyhow, thiserror, async-trait, futures, pin-project-lite, bytes, tokio, tokio-util, serde, serde_json, indexmap, regex, fancy-regex, base64, sha2, hex, rand, uuid, chrono, url. Extra deps per crate:

| Crate | Extra deps |
|---|---|
| pi-js | the common list minus pi-js, plus reqwest, rustls, form_urlencoded, percent-encoding, ryu-js, regress, icu_segmenter, icu_collator, unicode-normalization, httpdate, getrandom, filetime, tempfile; unix: nix, libc; windows: windows-sys |
| pi-telemetry, pi-chord | none |
| pi-tui | crossterm, image, arboard; unix: nix, libc; macos: objc2, objc2-app-kit; linux: x11rb; windows: windows-sys |
| pi-mcp | reqwest, hyper, hyper-util, http-body-util, which, getrandom, form_urlencoded, percent-encoding; unix: nix, libc; windows: windows-sys |
| pi-codemode | wasmtime, getrandom |
| pi-ai | pi-telemetry, reqwest, tokio-tungstenite, zstd, flate2, hyper, hyper-util, http-body-util, aws-config, aws-sdk-bedrockruntime, aws-smithy-types, gcp_auth, httpdate, getrandom, percent-encoding, form_urlencoded |
| pi-protocol | pi-chord |
| pi-agent-core | pi-ai; dev: pi-mcp, pi-codemode (for `examples/mcp_codemode`) |
| pi-client, pi-server | pi-chord, pi-protocol; unix: nix, libc; windows: windows-sys |
| pi-durable | pi-chord, pi-ai, rusqlite, tempfile; unix: nix, libc (process-group kill in the env tools, §4.5) |
| pi-coding-agent | every internal crate, plus reqwest, tokio-tungstenite, crossterm, image, syntect, two-face, ignore, node-semver, serde_yaml_ng, notify, filetime, tempfile, which, zstd, flate2, crc32fast, arboard, hyper, hyper-util, http-body-util, httpdate, getrandom, percent-encoding, form_urlencoded; unix: nix, libc; windows: windows-sys |
| pi-evals | pi-ai, pi-agent-core, pi-coding-agent, which, tempfile, hyper, hyper-util, http-body-util (local `acme-server` fixture, §7); unix: nix, libc (root sandbox `setgroups`/`setgid`/`setuid`) |

If you need something that is missing, do not add it: see NEEDS_DEPS in §11.

## 9. Platform

1. **Runtime checks vs `cfg`.** TS runtime checks (`process.platform === "win32"`) become runtime checks on `pi_js::env::platform()` (`"darwin" | "linux" | "win32"`), which tests can override. Use `#[cfg(...)]` only where code cannot compile everywhere (FFI, unix sockets, `process_group`, named pipes). Every `cfg(unix)` item needs a `cfg(windows)` sibling, or a fallback that behaves like TS when the native capability is absent.
2. **Paths** are `String` throughout, exactly as in TS. Node `path` semantics come from `pi_js::path` (platform default, plus `posix` / `win32` modules for code or tests that use `path.posix` / `path.win32`). Convert to `std::path::Path` only at OS calls. Never use `PathBuf::join` for logic that TS does with `path.join` / `resolve`.
3. **Unix sockets** (pi-server, pi-client, experimental) use `tokio::net::UnixListener` / `UnixStream` under `cfg(unix)` and `tokio::net::windows::named_pipe` under `cfg(windows)`. Socket paths and the stale-socket probing logic are ported 1:1.
4. **TUI.** crossterm is used only for raw mode, `size()` and Windows VT enabling. Stdin is read as raw bytes by a dedicated reader task and fed to the ported `StdinBuffer` / `keys` parser. Resize uses `tokio::signal::unix::signal(SignalKind::window_change())`, or polling on Windows as in TS. Output is the exact escape strings TS writes, through one buffered writer. `Component` is a trait with `&self` methods (`render(&self, width: usize) -> Vec<String>`, `handle_input(&self, data: &str)`, `handle_mouse`, `wants_key_release`, `invalidate`). Components keep their state in an internal `Mutex` and follow the §4.4 lock/unlock-then-callback rule. Containers hold `Arc<dyn Component>`; subclassing (`class Box extends Container`) becomes composition plus delegation.
5. **Native addon replacement.** Modifier state (`native-modifiers.ts`): macOS `NSEvent::modifierFlags()` (objc2-app-kit), Linux X11 `x11rb` `QueryPointer` mask, Windows `GetAsyncKeyState`; on failure return `None` exactly as TS does when the addon is unavailable. Native clipboard image/text: arboard. Command-based clipboard paths (pbcopy, wl-copy, xclip, xsel, powershell, termux, OSC 52) are ported 1:1 from `utils/clipboard*.ts`.
6. **Width and segmentation.** Character width comes from the ported `get-east-asian-width` table; never use `unicode-width`. Grapheme and word segmentation (`Intl.Segmenter`) use `pi_js::intl` (icu_segmenter). Indices are converted to UTF-16 where TS exposes `SegmentData.index`.
7. **Unsafe FFI** lives only in `native_platform.rs`, `native_modifiers.rs` and the Windows console/process helpers. Every `unsafe` block carries a `// SAFETY:` comment.

## 10. Extension API (native Rust)

All of this lives in `crates/pi-coding-agent/src/core/extensions/types.rs`, owned by the `types.ts` agent. The signatures below are binding.

### 10.1 Factory

```rust
#[async_trait]
pub trait Extension: Send + Sync + 'static {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()>;
}
#[async_trait]
impl<F, Fut> Extension for F
where F: Fn(ExtensionApi) -> Fut + Send + Sync + 'static,
      Fut: Future<Output = pi_js::Result<()>> + Send + 'static
{ async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> { (self)(pi).await } }

pub type ExtensionFactory = Arc<dyn Extension>;                 // TS ExtensionFactory
pub enum InlineExtension { Factory(ExtensionFactory), Named(NamedInlineExtension) }
pub struct NamedInlineExtension { pub name: String, pub factory: ExtensionFactory,
    pub hidden: bool, pub replaceable: bool, pub builtin: bool }
```

### 10.2 `ExtensionApi`

`ExtensionApi` (TS `ExtensionAPI`) is a `Clone` handle. Every `ExtensionAPI` member becomes a snake_case method with the same semantics, for example `register_tool(ToolDefinition)`, `register_command(&str, RegisteredCommandOptions)`, `register_shortcut(KeyId, ShortcutOptions)`, `register_flag`, `get_flag -> Option<FlagValue>`, `register_message_renderer`, `register_markdown_transformer`, `register_entry_renderer`, `send_message`, `send_user_message`, `append_entry`, `set_session_name`, `get_session_name`, `set_label`, `exec -> ExecResult` (async), `get_active_tools`, `get_all_tools`, `set_active_tools`, `get_commands`, `set_model` (async), `get_thinking_level`, `set_thinking_level`, `events() -> EventBus`.

### 10.3 Events

The overloaded `on(event, handler)` becomes one generic method:

```rust
pub trait ExtensionEvent: Clone + Send + Sync + 'static {
    const TYPE: &'static str;   // exact TS event name: "session_start", "tool_call", "before_provider_request", …
    type Output: Send + 'static; // TS handler result R; `()` when R is undefined
}
impl ExtensionApi {
    pub fn on<E: ExtensionEvent>(&self,
        handler: impl Fn(E, ExtensionContext) -> BoxFuture<pi_js::Result<Option<E::Output>>> + Send + Sync + 'static,
    ) -> Unsubscribe;
}
```

- Each event is a struct named as the TS interface (`SessionStartEvent`, `ToolCallEvent`, …) with the TS fields except `type`, plus `impl ExtensionEvent`. Results are structs named as in TS (`ToolCallEventResult`, …). Special handler types (`project_trust`) get dedicated methods (`on_project_trust`).
- The runner (`runner.rs`) stores handlers per `E::TYPE` in registration order and ports the chaining, short-circuit and error-reporting semantics of `runner.ts` exactly.

### 10.4 Context

`ExtensionContext`, `ExtensionCommandContext`, `ExtensionToolContext` and `ReplacedSessionContext` are `Clone` handles.

- Every accessor that can throw the stale error in TS returns `pi_js::Result<T>`.
- The stale check compares the captured generation with the runner's and fails with the exact `runner.ts` `invalidate()` message.
- `ctx.ui()` returns `Arc<dyn ExtensionUiContext>`, an `#[async_trait]` trait with every `ExtensionUIContext` member. There are three impls: interactive (TUI), RPC (UI requests over JSONL) and no-op (print / no UI).
- `custom()` takes a factory `Box<dyn FnOnce(TuiHandle, Theme, KeybindingsManager, DoneFn) -> Arc<dyn pi_tui::Component> + Send>` and resolves to `serde_json::Value`.

### 10.5 Tools

`ToolDefinition` is a `Clone` struct with every TS field, so that `{...tool, execute: wrapped}` becomes struct-update syntax.

- `parameters: Schema`, `output_schema: Option<Schema>`.
- `execute: Arc<dyn Fn(String, Value, Option<AbortSignal>, Option<AgentToolUpdateCallback>, ExtensionToolContext) -> BoxFuture<pi_js::Result<AgentToolResult>> + Send + Sync>`.
- Optional `prepare_arguments`, `prepare_loadout`, `render_call`, `render_result` as `Option<Arc<dyn Fn…>>`; render functions return `Arc<dyn pi_tui::Component>`. The per-tool render state (`TState`) is `ToolRenderContext.state: Arc<Mutex<Option<Box<dyn Any + Send>>>>`.

`AgentTool` in pi-agent-core follows the same closure-field struct pattern. Built-in tools (`core/tools/*`) are constructed the same way. Shared types (`ToolDefinition`, `AgentToolResult`, details structs, operations interfaces) stay in the modules TS defines them in.

### 10.6 Built-ins and user extensions

- `crate::extensions::built_in_extensions() -> Vec<InlineExtension>` returns, in TS order: llama.cpp (builtin), codemode (replaceable, builtin), tool-search (replaceable, builtin), mcp (replaceable, builtin).
- Each built-in is ported natively from `src/extensions/<name>/` and exports `create_<name>_extension(options) -> ExtensionFactory` plus `default_export()`.
- User TS/JS extensions are never executed. Discovery, settings, `pi config` listing, package resolution and enable/disable all work as in TS, but loading any non-builtin, non-inline path yields the load result `error: "Failed to load extension: <path>: TypeScript/JavaScript extensions are not supported by pi-rs"`. Downstream diagnostics wrap it exactly as TS does.
- Packages keep working for skills, prompts and themes.

### 10.7 Embedding

- `pi_coding_agent::main_::main(args: Vec<String>, options: Option<MainOptions>) -> pi_js::Result<()>` ports TS `main`; `MainOptions.extension_factories: Vec<InlineExtension>`.
- `cli::run_with(extensions: Vec<InlineExtension>) -> ExitCode` (Rust-only, owned by the `cli.ts` agent) is the entry point for custom binaries and the Rust examples.

## 11. Rules for parallel agents

1. **Ownership.** You own exactly the Rust files mapped (§2) from your assigned TS files: src, tests, support, fixture bins, examples, and any vendor dir assigned to you. You may read anything; you write only what you own.
2. **Never edit** any `Cargo.toml`, `Cargo.lock`, `rustfmt.toml` or `.cargo/`, generated mod blocks, copied assets and fixtures, `legacy/`, `PORTING.md`, or another agent's files.
3. **Missing items.**
   - **Dependencies.** Do not add a dependency. Implement the narrow spot with available crates, or with `unimplemented!("NEEDS_DEPS <crate>: <purpose>")`. List it under `NEEDS_DEPS:` in your final report as `crate = "version" — why`.
   - **Assets and fixtures.** Report a missing one under `MISSING_ASSETS:`.
   - **Items owned by other agents.** Reference another agent's item by the deterministic path and name from §2 and §3, even if it does not exist yet. Never stub or duplicate it in your file. List it under `WAITING_ON:` with the Rust path.
4. **Checking.** Run from `$RS`:

   ```
   cargo check -p <crate> --all-targets --message-format=short 2>&1 | grep -E '^crates/<crate>/(src|tests|examples)/<your paths>'
   ```

   Fix every error in your files; errors elsewhere are not yours. If the check fails inside an upstream crate you get no diagnostics: keep porting from the TS source and Appendix A, and report `BLOCKED_UPSTREAM: <crate>`. "Blocking waiting for file lock" is normal: wait (timeout of 15 minutes or more), and never kill other cargo processes.
5. **Running tests.** Run only your own binaries: `cargo test -p <crate> --test <binary>` (fixture bins first need `cargo build -p <crate> --bin fixture-<name>`). Never run the full workspace test suite or anything network-dependent.
6. **Banned commands:** `cargo clean`, `cargo update`, `cargo fmt` (workspace-wide), `cargo fix`, `cargo clippy --fix`, `cargo add` / `cargo remove`; `git stash` / `reset` / `checkout` / `restore` / `clean` / `add` / `commit` / `rebase` / `pull` / `push`. Allowed: `rustfmt --edition 2024 <your files>` and read-only git (`status`, `diff`, `log`, `show`).
7. **Final report** (plain text, no files): files written; `STATUS: complete|partial`; `NEEDS_DEPS:`; `WAITING_ON:`; `BLOCKED_UPSTREAM:`; `MISSING_ASSETS:`; `DEVIATIONS:` (every observable difference from TS, with its `// PORT:` location); `SKIPPED_TESTS:` (name and reason).
8. **Completeness.** Port every function, branch and test case. No `todo!()` left in your files at `STATUS: complete`.

## 12. Tests

1. **Structure.**

   | vitest | Rust |
   |---|---|
   | `describe("X")` | `mod snake(X)` (nested) |
   | `it/test("does y")` | `fn snake(does y)`, truncated to 80 chars; duplicates get `_2`, `_3` |
   | `it.skip` / `it.todo` | `#[ignore = "<reason>"]` |
   | `it.each(table)` | one fn looping over the table; the case label goes into assert messages |
   | `beforeEach` / `afterEach` | a fixture struct built at the start of each test, with `Drop` for cleanup |
   | `beforeAll` | a `LazyLock` / `tokio::sync::OnceCell` per binary |

   Async tests use `#[tokio::test]` (current-thread; never `flavor = "multi_thread"`). Sync tests use `#[test]`. Add `#[serial_test::serial]` only to tests that touch real process-global state.
2. **Assertions.**

   | vitest | Rust |
   |---|---|
   | `toBe` / `toEqual` / `toStrictEqual` | `pretty_assertions::assert_eq!` (typed values, or `serde_json::json!` values) |
   | `toMatchObject` | `pi_js::testing::assert_json_matches(&actual, &expected)` (subset match) |
   | `toThrow("m")` | error `to_string()` contains `m` |
   | `toThrow(/re/)` | regex match |
   | `toContain` | `contains` |
   | `expect.any(T)` | type check on the `Value` |
   | inline snapshot | a literal expected string |
3. **Mocks become seams.**
   - **Global replacements.** `vi.stubGlobal("fetch")` / `vi.spyOn(globalThis, "fetch")` become `pi_js::fetch::testing::mock_fetch(handler)` (thread-local guard). `vi.mock("openai" | "@anthropic-ai/sdk" | "@google/genai")` uses the same `mock_fetch`, asserting on the captured request JSON body and headers and returning canned SSE bodies. `vi.mock("@aws-sdk/client-bedrock-runtime")` uses the `BedrockTransport` slot.
   - **Other modules.** For `vi.mock` / `vi.spyOn` on any other module function, the module owning that function exposes a thread-local override slot:

     ```rust
     thread_local! { static READ_CLIPBOARD: pi_js::seam::Slot<dyn Fn(&str) -> pi_js::Result<String> + Send + Sync> = pi_js::seam::Slot::new(); }
     pub fn read_clipboard(cmd: &str) -> pi_js::Result<String> {
         if let Some(f) = READ_CLIPBOARD.with(|s| s.get()) { return f(cmd); }
         read_clipboard_real(cmd)
     }
     pub mod testing { pub fn override_read_clipboard(f: impl Fn(&str) -> pi_js::Result<String> + Send + Sync + 'static) -> pi_js::seam::Guard { super::READ_CLIPBOARD.with(|s| s.set(std::sync::Arc::new(f))) } }
     ```

     If you write a test that needs a seam in another agent's module, list it under `WAITING_ON:` as `seam <path>::testing::override_<fn>`. The module owner adds every seam that the TS tests of their module's consumers mock (grep `vi.mock("…/<file>")` in `$TS`).
   - **Process state.** `process.env` mutation / `vi.stubEnv` → `pi_js::env::testing::EnvGuard::set(k, Some(v))`; `Object.defineProperty(process, "platform")` → `PlatformGuard`; `process.chdir` → `CwdGuard`; `vi.fn()` → a closure recording into `pi_js::testing::Recorder<T>`.
   - **Fake timers.** `vi.useFakeTimers()` → `#[tokio::test(start_paused = true)]`; `vi.setSystemTime(t)` → `pi_js::time::testing::set_system_time(t)` (wall clock then follows the paused tokio clock); `vi.advanceTimersByTime(n)` → `tokio::time::advance(Duration::from_millis(n)).await`; `runAllTimers` → advance past the last scheduled timer. This only works because production code uses `tokio::time` and `pi_js::time`.
4. **No network, no real providers.** LLM interaction uses the faux provider `pi_ai::providers::faux` (`register_faux_provider`, scripted `FauxResponseStep`s). Coding-agent suite tests use `tests/support/suite/harness.rs` (port of `test/suite/harness.ts`) plus faux. Tests that would otherwise hit `fetch` install `mock_fetch` or `pi_js::fetch::testing::deny_network()`. Local servers bind `127.0.0.1:0`.
5. **Filesystem.** Use `tempfile::TempDir` and point `HOME`, `PI_CODING_AGENT_DIR` and `PI_CODING_AGENT_SESSION_DIR` at it via `EnvGuard`; never touch the real `~/.pi`. Tests that check file modes are `cfg(unix)`.
6. **Regression tests** keep the TS issue number in the file name (§2.7) and a `// #<issue>` comment.

## 13. Interop guarantees

### 13.1 Byte-compatible artifacts

These must be byte-identical to what TS v1.0.0 produces and consumes:

1. **`~/.pi/agent` layout.** File and directory names, the session dir naming (cwd encoding), session file names (timestamp plus id), file modes (`0o600` for auth), atomic-write and lock behavior (`proper-lockfile` `<file>.lock` dirs, mtime refresh, staleness) so that TS and Rust pi can run concurrently on the same files.
2. **JSON files.** `auth.json`, `settings.json` (global and project `.pi/`), `models.json`, `keybindings.json`, trust and MCP configs: `JSON.stringify(v, null, 2)` plus any trailing newline exactly as TS writes it, key order preserved. Experimental `meta.json`: tab indent.
3. **Session JSONL.** One `JSON.stringify(entry)` per line, terminated with `\n`. Every entry type, field order and migration (`migrations.ts`, the session version) is preserved. Unknown entries and fields round-trip untouched.
4. **RPC mode.** JSONL on stdin/stdout: command and response and event objects, key order, `id` echo, error shapes, extension UI request/response.
5. **pi-protocol.** Strict CBOR encoding (canonical rules of `cbor/encoder.ts`, decoder strictness and errors) and length-prefixed framing (`framing.ts`); hand-ported, no CBOR crate.
6. **Durable storage.** The JSONL layout, the SQLite schema and SQL text, and ids (`ids.ts`, uuidv7 monotonic generator).
7. **CLI.** Every flag, alias, positional rule, `--help` / `--version` text, exit code, and stdout/stderr split of `cli/args.ts`, `main.ts` and subcommands. The parser is the hand port of `args.ts`, not clap.
8. **Terminal output.** Exact ANSI/OSC sequences and rendered lines (tests compare strings), the theme JSON format, and HTML export output (the same template with injected data).
9. **Provider wire.** Request bodies and headers equal what the TS SDK path sends (§7). Responses go through the same parse paths.

### 13.2 JS semantics that must be reproduced

All through `pi_js` (Appendix A):

- **JSON numbers.** `JSON.stringify` and `String(n)`: `1.0` prints as `1`, `-0` as `0`, `1e21` as `1e+21`; NaN and ±Infinity serialize as `null`.
- **JSON escaping.** No HTML or U+2028 escaping; lowercase `\u001f`.
- **JSON.parse** accepts lone surrogate escapes by decoding them to U+FFFD. Error messages match Node 24 (`Unexpected end of JSON input`, `Unexpected token 'x', "…" is not valid JSON`, `… at position N (line L column C)`).
- **UTF-16 semantics** wherever TS uses `.length`, `slice`, `substring`, `indexOf`, `charCodeAt` or `padStart`, or compares strings with `<` or the default `sort()`. Use `pi_js::str16`. Examples: truncation limits, chars/4 token estimates, cursor columns, paste markers, `shortHash`, frame deltas, chord ops.
- **Number formatting.** `toFixed` rounds ties up (`(1.25).toFixed(1)` is `"1.3"`, `(0.125).toFixed(2)` is `"0.13"`); `toPrecision`; `Math.round` is `floor(x + 0.5)`; `parseInt` / `parseFloat` are prefix parsers; `Number(str)`; `Math.imul` and `>>> 0` are `i32`/`u32` wrapping.
- **JS whitespace.** `trim` and `\s` use the JS whitespace set (includes U+FEFF, excludes U+0085).
- **Regex translation.** `\d` → `[0-9]`; `\w` → `[A-Za-z0-9_]`; `\s` → `pi_js::regex::JS_WS_CLASS`; `.` → `[^\n\r\u{2028}\u{2029}]`. Lookaround or backreferences use `fancy_regex`; user- or data-provided patterns use `pi_js::regex::ecma` (regress). Compile once in a `static LazyLock`.
- **Intl and web APIs.** `localeCompare` uses ICU root collation; `toLocaleString()` on numbers and dates uses the en-US format; `encodeURIComponent` / `encodeURI` / `URLSearchParams` follow WHATWG rules (`form_urlencoded`); `new URL` uses the `url` crate; `Buffer` base64 is standard padded and lenient on decode, base64url has no padding; `crypto.randomUUID` is uuid v4 lowercase; `createHash("sha256")` is sha2.

### 13.3 Verification

1. **Golden tests.** The owner of each TS file that defines a persisted or wire format also owns:
   - `interop/gen/<format>.ts`, run with `node` (v24 strips types). It imports the TS implementation from `$TS` (override with env `PI_TS_REPO`) and writes golden inputs and outputs to `crates/<crate>/tests/golden/<format>/`.
   - `crates/<crate>/tests/interop__<format>.rs`, which parses each golden input, re-serializes it, and asserts byte equality. It also replays golden operations (settings updates, auth writes, session appends, RPC exchanges against the faux provider, CBOR encode/decode vectors) and compares bytes.

   Goldens are committed. Required formats: `session_jsonl`, `settings_json`, `auth_json`, `models_json`, `rpc_jsonl`, `protocol_cbor`, `durable_jsonl`, `durable_sqlite`, `export_html`, `theme_json`, `cli_help`.
2. **Live CLI diff** (`crates/pi-coding-agent/tests/interop__cli_live.rs`, W5). It runs only when `PI_TS_REPO` is set. For a fixed env and temp HOME, it compares the stdout, stderr and exit code of `node $PI_TS_REPO/packages/coding-agent/src/cli.ts <args>` and `CARGO_BIN_EXE_pi <args>` for `--help`, `--version`, `--list-models`, and a set of invalid-flag and missing-argument cases.
3. **Lint gates.** The integrator runs these after each wave; each must print nothing. Every remaining `.remove(` on a map is reviewed by hand (must be `shift_remove`).

   ```
   rg -n 'serde_json::to_(string|vec|writer)' crates --glob '!crates/pi-js/**'
   rg -n 'Hash(Map|Set)<|std::env::(set_var|remove_var)|println!|eprintln!' crates/*/src --glob '!**/bin/fixture-*'
   ```

## Appendix A. pi-js API contract (binding signatures)

W0 implements these. Everyone else calls them as written. `Result<T> = std::result::Result<T, pi_js::Error>`, and `BoxFuture<T> = futures::future::BoxFuture<'static, T>`.

```rust
// lib.rs
pub use error::{Error, JsError, NodeError, Result};
pub type BoxFuture<T> = futures::future::BoxFuture<'static, T>;
pub use callback::Unsubscribe;
pub fn ensure_crypto_provider();                  // installs rustls aws-lc-rs default provider once

pub mod error {
  #[derive(Debug, thiserror::Error)]
  pub enum Error {
    #[error("{0}")] Js(JsError),                  // Error/TypeError/RangeError/SyntaxError; Display = message
    #[error("{}", .0.message)] Abort(crate::abort::AbortReason),
    #[error("{0}")] Node(NodeError),              // Display = Node message, e.g. "ENOENT: no such file or directory, open '/x'"
    #[error("{1}")] Typed(&'static str, Box<dyn std::error::Error + Send + Sync>),
    #[error("process.exit({0})")] Exit(i32),
  }
  pub struct JsError { pub name: String, pub message: String, pub code: Option<String>, pub cause: Option<Box<Error>> }
  pub struct NodeError { pub code: String, pub errno: i32, pub syscall: String, pub path: Option<String>, pub dest: Option<String>, pub message: String }
  impl Error {
    pub fn msg(m: impl Into<String>) -> Error;    // new Error(m)
    pub fn js(name: &str, m: impl Into<String>) -> Error;
    pub fn typed<E: std::error::Error + Send + Sync + 'static>(name: &'static str, e: E) -> Error;
    pub fn name(&self) -> &str; pub fn message(&self) -> String; pub fn code(&self) -> Option<&str>;
    pub fn is_abort(&self) -> bool; pub fn to_js_string(&self) -> String;   // "Name: message"
    pub fn downcast_ref<E: std::error::Error + 'static>(&self) -> Option<&E>;
    pub fn from_io(e: std::io::Error, syscall: &str, path: Option<&str>, dest: Option<&str>) -> Error;
  }
  pub type Result<T, E = Error> = std::result::Result<T, E>;
}
pub mod callback { pub struct Unsubscribe; impl Unsubscribe { pub fn new(f: impl FnOnce() + Send + Sync + 'static) -> Self; pub fn noop() -> Self; pub fn call(self); } }
pub mod abort {
  #[derive(Clone, Debug)] pub struct AbortReason { pub name: String, pub message: String, pub value: Option<serde_json::Value> }
  pub struct AbortController;                      // new(), signal() -> AbortSignal, abort(Option<AbortReason>)
  #[derive(Clone)] pub struct AbortSignal;         // aborted(), reason() -> Option<AbortReason>, cancelled().await,
  // on_abort(f: impl FnOnce(&AbortReason) + Send + 'static) -> ListenerGuard (drop = removeEventListener),
  // throw_if_aborted() -> Result<()>, any(&[AbortSignal]) -> AbortSignal, timeout(ms: u64) -> AbortSignal,
  // never() -> AbortSignal, token() -> tokio_util::sync::CancellationToken
  impl AbortReason { pub fn abort() -> Self; pub fn timeout() -> Self; pub fn error(name: &str, message: &str) -> Self; }
}
pub mod json {
  pub fn stringify<T: serde::Serialize + ?Sized>(v: &T) -> String;                 // JSON.stringify(v)
  pub fn stringify_pretty<T: serde::Serialize + ?Sized>(v: &T, indent: &str) -> String; // JSON.stringify(v, null, indent)
  pub fn parse(s: &str) -> Result<serde_json::Value>;                            // JSON.parse (Node 24 messages, lone surrogates -> U+FFFD)
  pub fn parse_as<T: serde::de::DeserializeOwned>(s: &str) -> Result<T>;
  pub fn number_to_string(n: f64) -> String;                                     // Number#toString()
  pub mod double_option { /* serde `with` module for Option<Option<T>> */ }
}
pub mod num {
  pub fn to_fixed(x: f64, digits: u32) -> String; pub fn to_precision(x: f64, p: u32) -> String;
  pub fn to_js_string(x: f64) -> String; pub fn parse_float(s: &str) -> f64; pub fn parse_int(s: &str, radix: Option<u32>) -> f64;
  pub fn number(s: &str) -> f64; pub fn math_round(x: f64) -> f64; pub fn imul(a: i32, b: i32) -> i32;
  pub fn to_int32(x: f64) -> i32; pub fn to_uint32(x: f64) -> u32; pub const MAX_SAFE_INTEGER: i64 = 9007199254740991;
}
pub mod str16 {                                    // UTF-16 code-unit semantics; split surrogate halves become U+FFFD
  pub fn len(s: &str) -> usize; pub fn slice(s: &str, start: i64, end: Option<i64>) -> String;
  pub fn substring(s: &str, start: usize, end: Option<usize>) -> String; pub fn char_code_at(s: &str, i: usize) -> Option<u16>;
  pub fn index_of(s: &str, needle: &str, from: usize) -> Option<usize>; pub fn last_index_of(s: &str, needle: &str) -> Option<usize>;
  pub fn byte_to_utf16(s: &str, byte: usize) -> usize; pub fn utf16_to_byte(s: &str, unit: usize) -> usize;
  pub fn pad_start(s: &str, len: usize, fill: &str) -> String; pub fn pad_end(s: &str, len: usize, fill: &str) -> String;
  pub fn trim(s: &str) -> &str; pub fn trim_start(s: &str) -> &str; pub fn trim_end(s: &str) -> &str;
  pub fn is_js_whitespace(c: char) -> bool; pub fn cmp(a: &str, b: &str) -> std::cmp::Ordering;
}
pub mod text { pub struct Utf8StreamDecoder; /* new(), decode(&mut self, bytes: &[u8], stream: bool) -> String */ pub fn utf8_byte_length(s: &str) -> usize; }
pub mod time {
  pub fn now_ms() -> i64; pub fn performance_now() -> f64; pub fn iso_string(ms: i64) -> String; pub fn iso_now() -> String;
  pub fn parse_date(s: &str) -> Option<i64>;                                     // Date.parse
  pub async fn sleep(ms: u64, signal: Option<&crate::abort::AbortSignal>) -> Result<()>;
  pub fn set_timeout(ms: u64, f: impl FnOnce() + Send + 'static) -> Timeout;    // Timeout::clear(&self)
  pub fn set_interval(ms: u64, f: impl FnMut() + Send + 'static) -> Interval;   // Interval::clear(&self)
  pub mod testing { pub fn set_system_time(ms: i64); }
}
pub mod env {
  pub fn var(k: &str) -> Option<String>; pub fn vars() -> indexmap::IndexMap<String, String>;
  pub fn set_var(k: &str, v: &str); pub fn remove_var(k: &str);                  // process-global overlay
  pub fn home_dir() -> String; pub fn tmp_dir() -> String; pub fn cwd() -> String; pub fn chdir(p: &str) -> Result<()>;
  pub fn platform() -> &'static str; pub fn arch() -> &'static str; pub fn os_release() -> String; pub fn pid() -> u32;
  pub mod testing { pub struct EnvGuard; pub struct PlatformGuard; pub struct CwdGuard; } // thread-local, restore on Drop
}
pub mod path {                                     // Node `path`; platform default + posix + win32 with identical fns
  pub fn join(parts: &[&str]) -> String; pub fn resolve(parts: &[&str]) -> String; pub fn normalize(p: &str) -> String;
  pub fn relative(from: &str, to: &str) -> String; pub fn dirname(p: &str) -> String; pub fn basename(p: &str, ext: Option<&str>) -> String;
  pub fn extname(p: &str) -> String; pub fn is_absolute(p: &str) -> bool; pub fn sep() -> &'static str; pub fn delimiter() -> &'static str;
  pub mod posix {} pub mod win32 {}
}
pub mod fs {                                       // node:fs sync API with NodeError messages; `promises` = async mirror
  pub fn read_file(p: &str) -> Result<Vec<u8>>; pub fn read_to_string(p: &str) -> Result<String>;
  pub fn write_file(p: &str, data: impl AsRef<[u8]>, mode: Option<u32>) -> Result<()>; pub fn append_file(p: &str, data: impl AsRef<[u8]>) -> Result<()>;
  pub fn mkdir(p: &str, recursive: bool, mode: Option<u32>) -> Result<()>; pub fn readdir(p: &str) -> Result<Vec<String>>;
  pub fn readdir_with_file_types(p: &str) -> Result<Vec<Dirent>>; pub fn stat(p: &str) -> Result<Stats>; pub fn lstat(p: &str) -> Result<Stats>;
  pub fn exists(p: &str) -> bool; pub fn rm(p: &str, recursive: bool, force: bool) -> Result<()>; pub fn unlink(p: &str) -> Result<()>;
  pub fn rename(from: &str, to: &str) -> Result<()>; pub fn copy_file(from: &str, to: &str) -> Result<()>; pub fn realpath(p: &str) -> Result<String>;
  pub fn chmod(p: &str, mode: u32) -> Result<()>; pub fn mkdtemp(prefix: &str) -> Result<String>; pub fn symlink(target: &str, p: &str) -> Result<()>;
  pub struct Stats; /* is_file(), is_directory(), is_symbolic_link(), size, mtime_ms: f64, mode */ pub struct Dirent; /* name, is_file(), is_directory(), is_symbolic_link() */
  pub mod promises { /* async versions of all of the above, same names */ }
}
pub mod fetch {
  pub struct Headers;    // case-insensitive, ordered; get/set/append/delete/has/entries (WHATWG sorted iteration)
  pub struct Request { pub method: String, pub url: String, pub headers: Headers, pub body: Option<Body>, pub signal: Option<crate::abort::AbortSignal> }
  pub enum Body { Bytes(bytes::Bytes), Text(String) }
  pub struct Response;   // status() -> u16, status_text(), ok(), headers(), url(); async text()/json()/bytes(); body_stream(); sse()
  pub struct SseEvent { pub event: String, pub data: String, pub id: Option<String>, pub retry: Option<u64> }
  pub async fn fetch(req: Request) -> Result<Response>;
  pub fn set_default_client(c: reqwest::Client); pub fn default_client() -> reqwest::Client;
  pub mod testing { pub fn mock_fetch(h: impl Fn(Request) -> crate::BoxFuture<Result<Response>> + Send + Sync + 'static) -> crate::seam::Guard;
                    pub fn deny_network() -> crate::seam::Guard; pub fn response(status: u16, headers: &[(&str, &str)], body: impl Into<bytes::Bytes>) -> Response; }
}
pub mod intl {
  pub fn locale_compare(a: &str, b: &str) -> std::cmp::Ordering; pub fn number_to_locale_string(n: f64) -> String;
  pub fn date_to_locale_string(ms: i64) -> String; pub fn date_to_locale_time_string(ms: i64, two_digit: bool) -> String;
  pub struct Segment<'a> { pub segment: &'a str, pub index: usize /* UTF-16 */, pub byte_index: usize, pub is_word_like: Option<bool> }
  pub fn graphemes(s: &str) -> Vec<Segment<'_>>; pub fn words(s: &str) -> Vec<Segment<'_>>;
}
pub mod uri { pub fn encode_uri_component(s: &str) -> String; pub fn encode_uri(s: &str) -> String; pub fn decode_uri_component(s: &str) -> Result<String>; }
pub mod regex { pub const JS_WS_CLASS: &str; pub fn ecma(pattern: &str, flags: &str) -> Result<regress::Regex>; }
pub mod console { pub fn log(s: &str); pub fn error(s: &str); pub fn warn(s: &str); pub fn stdout_write(s: &str); pub fn stderr_write(s: &str);
                  pub mod testing { pub fn capture() -> Capture; /* stdout(), stderr() */ } }
pub mod crypto { pub fn random_uuid() -> String; pub fn random_bytes(n: usize) -> Vec<u8>; pub fn sha256(b: &[u8]) -> [u8; 32];
                 pub fn sha256_hex(b: &[u8]) -> String; pub fn math_random() -> f64; }
pub mod b64 { pub fn encode(b: &[u8]) -> String; pub fn encode_url(b: &[u8]) -> String; pub fn decode(s: &str) -> Vec<u8>; pub fn decode_url(s: &str) -> Vec<u8>; }
pub mod seam { pub struct Slot<T: ?Sized>; /* new(), get() -> Option<Arc<T>>, set(Arc<T>) -> Guard */ pub struct Guard; /* restores previous on Drop */ }
pub mod testing { pub fn assert_json_matches(actual: &serde_json::Value, expected: &serde_json::Value); pub struct Recorder<T>; /* push, calls() -> Vec<T>, len */ }
pub mod vendor { pub mod typebox; /* §6: t::*, Schema, compile, Validator, ValueError, value::{check, convert, errors, clean, default} */
                 pub mod jsdiff; /* diff_lines, diff_words, structured_patch, create_two_files_patch, create_patch, apply_patch, parse_patch: names = snake(jsdiff export) */ }
```

If an Appendix A signature makes a TS port impossible, the W0 owner may add new items, but must not change existing ones. Other agents report the need under `WAITING_ON: pi_js::<item>`.
