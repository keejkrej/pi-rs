export const meta = {
  name: 'pi-port-w2-implement',
  description: 'Implement units in private clones with adversarial parity review, then integrate packages until all tests pass',
  phases: [
    { title: 'Implement', detail: 'per-unit full implementation + test port in a private clone' },
    { title: 'Parity', detail: 'per-unit adversarial TS-parity review and fixes' },
    { title: 'Package-Int', detail: 'per-package integration: clean build, all ported tests green' },
  ],
}

const DATA = __DATA__
const { lang, repo, pkgDeps, tasks, units, intPkgs, label } = DATA
const TS = DATA.ts
const BIN = DATA.bin
const RUST = lang === 'rust'
const NAME = RUST ? 'Rust' : 'Go'
const TOOL = RUST ? `${BIN}/cargo` : `${BIN}/go`
const CRATE = { foundation: 'pi-js', telemetry: 'pi-telemetry', chord: 'pi-chord', tui: 'pi-tui', mcp: 'pi-mcp', codemode: 'pi-codemode', ai: 'pi-ai', protocol: 'pi-protocol', agent: 'pi-agent-core', client: 'pi-client', server: 'pi-server', durable: 'pi-durable', 'coding-agent': 'pi-coding-agent', 'coding-agent-post': 'pi-coding-agent', evals: 'pi-evals' }
const GODIRS = { foundation: './internal/...', telemetry: './telemetry/...', chord: './chord/...', tui: './tui/...', mcp: './mcp/...', codemode: './codemode/...', ai: './ai/... ./cmd/pi-ai/ ./cmd/generate-models/ ./cmd/check-model-data/ ./cmd/generate-test-image/', protocol: './protocol/...', agent: './agent/...', client: './client/...', server: './server/...', durable: './durable/...', 'coding-agent': './codingagent/... ./cmd/pi/', 'coding-agent-post': './codingagent/... ./cmd/pi/', evals: './evals/... ./cmd/pi-evals/ ./cmd/pi-evals-entrypoint/' }
const CHECK = pkg => RUST
  ? `${TOOL} check -p ${CRATE[pkg]} --all-targets --message-format=short`
  : `${TOOL} build -gcflags=-e ${GODIRS[pkg]} && ${TOOL} vet ${GODIRS[pkg]}`
const TEST = pkg => RUST ? `${TOOL} test -p ${CRATE[pkg]} --no-fail-fast` : `${TOOL} test ${GODIRS[pkg]}`
const SPEC = id => `${DATA.unitsDir}/${id}.md`
const PH = RUST ? 'todo!("port: ...")' : 'panic("unported: ...")'

const PREAMBLE = `You are one of many agents porting pi v1.0.0 (a TypeScript monorepo) to ${NAME}.
- TS source of truth (read-only): ${TS} (node_modules installed; model data hydrated under packages/ai/src/providers/data).
- Target main tree: ${repo}. Binding conventions: ${repo}/PORTING.md — READ IT IN FULL FIRST; it overrides your defaults. Ownership map: ${repo}/port-map.json${RUST ? '' : ' and porting/filemap.tsv'}.
- User decisions: complete 1:1 port of all 13 packages; on-disk/wire/CLI formats byte-compatible with TS pi; native extension API (user TS extensions are not loaded); codemode runs the quickjs-wasi wasm via ${RUST ? 'wasmtime' : 'wazero'}; unit tests are ported (no network, no real providers, no paid tokens).
- Build tools: ALWAYS invoke ${TOOL} (a semaphore wrapper limiting concurrent builds; the machine has 18 GB RAM) instead of plain ${RUST ? 'cargo' : 'go'}, and give those Bash calls timeout 600000 (waiting for a build slot is normal). Prefer targeted commands (${RUST ? 'cargo check; cargo test -p <crate> --test <name> / --lib <filter>' : 'go build/vet of your package; go test -run <pattern> ./<pkg>/'}) while iterating.${RUST ? '' : ' gofmt and ~/go/bin/gopls may be used directly.'}
- Never run git write commands (add/commit/stash/reset/checkout/restore/clean/rebase). Never edit files you do not own. Never run ${RUST ? 'cargo clean/update/fix or workspace-wide cargo fmt (rustfmt on your own files is fine)' : 'go mod tidy/go get/go mod edit'}. Never edit ${RUST ? 'Cargo.toml files' : 'go.mod/go.sum'}; if you need a dependency, report it in needs_deps.
- Use /tmp for scratch files and delete them when done.
- Your final answer is the structured report; be factual (never claim something compiles or passes unless you ran it).`

