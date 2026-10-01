// ALTER DATABASE SET READ_COMMITTED_SNAPSHOT and user access with ROLLBACK
// IMMEDIATE, SINGLE_USER admission, sys.dm_exec_sessions and @@SPID.
// Expected tokens come from reference/alter-database-sessions.json (SQL Server
// 2025, see docs/alter-database-sessions.md).
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { Connection, Request } from 'tedious'
import { start } from './support/client.mjs'

const reference = JSON.parse(readFileSync(new URL('../reference/alter-database-sessions.json', import.meta.url), 'utf8')).runs[0]
const observed = name => {
  const found = reference.find(item => item.name === name)
  assert(found, `missing reference observation ${name}`)
  return found.result ?? found
}
const messages = list => list.map(m => [m.number, m.state, m.class, m.message])
const client = { appName: 'msduck-capture', workstationId: 'msduck-host' }

// One batch: rows, descriptors, INFO and ERROR tokens. Never rejects.
function run(connection, sql) {
  return new Promise(resolve => {
    const result = { rows: [], columns: [], infos: [], errors: [] }
    const onInfo = m => result.infos.push(m)
    const onError = m => result.errors.push(m)
    connection.on('infoMessage', onInfo)
    connection.on('errorMessage', onError)
    const request = new Request(sql, error => {
      connection.off('infoMessage', onInfo)
      connection.off('errorMessage', onError)
      if (error && !result.errors.length) result.transport = error.message
      result.infos = messages(result.infos)
      result.errors = messages(result.errors)
      resolve(result)
    })
    request.on('columnMetadata', columns => result.columns.push(columns.map(c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, flags: c.flags }))))
    request.on('row', cells => result.rows.push(cells.map(cell => cell.value)))
    try { connection.execSqlBatch(request) } catch (error) { result.transport = error.message; resolve(result) }
  })
}
async function open(c, database, options = {}) {
  const connection = new Connection({ ...c.config, options: { ...c.config.options, database, ...client, ...options } })
  const events = { end: false, error: null, tokens: [] }
  connection.on('end', () => { events.end = true })
  connection.on('error', () => {})
  connection.on('errorMessage', m => events.tokens.push(m))
  events.error = await new Promise(resolve => connection.connect(resolve))
  events.tokens = messages(events.tokens)
  return { connection, events }
}
async function closed(events) {
  for (let i = 0; i < 100 && !events.end; i++) await new Promise(resolve => setTimeout(resolve, 50))
  return events.end
}
const state = (c, name) => run(c, `SELECT name,user_access,user_access_desc,is_read_committed_snapshot_on FROM sys.databases WHERE name=N'${name}'`)

test('the owner sequence terminates sessions and drops the database', { timeout: 60000 }, async t => {
  const c = await start(t, { options: client })
  assert.deepEqual((await run(c, 'CREATE DATABASE [probe_db]')).errors, [])
  const idle = await open(c, 'probe_db')
  const busy = await open(c, 'probe_db')
  assert.equal(idle.events.error, undefined)
  await run(busy.connection, 'CREATE TABLE dbo.t(id INT); BEGIN TRANSACTION; INSERT INTO dbo.t VALUES(1)')
  const rcsi = await run(c, 'alter database [probe_db] set read_committed_snapshot on with rollback immediate;')
  assert.deepEqual(rcsi.errors, [])
  assert.deepEqual(rcsi.infos, messages(observed('rollback immediate with users').info))
  assert.equal(await closed(idle.events), true)
  assert.equal(await closed(busy.events), true)
  // The terminated session's transaction was rolled back.
  // msduck reads another database's tables only after USE.
  const kept = await run(c, 'USE [probe_db]; SELECT COUNT(*) FROM dbo.t; USE [master]')
  assert.deepEqual(kept.errors, [])
  assert.deepEqual(kept.rows, [[0]])
  assert.deepEqual((await state(c, 'probe_db')).rows, observed('rollback immediate with users state').sets[0].rows.map(row => row.slice(0, 4)))

  const program = await run(c, 'select program_name from sys.dm_exec_sessions where session_id = @@spid;')
  assert.deepEqual(program.rows, observed('owner program_name query').sets[0].rows)
  assert.deepEqual(program.columns, [observed('owner program_name query').sets[0].columns.map(({ name, type, length, flags }) => ({ name, type, length, flags }))])

  const another = await open(c, 'probe_db')
  const single = await run(c, 'alter database [probe_db] set single_user with rollback immediate;')
  assert.deepEqual(single.infos, messages(observed('owner single_user with users').info))
  assert.equal(await closed(another.events), true)
  assert.deepEqual((await state(c, 'probe_db')).rows, [['probe_db', 1, 'SINGLE_USER', true]])
  assert.deepEqual(await run(c, 'drop database [probe_db];'), { rows: [], columns: [], infos: [], errors: [] })
  assert.deepEqual((await state(c, 'probe_db')).rows, [])
})

