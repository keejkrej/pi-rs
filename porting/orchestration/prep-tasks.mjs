import fs from 'node:fs'
const U = JSON.parse(fs.readFileSync('/tmp/pi-port-understand.json', 'utf8'))
const readerUnits = U.reports.flatMap(r => r.units)
const strip = f => f.split('#')[0]

const POST = id => /^coding-agent-(docs|examples)-/.test(id)
const PKG_DEPS = {
  foundation: [],
  telemetry: ['foundation'], chord: ['foundation'], tui: ['foundation'], mcp: ['foundation'], codemode: ['foundation'],
  ai: ['foundation', 'telemetry'], protocol: ['foundation', 'chord'],
  agent: ['foundation', 'ai'], client: ['foundation', 'chord', 'protocol'], server: ['foundation', 'chord', 'protocol'],
  durable: ['foundation', 'chord', 'ai'],
  'coding-agent': ['foundation', 'chord', 'agent', 'ai', 'client', 'codemode', 'durable', 'mcp', 'protocol', 'server', 'tui', 'telemetry'],
  'coding-agent-post': ['coding-agent'],
  evals: ['foundation', 'ai', 'agent', 'coding-agent'],
}

function foundation(lang) {
  if (lang === 'rust') {
    const p = 'crates/pi-js/src/'
    return [
      { id: 'pijs-core', title: 'pi-js core: lib.rs, error, callback, abort, seam, testing, console', owned: ['lib.rs','error.rs','callback.rs','abort.rs','seam.rs','testing.rs','console.rs'].map(f=>p+f), deps: [] },
      { id: 'pijs-data', title: 'pi-js data semantics: json (V8 JSON.stringify/parse incl. Node 24 error messages), num, str16 (UTF-16), text, b64, uri, crypto', owned: ['json.rs','num.rs','str16.rs','text.rs','b64.rs','uri.rs','crypto.rs'].map(f=>p+f), deps: ['pijs-core'] },
      { id: 'pijs-system', title: 'pi-js system: time (fakeable clock, timers), env (overlay + guards), path (Node path posix/win32), fs (node:fs with Node error messages, sync + promises)', owned: ['time.rs','env.rs','path.rs','fs.rs'].map(f=>p+f), deps: ['pijs-core'] },
      { id: 'pijs-net-intl', title: 'pi-js fetch (reqwest WHATWG-like fetch, Headers, SSE parser, mock_fetch/deny_network), regex (ecma via regress, JS_WS_CLASS), intl (ICU collation/segmentation, en-US number/date formatting)', owned: ['fetch.rs','regex.rs','intl.rs'].map(f=>p+f), deps: ['pijs-core'] },
      { id: 'pijs-typebox', title: 'pi-js vendor/typebox: hand port of typebox 1.3.27 subset (Type builders with exact key order, Compile/Validator, Value.Check/Errors/Convert/Clean/Default, en_US messages) from node_modules/typebox', owned: [p+'vendor/typebox.rs', p+'vendor/typebox/'], deps: ['pijs-core','pijs-data','pijs-net-intl'] },
      { id: 'pijs-jsdiff', title: 'pi-js vendor/jsdiff: hand port of diff@8.0.4 (diffLines, diffWords, structuredPatch, createTwoFilesPatch, createPatch, applyPatch, parsePatch) with identical output', owned: [p+'vendor/jsdiff.rs', p+'vendor/jsdiff/'], deps: ['pijs-core','pijs-data'] },
    ].map(u => ({ ...u, owned: [...u.owned, `crates/pi-js/tests/${u.id.replace('pijs-','pijs_')}*.rs (new test files you create for your modules)`] }))
  }
  const g = (id, title, pkgs, deps) => ({ id, title, owned: [...pkgs.map(x => `internal/${x}/ (whole package dir, incl. _test.go and testdata/)`)], deps })
  return [
    g('go-js-omap', 'internal/js (JS semantics: UTF-16 strings, numbers, dates, platform, URI, errors, Sleep — PORTING.md 6.1/6.2) and internal/omap (ordered Map/Set, 6.4)', ['js','omap'], []),
    g('go-jsonx', 'internal/jsonx (V8-exact JSON.stringify/parse, ordered Object, Opt[T], Node 24 parse error messages — PORTING.md 5.1)', ['jsonx'], ['go-js-omap']),
    g('go-jsre-sse-xspawn', 'internal/jsre (regexp2 ECMAScript wrapper, UTF-16 indices, 6.3), internal/sse (eventsource-parser semantics, section 10), internal/xspawn (cross-spawn 7.0.6 port)', ['jsre','sse','xspawn'], ['go-js-omap']),
    g('go-typebox', 'internal/typebox: hand port of typebox 1.3.27 subset (section 9) from node_modules/typebox', ['typebox'], ['go-jsonx','go-jsre-sse-xspawn']),
    g('go-jsdiff-partialjson', 'internal/jsdiff (diff@8.0.4 identical output) and internal/partialjson (partial-json@0.1.7)', ['jsdiff','partialjson'], ['go-jsonx']),
    g('go-ignore-minimatch-semver', 'internal/ignore (ignore@7.0.8), internal/minimatch (minimatch@10.2.6), internal/semver (semver@7.8.5 npm semantics)', ['ignore','minimatch','semver'], ['go-js-omap','go-jsre-sse-xspawn']),
    g('go-hostedgit-lockfile-ansi', 'internal/hostedgit (hosted-git-info@9.0.3), internal/lockfile (proper-lockfile@4.1.2, interoperable with running TS pi), internal/ansi (chalk@6.0.0 + supports-color)', ['hostedgit','lockfile','ansi'], ['go-js-omap']),
    g('go-marked', 'internal/marked: hand port of marked@18.0.11 Lexer (block+inline tokenizers, extensions) with token structs matching Tokens.*', ['marked'], ['go-js-omap','go-jsre-sse-xspawn']),
    g('go-eastasian-highlight-mermaid', 'internal/eastasian (get-east-asian-width@1.6.0 tables), internal/highlight (highlight.js 10.7.3 replacement via chroma mapped to hljs scopes), internal/mermaid (grok-mermaid@0.2.3 port)', ['eastasian','highlight','mermaid'], ['go-js-omap','go-jsre-sse-xspawn']),
    g('go-quickjs', 'internal/quickjs: port of quickjs-wasi@3.6.2 JS glue (dist/index.js, wasi-shim.js) on wazero running the embedded internal/quickjs/quickjs.wasm (do not modify the wasm)', ['quickjs'], ['go-js-omap']),
  ]
}