const byId = {}; for (const t of tasks) byId[t.id] = t
const ownedList = t => t.owned.map(o => '  - ' + o).join('\n')
const CLONE = (name, unit) => `WORKSPACE PROTOCOL (mandatory): you work in a private copy-on-write clone of the main tree, so concurrent agents cannot break your builds.
  1. WT=$(${BIN}/wt-begin ${lang} ${name})   # prints the clone path; includes the build cache
  2. Do ALL editing, building and testing inside that clone (absolute paths under it; run ${TOOL} with the clone as cwd).
  3. ${BIN}/wt-refresh ${lang} "$WT" pulls the newest main-tree versions of files you have not modified (finished sibling/upstream units). Run it before your first test run and again whenever a test fails only because someone else's code is still a placeholder.
  4. When finished: ${BIN}/wt-sync ${lang} ${unit} "$WT" --dry (preview), then without --dry to copy your owned changed files back to ${repo}. On CONFLICT, merge the main-tree version into your clone copy and sync again. REJECTED files are not yours — revert those edits in the clone (and describe the need in your report).
  5. ${BIN}/wt-end "$WT"
  Put the final wt-sync output in sync_output.`

const UNIT_SCHEMA = { type: 'object', properties: {
  status: { type: 'string', enum: ['complete', 'partial'] },
  files: { type: 'array', items: { type: 'string' } },
  tests: { type: 'string', description: 'tests ported / run / passing / failing counts' },
  placeholders_left: { type: 'array', items: { type: 'string' } },
  api_changes: { type: 'array', items: { type: 'string' }, description: 'public signatures you changed vs the skeleton (callers elsewhere must adapt)' },
  waiting_on: { type: 'array', items: { type: 'string' }, description: 'failing tests blocked only by other units placeholders/bugs: test -> item' },
  needs_deps: { type: 'array', items: { type: 'string' } },
  gaps_remaining: { type: 'array', items: { type: 'string' } },
  sync_output: { type: 'string' }, summary: { type: 'string' } },
  required: ['status', 'files', 'tests', 'placeholders_left', 'api_changes', 'waiting_on', 'needs_deps', 'gaps_remaining', 'sync_output', 'summary'] }

const implPrompt = t => `${PREAMBLE}

PHASE B — IMPLEMENTATION of unit ${t.id} (TS package "${t.package}"): ${t.title}
Unit spec (read it first — TS files, TS tests to port, tests to skip, owned targets, reader notes): ${SPEC(t.id)}
Owned files (exclusively yours):
${ownedList(t)}
State of the tree: the foundation is fully implemented; every package has a compiling API skeleton (all types, constants and signatures, bodies ${PH}). Other units — siblings in this package and units of other packages — are being implemented concurrently in their own clones and synced back when done.
${CLONE('b-' + t.id, t.id)}
Do:
1. Read the TS files of the unit in full, and the skeleton of your owned files.
2. Replace EVERY placeholder in your owned files with a complete, faithful implementation: all branches and edge cases, error/log/user-facing messages verbatim, same ordering and defaults, same file/wire formats, comments where the TS has non-obvious ones. No simplifications, no "for now" shortcuts, no stubs. Keep the skeleton's public signatures; if one is genuinely wrong, fix it in your files and list it in api_changes.
3. Port every owned test file from its TS counterpart test-by-test (same names, same assertions, same fixtures), except the tests the spec lists as skipped. Use the faux provider and test helpers per PORTING.md; never touch the network.
4. Build and test (filter output to your files): ${CHECK(t.package)} ; then your own tests. Tests failing only because another unit still has a placeholder are acceptable — list them in waiting_on (test -> missing item). Everything else must pass, with no compiler warnings in your files.
5. grep your owned files for placeholders (${RUST ? 'todo!, unimplemented!' : 'panic("unported'}, TODO, FIXME): there must be none left; list any in placeholders_left.
6. Sync back and end the workspace.`

