# pi v1 port: state and how to restart

This file describes the state of the port of pi v1 (TypeScript) to Rust (`keejkrej/pi-rs`) and Go (`keejkrej/pi-go`), and how to restart the work. The same file is in both repos, on branch `port/v1`.

## Goal and user decisions

- Port the whole pi monorepo **completely**: all 13 packages (ai, agent, tui, coding-agent, mcp, codemode, telemetry, chord, durable, protocol, client, server, evals), including the experimental modes, docs and examples.
- Restructure in place. The old code is in `legacy/`. Delete `legacy/` only after asking the user.
- On-disk, wire and CLI formats must be byte-compatible with TS pi:
  - files: sessions JSONL, settings/auth/models JSON, the `~/.pi/agent` layout, lockfiles;
  - wire: RPC JSONL, CBOR protocol;
  - CLI text.
- Extensions:
  - The extension API is a native API (Rust trait / Go interface).
  - The built-in extensions are ported natively: llama.cpp, codemode, tool-search, mcp.
  - codemode runs the quickjs-wasi wasm via wasmtime (Rust) / wazero (Go).
  - User TypeScript extensions are not loaded.
- Port unit and integration tests that use faux providers. Skip e2e tests that hit real providers. No network, no paid tokens.
- Commit only when the user asks.

## Sources

- **TS source of truth:** `keejkrej/pi` at commit `7fbbd5f4a1d982bb02d63472dde0774fa639f99b` (v1.0.0 + 2 commits).
  - Run `npm ci --ignore-scripts`.
  - Run `npm run hydrate:model-data` to generate `packages/ai/src/providers/data`.
- **Binding plan:** `PORTING.md` in each repo: layout, file mapping, naming, JS semantics, dependencies with exact versions, extension API, agent rules, tests, interop.
- **Work units:** `port-map.json` maps every TS file and test to its target file and its unit (`unit` field).
  - Rust: 127 units. Go: 126 units.
  - A unit is one agent's job and owns its target files exclusively.
- **Foundation units:** these modules are Rust-/Go-only (no TS source), so they are not in `port-map.json`:
  - Rust `crates/pi-js` (contract: PORTING.md Appendix A, §6, §13.2):
    - `pijs-core`: lib, error, callback, abort, seam, testing, console
    - `pijs-data`: json, num, str16, text, b64, uri, crypto
    - `pijs-system`: time, env, path, fs
    - `pijs-net-intl`: fetch, regex, intl
    - `pijs-typebox`: `src/vendor/typebox*`
    - `pijs-jsdiff`: `src/vendor/jsdiff*`
  - Go `internal/*` (contract: PORTING.md §5.1, §6, §7.1, §9, §10, §11.1):
    - `js` + `omap`
    - `jsonx`
    - `jsre` + `sse` + `xspawn`
    - `typebox`
    - `jsdiff` + `partialjson`
    - `ignore` + `minimatch` + `semver`
    - `hostedgit` + `lockfile` + `ansi`
    - `marked`
    - `eastasian` + `highlight` + `mermaid`
    - `quickjs`
  - Hand-ported npm libraries are ported from the pinned sources in the TS repo's `node_modules`.

## Package order

| Package | Depends on |
|---|---|
| foundation | nothing |
| telemetry, chord, tui, mcp, codemode | foundation |
| ai | telemetry |
| protocol | chord |
| agent | ai |
| client, server | chord, protocol |
| durable | chord, ai |
| coding-agent | everything above |
| evals | coding-agent |
| coding-agent docs/examples | coding-agent (do them last) |

## State (2026-10-02)

Done:

- Design: `PORTING.md` and `port-map.json`.
- Layout: every crate/package and target file exists as a header-only stub.
- Part of the foundation (below).

Not started:

- API skeletons
- implementation
- integration
- audit

### Rust foundation

| Unit | State |
|---|---|
| pijs-data, pijs-system, pijs-net-intl | ported, not yet independently reviewed |
| pijs-jsdiff | ported and reviewed |
| pijs-core | **missing** |
| pijs-typebox | **missing** |

`cargo check -p pi-js` fails with 26 errors. All of them are missing pijs-core items: `error::{Error, Result, JsError, NodeError}`, `abort::*`, `seam::{Guard, Slot}`, `BoxFuture`.

### Go foundation

| Unit | State |
|---|---|
| jsonx, quickjs | ported and reviewed |
| js/omap, hostedgit/lockfile/ansi, jsdiff/partialjson | ported, review pending |
| jsre/sse/xspawn | interrupted mid-port |
| typebox, ignore/minimatch/semver, marked, eastasian/highlight/mermaid | not started |

`go build ./internal/...` and `go test ./internal/...` pass for the 9 existing packages.

## Method used (restart from here)

1. **Foundation.** Finish the missing units, then do an adversarial review of every unit that is not yet reviewed:
   - Compare against the contract and the JS/npm source.
   - Generate test vectors by running node against the real JS.
   - Finish with the whole foundation building warning-free and all its tests green.
2. **API skeleton.** Do this per package, in package order, with one agent per unit working in the main tree.
   - Every owned source file gets all its imports, types (all fields, exact serde/json shapes and field order), constants with real values, and every function/method signature.
   - Bodies are `todo!("port: <name>")` / `panic("unported: <name>")`. Trivial bodies are written in full.
   - Then a per-package integration step makes the package compile. For ai and coding-agent this is triage plus parallel fixers that own disjoint files.
   - This gives every later agent a stable, compiling API to code against.
3. **Implementation.** One agent per unit:
   - Each agent works in a private copy of the repo and copies back only the files its unit owns.
   - It implements every body faithfully and ports the unit's TS tests test by test.
   - An adversarial parity review follows: compare against the TS line by line (branches, messages, formats, UTF-16, abort semantics) and port any missing tests.
   - Then a per-package integration loop runs until the package builds clean, has no placeholders, and passes all its tests.
   - Integrate upstream packages before downstream ones.
4. **Repo-wide integration.** The full build and every test pass, `cargo clippy --all-targets -D warnings` / `go vet` pass, and no placeholders remain.
5. **Audit:**
   - completeness critic: every TS export, CLI flag and test has a counterpart;
   - interop goldens per PORTING.md (Rust §13, Go §16);
   - diff live CLI output against TS pi;
   - interactive tmux test;
   - port the docs and examples.

## Lessons from the first run

- Limit concurrent builds by RAM. rustc on the large crates needs 2-4 GB per invocation.
- Give each agent a private workspace. A sibling's broken file must not block another agent's build or tests.
- Network blips kill in-flight agents. Record what finished, so a rerun skips it.
- Agents sometimes create local shims for items another unit has not written yet. Never merge shims.
