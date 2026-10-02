export const meta = {
  name: 'pi-port-w1-foundation-skeleton',
  description: 'Port the foundation fully, then cascade compiling API skeletons through every package in dependency order',
  phases: [
    { title: 'Foundation', detail: 'full port + parity review of the foundation (private clones)' },
    { title: 'Skeleton', detail: 'per-unit API skeletons (types + signatures, placeholder bodies)' },
    { title: 'Skeleton-Int', detail: 'per-package compile integration of skeletons' },
  ],
}

const DATA = __DATA__
const { lang, repo, pkgDeps, tasks } = DATA
const TS = DATA.ts
const BIN = DATA.bin
const RUST = lang === 'rust'
const NAME = RUST ? 'Rust' : 'Go'
const TOOL = RUST ? `${BIN}/cargo` : `${BIN}/go`
const CRATE = { foundation: 'pi-js', telemetry: 'pi-telemetry', chord: 'pi-chord', tui: 'pi-tui', mcp: 'pi-mcp', codemode: 'pi-codemode', ai: 'pi-ai', protocol: 'pi-protocol', agent: 'pi-agent-core', client: 'pi-client', server: 'pi-server', durable: 'pi-durable', 'coding-agent': 'pi-coding-agent', evals: 'pi-evals' }
const GODIRS = { foundation: './internal/...', telemetry: './telemetry/...', chord: './chord/...', tui: './tui/...', mcp: './mcp/...', codemode: './codemode/...', ai: './ai/... ./cmd/pi-ai/ ./cmd/generate-models/ ./cmd/check-model-data/ ./cmd/generate-test-image/', protocol: './protocol/...', agent: './agent/...', client: './client/...', server: './server/...', durable: './durable/...', 'coding-agent': './codingagent/... ./cmd/pi/', evals: './evals/... ./cmd/pi-evals/ ./cmd/pi-evals-entrypoint/' }
const CHECK = pkg => RUST
  ? `${TOOL} check -p ${CRATE[pkg]} --all-targets --message-format=short`
  : `${TOOL} build -gcflags=-e ${GODIRS[pkg]} && ${TOOL} vet ${GODIRS[pkg]}`
const PLACEHOLDER = RUST ? 'todo!("port: <TS name>")' : 'panic("unported: <TS name>")'
const SPEC = id => `${DATA.unitsDir}/${id}.md`

const PREAMBLE = `You are one of many agents porting pi v1.0.0 (a TypeScript monorepo) to ${NAME}.
- TS source of truth (read-only): ${TS} (node_modules installed; model data hydrated under packages/ai/src/providers/data).
- Target main tree: ${repo}. Binding conventions: ${repo}/PORTING.md — READ IT IN FULL FIRST; it overrides your defaults. Ownership map: ${repo}/port-map.json${RUST ? '' : ' and porting/filemap.tsv'}.
- User decisions: complete 1:1 port of all 13 packages; on-disk/wire/CLI formats byte-compatible with TS pi; native extension API (user TS extensions are not loaded); codemode runs the quickjs-wasi wasm via ${RUST ? 'wasmtime' : 'wazero'}; unit tests are ported (no network, no real providers).
- Build tools: ALWAYS invoke ${TOOL} (a semaphore wrapper limiting concurrent builds; the machine has 18 GB RAM) instead of plain ${RUST ? 'cargo' : 'go'}, and give those Bash calls timeout 600000 (waiting for a build slot is normal).${RUST ? '' : ' gofmt and ~/go/bin/gopls may be used directly.'}
- Never run git write commands (add/commit/stash/reset/checkout/restore/clean/rebase). Never edit files you do not own. Never run ${RUST ? 'cargo clean/update/fix or workspace-wide cargo fmt (rustfmt on your own files is fine)' : 'go mod tidy/go get/go mod edit'}. Never edit ${RUST ? 'Cargo.toml files' : 'go.mod/go.sum'}; if you need a dependency, report it.
- Use /tmp for scratch files and delete them when done.
- Your final answer is the structured report; be factual (never claim something compiles or passes unless you ran it).`

