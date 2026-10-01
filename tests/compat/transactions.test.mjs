// Isolation levels, savepoints, WAITFOR and DBCC USEROPTIONS through tedious
// (issue #727). Expected values come from reference/savepoint.json and
// reference/gaps-transactions.json; see docs/gaps-transactions.md.
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { Connection, ISOLATION_LEVEL, Request, TYPES } from 'tedious'
import { start, query } from '../support/client.mjs'

function transaction(connection, operation, ...args) {
  return new Promise((resolve, reject) => connection[operation](error => error ? reject(error) : resolve(), ...args))
}

// A second connection to the same server.
async function another(t, c) {
  const connection = new Connection(c.config)
  t.after(() => connection.close())
  connection.on('error', () => {})
  await new Promise((resolve, reject) => connection.connect(error => error ? reject(error) : resolve()))
  return connection
}

const level = async c => (await query(c, 'SELECT transaction_isolation_level FROM sys.dm_exec_sessions WHERE session_id = @@SPID')).rows[0][0]

test('the driver begins SERIALIZABLE and every other isolation level', async t => {
  const c = await start(t)
  assert.equal(await level(c), 2)
  for (const [name, value] of [['READ_UNCOMMITTED', 1], ['READ_COMMITTED', 2], ['REPEATABLE_READ', 3], ['SERIALIZABLE', 4], ['SNAPSHOT', 5]]) {
    await transaction(c, 'beginTransaction', '', ISOLATION_LEVEL[name])
    assert.deepEqual((await query(c, 'SELECT @@TRANCOUNT')).rows, [[1]], name)
    // The level chosen at begin stays the session's level, as in SQL Server.
    assert.equal(await level(c), value, name)
    await transaction(c, 'commitTransaction')
    assert.equal(await level(c), value, name)
  }
})

test('SET TRANSACTION ISOLATION LEVEL is accepted, reported and scoped to sp_executesql', async t => {
  const c = await start(t)
  for (const [sql, value, label] of [
    ['READ UNCOMMITTED', 1, 'read uncommitted'], ['REPEATABLE READ', 3, 'repeatable read'],
    ['SNAPSHOT', 5, 'snapshot'], ['SERIALIZABLE', 4, 'serializable'], ['READ COMMITTED', 2, 'read committed']
  ]) {
    await query(c, `SET TRANSACTION ISOLATION LEVEL ${sql}`)
    assert.equal(await level(c), value, sql)
    const options = await query(c, 'DBCC USEROPTIONS')
    assert.deepEqual(options.rows.at(-1), ['isolation level', label], sql)
    assert.deepEqual(options.columns[0].map(column => [column.colName, column.type.name, column.dataLength]), [['Set Option', 'NVarChar', 256], ['Value', 'NVarChar', 92]])
  }
  assert.deepEqual((await query(c, 'DBCC USEROPTIONS')).rows, [
    ['textsize', '2147483647'], ['language', 'us_english'], ['dateformat', 'mdy'], ['datefirst', '7'],
    ['lock_timeout', '-1'], ['quoted_identifier', 'SET'], ['arithabort', 'SET'], ['ansi_null_dflt_on', 'SET'],
    ['ansi_warnings', 'SET'], ['ansi_padding', 'SET'], ['ansi_nulls', 'SET'], ['concat_null_yields_null', 'SET'],
    ['isolation level', 'read committed']
  ])
  // A level set inside sp_executesql applies inside it and reverts after.
  const inner = await query(c, 'SET TRANSACTION ISOLATION LEVEL SERIALIZABLE; SELECT transaction_isolation_level FROM sys.dm_exec_sessions WHERE session_id = @@SPID AND @p = 1', [['p', TYPES.Int, 1]])
  assert.deepEqual(inner.rows, [[4]])
  assert.equal(await level(c), 2)
})

