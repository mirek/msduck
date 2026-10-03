#!/usr/bin/env node
// First-party SQL Server evidence for issue #727: isolation levels
// (SET TRANSACTION ISOLATION LEVEL, transaction-manager begin requests,
// sys.dm_exec_sessions, DBCC USEROPTIONS, sp_executesql scope) and WAITFOR
// DELAY/TIME validation and waiting. Savepoint rules are retained separately
// in reference/savepoint.json. See docs/gaps-transactions.md.
//
//   node scripts/capture-gaps-transactions.mjs [output] [--write-fixture]
//   node scripts/capture-gaps-transactions.mjs --snapshot [output] [--write-fixture]
//
// Waits are recorded as booleans (at least the requested time, and well
// under a bound) so that two runs compare exactly.
//
// --snapshot captures only ALTER DATABASE ... SET ALLOW_SNAPSHOT_ISOLATION
// and SNAPSHOT transactions (issue #870) with three connections: `a` and `b`
// in the fresh database and `m` in master. It is retained as the fixture's
// separate `snapshot` section ({ image, run }), so the original `run`
// (captured from an earlier image) stays unchanged; --write-fixture adds
// the section only when it is absent.
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { Connection, ISOLATION_LEVEL } from 'tedious'
import { withReferenceContainer } from './lib/reference-container.mjs'
import { isolatedReference, assertSameCapture, refuseExistingFixture, writeNewFixture } from './lib/reference.mjs'
import { capture, canonical } from './lib/compatibility.mjs'

process.env.TZ = 'UTC'
const fixture = new URL('../reference/gaps-transactions.json', import.meta.url)
const args = process.argv.slice(2)
const writeFixture = args.includes('--write-fixture')
const snapshotOnly = args.includes('--snapshot')
const positional = args.filter(arg => arg !== '--write-fixture' && arg !== '--snapshot')
if (positional.length > 1) throw new Error('expected at most one output path')
const output = resolve(positional[0] ?? `artifacts/compatibility/gaps-transactions/gaps-transactions${snapshotOnly ? '-snapshot' : ''}.json`)

const level = 'SELECT transaction_isolation_level FROM sys.dm_exec_sessions WHERE session_id = @@SPID'

function bindGeneratedDatabaseNames(run) {
  return JSON.parse(JSON.stringify(run).replaceAll(/msduck_audit_[0-9a-f]{32}/g, '<fresh-database>'))
}

