#!/usr/bin/env node
// Preserve SQL Server MERGE TOP selection/order without inventing a tie rule.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/merge-top.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/merge-top/merge-top.json')

const mixed = [
  ['mixed top zero', '0', '(1,11),(2,22),(4,44)', 0],
  ['mixed top one', '1', '(1,11),(2,22),(4,44)', 1],
  ['mixed top two', '2', '(1,11),(2,22),(4,44)', 2],
  ['mixed top ten', '10', '(1,11),(2,22),(4,44)', 4],
  ['mixed reverse top one', '1', '(4,44),(2,22),(1,11)', 1],
  ['mixed reverse top two', '2', '(4,44),(2,22),(1,11)', 2]
]
const nonqualifying = [
  ['nonqualifying only top one', '1', '(1,11)'],
  ['nonqualifying top one', '1', '(1,11),(4,44)'],
  ['nonqualifying top two', '2', '(1,11),(4,44)']
]
const duplicate = [
  ['duplicate top one', '1', false],
  ['duplicate top two', '2', true]
]
const forms = [
  ['expression top', '1+1', ''],
  ['percentage top', '50', ' PERCENT'],
  ['variable top', '@n', ''],
  ['negative top', '-1', ''],
  ['null top', 'NULL', ''],
  ['fractional top', '1.5', '']
]

function observed(run, name) {
  const item = run.find(entry => entry.name === name)
  assert(item, `missing ${name}`)
  return item.result
}
function resultRows(run, name) {
  const result = observed(run, name)
  assert.deepEqual(result.errors, [], name)
  assert.equal(result.sets.length, 1, name)
  return result.sets[0].rows
}
function affected(run, label) {
  const rows = resultRows(run, `${label} rowcount`)
  assert.equal(rows.length, 1, label)
  return rows[0][0]
}
function emitted(run, label) {
  return observed(run, `${label} merge`).sets.flatMap(set => set.rows)
}

