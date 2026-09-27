#!/usr/bin/env node
// Owner-run SQL Server evidence for decimal-to-Unicode character conversion.
process.env.TZ = 'UTC'
if (new Date(0).getTimezoneOffset() !== 0) throw new Error('UTC client time zone is required')

import { access, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises'
import { basename, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { canonical, capture } from './lib/compatibility.mjs'
import { assertSameCapture, isolatedReference } from './lib/reference.mjs'
import { withReferenceContainer } from './lib/reference-container.mjs'

const fixture = new URL('../reference/decimal-unicode.json', import.meta.url)
const fixturePath = fileURLToPath(fixture)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const oneDatabase = args.includes('--one-database')
const positional = args.filter(arg => !['--write-fixture', '--one-database'].includes(arg))
if (positional.length > 1) throw new Error('expected at most one scratch output path')
if (writeFixture && oneDatabase) throw new Error('a retained fixture requires four independent captures')
const output = resolve(positional[0] ?? 'artifacts/decimal-unicode-reference-v1/capture.json')

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
if (await canonicalPath(output) === await canonicalPath(fixturePath) ||
    (await identity(fixturePath) !== null && await identity(output) === await identity(fixturePath))) {
  throw new Error('refusing to write scratch output over retained fixture ' + fixturePath)
}
let fixtureExists = false
try { await access(fixture); fixtureExists = true }
catch (error) { if (error.code !== 'ENOENT') throw error }
if (writeFixture && fixtureExists) throw new Error('refusing to overwrite retained fixture ' + fixturePath)

const positive = `0.${'0'.repeat(37)}1`
const negative = `-${positive}`
const decimal = value => `CAST('${value}' AS DECIMAL(38,38))`
const programs = [
  ['zero and typed NULL', "SELECT CONVERT(NVARCHAR(40),CAST('0' AS DECIMAL(38,38))) AS zero_value,CONVERT(NCHAR(40),CAST('0' AS DECIMAL(38,38))) AS fixed_zero,CONVERT(NVARCHAR(40),CAST(NULL AS DECIMAL(38,38))) AS null_value"],
  ['positive exact width and padding', `SELECT CONVERT(NVARCHAR(40),${decimal(positive)}) AS bounded,CONVERT(NCHAR(40),${decimal(positive)}) AS fixed_exact,CONVERT(NCHAR(41),${decimal(positive)}) AS fixed_padded`],
  ['negative exact width', `SELECT CONVERT(NVARCHAR(41),${decimal(negative)}) AS bounded,CONVERT(NCHAR(41),${decimal(negative)}) AS fixed_exact`],
  ['TRY narrow widths', `SELECT TRY_CONVERT(NVARCHAR(39),${decimal(positive)}) AS unicode_short,TRY_CONVERT(NCHAR(39),${decimal(positive)}) AS fixed_short,TRY_CONVERT(NVARCHAR(40),${decimal(negative)}) AS negative_short`],
  ['NVARCHAR narrow overflow', `SELECT CONVERT(NVARCHAR(39),${decimal(positive)}) AS too_short`],
  ['NCHAR narrow overflow', `SELECT CONVERT(NCHAR(39),${decimal(positive)}) AS too_short`],
  ['decimal AVG', `SELECT CONVERT(NVARCHAR(40),AVG(d)) AS exact_value FROM (VALUES(${decimal(positive)}),(CAST('0' AS DECIMAL(38,38))),(CAST('0' AS DECIMAL(38,38)))) t(d)`],
  ['float and text controls', "SELECT CONVERT(NVARCHAR(40),CAST(0.5 AS FLOAT)) AS floating_value,CONVERT(NVARCHAR(40),N'.25') AS text_value"],
  ['empty descriptor', `SELECT CONVERT(NVARCHAR(40),${decimal(positive)}) AS empty_value WHERE 1=0`],
]

async function observe(connection) {
  const records = []
  for (const [name, sql] of programs) {
    const result = canonical(await capture(connection, sql))
    if (result.sets.length > 1 || result.sets.some(set => set.rows.length > 1) ||
        result.errors.length > 4 || result.done.length > 8) throw new Error(`unbounded capture: ${name}`)
    records.push({ name, sql, result })
  }
  return records
}

await mkdir(dirname(output), { recursive: true })
const containers = []
for (let containerIndex = 0; containerIndex < (oneDatabase ? 1 : 2); containerIndex++) {
  await withReferenceContainer(async (config, container) => {
    const runs = []
    for (let databaseIndex = 0; databaseIndex < (oneDatabase ? 1 : 2); databaseIndex++) {
      runs.push(await isolatedReference(config, observe))
    }
    if (!oneDatabase) assertSameCapture(runs[0], runs[1], 'decimal Unicode observations differ across fresh databases')
    containers.push({ image: container.image, runs })
  })
}
if (!oneDatabase) assertSameCapture(containers[0].runs[0], containers[1].runs[0], 'decimal Unicode observations differ across containers')
const actual = { containers }
const retained = fixtureExists ? JSON.parse(await readFile(fixture, 'utf8')) : null
await writeFile(output, JSON.stringify(actual) + '\n')
if (retained && !oneDatabase) assertSameCapture(actual, retained, 'decimal Unicode observations differ from retained fixture')
if (writeFixture) await writeFile(fixture, JSON.stringify(actual) + '\n', { flag: 'wx' })
console.log(`Captured ${programs.length} decimal Unicode observations in ${oneDatabase ? 'one diagnostic database' : 'four fresh databases across two containers'}${retained && !oneDatabase ? ' and matched retained fixture' : ''}`)
