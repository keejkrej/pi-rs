# pi v1 port: handoff

This file describes how to continue the port of pi v1 (TypeScript) to Rust (`pi-rs`) and Go (`pi-go`) on another machine. The same file and the same orchestration bundle are in both repositories under `porting/`.

## Goal and user decisions

- Port the whole pi monorepo **completely**: all 13 packages (ai, agent, tui, coding-agent, mcp, codemode, telemetry, chord, durable, protocol, client, server, evals), including the experimental modes, docs and examples.
- Restructure in place. The old code was moved to `legacy/`. Delete `legacy/` only after asking the user.
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
- Never commit unless the user asks. Saving this state on branch `port/v1` was explicitly requested.

## Sources

| What | Where |
|---|---|
| TS source of truth | `github.com/keejkrej/pi` at commit `7fbbd5f4a1d982bb02d63472dde0774fa639f99b` (v1.0.0 + 2 commits) |
| Rust target | `github.com/keejkrej/pi-rs`, branch `port/v1` |
| Go target | `github.com/keejkrej/pi-go`, branch `port/v1` |
| Binding conventions | `PORTING.md` in each target repo (read in full; it overrides defaults) |
| File ownership | `port-map.json` (Rust); `port-map.json` + `porting/filemap.tsv` (Go) |

Set up the TS tree:

```bash
git clone https://github.com/keejkrej/pi.git && cd pi && git checkout 7fbbd5f4a1d982bb02d63472dde0774fa639f99b
npm ci --ignore-scripts
# model data snapshot used so far (do NOT re-hydrate: upstream data drifts)
mkdir -p packages/ai/src/providers/data && cp -r ../pi-rs/porting/orchestration/ts-model-data/. packages/ai/src/providers/data/
```

## Status at handoff (2026-10-02)

Done:

- Understanding and design phase:
  - `PORTING.md` and `port-map.json` for both languages.
  - 132 Rust and 136 Go work units, described in `pi-port-tasks-<lang>.json`, with per-unit specs.
  - Workspace layout: every crate/package exists with header-only files.
- Foundation (W1 stage 1), partially done. Details below.

Not started:

- API skeletons (W1 stages 2-3).
- Implementation (W2).
- Repo-wide integration and audit (W3/W4).

### Rust foundation (`crates/pi-js`)

| Unit | State | Next step |
|---|---|---|
| pijs-core (lib, error, callback, abort, seam, testing, console) | **missing**: the agent died in a network outage | port + review |
| pijs-data | ported (by its reviewer after the port agent died) | independent review |
| pijs-system (time, env, path, fs) | ported | review |
| pijs-net-intl (fetch, regex, intl) | ported | review |
| pijs-jsdiff | ported + reviewed | none |
| pijs-typebox | **missing** | port + review |

`cargo check -p pi-js` fails with 26 errors. All of them are missing pijs-core items (`error::{Error, Result, JsError, NodeError}`, `abort::*`, `seam::{Guard, Slot}`, `BoxFuture`). Those items are specified in PORTING.md Appendix A.

### Go foundation (`internal/*`)

| Unit | State | Next step |
|---|---|---|
| go-js-omap | ported (by its reviewer) | independent review |
| go-jsonx | ported + reviewed | none |
| go-quickjs | ported + reviewed | none |
| go-hostedgit-lockfile-ansi | ported; review interrupted (partial work saved) | review |
| go-jsdiff-partialjson | ported; review interrupted (partial work saved) | review |
| go-jsre-sse-xspawn | port interrupted (22 files saved in `partial/`) | port + review |
| go-typebox, go-ignore-minimatch-semver, go-marked, go-eastasian-highlight-mermaid | not started | port + review |

`go build ./internal/...` and `go test ./internal/...` are green for the 9 packages that exist.

`state/<lang>.json` records this table. `gen-w1.mjs` reads it, so a regenerated W1 run skips finished work. It also points interrupted units at their partial files.

## Orchestration bundle (`porting/orchestration/`)

| File | Purpose |
|---|---|
| `config.mjs` | Paths, overridable by env: `PI_PORT_HOME` (state dir, defaults to the bundle dir itself), `PI_PORT_TS`, `PI_PORT_RUST`, `PI_PORT_GO`, `PI_PORT_BIN` |
| `bin/cargo`, `bin/go` | Semaphore wrappers that limit concurrent builds machine-wide. Agents must call them by absolute path. Tune with `PI_PORT_CARGO_SLOTS` (default 2), `CARGO_BUILD_JOBS` (5), `PI_PORT_GO_SLOTS` (3), `GOFLAGS` (-p=4); `PI_PORT_REAL_CARGO` / `PI_PORT_REAL_GO` select the real tool. Do not put `bin/` on PATH. |
| `bin/wt-begin`, `bin/wt-sync`, `bin/wt-refresh`, `bin/wt-end` | Private per-agent workspaces (`wt.mjs`). `wt-begin` clones the main tree. `wt-sync` copies back only files the unit owns, and reports REJECTED and CONFLICT files. `wt-refresh` pulls finished sibling work into the clone. `wt-end` deletes the clone. |
| `pi-port-tasks-<lang>.json` | Work units: id, package, TS files, tests, skipped tests, owned target files, reader notes |
| `gen-units.mjs` | Writes `units/<lang>/<id>.md` spec files; agents read these |
| `w1-template.js`, `gen-w1.mjs` | W1: foundation port and review, then a per-package API skeleton cascade in dependency order, then per-package compile integration |
| `w2-template.js`, `gen-w2.mjs` | W2: per-unit implementation and test port in a private clone, then an adversarial TS-parity review, then per-package integration until all tests pass. Uses a priority scheduler (10 agents per workflow). |
| `pkg-deps.mjs` | Package dependency graph |
| `sim-w2.mjs` | Scheduler simulation with mocked agents |
| `state/<lang>.json` | Resume state: finished foundation units and partial-work directories |
| `partial/` | Unsynced files from agents that were interrupted |
| `results/<lang>-w1-journal.jsonl` | Reports of the W1 agents that finished |
| `prep-tasks.mjs`, `pi-port-understand.json` | How the task files were built from the design phase (reference only) |
| `ts-model-data/` | Snapshot of `packages/ai/src/providers/data` (the hydrated model catalog) |