test('SQL savepoints undo only later work and keep @@TRANCOUNT', async t => {
  const c = await start(t)
  await query(c, 'CREATE TABLE dbo.svp(id INT IDENTITY(1,1) CONSTRAINT pk_svp PRIMARY KEY, v INT NOT NULL)')
  await query(c, 'BEGIN TRANSACTION; INSERT dbo.svp(v) VALUES (1); SAVE TRANSACTION s1; INSERT dbo.svp(v) VALUES (2); UPDATE dbo.svp SET v = 10 WHERE id = 1')
  assert.deepEqual((await query(c, 'SELECT id, v FROM dbo.svp ORDER BY id')).rows, [[1, 10], [2, 2]])
  assert.deepEqual((await query(c, 'ROLLBACK TRANSACTION s1; SELECT @@TRANCOUNT, XACT_STATE()')).rows, [[1, 1]])
  assert.deepEqual((await query(c, 'SELECT id, v FROM dbo.svp ORDER BY id')).rows, [[1, 1]])
  await assert.rejects(query(c, 'ROLLBACK TRANSACTION s1'), error => error.number === 6401 && error.message === 'Cannot roll back s1. No transaction or savepoint of that name was found.')
  assert.deepEqual((await query(c, 'INSERT dbo.svp(v) VALUES (3); COMMIT; SELECT id, v FROM dbo.svp ORDER BY id')).rows, [[1, 1], [3, 3]])
  await assert.rejects(query(c, 'SAVE TRANSACTION nope'), error => error.number === 628 && error.message === 'Cannot issue SAVE TRANSACTION when there is no active transaction.')
  await assert.rejects(query(c, `SAVE TRANSACTION ${'n'.repeat(33)}`), error => error.number === 103)
  assert.deepEqual((await query(c, 'SELECT @@TRANCOUNT')).rows, [[0]])
})

test('transaction-manager savepoints share the SQL namespace', async t => {
  const c = await start(t)
  await query(c, 'CREATE TABLE dbo.tm(n INT)')
  await transaction(c, 'beginTransaction', 'outer', ISOLATION_LEVEL.SERIALIZABLE)
  await query(c, 'INSERT dbo.tm VALUES (1)')
  await transaction(c, 'saveTransaction', 'point')
  await query(c, 'INSERT dbo.tm VALUES (2)')
  await transaction(c, 'rollbackTransaction', 'POINT')
  assert.deepEqual((await query(c, 'SELECT n FROM dbo.tm; SELECT @@TRANCOUNT')).rows, [[1], [1]])
  await assert.rejects(transaction(c, 'rollbackTransaction', 'point'), error => error.number === 6401)
  await query(c, 'SAVE TRANSACTION from_sql; INSERT dbo.tm VALUES (3)')
  await transaction(c, 'rollbackTransaction', 'from_sql')
  await transaction(c, 'commitTransaction')
  assert.deepEqual((await query(c, 'SELECT n FROM dbo.tm; SELECT @@TRANCOUNT')).rows, [[1], [0]])
  await assert.rejects(transaction(c, 'saveTransaction', 'outside'), error => error.number === 628)
})

test('WAITFOR DELAY and TIME wait, validate and serve polling loops', async t => {
  const c = await start(t)
  let started = Date.now()
  await query(c, "WAITFOR DELAY '00:00:00.001'")
  await query(c, "WAITFOR DELAY '00:00:00.300'")
  assert.ok(Date.now() - started >= 300)
  started = Date.now()
  await query(c, "DECLARE @d VARCHAR(12) = '00:00:00.200'; WAITFOR DELAY @d")
  assert.ok(Date.now() - started >= 200)
  // A server-side delayed polling loop sees another session's commit.
  await query(c, 'CREATE TABLE dbo.jobs(id INT, done BIT); INSERT dbo.jobs VALUES (1, 0)')
  const other = await another(t, c)
  started = Date.now()
  const poll = query(c, "DECLARE @tries INT = 0; WHILE NOT EXISTS (SELECT 1 FROM dbo.jobs WHERE done = 1) AND @tries < 200 BEGIN WAITFOR DELAY '00:00:00.050'; SET @tries += 1 END; SELECT @tries")
  await new Promise(resolve => setTimeout(resolve, 400))
  await query(other, 'UPDATE dbo.jobs SET done = 1')
  const { rows } = await poll
  assert.ok(rows[0][0] > 0 && rows[0][0] < 200, `polled ${rows[0][0]} times`)
  assert.ok(Date.now() - started >= 400)
  started = Date.now()
  await query(c, 'DECLARE @t INT = DATEDIFF(SECOND, CAST(CAST(GETDATE() AS DATE) AS DATETIME), GETDATE()) + 1; WAITFOR TIME @t')
  assert.ok(Date.now() - started < 3000)
  await assert.rejects(query(c, "WAITFOR DELAY 'abc'"), error => error.number === 148 && error.message === "Incorrect time syntax in time string 'abc' used with WAITFOR.")
  await assert.rejects(query(c, "DECLARE @d VARCHAR(20) = 'abc'; WAITFOR DELAY @d"), error => error.number === 241)
  await assert.rejects(query(c, 'DECLARE @d BIGINT = 1; WAITFOR DELAY @d'), error => error.number === 9815 && error.message === 'Waitfor delay and waitfor time cannot be of type bigint.')
})

