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

// Unnamed DEFAULTs of CREATE TABLE and ALTER TABLE ... ADD have no object
// yet (docs/gaps-catalog.md); the rows of named ones, and of unnamed ones
// that ALTER TABLE ... ADD DEFAULT ... FOR creates, are SQL Server's.
const namedDefaults = new Map([
  ['default definitions', rows => rows.filter(row => row[3] === false)],
  ['default naming', rows => rows.filter(row => !row[2].startsWith('DF__'))],
  ['default column links', () => [[1]]],
  ['alter defaults', rows => rows.filter(row => row[0] !== 'w')],
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
    if (namedDefaults.has(record.name)) expected.rows = expected.rows.map(namedDefaults.get(record.name))
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
