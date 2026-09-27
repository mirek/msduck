#!/usr/bin/env node
// Owner-run SQL Server evidence for additional ISJSON source-type binding and diagnostics.
process.env.TZ = 'UTC'
if (new Date(0).getTimezoneOffset() !== 0) throw new Error('UTC client time zone is required')

import { lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { canonical, capture } from './lib/compatibility.mjs'
import { assertSameCapture, isolatedReference, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'

const fixture = new URL('../reference/isjson-source-types.json', import.meta.url)
const writeFixture = process.argv.includes('--write-fixture')
const positional = process.argv.slice(2).filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one scratch output path')
const output = resolve(positional[0] ?? 'artifacts/isjson-source-types/capture.json')
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

const sources = [
  ['tinyint', 'CAST(1 AS TINYINT)'],
  ['smallint', 'CAST(1 AS SMALLINT)'],
  ['bigint', 'CAST(1 AS BIGINT)'],
  ['real', 'CAST(1.25 AS REAL)'],
  ['money', "CAST('1.25' AS MONEY)"],
  ['smallmoney', "CAST('1.25' AS SMALLMONEY)"],
  ['time', "CAST('12:34:56.1234567' AS TIME(7))"],
  ['datetime', "CAST('2025-01-02T03:04:05' AS DATETIME)"],
  ['smalldatetime', "CAST('2025-01-02T03:04:00' AS SMALLDATETIME)"],
  ['datetimeoffset', "CAST('2025-01-02T03:04:05+02:00' AS DATETIMEOFFSET(7))"],
]
const inputs = sources.flatMap(([name, value]) => [
  { name: `${name} value`, input: value },
  { name: `${name} typed NULL`, input: `CAST(NULL AS ${name.toUpperCase()})` },
  { name: `${name} source column`, input: 'v', from: `(VALUES (${value})) AS source(v)` },
])
inputs.push(
  { name: 'varchar source column', input: 'v', from: "(VALUES (CAST('{}' AS VARCHAR(20)))) AS source(v)" },
  { name: 'nvarchar source column', input: 'v', from: "(VALUES (CAST(N'{}' AS NVARCHAR(20)))) AS source(v)" },
  { name: 'varchar NULL source column', input: 'v', from: '(VALUES (CAST(NULL AS VARCHAR(20)))) AS source(v)' },
  { name: 'nvarchar NULL source column', input: 'v', from: '(VALUES (CAST(NULL AS NVARCHAR(20)))) AS source(v)' },
)
const programs = inputs.flatMap(({ name, input, from }) => [
  { name, mode: 'default', sql: `SELECT ISJSON(${input}) AS is_json${from ? ` FROM ${from}` : ''}` },
  { name, mode: 'value', sql: `SELECT ISJSON(${input}, VALUE) AS is_json${from ? ` FROM ${from}` : ''}` },
])

async function observe(connection) {
  const records = []
  for (const { name, mode, sql } of programs) {
    const result = canonical(await capture(connection, sql))
    if (result.sets.length > 1 || result.sets.some(set => set.rows.length > 1) ||
        result.errors.length > 4 || result.done.length > 8) throw new Error(`unbounded capture: ${name}/${mode}`)
    records.push({ name, mode, sql, result })
  }
  return records
}

const containers = []
await withReferenceContainer(async (config, container) => {
  const runs = [await isolatedReference(config, observe), await isolatedReference(config, observe)]
  assertSameCapture(runs[0], runs[1], 'ISJSON source-type observations differ across fresh databases')
  containers.push({ image: container.image, runs })
})
const actual = { containers }
const retained = writeFixture ? null : JSON.parse(await readFile(fixture, 'utf8'))
if (retained) assertSameCapture(actual, retained, 'ISJSON source-type observations differ from retained fixture')
await mkdir(dirname(output), { recursive: true })
await writeFile(output, JSON.stringify(actual) + '\n')
if (writeFixture) await writeNewFixture(fixture, actual)
console.log(`Captured ${programs.length} ISJSON source-type observations twice${retained ? ' and matched retained fixture' : ''}`)