const ownedList = t => t.owned.map(o => '  - ' + o).join('\n')
// Resume support: foundDone[id] = 'port' (skip port, run parity) | 'all' (skip both); partial[id] = dir with an interrupted attempt's files.
const foundDone = DATA.foundDone || {}
const partial = DATA.partial || {}
const partialNote = (t, step) => partial[t.id] && (step === 'port' ? !foundDone[t.id] : foundDone[t.id] === 'port') ? `\nA previous attempt at this step was interrupted before syncing; its changed files are preserved (same relative paths) under ${partial[t.id]}. Review them and reuse whatever is correct.` : ''
const byPkg = {}
for (const t of tasks) (byPkg[t.package] ||= []).push(t)

const CLONE = (name, unit) => `WORKSPACE PROTOCOL (mandatory): you work in a private copy-on-write clone of the main tree, so concurrent agents cannot break your builds.
  1. WT=$(${BIN}/wt-begin ${lang} ${name})   # prints the clone path; it includes the build cache
  2. Do ALL reading of target code, editing, building and testing inside that clone (absolute paths under it; run ${TOOL} with the clone as cwd).
  3. When finished: ${BIN}/wt-sync ${lang} ${unit} "$WT" --dry  (preview), then without --dry to copy your owned changed files back to ${repo}. If it prints CONFLICT for a file, re-read the main-tree version, merge your change into it inside the clone and sync again. REJECTED files are not yours — revert those edits or report the need instead.
  4. ${BIN}/wt-end "$WT"
  Put the final wt-sync output in sync_output.`

// ---------- foundation ----------
const FOUND_SPEC = RUST
  ? 'PORTING.md Appendix A (binding pi-js API), §6 (TypeBox), §2.6 (vendor table), §13.2 (JS semantics)'
  : 'PORTING.md §5.1 (jsonx), §6 (js, jsre, omap), §9 (typebox), §10 (sse), §11.1 (hand ports table), §7.1 (xspawn)'
const PORT_SCHEMA = { type: 'object', properties: {
  status: { type: 'string', enum: ['complete', 'partial'] }, files: { type: 'array', items: { type: 'string' } },
  tests: { type: 'string', description: 'tests written / passing summary' }, gaps_remaining: { type: 'array', items: { type: 'string' } },
  sync_output: { type: 'string' }, summary: { type: 'string' } },
  required: ['status', 'files', 'tests', 'gaps_remaining', 'sync_output', 'summary'] }

const foundPort = t => `${PREAMBLE}

FOUNDATION UNIT ${t.id}: ${t.title}
Unit spec (read it first): ${SPEC(t.id)}
Owned files (exclusively yours):
${ownedList(t)}
These ${NAME}-only foundation modules are used by every other package. Their API contract is specified in ${FOUND_SPEC}. Implement every specified item with exactly the specified signatures (you may ADD items; never change specified ones). Other foundation units are being written concurrently: ${byPkg.foundation.filter(x => x.id !== t.id).map(x => x.id).join(', ')}. Your dependencies already finished and are in the main tree: ${t.deps.join(', ') || 'none'}.
${CLONE('found-' + t.id, t.id)}
${partialNote(t, 'port')}
Requirements:
- COMPLETE implementations, no placeholders. Fidelity to JS/Node/npm behaviour is the whole point: for hand-ported npm libraries port from the exact pinned sources in ${TS}/node_modules/<pkg>, covering at least everything pi uses (grep ${TS}/packages/*/src for call sites); prefer porting the full public surface reachable from those entry points.
- Thorough tests pinning parity: where useful, generate expected values by running node against the real JS implementation (scripts under /tmp) and embed the vectors in your tests.
- Check in the clone: ${CHECK('foundation')} (filter to your files), then run your tests${RUST ? ' (cargo test -p pi-js ...)' : ' (go test ./internal/<yourpkg>/...)'}. Everything you own must compile cleanly (no warnings) and your tests must pass before syncing.`

