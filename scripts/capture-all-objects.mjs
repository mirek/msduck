import assert from 'node:assert/strict'
import {mkdir, readFile, writeFile} from 'node:fs/promises'
import {resolve} from 'node:path'
import {withReferenceContainer} from './lib/reference-container.mjs'
import {connect, isolatedReference, assertSameCapture} from './lib/reference.mjs'
import {capture, canonical} from './lib/compatibility.mjs'

const views = ['all_objects', 'objects', 'system_objects']
const membership = `SELECT
 (SELECT COUNT(*) FROM sys.all_objects) AS all_count,
 (SELECT COUNT(*) FROM sys.objects) AS user_count,
 (SELECT COUNT(*) FROM sys.system_objects) AS system_count,
 (SELECT COUNT(*) FROM (SELECT object_id FROM sys.all_objects EXCEPT SELECT object_id FROM sys.objects EXCEPT SELECT object_id FROM sys.system_objects) q) AS all_only,
 (SELECT COUNT(*) FROM (SELECT object_id FROM sys.objects UNION SELECT object_id FROM sys.system_objects EXCEPT SELECT object_id FROM sys.all_objects) q) AS union_only,
 (SELECT COUNT(*) FROM sys.objects u JOIN sys.system_objects s ON u.object_id=s.object_id) AS overlap`
const snapshot = (owned = false) => `SELECT o.*,SCHEMA_NAME(o.schema_id) AS schema_name,
 p.name AS parent_name,SCHEMA_NAME(p.schema_id) AS parent_schema,
 CAST(CASE WHEN u.object_id IS NULL THEN 0 ELSE 1 END AS BIT) AS in_objects,
 CAST(CASE WHEN s.object_id IS NULL THEN 0 ELSE 1 END AS BIT) AS in_system_objects
 FROM sys.all_objects o LEFT JOIN sys.all_objects p ON p.object_id=o.parent_object_id
 LEFT JOIN sys.objects u ON u.object_id=o.object_id
 LEFT JOIN sys.system_objects s ON s.object_id=o.object_id
 ${owned ? "WHERE SCHEMA_NAME(o.schema_id) IN (N'catalog_probe',N'catalog_moved')" : ''}
 ORDER BY SCHEMA_NAME(o.schema_id),o.name,o.type`
const setup = [
 'CREATE SCHEMA catalog_probe AUTHORIZATION dbo',
 'CREATE SCHEMA catalog_moved AUTHORIZATION dbo',
 'CREATE TABLE catalog_probe.parent(id INT CONSTRAINT pk_parent PRIMARY KEY,n INT CONSTRAINT df_parent DEFAULT 7 CONSTRAINT ck_parent CHECK(n>=0))',
 'CREATE TABLE catalog_probe.child(id INT,parent_id INT CONSTRAINT fk_child REFERENCES catalog_probe.parent(id))',
 'CREATE VIEW catalog_probe.probe_view AS SELECT id,n FROM catalog_probe.parent',
 'CREATE PROCEDURE catalog_probe.probe_proc AS SELECT id FROM catalog_probe.parent',
 'CREATE FUNCTION catalog_probe.probe_scalar(@n INT) RETURNS INT AS BEGIN RETURN @n+1 END',
 'CREATE FUNCTION catalog_probe.probe_inline(@n INT) RETURNS TABLE AS RETURN (SELECT @n AS n)',
 'CREATE FUNCTION catalog_probe.probe_table(@n INT) RETURNS @t TABLE(n INT) AS BEGIN INSERT @t VALUES(@n); RETURN END',
 'CREATE TRIGGER catalog_probe.probe_trigger ON catalog_probe.parent AFTER INSERT AS BEGIN SET NOCOUNT ON END',
 'CREATE SEQUENCE catalog_probe.probe_sequence AS INT START WITH 5',
 'CREATE SYNONYM catalog_probe.probe_synonym FOR catalog_probe.parent',
]
const stages = [
 ['transaction create', ['BEGIN TRANSACTION', 'CREATE TABLE catalog_probe.transient(id INT)']],
 ['transaction schema transfer', ['ALTER SCHEMA catalog_moved TRANSFER catalog_probe.probe_view']],
 ['transaction drop', ['DROP TABLE catalog_probe.child']],
 ['rollback', ['ROLLBACK TRANSACTION']],
 ['alter table', ['ALTER TABLE catalog_probe.parent ADD extra INT']],
 ['committed drop', ['BEGIN TRANSACTION', 'DROP SYNONYM catalog_probe.probe_synonym', 'COMMIT TRANSACTION']],
]
const output = resolve(process.argv[2] ?? 'artifacts/compatibility/all-objects-reference')
await mkdir(output, {recursive: true})

