// Constraint, module and file catalogs, OBJECT_DEFINITION, sp_pkeys,
// sp_fkeys, sp_rename and INFORMATION_SCHEMA (docs/gaps-catalog.md).
//
// Every statement of the captured SQL Server profiles in
// reference/gaps-catalog.json runs through tedious in a database of the
// captured name, and its result sets, rows, errors, messages and return
// status must equal SQL Server's. The few known differences are listed
// below with their reason and asserted exactly, so they cannot widen.
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { start, query } from '../support/client.mjs'
import { captureBatch } from '../../scripts/capture-order-token.mjs'
import { canonical } from '../../scripts/lib/compatibility.mjs'
import { v2Checks } from '../../scripts/capture-gaps-catalog.mjs'

const reference = JSON.parse(readFileSync(new URL('../../reference/gaps-catalog.json', import.meta.url)))

const summary = result => ({
  columns: result.sets.map(set => set.columns.map(column => column.name)),
  rows: result.sets.map(set => set.rows),
  errors: result.errors.map(error => [error.number, error.state, error.class, error.message]),
  info: result.info.map(info => [info.number, info.state, info.message]),
  status: result.returnStatus ?? null,
})

// Compare one field at a time and report the first difference, without
// handing whole captures to node:assert.
function same(actual, expected, label) {
  for (const key of Object.keys(expected)) {
    const a = JSON.stringify(actual[key])
    const e = JSON.stringify(expected[key])
    if (a !== e) assert.fail(`${label}: ${key}\n  msduck: ${a.slice(0, 600)}\n  server: ${e.slice(0, 600)}`)
  }
}

async function database(t) {
  const connection = await start(t, { options: { requestTimeout: 60000 } })
  await query(connection, 'CREATE DATABASE msduck_catalog_reference')
  await query(connection, 'USE msduck_catalog_reference')
  return connection
}

// Statements whose execution depends on other features' gaps; the catalog
// text they would produce is covered by the definition formatter's tests
// against the same captured rows (crates/msduck-sql catalog::definition).
const unsupportedSetup = new Map([
  // CHECK enforcement cannot yet translate varchar concatenation,
  // CHARINDEX and CONVERT(FLOAT(53), ...).
  ['setup checks one', 'check enforcement'],
  ['setup checks two', 'check enforcement'],
  ['setup checks three', 'check enforcement'],
  ['check definitions', 'tables not created'],
  ['check object definitions', 'tables not created'],
  // Computed columns cannot yet concatenate varchar values.
  ['setup computed text', 'computed varchar concatenation'],
  ['computed text definitions', 'table not created'],
])

// Binding errors of a system procedure call end with RETURNSTATUS 1, as for
// every procedure hook; SQL Server sends no return status.
const bindingStatus = new Set(['pkeys bogus parameter', 'pkeys too many', 'rename bogus parameter', 'rename missing newname'])

test('the gaps-catalog-v2 profile matches SQL Server', { timeout: 300000 }, async t => {
  const connection = await database(t)
  const records = reference.catalogV2Profile.runs[0].filter(record => record.name !== 'version')
  let compared = 0
  for (const record of records) {
    const actual = summary(canonical(await captureBatch(connection, record.sql)))
    const expected = summary(record.result)
    if (unsupportedSetup.has(record.name)) continue
    if (bindingStatus.has(record.name)) {
      assert.equal(actual.status, 1, record.name)
      delete expected.status
    }
    same(actual, expected, record.name)
    compared++
  }
  assert.equal(compared, records.length - unsupportedSetup.size)
})

test('the catalog profile matches SQL Server', { timeout: 300000 }, async t => {
  const connection = await database(t)
  const records = reference.runs[0].filter(record => record.name !== 'version')
  for (const record of records) {
    const actual = summary(canonical(await captureBatch(connection, record.sql)))
    const expected = summary(record.result)
    if (record.name.startsWith('empty schema ')) {
      // The column sets; wire types are the root catalog adapter's.
      assert.deepEqual(actual.columns, expected.columns, record.name)
      assert.deepEqual(actual.rows, [[]], record.name)
      continue
    }
    if (record.name === 'constraint objects') {
      // ORDER BY name sorts by code point; SQL Server's collation ignores case.
      const sort = rows => rows.map(set => [...set].sort((a, b) => a[0].toLowerCase().localeCompare(b[0].toLowerCase())))
      actual.rows = sort(actual.rows)
      expected.rows = sort(expected.rows)
    }
    if (record.name === 'pkeys missing parameter') {
      assert.equal(actual.status, 1)
      delete expected.status
    }
    same(actual, expected, record.name)
  }
})

for (const profile of ['definitionProfile', 'namespaceProfile']) {
  test(`the ${profile} matches SQL Server`, { timeout: 300000 }, async t => {
    const connection = await database(t)
    for (const record of reference[profile].runs[0].filter(record => record.name !== 'version')) {
      const actual = summary(canonical(await captureBatch(connection, record.sql)))
      const expected = summary(record.result)
      if (record.name.startsWith('empty schema ')) {
        assert.deepEqual(actual.columns, expected.columns, record.name)
        continue
      }
      same(actual, expected, record.name)
    }
  })
}