async function observe(connection) {
  const run = []
  async function record(name, sql, wait) {
    const started = Date.now()
    const result = canonical(await capture(connection, sql))
    const elapsed = Date.now() - started
    const entry = { name, sql, result }
    if (wait) {
      entry.wait = wait
      entry.waited = { atLeast: elapsed >= wait.atLeast, under: elapsed < wait.under }
    }
    run.push(entry)
    return result
  }
  async function manager(name, operation, ...args) {
    const error = await new Promise(resolve => connection[operation](error => resolve(error), ...args))
    run.push({ name, request: operation, args: args.map(canonical), error: error ? { number: error.number ?? null, message: error.message } : null })
  }

  await record('server identity', "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)) AS product_version, CAST(DATABASEPROPERTYEX(DB_NAME(), 'Collation') AS NVARCHAR(128)) AS database_collation")

  // Isolation levels through SQL.
  await record('default level', level)
  for (const name of ['READ UNCOMMITTED', 'REPEATABLE READ', 'SERIALIZABLE', 'SNAPSHOT', 'READ COMMITTED']) {
    await record(`set ${name}`, `SET TRANSACTION ISOLATION LEVEL ${name}`)
    await record(`level after ${name}`, level)
    await record(`useroptions after ${name}`, 'DBCC USEROPTIONS')
  }
  await record('set inside a transaction', `SET TRANSACTION ISOLATION LEVEL SERIALIZABLE; BEGIN TRAN; SET TRANSACTION ISOLATION LEVEL REPEATABLE READ; ${level}; COMMIT; ${level}`)
  await record('reset to read committed', 'SET TRANSACTION ISOLATION LEVEL READ COMMITTED')
  await record('sp_executesql scope', `EXEC sp_executesql N'SET TRANSACTION ISOLATION LEVEL SERIALIZABLE; ${level}'`)
  await record('level after sp_executesql', level)
  await record('useroptions with options', 'SET NOCOUNT ON; SET XACT_ABORT ON; SET ANSI_WARNINGS OFF; SET DATEFIRST 3; DBCC USEROPTIONS')
  await record('useroptions with all options on', 'SET ANSI_WARNINGS ON; DBCC USEROPTIONS WITH NO_INFOMSGS')
  await record('useroptions restore', 'SET NOCOUNT OFF; SET XACT_ABORT OFF; SET DATEFIRST 7; DBCC USEROPTIONS WITH NO_INFOMSGS; SELECT @@ROWCOUNT AS row_count')
  await record('useroptions inside a transaction', 'BEGIN TRAN; DBCC USEROPTIONS; SELECT @@ROWCOUNT AS row_count; COMMIT')
  await record('read committed snapshot setup', 'ALTER DATABASE CURRENT SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE')
  await record('useroptions under read committed snapshot', `DBCC USEROPTIONS WITH NO_INFOMSGS; ${level}`)
  await record('read committed snapshot teardown', 'ALTER DATABASE CURRENT SET READ_COMMITTED_SNAPSHOT OFF WITH ROLLBACK IMMEDIATE')

  // Transaction-manager begin requests set the session's level.
  for (const [name, isolation] of [['SERIALIZABLE', ISOLATION_LEVEL.SERIALIZABLE], ['SNAPSHOT', ISOLATION_LEVEL.SNAPSHOT], ['REPEATABLE_READ', ISOLATION_LEVEL.REPEATABLE_READ], ['READ_UNCOMMITTED', ISOLATION_LEVEL.READ_UNCOMMITTED], ['READ_COMMITTED', ISOLATION_LEVEL.READ_COMMITTED]]) {
    await manager(`tm begin ${name}`, 'beginTransaction', '', isolation)
    await record(`level in tm ${name}`, `${level}; SELECT @@TRANCOUNT AS tran_count`)
    await manager(`tm commit ${name}`, 'commitTransaction')
    await record(`level after tm ${name}`, level)
  }
  await manager('tm begin outer snapshot', 'beginTransaction', 'outer', ISOLATION_LEVEL.SNAPSHOT)
  await manager('tm begin nested read uncommitted', 'beginTransaction', 'inner', ISOLATION_LEVEL.READ_UNCOMMITTED)
  await record('level in nested tm', `${level}; SELECT @@TRANCOUNT AS tran_count`)
  await manager('tm rollback outer', 'rollbackTransaction', 'outer')
  await record('level after tm rollback', `${level}; SELECT @@TRANCOUNT AS tran_count`)
  await manager('tm begin no change', 'beginTransaction', '', ISOLATION_LEVEL.NO_CHANGE)
  await record('level in tm no change', level)
  await manager('tm rollback no change', 'rollbackTransaction')
  await record('isolation reset', 'SET TRANSACTION ISOLATION LEVEL READ COMMITTED')

  // WAITFOR literals and variables. Long or unbounded waits are excluded.
  const waits = [
    ["delay 1 ms", "WAITFOR DELAY '00:00:00.001'", 0],
    ["delay 300 ms", "WAITFOR DELAY '00:00:00.300'", 300],
    ["delay colon milliseconds", "WAITFOR DELAY '00:00:00:200'", 200],
    ["delay short fields", "WAITFOR DELAY '0:0:0.2'", 200],
    ["delay hours and minutes", "WAITFOR DELAY '00:00'", 0],
    ["delay empty", "WAITFOR DELAY ''", 0],
    ["delay blanks", "WAITFOR DELAY ' 00:00:00.100 '", 100],
    ["delay AM", "WAITFOR DELAY '12:00:00.100 AM'", 100],
    ["delay unicode", "WAITFOR DELAY N'00:00:00.100'", 100],
    ["delay varchar", "DECLARE @d VARCHAR(20) = '00:00:00.200'; WAITFOR DELAY @d", 200],
    ["delay nvarchar", "DECLARE @d NVARCHAR(20) = N'00:00:00.200'; WAITFOR DELAY @d", 200],
    ["delay char", "DECLARE @d CHAR(12) = '00:00:00.300'; WAITFOR DELAY @d", 300],
    ["delay datetime", "DECLARE @d DATETIME = '2020-05-05T00:00:00.250'; WAITFOR DELAY @d", 250],
    ["delay int", "DECLARE @d INT = 1; WAITFOR DELAY @d", 1000],
    ["delay smallint zero", "DECLARE @d SMALLINT = 0; WAITFOR DELAY @d", 0],
    ["delay null", "DECLARE @d VARCHAR(20) = NULL; WAITFOR DELAY @d", 0],
    ["delay empty variable", "DECLARE @d VARCHAR(20) = ''; WAITFOR DELAY @d", 0],
    ["delay in transaction", "BEGIN TRAN; WAITFOR DELAY '00:00:00.050'; SELECT @@TRANCOUNT AS tran_count; COMMIT", 50],
    ["delay rowcount", "SELECT 1 AS one; WAITFOR DELAY '00:00:00.010'; SELECT @@ROWCOUNT AS row_count", 10],
    ["delay in IF", "IF 1 = 1 WAITFOR DELAY '00:00:00.010'", 10],
    ["delay through sp_executesql", "EXEC sp_executesql N'WAITFOR DELAY @p', N'@p VARCHAR(20)', @p = '00:00:00.100'", 100],
    ["time in one second", "DECLARE @t INT = DATEDIFF(SECOND, CAST(CAST(GETDATE() AS DATE) AS DATETIME), GETDATE()) + 2; WAITFOR TIME @t; SELECT 'woke' AS woke", 0],
    ["time null", "DECLARE @t VARCHAR(20) = NULL; WAITFOR TIME @t", 0]
  ]
  for (const [name, sql, atLeast] of waits) await record(`waitfor ${name}`, sql, { atLeast, under: atLeast + 3000 })
  for (const [name, sql] of [
    ['invalid literal', "WAITFOR DELAY 'abc'"],
    ['hour 25', "WAITFOR DELAY '25:00'"],
    ['hour 24', "WAITFOR DELAY '24:00:00'"],
    ['minute 60', "WAITFOR DELAY '00:60:00'"],
    ['date part', "WAITFOR DELAY '2020-01-01 00:00:00.100'"],
    ['four fraction digits', "WAITFOR DELAY '00:00:00.0005'"],
    ['seven fraction digits', "WAITFOR DELAY '00:00:00.1234567'"],
    ['four colon digits', "WAITFOR DELAY '00:00:00:1000'"],
    ['hour 0 PM', "WAITFOR TIME '00:00:00.100 PM'"],
    ['attached PM', "WAITFOR DELAY '00:00:00.100PM'"],
    ['one field', "WAITFOR DELAY '5'"],
    ['leading colon', "WAITFOR DELAY ':05'"],
    ['time literal', "WAITFOR TIME 'abc'"],
    ['compile error before earlier statement', "SELECT 1 AS one; WAITFOR DELAY 'abc'; SELECT 2 AS two"],
    ['compile error in TRY', "BEGIN TRY WAITFOR DELAY 'abc' END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS caught END CATCH"],
    ['integer literal', 'WAITFOR DELAY 5'],
    ['expression', "WAITFOR DELAY '00:00:00.1' + ''"],
    ['missing argument', 'WAITFOR TIME'],
    ['timeout clause', "WAITFOR DELAY '00:00:00.100', TIMEOUT 5"],
    ['undeclared variable', 'WAITFOR DELAY @undeclared'],
    ['invalid varchar', "DECLARE @d VARCHAR(20) = 'abc'; WAITFOR DELAY @d"],
    ['varchar hour 25', "DECLARE @d VARCHAR(30) = '25:00'; WAITFOR DELAY @d"],
    ['varchar date part', "DECLARE @d VARCHAR(30) = '2020-01-01 00:00:00.300'; WAITFOR DELAY @d"],
    ['varchar max', "DECLARE @d VARCHAR(MAX) = '00:00:00.100'; WAITFOR DELAY @d"],
    ['nvarchar max', "DECLARE @d NVARCHAR(MAX) = N'00:00:00.100'; WAITFOR DELAY @d"],
    ['caught invalid varchar', "BEGIN TRY DECLARE @d VARCHAR(20) = 'abc'; WAITFOR DELAY @d END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS number, ERROR_SEVERITY() AS severity, ERROR_STATE() AS state, ERROR_MESSAGE() AS message END CATCH"],
    ...[['BIGINT', '1'], ['TINYINT', '0'], ['BIT', '1'], ['DECIMAL(5,2)', '0.5'], ['FLOAT', '0.000005'], ['MONEY', '0'],
      ['DATE', "'2020-01-01'"], ['TIME', "'00:00:00.250'"], ['DATETIME2', "'2020-01-01 00:00:00.300'"],
      ['SMALLDATETIME', "'2020-01-01 00:00'"], ['DATETIMEOFFSET', "'2020-01-01 00:00:00.300 +00:00'"],
      ['UNIQUEIDENTIFIER', 'NEWID()'], ['VARBINARY(10)', '0x01'], ['SQL_VARIANT', "'00:00:00.100'"]]
      .map(([type, value]) => [`type ${type}`, `DECLARE @d ${type} = ${value}; WAITFOR DELAY @d`]),
    ['null bigint', 'DECLARE @d BIGINT; WAITFOR DELAY @d'],
    ['null time', 'DECLARE @d TIME; WAITFOR TIME @d'],
    ['caught type', 'BEGIN TRY DECLARE @d BIT = 1; WAITFOR DELAY @d END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS number, ERROR_SEVERITY() AS severity, ERROR_STATE() AS state, ERROR_MESSAGE() AS message END CATCH']
  ]) await record(`waitfor ${name}`, sql)
  return run
}

