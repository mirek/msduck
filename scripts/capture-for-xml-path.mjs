#!/usr/bin/env node
// Capture SQL Server FOR XML PATH results and TDS tokens in two fresh databases.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/for-xml-path.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/for-xml-path/for-xml-path.json')
const cases = [
  { name: 'server identity', query: "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version,CAST(DATABASEPROPERTYEX(DB_NAME(),'Collation') AS NVARCHAR(128)) AS database_collation" },
  { name: 'ordinary row', query: "SELECT 1 AS value FOR XML PATH('row')" },
  { name: 'empty row name', query: "SELECT 1 AS value FOR XML PATH('')" },
  { name: 'default row name', query: 'SELECT 1 AS value FOR XML PATH' },
  { name: 'attribute and escaped element', query: "SELECT 7 AS [@id], N'A&B <C> \"quoted\"' AS [name] FOR XML PATH('item')" },
  { name: 'unnamed text', query: "SELECT N'A&B <C>' FOR XML PATH('row')" },
  { name: 'text node alias', query: "SELECT N'A&B <C>' AS [text()] FOR XML PATH('row')" },
  { name: 'nested element alias', query: "SELECT 1 AS [meta/id], N'X' AS [meta/name] FOR XML PATH('row')" },
  { name: 'ordered rows', query: "SELECT v AS [@id], label AS [name] FROM (VALUES (2,N'two'),(1,N'one')) AS t(v,label) ORDER BY v FOR XML PATH('item')" },
  { name: 'null element omitted', query: "SELECT CAST(NULL AS NVARCHAR(8)) AS [value] FOR XML PATH('row')" },
  { name: 'null attribute omitted', query: "SELECT CAST(NULL AS NVARCHAR(8)) AS [@code] FOR XML PATH('row')" },
  { name: 'null element xinil', query: "SELECT CAST(NULL AS NVARCHAR(8)) AS [value] FOR XML PATH('row'), ELEMENTS XSINIL" },
  { name: 'zero source rows', query: "SELECT 1 AS [value] WHERE 1=0 FOR XML PATH('row')" },
  { name: 'root wrapper', query: "SELECT 1 AS [value] FOR XML PATH('row'), ROOT('root')" },
  { name: 'xml type', query: "SELECT 1 AS [value] FOR XML PATH('row'), TYPE" },
  { name: 'xml type with root', query: "SELECT 1 AS [value] FOR XML PATH('row'), TYPE, ROOT('root')" },
  { name: 'typed nested xml', query: "SELECT (SELECT 1 AS [value] FOR XML PATH('item'), TYPE) AS payload" },
  { name: 'binary base64', query: "SELECT CONVERT(VARBINARY(3),0x00FF3C) AS [data] FOR XML PATH('row'), BINARY BASE64" },
  { name: 'attribute after element', query: "SELECT 1 AS [@id], 2 AS [value], 3 AS [@late] FOR XML PATH('row')", expectError: true },
  { name: 'duplicate root directive', query: "SELECT 1 AS [value] FOR XML PATH('row'), ROOT('a'), ROOT('b')", expectError: true },
  { name: 'duplicate type directive', query: "SELECT 1 AS [value] FOR XML PATH('row'), TYPE, TYPE", expectError: true }
]

function validate(run) {
  assert.equal(run.length, cases.length)
  for (const [index, entry] of run.entries()) {
    const scenario = cases[index]
    assert.equal(entry.name, scenario.name)
    assert.equal(entry.query, scenario.query)
    assert.equal(entry.expectError, Boolean(scenario.expectError))
    assert.equal(Boolean(entry.result.errors.length), Boolean(scenario.expectError), scenario.name)
    if (!scenario.expectError) {
      assert.equal(entry.result.sets.length, 1, scenario.name)
      assert.equal(entry.result.sets[0].columns.length, scenario.name === 'server identity' ? 2 : 1, scenario.name)
    }
  }
}

await mkdir(resolve(output, '..'), { recursive: true })
await withReferenceContainer(async (config, container) => {
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    runs.push(await isolatedReference(config, async connection => {
      const run = []
      for (const scenario of cases) {
        const result = canonical(await capture(connection, scenario.query))
        run.push({ name: scenario.name, query: scenario.query, expectError: Boolean(scenario.expectError), result })
      }
      validate(run)
      return run
    }))
  }
  assert.deepEqual(runs[0], runs[1], 'fresh SQL Server databases differ')
  const actual = { image: container.image, runs }
  let retained
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) assert.deepEqual(actual, retained, 'fresh capture differs from retained fixture')
  await writeFile(output, JSON.stringify(actual) + '\n')
  if (writeFixture) {
    assert.equal(retained, undefined, 'refusing to overwrite retained fixture')
    await writeFile(fixture, JSON.stringify(actual) + '\n')
  }
  console.log(`Captured ${cases.length} FOR XML PATH cases in two fresh databases${retained ? ' and matched the retained fixture' : ''}`)
})
