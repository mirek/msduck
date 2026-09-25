#!/usr/bin/env node
import assert from 'node:assert/strict'
import {mkdir, readFile, writeFile} from 'node:fs/promises'
import {resolve} from 'node:path'
import {withReferenceContainer} from './lib/reference-container.mjs'
import {isolatedReference} from './lib/reference.mjs'
import {capture, canonical} from './lib/compatibility.mjs'

const output = resolve(process.argv[2] ?? 'artifacts/compatibility/alter-named-default-reference')
const fixture = new URL('../reference/alter-named-default.json', import.meta.url)
const scenarios = [
  ...[false, true].flatMap(populated =>
    [false, true].flatMap(nullable =>
      [false, true].map(withValues => ({
        name: `${nullable ? 'nullable' : 'required'}_${populated ? 'populated' : 'empty'}_${withValues ? 'with' : 'without'}_values`,
        populated, nullable, withValues
      }))))
]
const table = scenario => `dbo.alter_${scenario.name}`
const constraint = scenario => `df_alter_${scenario.name}`
const catalog = (tableName, columnName = 'new_value') => `
SELECT t.object_id AS table_object_id,c.name AS column_name,c.column_id,c.is_nullable,
 c.default_object_id,d.object_id AS constraint_object_id,d.name AS constraint_name,
 d.parent_object_id AS constraint_parent_object_id,d.parent_column_id,d.definition,d.is_system_named,
 o.object_id AS object_object_id,o.name AS object_name,o.type AS object_type,
 o.parent_object_id AS object_parent_object_id
FROM sys.columns c JOIN sys.tables t ON t.object_id=c.object_id
LEFT JOIN sys.default_constraints d ON d.parent_object_id=c.object_id AND d.parent_column_id=c.column_id
LEFT JOIN sys.objects o ON o.object_id=c.default_object_id
WHERE t.object_id=OBJECT_ID(N'${tableName}') AND c.name=N'${columnName}'`

function observation(run, name) {
  const entry = run.find(item => item.name === name)
  assert(entry, `missing ${name}`)
  return entry.result
}
function rows(result) {
  assert.equal(result.sets.length, 1)
  const set = result.sets[0]
  return set.rows.map(row => Object.fromEntries(set.columns.map((column, index) => [column.name, row[index]])))
}
function success(run, name) {
  const result = observation(run, name)
  assert.deepEqual(result.errors, [], `${name} returned an error`)
  return result
}
function failure(run, name) {
  const result = observation(run, name)
  assert(result.errors.length, `${name} unexpectedly succeeded`)
  return result
}
function validate(run) {
  const summary = []
  success(run, 'server identity')
  for (const scenario of scenarios) {
    const prefix = scenario.name
    success(run, `${prefix} create`)
    if (scenario.populated) success(run, `${prefix} seed`)
    success(run, `${prefix} alter`)
    const before = rows(success(run, `${prefix} existing rows`))
    assert.equal(before.length, scenario.populated ? 2 : 0, prefix)
    for (const [index, row] of before.entries()) {
      assert.equal(row.id, index + 1)
      assert.equal(row.old_value, (index + 1) * 10)
      assert.equal(row.new_value, scenario.nullable && !scenario.withValues ? null : 7)
    }
    const catalogRows = rows(success(run, `${prefix} catalog`))
    assert.equal(catalogRows.length, 1, `${prefix} catalog cardinality`)
    const [column] = catalogRows
    assert(column, `${prefix} lost its catalog row`)
    assert.equal(column.column_name, 'new_value')
    assert.equal(column.is_nullable, scenario.nullable)
    assert.equal(column.constraint_name, constraint(scenario))
    assert.equal(column.object_name, constraint(scenario))
    assert.equal(column.object_type, 'D ')
    assert.equal(column.is_system_named, false)
    assert.equal(column.default_object_id, column.constraint_object_id)
    assert.equal(column.default_object_id, column.object_object_id)
    assert.equal(column.table_object_id, column.constraint_parent_object_id)
    assert.equal(column.table_object_id, column.object_parent_object_id)
    assert.equal(column.column_id, column.parent_column_id)
    assert(column.default_object_id > 0 && column.table_object_id > 0)
    success(run, `${prefix} future insert`)
    const after = rows(success(run, `${prefix} after insert`))
    assert.equal(after.length, before.length + 1)
    assert.deepEqual(after.slice(0, before.length), before)
    assert.deepEqual(after.at(-1), {id: 3, old_value: 30, new_value: 7})
    summary.push({name: prefix, existingRows: before.length, backfilled: before.filter(row => row.new_value === 7).length, definition: column.definition})
  }

  success(run, 'duplicate create')
  const duplicate = failure(run, 'duplicate alter')
  assert.deepEqual(rows(success(run, 'duplicate catalog')), [])
  assert.deepEqual(rows(success(run, 'duplicate original rows')), [{id: 1, old_value: 10}])
  success(run, 'invalid default create')
  success(run, 'invalid default seed')
  const invalid = failure(run, 'invalid default alter')
  assert.deepEqual(rows(success(run, 'invalid default catalog')), [])
  assert.deepEqual(rows(success(run, 'invalid default original rows')), [{id: 1, old_value: 10}])

  success(run, 'rollback create')
  success(run, 'rollback seed')
  success(run, 'rollback begin')
  success(run, 'rollback alter')
  const pendingRows = rows(success(run, 'rollback pending catalog'))
  assert.equal(pendingRows.length, 1)
  const [pending] = pendingRows
  assert(pending?.default_object_id > 0)
  assert.deepEqual(rows(success(run, 'rollback pending rows')), [{id: 1, old_value: 10, new_value: 7}])
  success(run, 'rollback transaction')
  assert.deepEqual(rows(success(run, 'rollback catalog')), [])
  assert.deepEqual(rows(success(run, 'rollback original rows')), [{id: 1, old_value: 10}])
  assert.deepEqual(rows(success(run, 'rollback default object')), [])
  return {scenarios: summary, duplicateErrors: duplicate.errors, invalidDefaultErrors: invalid.errors}
}