function build(lang) {
  const repo = lang === 'rust' ? '/Users/jack/workspace/pi-rs' : '/Users/jack/workspace/pi-go'
  const pm = JSON.parse(fs.readFileSync(repo + '/port-map.json', 'utf8'))
  const tKey = lang === 'rust' ? 'rust' : 'go', tTestKey = lang === 'rust' ? 'rust_test' : 'go_test'
  const units = readerUnits.map(u => ({ ...u, package: POST(u.id) ? 'coding-agent-post' : u.package }))
  const fileToUnit = {}
  for (const u of units) for (const f of u.ts_files) { const k = strip(f); (fileToUnit[k] ||= []).includes(u.id) || fileToUnit[k].push(u.id) }
  // intra-package deps
  const byId = Object.fromEntries(units.map(u => [u.id, u]))
  const rawDeps = {}
  for (const u of units) {
    const s = new Set()
    for (const f of u.depends_on_files) for (const d of (fileToUnit[strip(f)] || [])) if (d !== u.id && byId[d].package === u.package) s.add(d)
    rawDeps[u.id] = [...s]
  }
  // Tarjan SCC
  let idx = 0; const st = [], on = new Set(), ix = {}, low = {}, comp = {}; let c = 0
  const sc = v => { ix[v] = low[v] = idx++; st.push(v); on.add(v)
    for (const w of rawDeps[v]) { if (ix[w] === undefined) { sc(w); low[v] = Math.min(low[v], low[w]) } else if (on.has(w)) low[v] = Math.min(low[v], ix[w]) }
    if (low[v] === ix[v]) { let w; do { w = st.pop(); on.delete(w); comp[w] = c } while (w !== v); c++ } }
  for (const u of units) if (ix[u.id] === undefined) sc(u.id)
  const deps = {}; for (const u of units) deps[u.id] = rawDeps[u.id].filter(d => comp[d] !== comp[u.id])
  // depth for shared-ownership resolution
  const lvl = { foundation: 0, telemetry: 1, chord: 1, tui: 1, mcp: 1, codemode: 1, ai: 2, protocol: 2, agent: 3, client: 3, server: 3, durable: 3, 'coding-agent': 4, 'coding-agent-post': 5, evals: 6 }
  const depthMemo = {}; const depth = id => depthMemo[id] ??= (deps[id].length ? 1 + Math.max(...deps[id].map(depth)) : 0)
  const rank = id => lvl[byId[id].package] * 1000 + depth(id)
  // ownership
  const targetsByUnit = {}
  const byTarget = {}
  for (const e of pm) { const t = e[tKey] ?? e[tTestKey]; if (!t || !e.unit) continue; (byTarget[t] ||= []).push(e) }
  for (const [t, es] of Object.entries(byTarget)) {
    let owner
    const explicit = es.find(e => e.owner); if (explicit) owner = explicit.owner
    else { const us = [...new Set(es.map(e => e.unit))].filter(x => byId[x]); owner = us.sort((a, b) => rank(b) - rank(a))[0] }
    if (!owner) continue
    ;(targetsByUnit[owner] ||= []).push({ ts: es[0].ts ?? es[0].ts_test, target: t, kind: es[0].kind })
  }
  // orphans
  if (lang === 'rust') (targetsByUnit['coding-agent-cli-main-entry'] ||= []).push({ ts: 'packages/coding-agent/src/client/index.ts', target: 'crates/pi-coding-agent/src/client.rs' })
  const tuiText = units.find(u => u.package === 'tui' && /text|util/.test(u.id))?.id
  ;(targetsByUnit[tuiText] ||= []).push({ ts: 'packages/coding-agent/test/truncate-to-width.test.ts', target: lang === 'rust' ? 'crates/pi-tui/tests/coding_agent__truncate_to_width.rs (create)' : 'tui/coding_agent_truncate_to_width_test.go (create)' })
  // ts->target map for cross refs
  const tsTarget = {}; for (const e of pm) if (e.ts && e[tKey]) tsTarget[strip(e.ts)] = e[tKey]
  const tasks = units.map(u => ({
    id: u.id, package: u.package, title: u.title, ts_files: u.ts_files, test_files: u.test_files, skipped_tests: u.skipped_tests,
    notes: u.notes, src_lines: u.src_lines, deps: deps[u.id],
    owned: (targetsByUnit[u.id] || []).map(x => `${x.target}  <=  ${x.ts}${x.kind ? ' [' + x.kind + ']' : ''}`),
    dep_targets: [...new Set(u.depends_on_files.map(f => tsTarget[strip(f)]).filter(Boolean))],
  }))
  const fnd = foundation(lang).map(f => ({ id: f.id, package: 'foundation', title: f.title, ts_files: [], test_files: [], skipped_tests: [], notes: '', src_lines: 0, deps: f.deps, owned: f.owned, dep_targets: [] }))
  const all = [...fnd, ...tasks]
  const empty = all.filter(t => !t.owned.length).map(t => t.id)
  console.error(lang, 'tasks', all.length, 'no-owned', empty, 'sccs>1', Object.values(Object.groupBy(units, u => comp[u.id])).filter(g => g.length > 1).map(g => g.map(x => x.id)))
  fs.writeFileSync(`/tmp/pi-port-tasks-${lang}.json`, JSON.stringify({ lang, repo, pkgDeps: PKG_DEPS, tasks: all }))
  console.error(lang, 'bytes', fs.statSync(`/tmp/pi-port-tasks-${lang}.json`).size)
}
build('rust'); build('go')