const foundParity = t => `${PREAMBLE}

ADVERSARIAL PARITY REVIEW of foundation unit ${t.id}: ${t.title}
Unit spec: ${SPEC(t.id)}
Owned files (you may edit them):
${ownedList(t)}
Another agent just implemented this unit (contract: ${FOUND_SPEC}). Assume it has gaps until proven otherwise.
${CLONE('fparity-' + t.id, t.id)}
${partialNote(t, 'parity')}
1. Diff the implementation against the contract item by item (every specified function/type/method must exist with the exact signature and full behaviour).
2. For hand-ported npm libraries, compare with the JS source in ${TS}/node_modules: list every code path pi relies on (grep ${TS}/packages/*/src usages) and verify each is ported (edge cases, error messages, options).
3. Hunt for JS-semantics mistakes: UTF-16 vs bytes, number formatting, JSON escaping/ordering, regex flavour differences, Node error message texts, timing/abort semantics.
4. grep for placeholders (todo!, unimplemented!, panic("unported"), TODO, FIXME, "not implemented").
5. Fix every gap directly, add missing tests (node-generated vectors where possible), re-run the check (${CHECK('foundation')}) and the tests, then sync.
In gaps_remaining list anything you could not fix.`

const INT_SCHEMA = { type: 'object', properties: {
  green: { type: 'boolean' }, summary: { type: 'string' }, remaining_errors: { type: 'array', items: { type: 'string' } } },
  required: ['green', 'summary', 'remaining_errors'] }

const foundInt = prev => `${PREAMBLE}

FOUNDATION INTEGRATION (main tree ${repo}, work there directly). All foundation units (${byPkg.foundation.map(x => x.id).join(', ')}) are implemented and reviewed. Make the whole foundation (${RUST ? 'crate pi-js' : './internal/...'}) build with zero errors and zero warnings, and ALL its tests pass:
  ${CHECK('foundation')}
  ${RUST ? TOOL + ' test -p pi-js' : TOOL + ' test ./internal/...'}
You may edit any foundation file. Fix root causes; never delete or skip tests to get green. Also confirm the contract in PORTING.md is fully present (add anything missing).
${prev ? 'A previous integration round ended with: ' + JSON.stringify(prev) : ''}
Return {green, summary, remaining_errors}.`

// ---------- skeleton ----------
const SKEL_SCHEMA = { type: 'object', properties: {
  status: { type: 'string', enum: ['complete', 'partial'] }, files: { type: 'array', items: { type: 'string' } },
  unresolved_refs: { type: 'array', items: { type: 'string' } }, needs_deps: { type: 'array', items: { type: 'string' } }, notes: { type: 'string' } },
  required: ['status', 'files', 'unresolved_refs', 'needs_deps', 'notes'] }

const skelPrompt = t => `${PREAMBLE}

PHASE A — API SKELETON for unit ${t.id} (TS package "${t.package}"): ${t.title}
We port in two phases. In phase A (you), every owned SOURCE file receives the complete *shape* of its TS counterpart, so that in phase B many agents can implement bodies in parallel against a stable, compiling API.
Unit spec (read it first — TS files, owned targets, reader notes, imported targets): ${SPEC(t.id)}
TS files of this unit (read each IN FULL): ${t.ts_files.join(', ')}
Owned target files (in this phase write ONLY the non-test source/support files; test files are written in phase B):
${ownedList(t)}
Work directly in the main tree ${repo}. Upstream packages (${pkgDeps[t.package].join(', ')}) already have compiling skeletons or implementations — read their ${NAME} code and use their real names/types.
Sibling units of this package are being skeletoned concurrently in the same tree. If a sibling item you need is not declared yet, reference it by its deterministic PORTING.md path/name anyway (it is reconciled in A-INT); never declare another unit's items yourself.
For each owned source file, write:
- all imports; keep the header line required by PORTING.md;
- EVERY type (struct/enum/trait/interface/type alias/union) with ALL fields/variants, exact ${RUST ? 'serde attributes' : 'json tags / custom (un)marshalers'} and field order per PORTING.md, doc comments ported from TSDoc;
- EVERY constant/static/table with its real value (regexes, templates, defaults, prompt texts, big tables included);
- EVERY function, method, constructor and ${RUST ? 'trait impl' : 'interface implementation'} — exported and private — with its final signature and placeholder body \`${PLACEHOLDER}\`. Trivial bodies (about 10 lines or less: getters, simple constructors, defaults, enum<->string) should be implemented fully now.
- module wiring (re-exports for index.ts per PORTING.md, registrations).
Keep every file parseable at all times (other agents compile the same ${RUST ? 'crate' : 'packages'}).
Check: ${CHECK(t.package)} (cwd ${repo}) filtered to your files; fix every error located in your files. Allowed leftovers: references to sibling-unit items not declared yet — list them in unresolved_refs. Crates/modules you need that are not available go in needs_deps.`

