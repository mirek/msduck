#!/usr/bin/env node
// Capture raw SQL Server QUOTENAME values and TDS descriptors in fresh databases.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/quotename.json', import.meta.url)
const output = resolve(process.argv[2] ?? 'artifacts/compatibility/quotename/quotename.json')
const writeFixture = process.argv.includes('--write-fixture')
const literal = value => `N'${value.replaceAll("'", "''")}'`
function utf16Hex(value) {
  let hex = ''
  for (let index = 0; index < value.length; index++) {
    const unit = value.charCodeAt(index)
    hex += (unit & 0xff).toString(16).padStart(2, '0')
    hex += (unit >> 8).toString(16).padStart(2, '0')
  }
  return hex
}
const expression = (source, delimiter) =>
  `QUOTENAME(${source}${delimiter === undefined ? '' : `,${delimiter}`})`
const cases = []
function add(name, source, delimiter) {
  const quoted = expression(source, delimiter)
  const delimiterBytes = delimiter === undefined
    ? 'CAST(NULL AS VARBINARY(MAX))'
    : `CONVERT(VARBINARY(MAX),${delimiter})`
  cases.push({
    name,
    defaultDelimiter: delimiter === undefined,
    query: `SELECT ${quoted} AS quoted,CONVERT(VARBINARY(MAX),${quoted}) AS utf16,` +
      `CONVERT(VARBINARY(MAX),${source}) AS source_utf16,${delimiterBytes} AS delimiter_utf16`
  })
}

add('default brackets', literal('abc[]def'))
add('empty input', literal(''))
add('embedded nul', "N'a'+NCHAR(0)+N'b'")
add('leading nul', "NCHAR(0)+N'a'")
add('trailing nul', "N'a'+NCHAR(0)")
add('null input', 'CAST(NULL AS NVARCHAR(128))')
add('null delimiter', literal('a'), 'CAST(NULL AS NCHAR(1))')
for (const delimiter of ["'", '[', ']', '"', '(', ')', '<', '>', '{', '}', '`']) {
  add(`delimiter ${delimiter}`, literal(`a${delimiter}b`), literal(delimiter))
}
for (const delimiter of ['', '[]', ']x', '[x', 'x[', 'ab', ' ', '😀']) {
  add(`boundary delimiter ${JSON.stringify(delimiter)}`, literal('a]b'), literal(delimiter))
}
add('nul delimiter', literal('a]b'), 'NCHAR(0)')
add('nul delimiter empty input', literal(''), 'NCHAR(0)')
add('nul delimiter apostrophe', literal("a'b"), 'NCHAR(0)')
add('nul delimiter maximum input', "REPLICATE(N']',128)", 'NCHAR(0)')
add('nul delimiter overlong input', "REPLICATE(N'a',129)", 'NCHAR(0)')
add('nul delimiter first of two', literal('a]b'), "NCHAR(0)+N'['")
add('nul delimiter only nul input', 'NCHAR(0)', 'NCHAR(0)')
add('nul delimiter embedded nul input', "N'a'+NCHAR(0)+N'b'", 'NCHAR(0)')
add('nul input and delimiter', "N'a'+NCHAR(0)", 'NCHAR(0)')
add('only nul input', 'NCHAR(0)')
add('tab delimiter', literal('a]b'), 'NCHAR(9)')
add('length 128', "REPLICATE(N'a',128)")
add('length 129', "REPLICATE(N'a',129)")
add('escaped closing length 128', "REPLICATE(N']',128)")
add('paired supplementary 64', 'REPLICATE(CONVERT(NVARCHAR(2),0x3DD800DE),64)')
add('paired supplementary 65', 'REPLICATE(CONVERT(NVARCHAR(2),0x3DD800DE),65)')
add('isolated high surrogate', 'CONVERT(NVARCHAR(1),0x3DD8)')
add('isolated low surrogate', 'CONVERT(NVARCHAR(1),0x00DE)')
add('column input', 'v')
cases.at(-1).query += " FROM (VALUES(N'a]b')) t(v)"
add('empty column input', 'v')
cases.at(-1).query += " FROM (VALUES(N'a]b')) t(v) WHERE 1=0"
add('empty projection', literal('a]b'))
cases.at(-1).query += ' WHERE 1=0'

function validate(run) {
  assert.equal(run.length, cases.length)
  for (const [index, entry] of run.entries()) {
    assert.equal(entry.name, cases[index].name)
    assert.deepEqual(entry.result.errors, [], entry.name)
    assert.equal(entry.result.sets.length, 1, entry.name)
    assert.equal(entry.result.sets[0].columns.length, 4, entry.name)
    assert.equal(entry.result.sets[0].rows.length, entry.name.startsWith('empty ') && entry.name !== 'empty input' ? 0 : 1, entry.name)
    const row = entry.result.sets[0].rows[0]
    if (row?.[0] !== null && row) {
      assert.equal(row[0].kind, 'utf16', entry.name)
      assert.equal(row[1].kind, 'binary', entry.name)
      // The raw UTF-16 bytes, rather than a client-decoded string alone, are
      // the authority for isolated surrogate and embedded-NUL cases.
      assert.equal(row[0].value, row[1].value, entry.name)
      assert.equal(row[1].value.length % 4, 0, entry.name)
    }
  }
}

if (writeFixture) await refuseExistingFixture(fixture)
await mkdir(resolve(output, '..'), { recursive: true })
await withReferenceContainer(async (config, container) => {
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    runs.push(await isolatedReference(config, async connection => {
      const run = []
      for (const item of cases) {
        const result = canonical(await capture(connection, item.query))
        for (const set of result.sets) for (const row of set.rows) {
          if (row[0] !== null) {
            assert.equal(typeof row[0], 'string')
            row[0] = { kind: 'utf16', value: utf16Hex(row[0]) }
          }
        }
        run.push({ name: item.name, defaultDelimiter: item.defaultDelimiter, query: item.query, result })
      }
      validate(run)
      return run
    }))
  }
  assertSameCapture(runs[0], runs[1], 'fresh SQL Server databases differ')
  const actual = { image: container.image, runs }
  let retained
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) assertSameCapture(actual, retained, 'fresh capture differs from retained fixture')
  await writeFile(output, JSON.stringify(actual) + '\n')
  if (writeFixture) {
    await writeNewFixture(fixture, actual)
  }
  console.log(`Captured ${cases.length} QUOTENAME cases in two fresh databases${retained ? ' and matched the retained fixture' : ''}`)
})
