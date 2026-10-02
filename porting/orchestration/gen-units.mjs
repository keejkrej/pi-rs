// Writes one spec file per unit: $PI_PORT_HOME/units/<lang>/<id>.md (agents read these).
import fs from 'node:fs'; import path from 'node:path'
import { HOME, TS } from './config.mjs'
for (const lang of ['rust', 'go']) {
  const tf = path.join(HOME, `pi-port-tasks-${lang}.json`); if (!fs.existsSync(tf)) continue
  const j = JSON.parse(fs.readFileSync(tf, 'utf8'))
  const dir = path.join(HOME, 'units', lang); fs.mkdirSync(dir, { recursive: true })
  for (const t of j.tasks) {
    const md = `# Unit ${t.id} (${lang}) — TS package "${t.package}"
${t.title}

## TS source files (in ${TS})
${t.ts_files.map(f => '- ' + f).join('\n') || '(none)'}

## TS test files to port (phase B)
${t.test_files.map(f => '- ' + f).join('\n') || '(none)'}

## Tests NOT to port (e2e / real providers) with reasons
${t.skipped_tests.map(f => '- ' + (typeof f === 'string' ? f : JSON.stringify(f))).join('\n') || '(none)'}

## Owned target files (target  <=  TS source [kind])
${t.owned.map(f => '- ' + f).join('\n')}

## Target files of the TS modules this unit imports (other units own these)
${t.dep_targets.map(f => '- ' + f).join('\n') || '(none)'}

## Reader notes (from the design phase; cover both languages — apply the ${lang === 'rust' ? 'Rust' : 'Go'} parts)
${t.notes}
`
    fs.writeFileSync(path.join(dir, `${t.id}.md`), md)
  }
  console.log(lang, j.tasks.length, 'unit specs ->', dir)
}