const TRIAGE_SCHEMA = { type: 'object', properties: {
  green: { type: 'boolean' }, error_count: { type: 'number' },
  groups: { type: 'array', items: { type: 'object', properties: {
    name: { type: 'string' }, paths: { type: 'array', items: { type: 'string' } }, summary: { type: 'string' } }, required: ['name', 'paths', 'summary'] } } },
  required: ['green', 'error_count', 'groups'] }
const FIX_SCHEMA = { type: 'object', properties: { summary: { type: 'string' }, cross_group_needs: { type: 'array', items: { type: 'string' } } }, required: ['summary', 'cross_group_needs'] }

const intBase = (pkg, reports) => `${PREAMBLE}

PHASE A-INT for TS package "${pkg}" (${RUST ? 'crate ' + CRATE[pkg] : GODIRS[pkg]}), main tree ${repo}. All phase-A skeleton agents of this package finished. Make the package compile with zero errors (placeholder bodies are fine; warnings OK) together with its upstream packages:
  ${CHECK(pkg)}
Resolve cross-unit mismatches with the TS source as truth and PORTING.md naming: declare missing items in the file that owns them per port-map.json (full fields/signature + placeholder body); fix signature/type mismatches; ${RUST ? 'fix module paths/visibility' : 'break import cycles per PORTING.md and fix duplicate identifiers per the naming rules'}. Do not implement bodies beyond trivial ones. Do not modify other packages except minimal additive declarations when unavoidable (then re-check those packages too). If a missing external dependency blocks compilation, you MAY add it to ${RUST ? 'the crate Cargo.toml using the version pinned in PORTING.md / [workspace.dependencies]' : 'go.mod using the version pinned in PORTING.md (go get <mod>@<pinned version>)'} and say so in the summary.
Unit spec files for this package: ${DATA.unitsDir}/<unit-id>.md
Phase-A reports for this package (unresolved refs, needs, notes): ${JSON.stringify(reports)}`