// Validate possibilities, not arbitrary TOP selection or output row order.
function validate(run) {
  const identity = resultRows(run, 'server identity')
  assert.equal(identity.length, 1)
  assert.match(identity[0][0], /^17\./)
  const summary = { identity, mixed: {}, nonqualifying: {}, duplicate: {}, forms: {} }
  const choices = [
    ['UPDATE', 1, 1], ['DELETE', null, 2],
    ['DELETE', null, 3], ['INSERT', 4, null]
  ]
  for (const [label, , , count] of mixed) {
    const merge = observed(run, `${label} merge`)
    assert.deepEqual(merge.errors, [], label)
    assert.equal(merge.done.at(-1).rowCount, count, label)
    assert.equal(affected(run, label), count, label)
    const actions = emitted(run, label)
    assert.equal(actions.length, count, label)
    const selected = new Set()
    for (const action of actions) {
      const index = choices.findIndex(candidate => JSON.stringify(candidate) === JSON.stringify(action))
      assert(index >= 0, `${label}: unexpected output ${JSON.stringify(action)}`)
      assert(!selected.has(index), `${label}: repeated output ${JSON.stringify(action)}`)
      selected.add(index)
    }
    const final = [
      [1, selected.has(0) ? 11 : 10],
      ...(!selected.has(1) ? [[2, 20]] : []),
      ...(!selected.has(2) ? [[3, 30]] : []),
      ...(selected.has(3) ? [[4, 44]] : [])
    ]
    assert.deepEqual(resultRows(run, `${label} rows`), final, label)
    summary.mixed[label] = { count, outputColumns: merge.sets.map(set => set.columns) }
  }
  for (const [label, top] of nonqualifying) {
    const merge = observed(run, `${label} merge`)
    assert.deepEqual(merge.errors, [], label)
    const count = affected(run, label)
    assert([0, 1].includes(count), label)
    if (label === 'nonqualifying only top one') assert.equal(count, 0, label)
    if (top === '2') assert.equal(count, 1, label)
    assert.equal(merge.done.at(-1).rowCount, count, label)
    assert.deepEqual(emitted(run, label), count ? [['INSERT', 4, null]] : [], label)
    assert.deepEqual(resultRows(run, `${label} rows`), count ? [[1, 10], [4, 44]] : [[1, 10]], label)
    // TOP (1) may select the matched candidate whose arm does not qualify.
    summary.nonqualifying[label] = { outputColumns: merge.sets.map(set => set.columns) }
  }
  for (const [label, , expectError] of duplicate) {
    const merge = observed(run, `${label} merge`)
    if (expectError) {
      assert.equal(merge.errors[0]?.number, 8672, label)
      assert.equal(affected(run, label), 0, label)
      assert.deepEqual(resultRows(run, `${label} rows`), [[1, 10]], label)
    } else {
      assert.deepEqual(merge.errors, [], label)
      assert.equal(merge.done.at(-1).rowCount, 1, label)
      assert.equal(affected(run, label), 1, label)
      const final = resultRows(run, `${label} rows`)
      assert.equal(final.length, 1, label)
      assert.equal(final[0][0], 1, label)
      assert([11, 12].includes(final[0][1]), label)
      assert.deepEqual(emitted(run, label), [['UPDATE', 1, 1]], label)
    }
    summary.duplicate[label] = { error: merge.errors.map(error => [error.number, error.state, error.class]) }
  }
  for (const [label] of forms) {
    const merge = observed(run, `${label} merge`)
    const rows = resultRows(run, `${label} rows`)
    if (merge.errors.length) {
      assert.deepEqual(rows, [], `${label}: error must not write`)
    } else {
      const selected = emitted(run, label)
      assert(selected.every(row => row[0] === 'INSERT'), label)
      assert.equal(new Set(selected.map(row => row[1])).size, selected.length, label)
      assert.equal(merge.done.at(-1).rowCount, rows.length, label)
      assert.equal(affected(run, label), rows.length, label)
      assert(rows.length <= 3, label)
      assert(rows.every(([id, n]) => id >= 1 && id <= 3 && n === id * 10), label)
    }
    summary.forms[label] = {
      errors: merge.errors.map(error => [error.number, error.state, error.class]),
      count: merge.errors.length ? null : rows.length
    }
  }
  return summary
}