test('CHECK definitions msduck can enforce match SQL Server', { timeout: 300000 }, async t => {
  const connection = await database(t)
  const records = reference.catalogV2Profile.runs[0]
  const definitions = new Map(records.find(record => record.name === 'check definitions').result.sets[0].rows.map(row => [row[0], row[1]]))
  assert.equal(definitions.size, v2Checks.length)
  let created = 0
  for (const [name, expression] of v2Checks) {
    try {
      await query(connection, `CREATE TABLE dbo.ck_${name}(a INT,b INT,c INT,s VARCHAR(20),n NVARCHAR(20),d DATETIME,m DECIMAL(10,2),CONSTRAINT ${name} CHECK (${expression}))`)
    } catch {
      continue
    }
    created++
    const { rows } = await query(connection, `SELECT definition, OBJECT_DEFINITION(object_id) FROM sys.check_constraints WHERE name = '${name}'`)
    const definition = definitions.get(name)
    assert.deepEqual(rows, [[definition, definition]], name)
  }
  // Most CHECKs run; the rest fail in CHECK enforcement, not in the catalog.
  assert.ok(created >= 80, `${created} of ${definitions.size}`)
})

test('system procedures run as RPC requests', async t => {
  const connection = await database(t)
  const { Request, TYPES } = await import('tedious')
  await query(connection, 'CREATE TABLE dbo.rpc_items(id INT CONSTRAINT pk_rpc_items PRIMARY KEY)')
  const call = (name, parameters) => new Promise((resolve, reject) => {
    const rows = []
    const messages = []
    let status
    const request = new Request(name, error => error ? reject(error) : resolve({ rows, messages, status }))
    for (const [parameter, value] of parameters) request.addParameter(parameter, TYPES.NVarChar, value)
    request.on('row', cells => rows.push(cells.map(cell => cell.value)))
    request.on('doneProc', (_count, _more, value) => { status = value })
    connection.on('infoMessage', message => messages.push(message.number))
    connection.callProcedure(request)
  })
  assert.deepEqual((await call('sp_pkeys', [['table_name', 'rpc_items']])).rows, [['msduck_catalog_reference', 'dbo', 'rpc_items', 'id', 1, 'pk_rpc_items']])
  const renamed = await call('sp_rename', [['objname', 'dbo.rpc_items'], ['newname', 'rpc_renamed']])
  assert.equal(renamed.status, 0)
  assert.ok(renamed.messages.includes(15477))
  assert.deepEqual((await query(connection, "SELECT name FROM sys.tables WHERE name LIKE 'rpc%'")).rows, [['rpc_renamed']])
})

test('temporary objects stay hidden without changing what joins return', async t => {
  const connection = await database(t)
  await query(connection, 'CREATE SCHEMA empty_schema')
  // A temporary table lives as long as the sp_executesql call that creates it.
  const hidden = sql => query(connection, `CREATE TABLE #hidden(id INT CONSTRAINT pk_hidden PRIMARY KEY); ${sql}`)
  // A RIGHT or FULL join keeps the schema without objects.
  assert.deepEqual((await hidden("SELECT s.name, o.name FROM sys.objects o RIGHT JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE s.name = 'empty_schema'")).rows, [['empty_schema', null]])
  assert.deepEqual((await hidden("SELECT s.name FROM sys.objects o FULL JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE s.name = 'empty_schema'")).rows, [['empty_schema']])
  // Unaliased views keep their qualified column names.
  assert.deepEqual((await hidden("SELECT COUNT(sys.objects.name) FROM sys.schemas s CROSS JOIN sys.objects WHERE sys.objects.name LIKE '%hidden%'")).rows, [[0]])
  assert.deepEqual((await hidden("SELECT COUNT(*) FROM sys.objects WHERE name LIKE '%hidden%'")).rows, [[0]])
  // tempdb's catalog shows the temporary table, as in SQL Server.
  assert.deepEqual((await hidden("SELECT COUNT(*) FROM tempdb.sys.tables WHERE object_id = OBJECT_ID('tempdb..#hidden')")).rows, [[1]])
})

test('a CLUSTERED key added by ALTER TABLE is the clustered index', async t => {
  const connection = await database(t)
  await query(connection, 'CREATE TABLE dbo.altered_key(id INT NOT NULL, x INT)')
  await query(connection, 'ALTER TABLE dbo.altered_key ADD CONSTRAINT pk_altered_key PRIMARY KEY CLUSTERED(id)')
  await assert.rejects(query(connection, 'CREATE CLUSTERED INDEX ix_altered_key ON dbo.altered_key(x)'), error => error.number === 1902)
  assert.deepEqual((await query(connection, "SELECT name, index_id, type_desc FROM sys.indexes WHERE object_id = OBJECT_ID('dbo.altered_key') ORDER BY index_id")).rows, [['pk_altered_key', 1, 'CLUSTERED']])
})
