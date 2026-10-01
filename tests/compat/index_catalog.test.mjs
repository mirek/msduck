// sys.indexes and sys.index_columns for PRIMARY KEY and UNIQUE constraints
// and keys-managed indexes (src/index_catalog.rs). The expected rows were
// captured from SQL Server 2022 (16.0.4236.2) running the same statements in
// a fresh database; tests/gaps_index_catalog.rs asserts the full row sets.
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { start, query } from '../support/client.mjs'

const schema = [
  'CREATE TABLE items (id int NOT NULL CONSTRAINT pk_items PRIMARY KEY, name nvarchar(100) NULL)',
  'CREATE TABLE orders (id int NOT NULL CONSTRAINT pk_orders PRIMARY KEY NONCLUSTERED, placed int NOT NULL, customer int NULL)',
  'CREATE CLUSTERED INDEX cx_orders ON orders(placed)',
  'CREATE INDEX ix_orders_customer ON orders(customer)',
  'CREATE TABLE people (id int NOT NULL CONSTRAINT pk_people PRIMARY KEY, email nvarchar(200) NULL CONSTRAINT uq_people_email UNIQUE, first nvarchar(50) NOT NULL, last nvarchar(50) NOT NULL, CONSTRAINT uq_people_name UNIQUE (last, first))',
  'CREATE TABLE events (id int NOT NULL, kind int NULL, seen datetime2 NULL, payload nvarchar(400) NULL)',
  'CREATE UNIQUE INDEX ux_events_kind ON events(kind DESC, id) INCLUDE (payload)',
  'CREATE INDEX fx_events_seen ON events(seen) WHERE seen IS NOT NULL AND kind > 5',
]

let databases = 0
async function fresh(t) {
  const connection = await start(t)
  const name = `index_catalog_${process.pid}_${databases++}`
  await query(connection, `CREATE DATABASE ${name}`)
  await query(connection, `USE ${name}`)
  for (const sql of schema) await query(connection, sql)
  return connection
}

const indexes = table => `SELECT name, index_id, type, type_desc, is_unique, is_primary_key, is_unique_constraint, has_filter, filter_definition
  FROM sys.indexes WHERE object_id = OBJECT_ID(N'${table}') ORDER BY index_id`
const columns = table => `SELECT index_id, index_column_id, column_id, key_ordinal, is_descending_key, is_included_column
  FROM sys.index_columns WHERE object_id = OBJECT_ID(N'${table}') ORDER BY index_id, index_column_id`

test('SELECT * by OBJECT_ID no longer fails for a table with a primary key', async t => {
  const connection = await fresh(t)
  const all = await query(connection, "SELECT * FROM sys.indexes WHERE object_id = OBJECT_ID(N'items')")
  assert.equal(all.rows.length, 1)
  assert.equal(all.rows[0][1], 'pk_items')
  const keys = await query(connection, "SELECT * FROM sys.index_columns WHERE object_id = OBJECT_ID(N'items')")
  assert.deepEqual(keys.rows.map(row => row.slice(1, 6)), [[1, 1, 1, 1, 0]])
})

test('constraint indexes match SQL Server', async t => {
  const connection = await fresh(t)
  assert.deepEqual((await query(connection, indexes('items'))).rows, [
    ['pk_items', 1, 1, 'CLUSTERED', true, true, false, false, null],
  ])
  assert.deepEqual((await query(connection, indexes('orders'))).rows, [
    ['cx_orders', 1, 1, 'CLUSTERED', false, false, false, false, null],
    ['pk_orders', 2, 2, 'NONCLUSTERED', true, true, false, false, null],
    ['ix_orders_customer', 3, 2, 'NONCLUSTERED', false, false, false, false, null],
  ])
  assert.deepEqual((await query(connection, indexes('people'))).rows, [
    ['pk_people', 1, 1, 'CLUSTERED', true, true, false, false, null],
    ['uq_people_name', 2, 2, 'NONCLUSTERED', true, false, true, false, null],
    ['uq_people_email', 3, 2, 'NONCLUSTERED', true, false, true, false, null],
  ])
  assert.deepEqual((await query(connection, columns('people'))).rows, [
    [1, 1, 1, 1, false, false],
    [2, 1, 4, 1, false, false],
    [2, 2, 3, 2, false, false],
    [3, 1, 2, 1, false, false],
  ])
})

test('unique, filtered and INCLUDE indexes match SQL Server', async t => {
  const connection = await fresh(t)
  assert.deepEqual((await query(connection, indexes('events'))).rows, [
    [null, 0, 0, 'HEAP', false, false, false, false, null],
    ['ux_events_kind', 2, 2, 'NONCLUSTERED', true, false, false, false, null],
    ['fx_events_seen', 3, 2, 'NONCLUSTERED', false, false, false, true, '([seen] IS NOT NULL AND [kind]>(5))'],
  ])
  assert.deepEqual((await query(connection, columns('events'))).rows, [
    [2, 1, 2, 1, true, false],
    [2, 2, 1, 2, false, false],
    [2, 3, 4, 0, false, true],
    [3, 1, 3, 1, false, false],
  ])
})