// SNAPSHOT isolation and ALLOW_SNAPSHOT_ISOLATION. Each entry names the
// connection that ran it; generated database names are bound afterwards.
function observeSnapshot(config) {
  return async a => {
    const run = []
    const [[[fresh]]] = (await capture(a, 'SELECT DB_NAME()')).sets.map(set => set.rows)
    const connections = { a }
    const open = async (name, database) => {
      const connection = new Connection({ ...config, options: { ...config.options, database, requestTimeout: 60000 } })
      connection.on('error', () => {})
      await new Promise((resolve, reject) => connection.connect(error => error ? reject(error) : resolve()))
      connections[name] = connection
    }
    const on = async (connection, name, sql) => {
      run.push({ name, connection, sql, result: canonical(await capture(connections[connection], sql)) })
    }
    const state = "SELECT name, snapshot_isolation_state, snapshot_isolation_state_desc, is_read_committed_snapshot_on FROM sys.databases WHERE name IN (N'master', DB_NAME()) ORDER BY database_id"
    const alter = 'ALTER DATABASE CURRENT SET ALLOW_SNAPSHOT_ISOLATION'
    try {
      await open('m', 'master')
      await on('a', 'setup', 'CREATE TABLE t (id INT PRIMARY KEY, v INT); INSERT t VALUES (1, 1)')
      await on('a', 'initial state', state)

      // Option OFF: SNAPSHOT transactions cannot access the database.
      await on('a', 'set snapshot level', 'SET TRANSACTION ISOLATION LEVEL SNAPSHOT')
      await on('a', 'off: no data access', 'SELECT 1 AS one; SELECT @@TRANCOUNT AS tc')
      await on('a', 'off: autocommit read', 'SELECT v FROM t; SELECT 2 AS after_error')
      await on('a', 'off: after autocommit read', 'SELECT @@TRANCOUNT AS tc, XACT_STATE() AS xs')
      await on('a', 'off: read in transaction', 'BEGIN TRAN; SELECT v FROM t; SELECT 2 AS after_error')
      await on('a', 'off: after read in transaction', 'SELECT @@TRANCOUNT AS tc, XACT_STATE() AS xs')
      await on('a', 'off: caught read', 'BEGIN TRY SELECT v FROM t END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS n, ERROR_SEVERITY() AS s, ERROR_STATE() AS st, XACT_STATE() AS xs, @@TRANCOUNT AS tc, ERROR_MESSAGE() AS m END CATCH')
      await on('a', 'off: caught read in transaction', 'BEGIN TRAN; BEGIN TRY SELECT v FROM t END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS n, XACT_STATE() AS xs, @@TRANCOUNT AS tc END CATCH; SELECT @@TRANCOUNT AS tc_after')
      await on('a', 'off: cleanup', 'IF @@TRANCOUNT > 0 ROLLBACK; SELECT @@TRANCOUNT AS tc')
      await on('a', 'off: insert', 'INSERT t VALUES (5, 5)')
      await on('a', 'off: update', 'UPDATE t SET v = 1 WHERE id = 1')
      await on('a', 'off: delete', 'DELETE t WHERE id = 99')
      await on('a', 'off: catalog view', "SELECT COUNT(*) AS c FROM sys.objects WHERE name = N't'")
      await on('a', 'off: temporary table', 'CREATE TABLE #tmp (v INT); INSERT #tmp VALUES (1); SELECT v FROM #tmp; DROP TABLE #tmp')
      await on('a', 'off: table variable', 'DECLARE @x TABLE (v INT); INSERT @x VALUES (1); SELECT v FROM @x')
      await on('a', 'off: common table expression', 'WITH c AS (SELECT 1 AS v) SELECT v FROM c')
      await on('a', 'off: missing table', 'SELECT v FROM missing_table')
      await on('a', 'off: create table', 'CREATE TABLE t2 (v INT)')
      await on('a', 'off: drop table', "IF OBJECT_ID(N't2') IS NOT NULL DROP TABLE t2")
      await on('m', 'off: three-part name from master', `SET TRANSACTION ISOLATION LEVEL SNAPSHOT; SELECT v FROM [${fresh}].dbo.t WHERE id = 1; SELECT 2 AS after_error`)
      await on('m', 'off: master after three-part name', 'SELECT @@TRANCOUNT AS tc; SET TRANSACTION ISOLATION LEVEL READ COMMITTED')
      await on('a', 'reset level', 'SET TRANSACTION ISOLATION LEVEL READ COMMITTED')

      // ALTER DATABASE forms.
      await on('a', 'alter: missing value', alter)
      await on('a', 'alter: rollback immediate', `${alter} ON WITH ROLLBACK IMMEDIATE`)
      await on('a', 'alter: no wait', `${alter} ON WITH NO_WAIT`)
      await on('a', 'alter: rollback after', `${alter} ON WITH ROLLBACK AFTER 1`)
      await on('a', 'alter: with read committed snapshot', `${alter} ON, READ_COMMITTED_SNAPSHOT ON`)
      await on('a', 'alter: after read committed snapshot', `ALTER DATABASE CURRENT SET READ_COMMITTED_SNAPSHOT ON, ALLOW_SNAPSHOT_ISOLATION ON`)
      await on('a', 'alter: with user access', `ALTER DATABASE CURRENT SET MULTI_USER, ALLOW_SNAPSHOT_ISOLATION ON`)
      await on('a', 'alter: conflicting values', `${alter} ON, ALLOW_SNAPSHOT_ISOLATION OFF`)
      await on('a', 'alter: repeated value', `${alter} ON, ALLOW_SNAPSHOT_ISOLATION ON`)
      await on('a', 'alter: repeated value in a batch', `SELECT 1 AS before_alter; ${alter} ON, ALLOW_SNAPSHOT_ISOLATION OFF; SELECT 2 AS after_alter`)
      await on('a', 'alter: in a transaction', `BEGIN TRAN; ${alter} ON; SELECT @@TRANCOUNT AS tc; ROLLBACK`)
      await on('a', 'alter: missing database', 'ALTER DATABASE missing_database SET ALLOW_SNAPSHOT_ISOLATION ON')
      await on('a', 'alter: missing database with termination', 'ALTER DATABASE missing_database SET ALLOW_SNAPSHOT_ISOLATION ON WITH NO_WAIT')
      await on('a', 'alter: missing database with another option', 'ALTER DATABASE missing_database SET ALLOW_SNAPSHOT_ISOLATION ON, MULTI_USER')
      await on('a', 'alter: master off', 'ALTER DATABASE master SET ALLOW_SNAPSHOT_ISOLATION OFF')
      await on('a', 'alter: master on with termination', 'ALTER DATABASE master SET ALLOW_SNAPSHOT_ISOLATION ON WITH NO_WAIT')
      await on('m', 'alter: current master', `${alter} ON`)
      await on('a', 'alter: caught', `BEGIN TRY ${alter} ON WITH NO_WAIT END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS n, ERROR_STATE() AS st, ERROR_MESSAGE() AS m END CATCH`)
      await on('a', 'alter: off when off', `${alter} OFF; SELECT @@ROWCOUNT AS rc`)
      await on('a', 'state after off', state)
      await on('a', 'alter: on', `${alter} ON; SELECT @@ROWCOUNT AS rc`)
      await on('a', 'state after on', state)
      await on('a', 'alter: on when on', `ALTER DATABASE [${fresh}] SET ALLOW_SNAPSHOT_ISOLATION ON`)
      await on('a', 'read committed snapshot on', 'ALTER DATABASE CURRENT SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE')
      await on('a', 'state with both options', state)
      await on('a', 'read committed snapshot off', 'ALTER DATABASE CURRENT SET READ_COMMITTED_SNAPSHOT OFF WITH ROLLBACK IMMEDIATE')
      await on('a', 'alter: through sp_executesql', `EXEC sp_executesql N'${alter} ON'`)
      await on('m', 'alter: by name from master', `ALTER DATABASE [${fresh}] SET ALLOW_SNAPSHOT_ISOLATION ON`)

      // Option ON, with a second connection writing concurrently.
      await open('b', fresh)
      await on('a', 'on: begin and read', 'SET TRANSACTION ISOLATION LEVEL SNAPSHOT; BEGIN TRAN; SELECT v FROM t WHERE id = 1')
      await on('b', 'on: concurrent update', 'UPDATE t SET v = 2 WHERE id = 1; SELECT v FROM t WHERE id = 1')
      await on('a', 'on: reads the snapshot', 'SELECT v FROM t WHERE id = 1; SELECT @@TRANCOUNT AS tc')
      await on('a', 'on: commit and read', 'COMMIT; SELECT v FROM t WHERE id = 1')
      await on('a', 'on: autocommit read', 'SELECT v FROM t WHERE id = 1; SELECT @@TRANCOUNT AS tc')
      await on('a', 'conflict: begin', 'BEGIN TRAN; SELECT v FROM t WHERE id = 1')
      await on('b', 'conflict: concurrent update', 'UPDATE t SET v = 3 WHERE id = 1')
      await on('a', 'conflict: update', 'UPDATE t SET v = 4 WHERE id = 1; SELECT 1 AS not_reached')
      await on('a', 'conflict: after update', 'SELECT @@TRANCOUNT AS tc, XACT_STATE() AS xs; SELECT v FROM t WHERE id = 1')
      await on('a', 'conflict delete: begin', 'BEGIN TRAN; SELECT v FROM t WHERE id = 1')
      await on('b', 'conflict delete: concurrent update', 'UPDATE t SET v = 5 WHERE id = 1')
      await on('a', 'conflict delete: delete', 'DELETE t WHERE id = 1')
      await on('a', 'conflict delete: after', 'SELECT @@TRANCOUNT AS tc; SELECT v FROM t WHERE id = 1')
      await on('a', 'caught conflict: begin', 'BEGIN TRAN; SELECT v FROM t WHERE id = 1')
      await on('b', 'caught conflict: concurrent update', 'UPDATE t SET v = 6 WHERE id = 1')
      await on('a', 'caught conflict: update', 'BEGIN TRY UPDATE t SET v = 7 WHERE id = 1 END TRY BEGIN CATCH SELECT ERROR_NUMBER() AS n, ERROR_SEVERITY() AS s, ERROR_STATE() AS st, XACT_STATE() AS xs, @@TRANCOUNT AS tc, ERROR_MESSAGE() AS m END CATCH')
      await on('a', 'caught conflict: after', 'SELECT @@TRANCOUNT AS tc')
      await on('a', 'disjoint writes: begin', 'BEGIN TRAN; SELECT COUNT(*) AS c FROM t')
      await on('b', 'disjoint writes: concurrent insert', 'INSERT t VALUES (10, 10)')
      await on('a', 'disjoint writes: update another row', 'SELECT COUNT(*) AS c FROM t; UPDATE t SET v = 8 WHERE id = 1; SELECT @@TRANCOUNT AS tc')
      await on('a', 'disjoint writes: commit', 'COMMIT; SELECT id, v FROM t ORDER BY id')
      await on('a', 'late access: begin', 'BEGIN TRAN; SELECT 1 AS no_data_access')
      await on('b', 'late access: concurrent update', 'UPDATE t SET v = 9 WHERE id = 1')
      await on('a', 'late access: first read', 'SELECT v FROM t WHERE id = 1; COMMIT')

      // Turning the option off again.
      await on('a', 'reset level again', 'SET TRANSACTION ISOLATION LEVEL READ COMMITTED')
      await on('m', 'alter: off by name', `ALTER DATABASE [${fresh}] SET ALLOW_SNAPSHOT_ISOLATION OFF`)
      await on('a', 'off again: read', 'SET TRANSACTION ISOLATION LEVEL SNAPSHOT; SELECT v FROM t WHERE id = 1')
      await on('a', 'off again: state', `SET TRANSACTION ISOLATION LEVEL READ COMMITTED; ${state}`)
    } finally {
      for (const name of ['b', 'm']) {
        const connection = connections[name]
        if (connection && !connection.closed) await new Promise(resolve => { connection.once('end', resolve); connection.close() })
      }
    }
    return run
  }
}