async function observe(connection, repeat) {
  const run = []
  async function record(name, sql, expectError = false) {
    const result = canonical(await capture(connection, sql))
    run.push({ name, sql, expectError, result })
    if (expectError !== null && Boolean(result.errors.length) !== expectError) {
      await writeFile(resolve(output, `../failed-${repeat}.json`), JSON.stringify(run, null, 2) + '\n')
      throw new Error(`${name}: ${expectError ? 'expected SQL error' : 'unexpected SQL error'}`)
    }
    return result
  }
  async function scenario(label, reset, sql, expectError = false) {
    await record(`${label} reset`, reset)
    await record(`${label} merge`, sql, expectError)
    await record(`${label} rowcount`, 'SELECT @@ROWCOUNT AS affected')
    const table = label.startsWith('mixed') ? 'merge_top_mix'
      : label.startsWith('nonqualifying') ? 'merge_top_nonqualifying'
        : label.startsWith('duplicate') ? 'merge_top_duplicate' : 'merge_top_forms'
    await record(`${label} rows`, `SELECT id,n FROM dbo.${table} ORDER BY id`)
  }

  await record('server identity', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version,CAST(DATABASEPROPERTYEX(DB_NAME(),'Collation') AS NVARCHAR(128)) AS database_collation")
  await record('create mixed', 'CREATE TABLE dbo.merge_top_mix(id INT NOT NULL PRIMARY KEY,n INT NOT NULL)')
  for (const [label, top, source] of mixed) {
    await scenario(label,
      'TRUNCATE TABLE dbo.merge_top_mix; INSERT dbo.merge_top_mix(id,n) VALUES (1,10),(2,20),(3,30)',
      `MERGE TOP (${top}) dbo.merge_top_mix AS t USING (VALUES ${source}) AS s(id,n) ON t.id=s.id
       WHEN MATCHED AND s.id=1 THEN UPDATE SET n=s.n
       WHEN MATCHED AND s.id=2 THEN DELETE
       WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n)
       WHEN NOT MATCHED BY SOURCE AND t.id=3 THEN DELETE
       OUTPUT $action AS action, inserted.id AS inserted_id, deleted.id AS deleted_id;`)
  }
  await record('create nonqualifying', 'CREATE TABLE dbo.merge_top_nonqualifying(id INT NOT NULL PRIMARY KEY,n INT NOT NULL)')
  for (const [label, top, source] of nonqualifying) {
    await scenario(label,
      'TRUNCATE TABLE dbo.merge_top_nonqualifying; INSERT dbo.merge_top_nonqualifying(id,n) VALUES (1,10)',
      `MERGE TOP (${top}) dbo.merge_top_nonqualifying AS t USING (VALUES ${source}) AS s(id,n) ON t.id=s.id
       WHEN MATCHED AND s.n>100 THEN UPDATE SET n=s.n
       WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n)
       OUTPUT $action AS action, inserted.id AS inserted_id, deleted.id AS deleted_id;`)
  }
  await record('create duplicate', 'CREATE TABLE dbo.merge_top_duplicate(id INT NOT NULL PRIMARY KEY,n INT NOT NULL)')
  for (const [label, top, expectError] of duplicate) {
    await scenario(label,
      'TRUNCATE TABLE dbo.merge_top_duplicate; INSERT dbo.merge_top_duplicate(id,n) VALUES (1,10)',
      `MERGE TOP (${top}) dbo.merge_top_duplicate AS t USING (VALUES (1,11),(1,12)) AS s(id,n) ON t.id=s.id
       WHEN MATCHED THEN UPDATE SET n=s.n
       OUTPUT $action AS action, inserted.id AS inserted_id, deleted.id AS deleted_id;`, expectError)
  }
  await record('create forms', 'CREATE TABLE dbo.merge_top_forms(id INT NOT NULL PRIMARY KEY,n INT NOT NULL)')
  for (const [label, expression, suffix] of forms) {
    const prefix = label === 'variable top' ? 'DECLARE @n INT=2; ' : ''
    await scenario(label, 'TRUNCATE TABLE dbo.merge_top_forms',
      `${prefix}MERGE TOP (${expression})${suffix} dbo.merge_top_forms AS t
       USING (VALUES (1,10),(2,20),(3,30)) AS s(id,n) ON t.id=s.id
       WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n)
       OUTPUT $action AS action, inserted.id AS inserted_id, deleted.id AS deleted_id;`, null)
  }
  try { validate(run) }
  catch (error) {
    await writeFile(resolve(output, `../failed-${repeat}.json`), JSON.stringify(run, null, 2) + '\n')
    throw error
  }
  return run
}

if (writeFixture) await refuseExistingFixture(fixture)
await mkdir(resolve(output, '..'), { recursive: true })
await withReferenceContainer(async (config, container) => {
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    runs.push(await isolatedReference({ ...config, options: { ...config.options, requestTimeout: 120000 } }, connection => observe(connection, repeat)))
  }
  const actual = { image: container.image, runs }
  await writeFile(output, JSON.stringify(actual) + '\n')
  const summaries = runs.map(validate)
  assertSameCapture(summaries[0], summaries[1], 'stable MERGE TOP invariants differ across fresh databases')
  let retained
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) {
    assert.equal(actual.image, retained.image)
    assertSameCapture(summaries, retained.runs.map(validate), 'stable MERGE TOP invariants differ from retained fixture')
  }
  if (writeFixture) {
    await writeNewFixture(fixture, actual)
  }
  console.log(`Captured ${runs[0].length} raw MERGE TOP observations in two fresh databases${retained ? ' and matched stable retained invariants' : ''}`)
})
