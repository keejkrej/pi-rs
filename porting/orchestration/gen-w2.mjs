// usage: node gen-w2.mjs <lang> <part a|b|c|d|e> [priorReports.json]  -> $PI_PORT_HOME/w2-<lang>-<part>.js
// a: telemetry, ai, agent, durable | b: chord, tui, mcp, codemode, protocol, client, server (a and b run concurrently)
// c: coding-agent lower half | d: coding-agent upper half (start c when b finishes, d when a finishes)
// e: coding-agent integration, then evals + docs/examples units (pass merged c+d unit results as priorReports)
import fs from 'node:fs'; import path from 'node:path'
import { HOME, BIN, TS, MAIN, toSlash } from './config.mjs'
import { PKG_DEPS } from './pkg-deps.mjs'
const [lang, part, priorFile] = process.argv.slice(2)
const CA1 = ['config-foundation-utils', 'tools-primitives', 'utils-images-clipboard', 'utils-network-git-release', 'tools-file-edit', 'tools-shell-search', 'tools-renderers-highlight', 'interactive-theme', 'interactive-components-foundation', 'keybindings-footer-http', 'skills-prompt-templates', 'settings-manager', 'session-manager', 'credential-stores', 'model-runtime', 'compaction', 'export-html', 'package-manager', 'cli-args-auth-migrations', 'interactive-components-dialogs', 'interactive-components-session-selector', 'interactive-components-tree-selector', 'interactive-components-config-selector', 'interactive-components-easter-eggs', 'interactive-components-messages', 'exp-process-coordinator', 'exp-services', 'ext-runner'].map(x => 'coding-agent-' + x)
const j = JSON.parse(fs.readFileSync(path.join(HOME, `pi-port-tasks-${lang}.json`), 'utf8'))
const ids = pkgs => j.tasks.filter(t => pkgs.includes(t.package)).map(t => t.id)
const CA = ids(['coding-agent'])
for (const x of CA1) if (!CA.includes(x)) throw new Error('unknown ' + x)
const CA2 = CA.filter(x => !CA1.includes(x))
const PARTS = {
  a: { units: ids(['telemetry', 'ai', 'agent', 'durable']), intPkgs: ['telemetry', 'ai', 'agent', 'durable'] },
  b: { units: ids(['chord', 'tui', 'mcp', 'codemode', 'protocol', 'client', 'server']), intPkgs: ['chord', 'tui', 'mcp', 'codemode', 'protocol', 'client', 'server'] },
  c: { units: CA1, intPkgs: [] },
  d: { units: CA2, intPkgs: [] },
  e: { units: [...ids(['evals']), ...ids(['coding-agent-post'])], intPkgs: ['coding-agent', 'evals', 'coding-agent-post'], gateUnitsOnInt: true },
}
const P = PARTS[part]; if (!P) throw new Error('part?')
const tasks = j.tasks.map(t => ({ id: t.id, package: t.package, title: t.title, ts_files: t.ts_files, deps: t.deps, owned: t.owned }))
const priorReports = priorFile ? JSON.parse(fs.readFileSync(priorFile, 'utf8')) : {}
const data = { lang, repo: toSlash(MAIN[lang]), ts: toSlash(TS), bin: toSlash(BIN), unitsDir: toSlash(path.join(HOME, 'units', lang)), label: `${lang}/${part}`, pkgDeps: PKG_DEPS, tasks, ...P, priorReports }
const out = fs.readFileSync(path.join(HOME, 'w2-template.js'), 'utf8').replace('__DATA__', JSON.stringify(data))
  .replace("name: 'pi-port-w2-implement'", `name: 'pi-port-w2-${lang}-${part}'`)
fs.writeFileSync(path.join(HOME, `w2-${lang}-${part}.js`), out)
console.log(`${path.join(HOME, `w2-${lang}-${part}.js`)} units=${P.units.length} ints=${P.intPkgs.join(',')} bytes=${out.length}`)
