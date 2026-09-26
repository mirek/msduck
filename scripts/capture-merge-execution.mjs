#!/usr/bin/env node
// Capture SQL Server MERGE execution, OUTPUT, errors and completion tokens.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

const fixture = new URL('../reference/merge-execution.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const positional = args.filter(arg => arg !== '--write-fixture')
if (positional.length > 1) throw new Error('expected at most one output path')
const output = resolve(positional[0] ?? 'artifacts/compatibility/merge-execution/merge-execution.json')

function bindGeneratedDatabaseNames(run) {
  return run.map(entry => {
    const copy = structuredClone(entry)
    for (const error of copy.result.errors) {
      if (typeof error.message === 'string') {
        error.message = error.message.replaceAll(/msduck_audit_[0-9a-f]{32}/g, '<fresh-database>')
      }
    }
    for (const info of copy.result.info) {
      if (typeof info.message === 'string') {
        info.message = info.message.replaceAll(/msduck_audit_[0-9a-f]{32}/g, '<fresh-database>')
      }
    }
    return copy
  })
}

function observation(run, name) {
  const entry = run.find(item => item.name === name)
  assert(entry, `missing ${name}`)
  return entry.result
}
function rows(run, name) {
  const result = observation(run, name)
  assert.deepEqual(result.errors, [], name)
  assert.equal(result.sets.length, 1, name)
  return result.sets[0].rows
}
function validate(run) {
  assert.equal(run.length, 46)
  assert.deepEqual(rows(run, 'basic final rows'), [[1, 11], [2, 20], [4, 40]])
  assert.deepEqual(rows(run, 'mixed rowcount'), [[4]])
  assert.equal(observation(run, 'mixed actions').done.at(-1).rowCount, 4)
  assert.deepEqual(rows(run, 'mixed output rows'), [
    ['DELETE', null, 2], ['DELETE', null, 3], ['INSERT', 4, null], ['UPDATE', 1, 1]
  ])
  assert.deepEqual(rows(run, 'mixed final rows'), [[1, 11], [4, 44]])
  assert.deepEqual(rows(run, 'cte rowcount'), [[1]])
  assert.equal(observation(run, 'duplicate source error').errors[0].number, 8672)
  assert.deepEqual(rows(run, 'duplicate final rows'), [[1, 10]])
  assert.equal(observation(run, 'constraint failure').errors[0].number, 547)
  assert.deepEqual(rows(run, 'constraint final rows'), [[1, 10]])
  assert.deepEqual(rows(run, 'rollback pending rows'), [[1, 11], [2, 20]])
  assert.deepEqual(rows(run, 'rollback final rows'), [[1, 10]])
  assert.deepEqual(rows(run, 'top zero rows'), [])
  assert.deepEqual(rows(run, 'top one rows'), [[1, 10]])
  assert.equal(observation(run, 'negative top error').errors[0].number, 127)
  assert.equal(observation(run, 'unconditional matched before conditional error').errors[0].number, 5324)
  assert.equal(observation(run, 'repeated matched update error').errors[0].number, 10714)
  assert.deepEqual(rows(run, 'invalid arms final rows'), [[1, 10]])
}

async function observe(connection, repeat) {
  const run = []
  async function record(name, sql, expectError = false) {
    const result = canonical(await capture(connection, sql))
    run.push({ name, sql, expectError, result })
    if (Boolean(result.errors.length) !== expectError) {
      await writeFile(resolve(output, `../failed-${repeat}.json`), JSON.stringify(run, null, 2) + '\n')
      throw new Error(`${name}: ${expectError ? 'expected SQL error' : 'unexpected SQL error'}`)
    }
    return result
  }

  await record('server identity', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version,CAST(DATABASEPROPERTYEX(DB_NAME(),'Collation') AS NVARCHAR(128)) AS database_collation")
  await record('basic create', 'CREATE TABLE dbo.merge_target(id INT NOT NULL PRIMARY KEY,n INT NOT NULL)')
  await record('basic seed', 'INSERT dbo.merge_target(id,n) VALUES (1,10),(2,20),(3,30)')
  await record('basic initial rows', 'SELECT id,n FROM dbo.merge_target ORDER BY id')
  await record('matched update', 'MERGE dbo.merge_target AS t USING (VALUES (1,11)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n OUTPUT $action AS action, inserted.id AS inserted_id, deleted.n AS prior_n, inserted.n AS current_n;')
  await record('matched rowcount', 'SELECT @@ROWCOUNT AS affected')
  await record('unmatched insert', 'MERGE dbo.merge_target AS t USING (VALUES (4,40)) AS s(id,n) ON t.id=s.id WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n) OUTPUT $action AS action, inserted.id AS inserted_id, inserted.n AS current_n;')
  await record('unmatched rowcount', 'SELECT @@ROWCOUNT AS affected')
  await record('by source delete', 'MERGE dbo.merge_target AS t USING (VALUES (1),(2),(4)) AS s(id) ON t.id=s.id WHEN NOT MATCHED BY SOURCE AND t.id=3 THEN DELETE OUTPUT $action AS action, deleted.id AS deleted_id, deleted.n AS prior_n;')
  await record('by source rowcount', 'SELECT @@ROWCOUNT AS affected')
  await record('basic final rows', 'SELECT id,n FROM dbo.merge_target ORDER BY id')

  await record('mixed create', 'CREATE TABLE dbo.merge_mix(id INT NOT NULL PRIMARY KEY,n INT NOT NULL)')
  await record('mixed seed', 'INSERT dbo.merge_mix(id,n) VALUES (1,10),(2,20),(3,30)')
  await record('mixed output create', 'CREATE TABLE dbo.merge_output([action] NVARCHAR(10) NOT NULL,inserted_id INT NULL,deleted_id INT NULL)')
  await record('mixed actions', `MERGE dbo.merge_mix AS t
    USING (VALUES (1,11),(2,22),(4,44)) AS s(id,n) ON t.id=s.id
    WHEN MATCHED AND s.id=1 THEN UPDATE SET n=s.n
    WHEN MATCHED AND s.id=2 THEN DELETE
    WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n)
    WHEN NOT MATCHED BY SOURCE AND t.id=3 THEN DELETE
    OUTPUT $action, inserted.id, deleted.id INTO dbo.merge_output([action],inserted_id,deleted_id);`)
  await record('mixed rowcount', 'SELECT @@ROWCOUNT AS affected')
  await record('mixed output rows', 'SELECT [action],inserted_id,deleted_id FROM dbo.merge_output ORDER BY [action],COALESCE(inserted_id,deleted_id)')
  await record('mixed final rows', 'SELECT id,n FROM dbo.merge_mix ORDER BY id')

  await record('cte source', 'WITH s AS (SELECT 5 AS id,50 AS n) MERGE dbo.merge_target AS t USING s ON t.id=s.id WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n);')
  await record('cte rowcount', 'SELECT @@ROWCOUNT AS affected')
  await record('cte final rows', 'SELECT id,n FROM dbo.merge_target ORDER BY id')

  await record('duplicate create', 'CREATE TABLE dbo.merge_duplicate(id INT NOT NULL PRIMARY KEY,n INT NOT NULL)')
  await record('duplicate seed', 'INSERT dbo.merge_duplicate(id,n) VALUES (1,10)')
  await record('duplicate source error', 'MERGE dbo.merge_duplicate AS t USING (VALUES (1,11),(1,12)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n;', true)
  await record('duplicate final rows', 'SELECT id,n FROM dbo.merge_duplicate ORDER BY id')

  await record('constraint create', 'CREATE TABLE dbo.merge_constraint(id INT NOT NULL PRIMARY KEY,n INT NOT NULL CONSTRAINT CK_merge_positive CHECK (n>0))')
  await record('constraint seed', 'INSERT dbo.merge_constraint(id,n) VALUES (1,10)')
  await record('constraint failure', 'MERGE dbo.merge_constraint AS t USING (VALUES (1,11),(2,-1)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n);', true)
  await record('constraint final rows', 'SELECT id,n FROM dbo.merge_constraint ORDER BY id')

  await record('rollback create', 'CREATE TABLE dbo.merge_rollback(id INT NOT NULL PRIMARY KEY,n INT NOT NULL)')
  await record('rollback seed', 'INSERT dbo.merge_rollback(id,n) VALUES (1,10)')
  await record('rollback begin', 'BEGIN TRANSACTION')
  await record('rollback merge', 'MERGE dbo.merge_rollback AS t USING (VALUES (1,11),(2,20)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n);')
  await record('rollback pending rows', 'SELECT id,n FROM dbo.merge_rollback ORDER BY id')
  await record('rollback transaction', 'ROLLBACK TRANSACTION')
  await record('rollback final rows', 'SELECT id,n FROM dbo.merge_rollback ORDER BY id')

  await record('top create', 'CREATE TABLE dbo.merge_top(id INT NOT NULL PRIMARY KEY,n INT NOT NULL)')
  await record('top zero', 'MERGE TOP (0) dbo.merge_top AS t USING (VALUES (1,10)) AS s(id,n) ON t.id=s.id WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n);')
  await record('top zero rows', 'SELECT id,n FROM dbo.merge_top ORDER BY id')
  await record('top one', 'MERGE TOP (1) dbo.merge_top AS t USING (VALUES (1,10)) AS s(id,n) ON t.id=s.id WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n);')
  await record('top one rows', 'SELECT id,n FROM dbo.merge_top ORDER BY id')
  await record('negative top error', 'MERGE TOP (-1) dbo.merge_top AS t USING (VALUES (2,20)) AS s(id,n) ON t.id=s.id WHEN NOT MATCHED BY TARGET THEN INSERT (id,n) VALUES (s.id,s.n);', true)
  await record('negative top final rows', 'SELECT id,n FROM dbo.merge_top ORDER BY id')
  await record('unconditional matched before conditional error', 'MERGE dbo.merge_top AS t USING (VALUES (1,11)) AS s(id,n) ON t.id=s.id WHEN MATCHED THEN UPDATE SET n=s.n WHEN MATCHED AND s.n>0 THEN DELETE;', true)
  await record('repeated matched update error', 'MERGE dbo.merge_top AS t USING (VALUES (1,11)) AS s(id,n) ON t.id=s.id WHEN MATCHED AND s.n>0 THEN UPDATE SET n=s.n WHEN MATCHED THEN UPDATE SET n=s.n;', true)
  await record('invalid arms final rows', 'SELECT id,n FROM dbo.merge_top ORDER BY id')
  validate(run)
  return run
}

if (writeFixture) await refuseExistingFixture(fixture)
await mkdir(resolve(output, '..'), { recursive: true })
await withReferenceContainer(async (config, container) => {
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    runs.push(await isolatedReference({ ...config, options: { ...config.options, requestTimeout: 120000 } }, connection => observe(connection, repeat)))
  }
  assertSameCapture(bindGeneratedDatabaseNames(runs[0]), bindGeneratedDatabaseNames(runs[1]), 'fresh SQL Server databases differ after binding only generated database names')
  const actual = { image: container.image, runs }
  let retained
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) {
    assert.equal(actual.image, retained.image)
    assertSameCapture(actual.runs.map(bindGeneratedDatabaseNames), retained.runs.map(bindGeneratedDatabaseNames), 'fresh capture differs from retained fixture after binding only generated database names')
  }
  await writeFile(output, JSON.stringify(actual) + '\n')
  if (writeFixture) {
    await writeNewFixture(fixture, actual)
  }
  console.log(`Captured ${runs[0].length} MERGE observations in two fresh databases${retained ? ' and matched the retained fixture after binding generated database names' : ''}`)
})