// Keep raw runs unchanged. Bind only the five catalog identity columns for
// comparing independent fresh databases whose allocated object IDs may differ.
function bound(run) {
  return run.map(entry => {
    const copy = structuredClone(entry)
    for (const set of copy.result.sets) {
      const names = set.columns.map(column => column.name)
      const at = name => names.indexOf(name)
      if (!names.includes('table_object_id') || !names.includes('constraint_name')) continue
      for (const row of set.rows) {
        const tableName = entry.name.split(' catalog')[0].replace(/^rollback pending$/, 'rollback')
        const constraintName = row[at('constraint_name')]
        for (const field of ['table_object_id','constraint_parent_object_id','object_parent_object_id']) {
          row[at(field)] = {table: tableName}
        }
        for (const field of ['default_object_id','constraint_object_id','object_object_id']) {
          row[at(field)] = {constraint: constraintName}
        }
      }
    }
    return copy
  })
}

await mkdir(output, {recursive: true})
await withReferenceContainer(async (config, container) => {
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    runs.push(await isolatedReference({...config, options: {...config.options, requestTimeout: 120000}}, async connection => {
      const observations = []
      async function record(name, sql, expectError = false) {
        const result = canonical(await capture(connection, sql))
        observations.push({name, sql, result})
        if (Boolean(result.errors.length) !== expectError) {
          await writeFile(resolve(output, `failed-${repeat}.json`), JSON.stringify(observations, null, 2) + '\n')
          throw Error(`${name}: ${expectError ? 'expected an error' : 'unexpected SQL error'}`)
        }
      }
      await record('server identity', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version,CAST(DATABASEPROPERTYEX(DB_NAME(),'Collation') AS NVARCHAR(128)) AS database_collation")
      for (const scenario of scenarios) {
        const prefix = scenario.name
        const name = table(scenario)
        await record(`${prefix} create`, `CREATE TABLE ${name}(id INT NOT NULL PRIMARY KEY,old_value INT NOT NULL)`)
        if (scenario.populated) await record(`${prefix} seed`, `INSERT ${name}(id,old_value) VALUES (1,10),(2,20)`)
        await record(`${prefix} alter`, `ALTER TABLE ${name} ADD new_value INT ${scenario.nullable ? 'NULL' : 'NOT NULL'} CONSTRAINT ${constraint(scenario)} DEFAULT (7)${scenario.withValues ? ' WITH VALUES' : ''}`)
        await record(`${prefix} existing rows`, `SELECT id,old_value,new_value FROM ${name} ORDER BY id`)
        await record(`${prefix} catalog`, catalog(name))
        await record(`${prefix} future insert`, `INSERT ${name}(id,old_value) VALUES (3,30)`)
        await record(`${prefix} after insert`, `SELECT id,old_value,new_value FROM ${name} ORDER BY id`)
      }
      await record('duplicate create', 'CREATE TABLE dbo.alter_duplicate(id INT NOT NULL PRIMARY KEY,old_value INT NOT NULL); INSERT dbo.alter_duplicate(id,old_value) VALUES (1,10)')
      await record('duplicate alter', `ALTER TABLE dbo.alter_duplicate ADD new_value INT NULL CONSTRAINT ${constraint(scenarios[0])} DEFAULT (7)`, true)
      await record('duplicate catalog', catalog('dbo.alter_duplicate'))
      await record('duplicate original rows', 'SELECT id,old_value FROM dbo.alter_duplicate ORDER BY id')
      await record('invalid default create', 'CREATE TABLE dbo.alter_invalid(id INT NOT NULL PRIMARY KEY,old_value INT NOT NULL)')
      await record('invalid default seed', 'INSERT dbo.alter_invalid(id,old_value) VALUES (1,10)')
      await record('invalid default alter', "ALTER TABLE dbo.alter_invalid ADD new_value INT NOT NULL CONSTRAINT df_alter_invalid DEFAULT (CONVERT(INT,'bad')) WITH VALUES", true)
      await record('invalid default catalog', catalog('dbo.alter_invalid'))
      await record('invalid default original rows', 'SELECT id,old_value FROM dbo.alter_invalid ORDER BY id')
      await record('rollback create', 'CREATE TABLE dbo.alter_rollback(id INT NOT NULL PRIMARY KEY,old_value INT NOT NULL)')
      await record('rollback seed', 'INSERT dbo.alter_rollback(id,old_value) VALUES (1,10)')
      await record('rollback begin', 'BEGIN TRANSACTION')
      await record('rollback alter', 'ALTER TABLE dbo.alter_rollback ADD new_value INT NOT NULL CONSTRAINT df_alter_rollback DEFAULT (7) WITH VALUES')
      await record('rollback pending catalog', catalog('dbo.alter_rollback'))
      await record('rollback pending rows', 'SELECT id,old_value,new_value FROM dbo.alter_rollback ORDER BY id')
      await record('rollback transaction', 'ROLLBACK TRANSACTION')
      await record('rollback catalog', catalog('dbo.alter_rollback'))
      await record('rollback original rows', 'SELECT id,old_value FROM dbo.alter_rollback ORDER BY id')
      await record('rollback default object', "SELECT object_id,name,type,parent_object_id FROM sys.objects WHERE name=N'df_alter_rollback'")
      return observations
    }))
  }
  const validation = runs.map(validate)
  assert.deepEqual(bound(runs[0]), bound(runs[1]), 'fresh database runs differ beyond allocated object IDs')
  const actual = {image: container.image,
    comparisonBindings: 'Raw observations remain untouched. Cross-run comparison replaces only catalog table/default object IDs by scenario table and constraint name; all descriptors, rows, diagnostics and completions otherwise compare exactly.',
    validation, runs}
  let retained
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) {
    assert.equal(actual.image, retained.image)
    assert.equal(actual.comparisonBindings, retained.comparisonBindings)
    for (const run of actual.runs) for (const previous of retained.runs) {
      assert.deepEqual(bound(run), bound(previous), 'independent capture differs')
    }
  }
  await writeFile(resolve(output, 'alter-named-default.json'), JSON.stringify(actual) + '\n')
  console.log(`Captured ${scenarios.length} ALTER variants in two fresh databases${retained ? ' and matched the retained fixture' : ''}`)
})
