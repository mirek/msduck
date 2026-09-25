import assert from 'node:assert/strict'
import {mkdir, readFile, writeFile} from 'node:fs/promises'
import {resolve} from 'node:path'
import {withReferenceContainer} from './lib/reference-container.mjs'
import {isolatedReference} from './lib/reference.mjs'
import {capture, canonical} from './lib/compatibility.mjs'

const output = resolve(process.argv[2] ?? 'artifacts/compatibility/system-all-columns-reference')
const fixture = new URL('../reference/system-all-columns.json', import.meta.url)
const objectsFixture = new URL('../reference/all-objects.json', import.meta.url)
const views = ['all_columns', 'columns', 'system_columns']
const declarations = [...views, 'objects', 'system_objects', 'all_objects']
const columnKey = row => `${row.object_id}:${row.column_id}`
const membership = `WITH sources AS (
 SELECT object_id,column_id FROM sys.columns
 UNION ALL SELECT object_id,column_id FROM sys.system_columns
), missing AS (
 SELECT object_id,column_id FROM sys.all_columns
 EXCEPT SELECT object_id,column_id FROM sources
), extra AS (
 SELECT object_id,column_id FROM sources
 EXCEPT SELECT object_id,column_id FROM sys.all_columns
)
SELECT (SELECT COUNT(*) FROM sys.all_columns) AS all_count,
 (SELECT COUNT(*) FROM sys.columns) AS user_count,
 (SELECT COUNT(*) FROM sys.system_columns) AS system_count,
 (SELECT COUNT(*) FROM missing) AS missing_count,
 (SELECT COUNT(*) FROM extra) AS extra_count,
 (SELECT COUNT(*) FROM sys.columns u JOIN sys.system_columns s ON u.object_id=s.object_id AND u.column_id=s.column_id) AS overlap_count`
const valueMembership = `WITH sources AS (
 SELECT * FROM sys.columns UNION ALL SELECT * FROM sys.system_columns
), missing AS (
 SELECT * FROM sys.all_columns EXCEPT SELECT * FROM sources
), extra AS (
 SELECT * FROM sources EXCEPT SELECT * FROM sys.all_columns
)
SELECT (SELECT COUNT(*) FROM missing) AS missing_values,
 (SELECT COUNT(*) FROM extra) AS extra_values`
const snapshot = own => `SELECT c.*,
 OBJECT_SCHEMA_NAME(c.object_id) AS owner_schema,
 OBJECT_NAME(c.object_id) AS owner_name,
 o.type AS owner_type, o.parent_object_id AS owner_parent_object_id,
 OBJECT_SCHEMA_NAME(o.parent_object_id) AS owner_parent_schema,
 OBJECT_NAME(o.parent_object_id) AS owner_parent_name,
 p.type AS owner_parent_type,
 OBJECT_SCHEMA_NAME(c.default_object_id) AS default_schema,
 OBJECT_NAME(c.default_object_id) AS default_name,
 OBJECT_SCHEMA_NAME(c.rule_object_id) AS rule_schema,
 OBJECT_NAME(c.rule_object_id) AS rule_name,
 CAST(CASE WHEN u.object_id IS NULL THEN 0 ELSE 1 END AS BIT) AS in_columns,
 CAST(CASE WHEN s.object_id IS NULL THEN 0 ELSE 1 END AS BIT) AS in_system_columns
FROM sys.all_columns c
LEFT JOIN sys.all_objects o ON o.object_id=c.object_id
LEFT JOIN sys.all_objects p ON p.object_id=o.parent_object_id
LEFT JOIN sys.columns u ON u.object_id=c.object_id AND u.column_id=c.column_id
LEFT JOIN sys.system_columns s ON s.object_id=c.object_id AND s.column_id=c.column_id
${own ? "WHERE c.object_id IN (SELECT object_id FROM sys.objects WHERE schema_id=SCHEMA_ID(N'column_probe'))" : ''}
ORDER BY c.object_id,c.column_id`

