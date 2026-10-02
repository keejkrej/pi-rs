// Private workspace helper for parallel porting agents.
// wt-begin   <lang> <name>                      -> clones main tree into $PI_PORT_HOME/wt/<lang>-<name>, prints path
// wt-sync    <lang> <unit|--any> <wtdir> [--dry] -> copies changed/new owned files back to main tree
// wt-refresh <lang> <wtdir>                      -> pulls main-tree changes into files the agent has not modified
// wt-end     <wtdir>                             -> removes the private workspace
// Clone method: macOS uses APFS clonefile (cp -c), Linux tries reflinks (cp --reflink=auto); otherwise a plain recursive copy.
// Rust target/ dirs are copied only on macOS or when PI_PORT_CLONE_TARGET=1 (use sccache instead on non-CoW filesystems).
import fs from 'node:fs'; import path from 'node:path'; import crypto from 'node:crypto'; import { execFileSync } from 'node:child_process'
import { HOME, MAIN } from '../config.mjs'
const WTROOT = path.join(HOME, 'wt')
const SKIP = new Set(['.git', 'target', 'legacy', 'node_modules'])
const [cmd, ...a] = process.argv.slice(2)
function walk(root, rel = '', out = {}) {
  for (const e of fs.readdirSync(path.join(root, rel), { withFileTypes: true })) {
    if (SKIP.has(e.name) || e.name === '.pi-port-manifest.json' || (rel === '' && e.name.startsWith('target'))) continue
    const r = rel ? rel + '/' + e.name : e.name
    if (e.isDirectory()) walk(root, r, out)
    else if (e.isFile()) out[r] = crypto.createHash('md5').update(fs.readFileSync(path.join(root, r))).digest('hex')
  }
  return out
}
function die(m) { console.error(m); process.exit(1) }
function cloneTree(src, dst) {
  const withTarget = process.platform === 'darwin' || process.env.PI_PORT_CLONE_TARGET === '1'
  if (process.platform === 'darwin') { execFileSync('cp', ['-c', '-R', src, dst]); return }
  if (process.platform === 'linux' && withTarget) {
    try { execFileSync('cp', ['--reflink=auto', '-R', src, dst]); return } catch { fs.rmSync(dst, { recursive: true, force: true }) }
  }
  fs.cpSync(src, dst, { recursive: true, filter: s => {
    const rel = path.relative(src, s); const top = rel.split(path.sep)[0]
    if (top === 'legacy' || top === '.git') return false
    if (!withTarget && /^target/.test(top)) return false
    return true
  } })
}
if (cmd === 'begin') {
  const [lang, name] = a; if (!MAIN[lang] || !name) die('usage: wt-begin <rust|go> <name>')
  const dst = path.join(WTROOT, `${lang}-${name.replace(/[^A-Za-z0-9_.-]/g, '_')}`)
  fs.mkdirSync(WTROOT, { recursive: true })
  fs.rmSync(dst, { recursive: true, force: true })
  cloneTree(MAIN[lang], dst)
  fs.rmSync(path.join(dst, 'legacy'), { recursive: true, force: true })
  fs.writeFileSync(path.join(dst, '.pi-port-manifest.json'), JSON.stringify({ lang, files: walk(MAIN[lang]) }))
  console.log(dst)
} else if (cmd === 'sync') {
  const [lang, unit, wt, flag] = a; const dry = flag === '--dry'
  if (!MAIN[lang] || !unit || !wt) die('usage: wt-sync <rust|go> <unit-id|--any> <wtdir> [--dry]')
  const man = JSON.parse(fs.readFileSync(path.join(wt, '.pi-port-manifest.json'), 'utf8')).files
  const now = walk(wt); const mainNow = walk(MAIN[lang])
  let allow = () => true
  if (unit !== '--any') {
    const tasks = JSON.parse(fs.readFileSync(path.join(HOME, `pi-port-tasks-${lang}.json`), 'utf8')).tasks
    const t = tasks.find(x => x.id === unit); if (!t) die('unknown unit ' + unit)
    const pats = t.owned.map(o => o.split('  <=  ')[0].split(' (')[0].trim())
    const res = pats.map(p => p.endsWith('/') ? new RegExp('^' + p.replace(/[.+?^${}()|[\]\\]/g, '\\$&')) :
      new RegExp('^' + p.replace(/[.+?^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '[^/]*') + '$'))
    const extra = lang === 'rust' ? [/^interop\//, /^crates\/[^/]+\/tests\/(golden|fixtures)\//]
      : [/^porting\/needs\//, /(^|\/)testdata\//]
    const plat = lang === 'go' ? pats.filter(p => p.endsWith('.go')).map(p => new RegExp('^' + p.replace(/\.go$/, '').replace(/[.+?^${}()|[\]\\]/g, '\\$&') + '_(unix|windows|darwin|linux|other|freebsd|bsd)(_test)?\\.go$')) : []
    allow = f => res.some(r => r.test(f)) || extra.some(r => r.test(f)) || plat.some(r => r.test(f))
  }
  const copied = [], rejected = [], conflicts = [], deleted = []
  for (const [f, h] of Object.entries(now)) {
    if (man[f] === h) continue
    if (!allow(f)) { rejected.push(f); continue }
    if (man[f] !== undefined && mainNow[f] !== undefined && mainNow[f] !== man[f] && mainNow[f] !== h) { conflicts.push(f); continue }
    if (man[f] === undefined && mainNow[f] !== undefined && mainNow[f] !== h) { conflicts.push(f); continue }
    if (!dry) { fs.mkdirSync(path.dirname(path.join(MAIN[lang], f)), { recursive: true }); fs.copyFileSync(path.join(wt, f), path.join(MAIN[lang], f)) }
    copied.push(f)
  }
  for (const f of Object.keys(man)) if (!(f in now)) deleted.push(f)
  console.log(`${dry ? '[dry] ' : ''}copied ${copied.length}:${copied.map(f => '\n  ' + f).join('')}`)
  if (rejected.length) console.log(`REJECTED (not owned by ${unit}; NOT copied) ${rejected.length}:${rejected.map(f => '\n  ' + f).join('')}`)
  if (conflicts.length) console.log(`CONFLICT (main tree changed this file since your clone; NOT copied — merge the main version into your clone, then sync again) ${conflicts.length}:${conflicts.map(f => '\n  ' + f).join('')}`)
  if (deleted.length) console.log(`deleted in workspace (NOT propagated) ${deleted.length}:${deleted.map(f => '\n  ' + f).join('')}`)
} else if (cmd === 'refresh') {
  const [lang, wt] = a
  if (!MAIN[lang] || !wt) die('usage: wt-refresh <rust|go> <wtdir>')
  const mp = path.join(wt, '.pi-port-manifest.json')
  const manObj = JSON.parse(fs.readFileSync(mp, 'utf8')); const man = manObj.files
  const now = walk(wt); const mainNow = walk(MAIN[lang])
  const pulled = [], kept = []
  for (const [f, h] of Object.entries(mainNow)) {
    if (man[f] === h) continue
    if (now[f] !== undefined && now[f] !== man[f] && now[f] !== h) { kept.push(f); continue }
    fs.mkdirSync(path.dirname(path.join(wt, f)), { recursive: true }); fs.copyFileSync(path.join(MAIN[lang], f), path.join(wt, f))
    man[f] = h; pulled.push(f)
  }
  fs.writeFileSync(mp, JSON.stringify(manObj))
  console.log(`pulled ${pulled.length} file(s) from main tree:${pulled.map(f => '\n  ' + f).join('')}`)
  if (kept.length) console.log(`kept your local version (main also changed; merge manually if needed) ${kept.length}:${kept.map(f => '\n  ' + f).join('')}`)
} else if (cmd === 'end') {
  const [wt] = a; if (!wt || !path.resolve(wt).startsWith(path.resolve(WTROOT) + path.sep)) die('refusing: not under ' + WTROOT)
  fs.rmSync(wt, { recursive: true, force: true }); console.log('removed', wt)
} else die('unknown command')
