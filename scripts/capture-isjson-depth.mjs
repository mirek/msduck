#!/usr/bin/env node
// Owner-run SQL Server evidence for ISJSON nesting and UTF-16 syntax boundaries.
process.env.TZ = 'UTC'
if (new Date(0).getTimezoneOffset() !== 0) throw new Error('UTC client time zone is required')

import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { TYPES } from 'tedious'
import { canonical, capture } from './lib/compatibility.mjs'
import { assertSameCapture, isolatedReference, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'

const fixture = new URL('../reference/isjson-depth.json', import.meta.url)
const writeFixture = process.argv.includes('--write-fixture')
const positional = process.argv.slice(2).filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one scratch output path')
const output = resolve(positional[0] ?? 'artifacts/isjson-depth/capture.json')
async function canonicalPath(path) {
  const absolute = resolve(path)
  try { return await realpath(absolute) } catch (error) {
    if (error.code !== 'ENOENT') throw error
    const parent = dirname(absolute)
    return parent === absolute ? absolute : resolve(await canonicalPath(parent), basename(absolute))
  }
}
async function identity(path) {
  try { const value = await stat(path); return `${value.dev}:${value.ino}` }
  catch (error) { if (error.code === 'ENOENT') return null; throw error }
}
const fixturePath = fileURLToPath(fixture)
let outputIsSymlink = false
try { outputIsSymlink = (await lstat(output)).isSymbolicLink() }
catch (error) { if (error.code !== 'ENOENT') throw error }
if (outputIsSymlink ||
    await canonicalPath(output) === await canonicalPath(fixturePath) ||
    (await identity(fixturePath) !== null && await identity(output) === await identity(fixturePath))) {
  throw new Error('refusing to overwrite retained fixture ' + fixturePath)
}
if (writeFixture) await refuseExistingFixture(fixture)

const depths = [1, 2, 63, 64, 100, 127, 128, 129, 255, 256, 512, 1024, 2048, 4096]
const inputs = depths.flatMap(depth => ['array', 'object'].map(form => ({ name: `${form} depth ${depth}`, input: { form, depth } })))
inputs.push(...[128, 129, 130, 256].flatMap(depth => ['empty-array', 'empty-object'].map(form => ({ name: `${form} depth ${depth}`, input: { form, depth } }))))
inputs.push(
  { name: 'invalid before depth limit', input: { form: 'invalid-before-depth', depth: 129 } },
  { name: 'invalid after depth limit', input: { form: 'invalid-after-depth', depth: 129 } },
  { name: 'unclosed after depth limit', input: { form: 'unclosed-after-depth', depth: 129 } },
  { name: 'empty array at depth limit plus one', input: { form: 'empty-at-depth', depth: 129 } },
  { name: 'incomplete number after depth limit', input: { form: 'incomplete-after-depth', depth: 129 } },
  { name: 'only openings after depth limit', input: { form: 'openings-only', depth: 129 } },
  { name: 'trailing text after depth limit', input: { form: 'trailing-after-depth', depth: 129 } },
)
const unicode = [
  ['raw high surrogate', '{"v":"\ud800"}'],
  ['escaped high surrogate', '{"v":"\\ud800"}'],
  ['raw low surrogate', '{"v":"\udc00"}'],
  ['escaped low surrogate', '{"v":"\\udc00"}'],
  ['raw surrogate pair', '{"v":"\ud83e\udd86"}'],
  ['escaped surrogate pair', '{"v":"\\ud83e\\udd86"}'],
  ['raw NUL', '{"v":"\u0000"}'],
  ['escaped NUL', '{"v":"\\u0000"}'],
  ['raw line separator', '{"v":"\u2028"}'],
  ['raw BOM in string', '{"v":"\ufeff"}'],
  ['raw BOM before object', '\ufeff{}'],
  ['non-JSON whitespace before object', '\u00a0{}'],
]
inputs.push(...unicode.map(([name, value]) => ({ name, input: { form: 'units', units: value.split('').map(c => c.charCodeAt(0)) } })))
function valueFor(input) {
  if (input.form === 'array') return '['.repeat(input.depth) + '0' + ']'.repeat(input.depth)
  if (input.form === 'object') return '{"v":'.repeat(input.depth) + '0' + '}'.repeat(input.depth)
  if (input.form === 'invalid-before-depth') return '[x' + '['.repeat(input.depth - 1) + '0' + ']'.repeat(input.depth)
  if (input.form === 'invalid-after-depth') return '['.repeat(input.depth) + 'x' + ']'.repeat(input.depth)
  if (input.form === 'unclosed-after-depth') return '['.repeat(input.depth) + '0' + ']'.repeat(input.depth - 1)
  if (input.form === 'empty-array') return '['.repeat(input.depth) + ']'.repeat(input.depth)
  if (input.form === 'empty-object') return '{"v":'.repeat(input.depth - 1) + '{}' + '}'.repeat(input.depth - 1)
  if (input.form === 'empty-at-depth') return '['.repeat(input.depth) + ']'.repeat(input.depth)
  if (input.form === 'incomplete-after-depth') return '['.repeat(input.depth) + '1e' + ']'.repeat(input.depth)
  if (input.form === 'openings-only') return '['.repeat(input.depth)
  if (input.form === 'trailing-after-depth') return '['.repeat(input.depth) + '0' + ']'.repeat(input.depth) + 'x'
  if (input.form === 'units') return String.fromCharCode(...input.units)
  throw new Error(`unknown ISJSON input form ${input.form}`)
}
const programs = inputs.flatMap(({ name, input }) => [
  { name, mode: 'default', input, sql: 'SELECT ISJSON(@j) AS is_json' },
  { name, mode: 'value', input, sql: 'SELECT ISJSON(@j, VALUE) AS is_json' },
])
function rpc(connection, sql, value) {
  const transport = {
    on: (...args) => connection.on(...args),
    off: (...args) => connection.off(...args),
    execSqlBatch: request => {
      request.addParameter('j', TYPES.NVarChar, value, { length: Infinity })
      connection.execSql(request)
    },
  }
  return capture(transport, sql)
}
async function observe(connection) {
  const records = []
  for (const { name, mode, input, sql } of programs) {
    const result = canonical(await rpc(connection, sql, valueFor(input)))
    if (result.sets.length > 1 || result.sets.some(set => set.rows.length > 1) ||
        result.errors.length > 4 || result.done.length > 8) throw new Error(`unbounded capture: ${name}/${mode}`)
    records.push({ name, mode, input, sql, result })
  }
  return records
}

const containers = []
await withReferenceContainer(async (config, container) => {
  const runs = [await isolatedReference(config, observe), await isolatedReference(config, observe)]
  assertSameCapture(runs[0], runs[1], 'ISJSON depth observations differ across fresh databases')
  containers.push({ image: container.image, runs })
})
const actual = { containers }
const retained = writeFixture ? null : JSON.parse(await readFile(fixture, 'utf8'))
if (retained) assertSameCapture(actual, retained, 'ISJSON depth observations differ from retained fixture')
await mkdir(dirname(output), { recursive: true })
await writeFile(output, JSON.stringify(actual) + '\n')
if (writeFixture) await writeNewFixture(fixture, actual)
console.log(`Captured ${programs.length} ISJSON depth observations twice${retained ? ' and matched retained fixture' : ''}`)
