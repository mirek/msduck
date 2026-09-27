// Tedious Connection.reset() sends RESETCONNECTION (0x08) on its next batch.
// Expectations follow docs/session-reset-reference.md; see docs/session-reset.md.
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { Connection, Request, TYPES } from 'tedious'
import { start, query } from './support/client.mjs'

// execSql runs through sp_executesql, which scopes SET options to the call;
// session settings must be set with a plain SQL batch.
function batch(connection, sql) {
  return new Promise((resolve, reject) => {
    const rows = []
    const request = new Request(sql, (error, rowCount) => error ? reject(error) : resolve({ rows, rowCount }))
    request.on('row', cells => rows.push(cells.map(cell => cell.value)))
    connection.execSqlBatch(request)
  })
}
function reset(connection) {
  let acknowledged = false
  connection.once('resetConnection', () => { acknowledged = true })
  return new Promise((resolve, reject) => connection.reset(error => error ? reject(error) : resolve(acknowledged)))
}
const state = 'SELECT @@TRANCOUNT, XACT_STATE(), @@DATEFIRST'

test('reset restores session settings and keeps the login', { timeout: 30000 }, async t => {
  const c = await start(t, { authentication: { type: 'default', options: { userName: 'pool_user', password: 'x' } } })
  await batch(c, 'SET DATEFIRST 3; SET NOCOUNT ON; CREATE TABLE dbo.reset_settings (n INT)')
  assert.deepEqual((await batch(c, state)).rows, [[0, 0, 3]])
  assert.equal((await batch(c, 'INSERT dbo.reset_settings VALUES (1)')).rowCount, 0)
  assert.equal(await reset(c), true, 'ENVCHANGE type 18 acknowledges the reset')
  assert.deepEqual((await batch(c, state)).rows, [[0, 0, 7]])
  assert.equal((await batch(c, 'INSERT dbo.reset_settings VALUES (2)')).rowCount, 1)
  assert.deepEqual((await query(c, 'SELECT ORIGINAL_LOGIN()')).rows, [['pool_user']])
  assert.deepEqual((await query(c, 'SELECT 42 AS answer')).rows, [[42]])
})

test('reset rolls back an open transaction for every connection', { timeout: 30000 }, async t => {
  const c = await start(t)
  await batch(c, 'CREATE TABLE dbo.reset_marker (n INT)')
  await batch(c, 'BEGIN TRANSACTION; INSERT dbo.reset_marker VALUES (9)')
  assert.deepEqual((await batch(c, `${state}; SELECT n FROM dbo.reset_marker`)).rows, [[1, 1, 7], [9]])
  await reset(c)
  assert.deepEqual((await batch(c, `${state}; SELECT COUNT(*) FROM dbo.reset_marker`)).rows, [[0, 0, 7], [0]])
  const other = new Connection(c.config)
  other.on('error', () => {})
  t.after(() => other.close())
  await new Promise((resolve, reject) => other.connect(error => error ? reject(error) : resolve()))
  assert.deepEqual((await query(other, 'SELECT COUNT(*) FROM dbo.reset_marker')).rows, [[0]])
  // The rollback ENVCHANGE cleared the client's descriptor; new transactions work.
  await batch(c, 'BEGIN TRANSACTION; INSERT dbo.reset_marker VALUES (10); COMMIT')
  assert.deepEqual((await query(c, 'SELECT n FROM dbo.reset_marker')).rows, [[10]])
  assert.deepEqual((await query(c, 'SELECT 43 AS answer')).rows, [[43]])
})

test('reset invalidates prepared handles and survives repeated resets', { timeout: 30000 }, async t => {
  const c = await start(t)
  const request = new Request('SELECT @p + 1 AS answer', () => {})
  request.addParameter('p', TYPES.Int, undefined)
  await new Promise((resolve, reject) => { request.once('prepared', resolve); request.once('error', reject); c.prepare(request) })
  const execute = () => new Promise(resolve => {
    const rows = []
    request.removeAllListeners('row')
    request.on('row', cells => rows.push(cells.map(cell => cell.value)))
    request.callback = error => resolve({ rows, number: error?.number })
    c.execute(request, { p: 41 })
  })
  assert.deepEqual(await execute(), { rows: [[42]], number: undefined })
  await reset(c)
  assert.deepEqual(await execute(), { rows: [], number: 8179 })
  for (const answer of [44, 45]) {
    await reset(c)
    assert.deepEqual((await query(c, `SELECT ${answer} AS answer`)).rows, [[answer]])
  }
})