const parityPrompt = (t, impl) => `${PREAMBLE}

ADVERSARIAL PARITY REVIEW of unit ${t.id} (TS package "${t.package}"): ${t.title}
Unit spec: ${SPEC(t.id)}
Owned files (you may edit them):
${ownedList(t)}
Another agent just implemented this unit and reported: ${JSON.stringify(impl)}
Assume it has gaps until proven otherwise; your job is to find and fix every divergence from the TS behaviour.
${CLONE('p-' + t.id, t.id)}
1. For every function, method, class, constant and type in the unit's TS files, locate the ${NAME} counterpart and compare line by line: branches, defaults, early returns, error and log message texts, ordering, serialization (field names/order, absent vs null, number formatting), Unicode handling (UTF-16 indices), async/abort/timeout semantics, file paths and formats, env var names, CLI/user-facing text. Write the comparison as a checklist in /tmp (not in the repo) and work through it.
2. Test parity: every TS test case (describe/it/test) in the unit's TS test files must have a ${NAME} counterpart asserting the same things, unless the spec lists it as skipped. Port missing ones.
3. grep for placeholders and smells: ${RUST ? 'todo!, unimplemented!, unreachable! used as a stub' : 'panic("unported'}, TODO, FIXME, "for now", "simplified", "not supported", "stub", empty bodies, ignored errors.
4. Fix every gap, then build and run the unit's tests (refresh first). Same acceptance as the implementer: only tests blocked by other units' placeholders may fail (waiting_on).
5. Sync back and end the workspace. In summary, list the gaps you found and fixed.`

const INT_SCHEMA = { type: 'object', properties: {
  green: { type: 'boolean', description: 'true only if build is clean and every test of the package passes' },
  tests: { type: 'string' }, summary: { type: 'string' }, remaining: { type: 'array', items: { type: 'string' } } },
  required: ['green', 'tests', 'summary', 'remaining'] }
const TRIAGE_SCHEMA = { type: 'object', properties: {
  green: { type: 'boolean' }, failing: { type: 'string', description: 'counts of compile errors / failing tests' },
  groups: { type: 'array', items: { type: 'object', properties: {
    name: { type: 'string' }, paths: { type: 'array', items: { type: 'string' } }, summary: { type: 'string' } }, required: ['name', 'paths', 'summary'] } } },
  required: ['green', 'failing', 'groups'] }
const FIX_SCHEMA = { type: 'object', properties: { summary: { type: 'string' }, cross_group_needs: { type: 'array', items: { type: 'string' } } }, required: ['summary', 'cross_group_needs'] }

const intBase = (pkg, reports) => `${PREAMBLE}

PACKAGE INTEGRATION for TS package "${pkg}" (${RUST ? 'crate ' + CRATE[pkg] : GODIRS[pkg]}) in the main tree ${repo} (work there directly; no clone). All units of this package were implemented and parity-reviewed in private clones and synced back. Goal: zero errors, zero warnings, no placeholders left, and EVERY ported test of the package passing:
  ${CHECK(pkg)}
  ${TEST(pkg)}
  grep for leftover placeholders (${RUST ? 'todo!, unimplemented!' : 'panic("unported'}) in the package and implement them faithfully from the TS source.
Rules: the TS source is the truth. Fix root causes; never delete, skip, #[ignore]/t.Skip or weaken a ported test to get green (a test may be skipped only if it is an e2e/real-provider test per the unit spec). Adapt callers to api_changes reported by units. If a failure is caused by a bug in an upstream package, fix it there faithfully and re-run that package's tests too. You may add a missing external dependency to ${RUST ? 'the crate Cargo.toml using the version pinned in PORTING.md / [workspace.dependencies]' : 'go.mod using the version pinned in PORTING.md'} (say so in the summary).
Unit spec files: ${DATA.unitsDir}/<unit-id>.md
Unit reports for this package: ${JSON.stringify(reports)}`

// Priority scheduler: keep at most CAP agents running; lower prio runs first.
const CAP = 10
const queue = []; let running = 0
function pump() {
  while (running < CAP && queue.length) {
    queue.sort((a, b) => a.prio - b.prio)
    const j = queue.shift(); running++
    Promise.resolve().then(j.fn).then(j.res, j.rej).finally(() => { running--; pump() })
  }
}
const A = (prio, prompt, opts) => new Promise((res, rej) => { queue.push({ prio, fn: () => agent(prompt, opts), res, rej }); pump() }).catch(e => { log(`agent ${opts.label} failed: ${e}`); return null })
const P_INT = -100000

