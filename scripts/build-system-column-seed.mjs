#!/usr/bin/env node
import assert from 'node:assert/strict'
import {assertSameCapture} from './lib/reference.mjs'
import {mkdirSync, readFileSync, writeFileSync} from 'node:fs'

const capture = JSON.parse(readFileSync(new URL('../reference/system-all-columns.json', import.meta.url), 'utf8'))
const objects = JSON.parse(readFileSync(new URL('../reference/all-objects.json', import.meta.url), 'utf8'))
const output = new URL('../src/column_catalog/system_columns.json', import.meta.url)
const expected = ['object_id','name','column_id','system_type_id','user_type_id','max_length',
 'precision','scale','collation_name','is_nullable','is_ansi_padded','is_rowguidcol',
 'is_identity','is_computed','is_filestream','is_replicated','is_non_sql_subscribed',
 'is_merge_published','is_dts_replicated','is_xml_document','xml_collection_id',
 'default_object_id','rule_object_id','is_sparse','is_column_set','generated_always_type',
 'generated_always_type_desc','encryption_type','encryption_type_desc',
 'encryption_algorithm_name','column_encryption_key_id','column_encryption_key_database_name',
 'is_hidden','is_masked','graph_type','graph_type_desc','is_data_deletion_filter_column',
 'ledger_view_column_type','ledger_view_column_type_desc','is_dropped_ledger_column',
 'vector_dimensions','vector_base_type','vector_base_type_desc']

function set(run, name) {
 const entry = run.find(observation => observation.name === name)
 assert(entry, `Missing ${name}`)
 assert.deepEqual(entry.result.errors, [], `${name}: SQL errors`)
 assert.equal(entry.result.sets.length, 1)
 return entry.result.sets[0]
}
assert.equal(capture.image, objects.image, 'Catalog reference images differ')
assert.equal(capture.runs.length, 2)
const objectSet = set(objects.runs[0], 'fresh full catalog')
const objectNames = objectSet.columns.map(column => column.name)
const objectAt = (row, name) => row[objectNames.indexOf(name)]
const visible = new Map(objectSet.rows.map(row => [objectAt(row, 'object_id'),
 `${objectAt(row, 'schema_name')}.${objectAt(row, 'name')}:${objectAt(row, 'type')}`]))

function inventory(run) {
 const result = set(run, 'fresh full catalog')
 const names = result.columns.map(column => column.name)
 assert.deepEqual(names.slice(0, 43), expected)
 assert.equal(result.rows.length, 12803)
 const at = (row, name) => row[names.indexOf(name)]
 const ids = new Set()
 const hidden = new Map()
 const rows = []
 let user = 0, system = 0
 for (const row of result.rows) {
  const objectId = at(row, 'object_id'), columnId = at(row, 'column_id')
  const key = `${objectId}:${columnId}`
  assert(!ids.has(key), `Duplicate column key ${key}`)
  ids.add(key)
  assert.equal(row.length, names.length)
  const inUser = at(row, 'in_columns'), inSystem = at(row, 'in_system_columns')
  assert.equal(typeof inUser, 'boolean')
  assert.equal(typeof inSystem, 'boolean')
  assert.notEqual(inUser, inSystem, `Ambiguous membership ${key}`)
  if (inUser) user++; else system++
  const schema = at(row, 'owner_schema'), name = at(row, 'owner_name')
  const type = at(row, 'owner_type')
  assert.equal(typeof schema, 'string')
  assert.equal(typeof name, 'string')
  if (type === null) {
   assert(inSystem, `Hidden owner in sys.columns: ${key}`)
   assert.equal(schema, 'sys')
   const identity = [schema, name]
   if (hidden.has(objectId)) assert.deepEqual(hidden.get(objectId), identity)
   hidden.set(objectId, identity)
  } else {
   assert.equal(visible.get(objectId), `${schema}.${name}:${type}`, `Visible owner ${objectId} differs from all_objects`)
  }
  rows.push([...row.slice(0, 43), inSystem])
 }
 assert.equal(user, 1269)
 assert.equal(system, 11534)
 assert.equal(hidden.size, 166)
 assert.equal(new Set([...hidden.values()].map(identity => JSON.stringify(identity))).size, hidden.size)
 assert.equal(result.rows.filter(row => at(row, 'owner_type') === null).length, 2306)
 return {rows, hidden_owners: [...hidden].sort((a,b) => a[0]-b[0]).map(([id, identity]) => [id, ...identity])}
}
const first = inventory(capture.runs[0])
const second = inventory(capture.runs[1])
assertSameCapture(first, second, 'Retained fresh-database inventories differ')
const seed = {source_image: capture.image, columns: [...expected, 'in_system_columns'],
 rows: first.rows, hidden_owners: first.hidden_owners}
mkdirSync(new URL('../src/column_catalog/', import.meta.url), {recursive: true})
writeFileSync(output, JSON.stringify(seed) + '\n')
console.log(`${first.rows.length} columns, ${first.hidden_owners.length} hidden owners`)