function rows(run, name) {
 const observation = run.find(entry => entry.name === name)
 assert(observation, `Missing ${name}`)
 const [set] = observation.result.sets
 assert(set, `${name} returned no result set`)
 return set.rows.map(row => Object.fromEntries(set.columns.map((column, i) => [column.name, row[i]])))
}
function validate(run, objectReference) {
 const [counts] = rows(run, 'fresh membership')
 assert.equal(counts.all_count, counts.user_count + counts.system_count)
 assert.equal(counts.missing_count + counts.extra_count + counts.overlap_count, 0)
 const [values] = rows(run, 'fresh value membership')
 assert.equal(values.missing_values + values.extra_values, 0)
 const all = rows(run, 'fresh full catalog')
 assert.equal(all.length, counts.all_count)
 assert.equal(new Set(all.map(columnKey)).size, all.length, 'Duplicate all_columns key')
 for (const view of views) {
  const descriptor = run.find(entry => entry.name === `${view} descriptor`).result.sets[0]
  assert.equal(descriptor.columns.length, 43)
  assert.equal(descriptor.rows.length, 0)
  assert.equal(rows(run, `${view} declarations`).length, 43)
 }
 for (const view of ['objects', 'system_objects', 'all_objects']) {
  assert.equal(rows(run, `${view} declarations`).length, 12)
 }
 const hidden = new Map()
 const observedOwners = new Map()
 for (const row of all) {
  assert.equal(row.in_columns + row.in_system_columns, 1, `Unmatched source ${columnKey(row)}`)
  if (row.owner_type === null) assert.equal(row.in_columns, false)
  assert.equal(typeof row.owner_schema, 'string')
  assert.equal(typeof row.owner_name, 'string')
  const identity = `${row.owner_schema}.${row.owner_name}:${row.owner_type ?? '<outside all_objects>'}`
  const prior = observedOwners.get(row.object_id)
  if (prior) assert.equal(prior, identity, `Ambiguous owner ${row.object_id}`)
  observedOwners.set(row.object_id, identity)
  if (row.owner_type === null) hidden.set(row.object_id, identity)
  else {
   const reference = objectReference.get(row.object_id)
   assert(reference, `Visible owner absent from all_objects fixture: ${identity}`)
   assert.equal(reference, identity, `all_objects identity mismatch for ${row.object_id}`)
   if (row.owner_parent_object_id !== 0) {
    const parent = objectReference.get(row.owner_parent_object_id)
    assert.equal(parent, `${row.owner_parent_schema}.${row.owner_parent_name}:${row.owner_parent_type}`)
   }
  }
 }
 assert.equal(new Set(hidden.values()).size, hidden.size, 'Hidden owner names are ambiguous')
 assert.equal(hidden.size, 166)
 assert.equal(all.filter(row => row.owner_type === null).length, 2306)
 assert.equal(rows(run, 'created columns').length, 6)
 for (const row of rows(run, 'created columns')) {
  assert.equal(row.in_columns, true)
  assert.equal(row.in_system_columns, false)
  assert.equal(row.owner_schema, 'column_probe')
 }
 return {counts, hiddenOwners: hidden.size, hiddenColumns: all.filter(row => row.owner_type === null).length}
}
function bind(run) {
 const result = structuredClone(run)
 for (const observation of result) {
  if (!['fresh full catalog', 'created columns'].includes(observation.name)) continue
  const set = observation.result.sets[0]
  const names = set.columns.map(column => column.name)
  const index = name => names.indexOf(name)
  for (const row of set.rows) {
   const owner = `${row[index('owner_schema')]}.${row[index('owner_name')]}:${row[index('owner_type')] ?? '<outside all_objects>'}`
   row[index('object_id')] = {owner}
   for (const [field, schema, name, type] of [
    ['owner_parent_object_id', 'owner_parent_schema', 'owner_parent_name', 'owner_parent_type'],
    ['default_object_id', 'default_schema', 'default_name', null],
    ['rule_object_id', 'rule_schema', 'rule_name', null]
   ]) {
    if (row[index(field)] === 0 || row[index(field)] === null) continue
    assert.equal(typeof row[index(schema)], 'string', `Missing ${field} schema of ${owner}`)
    assert.equal(typeof row[index(name)], 'string', `Missing ${field} name of ${owner}`)
    if (type) assert.equal(typeof row[index(type)], 'string', `Missing ${field} type of ${owner}`)
    row[index(field)] = {identity: `${row[index(schema)]}.${row[index(name)]}${type ? `:${row[index(type)]}` : ''}`}
   }
  }
 }
 return result
}
function compare(left, right, label) {
 const a = bind(left), b = bind(right)
 assert.equal(a.length, b.length, label)
 for (let i = 0; i < a.length; i++) {
  if (JSON.stringify(a[i]) !== JSON.stringify(b[i])) {
   throw Error(`${label}: ${a[i].name} differs; raw captures retained`)
  }
 }
}

