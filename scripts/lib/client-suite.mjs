// One explicit client inventory; CI adds its independently registered coverage.
import {readdir} from 'node:fs/promises'
import {resolve} from 'node:path'
export const npmFiles = Object.freeze([
  'tests/tedious.test.mjs', 'tests/compatibility.test.mjs', 'tests/tls.test.mjs',
  'tests/datepart_numeric.test.mjs', 'tests/reference-compare.test.mjs',
  'tests/reference-prepared.test.mjs',
])
export const ciExtras = Object.freeze([
  'tests/session_reset.test.mjs', 'tests/databases.test.mjs',
  'tests/login_database_error.test.mjs', 'tests/alter_database_sessions.test.mjs',
  'tests/tedious_compat_gaps.test.mjs',
])
// These historical diagnostic replays retain intentional opt-in skips.
export const diagnosticFiles = Object.freeze(['tests/aggregate_diagnostics.test.mjs', 'tests/character_extrema.test.mjs'])
export async function suiteFiles(suite = 'npm') {
  if (suite === 'npm') return [...npmFiles]
  if (suite === 'ci-replays') return [...diagnosticFiles]
  if (suite !== 'ci') throw Error('suite must be npm, ci or ci-replays')
  const compat = (await readdir('tests/compat')).filter(x => x.endsWith('.test.mjs')).sort()
  return [...npmFiles, ...ciExtras, ...compat.map(x => `tests/compat/${x}`)]
}
export function clientJobs(value) {
  if (value === undefined) return 1
  if (!/^(?:[1-9]|1[0-6])$/.test(value)) throw Error('MSDUCK_CLIENT_JOBS must be an integer from 1 to 16')
  return Number(value)
}
export function strictResult(result, events) {
  const problems = [...result.problems]
  if (result.passed !== result.expected || result.skipped || result.todo) problems.push('Full suite requires every assigned test to pass without skip or TODO')
  if (events.some(e => e.type === 'test:fail' || e.skip || e.todo)) problems.push('Full suite contains a failed, cancelled, skipped or TODO result')
  return {...result, problems, ok: result.ok && !problems.length}
}
// Hash transitive repository source inputs too, not only selected test files.
export async function sourceFiles() {
  const files = []
  async function walk(path) {
    for (const entry of await readdir(path, {withFileTypes: true})) {
      const child = `${path}/${entry.name}`
      if (entry.name === 'target' || entry.name === 'node_modules' || entry.name === '.tmp') continue
      if (entry.isDirectory()) await walk(child)
      else if (entry.isFile()) files.push(resolve(child))
    }
  }
  for (const path of ['src', 'crates', 'scripts', 'tests', 'vendor', 'reference']) await walk(path)
  return [...files, ...['Cargo.toml', 'Cargo.lock', 'package.json', 'package-lock.json'].map(x => resolve(x))].sort()
}
