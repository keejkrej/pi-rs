// usage: node gen-w1.mjs <rust|go>  -> $PI_PORT_HOME/w1-<lang>.js (foundation + API skeleton cascade)
// Resume state (foundation units already done, partial work dirs) comes from state/<lang>.json if present.
import fs from 'node:fs'; import path from 'node:path'
import { HOME, BIN, TS, MAIN, toSlash } from './config.mjs'
import { PKG_DEPS } from './pkg-deps.mjs'
const langs = process.argv[2] ? [process.argv[2]] : ['rust', 'go']
const tpl = fs.readFileSync(path.join(HOME, 'w1-template.js'), 'utf8')
for (const lang of langs) {
  const j = JSON.parse(fs.readFileSync(path.join(HOME, `pi-port-tasks-${lang}.json`), 'utf8'))
  const tasks = j.tasks.map(t => ({ id: t.id, package: t.package, title: t.title, ts_files: t.ts_files, deps: t.deps, owned: t.owned }))
  const sf = path.join(HOME, 'state', `${lang}.json`)
  const state = fs.existsSync(sf) ? JSON.parse(fs.readFileSync(sf, 'utf8')) : {}
  const partial = Object.fromEntries(Object.entries(state.partial || {}).map(([k, v]) => [k, toSlash(path.resolve(HOME, v))]))
  const data = { lang, repo: toSlash(MAIN[lang]), ts: toSlash(TS), bin: toSlash(BIN), unitsDir: toSlash(path.join(HOME, 'units', lang)), pkgDeps: PKG_DEPS, tasks, foundDone: state.foundDone || {}, partial }
  const out = tpl.replace('__DATA__', JSON.stringify(data))
  fs.writeFileSync(path.join(HOME, `w1-${lang}.js`), out)
  console.log(path.join(HOME, `w1-${lang}.js`), out.length, 'bytes; foundDone:', JSON.stringify(data.foundDone))
}