test('the owner batch runs as one request', { timeout: 60000 }, async t => {
  const c = await start(t, { options: client })
  await run(c, 'CREATE DATABASE [probe_db]')
  const idle = await open(c, 'probe_db')
  const result = await run(c, 'alter database [probe_db] set read_committed_snapshot on with rollback immediate; select program_name from sys.dm_exec_sessions where session_id = @@spid; alter database [probe_db] set single_user with rollback immediate; drop database [probe_db];')
  assert.deepEqual(result.errors, [])
  assert.deepEqual(result.rows, [['msduck-capture']])
  // SQL Server also sends the 5060 pair for the second ALTER, after enabling
  // READ_COMMITTED_SNAPSHOT; msduck has no session left to terminate then.
  assert.deepEqual(result.infos, messages(observed('rollback immediate with users').info))
  assert.equal(await closed(idle.events), true)
})

test('the session that sets SINGLE_USER holds the database', { timeout: 60000 }, async t => {
  const c = await start(t, { options: client })
  await run(c, 'CREATE DATABASE [probe_db]')
  assert.deepEqual((await run(c, 'ALTER DATABASE [probe_db] SET SINGLE_USER WITH ROLLBACK IMMEDIATE')).errors, [])
  const refused = await open(c, 'probe_db')
  assert.equal(refused.events.error?.code, 'ELOGIN')
  assert.deepEqual(refused.events.tokens.map(m => m[0]), observed('second login refused').tokens.map(m => m.number))
  assert.deepEqual(refused.events.tokens[0], [4060, 1, 11, 'Cannot open database "probe_db" requested by the login. The login failed.'])
  const other = await open(c, 'master')
  assert.deepEqual((await run(other.connection, 'USE [probe_db]')).errors, messages(observed('use refused').errors))
  assert.deepEqual((await run(other.connection, 'SELECT DB_NAME()')).rows, [['master']])
  assert.deepEqual((await run(other.connection, 'DROP DATABASE [probe_db]')).errors, messages(observed('drop by other while held').errors))
  assert.deepEqual((await run(other.connection, 'ALTER DATABASE [probe_db] SET MULTI_USER WITH NO_WAIT')).errors, messages(observed('alter by other while held').errors))
  const own = await run(c, 'USE [probe_db]; SELECT DB_NAME(); USE [master]')
  assert.deepEqual(own.errors, [])
  assert.deepEqual(own.rows, [['probe_db']])
  assert.deepEqual((await run(c, 'drop database [probe_db];')).errors, [])

  // When the holder disconnects, the first session to enter holds it.
  await run(c, 'CREATE DATABASE [probe_db]')
  const issuer = await open(c, 'master')
  await run(issuer.connection, 'ALTER DATABASE [probe_db] SET SINGLE_USER')
  await new Promise(resolve => { issuer.connection.once('end', resolve); issuer.connection.close() })
  for (let i = 0; i < 100 && (await run(c, 'SELECT 1')).errors.length === 0; i++) {
    const holder = await open(c, 'probe_db')
    if (!holder.events.error) {
      assert.equal((await open(c, 'probe_db')).events.error?.code, 'ELOGIN')
      assert.deepEqual((await run(c, 'ALTER DATABASE [probe_db] SET MULTI_USER WITH NO_WAIT')).errors, messages(observed('multi_user no_wait while held').errors))
      holder.connection.close()
      break
    }
    await new Promise(resolve => setTimeout(resolve, 50))
  }
  for (let i = 0; i < 100; i++) {
    if ((await run(c, 'DROP DATABASE [probe_db]')).errors.length === 0) break
    await new Promise(resolve => setTimeout(resolve, 50))
  }
  assert.deepEqual((await state(c, 'probe_db')).rows, [])
})