// Raw captures are never rewritten. Only cross-database comparison binds IDs to
// the same row's qualified identity and omits values supplied by the server clock.
function bound(run) {
 const result = structuredClone(run)
 for (const entry of result) for (const set of entry.result.sets) {
  const names = set.columns.map(c => c.name)
  if (!names.includes('schema_name') || !names.includes('object_id')) continue
  const at = (row, name) => row[names.indexOf(name)]
  const identities = new Set()
  for (const row of set.rows) {
   const identity = `${at(row, 'schema_name')}.${at(row, 'name')}:${at(row, 'type')}`
   assert(!identities.has(identity), `Ambiguous captured identity ${identity}`)
   identities.add(identity)
   assert.equal(typeof at(row, 'object_id'), 'number')
   row[names.indexOf('object_id')] = {identity}
   row[names.indexOf('schema_id')] = {schema: at(row, 'schema_name')}
   const parent = at(row, 'parent_object_id')
   if (parent !== 0) {
    assert.equal(typeof at(row, 'parent_name'), 'string')
    row[names.indexOf('parent_object_id')] = {parent: `${at(row, 'parent_schema')}.${at(row, 'parent_name')}`}
   }
   for (const name of ['create_date', 'modify_date']) {
    const value = at(row, name)
    assert.equal(value.kind, 'date')
    assert(!Number.isNaN(Date.parse(value.value)), 'Invalid captured catalog date')
    row[names.indexOf(name)] = {serverClock: name}
   }
  }
 }
 return result
}
function validate(run) {
 const rows = name => {
  const set = run.find(entry => entry.name === name).result.sets[0]
  return set.rows.map(row => Object.fromEntries(set.columns.map((c,i) => [c.name,row[i]])))
 }
 for (const name of ['fresh membership','created membership','final membership']) {
  const [r] = rows(name)
  assert.equal(r.all_count, r.user_count+r.system_count)
  assert.equal(r.all_only+r.union_only+r.overlap, 0)
 }
 for (const entry of run) for (const set of entry.result.sets) {
  const columns = set.columns.map(c => c.name)
  if (!columns.includes('schema_name') || !columns.includes('object_id')) continue
  const ids = set.rows.map(row => row[columns.indexOf('object_id')])
  assert.equal(new Set(ids).size, ids.length, `${entry.name}: duplicate object IDs`)
  const objects = new Map(set.rows.map(row => [row[columns.indexOf('object_id')], row]))
  for (const row of set.rows) {
   const parentId = row[columns.indexOf('parent_object_id')]
   if (parentId !== 0) {
    const parent = objects.get(parentId)
    assert(parent, `${entry.name}: missing parent row`)
    assert.equal(parent[columns.indexOf('name')], row[columns.indexOf('parent_name')])
    assert.equal(parent[columns.indexOf('schema_name')], row[columns.indexOf('parent_schema')])
   }
  }
 }
 const created = new Map(rows('created objects').map(row => [row.name,row]))
 assert.equal(created.size, 14)
 for (const name of ['transaction create owner objects','transaction schema transfer owner objects','transaction drop owner objects','rollback owner objects','alter table owner objects','committed drop owner objects']) {
  for (const row of rows(name)) {
   if (created.has(row.name)) assert.equal(row.object_id, created.get(row.name).object_id, `${name}: identity changed`)
  }
 }
 const rolledBack = rows('rollback owner objects')
 assertSameCapture(rolledBack, rows('created objects'), 'Rollback did not restore the complete captured catalog rows')
}
function compare(left, right, description) {
 const a = bound(left), b = bound(right)
 assert.equal(a.length, b.length, description)
 for (let i=0;i<a.length;i++) if (JSON.stringify(a[i]) !== JSON.stringify(b[i])) {
  throw Error(`${description}: observation ${i} (${a[i].name}); raw captures retained`)
 }
}
await withReferenceContainer(async (config, container) => {
 const runs = []
 for (let repeat=0;repeat<2;repeat++) runs.push(await isolatedReference(config, async connection => {
  const results = []
  async function record(name, sql, client = connection, allowLockTimeout = false) {
   const result = canonical(await capture(client, sql))
   results.push({name, sql, result})
   if (!result.errors.every(e => allowLockTimeout && e.number === 1222)) {
    await writeFile(resolve(output, `failed-${repeat}.json`), JSON.stringify(results, null, 2)+'\n')
    throw Error(`${name}: unexpected SQL error; raw observations retained`)
   }
   return result
  }
  await record('server identity', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version,CAST(SERVERPROPERTY('Edition') AS NVARCHAR(128)) AS edition,CAST(DATABASEPROPERTYEX(DB_NAME(),'Collation') AS NVARCHAR(128)) AS database_collation")
  for (const view of views) {
   await record(`${view} descriptor`, `SELECT * FROM sys.${view} WHERE 1=0`)
   await record(`${view} declarations`, `SELECT name,column_id,system_type_id,user_type_id,max_length,precision,scale,collation_name,is_nullable FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.${view}') ORDER BY column_id`)
  }
  await record('fresh membership', membership)
  await record('fresh full catalog', snapshot())
  for (let i=0;i<setup.length;i++) await record(`setup ${i}`, setup[i])
  await record('created membership', membership)
  await record('created objects', snapshot(true))
  const database = (await capture(connection, 'SELECT DB_NAME()')).sets[0].rows[0][0]
  const observer = await connect({...config, options: {...config.options, database}})
  try {
   await record('observer lock timeout', 'SET LOCK_TIMEOUT 1000', observer)
   await record('observer committed objects', snapshot(true), observer)
   for (const [name, statements] of stages) {
    for (let i=0;i<statements.length;i++) await record(`${name} statement ${i}`, statements[i])
    await record(`${name} owner objects`, snapshot(true))
    await record(`${name} observer objects`, snapshot(true), observer, name.startsWith('transaction'))
   }
   await record('final membership', membership)
  } finally { observer.close() }
  return results
 }))
 await writeFile(resolve(output, 'runs.json'), JSON.stringify(runs, null, 2)+'\n')
 for (const run of runs) validate(run)
 compare(runs[0], runs[1], 'Fresh catalog captures differ')
 const actual = {image: container.image, comparisonBindings: 'object_id and schema_id bind to captured qualified identities; nonzero parent_object_id binds to captured parent identity; create_date and modify_date are server clock values. Raw rows are retained unchanged in both runs.', runs}
 await writeFile(resolve(output, 'all-objects.json'), JSON.stringify(actual, null, 2)+'\n')
 let retained
 try { retained=JSON.parse(await readFile(new URL('../reference/all-objects.json', import.meta.url), 'utf8')) }
 catch(error) { if(error.code!=='ENOENT') throw error }
 if(retained) {
  assert.equal(actual.image, retained.image)
  assert.equal(actual.comparisonBindings, retained.comparisonBindings)
  for(const run of actual.runs) for(const previous of retained.runs) compare(run, previous, 'Retained catalog capture differs')
 }
 console.log(`Captured ${runs[0].length} catalog observations twice${retained ? ' and independently matched the retained fixture' : ''}`)
})