if (snapshotOnly) {
  let retained
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (writeFixture && retained?.snapshot) throw new Error('refusing to overwrite the retained snapshot section')
  if (writeFixture && !retained) throw new Error('the snapshot section extends an existing fixture')
  await mkdir(resolve(output, '..'), { recursive: true })
  await withReferenceContainer(async (config, container) => {
    const runs = []
    for (let repeat = 0; repeat < 2; repeat++) {
      runs.push(bindGeneratedDatabaseNames(await isolatedReference({ ...config, options: { ...config.options, requestTimeout: 60000 } }, observeSnapshot(config))))
    }
    assertSameCapture(runs[0], runs[1], 'fresh SQL Server databases differ after binding only generated database names')
    const actual = { image: container.image, run: runs[0] }
    if (retained?.snapshot) {
      if (actual.image !== retained.snapshot.image) throw new Error(`image ${actual.image} differs from the retained ${retained.snapshot.image}`)
      assertSameCapture(actual.run, retained.snapshot.run, 'fresh capture differs from the retained snapshot section')
    }
    await writeFile(output, JSON.stringify(actual) + '\n')
    if (writeFixture) await writeFile(fixture, JSON.stringify({ ...retained, snapshot: actual }) + '\n')
    console.log(`Captured ${actual.run.length} snapshot observations in two fresh databases${retained?.snapshot ? ' and matched the retained section' : ''}`)
  })
  process.exit(0)
}

if (writeFixture) await refuseExistingFixture(fixture)
await mkdir(resolve(output, '..'), { recursive: true })
await withReferenceContainer(async (config, container) => {
  const runs = []
  for (let repeat = 0; repeat < 2; repeat++) {
    runs.push(bindGeneratedDatabaseNames(await isolatedReference({ ...config, options: { ...config.options, requestTimeout: 60000 } }, observe)))
  }
  assertSameCapture(runs[0], runs[1], 'fresh SQL Server databases differ after binding only generated database names')
  const actual = { image: container.image, run: runs[0] }
  let retained
  try { retained = JSON.parse(await readFile(fixture, 'utf8')) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (retained) {
    if (actual.image !== retained.image) throw new Error(`image ${actual.image} differs from the retained ${retained.image}`)
    assertSameCapture(actual.run, retained.run, 'fresh capture differs from the retained fixture')
  }
  await writeFile(output, JSON.stringify(actual) + '\n')
  if (writeFixture) await writeNewFixture(fixture, actual)
  console.log(`Captured ${actual.run.length} observations in two fresh databases${retained ? ' and matched the retained fixture' : ''}`)
})
