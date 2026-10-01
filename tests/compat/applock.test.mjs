// Application locks through tedious (issue #713; docs/gaps-applock.md):
// several connections to one server contend for sp_getapplock locks.
// Expected values come from reference/gaps-applock.json.
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { Connection, Request } from 'tedious'
import { start } from '../support/client.mjs'

// One batch: rows, diagnostics and the procedure return statuses.
function run(connection, sql) {
  return new Promise((resolve, reject) => {
    const result = { rows: [], errors: [], info: [], statuses: [] }
    const onError = e => result.errors.push({ number: e.number, state: e.state, class: e.class, message: e.message })
    const onInfo = e => result.info.push({ number: e.number, state: e.state, class: e.class, message: e.message })
    connection.on('errorMessage', onError)
    connection.on('infoMessage', onInfo)
    const started = Date.now()
    const request = new Request(sql, (error, rowCount) => {
      connection.off('errorMessage', onError)
      connection.off('infoMessage', onInfo)
      result.elapsed = Date.now() - started
      result.rowCount = rowCount
      if (error && !result.errors.length) reject(error)
      else resolve(result)
    })
    request.on('row', row => result.rows.push(row.map(cell => cell.value)))
    request.on('returnStatus', status => result.statuses.push(status))
    connection.execSqlBatch(request)
  })
}

// Further connections to the server `start` launched for `first`.
async function another(t, first, database) {
  const connection = new Connection({ ...first.config, options: { ...first.config.options, database } })
  connection.on('error', () => {})
  t.after(() => connection.close())
  await new Promise((resolve, reject) => connection.connect(error => error ? reject(error) : resolve()))
  return connection
}

const get = (resource, mode, owner = 'Session', timeout = 0) =>
  `DECLARE @rc int; EXEC @rc = sp_getapplock @Resource = N'${resource}', @LockMode = '${mode}', @LockOwner = '${owner}', @LockTimeout = ${timeout}; SELECT @rc AS rc`
const release = (resource, owner = 'Session') =>
  `DECLARE @rc int; EXEC @rc = sp_releaseapplock @Resource = N'${resource}', @LockOwner = '${owner}'; SELECT @rc AS rc`
const status = async (connection, sql) => (await run(connection, sql)).rows.at(-1)[0]

async function setup(t, database) {
  const a = await start(t, { options: { requestTimeout: 30000 } })
  await run(a, `CREATE DATABASE ${database}`)
  await run(a, `USE ${database}`)
  const b = await another(t, a, database)
  return { a, b }
}

test('the workload repro takes a session lock that other connections respect', async t => {
  const { a, b } = await setup(t, 'applock_repro')
  const repro = await run(a, "EXEC sp_getapplock @Resource=N'foo', @LockMode='Exclusive', @LockOwner='Session', @LockTimeout=0;")
  assert.deepEqual(repro.errors, [])
  // Four DONEINPROC tokens of the procedure body carry a row count of one.
  assert.equal(repro.rowCount, 4)
  const positional = await run(a, "DECLARE @rc int; EXEC @rc = sp_getapplock N'foo', 'Exclusive', 'Session', 0; SELECT @rc AS rc, APPLOCK_MODE('public', 'foo', 'Session') AS m, APPLOCK_TEST('public', 'foo', 'Exclusive', 'Session') AS t")
  assert.deepEqual(positional.rows, [[0, 'Exclusive', 1]])
  assert.equal(await status(b, get('foo', 'Exclusive')), -1)
  const timed = await run(b, get('foo', 'Shared', 'Session', 400))
  assert.deepEqual(timed.rows, [[-1]])
  assert.ok(timed.elapsed >= 400, `waited ${timed.elapsed} ms`)
  assert.deepEqual((await run(b, "SELECT APPLOCK_TEST('public', 'foo', 'IntentShared', 'Session') AS t, APPLOCK_MODE('public', 'foo', 'Session') AS m")).rows, [[0, 'NoLock']])
  // Only the owner can release; two references need two releases.
  const foreign = await run(b, release('foo'))
  assert.deepEqual(foreign.rows, [[-999]])
  assert.deepEqual(foreign.errors.map(e => [e.number, e.state, e.class]), [[1223, 1, 16]])
  assert.equal(foreign.errors[0].message, "Cannot release the application lock (Database Principal: 'public', Resource: 'foo') because it is not currently held.")
  assert.equal(await status(a, release('foo')), 0)
  assert.equal(await status(b, get('foo', 'Exclusive')), -1)
  // A waiting request is granted after the release and reports 1.
  const waiting = run(b, get('foo', 'Exclusive', 'Session', 10000))
  await new Promise(resolve => setTimeout(resolve, 300))
  assert.equal(await status(a, release('foo')), 0)
  assert.deepEqual((await waiting).rows, [[1]])
  assert.equal(await status(a, get('foo', 'Exclusive')), -1)
})

