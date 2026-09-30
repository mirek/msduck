#!/usr/bin/env node
// Independent SQL Server evidence for ALTER DATABASE SET READ_COMMITTED_SNAPSHOT
// and user access with termination clauses, the sessions they terminate,
// SINGLE_USER admission, sys.dm_exec_sessions and @@SPID (issue #680).
//
// Every observation runs in a fresh pinned container against fixed database
// names, so messages are stable. Values that identify the environment
// (SPIDs, login times, host process IDs, client versions) are reduced to
// their type or to comparisons made inside the server.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, readFile, realpath, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { basename, dirname, resolve } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'
import { fileURLToPath } from 'node:url'
import { Connection, Request } from 'tedious'
import { canonical } from './lib/compatibility.mjs'
import { withReferenceContainer, referenceImage } from './lib/reference-container.mjs'
import { assertSameCapture, connect, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'

const fixture = new URL('../reference/alter-database-sessions.json', import.meta.url)
const fixtureSha256 = '169ac0d27094852ebe58cc2d442888d4825ec35bd2471c51b99f51e9405e231c'
const args = process.argv.slice(2)
const check = args.includes('--check')
const writeFixture = args.includes('--write-fixture')
const paths = args.filter(arg => !['--check', '--write-fixture'].includes(arg))
if (paths.length > 1 || (check && writeFixture)) throw Error('usage: capture-alter-database-sessions.mjs [--check | --write-fixture] [output]')
const output = resolve(paths[0] ?? 'artifacts/compatibility/alter-database-sessions-reference/capture.json')

// DONE bodies (TDS 7.2+: USHORT status, USHORT CurCmd, ULONGLONG row count)
// are recorded verbatim before tedious parses them. The parser's options
// object belongs to one connection, so concurrent connections keep separate
// sinks.
const StreamParser = createRequire(import.meta.url)('tedious/lib/token/stream-parser.js')
const doneTypes = new Map([[0xFD, 'done'], [0xFE, 'doneProc'], [0xFF, 'doneInProc']])
const sinks = new WeakMap()
const readToken = StreamParser.prototype.readToken
StreamParser.prototype.readToken = function (type) {
  return doneTypes.has(type) ? recordDone(this, type) : readToken.call(this, type)
}
function recordDone(parser, type) {
  const at = parser.position
  if (parser.buffer.length < at + 12) return parser.waitForChunk().then(() => recordDone(parser, type))
  sinks.get(parser.options)?.push({
    kind: doneTypes.get(type),
    status: parser.buffer.readUInt16LE(at),
    curCmd: parser.buffer.readUInt16LE(at + 2),
    rowCount: parser.buffer.readBigUInt64LE(at + 4).toString(),
  })
  return readToken.call(parser, type)
}

// One batch with its descriptors, rows, messages and raw DONE tokens.
function run(connection, sql) {
  return new Promise(resolve => {
    const result = { sets: [], done: [], errors: [], info: [] }
    const done = []
    sinks.set(connection.config.options, done)
    const onError = e => result.errors.push({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, message: e.message })
    const onInfo = e => result.info.push({ number: e.number, state: e.state, class: e.class, lineNumber: e.lineNumber, message: e.message })
    const finish = error => {
      connection.off('errorMessage', onError)
      connection.off('infoMessage', onInfo)
      sinks.delete(connection.config.options)
      result.done = done
      if (error && !result.errors.length) result.transport = { code: error.code ?? null, message: error.message }
      resolve(canonical(result))
    }
    const request = new Request(sql, error => finish(error))
    connection.on('errorMessage', onError)
    connection.on('infoMessage', onInfo)
    request.on('columnMetadata', metadata => result.sets.push({
      columns: metadata.map(c => ({ name: c.colName, type: c.type.name, length: c.dataLength ?? null, precision: c.precision ?? null, scale: c.scale ?? null, flags: c.flags, collation: c.collation ?? null })), rows: [],
    }))
    request.on('row', row => result.sets.at(-1).rows.push(row.map(c => c.value)))
    try { connection.execSqlBatch(request) } catch (error) { finish(error) }
  })
}

// A connection whose closure and errors are observable after the fact.
async function open(config, database, appName = 'msduck-capture') {
  const events = { end: false, errors: [] }
  const connection = await connect({ ...config, options: { ...config.options, database, appName, workstationId: 'msduck-host', connectTimeout: 30000 } })
  connection.on('end', () => { events.end = true })
  connection.on('error', error => events.errors.push({ code: error.code ?? null, message: error.message }))
  return { connection, events }
}
// A login SQL Server refuses, with the ERROR and INFO tokens it sent.
function refused(config, database) {
  return new Promise(resolve => {
    const tokens = []
    const connection = new Connection({ ...config, options: { ...config.options, database, appName: 'msduck-capture', workstationId: 'msduck-host', connectTimeout: 30000 } })
    connection.on('error', () => {})
    connection.on('errorMessage', e => tokens.push({ kind: 'error', number: e.number, state: e.state, class: e.class, message: e.message }))
    connection.on('infoMessage', e => tokens.push({ kind: 'info', number: e.number, state: e.state, class: e.class, message: e.message }))
    connection.connect(error => {
      connection.close()
      resolve(error ? { connected: false, code: error.code ?? null, message: error.message, tokens } : { connected: true, tokens: tokens.filter(t => t.kind === 'error') })
    })
  })
}
// SQL Server can briefly hold a SINGLE_USER database after terminating its
// sessions; retry the login within a bound and report the first refusal only
// on the console, since its timing is not deterministic.
async function openRetry(config, database) {
  for (let attempt = 0; ; attempt++) {
    try { return await open(config, database) } catch (error) {
      if (attempt === 0) console.error(`login to ${database} refused before retry: ${(error.errors ?? [error]).map(e => `${e.number ?? ''} ${e.message}`).join('; ')}`)
      if (attempt >= 40) throw error
      await delay(500)
    }
  }
}
function close({ connection }) {
  if (connection.closed) return Promise.resolve()
  return new Promise(resolve => { connection.once('end', resolve); connection.close() })
}
// Wait for a terminated connection to notice closure, within a bound.
async function settled(events) {
  for (let i = 0; i < 50 && !events.end; i++) await delay(100)
  return { end: events.end, errors: events.errors }
}

const databaseState = name => `SELECT name,user_access,user_access_desc,is_read_committed_snapshot_on,snapshot_isolation_state FROM sys.databases WHERE name=N'${name}'`
const users = name => `SELECT COUNT(*) AS sessions FROM sys.dm_exec_sessions WHERE database_id=DB_ID(N'${name}') AND session_id<>@@SPID AND is_user_process=1`
const sessionColumns = 'session_id,login_time,host_name,program_name,host_process_id,client_version,client_interface_name,login_name,status,database_id,is_user_process,original_login_name'
const declarations = view => `SELECT name,column_id,system_type_id,user_type_id,max_length,precision,scale,collation_name,is_nullable FROM sys.all_columns WHERE object_id=OBJECT_ID(N'${view}') ORDER BY column_id`

async function observe(config) {
  const observations = []
  const record = (name, value) => { observations.push({ name, ...value }); console.log(name) }
  const a = await open(config, 'master')
  const on = async (name, session, sql) => record(name, { sql, result: await run(session.connection, sql) })
  try {
    await on('server version', a, "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version")

    // Session catalog and @@SPID.
    await on('dm_exec_sessions declarations', a, declarations('sys.dm_exec_sessions'))
    await on('dm_exec_sessions complete empty', a, 'SELECT * FROM sys.dm_exec_sessions WHERE 1=0')
    await on('dm_exec_sessions subset empty', a, `SELECT ${sessionColumns} FROM sys.dm_exec_sessions WHERE 1=0`)
    await on('spid descriptor', a, 'SELECT @@SPID AS spid, @@spid')
    await on('spid is user session', a, 'SELECT CASE WHEN @@SPID>50 THEN 1 ELSE 0 END AS user_range, CASE WHEN @@SPID=(SELECT session_id FROM sys.dm_exec_sessions WHERE session_id=@@SPID) THEN 1 ELSE 0 END AS listed')
    await on('owner program_name query', a, 'select program_name from sys.dm_exec_sessions where session_id = @@spid;')
    await on('own session values', a, "SELECT host_name,program_name,client_interface_name,login_name,original_login_name,status,DB_NAME(database_id) AS database_name,is_user_process,CASE WHEN login_time<=GETDATE() THEN 1 ELSE 0 END AS logged_in_before_now,CASE WHEN host_process_id IS NULL THEN 0 ELSE 1 END AS has_host_process FROM sys.dm_exec_sessions WHERE session_id=@@SPID")
    {
      // tedious sends its own name when appName is empty.
      const other = await open(config, 'master', '')
      await on('empty application name', other, 'SELECT program_name,DATALENGTH(program_name) AS bytes FROM sys.dm_exec_sessions WHERE session_id=@@SPID')
      await on('other session status', a, "SELECT status,is_user_process,DB_NAME(database_id) AS database_name FROM sys.dm_exec_sessions WHERE program_name=N'Tedious' AND session_id<>@@SPID")
      await close(other)
    }
    await on('sys.databases option declarations', a, "SELECT name,column_id,system_type_id,user_type_id,max_length,precision,scale,collation_name,is_nullable FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.databases') AND name IN (N'user_access',N'user_access_desc',N'is_read_committed_snapshot_on',N'snapshot_isolation_state') ORDER BY column_id")

    // Options on an unused database.
    await on('create probe', a, 'CREATE DATABASE [probe_db]')
    await on('initial state', a, databaseState('probe_db'))
    await on('rcsi on', a, 'ALTER DATABASE [probe_db] SET READ_COMMITTED_SNAPSHOT ON')
    await on('rcsi on state', a, databaseState('probe_db'))
    await on('rcsi on repeated with rollback immediate', a, 'alter database [probe_db] set read_committed_snapshot on with rollback immediate;')
    await on('rcsi off no_wait', a, 'ALTER DATABASE probe_db SET READ_COMMITTED_SNAPSHOT OFF WITH NO_WAIT')
    await on('rcsi off state', a, databaseState('probe_db'))
    await on('single_user unused', a, 'ALTER DATABASE [probe_db] SET SINGLE_USER')
    await on('single_user state', a, databaseState('probe_db'))
    await on('restricted_user', a, 'ALTER DATABASE [probe_db] SET RESTRICTED_USER WITH ROLLBACK IMMEDIATE')
    await on('restricted_user state', a, databaseState('probe_db'))
    await on('multi_user', a, 'ALTER DATABASE [probe_db] SET MULTI_USER')
    await on('multi_user state', a, databaseState('probe_db'))
    await on('rollback after', a, 'ALTER DATABASE [probe_db] SET READ_COMMITTED_SNAPSHOT OFF WITH ROLLBACK AFTER 1 SECONDS')
    await on('two options', a, 'ALTER DATABASE [probe_db] SET SINGLE_USER, READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE')
    await on('two options state', a, databaseState('probe_db'))
    await on('reset after two options', a, 'ALTER DATABASE [probe_db] SET MULTI_USER; ALTER DATABASE [probe_db] SET READ_COMMITTED_SNAPSHOT OFF')

    // Refusals.
    await on('unknown database', a, 'ALTER DATABASE [missing_db] SET SINGLE_USER WITH ROLLBACK IMMEDIATE')
    await on('unknown database rcsi', a, 'ALTER DATABASE missing_db SET READ_COMMITTED_SNAPSHOT ON')
    await on('master rcsi', a, 'ALTER DATABASE [master] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE')
    await on('master single_user', a, 'ALTER DATABASE master SET SINGLE_USER WITH ROLLBACK IMMEDIATE')
    await on('master multi_user', a, 'ALTER DATABASE master SET MULTI_USER')
    await on('master state', a, databaseState('master'))
    await on('user transaction', a, 'BEGIN TRANSACTION; ALTER DATABASE [probe_db] SET READ_COMMITTED_SNAPSHOT ON')
    await on('user transaction state', a, 'SELECT @@TRANCOUNT AS trancount, XACT_STATE() AS xact_state; IF @@TRANCOUNT>0 ROLLBACK')
    await on('user transaction single_user', a, 'BEGIN TRANSACTION; ALTER DATABASE [probe_db] SET SINGLE_USER WITH ROLLBACK IMMEDIATE')
    await on('user transaction single_user state', a, 'SELECT @@TRANCOUNT AS trancount, XACT_STATE() AS xact_state; IF @@TRANCOUNT>0 ROLLBACK')
    await on('unsupported option', a, 'ALTER DATABASE [probe_db] SET ALLOW_SNAPSHOT_ISOLATION ON')
    await on('unsupported option state', a, databaseState('probe_db'))
    await on('reset snapshot isolation', a, 'ALTER DATABASE [probe_db] SET ALLOW_SNAPSHOT_ISOLATION OFF')

    // CURRENT, and a session altering its own database.
    {
      const own = await open(config, 'probe_db')
      await on('current rcsi', own, 'ALTER DATABASE CURRENT SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE')
      await on('current single_user', own, 'ALTER DATABASE CURRENT SET SINGLE_USER WITH ROLLBACK IMMEDIATE')
      await on('current state', own, databaseState('probe_db'))
      await on('current multi_user', own, 'ALTER DATABASE CURRENT SET MULTI_USER; ALTER DATABASE CURRENT SET READ_COMMITTED_SNAPSHOT OFF')
      await on('current usable', own, 'SELECT DB_NAME() AS database_name')
      await close(own)
    }
    {
      const current = await open(config, 'master')
      await on('current master rcsi', current, 'ALTER DATABASE CURRENT SET READ_COMMITTED_SNAPSHOT ON')
      await close(current)
    }

    // Another session uses the database: NO_WAIT, then ROLLBACK IMMEDIATE
    // terminates an idle session and a session inside a transaction.
    {
      const idle = await open(config, 'probe_db')
      const busy = await open(config, 'probe_db')
      await on('busy transaction', busy, 'CREATE TABLE dbo.t(id INT); BEGIN TRANSACTION; INSERT INTO dbo.t VALUES(1)')
      await on('others before', a, users('probe_db'))
      await on('no_wait with users', a, 'ALTER DATABASE [probe_db] SET READ_COMMITTED_SNAPSHOT ON WITH NO_WAIT')
      await on('no_wait with users state', a, databaseState('probe_db'))
      await on('single_user no_wait with users', a, 'ALTER DATABASE [probe_db] SET SINGLE_USER WITH NO_WAIT')
      await on('rollback immediate with users', a, 'alter database [probe_db] set read_committed_snapshot on with rollback immediate;')
      await on('rollback immediate with users state', a, databaseState('probe_db'))
      await on('others after', a, users('probe_db'))
      record('idle session closure', await settled(idle.events))
      record('busy session closure', await settled(busy.events))
      await on('idle session next request', idle, 'SELECT 1 AS reusable')
      await on('busy session next request', busy, 'SELECT 1 AS reusable')
      await on('terminated transaction rolled back', a, 'SELECT COUNT(*) AS rows_kept FROM probe_db.dbo.t')
      await close(idle)
      await close(busy)
    }

    // A request running when the database is altered.
    {
      const running = await open(config, 'probe_db')
      const pending = run(running.connection, "WAITFOR DELAY '00:00:05'; SELECT 1 AS finished")
      await delay(1000)
      await on('rollback immediate with running request', a, 'ALTER DATABASE [probe_db] SET READ_COMMITTED_SNAPSHOT OFF WITH ROLLBACK IMMEDIATE')
      record('running request', { result: await pending })
      record('running session closure', await settled(running.events))
      await close(running)
    }

    // SINGLE_USER admission. The session that sets SINGLE_USER holds the
    // database (a shared DATABASE lock) even from master, so other sessions
    // are refused until it releases it; then the owner's teardown.
    const holds = "SELECT COUNT(*) AS database_locks FROM sys.dm_tran_locks WHERE resource_database_id=DB_ID(N'probe_db') AND resource_type=N'DATABASE' AND request_session_id=@@SPID"
    {
      const idle = await open(config, 'probe_db')
      await on('owner single_user with users', a, 'alter database [probe_db] set single_user with rollback immediate;')
      record('single_user terminated session closure', await settled(idle.events))
      await close(idle)
      await on('single_user with users state', a, databaseState('probe_db'))
      await on('issuer holds database', a, holds)
      record('second login refused', await refused(config, 'probe_db'))
      const other = await open(config, 'master')
      await on('use refused', other, 'USE [probe_db]')
      await on('use refused database', other, 'SELECT DB_NAME() AS database_name')
      await on('drop by other while held', other, 'DROP DATABASE [probe_db]')
      await on('alter by other while held', other, 'ALTER DATABASE [probe_db] SET MULTI_USER WITH NO_WAIT')
      await close(other)
      await on('issuer use', a, 'USE [probe_db]; SELECT DB_NAME() AS database_name; USE [master]')
      await on('issuer still holds database', a, holds)
      await on('owner drop', a, 'drop database [probe_db];')
      await on('dropped state', a, databaseState('probe_db'))
    }

    // An issuer that disconnects releases the database; the first session to
    // enter holds it next.
    {
      await on('create release probe', a, 'CREATE DATABASE [probe_db]')
      const issuer = await open(config, 'master')
      await on('issuer single_user', issuer, 'ALTER DATABASE [probe_db] SET SINGLE_USER')
      await close(issuer)
      const holder = await openRetry(config, 'probe_db')
      await on('holder database', holder, 'SELECT DB_NAME() AS database_name')
      record('login refused while held', await refused(config, 'probe_db'))
      await on('use refused while held', a, 'USE [probe_db]')
      await on('drop while held', a, 'DROP DATABASE [probe_db]')
      await on('multi_user no_wait while held', a, 'ALTER DATABASE [probe_db] SET MULTI_USER WITH NO_WAIT')
      await on('multi_user while held state', a, databaseState('probe_db'))
      await on('single_user rollback immediate while held', a, 'alter database [probe_db] set single_user with rollback immediate;')
      record('holder closure', await settled(holder.events))
      await close(holder)
      await on('drop after rollback immediate', a, 'drop database [probe_db];')
    }

    // The owner's complete sequence in one batch.
    {
      await on('create sequence probe', a, 'CREATE DATABASE [probe_db]')
      const idle = await open(config, 'probe_db')
      await on('owner sequence batch', a, 'alter database [probe_db] set read_committed_snapshot on with rollback immediate; select program_name from sys.dm_exec_sessions where session_id = @@spid; alter database [probe_db] set single_user with rollback immediate; drop database [probe_db];')
      record('owner sequence session closure', await settled(idle.events))
      await close(idle)
      await on('owner sequence dropped', a, databaseState('probe_db'))
    }
    await on('session reusable', a, 'SELECT @@TRANCOUNT AS trancount, DB_NAME() AS database_name')
  } catch (error) {
    await writeFile(`${output}.partial.json`, JSON.stringify(observations, null, 1) + '\n')
    throw error
  } finally { await close(a) }
  validate(observations)
  return observations
}

const numbers = messages => messages.map(m => m.number)
const done = result => result.done.map(d => [d.kind, d.status, d.curCmd, d.rowCount])
function validate(run) {
  const get = name => {
    const found = run.find(item => item.name === name)
    assert(found, `${name}: missing observation`)
    return found.result ?? found
  }
  const expect = (name, errors, info = []) => {
    assertSameCapture(numbers(get(name).errors), errors, `${name}: errors changed`)
    assertSameCapture(numbers(get(name).info), info, `${name}: messages changed`)
  }
  assertSameCapture(get('server version').sets[0].rows, [['17.0.4065.4']], 'reference image version changed')
  assert.equal(get('dm_exec_sessions declarations').sets[0].rows.length, 52, 'sys.dm_exec_sessions schema changed')
  assertSameCapture(get('spid descriptor').sets[0].columns.map(c => [c.type, c.flags]), [['SmallInt', 32], ['SmallInt', 32]], '@@SPID descriptor changed')
  assertSameCapture(get('owner program_name query').sets[0].rows, [['msduck-capture']], 'program name changed')
  assertSameCapture(done(get('rcsi on')), [['done', 0, 215, '0']], 'ALTER DATABASE completion changed')
  for (const name of ['rcsi on', 'rcsi off no_wait', 'single_user unused', 'restricted_user', 'multi_user', 'rollback after', 'two options']) expect(name, [])
  expect('unknown database', [5011, 5069])
  expect('master rcsi', [5058])
  expect('master single_user', [5058])
  expect('current master rcsi', [12104])
  expect('user transaction', [226])
  expect('no_wait with users', [5070, 5069])
  expect('single_user no_wait with users', [5070, 5069])
  expect('rollback immediate with users', [], [5060, 5060])
  assertSameCapture(get('others after').sets[0].rows, [[0]], 'terminated sessions remain')
  for (const name of ['idle session closure', 'busy session closure', 'running session closure', 'single_user terminated session closure']) assert.equal(get(name).end, true, `${name}: session not closed`)
  assertSameCapture(get('terminated transaction rolled back').sets[0].rows, [[0]], 'terminated transaction kept')
  expect('running request', [596])
  assertSameCapture(get('issuer holds database').sets[0].rows, [[1]], 'issuer does not hold the database')
  assertSameCapture(get('second login refused').tokens.map(t => t.number), [4060, 18456], 'single-user login refusal changed')
  expect('use refused', [924])
  expect('drop by other while held', [3702])
  expect('alter by other while held', [5064, 5069])
  expect('single_user rollback immediate while held', [5064, 5069])
  expect('owner drop', [])
  assertSameCapture(done(get('owner sequence batch')), [['done', 1, 215, '0'], ['done', 17, 193, '1'], ['done', 1, 215, '0'], ['done', 0, 204, '0']], 'owner sequence completion changed')
  assertSameCapture(get('owner sequence batch').sets[0].rows, [['msduck-capture']], 'owner sequence program name changed')
  expect('session reusable', [])
}

if (check) {
  const bytes = await readFile(fixture)
  assert.equal(createHash('sha256').update(bytes).digest('hex'), fixtureSha256, 'fixture checksum changed')
  const retained = JSON.parse(bytes)
  assert.equal(retained.image, referenceImage, 'reference image changed')
  assert.equal(retained.runs.length, 2, 'independent runs missing')
  assert.equal(createHash('sha256').update(JSON.stringify(retained.runs[0])).digest('hex'), retained.captureSha256, 'capture checksum changed')
  for (const run of retained.runs) validate(run)
  assertSameCapture(retained.runs[0], retained.runs[1], 'independent runs differ')
  console.log(`Checked ${retained.runs[0].length} ALTER DATABASE and session observations in two retained runs`)
} else {
  if (writeFixture) await refuseExistingFixture(fixture)
  await mkdir(dirname(output), { recursive: true })
  if (await realpath(dirname(output)).then(dir => resolve(dir, basename(output))) === fileURLToPath(fixture)) throw Error('capture output must not be the retained fixture')
  if (process.env.MSSQL_REFERENCE_IMAGE && process.env.MSSQL_REFERENCE_IMAGE !== referenceImage) throw Error('reference image must be pinned')
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    await withReferenceContainer(async (config, container) => {
      assert.equal(container.image, referenceImage, 'reference image must be pinned')
      const run = await observe(config)
      if (runs.length) assertSameCapture(run, runs[0], 'independent reference containers differ')
      runs.push(run)
    })
  }
  const actual = { image: referenceImage, captureSha256: createHash('sha256').update(JSON.stringify(runs[0])).digest('hex'), runs }
  await writeFile(output, JSON.stringify(actual, null, 1) + '\n', { flag: 'wx' })
  if (writeFixture) await writeNewFixture(fixture, actual)
  console.log(`Captured ${runs[0].length} ALTER DATABASE and session observations in two fresh containers`)
}