test('options change sys.databases and refusals match SQL Server', { timeout: 60000 }, async t => {
  const c = await start(t, { options: client })
  await run(c, 'CREATE DATABASE [probe_db]')
  assert.deepEqual((await state(c, 'probe_db')).rows, observed('initial state').sets[0].rows.map(row => row.slice(0, 4)))
  assert.deepEqual((await state(c, 'probe_db')).columns[0], observed('initial state').sets[0].columns.slice(0, 4).map(({ name, type, length, flags }) => ({ name, type, length, flags })))
  for (const [sql, name] of [
    ['ALTER DATABASE [probe_db] SET READ_COMMITTED_SNAPSHOT ON', 'rcsi on state'],
    ['ALTER DATABASE probe_db SET READ_COMMITTED_SNAPSHOT OFF WITH NO_WAIT', 'rcsi off state'],
    ['ALTER DATABASE [probe_db] SET SINGLE_USER', 'single_user state'],
    ['ALTER DATABASE [probe_db] SET RESTRICTED_USER WITH ROLLBACK IMMEDIATE', 'restricted_user state'],
    ['ALTER DATABASE [probe_db] SET MULTI_USER', 'multi_user state'],
    ['ALTER DATABASE [probe_db] SET SINGLE_USER, READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE', 'two options state'],
  ]) {
    assert.deepEqual(await run(c, sql), { rows: [], columns: [], infos: [], errors: [] }, sql)
    assert.deepEqual((await state(c, 'probe_db')).rows, observed(name).sets[0].rows.map(row => row.slice(0, 4)), sql)
  }
  await run(c, 'ALTER DATABASE [probe_db] SET MULTI_USER; ALTER DATABASE [probe_db] SET READ_COMMITTED_SNAPSHOT OFF')
  for (const name of ['unknown database', 'unknown database rcsi', 'master rcsi', 'master single_user', 'master multi_user', 'current master rcsi']) {
    const { sql, result } = reference.find(item => item.name === name)
    assert.deepEqual((await run(c, sql)).errors, messages(result.errors), sql)
  }
  const transaction = observed('user transaction')
  assert.deepEqual((await run(c, 'BEGIN TRANSACTION; ALTER DATABASE [probe_db] SET READ_COMMITTED_SNAPSHOT ON')).errors, messages(transaction.errors))
  assert.deepEqual((await run(c, 'SELECT @@TRANCOUNT; IF @@TRANCOUNT>0 ROLLBACK')).rows, [[1]])

  // CURRENT alters the session's own database, which it keeps using.
  const own = await open(c, 'probe_db')
  assert.deepEqual((await run(own.connection, 'ALTER DATABASE CURRENT SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE')).errors, [])
  assert.deepEqual((await run(own.connection, 'ALTER DATABASE CURRENT SET SINGLE_USER WITH ROLLBACK IMMEDIATE')).errors, [])
  assert.deepEqual((await run(own.connection, 'ALTER DATABASE CURRENT SET MULTI_USER; ALTER DATABASE CURRENT SET READ_COMMITTED_SNAPSHOT OFF; SELECT DB_NAME()')).rows, [['probe_db']])

  // Another session in the database: NO_WAIT refuses, and without a
  // termination clause msduck refuses instead of waiting as SQL Server does.
  assert.deepEqual((await run(c, 'ALTER DATABASE [probe_db] SET READ_COMMITTED_SNAPSHOT ON WITH NO_WAIT')).errors, messages(observed('no_wait with users').errors))
  assert.deepEqual((await run(c, 'ALTER DATABASE [probe_db] SET SINGLE_USER WITH NO_WAIT')).errors, messages(observed('single_user no_wait with users').errors))
  const waiting = await run(c, 'ALTER DATABASE [probe_db] SET READ_COMMITTED_SNAPSHOT ON')
  assert.match(waiting.errors[0]?.[3] ?? '', /without a termination clause/)
  assert.deepEqual((await state(c, 'probe_db')).rows, [['probe_db', 0, 'MULTI_USER', false]])
  // ROLLBACK AFTER waits, then terminates the remaining session.
  const after = await run(c, 'ALTER DATABASE [probe_db] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK AFTER 1 SECONDS')
  assert.deepEqual(after.errors, [])
  assert.deepEqual(after.infos, messages(observed('rollback immediate with users').info))
  assert.equal(await closed(own.events), true)
})