test('validation follows sp_getapplock and sp_releaseapplock', async t => {
  const { a } = await setup(t, 'applock_validation')
  const mode = await run(a, "DECLARE @rc int; EXEC @rc = sp_getapplock N'x', 'Bogus', 'Session'; SELECT @rc AS rc, @@ERROR AS e")
  assert.deepEqual(mode.rows, [[-999, 0]])
  assert.deepEqual(mode.info.map(m => [m.number, m.state, m.class, m.message]), [[15625, 1, 0, "Option 'Bogus' not recognized for '@LockMode' parameter."]])
  const owner = await run(a, "DECLARE @rc int; EXEC @rc = sp_getapplock N'x', 'Shared', 'Bogus'; SELECT @rc AS rc")
  assert.deepEqual(owner.info.map(m => m.message), ["Option 'Bogus' not recognized for '@LockOwner' parameter."])
  const transaction = await run(a, "DECLARE @rc int; EXEC @rc = sp_getapplock N'x', 'Shared'; SELECT @rc AS rc")
  assert.deepEqual(transaction.rows, [[-999]])
  assert.deepEqual(transaction.info.map(m => m.number), [15626])
  for (const [sql, number] of [
    ["EXEC sp_getapplock NULL, 'Shared', 'Session'", 1224],
    ["EXEC sp_getapplock N'x', 'Shared', 'Session', -5", 1227],
    ["EXEC sp_getapplock N'x', 'Shared', 'Session', 0, 'nobody'", 1202],
    ["EXEC sp_releaseapplock N'x'", 3918],
    ["SELECT APPLOCK_MODE('public', 'x', 'Bogus')", 1226],
    ["SELECT APPLOCK_TEST('public', 'x', 'Bogus', 'Session')", 1225],
    ["SELECT APPLOCK_MODE('public', 'x', 'Transaction')", 3918],
  ]) {
    const result = await run(a, sql)
    assert.deepEqual(result.errors.map(e => e.number), [number], sql)
  }
})

test('transaction locks, unions and deadlocks', async t => {
  const { a, b } = await setup(t, 'applock_transactions')
  await run(a, `BEGIN TRAN; ${get('t', 'Shared', 'Transaction')}`)
  await run(a, get('t', 'IntentExclusive', 'Transaction'))
  assert.deepEqual((await run(a, "SELECT APPLOCK_MODE('public', 't', 'Transaction') AS m")).rows, [['SharedIntentExclusive']])
  assert.equal(await status(b, get('t', 'IntentShared')), 0)
  assert.equal(await status(b, release('t')), 0)
  assert.equal(await status(b, get('t', 'Shared')), -1)
  const waiting = run(b, get('t', 'Exclusive', 'Session', 10000))
  await new Promise(resolve => setTimeout(resolve, 300))
  await run(a, 'COMMIT')
  assert.deepEqual((await waiting).rows, [[1]])
  assert.equal(await status(b, release('t')), 0)

  // Each holds one lock and waits for the other's: one request is the
  // deadlock victim (-3) without an error, its transaction stays open, and
  // the other is granted once the victim rolls back. SQL Server's lock
  // monitor picks the victim by cost; msduck picks the longest waiter.
  await run(a, `BEGIN TRAN; ${get('d1', 'Exclusive', 'Transaction')}`)
  await run(b, `BEGIN TRAN; ${get('d2', 'Exclusive', 'Transaction')}`)
  const first = run(a, `${get('d2', 'Exclusive', 'Transaction', -1)}, @@TRANCOUNT AS c`)
  await new Promise(resolve => setTimeout(resolve, 300))
  const second = run(b, `${get('d1', 'Exclusive', 'Transaction', 10000)}, @@TRANCOUNT AS c`)
  assert.deepEqual((await first).rows, [[-3, 1]])
  await run(a, 'ROLLBACK')
  assert.deepEqual((await second).rows, [[1, 1]])
  await run(b, 'ROLLBACK')
})

test('closing a connection releases its session locks', async t => {
  const { a, b } = await setup(t, 'applock_disconnect')
  assert.equal(await status(a, get('s', 'Exclusive')), 0)
  const waiting = run(b, get('s', 'Exclusive', 'Session', 10000))
  await new Promise(resolve => setTimeout(resolve, 300))
  a.close()
  assert.deepEqual((await waiting).rows, [[1]])
})