async function intPackage(pkg, reports) {
  const n = tasks.filter(t => t.package === pkg).length
  if (n <= 12) {
    let last = null
    for (let round = 1; round <= 4; round++) {
      last = await A(P_INT, `${intBase(pkg, reports)}\n${last ? 'The previous integration round ended with: ' + JSON.stringify(last) : ''}\nReturn {green, tests, summary, remaining}.`,
        { label: `int:${pkg}:r${round}`, phase: 'Package-Int', schema: INT_SCHEMA })
      if (last && last.green) return last
    }
    return last
  }
  let needs = []
  for (let round = 1; round <= 6; round++) {
    const tri = await A(P_INT, `${intBase(pkg, reports)}
You are the TRIAGE step of round ${round}. Run the build and the full test suite, then partition the failures (compile errors, failing tests, leftover placeholders) into 3-10 groups by the source files that must change, so parallel fixers can each own one group (groups must not share files). Do not fix anything yourself unless the total is small (under ~15 failures; then fix them all and report green only after a clean re-run).
${needs.length ? 'Cross-group needs reported by the previous round fixers (do these first; they are yours):\n' + needs.join('\n') : ''}`,
      { label: `int:${pkg}:triage${round}`, phase: 'Package-Int', schema: TRIAGE_SCHEMA })
    if (!tri) continue
    if (tri.green) return { green: true, tests: tri.failing, summary: `green after triage round ${round}`, remaining: [] }
    log(`${label} ${pkg} int round ${round}: ${tri.failing}; ${tri.groups.length} groups`)
    const fixes = await Promise.all(tri.groups.map(g => A(P_INT, `${intBase(pkg, reports)}
You are a FIXER in round ${round}, owning ONLY these paths: ${g.paths.join(', ')}. Group "${g.name}": ${g.summary}
Fix every failure attributed to your paths (re-run the relevant tests). If a fix requires changing a file outside your paths, do NOT edit it; describe the exact need (file, item, change) in cross_group_needs.`,
      { label: `int:${pkg}:fix${round}:${g.name}`, phase: 'Package-Int', schema: FIX_SCHEMA })))
    needs = fixes.filter(Boolean).flatMap(f => f.cross_group_needs)
  }
  return await A(P_INT, `${intBase(pkg, reports)}\nFINAL integration pass after 6 parallel rounds; outstanding needs:\n${needs.join('\n')}\nGet it fully green. Return {green, tests, summary, remaining}.`,
    { label: `int:${pkg}:final`, phase: 'Package-Int', schema: INT_SCHEMA })
}

const results = { units: {}, ints: {} }
const unitDone = {}
const prior = DATA.priorReports || {}
const intDone = {}
units.forEach((id, i) => {
  const t = byId[id]
  unitDone[id] = (async () => {
    // Optionally wait until the upstream packages integrated in this run are green.
    if (DATA.gateUnitsOnInt) await Promise.all((pkgDeps[t.package] || []).filter(p => intPkgs.includes(p)).map(integrate))
    const impl = await A(i, implPrompt(t), { label: `impl:${t.id}`, phase: 'Implement', schema: UNIT_SCHEMA })
    const par = await A(i - 50000, parityPrompt(t, impl || 'implementer agent failed — implement everything yourself'), { label: `parity:${t.id}`, phase: 'Parity', schema: UNIT_SCHEMA })
    results.units[t.id] = { impl, parity: par }
    log(`${label} done: ${t.id} impl=${impl ? impl.status : 'failed'} parity=${par ? par.status : 'failed'}`)
  })()
})
// Each package integration waits for its own units in this run and for upstream integrations in this run.
function integrate(pkg) {
  if (!intDone[pkg]) intDone[pkg] = (async () => {
    await Promise.all(units.filter(id => byId[id].package === pkg).map(id => unitDone[id]))
    await Promise.all((pkgDeps[pkg] || []).filter(p => intPkgs.includes(p)).map(integrate))
    const reports = tasks.filter(t => t.package === pkg).map(t => {
      const r = results.units[t.id] || prior[t.id]; const p = r && (r.parity || r.impl)
      return p ? { unit: t.id, status: p.status, tests: p.tests, placeholders_left: p.placeholders_left, api_changes: [...((r.impl && r.impl.api_changes) || []), ...((r.parity && r.parity.api_changes) || [])], waiting_on: p.waiting_on, needs_deps: [...((r.impl && r.impl.needs_deps) || []), ...((r.parity && r.parity.needs_deps) || [])], gaps_remaining: p.gaps_remaining } : { unit: t.id, status: r ? 'agent-failed' : 'implemented-in-another-run' }
    })
    const int = await intPackage(pkg, reports)
    results.ints[pkg] = int
    log(`${label} integrated ${pkg}: green=${int && int.green}`)
    return int
  })()
  return intDone[pkg]
}
await Promise.all([...intPkgs.map(integrate), ...Object.values(unitDone)])
return results
