// Computed columns over Unicode and JSON expressions, and column DEFAULTs that
// read session state (issue #718, docs/gaps-computed.md). Every case of
// scripts/capture-gaps-computed.mjs runs against msduck and is compared with
// reference/gaps-computed.json, captured from SQL Server. The differences
// listed in `known` are asserted as msduck's current behavior.
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { Connection, TYPES } from 'tedious'
import { start, query } from '../support/client.mjs'
import { capture, canonical } from '../../scripts/lib/compatibility.mjs'
import { cases, client } from '../../scripts/capture-gaps-computed.mjs'

const reference = JSON.parse(readFileSync(new URL('../../reference/gaps-computed.json', import.meta.url), 'utf8'))

const errors = result => result.errors.map(e => [e.number, e.state, e.class, e.message])
const rows = result => result.sets.map(set => set.rows)
const columns = result => result.sets.map(set => set.columns.map(c => [c.name, c.type, c.length]))

// Differences from SQL Server outside this task's scope, by case and step.
const known = {
  // DuckDB verifies the persisted expression during the write and reports
  // its own constraint error (SQL Server: 13609, state 2).
  'json-persisted#8': local => {
    assert.equal(local.errors.length, 1)
    assert.equal(local.errors[0].number, 50000)
    assert.match(local.errors[0].message, /^Constraint Error: Incorrect value for generated column "value /)
  },
  // msduck's general conversion error (SQL Server: 8114, state 5).
  'session-default-conversions#7': local => {
    assert.deepEqual(errors(local).map(e => e[0]), [245])
  },
  // A missing INSERT target and a missing SELECT source fail with msduck's
  // own diagnostics, as for any table.
  'session-default-variant#2': local => assert.equal(local.errors[0].number, 207),
  'session-default-variant#3': local => assert.equal(local.errors[0].number, 208),
  // A bare SESSION_CONTEXT DEFAULT on a sql_variant column is refused.
  'session-default-variant#4': local => {
    assert.deepEqual(errors(local), [[40515, 1, 16, 'unsupported SESSION_CONTEXT outside an explicit CAST or CONVERT in a DEFAULT']])
  },
  'session-default-variant#5': local => assert.equal(local.errors[0].number, 207),
  'session-default-variant#6': local => assert.equal(local.errors[0].number, 208),
}

let databases = 0
async function fresh(t, options = {}) {
  const connection = await start(t, { options: { ...client, ...options } })
  const name = `computed_${process.pid}_${databases++}`
  await query(connection, `CREATE DATABASE ${name}`)
  await query(connection, `USE ${name}`)
  return { connection, name }
}

for (const [id, statements] of cases) {
  test(`${id} matches SQL Server`, async t => {
    const expected = reference.cases.find(c => c.id === id)
    assert.ok(expected, `reference case ${id}`)
    const { connection } = await fresh(t)
    for (const [index, sql] of statements.entries()) {
      const local = canonical(await capture(connection, sql))
      const remote = expected.steps[index].result
      assert.equal(expected.steps[index].sql, sql)
      const difference = known[`${id}#${index}`]
      if (difference) {
        difference(local, remote)
        continue
      }
      assert.deepEqual(errors(local), errors(remote), sql)
      assert.deepEqual(rows(local), rows(remote), sql)
      // Catalog view descriptors are outside this task (docs/gaps-computed.md).
      if (!/sys\.columns/.test(sql)) assert.deepEqual(columns(local), columns(remote), sql)
    }
  })
}

// Another session on the server `connection` uses.
async function connect(t, connection, options) {
  const { server, authentication, options: base } = connection.config
  const other = new Connection({ server, authentication, options: { port: base.port, encrypt: false, connectTimeout: 5000, requestTimeout: 5000, ...options } })
  t.after(() => other.close())
  other.on('error', () => {})
  await new Promise((resolve, reject) => other.connect(error => error ? reject(error) : resolve()))
  return other
}

const sessionTable = "CREATE TABLE items (id int NOT NULL, tenant nvarchar(50) NULL DEFAULT (CONVERT(nvarchar(50), SESSION_CONTEXT(N'tenant'))), who nvarchar(128) NULL DEFAULT (SUSER_SNAME()), host nvarchar(128) NULL DEFAULT (HOST_NAME()), app nvarchar(128) NULL DEFAULT (APP_NAME()))"

test('defaults read the session that inserts, including RPC and after reset', async t => {
  const { connection: a, name } = await fresh(t, { workstationId: 'host-a', appName: 'app-a' })
  const b = await connect(t, a, { database: name, workstationId: 'host-b', appName: 'app-b' })
  await query(a, sessionTable)
  await query(a, "EXEC sp_set_session_context N'tenant', N'alpha'")
  await query(b, "EXEC sp_set_session_context N'tenant', N'\u{1F986}beta'")
  await query(a, 'INSERT items (id) VALUES (1)')
  await query(b, 'INSERT items (id) VALUES (2)')
  // sp_executesql binds the same defaults in the calling session.
  await query(b, 'INSERT items (id) VALUES (@id)', [['id', TYPES.Int, 3]])
  await query(a, 'INSERT items (id) SELECT @id', [['id', TYPES.Int, 4]])
  // RESETCONNECTION starts with an empty session context.
  await new Promise((resolve, reject) => a.reset(error => error ? reject(error) : resolve()))
  await query(a, `USE ${name}`)
  await query(a, 'INSERT items (id) VALUES (5)')
  const { rows: result } = await query(a, 'SELECT id, tenant, who, host, app FROM items ORDER BY id')
  assert.deepEqual(result, [
    [1, 'alpha', 'sa', 'host-a', 'app-a'],
    [2, '\u{1F986}beta', 'sa', 'host-b', 'app-b'],
    [3, '\u{1F986}beta', 'sa', 'host-b', 'app-b'],
    [4, 'alpha', 'sa', 'host-a', 'app-a'],
    [5, null, 'sa', 'host-a', 'app-a'],
  ])
  const { rows: names } = await query(b, 'SELECT HOST_NAME(), APP_NAME(), SUSER_SNAME()')
  assert.deepEqual(names, [['host-b', 'app-b', 'sa']])
  // A view reads the session that queries it.
  await query(b, 'CREATE VIEW client_names AS SELECT HOST_NAME() AS host, APP_NAME() AS app')
  assert.deepEqual((await query(a, 'SELECT host, app FROM client_names')).rows, [['host-a', 'app-a']])
  assert.deepEqual((await query(b, 'SELECT host, app FROM client_names')).rows, [['host-b', 'app-b']])
})

test('computed columns over Unicode columns are filtered, indexed and reported', async t => {
  const { connection } = await fresh(t)
  await query(connection, "CREATE TABLE people (id int NOT NULL, first nvarchar(50) NULL, last nvarchar(50) NULL, profile nvarchar(max) NULL, display AS (first + N' ' + last) PERSISTED, city AS CONVERT(nvarchar(60), JSON_VALUE(profile, N'$.city')))")
  await query(connection, `INSERT people (id, first, last, profile) VALUES (1, N'Zoë', N'Łukasz', N'{"city":"Gdańsk"}'), (2, N'Ann', N'Lee', N'{"city":"Oslo"}'), (3, N'Bo', NULL, NULL)`)
  await query(connection, 'CREATE INDEX ix_people_display ON people(display)')
  await query(connection, 'CREATE INDEX ix_people_city ON people(city)')
  const { rows: found, columns: [metadata] } = await query(connection, "SELECT id, display, city FROM people WHERE city = N'Gdańsk' OR display = N'Ann Lee' ORDER BY id")
  assert.deepEqual(found, [[1, 'Zoë Łukasz', 'Gdańsk'], [2, 'Ann Lee', 'Oslo']])
  assert.deepEqual(metadata.map(c => [c.colName, c.type.name, c.dataLength ?? null]), [['id', 'Int', null], ['display', 'NVarChar', 202], ['city', 'NVarChar', 120]])
  const { rows: catalog } = await query(connection, "SELECT name, is_computed, COLUMNPROPERTY(object_id, name, 'IsComputed') FROM sys.columns WHERE object_id = OBJECT_ID('people') ORDER BY column_id")
  assert.deepEqual(catalog, [['id', false, 0], ['first', false, 0], ['last', false, 0], ['profile', false, 0], ['display', true, 1], ['city', true, 1]])
  const { rows: missing } = await query(connection, 'SELECT id FROM people WHERE display IS NULL')
  assert.deepEqual(missing, [[3]])
})
