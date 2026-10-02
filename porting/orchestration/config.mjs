// Shared configuration for the pi port orchestration. Override any path with env vars.
import path from 'node:path'
import { fileURLToPath } from 'node:url'
const here = path.dirname(fileURLToPath(import.meta.url))
export const HOME = process.env.PI_PORT_HOME || here                      // state dir: tasks, units, slots, wt clones, generated scripts
export const BIN = process.env.PI_PORT_BIN || path.join(HOME, 'bin')
export const TS = process.env.PI_PORT_TS || '/Users/jack/.t3/worktrees/pi/t3code-ef43beb3' // pi TS checkout at 7fbbd5f4a (v1.0.0-2)
export const MAIN = {
  rust: process.env.PI_PORT_RUST || '/Users/jack/workspace/pi-rs',
  go: process.env.PI_PORT_GO || '/Users/jack/workspace/pi-go',
}
export const toSlash = p => p.split(path.sep).join('/')