async function intPackage(pkg, reports) {
  const units = byPkg[pkg] || []
  if (units.length <= 14) {
    let last = null
    for (let round = 1; round <= 4; round++) {
      last = await agent(`${intBase(pkg, reports)}\n${last ? `A previous integration round ended with: ${JSON.stringify(last)}` : ''}\nReturn {green, summary, remaining_errors}.`,
        { label: `skel-int:${pkg}:r${round}`, phase: 'Skeleton-Int', schema: INT_SCHEMA })
      if (last && last.green) return last
    }
    return last
  }
  let needs = []
  for (let round = 1; round <= 6; round++) {
    const tri = await agent(`${intBase(pkg, reports)}
You are the TRIAGE step of round ${round}. Run the check, then partition the failing files into 3-10 groups of related files (by module/directory) so that parallel fixers can each own one group (groups must not share files). Do not fix anything yourself unless the total error count is below 40 (then fix them all and report green only after a clean re-check).
${needs.length ? 'Cross-group needs reported by the previous round fixers (apply these first, they are yours to do):\n' + needs.join('\n') : ''}`,
      { label: `skel-int:${pkg}:triage${round}`, phase: 'Skeleton-Int', schema: TRIAGE_SCHEMA })
    if (!tri) continue
    if (tri.green) return { green: true, summary: `green after triage round ${round}`, remaining_errors: [] }
    log(`${pkg} A-int round ${round}: ${tri.error_count} errors in ${tri.groups.length} groups`)
    const fixes = await parallel(tri.groups.map(g => () => agent(`${intBase(pkg, reports)}
You are a FIXER in round ${round}, owning ONLY these paths: ${g.paths.join(', ')}. Group "${g.name}": ${g.summary}
Fix every compile error located in your paths. If a fix requires declaring/changing an item in a file outside your paths, do NOT edit it; describe the exact need (file, item, full signature) in cross_group_needs.`,
      { label: `skel-int:${pkg}:fix${round}:${g.name}`, phase: 'Skeleton-Int', schema: FIX_SCHEMA })))
    needs = fixes.filter(Boolean).flatMap(f => f.cross_group_needs)
  }
  return await agent(`${intBase(pkg, reports)}\nFINAL integration pass after 6 parallel rounds; outstanding needs:\n${needs.join('\n')}\nGet it to zero errors. Return {green, summary, remaining_errors}.`,
    { label: `skel-int:${pkg}:final`, phase: 'Skeleton-Int', schema: INT_SCHEMA })
}

// ---------- scheduler ----------
const results = { foundation: {}, skeleton: {} }
const unitDone = {}
function runFoundationUnit(t) {
  if (!unitDone[t.id]) unitDone[t.id] = (async () => {
    await Promise.all(t.deps.map(d => runFoundationUnit(byPkg.foundation.find(x => x.id === d))))
    const port = foundDone[t.id] ? { status: 'complete', summary: 'ported in an earlier run' } : await agent(foundPort(t), { label: `found:${t.id}`, phase: 'Foundation', schema: PORT_SCHEMA })
    results.foundation[t.id] = { port }
    log(`foundation ported: ${t.id} (${port ? port.status : 'agent failed'})`)
    return port
  })()
  return unitDone[t.id]
}
const apiReady = {}
function ready(pkg) {
  if (!apiReady[pkg]) apiReady[pkg] = (async () => {
    if (pkg === 'foundation') {
      await parallel(byPkg.foundation.map(t => () => runFoundationUnit(t).then(async () => {
        if (foundDone[t.id] !== 'all') results.foundation[t.id].parity = await agent(foundParity(t), { label: `found-parity:${t.id}`, phase: 'Foundation', schema: PORT_SCHEMA })
      })))
      let int = null
      for (let round = 1; round <= 4; round++) {
        int = await agent(foundInt(int), { label: `found-int:r${round}`, phase: 'Foundation', schema: INT_SCHEMA })
        if (int && int.green) break
      }
      results.foundation.int = int
      log(`foundation integrated: green=${int && int.green}`)
      return
    }
    await Promise.all(pkgDeps[pkg].map(ready))
    const units = byPkg[pkg] || []
    log(`skeleton start: ${pkg} (${units.length} units)`)
    const reps = await parallel(units.map(t => () => agent(skelPrompt(t), { label: `skel:${t.id}`, phase: 'Skeleton', schema: SKEL_SCHEMA })))
    const compact = units.map((t, i) => ({ unit: t.id, status: reps[i] ? reps[i].status : 'agent-failed', unresolved: reps[i] ? reps[i].unresolved_refs : [], needs_deps: reps[i] ? reps[i].needs_deps : [], notes: reps[i] ? reps[i].notes : '' }))
    const int = await intPackage(pkg, compact)
    results.skeleton[pkg] = { units: compact, int }
    log(`skeleton integrated: ${pkg} green=${int && int.green}`)
  })()
  return apiReady[pkg]
}
const PKGS = ['telemetry', 'chord', 'tui', 'mcp', 'codemode', 'ai', 'protocol', 'agent', 'client', 'server', 'durable', 'coding-agent', 'evals']
await parallel(PKGS.map(p => () => ready(p)))
return results
