#!/usr/bin/env node
// First-party SQL Server MERGE TOP PERCENT boundary evidence.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/merge-top-percent.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/merge-top-percent/merge-top-percent.json')

const cases = [
  ['zero', '0', 7], ['one', '1', 7], ['quarter', '25', 7],
  ['third decimal', '33.333', 7], ['half', '50', 7],
  ['three quarters', '75', 7], ['full', '100', 7],
  ['above full', '101', 7], ['negative', '-1', 7],
  ['null', 'NULL', 7], ['fractional', '1.5', 7],
  ['decimal half', '50.0', 7], ['near full', '99.999', 7],
  ['above full decimal', '100.001', 7],
  ['int variable', '@p', 7, 'DECLARE @p INT=25; '],
  ['decimal variable', '@p', 7, 'DECLARE @p DECIMAL(6,3)=33.333; '],
  ['empty quarter', '25', 0], ['one quarter', '25', 1],
  ['two quarter', '25', 2], ['three quarter', '25', 3],
  ['four quarter', '25', 4], ['three third', '33.333', 3],
  ['four third', '33.333', 4], ['three half', '50', 3],
  ['four half', '50', 4], ['three three quarters', '75', 3],
  ['four three quarters', '75', 4]
]

function get(run, label, part) {
  const item = run.find(item => item.name === `${label} ${part}`)
  assert(item, `missing ${label} ${part}`)
  return item.result
}
function summary(run) {
  const identity = get(run, 'server', 'identity')
  assert.match(identity.sets[0].rows[0][0], /^17\./)
  const out = []
  for (const [label, , eligible] of cases) {
    const result = get(run, label, 'merge')
    const count = get(run, label, 'rowcount').sets[0].rows[0][0]
    const rows = get(run, label, 'rows').sets[0].rows
    const errors = result.errors.map(({ number, state, class: severity, message }) =>
      ({ number, state, class: severity, message }))
    if (errors.length) {
      assert.equal(count, 0, label)
      assert.deepEqual(rows, [], label)
      assert.equal(result.done.at(-1).rowCount, null, label)
      out.push({ label, eligible, errors, count: null })
      continue
    }
    assert.equal(result.sets.length, 1, label)
    const emitted = result.sets[0].rows
    assert.equal(result.done.at(-1).rowCount, count, label)
    assert.equal(rows.length, count, label)
    assert.equal(emitted.length, count, label)
    assert(count <= eligible, label)
    assert(emitted.every(([action, id]) => action === 'INSERT' && id >= 1 && id <= eligible), label)
    assert.equal(new Set(emitted.map(row => row[1])).size, count, label)
    assert.deepEqual(emitted.map(row => row[1]).sort((a, b) => a - b), rows.map(row => row[0]), label)
    out.push({ label, eligible, errors, count })
  }
  return out
}

async function observe(connection) {
  const run = []
  async function record(name, sql) {
    const result = canonical(await capture(connection, sql))
    run.push({ name, sql, result })
  }
  await record('server identity', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version,CAST(DATABASEPROPERTYEX(DB_NAME(),'Collation') AS NVARCHAR(128)) AS database_collation")
  await record('create target', 'CREATE TABLE dbo.merge_percent(id INT NOT NULL PRIMARY KEY)')
  const values = Array.from({ length: 7 }, (_, index) => `(${index + 1})`).join(',')
  for (const [label, expression, eligible, prefix = ''] of cases) {
    await record(`${label} reset`, 'TRUNCATE TABLE dbo.merge_percent')
    await record(`${label} merge`, `${prefix}MERGE TOP (${expression}) PERCENT dbo.merge_percent AS t
       USING (SELECT id FROM (VALUES ${values}) AS v(id) WHERE id<=${eligible}) AS s ON t.id=s.id
       WHEN NOT MATCHED BY TARGET THEN INSERT (id) VALUES (s.id)
       OUTPUT $action AS action, inserted.id AS inserted_id;`)
    await record(`${label} rowcount`, 'SELECT @@ROWCOUNT AS affected')
    await record(`${label} rows`, 'SELECT id FROM dbo.merge_percent ORDER BY id')
  }
  summary(run)
  return run
}

await mkdir(resolve(output, '..'), { recursive: true })
await withReferenceContainer(async (config, container) => {
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    runs.push(await isolatedReference({ ...config, options: { ...config.options, requestTimeout: 120000 } }, observe))
  }
  const actual = { image: container.image, runs }
  const summaries = runs.map(summary)
  assert.deepEqual(summaries[0], summaries[1], 'percentage counts and diagnostics differ across fresh databases')
  await writeFile(output, JSON.stringify(actual) + '\n')
  let retained
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) {
    assert.equal(actual.image, retained.image)
    assert.deepEqual(summaries, retained.runs.map(summary), 'percentage invariants differ from retained fixture')
  }
  if (writeFixture) {
    assert.equal(retained, undefined, 'refusing to overwrite retained fixture')
    await writeFile(fixture, JSON.stringify(actual) + '\n')
  }
  console.log(`Captured ${runs[0].length} raw MERGE TOP PERCENT observations in two fresh databases${retained ? ' and matched retained invariants' : ''}`)
})