test('sessions cannot enter a database while ALTER DATABASE terminates its users', { timeout: 60000 }, async t => {
  const c = await start(t, { options: client })
  await run(c, 'CREATE DATABASE [probe_db]')
  const idle = await open(c, 'probe_db')
  const pending = run(c, 'ALTER DATABASE [probe_db] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK AFTER 2 SECONDS')
  await new Promise(resolve => setTimeout(resolve, 500))
  // Reconnecting clients cannot keep the database in use meanwhile.
  const login = await open(c, 'probe_db')
  assert.equal(login.events.error?.code, 'ELOGIN')
  assert.deepEqual(login.events.tokens.map(m => m[0]), [4060, 18456])
  const other = await open(c, 'master')
  assert.deepEqual((await run(other.connection, 'USE [probe_db]')).errors,
    [[952, 1, 16, "Database 'probe_db' is in transition. Try the statement later."]])
  const altered = await pending
  assert.deepEqual(altered.errors, [])
  assert.deepEqual(altered.infos, messages(observed('rollback immediate with users').info))
  assert.equal(await closed(idle.events), true)
  assert.deepEqual((await run(other.connection, 'USE [probe_db]; SELECT DB_NAME()')).rows, [['probe_db']])
  other.connection.close()
})

test('sys.dm_exec_sessions and @@SPID describe sessions as SQL Server does', { timeout: 60000 }, async t => {
  const c = await start(t, { options: client })
  const spid = await run(c, 'SELECT @@SPID AS spid, @@spid')
  assert.deepEqual(spid.columns[0].map(({ type, flags }) => [type, flags]), observed('spid descriptor').sets[0].columns.map(({ type, flags }) => [type, flags]))
  assert.equal(spid.rows[0][0] > 50, true)
  assert.deepEqual((await run(c, observed('spid is user session').sql ?? reference.find(item => item.name === 'spid is user session').sql)).rows, [[1, 1]])
  // msduck omits client_version, whose value was not captured.
  const subset = observed('dm_exec_sessions subset empty').sets[0].columns.filter(column => column.name !== 'client_version')
  assert.deepEqual((await run(c, `SELECT ${subset.map(column => column.name).join(',')} FROM sys.dm_exec_sessions WHERE 1=0`)).columns[0],
    subset.map(({ name, type, length, flags }) => ({ name, type, length, flags })))
  // SELECT * lists the columns msduck provides, in SQL Server's order.
  // transaction_isolation_level was added later (docs/gaps-transactions.md).
  const provided = new Set([...subset.map(column => column.name), 'transaction_isolation_level'])
  assert.deepEqual((await run(c, 'SELECT * FROM sys.dm_exec_sessions WHERE 1=0')).columns[0].map(column => column.name),
    observed('dm_exec_sessions declarations').sets[0].rows.filter(([name]) => provided.has(name)).map(([name]) => name))
  const own = reference.find(item => item.name === 'own session values')
  // msduck does not provide GETDATE(); CURRENT_TIMESTAMP compares the same way.
  const values = await run(c, own.sql.replace('GETDATE()', 'CURRENT_TIMESTAMP'))
  assert.deepEqual(values.errors, [])
  assert.deepEqual(values.rows, own.result.sets[0].rows)
  const other = await open(c, 'master', { appName: 'msduck-other' })
  assert.deepEqual((await run(c, "SELECT status,is_user_process,DB_NAME(database_id) FROM sys.dm_exec_sessions WHERE program_name=N'msduck-other'")).rows,
    observed('other session status').sets[0].rows)
  // Each session has its own SPID; RESETCONNECTION keeps it.
  const [[otherSpid]] = (await run(other.connection, 'SELECT @@SPID')).rows
  assert.notEqual(otherSpid, spid.rows[0][0])
  await new Promise((resolve, reject) => other.connection.reset(error => error ? reject(error) : resolve()))
  assert.deepEqual((await run(other.connection, 'SELECT @@SPID, program_name FROM sys.dm_exec_sessions WHERE session_id=@@SPID')).rows, [[otherSpid, 'msduck-other']])
  other.connection.close()
})