On macOS the clones are APFS copy-on-write and include `target/`. Elsewhere `target/` is skipped unless `PI_PORT_CLONE_TARGET=1`. On Linux, `cp --reflink=auto` is tried first. Without copy-on-write, use sccache (`RUSTC_WRAPPER=sccache`) so each clone does not rebuild all dependencies from scratch.

## Continuing on the Windows machine

Recommended: run everything inside **WSL2 (Ubuntu)**.

- Keep the repos on the WSL filesystem, not `/mnt/c`, which is slow.
- Install:
  - Rust 1.98+ (edition 2024)
  - Go 1.26+
  - Node >= 22.19 (v24 was used)
  - git, gh, sccache, tmux
  - Claude Code
- Native Windows with Git Bash may work, but Node and bash disagree about `/tmp` paths there. If you do use it, set every `PI_PORT_*` variable to forward-slash Windows paths (for example `C:/pi-port`).

Setup:

```bash
export PI_PORT_HOME=~/pi-port PI_PORT_TS=~/src/pi PI_PORT_RUST=~/src/pi-rs PI_PORT_GO=~/src/pi-go
git clone -b port/v1 https://github.com/keejkrej/pi-rs.git ~/src/pi-rs
git clone -b port/v1 https://github.com/keejkrej/pi-go.git ~/src/pi-go
mkdir -p $PI_PORT_HOME
cp -r ~/src/pi-rs/porting/orchestration/. $PI_PORT_HOME/
cp -r ~/src/pi-go/porting/orchestration/. $PI_PORT_HOME/   # shared files are identical; adds Go state
# size build concurrency to the machine: roughly one cargo slot per 6 GB of RAM
export PI_PORT_CARGO_SLOTS=4 CARGO_BUILD_JOBS=8 PI_PORT_GO_SLOTS=6
node $PI_PORT_HOME/gen-units.mjs
node $PI_PORT_HOME/gen-w1.mjs
```

The generated workflow scripts embed absolute paths. Re-run the generators whenever an env var changes. Agents inherit the env of the Claude Code session, so export these variables before starting `claude`.

Run order in Claude Code (ultracode on, or say "use a workflow"):

1. **W1**: `Workflow({scriptPath: "$PI_PORT_HOME/w1-rust.js"})` and `w1-go.js`, concurrently. When it finishes, check `results.foundation.int.green` and every `results.skeleton[pkg].int.green`. Fix anything red before W2.
2. **W2**, per language:
   1. Run `node gen-w2.mjs <lang> a` and `... b`, and launch both concurrently.
   2. When b finishes, generate and launch c (lower half of coding-agent).
   3. When a finishes, generate and launch d (upper half of coding-agent).
   4. When both c and d have finished, merge their `results.units` into one JSON file, then run `node gen-w2.mjs <lang> e merged.json` and launch e. Part e integrates coding-agent, then implements and integrates evals and the docs/examples units.
3. **W3**: repo-wide integration (not scripted yet).
   - The full build is clean.
   - Every ported test passes.
   - `cargo clippy --all-targets -D warnings` / `go vet` pass.
   - No placeholders are left: grep for `todo!`, `unimplemented!`, `panic("unported`.
   - The `NEEDS_DEPS` / `porting/needs` items are resolved.
4. **W4**: audit (not scripted yet).
   - Completeness critic: every TS export, CLI flag and test has a counterpart.
   - Interop goldens per PORTING.md (Rust §13, Go §16): generate the golden files with the TS scripts under `interop/gen/` and check them against the port.
   - Diff live CLI output against TS pi.
   - Interactive tmux test.
   - Port the docs and examples.

## Lessons from the first run

- A short network outage (`ENOTFOUND`) killed 5 agents mid-task. Their clones keep unsynced work. To save it:
  1. Run `wt-sync <lang> <unit> <clone> --dry` to list the changed files.
  2. Copy them under `partial/`.
  3. Record them in `state/<lang>.json`.
  4. Regenerate the scripts.
- Workflow resume caches are local to the machine and session. On a new machine, use `state/<lang>.json` instead.
- Agents that need items another unit has not written yet sometimes create local shims in their clone. `wt-sync` rejects those (not owned), which is expected.
- Builds are the bottleneck. rustc on the large crates needs 2-4 GB per invocation, so size the build slots to RAM, not to CPU count.