test('a cancelled WAITFOR completes the request and keeps the connection usable', async t => {
  const c = await start(t, { options: { requestTimeout: 15000 } })
  const error = await new Promise(resolve => {
    const request = new Request("WAITFOR DELAY '00:00:01'", error => resolve(error))
    c.execSql(request)
    setTimeout(() => c.cancel(), 100)
  })
  assert.equal(error?.code, 'ECANCEL')
  assert.deepEqual((await query(c, 'SELECT 1')).rows, [[1]])
})

// Replay reference/gaps-transactions.json (SQL Server 2022, captured by
// scripts/capture-gaps-transactions.mjs) in a fresh database. Rows, errors
// and whether each WAITFOR waited must match exactly.
test('replays the captured isolation and WAITFOR reference', { timeout: 120000 }, async t => {
  const { readFile } = await import('node:fs/promises')
  const { capture, canonical } = await import('../../scripts/lib/compatibility.mjs')
  const fixture = JSON.parse(await readFile(new URL('../../reference/gaps-transactions.json', import.meta.url), 'utf8'))
  const admin = await start(t)
  await query(admin, 'CREATE DATABASE gaps_transactions')
  const c = new Connection({ ...admin.config, options: { ...admin.config.options, database: 'gaps_transactions', requestTimeout: 30000 } })
  t.after(() => c.close())
  c.on('error', () => {})
  await new Promise((resolve, reject) => c.connect(error => error ? reject(error) : resolve()))
  // Differences owned by other features, retained exactly so that a change
  // is noticed: EXEC sp_executesql inside a SQL batch, the engine's message
  // for an undeclared variable and SQL_VARIANT variables.
  const unsupportedExec = { rows: [], errors: [[40515, 1, 16, 'unsupported T-SQL statement']] }
  const known = {
    'sp_executesql scope': unsupportedExec,
    'waitfor delay through sp_executesql': { ...unsupportedExec, waited: { atLeast: false, under: true } },
    'waitfor undeclared variable': { rows: [], errors: [[137, 1, 16, 'Must declare the scalar variable @undeclared']] },
    'waitfor type SQL_VARIANT': { rows: [], errors: [[50000, 1, 16, 'SQL_VARIANT variables are not yet supported']] }
  }
  const differences = []
  for (const expected of fixture.run) {
    if (expected.name === 'server identity') continue
    if (expected.request) {
      const error = await new Promise(resolve => c[expected.request](error => resolve(error), ...expected.args))
      const actual = error ? { number: error.number ?? null, message: error.message } : null
      if (JSON.stringify(actual) !== JSON.stringify(expected.error)) differences.push({ name: expected.name, actual, expected: expected.error })
      continue
    }
    const started = Date.now()
    const result = canonical(await capture(c, expected.sql))
    const elapsed = Date.now() - started
    const shape = r => ({
      rows: r.sets.map(set => set.rows),
      errors: r.errors.map(e => [e.number, e.state, e.class, e.message])
    })
    const actual = shape(result)
    const reference = shape(expected.result)
    if (expected.wait) {
      actual.waited = { atLeast: elapsed >= expected.wait.atLeast, under: elapsed < expected.wait.under }
      reference.waited = expected.waited
    }
    if (JSON.stringify(actual) !== JSON.stringify(known[expected.name] ?? reference)) {
      differences.push({ name: expected.name, actual, reference })
    }
  }
  assert.deepEqual(differences, [])
})
