#!/usr/bin/env node
// Owner-run SQL Server evidence for ISJSON input binding and diagnostics.
process.env.TZ = 'UTC'
if (new Date(0).getTimezoneOffset() !== 0) throw new Error('UTC client time zone is required')

import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { canonical, capture } from './lib/compatibility.mjs'
import { assertSameCapture, isolatedReference, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'

const fixture = new URL('../reference/isjson-coercion.json', import.meta.url)
const writeFixture = process.argv.includes('--write-fixture')
const positional = process.argv.slice(2).filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one scratch output path')
const output = resolve(positional[0] ?? 'artifacts/isjson-coercion/capture.json')
if (writeFixture) await refuseExistingFixture(fixture)

const inputs = [
  ['unicode object', "N'{}'"],
  ['ansi object', "'{}'"],
  ['varchar object', "CAST('{}' AS VARCHAR(20))"],
  ['nvarchar object', "CAST(N'{}' AS NVARCHAR(20))"],
  ['integer one', '1'],
  ['integer zero', '0'],
  ['integer negative', '-1'],
  ['bit one', 'CAST(1 AS BIT)'],
  ['decimal', 'CAST(1.25 AS DECIMAL(5,2))'],
  ['float', 'CAST(1.25 AS FLOAT)'],
  ['date', "CAST('2025-01-02' AS DATE)"],
  ['datetime2', "CAST('2025-01-02T03:04:05' AS DATETIME2)"],
  ['uniqueidentifier', "CAST('00112233-4455-6677-8899-aabbccddeeff' AS UNIQUEIDENTIFIER)"],
  ['binary object', 'CAST(0x7B7D AS VARBINARY(2))'],
  ['binary scalar', 'CAST(0x31 AS VARBINARY(1))'],
  ['binary then text', 'CAST(CAST(0x7B7D AS VARBINARY(2)) AS VARCHAR(2))'],
  ['typed integer NULL', 'CAST(NULL AS INT)'],
  ['typed binary NULL', 'CAST(NULL AS VARBINARY(2))'],
  ['typed unicode NULL', 'CAST(NULL AS NVARCHAR(20))'],
  ['untyped NULL', 'NULL'],
  ['xml', "CAST('<r/>' AS XML)"],
  ['text', "CAST('{}' AS TEXT)"],
]
const programs = inputs.flatMap(([name, input]) => [
  { name, mode: 'default', sql: `SELECT ISJSON(${input}) AS is_json` },
  { name, mode: 'value', sql: `SELECT ISJSON(${input}, VALUE) AS is_json` },
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
  assertSameCapture(runs[0], runs[1], 'ISJSON observations differ across fresh databases')
  containers.push({ image: container.image, runs })
})
const actual = { containers }
const retained = writeFixture ? null : JSON.parse(await readFile(fixture, 'utf8'))
if (retained) assertSameCapture(actual, retained, 'ISJSON observations differ from retained fixture')
await mkdir(dirname(output), { recursive: true })
await writeFile(output, JSON.stringify(actual) + '\n')
if (writeFixture) await writeNewFixture(fixture, actual)
console.log(`Captured ${programs.length} ISJSON observations twice${retained ? ' and matched retained fixture' : ''}`)