const objectCapture = JSON.parse(await readFile(objectsFixture, 'utf8'))
const objectSet = objectCapture.runs[0].find(entry => entry.name === 'fresh full catalog').result.sets[0]
const objectNames = objectSet.columns.map(column => column.name)
const objectReference = new Map(objectSet.rows.map(row => [
 row[objectNames.indexOf('object_id')],
 `${row[objectNames.indexOf('schema_name')]}.${row[objectNames.indexOf('name')]}:${row[objectNames.indexOf('type')]}`
]))
await mkdir(output, {recursive: true})
await withReferenceContainer(async (config, container) => {
 assert.equal(container.image, objectCapture.image, 'all_objects and all_columns captures use different SQL Server images')
 const runs = []
 for (let repeat = 0; repeat < 2; repeat++) {
  runs.push(await isolatedReference({...config, options: {...config.options, requestTimeout: 120000}}, async connection => {
   const observations = []
   async function record(name, sql) {
    const result = canonical(await capture(connection, sql))
    observations.push({name, sql, result})
    if (result.errors.length) {
     await writeFile(resolve(output, `failed-${repeat}.json`), JSON.stringify(observations, null, 2) + '\n')
     throw Error(`${name}: SQL error ${result.errors.map(error => error.message).join('; ')}`)
    }
   }
   await record('server identity', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version,CAST(SERVERPROPERTY('Edition') AS NVARCHAR(128)) AS edition,CAST(DATABASEPROPERTYEX(DB_NAME(),'Collation') AS NVARCHAR(128)) AS database_collation")
   for (const view of views) await record(`${view} descriptor`, `SELECT * FROM sys.${view} WHERE 1=0`)
   for (const view of declarations) await record(`${view} declarations`, `SELECT c.* FROM sys.all_columns c WHERE c.object_id=OBJECT_ID(N'sys.${view}') ORDER BY c.column_id`)
   await record('fresh membership', membership)
   await record('fresh value membership', valueMembership)
   await record('fresh full catalog', snapshot(false))
   await record('create schema', 'CREATE SCHEMA column_probe AUTHORIZATION dbo')
   await record('create table', "CREATE TABLE column_probe.sample(id INT IDENTITY(1,1) PRIMARY KEY, label NVARCHAR(20) COLLATE Latin1_General_100_CI_AS NOT NULL CONSTRAINT df_sample_label DEFAULT N'x', amount DECIMAL(9,2) NULL, doubled AS amount * 2)")
   await record('create view', 'CREATE VIEW column_probe.sample_view AS SELECT id,label FROM column_probe.sample')
   await record('created columns', snapshot(true))
   return observations
  }))
 }
 await writeFile(resolve(output, 'runs.json'), JSON.stringify(runs) + '\n')
 const validation = runs.map(run => validate(run, objectReference))
 compare(runs[0], runs[1], 'Two fresh databases')
 const actual = {
  image: container.image,
  comparisonBindings: 'object_id is compared by captured owner schema/name/type; nonzero owner-parent IDs bind to captured schema/name/type, and default/rule IDs bind to resolved qualified names. All other row values, descriptors, errors and completions compare exactly. Raw runs remain untouched.',
  validation, runs
 }
 let retained
 try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
 catch (error) { if (error.code !== 'ENOENT') throw error }
 if (retained) {
  assert.equal(actual.image, retained.image)
  assert.equal(actual.comparisonBindings, retained.comparisonBindings)
  for (const run of actual.runs) for (const previous of retained.runs) compare(run, previous, 'Independent retained capture')
 }
 await writeFile(resolve(output, 'system-all-columns.json'), JSON.stringify(actual) + '\n')
 console.log(`Captured ${validation[0].counts.all_count} columns in two fresh databases${retained ? ' and matched the retained capture' : ''}`)
})
