import fs from 'node:fs'
const tpl = fs.readFileSync(new URL('./w2-template.js', import.meta.url), 'utf8').replace(/^export const meta[\s\S]*?\n}\n/, '')
const tasks = [
  ...Array.from({ length: 15 }, (_, i) => ({ id: 'ai-' + i, package: 'ai', title: 't', owned: ['x'] })),
  ...Array.from({ length: 3 }, (_, i) => ({ id: 'ag-' + i, package: 'agent', title: 't', owned: ['x'] })),
  { id: 'tel', package: 'telemetry', title: 't', owned: ['x'] },
]
const DATA = { lang: 'rust', repo: '/r', label: 'sim', pkgDeps: { ai: ['telemetry'], agent: ['ai'], telemetry: [] }, tasks, units: ['tel', ...tasks.filter(t => t.id !== 'tel').map(t => t.id)], intPkgs: ['telemetry', 'ai', 'agent'], gateUnitsOnInt: true }
let running = 0, max = 0, order = [], tri = 0
const agent = async (prompt, opts) => {
  running++; max = Math.max(max, running); order.push(opts.label)
  await new Promise(r => setTimeout(r, 5 + (opts.label.length % 7)))
  running--
  if (opts.label.includes('triage')) { tri++; return tri < 2 ? { green: false, failing: '3', groups: [{ name: 'g1', paths: ['a'], summary: '' }, { name: 'g2', paths: ['b'], summary: '' }] } : { green: true, failing: '0', groups: [] } }
  if (opts.label.includes('fix')) return { summary: '', cross_group_needs: ['need'] }
  if (opts.label.startsWith('int:')) return { green: true, tests: 'ok', summary: '', remaining: [] }
  return { status: 'complete', tests: '', placeholders_left: [], api_changes: [], waiting_on: [], needs_deps: [], gaps_remaining: [], files: [], sync_output: '', summary: '' }
}
const log = m => {}
const fn = new Function('agent', 'log', 'return (async()=>{' + tpl.replace('__DATA__', JSON.stringify(DATA)) + '})()')
const res = await fn(agent, log)
console.log('max concurrent', max, 'units', Object.keys(res.units).length, 'ints', JSON.stringify(Object.keys(res.ints)))
console.log(order.filter(l => l.startsWith('int')).join(' '))
console.log('position of int:ai triage1', order.indexOf('int:ai:triage1'), 'of', order.length, '; first agent impl', order.indexOf('impl:ag-0'), 'tel int', order.indexOf('int:telemetry:r1'))
